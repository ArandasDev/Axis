//! Peças da composição do binário `axis` (usadas por `main.rs`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::autostart::RunKey;
use crate::config::{AppSettings, ConfigStore};
use crate::manager::Event;
use crate::ports::Notifier;
use crate::tray::TrayCommand;

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

/// Traduz um comando da bandeja num evento do gerente (`ToggleAutostart` não vira evento).
pub fn command_event(cmd: TrayCommand) -> Option<Event> {
    match cmd {
        TrayCommand::Pause => Some(Event::SetPaused(true)),
        TrayCommand::Resume => Some(Event::SetPaused(false)),
        TrayCommand::Restore => Some(Event::RestoreControllers),
        TrayCommand::Quit => Some(Event::Quit),
        TrayCommand::ToggleAutostart => None,
    }
}

/// Monta o tratador de comandos da bandeja: comandos viram `Event`s, exceto
/// `ToggleAutostart`, que grava as configurações e a chave Run ali mesmo.
pub fn command_handler(
    events: Sender<Event>,
    store: Arc<Mutex<ConfigStore>>,
    exe: PathBuf,
    autostart: Arc<AtomicBool>,
) -> impl Fn(TrayCommand) + Send + 'static {
    move |cmd| match command_event(cmd) {
        Some(ev) => {
            let _ = events.send(ev);
        }
        None => {
            let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
            toggle_autostart(&mut store, &RunKey::user_run(RUN_VALUE), &exe, &autostart);
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
}
