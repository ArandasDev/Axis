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
use crate::store::write_atomic;
use crate::worker::{SharedPad, WorkerCommand, WorkerExit, WorkerStatus, spawn_worker};

const NO_VIGEM_NOTICE: &str = "ViGEmBus não encontrado: o Axis só mostra os controles, sem emular.";
const COLOR_NAMES: [&str; 6] = ["verde", "roxo", "amarelo", "azul", "laranja", "rosa"];

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

/// Estado de um controle conhecido; alguns campos só são lidos a partir da Task 8.
#[allow(dead_code)]
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
    status: Arc<Mutex<WorkerStatus>>,
    pad: Option<SharedPad>,
}

#[derive(Serialize, Deserialize)]
struct HiddenFile {
    schema_version: u32,
    ids: Vec<String>,
}

pub struct Manager {
    opener: Box<dyn ControllerOpener>,
    bus: Option<Arc<dyn VirtualBus>>,
    hider: Option<Arc<dyn DeviceHider>>,
    store: ConfigStore,
    #[allow(dead_code)]
    scheduler: Box<dyn Scheduler>,
    notifier: Box<dyn Notifier>,
    events: Sender<Event>,
    sessions: Vec<Session>,
    hidden: Vec<String>,
    next_session: u64,
    warned_no_vigem: bool,
}

impl Manager {
    pub fn new(deps: ManagerDeps) -> Manager {
        let hidden = std::fs::read_to_string(deps.store.paths().hidden_file())
            .ok()
            .and_then(|t| toml::from_str::<HiddenFile>(&t).ok())
            .map(|h| h.ids)
            .unwrap_or_default();
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
            next_session: 0,
            warned_no_vigem: false,
        }
    }

    /// Enumera os controles já conectados.
    pub fn start(&mut self) {
        self.scan();
    }

    /// Devolve `false` apenas em `Quit`.
    pub fn handle(&mut self, e: Event) -> bool {
        match e {
            Event::DeviceArrived(_) | Event::Rescan => self.scan(),
            Event::Quit => return false,
            // Tratados na Task 8.
            other => log::debug!("evento ainda não tratado: {other:?}"),
        }
        true
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
            if self.sessions.iter().any(|s| s.path == c.path) {
                continue;
            }
            self.arrive(&c);
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
    fn remember_hidden(&mut self, instance: &str) {
        if !self.hidden.iter().any(|i| i == instance) {
            self.hidden.push(instance.to_string());
        }
        let file = HiddenFile {
            schema_version: 1,
            ids: self.hidden.clone(),
        };
        let path = self.store.paths().hidden_file();
        let result = toml::to_string_pretty(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| write_atomic(&path, t.as_bytes()).map_err(|e| e.to_string()));
        if let Err(e) = result {
            log::warn!("não foi possível gravar a lista de ocultos: {e}");
        }
    }

    fn arrive(&mut self, c: &HidCandidate) {
        let controller: Box<dyn Controller> = match self.opener.open(c) {
            Ok(ctl) => ctl,
            Err(e) => {
                log::warn!("não foi possível abrir o controle {}: {e}", c.path);
                return;
            }
        };
        let id = self.unique_id(controller.id());
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

        let mut view_only = self.bus.is_none() || family == DeviceFamily::Xbox;
        if self.bus.is_none() && !self.warned_no_vigem {
            self.warned_no_vigem = true;
            self.notifier.notify("Axis", NO_VIGEM_NOTICE);
        }
        let mut pad: Option<SharedPad> = None;
        if !view_only && let Some(bus) = &self.bus {
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
        if pad.is_some()
            && let Some(hider) = self.hider.clone()
        {
            let instance = instance_id_from_path(&c.path);
            self.remember_hidden(&instance);
            if let Err(e) = hider.hide(&instance) {
                log::warn!("não foi possível ocultar {instance}: {e}");
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
        if let Some(n) = player {
            self.notifier.notify(
                "Axis",
                &arrival_message(&record.owner_name, record.owner_color, n),
            );
        }
        self.sessions.push(Session {
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
        });
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
    use axis_virtual::{DeviceHider, VirtualBus};
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
        _rx: Receiver<Event>,
        mgr: Manager,
    }

    fn rig(name: &str, with_bus: bool, hide_fails: bool) -> Rig {
        let dir = TempDir::new(name);
        let paths = AppPaths::under(&dir.0);
        let opener = FakeOpener::default();
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let bus = with_bus.then(|| Arc::new(FakeBus::new(log.clone())));
        let hider: Arc<dyn DeviceHider> = Arc::new(FakeHider {
            log: log.clone(),
            fail: hide_fails,
        });
        let notifier = FakeNotifier::default();
        let (tx, rx) = channel();
        let mgr = Manager::new(ManagerDeps {
            opener: Box::new(opener.clone()),
            bus: bus.clone().map(|b| b as Arc<dyn VirtualBus>),
            hider: Some(hider),
            store: ConfigStore::new(paths.clone()),
            scheduler: Box::new(FakeScheduler::default()),
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
            _rx: rx,
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
    fn quit_returns_false_and_others_true() {
        let mut r = rig("quit", true, false);
        assert!(r.mgr.handle(Event::Rescan));
        assert!(!r.mgr.handle(Event::Quit));
    }
}
