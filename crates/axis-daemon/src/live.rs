//! Amostra ao vivo de um controle: o worker publica só quando alguém inspeciona.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axis_core::{InputState, Trace};
use axis_devices::DeviceStatus;

/// Fotografia do que o worker viu e produziu no último report publicado.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiveSample {
    pub input: InputState,
    pub output: InputState,
    pub trace: Trace,
    pub status: DeviceStatus,
    /// Total de reports lidos.
    pub reports: u64,
    /// p99 da última janela de 60 s do histograma (0 antes da primeira).
    pub latency_p99_us: u32,
    pub seq: u64,
}

/// Canal worker -> UI: desligado por padrão, o worker não paga nada por ele.
pub struct LiveTap {
    enabled: AtomicBool,
    sample: Mutex<LiveSample>,
}

impl LiveTap {
    pub fn new() -> Arc<LiveTap> {
        Arc::new(LiveTap {
            enabled: AtomicBool::new(false),
            sample: Mutex::new(LiveSample::default()),
        })
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn latest(&self) -> LiveSample {
        *self.sample.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn publish(&self, s: LiveSample) {
        *self.sample.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }
}
