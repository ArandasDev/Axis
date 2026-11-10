//! Gerente: laço de eventos, sessões por controle e fluxo de chegada.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use axis_core::{Corrections, DeviceFamily, Pipeline, Profile};
use axis_devices::{Controller, HidCandidate};
use axis_protocol::SessionInfo;
use axis_virtual::{DeviceHider, Rumble, VirtualBus, instance_id_from_path};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigStore, OWNER_PALETTE};
use crate::live::{LiveTap, UiRegistry, to_family};
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
    WorkerExited {
        session: u64,
        exit: WorkerExit,
    },
    GraceExpired {
        id: String,
        generation: u64,
    },
    /// O worker detectou silêncio (`up = false`) ou a volta dos reports.
    LinkChanged {
        session: u64,
        up: bool,
    },
    /// Fim da graça de uma sessão sem sinal (validado pela geração).
    SilenceExpired {
        session: u64,
        generation: u64,
    },
    Rumble {
        id: String,
        rumble: Rumble,
    },
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
    pub ui: Arc<UiRegistry>,
}

/// Estado de um controle conhecido. `connected = false` significa período de graça
/// (worker encerrado); `link_up = false`, controle sem sinal com o worker vivo.
struct Session {
    number: u64,
    id: String,
    path: String,
    owner: String,
    color: [u8; 3],
    family: DeviceFamily,
    player: Option<u8>,
    connected: bool,
    /// Falso enquanto o worker não recebe reports (ex.: DS4 desligado no Bluetooth).
    link_up: bool,
    experimental: bool,
    view_only: bool,
    tx: Sender<WorkerCommand>,
    handle: Option<JoinHandle<()>>,
    status: Arc<Mutex<WorkerStatus>>,
    /// Torneira ao vivo; sobrevive à reconexão dentro da graça.
    tap: Arc<LiveTap>,
    pad: Option<SharedPad>,
    /// Geração da última desconexão ou perda de sinal (valida `GraceExpired`/`SilenceExpired`).
    generation: u64,
    /// Quando ocorreu a última reabertura automática após erro.
    last_error_reopen: Option<std::time::Instant>,
    /// Instance IDs ocultados por esta sessão (estão em `hidden.toml`).
    instances: Vec<String>,
    /// Reabertura após erro adiada pelo intervalo mínimo; um `Rescan` está agendado.
    reopen_pending: bool,
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
    ui: Arc<UiRegistry>,
    sessions: Vec<Session>,
    hidden: Vec<String>,
    /// Falso quando o estado de ocultação não pôde ser lido: nada é gravado nem ocultado.
    hiding_enabled: bool,
    next_session: u64,
    warned_no_vigem: bool,
    warned_plug_failure: bool,
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
            deps.ui.push_notice(n);
        }
        Manager {
            opener: deps.opener,
            bus: deps.bus,
            hider: deps.hider,
            store: deps.store,
            scheduler: deps.scheduler,
            notifier: deps.notifier,
            events: deps.events,
            ui: deps.ui,
            sessions: Vec::new(),
            hidden,
            hiding_enabled,
            next_session: 0,
            warned_no_vigem: false,
            warned_plug_failure: false,
            grace_generation: 0,
            paused: false,
            emulation_disabled: false,
        }
    }

    /// Reconcilia ocultações de uma execução que travou e enumera os controles conectados.
    pub fn start(&mut self) {
        if !self.unhide_everything() {
            self.notify(
                "Axis",
                "Não foi possível desocultar os controles físicos; tente restaurar pelo menu.",
            );
        }
        self.scan();
        self.publish_ui();
    }

    /// Devolve `false` apenas em `Quit`.
    pub fn handle(&mut self, e: Event) -> bool {
        let keep_going = self.dispatch(e);
        self.publish_ui();
        keep_going
    }

    /// Avisa pela bandeja e guarda o texto para a interface.
    fn notify(&self, title: &str, body: &str) {
        self.notifier.notify(title, body);
        self.ui.push_notice(body);
    }

    /// Entrega à interface o estado atual (só muda a geração se algo mudou).
    fn publish_ui(&self) {
        let sessions = self
            .sessions
            .iter()
            .map(|s| {
                let st = s.status.lock().unwrap_or_else(|e| e.into_inner()).status;
                SessionInfo {
                    id: s.id.clone(),
                    owner: s.owner.clone(),
                    color: s.color,
                    family: to_family(s.family),
                    player: s.player,
                    connected: s.connected && s.link_up,
                    experimental: s.experimental,
                    view_only: s.view_only,
                    battery: st.battery_percent,
                    charging: st.charging,
                }
            })
            .collect();
        let taps = self
            .sessions
            .iter()
            .map(|s| (s.id.clone(), s.tap.clone()))
            .collect();
        self.ui
            .publish(sessions, taps, self.paused, !self.emulation_disabled);
    }

    fn dispatch(&mut self, e: Event) -> bool {
        match e {
            Event::DeviceArrived(path) => {
                log::info!("chegada de dispositivo: {path}");
                self.scan()
            }
            Event::Rescan => {
                log::info!("nova varredura de controles");
                self.scan()
            }
            Event::DeviceRemoved(path) => log::info!("dispositivo removido: {path}"),
            Event::WorkerExited { session, exit } => self.worker_exited(session, exit),
            Event::GraceExpired { id, generation } => self.grace_expired(&id, generation),
            Event::LinkChanged { session, up } => self.link_changed(session, up),
            Event::SilenceExpired {
                session,
                generation,
            } => self.silence_expired(session, generation),
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
                if !self.unhide_everything() {
                    self.notify(
                        "Axis",
                        "Não foi possível desocultar os controles físicos; isso será desfeito na próxima inicialização do Axis.",
                    );
                }
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
    /// Devolve `false` se a desocultação falhou (quem chama avisa o usuário).
    fn unhide_everything(&mut self) -> bool {
        if !self.hiding_enabled || self.hidden.is_empty() {
            return true;
        }
        let Some(hider) = self.hider.clone() else {
            return true;
        };
        let ids = self.hidden.clone();
        match hider.unhide_all(&ids) {
            Ok(()) => {
                if let Err(e) = self.write_hidden(Vec::new()) {
                    log::warn!("não foi possível limpar a lista de ocultos: {e}");
                }
                true
            }
            Err(e) => {
                log::warn!("não foi possível desocultar {ids:?}: {e}");
                false
            }
        }
    }

    fn restore(&mut self) {
        // Os virtuais saem antes da desocultação (evita entrada dupla).
        self.shutdown_all();
        if self.unhide_everything() {
            self.emulation_disabled = true;
            self.notify("Axis", "Controles restaurados");
        } else {
            // Os físicos continuam ocultos: a emulação volta para não deixar o
            // usuário sem controle.
            self.notify(
                "Axis",
                "Não foi possível restaurar os controles; a emulação continua.",
            );
        }
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
        log::info!("worker da sessão {session} saiu: {exit:?}");
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.number == session && s.connected)
        else {
            return;
        };
        let mut deferred = false;
        let reopen_path = match &exit {
            WorkerExit::Error(m) => {
                let s = &self.sessions[index];
                log::warn!("erro no controle {}: {m}", s.id);
                if s.last_error_reopen
                    .is_some_and(|t| t.elapsed() < ERROR_REOPEN_COOLDOWN)
                {
                    log::warn!(
                        "erros repetidos em {}; nova tentativa após o intervalo",
                        s.id
                    );
                    deferred = true;
                    None
                } else {
                    Some(s.path.clone())
                }
            }
            _ => None,
        };
        self.disconnect(index);
        if deferred {
            // O dispositivo continua presente (sem evento de hotplug): sem este
            // Rescan o controle ficaria parado.
            self.sessions[index].reopen_pending = true;
            self.scheduler.after(ERROR_REOPEN_COOLDOWN, Event::Rescan);
        }
        if let Some(path) = reopen_path {
            // Uma única tentativa de reabrir o mesmo path.
            if let Some(c) = self.opener.enumerate().into_iter().find(|c| c.path == path) {
                self.arrive(&c, true);
            }
        }
    }

    fn grace_expired(&mut self, id: &str, generation: u64) {
        log::info!("período de graça de {id} (geração {generation}) chegou ao fim");
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
        self.release_instances(instances);
    }

    /// Desoculta os `instances` que nenhuma sessão conectada ainda emula.
    fn release_instances(&mut self, instances: Vec<String>) {
        let free: Vec<String> = instances
            .into_iter()
            .filter(|i| {
                !self
                    .sessions
                    .iter()
                    .any(|s| s.connected && s.instances.contains(i))
            })
            .collect();
        self.unhide_ids(&free);
    }

    fn link_changed(&mut self, session: u64, up: bool) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.number == session && s.connected)
        else {
            return;
        };
        if self.sessions[index].link_up == up {
            return;
        }
        if !up {
            log::info!("sessão {session} sem sinal");
            let grace = std::time::Duration::from_secs(self.store.load_settings().grace_secs);
            self.grace_generation += 1;
            let generation = self.grace_generation;
            let s = &mut self.sessions[index];
            s.link_up = false;
            s.generation = generation;
            self.scheduler.after(
                grace,
                Event::SilenceExpired {
                    session,
                    generation,
                },
            );
            return;
        }
        log::info!("sessão {session} voltou");
        self.sessions[index].link_up = true;
        let s = &self.sessions[index];
        if s.pad.is_some() {
            // Dentro da graça: mesmo virtual e mesmo Player; o DS4 religado perdeu a luz.
            let _ = s.tx.send(WorkerCommand::SetLight {
                lightbar: s.color,
                player: s.player,
            });
            let notice = format!("{} reconectou", s.owner);
            self.notify("Axis", &notice);
            return;
        }
        if s.view_only || self.emulation_disabled || self.bus.is_none() {
            let _ = s.tx.send(WorkerCommand::SetLight {
                lightbar: s.color,
                player: None,
            });
            return;
        }
        // Depois da graça: um virtual novo (o Player pode ser outro).
        let id = s.id.clone();
        match self.plug(&id) {
            Some(pad) => {
                let player = player_of(&pad);
                let s = &mut self.sessions[index];
                let _ = s.tx.send(WorkerCommand::SetPad(Some(pad.clone())));
                let _ = s.tx.send(WorkerCommand::SetLight {
                    lightbar: s.color,
                    player: Some(player),
                });
                s.pad = Some(pad);
                s.player = Some(player);
                log::info!("sessão {session}: novo controle virtual, player {player}");
                let notice = arrival_message(&s.owner, s.color, player);
                self.notify("Axis", &notice);
            }
            None => {
                // Sem virtual, o físico não pode continuar oculto.
                let s = &mut self.sessions[index];
                s.view_only = true;
                let _ = s.tx.send(WorkerCommand::SetLight {
                    lightbar: s.color,
                    player: None,
                });
                let instances = std::mem::take(&mut s.instances);
                self.release_instances(instances);
            }
        }
    }

    fn silence_expired(&mut self, session: u64, generation: u64) {
        let Some(s) = self.sessions.iter_mut().find(|s| {
            s.number == session && s.connected && !s.link_up && s.generation == generation
        }) else {
            return;
        };
        let Some(pad) = s.pad.take() else {
            return;
        };
        log::info!("sessão {session}: silêncio além da graça, controle virtual liberado");
        // O worker solta o clone dele; com o nosso, o virtual é desconectado.
        // O físico continua oculto: o mesmo handle volta quando o controle religar.
        let _ = s.tx.send(WorkerCommand::SetPad(None));
        s.player = None;
        drop(pad);
    }

    /// Cria um virtual para `id`; em falha, avisa uma vez por execução.
    fn plug(&mut self, id: &str) -> Option<SharedPad> {
        let bus = self.bus.clone()?;
        let tx = self.events.clone();
        let rumble_id = id.to_string();
        let sink = Box::new(move |rumble| {
            let _ = tx.send(Event::Rumble {
                id: rumble_id.clone(),
                rumble,
            });
        });
        match bus.plug_x360(sink) {
            Ok(p) => Some(Arc::new(Mutex::new(p))),
            Err(e) => {
                log::warn!("não foi possível criar o controle virtual de {id}: {e}");
                if !self.warned_plug_failure {
                    self.warned_plug_failure = true;
                    self.notify(
                        "Axis",
                        &format!(
                            "Não foi possível criar o controle virtual ({e}); o controle fica só em visualização."
                        ),
                    );
                }
                None
            }
        }
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
                connected: s.connected && s.link_up,
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
            self.notify("Axis", &n);
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
        // Sessão em graça com o mesmo id (ou um sufixo `#n` dele): a nova chegada
        // a reaproveita, preferindo o id exato.
        let suffixed = format!("{base_id}#");
        let grace_index = self
            .sessions
            .iter()
            .position(|s| !s.connected && s.id == base_id)
            .or_else(|| {
                self.sessions
                    .iter()
                    .position(|s| !s.connected && s.id.starts_with(&suffixed))
            });
        let id = match grace_index {
            Some(i) => self.sessions[i].id.clone(),
            None => self.unique_id(&base_id),
        };
        // Reabertura após erro adiada: a chegada pelo `Rescan` conta como reabertura.
        let error_reopen =
            error_reopen || grace_index.is_some_and(|i| self.sessions[i].reopen_pending);
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
            self.notify(
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
            self.notify("Axis", NO_VIGEM_NOTICE);
        }
        let mut pad: Option<SharedPad> = None;
        if let Some(i) = grace_index {
            view_only = self.sessions[i].view_only;
            pad = self.sessions[i].pad.clone();
        }
        // Sessão nova, ou em graça cujo virtual já foi liberado pelo silêncio.
        let mut plugged_new = false;
        if pad.is_none() && !view_only {
            pad = self.plug(&id);
            plugged_new = pad.is_some();
            view_only = pad.is_none();
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
                    self.notify(
                        "Axis",
                        &format!(
                            "Não foi possível registrar a ocultação de {}; o controle físico continua visível.",
                            record.owner_name
                        ),
                    );
                }
            }
        }
        if pad.is_none() && !instances.is_empty() {
            // Graça sem virtual (falha ao criar): o físico não pode ficar oculto.
            self.release_instances(std::mem::take(&mut instances));
        }
        let player = pad.as_ref().map(player_of);

        let number = self.next_session;
        self.next_session += 1;
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        // A torneira acompanha a sessão reaproveitada (a interface já a conhece).
        let tap = grace_index
            .map(|i| self.sessions[i].tap.clone())
            .unwrap_or_else(LiveTap::new);
        let link_events = self.events.clone();
        let events = self.events.clone();
        // Xbox (XInput) não manda nada parado: silêncio não indica desligamento.
        let detect_silence = matches!(family, DeviceFamily::DualShock4 | DeviceFamily::DualSense);
        let (tx, handle) = spawn_worker(
            controller,
            pad.clone(),
            pipeline,
            status.clone(),
            detect_silence,
            move |exit| {
                let _ = events.send(Event::WorkerExited {
                    session: number,
                    exit,
                });
            },
            move |up| {
                let _ = link_events.send(Event::LinkChanged {
                    session: number,
                    up,
                });
            },
            tap.clone(),
        );
        let _ = tx.send(WorkerCommand::SetLight {
            lightbar: record.owner_color,
            player,
        });
        if self.paused {
            let _ = tx.send(WorkerCommand::SetPaused(true));
        }
        log::info!(
            "sessão {number} conectada: {id} em {} (player {player:?}, reaproveitada: {}, reabertura após erro: {error_reopen})",
            c.path,
            grace_index.is_some()
        );
        if grace_index.is_some() && !plugged_new {
            if !error_reopen {
                self.notify("Axis", &format!("{} reconectou", record.owner_name));
            }
        } else if let Some(n) = player {
            self.notify(
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
            link_up: true,
            experimental,
            view_only,
            tx,
            handle: Some(handle),
            status,
            tap,
            pad,
            generation: 0,
            last_error_reopen: if error_reopen {
                Some(std::time::Instant::now())
            } else {
                grace_index.and_then(|i| self.sessions[i].last_error_reopen)
            },
            instances,
            reopen_pending: false,
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

/// Player (1..) mostrado para o virtual.
fn player_of(pad: &SharedPad) -> u8 {
    let index = pad.lock().unwrap_or_else(|e| e.into_inner()).user_index();
    (index + 1) as u8
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
        unhide_all_fails: Arc<std::sync::atomic::AtomicBool>,
        mgr: Manager,
        ui: Arc<UiRegistry>,
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
        let unhide_all_fails = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hider: Arc<dyn DeviceHider> = Arc::new(FakeHider {
            log: log.clone(),
            fail: hide_fails,
            hidden_path: paths.hidden_file(),
            recorded: recorded.clone(),
            fail_unhide_all: unhide_all_fails.clone(),
        });
        let notifier = FakeNotifier::default();
        let scheduler = FakeScheduler::default();
        let ui = UiRegistry::new();
        let (tx, rx) = channel();
        let mgr = Manager::new(ManagerDeps {
            opener: Box::new(opener.clone()),
            bus: bus.clone().map(|b| b as Arc<dyn VirtualBus>),
            hider: Some(hider),
            store: ConfigStore::new(paths.clone()),
            scheduler: Box::new(scheduler.clone()),
            notifier: Box::new(notifier.clone()),
            events: tx,
            ui: ui.clone(),
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
            unhide_all_fails,
            mgr,
            ui,
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
    fn manager_publishes_sessions_with_battery_and_tap() {
        let mut r = rig("ui", true, false);
        let g0 = r.ui.snapshot().generation;
        r.opener.add(P1, "aa:01");
        r.mgr.handle(Event::DeviceArrived(P1.to_string()));
        let snap = r.ui.snapshot();
        assert!(snap.generation > g0);
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.sessions[0].owner, "Controle 1");
        assert_eq!(snap.sessions[0].player, Some(1));
        assert!(snap.sessions[0].connected);
        assert_eq!(snap.sessions[0].battery, None);
        assert!(snap.taps.contains_key(&snap.sessions[0].id));
        assert!(snap.emulation && !snap.paused);
        // Os avisos do gerente também chegam à interface.
        assert!(snap.notices.iter().any(|n| n.contains("Player 1")));
        // A torneira da interface é a mesma da sessão: sobrevive a um evento sem mudança.
        let tap = snap.taps[&snap.sessions[0].id].clone();
        r.mgr.handle(Event::Rescan);
        assert!(Arc::ptr_eq(
            &tap,
            &r.ui.snapshot().taps[&snap.sessions[0].id]
        ));
        r.mgr.handle(Event::SetPaused(true));
        assert!(r.ui.snapshot().paused);
    }

    #[test]
    fn start_and_restore_publish_the_real_emulation_state() {
        let mut r = rig("uiemu", true, false);
        r.opener.add(P1, "aa:01");
        let g0 = r.ui.snapshot().generation;
        r.mgr.start();
        let snap = r.ui.snapshot();
        assert!(snap.generation > g0, "start publica o estado");
        assert!(snap.emulation && !snap.paused);
        r.mgr.handle(Event::RestoreControllers);
        let snap = r.ui.snapshot();
        assert!(!snap.emulation, "depois de restaurar: só visualização");
        assert!(snap.sessions.iter().all(|s| s.view_only));
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
    fn reconnect_within_grace_reuses_the_live_tap() {
        let mut r = one_controller("recon-tap");
        let tap = r.ui.snapshot().taps["aa:01"].clone();
        disconnect(&mut r, P1);
        assert!(!r.ui.snapshot().sessions[0].connected);
        r.mgr.handle(Event::Rescan);
        let snap = r.ui.snapshot();
        assert!(snap.sessions[0].connected);
        assert!(Arc::ptr_eq(&tap, &snap.taps["aa:01"]));
        assert!(snap.notices.iter().any(|n| n == "Controle 1 reconectou"));
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
        // Sem reabertura agora: um Rescan fica agendado para depois do intervalo.
        assert!(
            r.scheduler
                .scheduled
                .lock()
                .unwrap()
                .iter()
                .any(|(d, e)| *d == ERROR_REOPEN_COOLDOWN && matches!(e, Event::Rescan))
        );
        r.mgr.handle(Event::Rescan);
        assert_eq!(r.opener.state.lock().unwrap().opened.len(), 3);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert!(s[0].connected);
        assert_eq!(plugs(&r), 1);
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

    #[test]
    fn suffixed_session_reconnects_in_grace_and_expiry_does_not_unhide_live() {
        let mut r = rig("suffix", true, false);
        r.opener.add(P1, "x");
        r.opener.add(P2, "x");
        r.mgr.start();
        disconnect(&mut r, P2);
        assert!(!r.mgr.sessions()[1].connected);
        r.mgr.handle(Event::Rescan);
        assert_eq!(plugs(&r), 2, "reaproveita o virtual de x#2");
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].id, "x#2");
        assert!(s[1].connected);
        assert_eq!(s[1].player, Some(2));
        // GraceExpired antigo: nada é desocultado e a sessão continua.
        r.mgr.handle(Event::GraceExpired {
            id: "x#2".to_string(),
            generation: 1,
        });
        assert!(
            !r.log
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("unhide")),
            "{:?}",
            r.log.lock().unwrap()
        );
        assert!(hidden_text(&r).contains(INST2));
        assert_eq!(r.mgr.sessions().len(), 2);
        assert_eq!(live(&r), 2);
    }

    #[test]
    fn grace_expiry_keeps_instances_used_by_a_connected_session() {
        let mut r = one_controller("liveinst");
        disconnect(&mut r, P1);
        // Outro controle (outro id) chega no mesmo path durante a graça.
        r.opener.state.lock().unwrap().ids[0].1 = "bb:02".to_string();
        r.mgr.handle(Event::Rescan);
        assert_eq!(r.mgr.sessions().len(), 2);
        r.mgr.handle(Event::GraceExpired {
            id: "aa:01".to_string(),
            generation: 1,
        });
        assert_eq!(r.mgr.sessions().len(), 1);
        assert!(!r.log.lock().unwrap().contains(&format!("unhide:{INST1}")));
        assert!(hidden_text(&r).contains(INST1));
    }

    #[test]
    fn plug_failure_notifies_once_and_stays_view_only() {
        let mut r = rig("plugfail", true, false);
        r.bus
            .as_ref()
            .unwrap()
            .fail_plug
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.opener.add(P1, "aa:01");
        r.opener.add(P2, "aa:02");
        r.mgr.start();
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|v| v.view_only && v.player.is_none()));
        let notice = "Não foi possível criar o controle virtual (sem slot livre no ViGEmBus); o controle fica só em visualização.";
        let m = messages(&r);
        assert_eq!(
            m.iter()
                .filter(|x| x.starts_with("Não foi possível criar o controle virtual"))
                .count(),
            1,
            "{m:?}"
        );
        assert!(m.contains(&notice.to_string()), "{m:?}");
        assert!(r.log.lock().unwrap().is_empty(), "sem plug nem ocultação");
    }

    fn next_link(r: &Rig, up: bool) -> Event {
        loop {
            let e =
                r.rx.recv_timeout(Duration::from_secs(3))
                    .expect("sem LinkChanged");
            if matches!(e, Event::LinkChanged { up: u, .. } if u == up) {
                return e;
            }
        }
    }

    /// O controle falso fica sem reports: o worker detecta o silêncio sozinho.
    fn go_silent(r: &mut Rig) {
        let e = next_link(r, false);
        r.mgr.handle(e);
    }

    fn come_back(r: &mut Rig, path: &str) {
        r.opener
            .reads_of(path)
            .lock()
            .unwrap()
            .push_back(Ok(Some(axis_devices::Report::default())));
        let e = next_link(r, true);
        r.mgr.handle(e);
    }

    fn last_scheduled(r: &Rig) -> (Duration, u64, u64) {
        let sched = r.scheduler.scheduled.lock().unwrap();
        match sched.last() {
            Some((
                d,
                Event::SilenceExpired {
                    session,
                    generation,
                },
            )) => (*d, *session, *generation),
            other => panic!("esperava SilenceExpired, veio {other:?}"),
        }
    }

    fn expire_silence(r: &mut Rig) {
        let (_, session, generation) = last_scheduled(r);
        r.mgr.handle(Event::SilenceExpired {
            session,
            generation,
        });
    }

    #[test]
    fn link_down_then_up_within_grace_keeps_pad_and_notifies_reconnect() {
        let mut r = one_controller("linkgrace");
        let writes = r.opener.writes_of(P1);
        wait_until(|| writes.lock().unwrap().len() == 1);
        go_silent(&mut r);
        let s = r.mgr.sessions();
        assert!(!s[0].connected, "a bandeja mostra reconectando…");
        assert_eq!(s[0].player, Some(1));
        assert_eq!(last_scheduled(&r), (Duration::from_secs(5), 0, 1));
        assert_eq!(
            *r.bus.as_ref().unwrap().last_input.lock().unwrap(),
            Some(axis_core::InputState::default())
        );
        come_back(&mut r, P1);
        let s = r.mgr.sessions();
        assert!(s[0].connected);
        assert_eq!(s[0].player, Some(1));
        assert_eq!(plugs(&r), 1);
        assert_eq!(live(&r), 1);
        assert_eq!(messages(&r).last().unwrap(), "Controle 1 reconectou");
        // A luz é reenviada: o DS4 desligado a perdeu.
        wait_until(|| writes.lock().unwrap().len() == 2);
        let w = writes.lock().unwrap()[1];
        assert_eq!((w.lightbar, w.player), (OWNER_PALETTE[0], Some(1)));
    }

    #[test]
    fn silence_beyond_grace_releases_pad_but_keeps_hidden_and_worker() {
        let mut r = one_controller("silexp");
        go_silent(&mut r);
        expire_silence(&mut r);
        wait_until(|| live(&r) == 0);
        assert!(hidden_text(&r).contains(INST1));
        assert!(
            !r.log
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("unhide")),
            "{:?}",
            r.log.lock().unwrap()
        );
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].player, None);
        assert!(!s[0].connected && !s[0].view_only);
        // O worker continua lendo o mesmo handle: um report volta a sinalizar o link.
        r.opener
            .reads_of(P1)
            .lock()
            .unwrap()
            .push_back(Ok(Some(axis_devices::Report::default())));
        loop {
            match r
                .rx
                .recv_timeout(Duration::from_secs(3))
                .expect("sem evento")
            {
                Event::WorkerExited { .. } => panic!("o worker não deveria sair"),
                Event::LinkChanged { up: true, .. } => break,
                _ => {}
            }
        }
        assert_eq!(r.opener.state.lock().unwrap().opened.len(), 1);
    }

    #[test]
    fn link_up_after_expiry_plugs_new_pad_and_announces_player() {
        let mut r = one_controller("silback");
        let writes = r.opener.writes_of(P1);
        r.mgr.handle(Event::Rumble {
            id: "aa:01".to_string(),
            rumble: Rumble {
                strong: 200,
                weak: 50,
            },
        });
        wait_until(|| {
            writes
                .lock()
                .unwrap()
                .iter()
                .any(|w| w.rumble_strong == 200)
        });
        go_silent(&mut r);
        expire_silence(&mut r);
        wait_until(|| live(&r) == 0);
        come_back(&mut r, P1);
        assert_eq!(plugs(&r), 2);
        assert_eq!(live(&r), 1);
        let s = r.mgr.sessions();
        assert!(s[0].connected && !s[0].view_only);
        assert_eq!(s[0].player, Some(2));
        assert_eq!(
            messages(&r).last().unwrap(),
            "Controle 1 (verde) → Player 2"
        );
        wait_until(|| writes.lock().unwrap().iter().any(|w| w.player == Some(2)));
        // A vibração antiga não volta com o controle religado.
        let w = *writes.lock().unwrap().last().unwrap();
        assert_eq!((w.rumble_strong, w.rumble_weak), (0, 0));
        // O virtual novo recebe a entrada.
        r.opener
            .reads_of(P1)
            .lock()
            .unwrap()
            .push_back(Ok(Some(axis_devices::Report::default())));
        let last = r.bus.as_ref().unwrap().last_input.clone();
        *last.lock().unwrap() = None;
        wait_until(|| last.lock().unwrap().is_some());
    }

    #[test]
    fn link_up_after_expiry_with_plug_failure_goes_view_only_and_unhides() {
        let mut r = one_controller("silfail");
        go_silent(&mut r);
        expire_silence(&mut r);
        wait_until(|| live(&r) == 0);
        r.bus
            .as_ref()
            .unwrap()
            .fail_plug
            .store(true, std::sync::atomic::Ordering::SeqCst);
        come_back(&mut r, P1);
        let s = r.mgr.sessions();
        assert!(s[0].connected && s[0].view_only && s[0].player.is_none());
        assert!(r.log.lock().unwrap().contains(&format!("unhide:{INST1}")));
        assert!(!hidden_text(&r).contains(INST1));
        assert!(
            messages(&r)
                .last()
                .unwrap()
                .starts_with("Não foi possível criar o controle virtual"),
            "{:?}",
            messages(&r)
        );
    }

    #[test]
    fn stale_silence_expired_is_ignored() {
        let mut r = one_controller("silstale");
        go_silent(&mut r);
        come_back(&mut r, P1);
        r.mgr.handle(Event::SilenceExpired {
            session: 0,
            generation: 1,
        });
        assert_eq!(live(&r), 1);
        assert_eq!(r.mgr.sessions()[0].player, Some(1));
        // Novo silêncio: a geração antiga continua sem efeito.
        go_silent(&mut r);
        r.mgr.handle(Event::SilenceExpired {
            session: 0,
            generation: 1,
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(live(&r), 1);
        assert_eq!(r.mgr.sessions()[0].player, Some(1));
    }

    #[test]
    fn failed_restore_keeps_emulation_running() {
        let mut r = rig("restorefail", true, false);
        r.opener.add(P1, "aa:01");
        r.opener.add(P2, "aa:02");
        r.mgr.start();
        r.unhide_all_fails
            .store(true, std::sync::atomic::Ordering::SeqCst);
        r.mgr.handle(Event::RestoreControllers);
        let m = messages(&r);
        assert!(!m.contains(&"Controles restaurados".to_string()), "{m:?}");
        assert!(
            m.contains(
                &"Não foi possível restaurar os controles; a emulação continua.".to_string()
            ),
            "{m:?}"
        );
        assert_eq!(live(&r), 2);
        let s = r.mgr.sessions();
        assert_eq!(s.len(), 2);
        assert!(
            s.iter()
                .all(|v| v.connected && !v.view_only && v.player.is_some())
        );
        let h = hidden_text(&r);
        assert!(h.contains(INST1) && h.contains(INST2), "{h}");
        // A emulação não foi desligada: chegadas novas ganham virtual.
        r.opener.add(
            r"\\?\HID#VID_054C&PID_05C4#7&ghi&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            "aa:03",
        );
        r.mgr.handle(Event::Rescan);
        assert_eq!(live(&r), 3);
        assert!(!r.mgr.sessions()[2].view_only);
        // Ao sair com a desocultação falhando, o aviso fala da próxima inicialização.
        assert!(!r.mgr.handle(Event::Quit));
        assert!(
            messages(&r)
                .iter()
                .any(|m| m.ends_with("será desfeito na próxima inicialização do Axis.")),
            "{:?}",
            messages(&r)
        );
    }
}
