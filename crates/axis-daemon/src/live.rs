//! Amostra ao vivo de um controle: o worker publica só quando alguém inspeciona.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axis_core::repair::{CaptureKind, Outcome, Stick};
use axis_core::stages::calibration::SECTORS;
use axis_core::{
    Corrections, DeviceFamily, InputState, StickCorrection, StickTrace, Trace, TriggerCalibration,
    Vec2,
};
use axis_devices::DeviceStatus;
use axis_protocol::{
    CaptureKindDto, CaptureResultDto, CaptureState, CorrectionsDto, Family, PadState, SessionInfo,
    StickCorrectionDto, StickStages, TriggerCalDto,
};

/// Estado da captura do kit de correção; o resultado fica até a próxima
/// `StartCapture`/`CancelCapture`.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureSnapshot {
    pub kind: CaptureKind,
    pub progress: f32,
    pub outcome: Option<Outcome>,
}

/// Fotografia do que o worker viu e produziu no último report publicado.
#[derive(Clone, Debug, Default, PartialEq)]
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
    pub capture: Option<CaptureSnapshot>,
    /// p99 de |intervalo − média| entre os últimos 256 reports, em µs.
    pub jitter_us: u32,
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
        self.sample
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
    /// Os avisos mais recentes (até 20).
    pub notices: Vec<String>,
    /// Total de avisos já guardados, inclusive os que saíram da janela.
    pub notices_total: u64,
}

/// Número máximo de avisos guardados.
const MAX_NOTICES: usize = 20;

struct Inner {
    ui: UiState,
    /// Quantos clientes inspecionam cada id; sobrevive à troca da torneira.
    inspecting: HashMap<String, usize>,
    /// Incrementado por `wake`: acorda quem espera sem mudar a geração.
    wakes: u64,
}

/// Ponte gerente -> servidor da interface: estado versionado e espera por mudança.
pub struct UiRegistry {
    state: Mutex<Inner>,
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
            state: Mutex::new(Inner {
                ui: UiState {
                    generation: 0,
                    sessions: Vec::new(),
                    taps: HashMap::new(),
                    paused: false,
                    emulation: true,
                    notices: Vec::new(),
                    notices_total: 0,
                },
                inspecting: HashMap::new(),
                wakes: 0,
            }),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
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
        let mut guard = self.lock();
        let inner = &mut *guard;
        let st = &mut inner.ui;
        if st.sessions == sessions
            && st.paused == paused
            && st.emulation == emulation
            && same_taps(&st.taps, &taps)
        {
            return;
        }
        // Torneira que saiu do registro (ou foi trocada) não fica ligada à toa.
        for (id, old) in &st.taps {
            if !taps.get(id).is_some_and(|t| Arc::ptr_eq(t, old)) {
                old.set_enabled(false);
            }
        }
        for (id, tap) in &taps {
            tap.set_enabled(inner.inspecting.get(id).is_some_and(|&n| n > 0));
        }
        st.sessions = sessions;
        st.taps = taps;
        st.paused = paused;
        st.emulation = emulation;
        st.generation += 1;
        drop(guard);
        self.changed.notify_all();
    }

    /// Guarda um aviso (os 20 mais recentes) e acorda quem espera.
    pub fn push_notice(&self, text: &str) {
        let mut guard = self.lock();
        let st = &mut guard.ui;
        st.notices.push(text.to_string());
        let excess = st.notices.len().saturating_sub(MAX_NOTICES);
        st.notices.drain(..excess);
        st.notices_total += 1;
        st.generation += 1;
        drop(guard);
        self.changed.notify_all();
    }

    pub fn snapshot(&self) -> UiState {
        self.lock().ui.clone()
    }

    /// Torneira atual do controle `id`, se houver.
    pub fn tap(&self, id: &str) -> Option<Arc<LiveTap>> {
        self.lock().ui.taps.get(id).cloned()
    }

    /// Um cliente passa a inspecionar (`on`) ou deixa de inspecionar `id`.
    /// A torneira fica ligada enquanto algum cliente a inspeciona.
    pub fn inspect(&self, id: &str, on: bool) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let count = if on {
            let n = inner.inspecting.entry(id.to_string()).or_insert(0);
            *n += 1;
            *n
        } else {
            match inner.inspecting.get_mut(id) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    *n
                }
                _ => {
                    inner.inspecting.remove(id);
                    0
                }
            }
        };
        if let Some(tap) = inner.ui.taps.get(id) {
            tap.set_enabled(count > 0);
        }
    }

    /// Acorda quem está em `wait_change` sem mudar a geração.
    pub fn wake(&self) {
        self.lock().wakes += 1;
        self.changed.notify_all();
    }

    /// Devolve a geração atual: na hora se já difere de `since`, senão espera
    /// a próxima mudança (até `timeout`, ou sem limite com `None`). Um `wake`
    /// também encerra a espera (com a mesma geração).
    pub fn wait_change(&self, since: u64, timeout: Option<Duration>) -> u64 {
        let guard = self.lock();
        let wakes = guard.wakes;
        let waiting = |s: &mut Inner| s.ui.generation == since && s.wakes == wakes;
        let guard = match timeout {
            Some(t) => {
                self.changed
                    .wait_timeout_while(guard, t, waiting)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
            None => self
                .changed
                .wait_while(guard, waiting)
                .unwrap_or_else(|e| e.into_inner()),
        };
        guard.ui.generation
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn pair(v: Vec2) -> [f32; 2] {
    [finite(v.x), finite(v.y)]
}

/// Converte o estado do núcleo para o do protocolo (não finitos viram 0).
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

fn to_stick_dto(c: &StickCorrection) -> StickCorrectionDto {
    StickCorrectionDto {
        center: c.center,
        outer_radius: c.outer_radius.to_vec(),
        drift_radius: c.drift_radius,
    }
}

fn to_trigger_dto(t: &TriggerCalibration) -> TriggerCalDto {
    TriggerCalDto {
        rest: t.rest,
        max: t.max,
    }
}

fn from_trigger_dto(t: &TriggerCalDto) -> TriggerCalibration {
    TriggerCalibration {
        rest: t.rest,
        max: t.max,
    }
}

/// Correções do núcleo para o protocolo.
pub(crate) fn to_corrections_dto(c: &Corrections) -> CorrectionsDto {
    CorrectionsDto {
        left: to_stick_dto(&c.left),
        right: to_stick_dto(&c.right),
        l2: to_trigger_dto(&c.l2),
        r2: to_trigger_dto(&c.r2),
    }
}

fn from_stick_dto(path: &str, d: &StickCorrectionDto) -> Result<StickCorrection, String> {
    let outer_radius: [f32; SECTORS] = d.outer_radius.as_slice().try_into().map_err(|_| {
        format!(
            "{path}.outer_radius precisa de {SECTORS} setores (veio {})",
            d.outer_radius.len()
        )
    })?;
    Ok(StickCorrection {
        center: d.center,
        outer_radius,
        drift_radius: d.drift_radius,
    })
}

/// Única conversão do protocolo para o núcleo: exige exatamente `SECTORS` setores
/// por analógico. Os limites dos valores ficam com `Corrections::validate`.
pub(crate) fn from_corrections_dto(d: &CorrectionsDto) -> Result<Corrections, String> {
    Ok(Corrections {
        left: from_stick_dto("left", &d.left)?,
        right: from_stick_dto("right", &d.right)?,
        l2: from_trigger_dto(&d.l2),
        r2: from_trigger_dto(&d.r2),
    })
}

pub(crate) fn from_capture_kind(k: CaptureKindDto) -> CaptureKind {
    match k {
        CaptureKindDto::DriftLeft => CaptureKind::Drift(Stick::Left),
        CaptureKindDto::DriftRight => CaptureKind::Drift(Stick::Right),
        CaptureKindDto::RangeLeft => CaptureKind::Range(Stick::Left),
        CaptureKindDto::RangeRight => CaptureKind::Range(Stick::Right),
        CaptureKindDto::Triggers => CaptureKind::Triggers,
    }
}

fn to_capture_kind(k: CaptureKind) -> CaptureKindDto {
    match k {
        CaptureKind::Drift(Stick::Left) => CaptureKindDto::DriftLeft,
        CaptureKind::Drift(Stick::Right) => CaptureKindDto::DriftRight,
        CaptureKind::Range(Stick::Left) => CaptureKindDto::RangeLeft,
        CaptureKind::Range(Stick::Right) => CaptureKindDto::RangeRight,
        CaptureKind::Triggers => CaptureKindDto::Triggers,
    }
}

fn finite_trigger(t: &TriggerCalibration) -> TriggerCalDto {
    TriggerCalDto {
        rest: finite(t.rest),
        max: finite(t.max),
    }
}

fn to_capture_result(o: &Outcome) -> CaptureResultDto {
    match o {
        Outcome::Drift(r) => CaptureResultDto::Drift {
            center: pair(r.center),
            noise: finite(r.noise),
            radius: finite(r.radius),
            worn: r.worn,
        },
        Outcome::Range(r) => CaptureResultDto::Range {
            outer_radius: r.outer_radius.iter().map(|&v| finite(v)).collect(),
            circularity_pct: finite(r.circularity_pct),
        },
        Outcome::Triggers(r) => CaptureResultDto::Triggers {
            l2: r.l2.as_ref().map(finite_trigger),
            r2: r.r2.as_ref().map(finite_trigger),
            l2_presses: r.l2_presses,
            r2_presses: r.r2_presses,
        },
        Outcome::Rejected(m) => CaptureResultDto::Rejected {
            reason: (*m).to_string(),
        },
        Outcome::Incomplete(m) => CaptureResultDto::Incomplete {
            reason: (*m).to_string(),
        },
        Outcome::Aborted => CaptureResultDto::Aborted,
    }
}

/// Estado da captura para o protocolo (não finitos viram 0).
pub(crate) fn to_capture_state(s: &CaptureSnapshot) -> CaptureState {
    CaptureState {
        kind: to_capture_kind(s.kind),
        progress: finite(s.progress),
        result: s.outcome.as_ref().map(to_capture_result),
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
            transport: None,
            disconnects: 0,
            corrections: Default::default(),
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
    fn inspect_counts_per_id_and_follows_tap_replacement() {
        let ui = UiRegistry::new();
        let tap = LiveTap::new();
        ui.publish(
            vec![info("a")],
            HashMap::from([("a".to_string(), tap.clone())]),
            false,
            true,
        );
        ui.inspect("a", true);
        ui.inspect("a", true);
        assert!(tap.enabled());
        ui.inspect("a", false);
        assert!(tap.enabled(), "outro cliente ainda inspeciona");
        // Torneira trocada (nova sessão): o contador do id vale para a nova.
        let novo = LiveTap::new();
        ui.publish(
            vec![info("a")],
            HashMap::from([("a".to_string(), novo.clone())]),
            false,
            true,
        );
        assert!(novo.enabled());
        assert!(
            !tap.enabled(),
            "a torneira antiga sai do registro desligada"
        );
        ui.inspect("a", false);
        assert!(!novo.enabled());
        // Desligar a mais não deixa o contador negativo.
        ui.inspect("a", false);
        ui.inspect("a", true);
        assert!(novo.enabled());
        // Id ainda sem torneira: liga quando ela aparecer.
        ui.inspect("b", true);
        let tb = LiveTap::new();
        ui.publish(
            vec![info("a"), info("b")],
            HashMap::from([
                ("a".to_string(), novo.clone()),
                ("b".to_string(), tb.clone()),
            ]),
            false,
            true,
        );
        assert!(tb.enabled());
    }

    #[test]
    fn notices_total_keeps_counting_past_the_window() {
        let ui = UiRegistry::new();
        for i in 0..25 {
            ui.push_notice(&format!("n{i}"));
        }
        let s = ui.snapshot();
        assert_eq!(s.notices_total, 25);
        assert_eq!(s.notices.len(), 20);
    }

    #[test]
    fn wake_returns_wait_change_early_with_the_same_generation() {
        let ui = UiRegistry::new();
        let g = ui.snapshot().generation;
        let ui2 = ui.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            ui2.wake();
        });
        let start = std::time::Instant::now();
        assert_eq!(ui.wait_change(g, Some(Duration::from_secs(5))), g);
        assert!(start.elapsed() < Duration::from_secs(2));
        t.join().unwrap();
    }

    #[test]
    fn tap_lookup_by_id() {
        let ui = UiRegistry::new();
        let tap = LiveTap::new();
        ui.publish(
            vec![info("a")],
            HashMap::from([("a".to_string(), tap.clone())]),
            false,
            true,
        );
        assert!(Arc::ptr_eq(&ui.tap("a").unwrap(), &tap));
        assert!(ui.tap("b").is_none());
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

    fn sample_corrections() -> axis_core::Corrections {
        let mut c = axis_core::Corrections::default();
        c.left.center = [0.02, -0.01];
        c.left.outer_radius[3] = 0.9;
        c.right.drift_radius = 0.05;
        c.right.outer_radius[15] = 1.2;
        c.l2 = axis_core::TriggerCalibration {
            rest: 0.04,
            max: 0.97,
        };
        c.r2.max = 0.8;
        c
    }

    #[test]
    fn corrections_round_trip_through_the_dto() {
        let c = sample_corrections();
        let dto = to_corrections_dto(&c);
        assert_eq!(dto.left.center, [0.02, -0.01]);
        assert_eq!(dto.left.outer_radius.len(), 16);
        assert_eq!(dto.left.outer_radius[3], 0.9);
        assert_eq!(dto.l2.rest, 0.04);
        assert_eq!(from_corrections_dto(&dto), Ok(c));
    }

    #[test]
    fn dto_with_wrong_sector_count_is_rejected() {
        let mut dto = to_corrections_dto(&sample_corrections());
        dto.left.outer_radius.pop();
        assert_eq!(dto.left.outer_radius.len(), 15);
        let e = from_corrections_dto(&dto).unwrap_err();
        assert!(e.starts_with("left.outer_radius"), "{e}");
        let mut dto = to_corrections_dto(&sample_corrections());
        dto.right.outer_radius.push(1.0);
        let e = from_corrections_dto(&dto).unwrap_err();
        assert!(e.starts_with("right.outer_radius"), "{e}");
    }

    #[test]
    fn capture_snapshot_maps_to_the_protocol() {
        use axis_core::repair::{DriftReport, RangeReport, Stick, TriggerReport};
        use axis_protocol::{CaptureKindDto, CaptureResultDto, TriggerCalDto};
        let snap = |kind, outcome| CaptureSnapshot {
            kind,
            progress: 0.5,
            outcome,
        };
        let s = to_capture_state(&snap(CaptureKind::Drift(Stick::Right), None));
        assert_eq!(
            (s.kind, s.progress, s.result),
            (CaptureKindDto::DriftRight, 0.5, None)
        );
        let s = to_capture_state(&snap(
            CaptureKind::Drift(Stick::Left),
            Some(Outcome::Drift(DriftReport {
                center: Vec2 { x: 0.1, y: -0.1 },
                noise: 0.01,
                radius: 0.04,
                worn: true,
                samples: 300,
            })),
        ));
        assert_eq!(s.kind, CaptureKindDto::DriftLeft);
        assert_eq!(
            s.result,
            Some(CaptureResultDto::Drift {
                center: [0.1, -0.1],
                noise: 0.01,
                radius: 0.04,
                worn: true
            })
        );
        let s = to_capture_state(&snap(
            CaptureKind::Range(Stick::Left),
            Some(Outcome::Range(RangeReport {
                outer_radius: [0.9; 16],
                circularity_pct: 2.5,
            })),
        ));
        assert_eq!(s.kind, CaptureKindDto::RangeLeft);
        assert_eq!(
            s.result,
            Some(CaptureResultDto::Range {
                outer_radius: vec![0.9; 16],
                circularity_pct: 2.5
            })
        );
        let s = to_capture_state(&snap(
            CaptureKind::Triggers,
            Some(Outcome::Triggers(TriggerReport {
                l2: Some(axis_core::TriggerCalibration {
                    rest: 0.05,
                    max: 0.95,
                }),
                r2: None,
                l2_presses: 3,
                r2_presses: 1,
            })),
        ));
        assert_eq!(s.kind, CaptureKindDto::Triggers);
        assert_eq!(
            s.result,
            Some(CaptureResultDto::Triggers {
                l2: Some(TriggerCalDto {
                    rest: 0.05,
                    max: 0.95
                }),
                r2: None,
                l2_presses: 3,
                r2_presses: 1
            })
        );
        let s = to_capture_state(&snap(
            CaptureKind::Range(Stick::Right),
            Some(Outcome::Rejected("mexeu")),
        ));
        assert_eq!(s.kind, CaptureKindDto::RangeRight);
        assert_eq!(
            s.result,
            Some(CaptureResultDto::Rejected {
                reason: "mexeu".into()
            })
        );
        let s = to_capture_state(&snap(
            CaptureKind::Triggers,
            Some(Outcome::Incomplete("faltou")),
        ));
        assert_eq!(
            s.result,
            Some(CaptureResultDto::Incomplete {
                reason: "faltou".into()
            })
        );
        let mut nan = snap(CaptureKind::Triggers, Some(Outcome::Aborted));
        nan.progress = f32::NAN;
        let s = to_capture_state(&nan);
        assert_eq!(
            (s.progress, s.result),
            (0.0, Some(CaptureResultDto::Aborted))
        );
    }

    #[test]
    fn capture_kind_from_the_protocol() {
        use axis_core::repair::Stick;
        use axis_protocol::CaptureKindDto as K;
        assert_eq!(
            from_capture_kind(K::DriftLeft),
            CaptureKind::Drift(Stick::Left)
        );
        assert_eq!(
            from_capture_kind(K::DriftRight),
            CaptureKind::Drift(Stick::Right)
        );
        assert_eq!(
            from_capture_kind(K::RangeLeft),
            CaptureKind::Range(Stick::Left)
        );
        assert_eq!(
            from_capture_kind(K::RangeRight),
            CaptureKind::Range(Stick::Right)
        );
        assert_eq!(from_capture_kind(K::Triggers), CaptureKind::Triggers);
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
