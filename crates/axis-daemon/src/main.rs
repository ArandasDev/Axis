//! Binário `axis`: o núcleo que roda na bandeja.
#![windows_subsystem = "windows"]

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use axis_daemon::app::{
    DeferredNotifier, INSTANCE_NAME, RUN_VALUE, command_handler, load_settings_and_sync_autostart,
};
use axis_daemon::autostart::RunKey;
use axis_daemon::config::ConfigStore;
use axis_daemon::instance::SingleInstance;
use axis_daemon::ipc::spawn_ipc_server;
use axis_daemon::live::UiRegistry;
use axis_daemon::logging::init_logging;
use axis_daemon::manager::{Event, Manager, ManagerDeps, SessionView};
use axis_daemon::paths::AppPaths;
use axis_daemon::ports::{ControllerOpener, HidOpener, Notifier, ThreadScheduler};
use axis_daemon::tray::{QuitDone, Tray, TrayHandle, signal_quit_done};
use axis_devices::{Controller, DeviceError, HidCandidate, HotplugEvent, HotplugWatcher};
use axis_virtual::{DeviceHider, HidHide, ViGEmBus, VirtualBus};

fn main() {
    let Some(_instance) = SingleInstance::acquire(INSTANCE_NAME) else {
        return; // já está rodando
    };
    let paths = AppPaths::from_env();
    // Sem console (subsistema "windows"): se o log falhar, segue sem log.
    let _ = init_logging(&paths.logs_dir());
    log::info!("Axis {} iniciando", env!("CARGO_PKG_VERSION"));
    let exe = std::env::current_exe().unwrap_or_default();

    let mut settings_store = ConfigStore::new(paths.clone());
    let settings =
        load_settings_and_sync_autostart(&mut settings_store, &RunKey::user_run(RUN_VALUE), &exe);
    let settings_store = Arc::new(Mutex::new(settings_store));

    let bus: Option<Arc<dyn VirtualBus>> = match ViGEmBus::connect() {
        Ok(b) => Some(Arc::new(b)),
        Err(e) => {
            log::warn!("ViGEmBus indisponível: {e}");
            None
        }
    };
    let hider: Option<Arc<dyn DeviceHider>> = match HidHide::open(&exe) {
        Ok(h) => Some(Arc::new(h)),
        Err(e) => {
            log::warn!("HidHide indisponível: {e}");
            None
        }
    };

    let (tx, rx) = mpsc::channel::<Event>();
    let opener: Box<dyn ControllerOpener> = match HidOpener::new() {
        Ok(o) => Box::new(o),
        Err(e) => {
            log::error!("HID indisponível: {e}");
            Box::new(NoDevices)
        }
    };
    let notifier = DeferredNotifier::<TrayHandle>::new();
    // O gerente alimenta o registro; o servidor do pipe o entrega à interface.
    let ui = UiRegistry::new();
    let mut manager = Manager::new(ManagerDeps {
        opener,
        bus,
        hider,
        store: ConfigStore::new(paths.clone()),
        scheduler: Box::new(ThreadScheduler::new(tx.clone())),
        notifier: Box::new(notifier.clone()),
        events: tx.clone(),
        ui: ui.clone(),
    });
    let _ipc = spawn_ipc_server(axis_protocol::PIPE_NAME, ui.clone(), tx.clone());

    let sessions = Arc::new(Mutex::new(Vec::new()));
    let autostart = Arc::new(AtomicBool::new(settings.autostart));
    let on_command = command_handler(tx.clone(), settings_store.clone(), exe, autostart.clone());
    let quit_done = QuitDone::default();
    let (tray, tray_thread) =
        Tray::spawn(on_command, sessions.clone(), autostart, quit_done.clone());
    notifier.attach(tray.clone());

    let hotplug_tx = tx.clone();
    let _hotplug = match HotplugWatcher::start(move |ev| {
        // Só envia: a callback roda numa thread do sistema e não pode bloquear.
        let ev = match ev {
            HotplugEvent::Arrived(p) => Event::DeviceArrived(p),
            HotplugEvent::Removed(p) => Event::DeviceRemoved(p),
        };
        let _ = hotplug_tx.send(ev);
    }) {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("sem aviso de conexão de dispositivos: {e}");
            None
        }
    };
    drop(tx);

    manager.start();
    publish(&manager, &sessions, &settings_store, &tray);
    while let Ok(ev) = rx.recv() {
        if !manager.handle(ev) {
            break;
        }
        publish(&manager, &sessions, &settings_store, &tray);
    }

    // Libera um fim de sessão do Windows que espera o encerramento do gerente.
    signal_quit_done(&quit_done);
    tray.close();
    let _ = tray_thread.join();
    log::info!("encerrado");
    // Dá tempo à thread do log de gravar as últimas linhas.
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// Atualiza a lista do menu e mostra os avisos das configurações.
fn publish(
    manager: &Manager,
    sessions: &Mutex<Vec<SessionView>>,
    store: &Mutex<ConfigStore>,
    tray: &TrayHandle,
) {
    *sessions.lock().unwrap_or_else(|e| e.into_inner()) = manager.sessions();
    let notices = store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take_notices();
    for n in notices {
        tray.notify("Axis", &n);
    }
}

/// Usado quando a API HID não abre: o Axis fica sem controles, mas a bandeja funciona.
struct NoDevices;

impl ControllerOpener for NoDevices {
    fn enumerate(&mut self) -> Vec<HidCandidate> {
        Vec::new()
    }

    fn open(&mut self, _c: &HidCandidate) -> Result<Box<dyn Controller>, DeviceError> {
        Err(DeviceError::Hid("HID indisponível".into()))
    }
}
