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
