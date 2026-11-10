#![windows_subsystem = "windows"]

mod client;
mod repair_view;
mod single;
mod theme;
mod view;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axis_protocol::{
    CaptureKindDto, CaptureResultDto, CaptureState, ClientMsg, CorrectionsDto, Family, LiveFrame,
    PadState, SessionInfo, StickStages, Transport, TriggerCalDto,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use client::{Client, UiUpdate};
use repair_view::{Stick, Wizard, WizardStep};
use view::{Connection, PadCard};

slint::include_modules!();

/// Quanto tempo "Conectando ao núcleo…" fica depois de iniciar o núcleo.
const START_GRACE: Duration = Duration::from_secs(3);

/// Telas da navegação (posição do item na barra lateral).
const SCREEN_OVERVIEW: i32 = 0;
const SCREEN_REPAIR: i32 = 2;

/// Pontos crus na nuvem do drift e no rastro do alcance.
const PLOT_POINTS: usize = 240;

/// Linha depois de enviar correções.
const APPLIED: &str = "Pronto: correção aplicada";
const UNDONE: &str = "Desfeito: as correções de antes voltaram";
const RESTORED: &str = "Padrão restaurado";
/// Começo do aviso do núcleo quando recusa um `SetCorrections`.
const NOT_APPLIED: &str = "Ajuste não aplicado";

/// Estado da interface que não mora no Slint (só na thread da interface).
#[derive(Default)]
struct Shell {
    /// `None` no modo demo: o que seria enviado ao núcleo é descartado.
    client: Option<Client>,
    /// Modo demo: o controle falso responde às medições.
    demo: bool,
    /// Medição do controle falso: tipo e início.
    demo_capture: Option<(CaptureKindDto, Instant)>,
    /// Cartões mostrados agora; o modelo só é recriado quando mudam.
    cards: Vec<PadCard>,
    selected: Option<String>,
    /// Última lista do núcleo (para o cabeçalho e para filtrar quadros).
    sessions: Vec<SessionInfo>,
    /// O núcleo tem barramento virtual (do `Connected`).
    emulation: bool,
    /// Já chegou um quadro do controle selecionado desde a seleção.
    has_live: bool,
    screen: i32,
    wizard: Wizard,
    right_stick: bool,
    /// Medição pedida por esta interface: controle e tipo. Só ela aparece.
    capture: Option<(String, CaptureKindDto)>,
    /// Já veio um quadro "medindo" dela; antes disso, um resultado no quadro
    /// é o da medição anterior, que o núcleo guarda até a próxima começar.
    capture_fresh: bool,
    step: WizardStep,
    /// Anel fixo dos últimos `PLOT_POINTS` pontos crus do analógico medido:
    /// o mesmo modelo do Slint, trocado uma linha por quadro.
    points: Rc<VecModel<PlotPoint>>,
    /// Próxima posição do anel.
    head: usize,
    /// Desfazer: controle e as correções dele logo antes da última aplicação.
    undo: Option<(String, CorrectionsDto)>,
    /// O que está digitado nos campos do especialista.
    fields: Vec<String>,
    /// Modo demo: lista de controles a republicar (como o núcleo faz depois
    /// de um `SetCorrections`).
    demo_sessions: Option<Vec<SessionInfo>>,
}

thread_local! {
    /// Geração da espera após iniciar o núcleo (só na thread da interface):
    /// um temporizador antigo não encerra uma espera mais nova.
    static GRACE: Cell<u32> = const { Cell::new(0) };
    static SHELL: RefCell<Shell> = RefCell::default();
}

fn main() -> Result<(), slint::PlatformError> {
    // Uma interface só (também no modo demo): a segunda traz a primeira para
    // frente e sai, antes de criar qualquer janela.
    let Some(_single) = single::SingleUi::acquire() else {
        single::focus_existing();
        return Ok(());
    };
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
    setup_repair(&app);

    // Modo demo: dados sintéticos, sem pipe e sem iniciar o núcleo (só para
    // captura de tela e desenvolvimento).
    if std::env::var_os("AXIS_UI_DEMO").is_some_and(|v| v == "1") {
        SHELL.with(|s| s.borrow_mut().demo = true);
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
/// o arraste é do `WindowMoveArea`, que pede ao winit o arraste do sistema; o
/// redimensionar é do `resize-border-width` da janela).
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
        let mut s = s.borrow_mut();
        if let Some(client) = &s.client {
            client.send(msg);
        } else if s.demo {
            // O controle falso do modo demo mede sozinho.
            match msg {
                ClientMsg::StartCapture { kind, .. } => {
                    s.demo_capture = Some((kind, Instant::now()));
                }
                ClientMsg::CancelCapture { .. } => s.demo_capture = None,
                ClientMsg::SetCorrections { id, corrections } => {
                    let mut sessions = s.sessions.clone();
                    if let Some(x) = sessions.iter_mut().find(|x| x.id == id) {
                        x.corrections = corrections;
                    }
                    s.demo_sessions = Some(sessions);
                }
                _ => {}
            }
        }
    });
}

/// Navegação, assistentes e medição da tela Correção.
fn setup_repair(app: &App) {
    let weak = app.as_weak();
    app.on_navigate(move |screen| {
        let Some(app) = weak.upgrade() else { return };
        if screen != SCREEN_OVERVIEW && screen != SCREEN_REPAIR {
            return;
        }
        let previous = SHELL.with(|s| std::mem::replace(&mut s.borrow_mut().screen, screen));
        if previous == screen {
            return;
        }
        // Sair da Correção no meio de uma medição a cancela.
        leave_capture(&app);
        app.set_screen(screen);
        refresh_overview(&app);
    });
    let weak = app.as_weak();
    app.on_choose_wizard(move |index| {
        let Some(app) = weak.upgrade() else { return };
        let wizard = Wizard::from_index(index);
        if SHELL.with(|s| std::mem::replace(&mut s.borrow_mut().wizard, wizard)) != wizard {
            leave_capture(&app);
        }
        app.set_wizard(wizard.index());
    });
    let weak = app.as_weak();
    app.on_choose_stick(move |right| {
        let Some(app) = weak.upgrade() else { return };
        if SHELL.with(|s| std::mem::replace(&mut s.borrow_mut().right_stick, right)) != right {
            leave_capture(&app);
        }
        app.set_right_stick(right);
    });
    let weak = app.as_weak();
    app.on_start_capture(move || {
        if let Some(app) = weak.upgrade() {
            start_capture(&app);
        }
    });
    let weak = app.as_weak();
    app.on_reset_wizard(move || {
        if let Some(app) = weak.upgrade() {
            leave_capture(&app);
        }
    });

    let work = app.global::<RepairWork>();
    let points = SHELL.with(|s| {
        let s = s.borrow();
        s.points.set_vec(vec![PlotPoint::default(); PLOT_POINTS]);
        s.points.clone()
    });
    work.set_points(ModelRc::from(points));
    work.set_error_index(-1);
    work.on_field_edited(|index, text| {
        SHELL.with(|s| {
            let mut s = s.borrow_mut();
            if let Some(field) = usize::try_from(index)
                .ok()
                .and_then(|i| s.fields.get_mut(i))
            {
                *field = text.to_string();
            }
        });
    });
    let weak = app.as_weak();
    work.on_apply_fields(move || {
        if let Some(app) = weak.upgrade() {
            apply_fields(&app);
        }
    });
    let weak = app.as_weak();
    work.on_restore_default(move || {
        if let Some(app) = weak.upgrade() {
            restore_default(&app);
        }
    });
    let weak = app.as_weak();
    work.on_undo(move || {
        if let Some(app) = weak.upgrade() {
            undo_correction(&app);
        }
    });
}

/// Controle, assistente, analógico e correções atuais do selecionado.
fn target() -> Option<(String, Wizard, Stick, CorrectionsDto)> {
    SHELL.with(|s| {
        let s = s.borrow();
        let id = s.selected.clone()?;
        let cur = s.sessions.iter().find(|x| x.id == id)?.corrections.clone();
        Some((id, s.wizard, Stick::from_right(s.right_stick), cur))
    })
}

/// Envia correções novas guardando as de agora (da `SessionInfo`) para o
/// Desfazer, e mostra `done` com o botão Desfazer.
fn send_corrections(
    app: &App,
    id: String,
    before: CorrectionsDto,
    next: CorrectionsDto,
    done: &str,
) {
    SHELL.with(|s| s.borrow_mut().undo = Some((id.clone(), before)));
    send(ClientMsg::SetCorrections {
        id,
        corrections: next,
    });
    let work = app.global::<RepairWork>();
    work.set_done(done.into());
    work.set_can_undo(true);
}

/// Desfazer: reenvia as correções de antes, só para o mesmo controle.
fn undo_correction(app: &App) {
    let msg = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let (id, before) = s.undo.take()?;
        (s.selected.as_deref() == Some(id.as_str())).then_some(ClientMsg::SetCorrections {
            id,
            corrections: before,
        })
    });
    let work = app.global::<RepairWork>();
    work.set_can_undo(false);
    if let Some(msg) = msg {
        send(msg);
        work.set_done(UNDONE.into());
    } else {
        work.set_done("".into());
    }
}

/// "Aplicar" do especialista: todos os campos valem ou nada é enviado.
fn apply_fields(app: &App) {
    let Some((id, wizard, stick, cur)) = target() else {
        return;
    };
    let texts = SHELL.with(|s| s.borrow().fields.clone());
    let work = app.global::<RepairWork>();
    match repair_view::corrections_from_fields(&cur, wizard, stick, &texts) {
        Ok(next) => {
            work.set_error_index(-1);
            work.set_field_error("".into());
            send_corrections(app, id, cur, next, APPLIED);
        }
        Err((index, message)) => {
            work.set_error_index(i32::try_from(index).unwrap_or(-1));
            work.set_field_error(message.into());
        }
    }
}

/// "Restaurar padrão": neutro só daquele analógico e etapa.
fn restore_default(app: &App) {
    let Some((id, wizard, stick, cur)) = target() else {
        return;
    };
    if let Some(next) = repair_view::neutral(&cur, wizard, stick) {
        send_corrections(app, id, cur, next, RESTORED);
    }
}

/// Limpa gráficos, aplicação e edição (anel inclusive) ao começar ou voltar.
fn reset_work(app: &App) {
    SHELL.with(|s| {
        let mut s = s.borrow_mut();
        s.undo = None;
        s.fields.clear();
        s.head = 0;
        // Uma notificação só (o modelo continua o mesmo).
        s.points.set_vec(vec![PlotPoint::default(); PLOT_POINTS]);
    });
    let work = app.global::<RepairWork>();
    work.set_has_result(false);
    work.set_polygon("".into());
    work.set_warning("".into());
    work.set_done("".into());
    work.set_can_undo(false);
    work.set_editing(false);
    work.set_fields(ModelRc::default());
    work.set_field_error("".into());
    work.set_error_index(-1);
    work.set_lap(repair_view::lap_text(0.0).into());
}

/// Resultado válido de drift ou alcance: desenha e, no modo leigo, aplica na
/// hora; no especialista, abre os números para editar.
fn show_result(app: &App, id: String, result: &CaptureResultDto) {
    if !matches!(
        result,
        CaptureResultDto::Drift { .. } | CaptureResultDto::Range { .. }
    ) {
        // Gatilhos: assistente da próxima etapa.
        return;
    }
    let work = app.global::<RepairWork>();
    work.set_has_result(true);
    if let CaptureResultDto::Drift { center, radius, .. } = result {
        work.set_center_x(center[0]);
        work.set_center_y(center[1]);
        work.set_radius(*radius);
    }
    work.set_polygon(repair_view::range_polygon(result).into());
    work.set_warning(repair_view::capture_warning(result).unwrap_or("").into());
    if app.get_expert() {
        let fields = repair_view::fields_for(result);
        SHELL.with(|s| s.borrow_mut().fields = fields.iter().map(|f| f.value.clone()).collect());
        let model: Vec<RepairField> = fields
            .into_iter()
            .map(|f| RepairField {
                label: f.label.into(),
                value: f.value.into(),
            })
            .collect();
        work.set_fields(ModelRc::new(VecModel::from(model)));
        work.set_editing(true);
        return;
    }
    let Some((selected, _, stick, cur)) = target() else {
        return;
    };
    if selected != id {
        return;
    }
    if let Some(next) = repair_view::auto_apply(&cur, stick, result) {
        send_corrections(app, id, cur, next, APPLIED);
    }
}

/// O núcleo recusou correções: o aviso dele substitui o "Pronto" (e não há
/// o que desfazer).
fn show_notice(app: &App, text: &str) {
    let work = app.global::<RepairWork>();
    if text.starts_with(NOT_APPLIED) && !work.get_done().is_empty() {
        SHELL.with(|s| s.borrow_mut().undo = None);
        work.set_done(text.into());
        work.set_can_undo(false);
    }
}

/// "Começar": pede ao núcleo a medição do assistente atual.
fn start_capture(app: &App) {
    let msg = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let kind = repair_view::capture_kind(s.wizard, s.right_stick)?;
        let id = s.selected.clone()?;
        let connected = s.sessions.iter().any(|x| x.id == id && x.connected);
        if !connected || s.step == WizardStep::Measuring {
            return None;
        }
        s.capture = Some((id.clone(), kind));
        s.capture_fresh = false;
        s.step = WizardStep::Measuring;
        Some(ClientMsg::StartCapture { id, kind })
    });
    if let Some(msg) = msg {
        reset_work(app);
        show_step(app, WizardStep::Measuring, 0.0, String::new());
        send(msg);
    }
}

/// Volta o assistente ao início; uma medição em andamento é cancelada no
/// núcleo (trocar de controle, de assistente, de analógico ou de tela).
fn leave_capture(app: &App) {
    let cancel = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let measuring = s.step == WizardStep::Measuring;
        s.step = WizardStep::Intro;
        s.capture_fresh = false;
        s.capture.take().filter(|_| measuring).map(|(id, _)| id)
    });
    if let Some(id) = cancel {
        send(ClientMsg::CancelCapture { id });
    }
    reset_work(app);
    show_step(app, WizardStep::Intro, 0.0, String::new());
}

fn show_step(app: &App, step: WizardStep, progress: f32, message: String) {
    app.set_repair_step(step.code());
    app.set_repair_progress(progress);
    app.set_repair_message(message.into());
    app.global::<RepairWork>()
        .set_lap(repair_view::lap_text(progress).into());
}

/// Ponto cru do analógico que está sendo medido, para o anel do gráfico.
fn raw_point(kind: CaptureKindDto, input: &PadState) -> Option<PlotPoint> {
    let (x, y) = match kind {
        CaptureKindDto::DriftLeft | CaptureKindDto::RangeLeft => (input.lx, input.ly),
        CaptureKindDto::DriftRight | CaptureKindDto::RangeRight => (input.rx, input.ry),
        CaptureKindDto::Triggers => return None,
    };
    // Um NaN do pipe não vira ponto (o Slint desenharia em lugar nenhum).
    (x.is_finite() && y.is_finite()).then_some(PlotPoint { x, y, on: true })
}

/// A medição pedida no quadro ao vivo: progresso e, no fim, o resultado.
fn show_capture(app: &App, frame: &LiveFrame) {
    let update = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        let (id, kind) = s.capture.clone()?;
        let state = frame.capture.as_ref().filter(|c| c.kind == kind);
        // Só quadros da medição em andamento (o que traz o resultado já é
        // depois do fim e fica fora do gráfico).
        if s.step == WizardStep::Measuring
            && frame.id == id
            && state.is_none_or(|c| c.result.is_none())
            && let Some(point) = raw_point(kind, &frame.input)
        {
            // Uma linha do anel por quadro (Live ≤ 60/s): nada é recriado.
            let head = s.head;
            s.points.set_row_data(head, point);
            s.head = (head + 1) % PLOT_POINTS;
        }
        if !s.capture_fresh {
            // Até o núcleo começar esta medição, o quadro traz nada ou o
            // resultado da anterior: continua "medindo" do zero.
            if state.is_none_or(|c| c.result.is_some()) {
                return None;
            }
            s.capture_fresh = true;
        }
        let step = repair_view::step_for(state, frame.id == id);
        let progress = state.map_or(0.0, |c| c.progress);
        let changed = std::mem::replace(&mut s.step, step) != step;
        if step == WizardStep::Intro {
            // O núcleo esqueceu a medição (reiniciou): volta ao começo.
            s.capture = None;
        }
        let result = state.and_then(|c| c.result.as_ref()).filter(|_| changed);
        let message = result.map(repair_view::capture_message).unwrap_or_default();
        // Só um resultado novo e válido é desenhado/aplicado, uma vez.
        let applied = result
            .filter(|_| step == WizardStep::Result)
            .map(|r| (id.clone(), r.clone()));
        (changed || step == WizardStep::Measuring).then_some((step, progress, message, applied))
    });
    if let Some((step, progress, message, applied)) = update {
        show_step(app, step, progress, message);
        if let Some((id, result)) = applied {
            show_result(app, id, &result);
        }
    }
}

/// O controle caiu no meio da medição: o assistente mostra o erro na hora
/// (o núcleo também marca a captura como abortada).
fn abort_if_disconnected(app: &App, connected: bool) {
    if !connected {
        abort_measuring(
            app,
            repair_view::capture_message(&CaptureResultDto::Aborted),
        );
    }
}

/// Mensagem do passo Erro quando a medição em andamento acaba por fora do
/// núcleo: a conexão com o núcleo caiu (a lista de controles inteira sumiu).
fn measuring_abort(update: &UiUpdate) -> Option<&'static str> {
    matches!(update, UiUpdate::Disconnected).then_some(repair_view::CORE_GONE)
}

/// Uma medição em andamento vira o passo Erro com `message`.
fn abort_measuring(app: &App, message: String) {
    let aborted = SHELL.with(|s| {
        let mut s = s.borrow_mut();
        if s.step != WizardStep::Measuring {
            return false;
        }
        s.step = WizardStep::Error;
        s.capture = None;
        true
    });
    if aborted {
        show_step(app, WizardStep::Error, 0.0, message);
    }
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
    app.set_status_active(view::status_active(app.get_paused(), emulation));
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
        let paused = app.get_paused();
        app.set_has_pad(true);
        app.set_header(header_data(view::header(
            index,
            session,
            s.emulation,
            paused,
        )));
        app.set_repair_header(header_data(view::repair_header(session)));
        app.set_repair_summary(repair_summary(&session.corrections));
        app.set_family(match session.family {
            Family::DualShock4 => 0,
            Family::DualSense => 1,
            Family::Xbox => 2,
        });
        app.set_emulating(s.emulation && !session.view_only);
        let (connected, view_only) = (session.connected, session.view_only);
        app.set_pad_connected(connected);
        if !connected {
            // Reconectando: palco esmaecido e valores em neutro.
            s.has_live = false;
            app.set_live(neutral_live());
        }
        show_badge(
            app,
            view::stage_badge(connected, paused, s.emulation, view_only, s.has_live),
        );
    });
    let connected = app.get_has_pad() && app.get_pad_connected();
    abort_if_disconnected(app, connected);
}

fn header_data(h: view::Header) -> OverviewHeader {
    OverviewHeader {
        kicker: h.kicker.into(),
        title: h.title.into(),
        subtitle: h.subtitle.into(),
    }
}

fn repair_summary(c: &CorrectionsDto) -> RepairSummary {
    let line = |w| repair_view::summary(w, c).into();
    RepairSummary {
        drift: line(Wizard::Drift),
        range: line(Wizard::Range),
        triggers: line(Wizard::Triggers),
        connection: line(Wizard::Connection),
    }
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
        jitter_us: 0,
        capture: None,
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
    show_capture(app, frame);
    if first {
        // Primeiro quadro: o selo do palco sai de "Aguardando dados".
        refresh_overview(app);
    }
}

/// Selo do canto do palco: bolinha ("Ao vivo") ou pílula de aviso.
fn show_badge(app: &App, badge: view::StageBadge) {
    app.set_stage_status(badge.text().into());
    app.set_stage_live(badge.is_live());
    app.set_stage_pill(badge.is_pill());
}

/// Seleciona um controle e pede ao núcleo os quadros ao vivo dele.
fn select(app: &App, id: Option<String>) {
    if SHELL.with(|s| s.borrow().selected != id) {
        // A medição é do controle anterior: cancela antes de trocar.
        leave_capture(app);
    }
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

/// A conexão caiu ou voltou: o último quadro ao vivo não vale mais.
fn clears_stage(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::Connected { .. } | UiUpdate::Disconnected)
}

fn apply(app: &App, update: UiUpdate, first_failure: bool) {
    if let Some(message) = measuring_abort(&update) {
        abort_measuring(app, message.into());
    }
    if clears_stage(&update) {
        // Nada de "Ao vivo" congelado: o palco espera o próximo quadro.
        SHELL.with(|s| s.borrow_mut().has_live = false);
        app.set_live(neutral_live());
        refresh_overview(app);
    }
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
        UiUpdate::State { paused, emulation } => {
            // Mudou fora daqui (ex.: pausa pela bandeja): reflete na hora.
            app.set_paused(paused);
            SHELL.with(|s| s.borrow_mut().emulation = emulation);
            refresh_status(app);
            refresh_overview(app);
        }
        // Por enquanto só a recusa de correções aparece (na Correção).
        UiUpdate::Notice(text) => show_notice(app, &text),
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
        transport: None,
        disconnects: 0,
        corrections: Default::default(),
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
                transport: Some(Transport::Bluetooth),
                disconnects: 1,
                corrections: demo_corrections(),
                ..pad("demo:01", "Maria", [0x5f, 0xb3, 0x7a], Family::DualShock4)
            },
            SessionInfo {
                player: Some(2),
                connected: false,
                transport: Some(Transport::Usb),
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

/// Correções já aplicadas no controle de demonstração.
fn demo_corrections() -> CorrectionsDto {
    let mut c = CorrectionsDto::default();
    c.left.center = [0.04, -0.03];
    c.left.drift_radius = 0.05;
    c.l2 = TriggerCalDto {
        rest: 0.04,
        max: 0.97,
    };
    c
}

/// Medição sintética do controle de demonstração, `secs` depois de começar:
/// progresso até o fim e então um resultado fixo (drift esquerdo ok, direito
/// desgastado; alcance esquerdo ok, direito incompleto; gatilhos com R2
/// apertado só 2 vezes).
fn demo_capture_state(kind: CaptureKindDto, secs: f32) -> CaptureState {
    let duration = match kind {
        CaptureKindDto::DriftLeft | CaptureKindDto::DriftRight => 3.0,
        CaptureKindDto::RangeLeft | CaptureKindDto::RangeRight => 6.0,
        CaptureKindDto::Triggers => 5.0,
    };
    let progress = (secs / duration).clamp(0.0, 1.0);
    let result = (progress >= 1.0).then(|| match kind {
        CaptureKindDto::DriftLeft => CaptureResultDto::Drift {
            center: [0.04, -0.03],
            noise: 0.008,
            radius: 0.03,
            worn: false,
        },
        CaptureKindDto::DriftRight => CaptureResultDto::Drift {
            center: [0.12, 0.02],
            noise: 0.03,
            radius: 0.06,
            worn: true,
        },
        CaptureKindDto::RangeRight => CaptureResultDto::Incomplete {
            reason: "Faltou girar até a borda em todas as direções.".into(),
        },
        CaptureKindDto::RangeLeft => CaptureResultDto::Range {
            // Gate quadrado gasto: diagonais mais longas que os eixos.
            outer_radius: (0..16)
                .map(|i| if i % 4 == 2 { 1.06 } else { 0.97 })
                .collect(),
            circularity_pct: 3.6,
        },
        CaptureKindDto::Triggers => CaptureResultDto::Triggers {
            l2: Some(TriggerCalDto {
                rest: 0.05,
                max: 0.96,
            }),
            r2: None,
            l2_presses: 3,
            r2_presses: 2,
        },
    });
    CaptureState {
        kind,
        progress,
        result,
    }
}

/// Analógico cru do controle de demonstração durante a medição: em repouso
/// com ruído perto do centro (drift) ou girando 3 voltas num "gate" de
/// diagonais mais longas (alcance).
fn demo_capture_input(kind: CaptureKindDto, secs: f32) -> Option<[f32; 2]> {
    use std::f32::consts::TAU;
    // Ruído determinístico em [−0,5; 0,5).
    let noise = |k: f32| ((secs * 997.0 + k).sin() * 43_758.547).rem_euclid(1.0) - 0.5;
    match kind {
        CaptureKindDto::DriftLeft => Some([0.04 + 0.02 * noise(0.0), -0.03 + 0.02 * noise(7.0)]),
        CaptureKindDto::DriftRight => Some([0.12 + 0.06 * noise(0.0), 0.02 + 0.06 * noise(7.0)]),
        CaptureKindDto::RangeLeft | CaptureKindDto::RangeRight => {
            let a = secs / 6.0 * 3.0 * TAU;
            let r = 0.97 + 0.09 * (2.0 * a).sin().abs().powi(3);
            Some([r * a.cos(), r * a.sin()])
        }
        CaptureKindDto::Triggers => None,
    }
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
            // Republica a lista depois de um `SetCorrections`, como o núcleo.
            if let Some(sessions) = SHELL.with(|s| s.borrow_mut().demo_sessions.take()) {
                apply(&app, UiUpdate::Sessions(sessions), false);
            }
            let Some((id, capture)) = SHELL.with(|s| {
                let s = s.borrow();
                let capture = s.demo_capture.map(|(kind, t0)| {
                    let secs = t0.elapsed().as_secs_f32();
                    (
                        demo_capture_state(kind, secs),
                        demo_capture_input(kind, secs),
                    )
                });
                s.selected.clone().map(|id| (id, capture))
            }) else {
                return;
            };
            let mut frame = demo_frame(id, start.elapsed().as_secs_f32());
            if let Some((state, input)) = capture {
                if let (None, Some([x, y])) = (&state.result, input) {
                    match state.kind {
                        CaptureKindDto::DriftLeft | CaptureKindDto::RangeLeft => {
                            (frame.input.lx, frame.input.ly) = (x, y);
                        }
                        _ => (frame.input.rx, frame.input.ry) = (x, y),
                    }
                }
                frame.capture = Some(state);
            }
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
        jitter_us: 0,
        capture: None,
        charging: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_capture_progresses_to_a_result() {
        use axis_protocol::{CaptureKindDto, CaptureResultDto};
        use repair_view::{WizardStep, step_for};

        let early = demo_capture_state(CaptureKindDto::DriftLeft, 1.5);
        assert_eq!(early.kind, CaptureKindDto::DriftLeft);
        assert!((early.progress - 0.5).abs() < 1e-6);
        assert_eq!(step_for(Some(&early), true), WizardStep::Measuring);

        let drift = demo_capture_state(CaptureKindDto::DriftLeft, 10.0);
        assert_eq!(drift.progress, 1.0);
        assert!(matches!(
            drift.result,
            Some(CaptureResultDto::Drift { worn: false, .. })
        ));
        // Direito desgastado (mostra o aviso); alcance direito incompleto.
        let worn = demo_capture_state(CaptureKindDto::DriftRight, 10.0);
        assert!(matches!(
            worn.result,
            Some(CaptureResultDto::Drift { worn: true, .. })
        ));
        let short = demo_capture_state(CaptureKindDto::RangeRight, 60.0);
        assert_eq!(step_for(Some(&short), true), WizardStep::Error);
        let range = demo_capture_state(CaptureKindDto::RangeLeft, 60.0);
        let Some(CaptureResultDto::Range { outer_radius, .. }) = range.result else {
            panic!("esperava Range: {range:?}");
        };
        assert_eq!(outer_radius.len(), 16);
        assert!(outer_radius.iter().all(|r| (0.3..=1.5).contains(r)));
        // Gatilhos: R2 apertado só 2 vezes fica sem medida.
        let triggers = demo_capture_state(CaptureKindDto::Triggers, 60.0);
        assert!(matches!(
            triggers.result,
            Some(CaptureResultDto::Triggers {
                l2: Some(_),
                r2: None,
                r2_presses: 2,
                ..
            })
        ));
    }

    #[test]
    fn core_drop_aborts_with_core_message() {
        assert_eq!(
            measuring_abort(&UiUpdate::Disconnected),
            Some("O núcleo do Axis parou durante a medição.")
        );
        for other in [
            UiUpdate::Incompatible,
            UiUpdate::Sessions(Vec::new()),
            UiUpdate::Notice(String::new()),
            UiUpdate::Connected {
                paused: false,
                emulation: true,
            },
        ] {
            assert_eq!(measuring_abort(&other), None);
        }
    }

    #[test]
    fn raw_point_follows_the_measured_stick() {
        let input = PadState {
            lx: 0.1,
            ly: -0.2,
            rx: 0.3,
            ry: 0.4,
            ..PadState::default()
        };
        let p = raw_point(CaptureKindDto::DriftLeft, &input).unwrap();
        assert_eq!((p.x, p.y, p.on), (0.1, -0.2, true));
        let p = raw_point(CaptureKindDto::RangeRight, &input).unwrap();
        assert_eq!((p.x, p.y), (0.3, 0.4));
        assert!(raw_point(CaptureKindDto::Triggers, &input).is_none());
        let nan = PadState {
            lx: f32::NAN,
            ..input
        };
        assert!(raw_point(CaptureKindDto::DriftLeft, &nan).is_none());
    }

    #[test]
    fn demo_capture_input_stays_in_the_plots() {
        for i in 0..300 {
            let t = i as f32 * 0.01;
            let [x, y] = demo_capture_input(CaptureKindDto::DriftLeft, t).unwrap();
            assert!((x - 0.04).abs() <= 0.011 && (y + 0.03).abs() <= 0.011);
            let [x, y] = demo_capture_input(CaptureKindDto::RangeLeft, t * 2.0).unwrap();
            let r = x.hypot(y);
            assert!((0.96..=1.07).contains(&r), "{r}");
        }
        assert!(demo_capture_input(CaptureKindDto::Triggers, 1.0).is_none());
    }

    #[test]
    fn link_changes_clear_the_stage() {
        assert!(clears_stage(&UiUpdate::Connected {
            paused: false,
            emulation: true
        }));
        assert!(clears_stage(&UiUpdate::Disconnected));
        assert!(!clears_stage(&UiUpdate::Incompatible));
        assert!(!clears_stage(&UiUpdate::Sessions(Vec::new())));
        assert!(!clears_stage(&UiUpdate::State {
            paused: true,
            emulation: true
        }));
        assert!(!clears_stage(&UiUpdate::Notice(String::new())));
    }
}
