//! Gerente: laço de eventos, sessões por controle e fluxo de chegada.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use axis_core::{Corrections, DeviceFamily, Pipeline, Profile};
use axis_devices::{Controller, HidCandidate};
use axis_virtual::{DeviceHider, Rumble, VirtualBus, instance_id_from_path};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigStore, OWNER_PALETTE};
use crate::ports::{ControllerOpener, Notifier, Scheduler};
use crate::store::{Loaded, load_toml, write_atomic};
use crate::worker::{SharedPad, WorkerCommand, WorkerExit, WorkerStatus, spawn_worker};

const NO_VIGEM_NOTICE: &str = "ViGEmBus não encontrado: o Axis só mostra os controles, sem emular.";
const COLOR_NAMES: [&str; 6] = ["verde", "roxo", "amarelo", "azul", "laranja", "rosa"];
/// Intervalo mínimo entre duas reaberturas automáticas após erro do mesmo controle.
const ERROR_REOPEN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug)]
pub enum Event {
    DeviceArrived(String),
    DeviceRemoved(String),
    WorkerExited { session: u64, exit: WorkerExit },
    GraceExpired { id: String, generation: u64 },
    Rumble { id: String, rumble: Rumble },
    SetPaused(bool),
    RestoreControllers,
    Quit,
    Rescan,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionView {
    pub id: String,
    pub owner: String,
    pub color: [u8; 3],
    pub family: DeviceFamily,
    pub player: Option<u8>,
    pub connected: bool,
    pub experimental: bool,
    pub view_only: bool,
}

pub struct ManagerDeps {
    pub opener: Box<dyn ControllerOpener>,
    pub bus: Option<Arc<dyn VirtualBus>>,
    pub hider: Option<Arc<dyn DeviceHider>>,
    pub store: ConfigStore,
    pub scheduler: Box<dyn Scheduler>,
    pub notifier: Box<dyn Notifier>,
    pub events: Sender<Event>,
}

/// Estado de um controle conhecido. `connected = false` significa período de graça.
struct Session {
    number: u64,
    id: String,
    path: String,
    owner: String,
    color: [u8; 3],
    family: DeviceFamily,
    player: Option<u8>,
    connected: bool,
    experimental: bool,
    view_only: bool,
    tx: Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
    #[allow(dead_code)] // lido pela bandeja/UI nas próximas tasks
    status: Arc<Mutex<WorkerStatus>>,
    pad: Option<SharedPad>,
    /// Geração da última desconexão (valida o `GraceExpired`).
    generation: u64,
    /// Quando ocorreu a última reabertura automática após erro.
    last_error_reopen: Option<std::time::Instant>,
    /// Instance IDs ocultados por esta sessão (estão em `hidden.toml`).
    instances: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct HiddenState {
    schema_version: u32,
    ids: Vec<String>,
}

pub struct Manager {
    opener: Box<dyn ControllerOpener>,
    bus: Option<Arc<dyn VirtualBus>>,
    hider: Option<Arc<dyn DeviceHider>>,
    store: ConfigStore,
    scheduler: Box<dyn Scheduler>,
    notifier: Box<dyn Notifier>,
    events: Sender<Event>,
    sessions: Vec<Session>,
    hidden: Vec<String>,
    /// Falso quando o estado de ocultação não pôde ser lido: nada é gravado nem ocultado.
    hiding_enabled: bool,
    next_session: u64,
    warned_no_vigem: bool,
    grace_generation: u64,
    paused: bool,
    /// Depois de `RestoreControllers`: só visualização até o fim da execução.
    emulation_disabled: bool,
}

impl Manager {
    pub fn new(deps: ManagerDeps) -> Manager {
        let (hidden, hiding_enabled, notices) = load_hidden(&deps.store);
        for n in &notices {
            deps.notifier.notify("Axis", n);
        }
        Manager {
            opener: deps.opener,
            bus: deps.bus,
            hider: deps.hider,
            store: deps.store,
            scheduler: deps.scheduler,
            notifier: deps.notifier,
            events: deps.events,
            sessions: Vec::new(),
            hidden,
            hiding_enabled,
            next_session: 0,
            warned_no_vigem: false,
            grace_generation: 0,
            paused: false,
            emulation_disabled: false,
        }
    }

    /// Reconcilia ocultações de uma execução que travou e enumera os controles conectados.
    pub fn start(&mut self) {
        self.unhide_everything();
        self.scan();
    }

    /// Devolve `false` apenas em `Quit`.
    pub fn handle(&mut self, e: Event) -> bool {
        match e {
            Event::DeviceArrived(_) | Event::Rescan => self.scan(),
            Event::DeviceRemoved(path) => log::debug!("dispositivo removido: {path}"),
            Event::WorkerExited { session, exit } => self.worker_exited(session, exit),
            Event::GraceExpired { id, generation } => self.grace_expired(&id, generation),
            Event::Rumble { id, rumble } => {
                if let Some(s) = self.sessions.iter().find(|s| s.connected && s.id == id) {
                    let _ = s.tx.send(WorkerCommand::Rumble(rumble));
                }
            }
            Event::SetPaused(p) => {
                self.paused = p;
                for s in self.sessions.iter().filter(|s| s.connected) {
                    let _ = s.tx.send(WorkerCommand::SetPaused(p));
                }
            }
            Event::RestoreControllers => self.restore(),
            Event::Quit => {
                self.shutdown_all();
                self.unhide_everything();
                return false;
            }
        }
        true
    }

    /// Para todos os workers, espera por eles e solta os virtuais (sessões esvaziadas).
    fn shutdown_all(&mut self) {
        for s in &self.sessions {
            let _ = s.tx.send(WorkerCommand::Shutdown);
        }
        for s in &mut self.sessions {
            if let Some(h) = s.handle.take() {
                let _ = h.join();
            }
        }
        // Drop dos pads: desconecta os virtuais antes de desocultar os físicos.
        self.sessions.clear();
    }

    /// Desoculta tudo o que consta em `hidden.toml` e limpa o arquivo.
    fn unhide_everything(&mut self) {
        if !self.hiding_enabled || self.hidden.is_empty() {
            return;
        }
        let Some(hider) = self.hider.clone() else {
            return;
        };
        let ids = self.hidden.clone();
        match hider.unhide_all(&ids) {
            Ok(()) => {
                if let Err(e) = self.write_hidden(Vec::new()) {
                    log::warn!("não foi possível limpar a lista de ocultos: {e}");
                }
            }
            Err(e) => {
                log::warn!("não foi possível desocultar {ids:?}: {e}");
                self.notifier.notify(
                    "Axis",
                    "Não foi possível desocultar os controles físicos; tente restaurar pelo menu.",
                );
            }
        }
    }

    fn restore(&mut self) {
        self.shutdown_all();
        self.unhide_everything();
        self.emulation_disabled = true;
        self.notifier.notify("Axis", "Controles restaurados");
        self.scan();
    }

    fn disconnect(&mut self, index: usize) {
        let grace = std::time::Duration::from_secs(self.store.load_settings().grace_secs);
        self.grace_generation += 1;
        let generation = self.grace_generation;
        let s = &mut self.sessions[index];
        let _ = s.tx.send(WorkerCommand::Shutdown);
        if let Some(h) = s.handle.take() {
            let _ = h.join();
        }
        s.connected = false;
        s.generation = generation;
        self.scheduler.after(
            grace,
            Event::GraceExpired {
                id: s.id.clone(),
                generation,
            },
        );
    }

    fn worker_exited(&mut self, session: u64, exit: WorkerExit) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.number == session && s.connected)
        else {
            return;
        };
        let reopen_path = match &exit {
            WorkerExit::Error(m) => {
                let s = &self.sessions[index];
                log::warn!("erro no controle {}: {m}", s.id);
                if s.last_error_reopen
                    .is_some_and(|t| t.elapsed() < ERROR_REOPEN_COOLDOWN)
                {
                    log::warn!("erros repetidos em {}; sem nova reabertura", s.id);
                    None
                } else {
                    Some(s.path.clone())
                }
            }
            _ => None,
        };
        self.disconnect(index);
        if let Some(path) = reopen_path {
            // Uma única tentativa de reabrir o mesmo path.
            if let Some(c) = self.opener.enumerate().into_iter().find(|c| c.path == path) {
                self.arrive(&c, true);
            }
        }
    }

    fn grace_expired(&mut self, id: &str, generation: u64) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| !s.connected && s.id == id && s.generation == generation)
        else {
            return;
        };
        let session = self.sessions.remove(index);
        let Session { pad, instances, .. } = session;
        drop(pad); // desconecta o virtual antes de desocultar o físico
        self.unhide_ids(&instances);
    }

    /// Desoculta `ids` e os remove de `hidden.toml` (só depois do sucesso).
    fn unhide_ids(&mut self, ids: &[String]) {
        if !self.hiding_enabled || ids.is_empty() {
            return;
        }
        let Some(hider) = self.hider.clone() else {
            return;
        };
        let mut done = Vec::new();
        for id in ids {
            match hider.unhide(id) {
                Ok(()) => done.push(id.clone()),
                Err(e) => log::warn!("não foi possível desocultar {id}: {e}"),
            }
        }
        let rest: Vec<String> = self
            .hidden
            .iter()
            .filter(|h| !done.contains(h))
            .cloned()
            .collect();
        if rest.len() != self.hidden.len()
            && let Err(e) = self.write_hidden(rest)
        {
            log::warn!("não foi possível atualizar a lista de ocultos: {e}");
        }
    }

    pub fn sessions(&self) -> Vec<SessionView> {
        self.sessions
            .iter()
            .map(|s| SessionView {
                id: s.id.clone(),
                owner: s.owner.clone(),
                color: s.color,
                family: s.family,
                player: s.player,
                connected: s.connected,
                experimental: s.experimental,
                view_only: s.view_only,
            })
            .collect()
    }

    fn scan(&mut self) {
        for c in self.opener.enumerate() {
            if self
                .sessions
                .iter()
                .any(|s| s.connected && s.path == c.path)
            {
                continue;
            }
            self.arrive(&c, false);
        }
    }

    fn forward_notices(&mut self) {
        for n in self.store.take_notices() {
            self.notifier.notify("Axis", &n);
        }
    }

    fn unique_id(&self, base: &str) -> String {
        let taken = |id: &str| self.sessions.iter().any(|s| s.connected && s.id == id);
        if !taken(base) {
            return base.to_string();
        }
        (2u32..)
            .map(|n| format!("{base}#{n}"))
            .find(|id| !taken(id))
            .expect("sufixo sempre disponível")
    }

    fn record_count(&self) -> usize {
        std::fs::read_dir(self.store.paths().controllers_dir())
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Grava a lista de ocultos ANTES de ocultar (write-ahead).
    fn remember_hidden(&mut self, instance: &str) -> Result<(), String> {
        let mut ids = self.hidden.clone();
        if !ids.iter().any(|i| i == instance) {
            ids.push(instance.to_string());
        }
        self.write_hidden(ids)
    }

    /// Grava `hidden.toml` e só então atualiza a lista em memória.
    fn write_hidden(&mut self, ids: Vec<String>) -> Result<(), String> {
        let file = HiddenState {
            schema_version: 1,
            ids,
        };
        let path = self.store.paths().hidden_file();
        let text = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
        write_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())?;
        self.hidden = file.ids;
        Ok(())
    }

    fn arrive(&mut self, c: &HidCandidate, error_reopen: bool) {
        let controller: Box<dyn Controller> = match self.opener.open(c) {
            Ok(ctl) => ctl,
            Err(e) => {
                log::warn!("não foi possível abrir o controle {}: {e}", c.path);
                return;
            }
        };
        let base_id = controller.id().to_string();
        // Sessão em graça com o mesmo id: a nova chegada a reaproveita.
        let grace_index = self
            .sessions
            .iter()
            .position(|s| !s.connected && s.id == base_id);
        let id = if grace_index.is_some() {
            base_id
        } else {
            self.unique_id(&base_id)
        };
        let family = controller.family();
        let experimental = controller.experimental();
        let palette_index = self.record_count();
        let record = self.store.load_record(&id).unwrap_or_else(|| {
            let r = ConfigStore::new_record(&id, family, palette_index);
            if let Err(e) = self.store.save_record(&r) {
                log::warn!("não foi possível gravar o registro de {id}: {e}");
            }
            r
        });
        let profile = self.store.load_profile(&record.active_profile);
        let pipeline = Pipeline::compile(&record.corrections, &profile).unwrap_or_else(|e| {
            self.notifier.notify(
                "Axis",
                &format!(
                    "Perfil \"{}\" inválido ({e:?}); usando o padrão",
                    record.active_profile
                ),
            );
            Pipeline::compile(&Corrections::default(), &Profile::default())
                .expect("perfil padrão sempre compila")
        });
        self.forward_notices();

        let mut view_only =
            self.bus.is_none() || family == DeviceFamily::Xbox || self.emulation_disabled;
        if self.bus.is_none() && !self.warned_no_vigem {
            self.warned_no_vigem = true;
            self.notifier.notify("Axis", NO_VIGEM_NOTICE);
        }
        let mut pad: Option<SharedPad> = None;
        if let Some(i) = grace_index {
            view_only = self.sessions[i].view_only;
            pad = self.sessions[i].pad.clone();
        } else if !view_only && let Some(bus) = &self.bus {
            let tx = self.events.clone();
            let rumble_id = id.clone();
            let sink = Box::new(move |rumble| {
                let _ = tx.send(Event::Rumble {
                    id: rumble_id.clone(),
                    rumble,
                });
            });
            match bus.plug_x360(sink) {
                Ok(p) => pad = Some(Arc::new(Mutex::new(p))),
                Err(e) => {
                    log::warn!("não foi possível criar o controle virtual de {id}: {e}");
                    view_only = true;
                }
            }
        }
        let mut instances: Vec<String> = grace_index
            .map(|i| self.sessions[i].instances.clone())
            .unwrap_or_default();
        let same_path = grace_index.is_some_and(|i| self.sessions[i].path == c.path);
        if pad.is_some()
            && self.hiding_enabled
            && !same_path
            && let Some(hider) = self.hider.clone()
        {
            let instance = instance_id_from_path(&c.path);
            match self.remember_hidden(&instance) {
                Ok(()) => {
                    if !instances.contains(&instance) {
                        instances.push(instance.clone());
                    }
                    if let Err(e) = hider.hide(&instance) {
                        log::warn!("não foi possível ocultar {instance}: {e}");
                    }
                }
                Err(e) => {
                    log::warn!("não foi possível gravar a lista de ocultos: {e}");
                    self.notifier.notify(
                        "Axis",
                        &format!(
                            "Não foi possível registrar a ocultação de {}; o controle físico continua visível.",
                            record.owner_name
                        ),
                    );
                }
            }
        }
        let player = pad.as_ref().map(|p| {
            let index = p.lock().unwrap_or_else(|e| e.into_inner()).user_index();
            (index + 1) as u8
        });

        let number = self.next_session;
        self.next_session += 1;
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        let events = self.events.clone();
        let (tx, handle) = spawn_worker(
            controller,
            pad.clone(),
            pipeline,
            status.clone(),
            move |exit| {
                let _ = events.send(Event::WorkerExited {
                    session: number,
                    exit,
                });
            },
        );
        let _ = tx.send(WorkerCommand::SetLight {
            lightbar: record.owner_color,
            player,
        });
        if self.paused {
            let _ = tx.send(WorkerCommand::SetPaused(true));
        }
        if grace_index.is_some() {
            if !error_reopen {
                self.notifier
                    .notify("Axis", &format!("{} reconectou", record.owner_name));
            }
        } else if let Some(n) = player {
            self.notifier.notify(
                "Axis",
                &arrival_message(&record.owner_name, record.owner_color, n),
            );
        }
        let session = Session {
            number,
            id,
            path: c.path.clone(),
            owner: record.owner_name,
            color: record.owner_color,
            family,
            player,
            connected: true,
            experimental,
            view_only,
            tx,
            handle: Some(handle),
            status,
            pad,
            generation: 0,
            last_error_reopen: if error_reopen {
                Some(std::time::Instant::now())
            } else {
                grace_index.and_then(|i| self.sessions[i].last_error_reopen)
            },
            instances,
        };
        match grace_index {
            Some(i) => self.sessions[i] = session,
            None => self.sessions.push(session),
        }
    }
}

/// Carrega `hidden.toml`: (ids, ocultação habilitada, avisos).
fn load_hidden(store: &ConfigStore) -> (Vec<String>, bool, Vec<String>) {
    let path = store.paths().hidden_file();
    let check = |h: &HiddenState| {
        if h.schema_version == 0 {
            Err("schema_version: não pode ser 0".to_string())
        } else {
            Ok(())
        }
    };
    match load_toml::<HiddenState>(&path, check) {
        Loaded::Fresh(h) => (h.ids, true, vec![]),
        Loaded::FromBackup(h) => (
            h.ids,
            true,
            vec![format!(
                "Estado de ocultação restaurado do backup: {}",
                path.display()
            )],
        ),
        Loaded::Missing => (vec![], true, vec![]),
        Loaded::Corrupt { preserved_as } => (
            vec![],
            true,
            vec![format!(
                "Arquivo corrompido preservado em {}",
                preserved_as.display()
            )],
        ),
        Loaded::TooNew { .. } | Loaded::Unreadable { .. } => (
            vec![],
            false,
            vec![
                "Estado de ocultação ilegível; o Axis não vai ocultar controles nesta execução."
                    .to_string(),
            ],
        ),
    }
}

fn arrival_message(owner: &str, color: [u8; 3], player: u8) -> String {
    match OWNER_PALETTE.iter().position(|c| *c == color) {
        Some(i) => format!("{owner} ({}) → Player {player}", COLOR_NAMES[i]),
        None => format!("{owner} → Player {player}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigStore, OWNER_PALETTE};
    use crate::paths::AppPaths;
    use crate::testkit::*;
    use axis_devices::DeviceError;
    use axis_virtual::{DeviceHider, Rumble, VirtualBus};
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "axis-test-{}-manager-{}",
                std::process::id(),
                name
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const P1: &str =
        r"\\?\HID#VID_054C&PID_05C4#7&abc&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const P2: &str =
        r"\\?\HID#VID_054C&PID_05C4#7&def&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const INST1: &str = r"HID\VID_054C&PID_05C4\7&ABC&0&0000";

    struct Rig {
        dir: TempDir,
        paths: AppPaths,
        opener: FakeOpener,
        log: Log,
        bus: Option<Arc<FakeBus>>,
        notifier: FakeNotifier,
        recorded: Arc<Mutex<Vec<bool>>>,
        rx: Receiver<Event>,
        scheduler: FakeScheduler,
        mgr: Manager,
    }

    fn rig(name: &str, with_bus: bool, hide_fails: bool) -> Rig {
        rig_with(name, with_bus, hide_fails, |_| {})
    }

    fn rig_with(
        name: &str,
        with_bus: bool,
        hide_fails: bool,
        setup: impl FnOnce(&AppPaths),
    ) -> Rig {
        let dir = TempDir::new(name);
        let paths = AppPaths::under(&dir.0);
        setup(&paths);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let opener = FakeOpener::default();
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let bus = with_bus.then(|| Arc::new(FakeBus::new(log.clone())));
        let hider: Arc<dyn DeviceHider> = Arc::new(FakeHider {
            log: log.clone(),
            fail: hide_fails,
            hidden_path: paths.hidden_file(),
            recorded: recorded.clone(),
        });
        let notifier = FakeNotifier::default();
        let scheduler = FakeScheduler::default();
        let (tx, rx) = channel();
        let mgr = Manager::new(ManagerDeps {
            opener: Box::new(opener.clone()),
            bus: bus.clone().map(|b| b as Arc<dyn VirtualBus>),
            hider: Some(hider),
            store: ConfigStore::new(paths.clone()),
            scheduler: Box::new(scheduler.clone()),
            notifier: Box::new(notifier.clone()),
            events: tx,
        });
        Rig {
            dir,
            paths,
            opener,
            log,
            bus,
            notifier,
            recorded,
            rx,
            scheduler,
            mgr,
        }
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..400 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("condição não atingida");
    }

    fn messages(r: &Rig) -> Vec<String> {
        r.notifier
            .messages
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.clone())
            .collect()
    }

    #[test]
    fn arrival_plugs_then_hides_then_lights_and_notifies() {
        let mut r = rig("order", true, false);
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert_eq!(
            *r.log.lock().unwrap(),
            vec!["plug".to_string(), format!("hide:{INST1}")]
        );
        assert_eq!(*r.recorded.lock().unwrap(), vec![true]);
        let writes = r.opener.writes_of(P1);
        wait_until(|| !writes.lock().unwrap().is_empty());
        let w = writes.lock().unwrap()[0];
        assert_eq!(w.lightbar, OWNER_PALETTE[0]);
        assert_eq!(w.player, Some(1));
        assert_eq!(
            r.notifier.messages.lock().unwrap()[0],
            (
                "Axis".to_string(),
                "Controle 1 (verde) → Player 1".to_string()
            )
        );
        let hidden = std::fs::read_to_string(r.paths.hidden_file()).unwrap();
        assert!(hidden.contains(INST1), "{hidden}");
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].player, Some(1));
        assert!(s[0].connected && !s[0].view_only && !s[0].experimental);
    }

    #[test]
    fn known_controller_keeps_owner_and_profile() {
        let mut r = rig("known", true, false);
        let store = ConfigStore::new(r.paths.clone());
        let mut rec = ConfigStore::new_record("aa:01", axis_core::DeviceFamily::DualShock4, 0);
        rec.owner_name = "Maria".to_string();
        rec.owner_color = OWNER_PALETTE[1];
        store.save_record(&rec).unwrap();
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert_eq!(messages(&r), vec!["Maria (roxo) → Player 1".to_string()]);
        let s = r.mgr.sessions();
        assert_eq!(
            (s[0].owner.as_str(), s[0].color),
            ("Maria", OWNER_PALETTE[1])
        );
    }

    #[test]
    fn new_controller_gets_record_saved_with_next_palette_color() {
        let mut r = rig("palette", true, false);
        r.opener.add(P1, "aa:01");
        r.opener.add(P2, "aa:02");
        r.mgr.start();
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].color, OWNER_PALETTE[0]);
        assert_eq!(s[1].color, OWNER_PALETTE[1]);
        let mut store = ConfigStore::new(r.paths.clone());
        assert_eq!(
            store.load_record("aa:01").unwrap().owner_color,
            OWNER_PALETTE[0]
        );
        assert_eq!(
            store.load_record("aa:02").unwrap().owner_color,
            OWNER_PALETTE[1]
        );
        assert_eq!(s[1].player, Some(2));
    }

    #[test]
    fn missing_vigem_means_view_only_with_single_notice() {
        let mut r = rig("novigem", false, false);
        r.opener.add(P1, "aa:01");
        r.opener.add(P2, "aa:02");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert!(r.log.lock().unwrap().is_empty());
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|v| v.view_only && v.player.is_none()));
        assert_eq!(
            messages(&r),
            vec!["ViGEmBus não encontrado: o Axis só mostra os controles, sem emular.".to_string()]
        );
        assert!(r.bus.is_none());
    }

    #[test]
    fn duplicate_id_gets_suffix_and_own_pad() {
        let mut r = rig("dup", true, false);
        r.opener.add(P1, "x");
        r.opener.add(P2, "x");
        r.mgr.start();
        let ids: Vec<String> = r.mgr.sessions().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["x".to_string(), "x#2".to_string()]);
        assert_eq!(
            r.bus
                .as_ref()
                .unwrap()
                .live
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    #[test]
    fn arrival_of_same_path_twice_is_ignored() {
        let mut r = rig("twice", true, false);
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert_eq!(r.mgr.sessions().len(), 1);
        assert_eq!(r.opener.state.lock().unwrap().opened.len(), 1);
    }

    #[test]
    fn hide_failure_keeps_id_written_ahead_and_session_works() {
        let mut r = rig("hidefail", true, true);
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        let hidden = std::fs::read_to_string(r.paths.hidden_file()).unwrap();
        assert!(hidden.contains(INST1), "{hidden}");
        assert!(hidden.contains("schema_version = 1"), "{hidden}");
        assert_eq!(r.mgr.sessions().len(), 1);
        assert!(!r.mgr.sessions()[0].view_only);
        let _ = &r.dir;
    }

    #[test]
    fn failed_write_ahead_means_no_hide_and_a_notice() {
        let mut r = rig("wafail", true, false);
        // Um diretório no lugar do arquivo faz o rename atômico falhar.
        std::fs::create_dir_all(r.paths.hidden_file()).unwrap();
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert_eq!(*r.log.lock().unwrap(), vec!["plug".to_string()]);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert!(!s[0].view_only && s[0].player == Some(1));
        assert!(
            messages(&r).contains(
                &"Não foi possível registrar a ocultação de Controle 1; o controle físico continua visível."
                    .to_string()
            ),
            "{:?}",
            messages(&r)
        );
    }

    #[test]
    fn corrupt_hidden_state_recovers_ids_from_backup() {
        let mut r = rig_with("hbak", true, false, |p| {
            std::fs::create_dir_all(&p.state).unwrap();
            std::fs::write(
                p.hidden_file().with_extension("toml.bak"),
                "schema_version = 1\nids = [\"OLD\\\\ID\"]\n",
            )
            .unwrap();
            std::fs::write(p.hidden_file(), "isto não é toml {{{").unwrap();
        });
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        let text = std::fs::read_to_string(r.paths.hidden_file()).unwrap();
        assert!(text.contains("OLD") && text.contains(INST1), "{text}");
        assert!(messages(&r).iter().any(|m| m.contains("backup")));
    }

    #[test]
    fn unreadable_hidden_state_disables_hiding_for_the_run() {
        let mut r = rig_with("hunread", true, false, |p| {
            std::fs::create_dir_all(p.hidden_file()).unwrap();
        });
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        assert_eq!(*r.log.lock().unwrap(), vec!["plug".to_string()]);
        assert!(r.paths.hidden_file().is_dir());
        assert_eq!(r.mgr.sessions().len(), 1);
        assert!(
            messages(&r).contains(
                &"Estado de ocultação ilegível; o Axis não vai ocultar controles nesta execução."
                    .to_string()
            ),
            "{:?}",
            messages(&r)
        );
    }

    #[test]
    fn quit_returns_false_and_others_true() {
        let mut r = rig("quit", true, false);
        assert!(r.mgr.handle(Event::Rescan));
        assert!(!r.mgr.handle(Event::Quit));
    }

    const INST2: &str = r"HID\VID_054C&PID_05C4\7&DEF&0&0000";

    fn live(r: &Rig) -> usize {
        r.bus
            .as_ref()
            .unwrap()
            .live
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn plugs(r: &Rig) -> usize {
        r.bus
            .as_ref()
            .unwrap()
            .plugs
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn next_exit(r: &Rig) -> Event {
        loop {
            let e =
                r.rx.recv_timeout(Duration::from_secs(2))
                    .expect("sem WorkerExited");
            if matches!(e, Event::WorkerExited { .. }) {
                return e;
            }
        }
    }

    fn make_worker_fail(r: &mut Rig, path: &str, err: DeviceError) {
        r.opener.reads_of(path).lock().unwrap().push_back(Err(err));
        let e = next_exit(r);
        r.mgr.handle(e);
    }

    fn disconnect(r: &mut Rig, path: &str) {
        make_worker_fail(r, path, DeviceError::Disconnected);
    }

    fn hidden_text(r: &Rig) -> String {
        std::fs::read_to_string(r.paths.hidden_file()).unwrap_or_default()
    }

    fn one_controller(name: &str) -> Rig {
        let mut r = rig(name, true, false);
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        r
    }

    #[test]
    fn disconnect_keeps_pad_and_sends_neutral_then_schedules_grace() {
        let mut r = one_controller("disc");
        disconnect(&mut r, P1);
        assert_eq!(live(&r), 1);
        assert_eq!(
            *r.bus.as_ref().unwrap().last_input.lock().unwrap(),
            Some(axis_core::InputState::default())
        );
        assert!(!r.mgr.sessions()[0].connected);
        let sched = r.scheduler.scheduled.lock().unwrap();
        assert_eq!(sched.len(), 1);
        assert_eq!(sched[0].0, Duration::from_secs(5));
        assert!(matches!(
            &sched[0].1,
            Event::GraceExpired { id, generation: 1 } if id == "aa:01"
        ));
    }

    #[test]
    fn reconnect_within_grace_reuses_the_same_pad_and_player() {
        let mut r = one_controller("recon");
        disconnect(&mut r, P1);
        r.mgr.handle(Event::Rescan);
        assert_eq!(plugs(&r), 1);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].player, Some(1));
        assert!(s[0].connected);
        assert_eq!(messages(&r).last().unwrap(), "Controle 1 reconectou");
        assert_eq!(messages(&r).len(), 2);
        assert_eq!(r.log.lock().unwrap().len(), 2, "um plug e um hide apenas");
    }

    #[test]
    fn reconnect_on_a_new_path_hides_it_and_keeps_the_old_entry() {
        let mut r = one_controller("recon2");
        disconnect(&mut r, P1);
        r.opener.state.lock().unwrap().candidates.clear();
        r.opener.add(P2, "aa:01");
        r.mgr.handle(Event::Rescan);
        assert_eq!(plugs(&r), 1);
        assert!(r.mgr.sessions()[0].connected);
        assert!(r.log.lock().unwrap().contains(&format!("hide:{INST2}")));
        let h = hidden_text(&r);
        assert!(h.contains(INST1) && h.contains(INST2), "{h}");
        assert_eq!(*r.recorded.lock().unwrap(), vec![true, true]);
        // ao expirar, os dois ids são liberados
        disconnect(&mut r, P2);
        r.mgr.handle(Event::GraceExpired {
            id: "aa:01".to_string(),
            generation: 2,
        });
        let h = hidden_text(&r);
        assert!(!h.contains(INST1) && !h.contains(INST2), "{h}");
        assert_eq!(live(&r), 0);
    }

    #[test]
    fn grace_expiry_unplugs_and_unhides() {
        let mut r = one_controller("expiry");
        disconnect(&mut r, P1);
        r.mgr.handle(Event::GraceExpired {
            id: "aa:01".to_string(),
            generation: 1,
        });
        assert_eq!(live(&r), 0);
        assert!(r.log.lock().unwrap().contains(&format!("unhide:{INST1}")));
        assert!(!hidden_text(&r).contains(INST1));
        assert!(r.mgr.sessions().is_empty());
    }

    #[test]
    fn stale_grace_expiry_is_ignored_after_reconnect() {
        let mut r = one_controller("stale");
        disconnect(&mut r, P1);
        r.mgr.handle(Event::Rescan);
        r.mgr.handle(Event::GraceExpired {
            id: "aa:01".to_string(),
            generation: 1,
        });
        assert_eq!(r.mgr.sessions().len(), 1);
        assert!(r.mgr.sessions()[0].connected);
        assert_eq!(live(&r), 1);
    }

    #[test]
    fn startup_reconciles_hidden_entries_from_a_crash() {
        let mut r = rig_with("reconcile", true, false, |p| {
            std::fs::create_dir_all(&p.state).unwrap();
            std::fs::write(
                p.hidden_file(),
                "schema_version = 1\nids = [\"HID\\\\X\"]\n",
            )
            .unwrap();
        });
        r.opener.add(P1, "aa:01");
        r.mgr.start();
        assert_eq!(
            *r.log.lock().unwrap(),
            vec![
                r"unhide_all:HID\X".to_string(),
                "plug".to_string(),
                format!("hide:{INST1}")
            ]
        );
        let state: HiddenState = toml::from_str(&hidden_text(&r)).unwrap();
        assert_eq!(state.ids, vec![INST1.to_string()]);
    }

    #[test]
    fn restore_unhides_everything_and_clears_state() {
        let mut r = rig("restore", true, false);
        r.opener.add(P1, "aa:01");
        r.opener.add(P2, "aa:02");
        r.mgr.start();
        r.mgr.handle(Event::RestoreControllers);
        assert!(
            r.log
                .lock()
                .unwrap()
                .contains(&format!("unhide_all:{INST1},{INST2}"))
        );
        let h = hidden_text(&r);
        assert!(!h.contains(INST1) && !h.contains(INST2), "{h}");
        assert_eq!(live(&r), 0);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|v| v.view_only && v.player.is_none()));
        assert!(messages(&r).contains(&"Controles restaurados".to_string()));
        // chegadas posteriores também ficam só em visualização, sem ocultar
        r.opener.add(
            r"\\?\HID#VID_054C&PID_05C4#7&ghi&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            "aa:03",
        );
        r.mgr.handle(Event::Rescan);
        assert_eq!(r.mgr.sessions().len(), 3);
        assert!(r.mgr.sessions().iter().all(|v| v.view_only));
        assert_eq!(plugs(&r), 2);
        assert!(!hidden_text(&r).contains("GHI"));
    }

    #[test]
    fn quit_shuts_down_workers_and_unhides() {
        let mut r = one_controller("quit2");
        assert!(!r.mgr.handle(Event::Quit));
        assert!(matches!(
            next_exit(&r),
            Event::WorkerExited {
                exit: crate::worker::WorkerExit::Shutdown,
                ..
            }
        ));
        assert_eq!(live(&r), 0);
        let log = r.log.lock().unwrap().clone();
        assert_eq!(log.last().unwrap(), &format!("unhide_all:{INST1}"));
        assert!(!hidden_text(&r).contains(INST1));
    }

    #[test]
    fn rumble_reaches_the_active_worker() {
        let mut r = one_controller("rumble");
        r.mgr.handle(Event::Rumble {
            id: "aa:01".to_string(),
            rumble: Rumble {
                strong: 200,
                weak: 50,
            },
        });
        let writes = r.opener.writes_of(P1);
        wait_until(|| {
            writes
                .lock()
                .unwrap()
                .iter()
                .any(|w| w.rumble_strong == 200)
        });
    }

    #[test]
    fn worker_error_reopens_the_same_path_once() {
        let mut r = one_controller("err");
        make_worker_fail(&mut r, P1, DeviceError::Hid("x".to_string()));
        assert_eq!(r.opener.state.lock().unwrap().opened.len(), 2);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert!(s[0].connected);
        assert_eq!(plugs(&r), 1);
    }

    #[test]
    fn repeated_errors_do_not_loop() {
        let mut r = one_controller("loop");
        make_worker_fail(&mut r, P1, DeviceError::Hid("x".to_string()));
        make_worker_fail(&mut r, P1, DeviceError::Hid("y".to_string()));
        assert_eq!(r.opener.state.lock().unwrap().opened.len(), 2);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert!(!s[0].connected);
        assert!(!messages(&r).iter().any(|m| m.contains("reconectou")));
    }

    #[test]
    fn worker_error_with_failed_reopen_is_a_disconnect() {
        let mut r = one_controller("err2");
        r.opener.state.lock().unwrap().candidates.clear();
        make_worker_fail(&mut r, P1, DeviceError::Hid("x".to_string()));
        assert!(!r.mgr.sessions()[0].connected);
        assert_eq!(r.scheduler.scheduled.lock().unwrap().len(), 1);
        assert_eq!(live(&r), 1);
    }
}
