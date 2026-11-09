//! Falsos compartilhados pelos testes do daemon.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axis_core::{DeviceFamily, InputState};
use axis_devices::{Controller, DeviceError, OutputState, Report, Transport};
use axis_virtual::{VirtualError, VirtualPad};

pub(crate) type ReadQueue = Arc<Mutex<VecDeque<Result<Option<Report>, DeviceError>>>>;

/// Controle falso: lê de uma fila roteirizada; com a fila vazia, dorme o timeout e devolve `Ok(None)`.
pub(crate) struct FakeController {
    pub id: String,
    pub reads: ReadQueue,
    pub writes: Arc<Mutex<Vec<OutputState>>>,
    pub fail_writes: bool,
}

impl FakeController {
    pub(crate) fn new(script: Vec<Result<Option<Report>, DeviceError>>) -> Self {
        Self {
            id: "fake".to_string(),
            reads: Arc::new(Mutex::new(script.into())),
            writes: Arc::new(Mutex::new(Vec::new())),
            fail_writes: false,
        }
    }
}

impl Controller for FakeController {
    fn id(&self) -> &str {
        &self.id
    }
    fn family(&self) -> DeviceFamily {
        DeviceFamily::DualShock4
    }
    fn transport(&self) -> Transport {
        Transport::Usb
    }
    fn experimental(&self) -> bool {
        false
    }
    fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError> {
        let next = self.reads.lock().unwrap().pop_front();
        match next {
            Some(r) => r,
            None => {
                std::thread::sleep(Duration::from_millis(timeout_ms.max(0) as u64));
                Ok(None)
            }
        }
    }
    fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError> {
        self.writes.lock().unwrap().push(*out);
        if self.fail_writes {
            return Err(DeviceError::Hid("escrita falhou".to_string()));
        }
        Ok(())
    }
}

/// Virtual falso: grava os estados recebidos; pode ser configurado para falhar.
pub(crate) struct FakePad {
    pub received: Arc<Mutex<Vec<InputState>>>,
    pub fail: bool,
}

impl FakePad {
    pub(crate) fn new() -> (Self, Arc<Mutex<Vec<InputState>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                received: received.clone(),
                fail: false,
            },
            received,
        )
    }
}

impl VirtualPad for FakePad {
    fn update(&mut self, input: &InputState) -> Result<(), VirtualError> {
        self.received.lock().unwrap().push(*input);
        if self.fail {
            Err(VirtualError::Bus("falha simulada".to_string()))
        } else {
            Ok(())
        }
    }
    fn user_index(&self) -> u32 {
        0
    }
}

// ---- Falsos do gerente ----

use axis_devices::HidCandidate;
use axis_virtual::{DeviceHider, HideError, RumbleSink, VirtualBus};

use crate::manager::Event;
use crate::ports::{ControllerOpener, Notifier, Scheduler};

pub(crate) type Log = Arc<Mutex<Vec<String>>>;

/// Dados compartilhados entre o teste e o `FakeOpener` entregue ao gerente.
#[derive(Default)]
pub(crate) struct OpenerState {
    pub candidates: Vec<HidCandidate>,
    /// Controle (id) a devolver por path.
    pub ids: Vec<(String, String)>,
    pub writes: Vec<(String, Arc<Mutex<Vec<OutputState>>>)>,
    pub opened: Vec<String>,
    pub reads: Vec<(String, ReadQueue)>,
}

#[derive(Clone, Default)]
pub(crate) struct FakeOpener {
    pub state: Arc<Mutex<OpenerState>>,
}

impl FakeOpener {
    pub(crate) fn add(&self, path: &str, id: &str) {
        let mut s = self.state.lock().unwrap();
        s.candidates.push(HidCandidate {
            path: path.to_string(),
            family: DeviceFamily::DualShock4,
            transport: Transport::Usb,
            serial: None,
        });
        s.ids.push((path.to_string(), id.to_string()));
    }

    pub(crate) fn reads_of(&self, path: &str) -> ReadQueue {
        let s = self.state.lock().unwrap();
        s.reads
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, r)| r.clone())
            .expect("controle ainda não aberto")
    }

    pub(crate) fn writes_of(&self, path: &str) -> Arc<Mutex<Vec<OutputState>>> {
        let s = self.state.lock().unwrap();
        s.writes
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, w)| w.clone())
            .expect("controle ainda não aberto")
    }
}

impl ControllerOpener for FakeOpener {
    fn enumerate(&mut self) -> Vec<HidCandidate> {
        self.state.lock().unwrap().candidates.clone()
    }

    fn open(&mut self, c: &HidCandidate) -> Result<Box<dyn Controller>, DeviceError> {
        let mut s = self.state.lock().unwrap();
        let id = s
            .ids
            .iter()
            .find(|(p, _)| *p == c.path)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| DeviceError::Hid("path desconhecido".to_string()))?;
        let mut ctl = FakeController::new(vec![]);
        ctl.id = id;
        s.writes.push((c.path.clone(), ctl.writes.clone()));
        s.reads.push((c.path.clone(), ctl.reads.clone()));
        s.opened.push(c.path.clone());
        Ok(Box::new(ctl))
    }
}

/// Barramento falso: registra "plug" no log compartilhado e conta virtuais vivos.
pub(crate) struct FakeBus {
    pub log: Log,
    pub live: Arc<std::sync::atomic::AtomicUsize>,
    pub next_index: std::sync::atomic::AtomicU32,
    pub last_input: Arc<Mutex<Option<InputState>>>,
    pub plugs: Arc<std::sync::atomic::AtomicUsize>,
    /// Quando ligado, `plug_x360` falha (sem slot livre).
    pub fail_plug: std::sync::atomic::AtomicBool,
}

impl FakeBus {
    pub(crate) fn new(log: Log) -> Self {
        Self {
            log,
            live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            next_index: std::sync::atomic::AtomicU32::new(0),
            last_input: Arc::new(Mutex::new(None)),
            plugs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail_plug: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

struct FakeBusPad {
    index: u32,
    last_input: Arc<Mutex<Option<InputState>>>,
    live: Arc<std::sync::atomic::AtomicUsize>,
}

impl VirtualPad for FakeBusPad {
    fn update(&mut self, input: &InputState) -> Result<(), VirtualError> {
        *self.last_input.lock().unwrap() = Some(*input);
        Ok(())
    }
    fn user_index(&self) -> u32 {
        self.index
    }
}

impl Drop for FakeBusPad {
    fn drop(&mut self) {
        self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl VirtualBus for FakeBus {
    fn plug_x360(&self, _on_rumble: RumbleSink) -> Result<Box<dyn VirtualPad>, VirtualError> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.fail_plug.load(SeqCst) {
            return Err(VirtualError::NoFreeSlot);
        }
        self.log.lock().unwrap().push("plug".to_string());
        self.live.fetch_add(1, SeqCst);
        self.plugs.fetch_add(1, SeqCst);
        Ok(Box::new(FakeBusPad {
            index: self.next_index.fetch_add(1, SeqCst),
            last_input: self.last_input.clone(),
            live: self.live.clone(),
        }))
    }
}

pub(crate) struct FakeHider {
    pub log: Log,
    pub fail: bool,
    /// Arquivo `hidden.toml` consultado no momento de cada `hide`.
    pub hidden_path: std::path::PathBuf,
    /// Para cada `hide`: o id já constava em `hidden.toml` naquele instante?
    pub recorded: Arc<Mutex<Vec<bool>>>,
    /// Quando ligado, `unhide_all` falha.
    pub fail_unhide_all: Arc<std::sync::atomic::AtomicBool>,
}

impl DeviceHider for FakeHider {
    fn hide(&self, instance_id: &str) -> Result<(), HideError> {
        let present =
            std::fs::read_to_string(&self.hidden_path).is_ok_and(|t| t.contains(instance_id));
        self.recorded.lock().unwrap().push(present);
        self.log.lock().unwrap().push(format!("hide:{instance_id}"));
        if self.fail {
            Err(HideError::Win32(5))
        } else {
            Ok(())
        }
    }
    fn unhide(&self, instance_id: &str) -> Result<(), HideError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("unhide:{instance_id}"));
        Ok(())
    }
    fn unhide_all(&self, instance_ids: &[String]) -> Result<(), HideError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("unhide_all:{}", instance_ids.join(",")));
        if self
            .fail_unhide_all
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(HideError::Win32(5));
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct FakeNotifier {
    pub messages: Arc<Mutex<Vec<(String, String)>>>,
}

impl Notifier for FakeNotifier {
    fn notify(&self, title: &str, body: &str) {
        self.messages
            .lock()
            .unwrap()
            .push((title.to_string(), body.to_string()));
    }
}

#[derive(Clone, Default)]
pub(crate) struct FakeScheduler {
    pub scheduled: Arc<Mutex<Vec<(Duration, Event)>>>,
}

impl Scheduler for FakeScheduler {
    fn after(&self, delay: Duration, event: Event) {
        self.scheduled.lock().unwrap().push((delay, event));
    }
}
