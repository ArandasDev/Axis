//! Amostra ao vivo de um controle: o worker publica só quando alguém inspeciona.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axis_core::{DeviceFamily, InputState, StickTrace, Trace, Vec2};
use axis_devices::DeviceStatus;
use axis_protocol::{Family, PadState, SessionInfo, StickStages};

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

/// Estado que a interface enxerga; o gerente o atualiza a cada evento.
#[derive(Clone)]
pub struct UiState {
    pub generation: u64,
    pub sessions: Vec<SessionInfo>,
    pub taps: HashMap<String, Arc<LiveTap>>,
    pub paused: bool,
    pub emulation: bool,
    pub notices: Vec<String>,
}

/// Número máximo de avisos guardados.
const MAX_NOTICES: usize = 20;

/// Ponte gerente -> servidor da interface: estado versionado e espera por mudança.
pub struct UiRegistry {
    state: Mutex<UiState>,
    changed: Condvar,
}

fn same_taps(a: &HashMap<String, Arc<LiveTap>>, b: &HashMap<String, Arc<LiveTap>>) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| Arc::ptr_eq(v, w)))
}

impl UiRegistry {
    pub fn new() -> Arc<UiRegistry> {
        Arc::new(UiRegistry {
            state: Mutex::new(UiState {
                generation: 0,
                sessions: Vec::new(),
                taps: HashMap::new(),
                paused: false,
                emulation: true,
                notices: Vec::new(),
            }),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, UiState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Atualiza o estado; só incrementa a geração (e acorda quem espera) se algo mudou.
    pub fn publish(
        &self,
        sessions: Vec<SessionInfo>,
        taps: HashMap<String, Arc<LiveTap>>,
        paused: bool,
        emulation: bool,
    ) {
        let mut st = self.lock();
        if st.sessions == sessions
            && st.paused == paused
            && st.emulation == emulation
            && same_taps(&st.taps, &taps)
        {
            return;
        }
        st.sessions = sessions;
        st.taps = taps;
        st.paused = paused;
        st.emulation = emulation;
        st.generation += 1;
        drop(st);
        self.changed.notify_all();
    }

    /// Guarda um aviso (os 20 mais recentes) e acorda quem espera.
    pub fn push_notice(&self, text: &str) {
        let mut st = self.lock();
        st.notices.push(text.to_string());
        let excess = st.notices.len().saturating_sub(MAX_NOTICES);
        st.notices.drain(..excess);
        st.generation += 1;
        drop(st);
        self.changed.notify_all();
    }

    pub fn snapshot(&self) -> UiState {
        self.lock().clone()
    }

    /// Devolve a geração atual: na hora se já difere de `since`, senão espera
    /// a próxima mudança (até `timeout`, ou sem limite com `None`).
    pub fn wait_change(&self, since: u64, timeout: Option<Duration>) -> u64 {
        let guard = self.lock();
        let guard = match timeout {
            Some(t) => {
                self.changed
                    .wait_timeout_while(guard, t, |s| s.generation == since)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
            None => self
                .changed
                .wait_while(guard, |s| s.generation == since)
                .unwrap_or_else(|e| e.into_inner()),
        };
        guard.generation
    }
}

#[allow(dead_code)] // usadas pelo servidor IPC (task 4)
fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

#[allow(dead_code)] // usadas pelo servidor IPC (task 4)
fn pair(v: Vec2) -> [f32; 2] {
    [finite(v.x), finite(v.y)]
}

/// Converte o estado do núcleo para o do protocolo (não finitos viram 0).
#[allow(dead_code)] // usada pelo servidor IPC (task 4)
pub(crate) fn to_pad_state(s: &InputState) -> PadState {
    PadState {
        lx: finite(s.left.x),
        ly: finite(s.left.y),
        rx: finite(s.right.x),
        ry: finite(s.right.y),
        l2: finite(s.l2),
        r2: finite(s.r2),
        buttons: s.buttons.0,
    }
}

/// Converte o rastro de um analógico (não finitos viram 0).
#[allow(dead_code)] // usada pelo servidor IPC (task 4)
pub(crate) fn to_stages(t: &StickTrace) -> StickStages {
    StickStages {
        raw: pair(t.raw),
        calibrated: pair(t.calibrated),
        drift: pair(t.drift),
        deadzone: pair(t.deadzone),
        curve: pair(t.curve),
        anti: pair(t.anti),
    }
}

pub(crate) fn to_family(f: DeviceFamily) -> Family {
    match f {
        DeviceFamily::DualShock4 => Family::DualShock4,
        DeviceFamily::DualSense => Family::DualSense,
        DeviceFamily::Xbox => Family::Xbox,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axis_core::Button;

    fn info(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            owner: "Controle 1".into(),
            color: [1, 2, 3],
            family: Family::DualShock4,
            player: Some(1),
            connected: true,
            experimental: false,
            view_only: false,
            battery: None,
            charging: false,
        }
    }

    #[test]
    fn publish_bumps_generation_only_on_change() {
        let ui = UiRegistry::new();
        let g0 = ui.snapshot().generation;
        let tap = LiveTap::new();
        let mk = || HashMap::from([("a".to_string(), tap.clone())]);
        ui.publish(vec![info("a")], mk(), false, true);
        assert_eq!(ui.snapshot().generation, g0 + 1);
        ui.publish(vec![info("a")], mk(), false, true);
        assert_eq!(ui.snapshot().generation, g0 + 1);
        ui.publish(vec![info("a")], mk(), true, true);
        assert_eq!(ui.snapshot().generation, g0 + 2);
        // Torneira diferente (mesma chave) também conta.
        ui.publish(
            vec![info("a")],
            HashMap::from([("a".to_string(), LiveTap::new())]),
            true,
            true,
        );
        assert_eq!(ui.snapshot().generation, g0 + 3);
    }

    #[test]
    fn push_notice_bumps_and_keeps_last_twenty() {
        let ui = UiRegistry::new();
        for i in 0..25 {
            ui.push_notice(&format!("n{i}"));
        }
        let s = ui.snapshot();
        assert_eq!(s.generation, 25);
        assert_eq!(s.notices.len(), 20);
        assert_eq!(s.notices[0], "n5");
        assert_eq!(s.notices[19], "n24");
    }

    #[test]
    fn wait_change_returns_on_publish_and_times_out() {
        let ui = UiRegistry::new();
        let g = ui.snapshot().generation;
        let ui2 = ui.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            ui2.publish(vec![info("a")], HashMap::new(), false, true);
        });
        assert_eq!(ui.wait_change(g, Some(Duration::from_secs(1))), g + 1);
        t.join().unwrap();
        // Já mudou em relação a `since`: volta na hora.
        assert_eq!(ui.wait_change(g, None), g + 1);
        // Sem publicação: expira e devolve a mesma geração.
        assert_eq!(
            ui.wait_change(g + 1, Some(Duration::from_millis(30))),
            g + 1
        );
    }

    #[test]
    fn conversions_map_buttons_and_stages() {
        let mut input = InputState::default();
        input.buttons.set(Button::South, true);
        input.left = Vec2 { x: 0.5, y: -0.25 };
        input.r2 = 1.0;
        let p = to_pad_state(&input);
        assert_eq!(p.buttons, 1);
        assert_eq!((p.lx, p.ly, p.rx, p.r2), (0.5, -0.25, 0.0, 1.0));

        let v = |x, y| Vec2 { x, y };
        let t = StickTrace {
            raw: v(0.1, 0.2),
            calibrated: v(0.3, 0.4),
            drift: v(0.5, 0.6),
            deadzone: v(0.7, 0.8),
            curve: v(0.9, 1.0),
            anti: v(-0.1, -0.2),
        };
        let s = to_stages(&t);
        assert_eq!(s.raw, [0.1, 0.2]);
        assert_eq!(s.calibrated, [0.3, 0.4]);
        assert_eq!(s.drift, [0.5, 0.6]);
        assert_eq!(s.deadzone, [0.7, 0.8]);
        assert_eq!(s.curve, [0.9, 1.0]);
        assert_eq!(s.anti, [-0.1, -0.2]);

        assert_eq!(to_family(DeviceFamily::DualSense), Family::DualSense);
        assert_eq!(to_family(DeviceFamily::Xbox), Family::Xbox);
        assert_eq!(to_family(DeviceFamily::DualShock4), Family::DualShock4);
    }

    #[test]
    fn conversions_sanitize_non_finite() {
        let input = InputState {
            left: Vec2 {
                x: f32::NAN,
                y: f32::INFINITY,
            },
            l2: f32::NEG_INFINITY,
            ..Default::default()
        };
        let p = to_pad_state(&input);
        assert_eq!((p.lx, p.ly, p.l2), (0.0, 0.0, 0.0));
        let t = StickTrace {
            raw: Vec2 {
                x: f32::NAN,
                y: 0.5,
            },
            ..Default::default()
        };
        assert_eq!(to_stages(&t).raw, [0.0, 0.5]);
    }
}
