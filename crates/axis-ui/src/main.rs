#![windows_subsystem = "windows"]

mod client;
mod theme;
mod view;

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slint::ComponentHandle;

use client::{Client, UiUpdate};
use view::Connection;

slint::include_modules!();

/// Quanto tempo "Conectando ao núcleo…" fica depois de iniciar o núcleo.
const START_GRACE: Duration = Duration::from_secs(3);

thread_local! {
    /// Geração da espera após iniciar o núcleo (só na thread da interface):
    /// um temporizador antigo não encerra uma espera mais nova.
    static GRACE: Cell<u32> = const { Cell::new(0) };
}

fn main() -> Result<(), slint::PlatformError> {
    let app = App::new()?;
    let [r, g, b] = theme::accent_rgb("laranja");
    app.global::<Tokens>()
        .set_accent(slint::Color::from_rgb_u8(r, g, b));
    set_connection(&app, Connection::Connecting);

    let weak = app.as_weak();
    app.on_start_daemon(move || {
        if let Some(app) = weak.upgrade() {
            start_and_wait(&app);
        }
    });

    // Só a primeira resposta decide se o núcleo é iniciado automaticamente.
    let first_seen = AtomicBool::new(false);
    let weak = app.as_weak();
    let _client = Client::spawn(move |update| {
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

    app.run()
}

fn set_connection(app: &App, c: Connection) {
    app.set_connection(c.code());
    app.set_connection_message(view::connection_message(c).into());
}

fn apply(app: &App, update: UiUpdate, first_failure: bool) {
    match update {
        UiUpdate::Connected { .. } => set_connection(app, Connection::Connected),
        UiUpdate::Incompatible => set_connection(app, Connection::Incompatible),
        UiUpdate::Disconnected if first_failure => start_and_wait(app),
        UiUpdate::Disconnected => {
            // Durante a espera após iniciar o núcleo, continua "Conectando…".
            if app.get_connection() != Connection::Connecting.code() {
                set_connection(app, Connection::Stopped);
            }
        }
        // Sessões, quadros ao vivo e avisos: telas das próximas tarefas.
        UiUpdate::Sessions(_) | UiUpdate::Live(_) | UiUpdate::Notice(_) => {}
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
