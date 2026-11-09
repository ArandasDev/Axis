//! Portas do gerente: tudo que toca o sistema fica atrás destes traits.

use std::sync::mpsc::Sender;
use std::time::Duration;

use axis_devices::{Controller, DeviceError, HidCandidate, HidController, enumerate_hid};
use hidapi::HidApi;

use crate::manager::Event;

/// Enumera e abre controles.
pub trait ControllerOpener: Send {
    fn enumerate(&mut self) -> Vec<HidCandidate>;
    fn open(&mut self, c: &HidCandidate) -> Result<Box<dyn Controller>, DeviceError>;
}

/// Agenda um evento para daqui a `delay`.
pub trait Scheduler: Send {
    fn after(&self, delay: Duration, event: Event);
}

/// Mostra uma mensagem ao usuário (bandeja).
pub trait Notifier: Send {
    fn notify(&self, title: &str, body: &str);
}

/// Abertor real, sobre hidapi.
pub struct HidOpener {
    api: HidApi,
}

impl HidOpener {
    pub fn new() -> Result<HidOpener, DeviceError> {
        let api = HidApi::new().map_err(|e| DeviceError::Hid(e.to_string()))?;
        Ok(HidOpener { api })
    }
}

impl ControllerOpener for HidOpener {
    fn enumerate(&mut self) -> Vec<HidCandidate> {
        if let Err(e) = self.api.refresh_devices() {
            log::warn!("falha ao atualizar a lista de dispositivos HID: {e}");
        }
        enumerate_hid(&self.api)
    }

    fn open(&mut self, c: &HidCandidate) -> Result<Box<dyn Controller>, DeviceError> {
        let ctl = HidController::open(&self.api, c)?;
        Ok(Box::new(ctl))
    }
}

/// Agendador real: uma thread que dorme e envia o evento.
pub struct ThreadScheduler {
    tx: Sender<Event>,
}

impl ThreadScheduler {
    pub fn new(tx: Sender<Event>) -> ThreadScheduler {
        ThreadScheduler { tx }
    }
}

impl Scheduler for ThreadScheduler {
    fn after(&self, delay: Duration, event: Event) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(event);
        });
    }
}
