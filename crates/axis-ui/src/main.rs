#![windows_subsystem = "windows"]

mod client;
mod theme;
mod view;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axis_protocol::{ClientMsg, Family, SessionInfo};
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
    let count = SHELL.with(|s| s.borrow().cards.len());
    app.set_status(view::status_line(app.get_paused(), count).into());
}

/// Seleciona um controle e pede ao núcleo os quadros ao vivo dele.
fn select(app: &App, id: Option<String>) {
    let changed = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let changed = s.selected != id;
        s.selected = id.clone();
        changed
    });
    if changed {
        app.set_selected_id(id.clone().unwrap_or_default().into());
        send(ClientMsg::Inspect { id });
    }
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
        UiUpdate::Connected { paused, .. } => {
            set_connection(app, Connection::Connected);
            // O estado do núcleo é a verdade: nada de alternar localmente.
            app.set_paused(paused);
            refresh_status(app);
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
        // Quadros ao vivo e avisos: telas das próximas tarefas.
        UiUpdate::Live(_) | UiUpdate::Notice(_) => {}
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
