#![windows_subsystem = "windows"]

mod client;
mod theme;
mod view;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axis_protocol::{ClientMsg, Family, LiveFrame, PadState, SessionInfo, StickStages};
use slint::{ComponentHandle, ModelRc, VecModel};

use client::{Client, UiUpdate};
use view::{Connection, PadCard};

slint::include_modules!();

/// Quanto tempo "Conectando ao núcleo…" fica depois de iniciar o núcleo.
const START_GRACE: Duration = Duration::from_secs(3);

/// Estado da interface que não mora no Slint (só na thread da interface).
#[derive(Default)]
struct Shell {
    /// `None` no modo demo: o que seria enviado ao núcleo é descartado.
    client: Option<Client>,
    /// Cartões mostrados agora; o modelo só é recriado quando mudam.
    cards: Vec<PadCard>,
    selected: Option<String>,
    /// Última lista do núcleo (para o cabeçalho e para filtrar quadros).
    sessions: Vec<SessionInfo>,
    /// O núcleo tem barramento virtual (do `Connected`).
    emulation: bool,
    /// Já chegou um quadro do controle selecionado desde a seleção.
    has_live: bool,
}

thread_local! {
    /// Geração da espera após iniciar o núcleo (só na thread da interface):
    /// um temporizador antigo não encerra uma espera mais nova.
    static GRACE: Cell<u32> = const { Cell::new(0) };
    static SHELL: RefCell<Shell> = RefCell::default();
}

fn main() -> Result<(), slint::PlatformError> {
    let app = App::new()?;
    setup_appearance(&app);
    setup_window_controls(&app);
    set_connection(&app, Connection::Connecting);

    let weak = app.as_weak();
    app.on_select_pad(move |id| {
        if let Some(app) = weak.upgrade() {
            select(&app, Some(id.to_string()));
        }
    });

    // Modo demo: dados sintéticos, sem pipe e sem iniciar o núcleo (só para
    // captura de tela e desenvolvimento).
    if std::env::var_os("AXIS_UI_DEMO").is_some_and(|v| v == "1") {
        for update in demo_updates() {
            apply(&app, update, false);
        }
        let _live = demo_live(&app);
        return app.run();
    }

    let weak = app.as_weak();
    app.on_start_daemon(move || {
        if let Some(app) = weak.upgrade() {
            start_and_wait(&app);
        }
    });

    // Só a primeira resposta decide se o núcleo é iniciado automaticamente.
    let first_seen = AtomicBool::new(false);
    let weak = app.as_weak();
    let client = Client::spawn(move |update| {
        let first_failure = match update {
            UiUpdate::Connected { .. } | UiUpdate::Incompatible => {
                first_seen.store(true, Ordering::SeqCst);
                false
            }
            UiUpdate::Disconnected => !first_seen.swap(true, Ordering::SeqCst),
            _ => false,
        };
        let weak = weak.clone();
        // Roda na thread do cliente: entrega à thread da interface sem esperar.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                apply(&app, update, first_failure);
            }
        });
    });
    SHELL.with(|s| s.borrow_mut().client = Some(client));

    let result = app.run();
    // Fecha o pipe antes de a thread principal acabar.
    drop(SHELL.with(|s| s.borrow_mut().client.take()));
    result
}

/// Paleta de destaque (de `theme::ACCENTS`) e destaque padrão.
fn setup_appearance(app: &App) {
    let accents: Vec<AccentData> = theme::ACCENTS
        .iter()
        .map(|(id, label)| AccentData {
            label: (*label).into(),
            color: rgb(theme::accent_rgb(id)),
        })
        .collect();
    app.set_accents(ModelRc::new(VecModel::from(accents)));
    app.set_accent_index(0);
    app.global::<Tokens>()
        .set_accent(rgb(theme::accent_rgb("laranja")));
}

/// Minimizar, maximizar e fechar da barra de título (a janela não tem moldura;
/// o arraste é do `WindowMoveArea`, que pede ao winit o arraste do sistema).
fn setup_window_controls(app: &App) {
    let weak = app.as_weak();
    app.on_minimize(move || {
        if let Some(app) = weak.upgrade() {
            app.window().set_minimized(true);
        }
    });
    let weak = app.as_weak();
    app.on_toggle_maximize(move || {
        if let Some(app) = weak.upgrade() {
            let w = app.window();
            w.set_maximized(!w.is_maximized());
        }
    });
    let weak = app.as_weak();
    app.on_close_window(move || {
        if let Some(app) = weak.upgrade() {
            // Esconder a única janela encerra o laço de eventos.
            let _ = app.hide();
        }
    });
}

fn rgb([r, g, b]: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(r, g, b)
}

fn send(msg: ClientMsg) {
    SHELL.with(|s| {
        if let Some(client) = &s.borrow().client {
            client.send(msg);
        }
    });
}

fn set_connection(app: &App, c: Connection) {
    app.set_connection(c.code());
    app.set_connection_message(view::connection_message(c).into());
}

fn refresh_status(app: &App) {
    let (emulation, count) = SHELL.with(|s| {
        let s = s.borrow();
        (s.emulation, view::connected_count(&s.sessions))
    });
    app.set_status(view::status_line(app.get_paused(), emulation, count).into());
}

/// Cabeçalho e palco do controle selecionado (fora do caminho de 60 Hz).
fn refresh_overview(app: &App) {
    SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let found = s
            .selected
            .as_deref()
            .and_then(|id| s.sessions.iter().enumerate().find(|(_, x)| x.id == id));
        let Some((index, session)) = found else {
            app.set_has_pad(false);
            return;
        };
        let h = view::header(index, session, s.emulation);
        app.set_has_pad(true);
        app.set_header(OverviewHeader {
            kicker: h.kicker.into(),
            title: h.title.into(),
            subtitle: h.subtitle.into(),
        });
        app.set_family(match session.family {
            Family::DualShock4 => 0,
            Family::DualSense => 1,
            Family::Xbox => 2,
        });
        app.set_emulating(s.emulation && !session.view_only);
        let connected = session.connected;
        app.set_pad_connected(connected);
        if !connected {
            // Reconectando: palco esmaecido e valores em neutro.
            s.has_live = false;
            app.set_live(neutral_live());
        }
        app.set_has_live(s.has_live);
        app.set_stage_status(view::stage_status(connected, s.has_live).into());
    });
}

/// Palco parado: analógicos no centro, gatilhos soltos, métricas sem dado.
fn neutral_live() -> LiveData {
    let rest = PadState::default();
    live_data(&LiveFrame {
        id: String::new(),
        input: rest,
        output: rest,
        left: StickStages::default(),
        right: StickStages::default(),
        rate_hz: 0,
        latency_p99_us: 0,
        battery: None,
        charging: false,
    })
}

fn live_data(f: &LiveFrame) -> LiveData {
    let [calibration_on, drift_on, deadzone_on, curve_on, anti_on] = view::active_stages(&f.left);
    let (i, o) = (&f.input, &f.output);
    LiveData {
        lx: o.lx,
        ly: o.ly,
        rx: o.rx,
        ry: o.ry,
        l2: o.l2,
        r2: o.r2,
        // Só os 16 bits de botões existem; o resto é descartado.
        buttons: (o.buttons & 0xffff) as i32,
        in_x: i.lx,
        in_y: i.ly,
        out_x: o.lx,
        out_y: o.ly,
        l2_text: view::percent_text(o.l2).into(),
        r2_text: view::percent_text(o.r2).into(),
        left_text: view::stick_text(o.lx, o.ly).into(),
        right_text: view::stick_text(o.rx, o.ry).into(),
        in_text: view::stick_text(i.lx, i.ly).into(),
        out_text: view::stick_text(o.lx, o.ly).into(),
        latency: view::latency_text(f.latency_p99_us).into(),
        rate: view::rate_text(f.rate_hz).into(),
        battery: view::battery_text(f.battery).into(),
        calibration_on,
        drift_on,
        deadzone_on,
        curve_on,
        anti_on,
    }
}

/// Um quadro ao vivo: só propriedades, nada de recriar modelos.
fn show_live(app: &App, frame: &LiveFrame) {
    let first = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        if !view::live_accepted(s.selected.as_deref(), &s.sessions, &frame.id) {
            return None;
        }
        Some(!std::mem::replace(&mut s.has_live, true))
    });
    let Some(first) = first else { return };
    app.set_live(live_data(frame));
    if first {
        app.set_has_live(true);
        app.set_stage_status(view::stage_status(true, true).into());
    }
}

/// Seleciona um controle e pede ao núcleo os quadros ao vivo dele.
fn select(app: &App, id: Option<String>) {
    let changed = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let changed = s.selected != id;
        s.selected = id.clone();
        if changed {
            s.has_live = false;
        }
        changed
    });
    if changed {
        app.set_selected_id(id.clone().unwrap_or_default().into());
        app.set_live(neutral_live());
        send(ClientMsg::Inspect { id });
    }
    refresh_overview(app);
}

fn show_sessions(app: &App, sessions: &[SessionInfo]) {
    let cards: Vec<PadCard> = sessions.iter().map(view::pad_card).collect();
    let current = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        if s.cards != cards {
            let model: Vec<PadCardData> = cards.iter().map(card_data).collect();
            app.set_pads(ModelRc::from(Rc::new(VecModel::from(model))));
            s.cards = cards;
        }
        s.sessions = sessions.to_vec();
        s.selected.clone()
    });
    select(app, view::next_selection(current.as_deref(), sessions));
    refresh_status(app);
}

fn card_data(c: &PadCard) -> PadCardData {
    PadCardData {
        id: c.id.as_str().into(),
        owner: c.owner.as_str().into(),
        line: c.line.as_str().into(),
        label: c.label.as_str().into(),
        color: rgb(c.color),
    }
}

fn apply(app: &App, update: UiUpdate, first_failure: bool) {
    match update {
        UiUpdate::Connected { paused, emulation } => {
            set_connection(app, Connection::Connected);
            // O estado do núcleo é a verdade: nada de alternar localmente.
            app.set_paused(paused);
            SHELL.with(|s| s.borrow_mut().emulation = emulation);
            refresh_status(app);
            refresh_overview(app);
            // Conexão nova não sabe o que inspecionávamos: pede de novo.
            let selected = SHELL.with(|s| s.borrow().selected.clone());
            if selected.is_some() {
                send(ClientMsg::Inspect { id: selected });
            }
        }
        UiUpdate::Incompatible => set_connection(app, Connection::Incompatible),
        UiUpdate::Disconnected if first_failure => start_and_wait(app),
        UiUpdate::Disconnected => {
            // Durante a espera após iniciar o núcleo, continua "Conectando…".
            if app.get_connection() != Connection::Connecting.code() {
                set_connection(app, Connection::Stopped);
            }
        }
        UiUpdate::Sessions(sessions) => show_sessions(app, &sessions),
        UiUpdate::Live(frame) => show_live(app, &frame),
        // Avisos: tela de uma próxima tarefa.
        UiUpdate::Notice(_) => {}
    }
}

/// Dados sintéticos do modo demo (`AXIS_UI_DEMO=1`).
fn demo_updates() -> Vec<UiUpdate> {
    let pad = |id: &str, owner: &str, color, family| SessionInfo {
        id: id.into(),
        owner: owner.into(),
        color,
        family,
        player: None,
        connected: true,
        experimental: false,
        view_only: false,
        battery: None,
        charging: false,
    };
    vec![
        UiUpdate::Connected {
            paused: false,
            emulation: true,
        },
        UiUpdate::Sessions(vec![
            SessionInfo {
                player: Some(1),
                battery: Some(64),
                ..pad("demo:01", "Maria", [0x5f, 0xb3, 0x7a], Family::DualShock4)
            },
            SessionInfo {
                player: Some(2),
                connected: false,
                ..pad("demo:02", "João", [0x9b, 0x7a, 0xd8], Family::DualSense)
            },
            SessionInfo {
                view_only: true,
                battery: Some(91),
                ..pad("demo:03", "Ana", [0xe0, 0xb0, 0x4a], Family::Xbox)
            },
        ]),
    ]
}

/// Inicia o núcleo e mostra "Conectando…" por até 3 s antes de "parado".
fn start_and_wait(app: &App) {
    if !client::start_daemon() {
        set_connection(app, Connection::Stopped);
        return;
    }
    set_connection(app, Connection::Connecting);
    let generation = GRACE.with(|g| {
        g.set(g.get().wrapping_add(1));
        g.get()
    });
    let weak = app.as_weak();
    slint::Timer::single_shot(START_GRACE, move || {
        if GRACE.with(Cell::get) != generation {
            return;
        }
        if let Some(app) = weak.upgrade()
            && app.get_connection() == Connection::Connecting.code()
        {
            set_connection(&app, Connection::Stopped);
        }
    });
}

/// Modo demo: quadros sintéticos a ~60 Hz para o controle selecionado, para o
/// palco parecer vivo na captura. O temporizador vive enquanto o valor viver.
fn demo_live(app: &App) -> slint::Timer {
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    let start = std::time::Instant::now();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            let Some(app) = weak.upgrade() else { return };
            let Some(id) = SHELL.with(|s| s.borrow().selected.clone()) else {
                return;
            };
            let frame = demo_frame(id, start.elapsed().as_secs_f32());
            apply(&app, UiUpdate::Live(frame), false);
        },
    );
    timer
}

/// Analógico esquerdo em círculo lento com drift corrigido e curva; direito
/// em oito; gatilhos oscilando; um botão por segundo.
fn demo_frame(id: String, t: f32) -> LiveFrame {
    use std::f32::consts::TAU;
    let a = t * TAU / 4.0;
    let raw = [0.72 * a.cos() + 0.03, 0.72 * a.sin() - 0.02];
    let drift = [raw[0] - 0.03, raw[1] + 0.02];
    let r = drift[0].hypot(drift[1]).max(f32::EPSILON);
    let k = r.powf(1.4) / r;
    let curve = [drift[0] * k, drift[1] * k];
    let left = StickStages {
        raw,
        calibrated: raw,
        drift,
        deadzone: drift,
        curve,
        anti: curve,
    };
    let rp = [0.5 * (1.3 * a).sin(), 0.35 * (2.6 * a).sin()];
    let right = StickStages {
        raw: rp,
        calibrated: rp,
        drift: rp,
        deadzone: rp,
        curve: rp,
        anti: rp,
    };
    const CYCLE: [u32; 6] = [0, 1, 3, 2, 12, 15];
    let buttons = if t.fract() < 0.35 {
        1 << CYCLE[t as usize % CYCLE.len()]
    } else {
        0
    };
    let pad = |l: &StickStages, r: &StickStages, pick: fn(&StickStages) -> [f32; 2]| {
        let (pl, pr) = (pick(l), pick(r));
        PadState {
            lx: pl[0],
            ly: pl[1],
            rx: pr[0],
            ry: pr[1],
            l2: ((t * 1.7).sin() + 1.0) / 2.0,
            r2: (t * 0.9).sin().max(0.0),
            buttons,
        }
    };
    LiveFrame {
        id,
        input: pad(&left, &right, |s| s.raw),
        output: pad(&left, &right, |s| s.anti),
        left,
        right,
        rate_hz: 1000,
        latency_p99_us: 48 + (6.0 * (t * 0.7).sin().abs()) as u32,
        battery: Some(64),
        charging: false,
    }
}
