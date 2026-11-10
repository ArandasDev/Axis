//! Peças da composição do binário `axis` (usadas por `main.rs`).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::autostart::RunKey;
use crate::config::{AppSettings, ConfigStore};
use crate::manager::Event;
use crate::ports::Notifier;
use crate::tray::TrayCommand;
use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

/// Nome do mutex de instância única.
pub const INSTANCE_NAME: &str = "Local\\AxisDaemon";
/// Nome do valor na chave Run do usuário.
pub const RUN_VALUE: &str = "Axis";

/// Notificador que guarda as mensagens até receber o notificador real.
///
/// O gerente é criado antes da bandeja; o que ele avisar nesse intervalo é
/// entregue assim que a bandeja existir.
pub struct DeferredNotifier<N> {
    slot: Arc<Mutex<Slot<N>>>,
}

struct Slot<N> {
    target: Option<N>,
    pending: Vec<(String, String)>,
}

impl<N> Clone for DeferredNotifier<N> {
    fn clone(&self) -> Self {
        DeferredNotifier {
            slot: self.slot.clone(),
        }
    }
}

impl<N: Notifier> Default for DeferredNotifier<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Notifier> DeferredNotifier<N> {
    pub fn new() -> Self {
        DeferredNotifier {
            slot: Arc::new(Mutex::new(Slot {
                target: None,
                pending: Vec::new(),
            })),
        }
    }

    /// Entrega o que estava pendente e passa a repassar direto para `target`.
    pub fn attach(&self, target: N) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        for (title, body) in std::mem::take(&mut slot.pending) {
            target.notify(&title, &body);
        }
        slot.target = Some(target);
    }
}

impl<N: Notifier> Notifier for DeferredNotifier<N> {
    fn notify(&self, title: &str, body: &str) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        match &slot.target {
            Some(t) => t.notify(title, body),
            None => slot.pending.push((title.to_string(), body.to_string())),
        }
    }
}

/// Nome do executável da interface (na mesma pasta do `axis.exe`).
pub const UI_EXE: &str = "axis-ui.exe";

/// `CREATE_NO_WINDOW` do Win32 (sem console para o processo filho).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Caminho da interface ao lado do executável do núcleo.
pub fn ui_exe_path(daemon_exe: &Path) -> Option<PathBuf> {
    let dir = daemon_exe.parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    Some(dir.join(UI_EXE))
}

/// Abre a interface. Se ela já estiver aberta, a nova instância traz a
/// existente para frente e sai (mutex `Local\AxisUi` na interface). Arquivo
/// ausente ou falha ao iniciar viram notificação.
pub fn open_ui(
    daemon_exe: &Path,
    notifier: &dyn Notifier,
    spawn: impl FnOnce(&Path) -> std::io::Result<()>,
) {
    let Some(ui) = ui_exe_path(daemon_exe).filter(|p| p.is_file()) else {
        log::warn!(
            "interface não encontrada ao lado de {}",
            daemon_exe.display()
        );
        notifier.notify("Axis", "Interface do Axis não encontrada");
        return;
    };
    if let Err(e) = spawn(&ui) {
        log::warn!("não foi possível abrir a interface: {e}");
        notifier.notify("Axis", "Não foi possível abrir a interface do Axis");
    }
}

/// Inicia `axis-ui.exe` sem console. Antes, libera o primeiro plano para que a
/// interface (nova, ou a existente chamada pela nova) possa aparecer na frente.
pub fn spawn_ui(ui: &Path) -> std::io::Result<()> {
    // SAFETY: sem ponteiros; só repassa o direito de primeiro plano desta thread.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    let mut cmd = Command::new(ui);
    if let Some(dir) = ui.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
}

/// Liga ou desliga o início com o Windows, registrando falhas no log.
fn apply_run_key(key: &RunKey, exe: &Path, enabled: bool) {
    let result = if enabled {
        key.enable(exe)
    } else {
        key.disable()
    };
    if let Err(e) = result {
        log::warn!("não foi possível ajustar o início com o Windows: {e}");
    }
}

/// Carrega as configurações e ajusta a chave Run. Na primeira execução (sem
/// `settings.toml`) o início com o Windows é ligado e as configurações gravadas.
pub fn load_settings_and_sync_autostart(
    store: &mut ConfigStore,
    key: &RunKey,
    exe: &Path,
) -> AppSettings {
    let first_run = !store.paths().settings_file().exists();
    let mut settings = store.load_settings();
    if first_run {
        settings.autostart = true;
        if let Err(e) = store.save_settings(&settings) {
            log::warn!("não foi possível gravar as configurações: {e}");
        }
    }
    apply_run_key(key, exe, settings.autostart);
    settings
}

/// Inverte `autostart`: grava as configurações, ajusta a chave Run e o indicador do menu.
pub fn toggle_autostart(store: &mut ConfigStore, key: &RunKey, exe: &Path, flag: &AtomicBool) {
    let mut settings = store.load_settings();
    settings.autostart = !settings.autostart;
    if let Err(e) = store.save_settings(&settings) {
        log::warn!("não foi possível gravar as configurações: {e}");
    }
    apply_run_key(key, exe, settings.autostart);
    flag.store(settings.autostart, Ordering::SeqCst);
    log::info!("início com o Windows: {}", settings.autostart);
}

/// Traduz um comando da bandeja num evento do gerente (`ToggleAutostart` e
/// `OpenUi` não viram evento: são tratados na própria bandeja).
pub fn command_event(cmd: TrayCommand) -> Option<Event> {
    match cmd {
        TrayCommand::Pause => Some(Event::SetPaused(true)),
        TrayCommand::Resume => Some(Event::SetPaused(false)),
        TrayCommand::Restore => Some(Event::RestoreControllers),
        TrayCommand::Quit => Some(Event::Quit),
        TrayCommand::ToggleAutostart | TrayCommand::OpenUi => None,
    }
}

/// Monta o tratador de comandos da bandeja: comandos viram `Event`s, exceto
/// `ToggleAutostart`, que grava as configurações e a chave Run ali mesmo, e
/// `OpenUi`, que abre a interface ao lado de `exe` (avisando por `notifier`).
pub fn command_handler(
    events: Sender<Event>,
    store: Arc<Mutex<ConfigStore>>,
    exe: PathBuf,
    autostart: Arc<AtomicBool>,
    notifier: impl Notifier + 'static,
) -> impl Fn(TrayCommand) + Send + 'static {
    move |cmd| match cmd {
        TrayCommand::OpenUi => open_ui(&exe, &notifier, spawn_ui),
        TrayCommand::ToggleAutostart => {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            toggle_autostart(&mut store, &RunKey::user_run(RUN_VALUE), &exe, &autostart);
        }
        _ => {
            if let Some(ev) = command_event(cmd) {
                let _ = events.send(ev);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::FakeNotifier;

    #[test]
    fn deferred_notifier_buffers_until_attached() {
        let late = DeferredNotifier::<FakeNotifier>::new();
        late.notify("Axis", "um");
        let fake = FakeNotifier::default();
        let messages = fake.messages.clone();
        late.attach(fake);
        assert_eq!(*messages.lock().unwrap(), [("Axis".into(), "um".into())]);
        late.clone().notify("Axis", "dois");
        assert_eq!(messages.lock().unwrap().len(), 2);
    }

    #[test]
    fn open_ui_is_not_a_manager_event() {
        assert!(command_event(TrayCommand::OpenUi).is_none());
    }

    #[test]
    fn ui_exe_sits_next_to_the_daemon() {
        assert_eq!(
            ui_exe_path(Path::new(r"C:\Axis\axis.exe")),
            Some(PathBuf::from(r"C:\Axis\axis-ui.exe"))
        );
        assert_eq!(ui_exe_path(Path::new("")), None);
    }

    #[test]
    fn open_ui_without_the_file_notifies_and_does_not_spawn() {
        let dir = std::env::temp_dir().join(format!("axis-open-missing-{}", std::process::id()));
        let fake = FakeNotifier::default();
        let mut spawned = false;
        open_ui(&dir.join("axis.exe"), &fake, |_| {
            spawned = true;
            Ok(())
        });
        assert!(!spawned);
        assert_eq!(
            *fake.messages.lock().unwrap(),
            [("Axis".into(), "Interface do Axis não encontrada".into())]
        );
    }

    #[test]
    fn open_ui_spawns_the_ui_next_to_the_daemon() {
        let dir = std::env::temp_dir().join(format!("axis-open-found-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ui = dir.join("axis-ui.exe");
        std::fs::write(&ui, b"").unwrap();
        let fake = FakeNotifier::default();
        let mut spawned = None;
        open_ui(&dir.join("axis.exe"), &fake, |p| {
            spawned = Some(p.to_path_buf());
            Ok(())
        });
        assert_eq!(spawned, Some(ui));
        assert!(fake.messages.lock().unwrap().is_empty());

        // Falha ao iniciar: avisa o usuário em vez de ficar em silêncio.
        open_ui(&dir.join("axis.exe"), &fake, |_| {
            Err(std::io::Error::other("negado"))
        });
        assert_eq!(
            *fake.messages.lock().unwrap(),
            [(
                "Axis".into(),
                "Não foi possível abrir a interface do Axis".into()
            )]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
