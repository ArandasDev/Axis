//! Captura e análise do kit de correção: o worker chama `push` a cada report
//! (sem alocação) e `finish` uma vez, ao fim da captura.

use crate::input::{InputState, Vec2};
use crate::settings::TriggerCalibration;
use crate::stages::calibration::SECTORS;
use std::f32::consts::{PI, TAU};

const DRIFT_CAPACITY: usize = 4096;
const RANGE_CAPACITY: usize = 16384;
const TRIGGER_CAPACITY: usize = 16384;

const DRIFT_SECS: f32 = 3.0;
const TIMEOUT_SECS: f32 = 30.0;
const RANGE_TURNS: f32 = 3.0;
const RANGE_MIN_MAGNITUDE: f32 = 0.5;
const ALL_SECTORS: u16 = u16::MAX;
const PRESSES: u8 = 3;
const PRESS_HIGH: f32 = 0.8;
const PRESS_LOW: f32 = 0.2;
const MIN_TRIGGER_SPAN: f32 = 0.3;

const MSG_DRIFT_MOVED: &str = "O analógico mexeu durante a medição. Solte e tente de novo.";
const MSG_NO_SAMPLES: &str = "Nenhuma leitura do controle durante a medição.";
const MSG_RANGE: &str = "Faltou girar até a borda em todas as direções.";
const MSG_TRIGGERS: &str = "Aperte cada gatilho até o fim e solte, 3 vezes.";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stick {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaptureKind {
    Drift(Stick),
    Range(Stick),
    Triggers,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DriftReport {
    pub center: Vec2,
    pub noise: f32,
    pub radius: f32,
    pub worn: bool,
    pub samples: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeReport {
    pub outer_radius: [f32; SECTORS],
    pub circularity_pct: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerReport {
    pub l2: Option<TriggerCalibration>,
    pub r2: Option<TriggerCalibration>,
    pub l2_presses: u8,
    pub r2_presses: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Drift(DriftReport),
    Range(RangeReport),
    Triggers(TriggerReport),
    Rejected(&'static str),
    Incomplete(&'static str),
    /// Nunca sai de `Capture::finish`; o worker o usa quando a captura é interrompida.
    Aborted,
}

/// Contador de apertos de um gatilho, com histerese sobre o intervalo observado.
#[derive(Clone, Copy, Debug)]
struct TriggerTrack {
    min: f32,
    max: f32,
    pressed: bool,
    presses: u8,
}

impl TriggerTrack {
    const fn new() -> Self {
        Self {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            pressed: false,
            presses: 0,
        }
    }

    fn push(&mut self, v: f32) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        let span = self.max - self.min;
        if span < MIN_TRIGGER_SPAN {
            return;
        }
        let n = (v - self.min) / span;
        if !self.pressed && n > PRESS_HIGH {
            self.pressed = true;
        } else if self.pressed && n < PRESS_LOW {
            self.pressed = false;
            self.presses = self.presses.saturating_add(1);
        }
    }
}

/// Uma captura em andamento. O buffer é alocado em `new`; `push` nunca aloca.
#[derive(Clone, Debug)]
pub struct Capture {
    kind: CaptureKind,
    center: Vec2,
    samples: Vec<Vec2>,
    capacity: usize,
    t0: Option<f32>,
    elapsed: f32,
    done: bool,
    // alcance
    covered: u16,
    angle_acc: f32,
    last_angle: Option<f32>,
    // gatilhos
    l2: TriggerTrack,
    r2: TriggerTrack,
}

impl Capture {
    /// `center` é o centro atual do analógico (usado pelo alcance).
    pub fn new(kind: CaptureKind, center: Vec2) -> Capture {
        let capacity = match kind {
            CaptureKind::Drift(_) => DRIFT_CAPACITY,
            CaptureKind::Range(_) => RANGE_CAPACITY,
            CaptureKind::Triggers => TRIGGER_CAPACITY,
        };
        Capture {
            kind,
            center,
            samples: Vec::with_capacity(capacity),
            capacity,
            t0: None,
            elapsed: 0.0,
            done: false,
            covered: 0,
            angle_acc: 0.0,
            last_angle: None,
            l2: TriggerTrack::new(),
            r2: TriggerTrack::new(),
        }
    }

    /// Registra um report. `t_secs` é qualquer relógio monotônico; o tempo conta a
    /// partir do primeiro report. Valores não finitos são ignorados; amostras além da
    /// capacidade são descartadas (o tempo continua contando).
    pub fn push(&mut self, input: &InputState, t_secs: f32) {
        if self.done || !t_secs.is_finite() {
            return;
        }
        let t0 = *self.t0.get_or_insert(t_secs);
        self.elapsed = (t_secs - t0).max(self.elapsed);

        let raw = match self.kind {
            CaptureKind::Drift(s) | CaptureKind::Range(s) => stick(input, s),
            CaptureKind::Triggers => Vec2 {
                x: input.l2,
                y: input.r2,
            },
        };
        // a checagem vem antes do recorte: `clamp` tornaria ±inf finito
        if raw.x.is_finite() && raw.y.is_finite() && self.samples.len() < self.capacity {
            let sample = match self.kind {
                CaptureKind::Triggers => Vec2 {
                    x: raw.x.clamp(0.0, 1.0),
                    y: raw.y.clamp(0.0, 1.0),
                },
                _ => clamp_stick(raw),
            };
            self.samples.push(sample);
            match self.kind {
                CaptureKind::Drift(_) => {}
                CaptureKind::Range(_) => self.track_range(sample),
                CaptureKind::Triggers => {
                    self.l2.push(sample.x);
                    self.r2.push(sample.y);
                }
            }
        }

        self.done = match self.kind {
            CaptureKind::Drift(_) => self.elapsed >= DRIFT_SECS,
            CaptureKind::Range(_) => self.range_complete() || self.elapsed >= TIMEOUT_SECS,
            CaptureKind::Triggers => self.triggers_complete() || self.elapsed >= TIMEOUT_SECS,
        };
    }

    fn track_range(&mut self, raw: Vec2) {
        let v = sub(raw, self.center);
        if v.length() <= RANGE_MIN_MAGNITUDE {
            self.last_angle = None;
            return;
        }
        let a = angle(v);
        self.covered |= 1 << sector(a);
        if let Some(last) = self.last_angle {
            let mut d = a - last;
            if d > PI {
                d -= TAU;
            } else if d < -PI {
                d += TAU;
            }
            self.angle_acc += d;
        }
        self.last_angle = Some(a);
    }

    fn turns(&self) -> f32 {
        self.angle_acc.abs() / TAU
    }

    fn range_complete(&self) -> bool {
        self.turns() >= RANGE_TURNS && self.covered == ALL_SECTORS
    }

    fn triggers_complete(&self) -> bool {
        self.l2.presses >= PRESSES && self.r2.presses >= PRESSES
    }

    /// Progresso em 0..=1.
    pub fn progress(&self) -> f32 {
        if self.done {
            return 1.0;
        }
        let p = match self.kind {
            CaptureKind::Drift(_) => self.elapsed / DRIFT_SECS,
            CaptureKind::Range(_) => {
                let sectors = self.covered.count_ones() as f32 / SECTORS as f32;
                (self.turns() / RANGE_TURNS).min(sectors)
            }
            CaptureKind::Triggers => {
                let l = self.l2.presses.min(PRESSES);
                let r = self.r2.presses.min(PRESSES);
                f32::from(l + r) / f32::from(2 * PRESSES)
            }
        };
        p.clamp(0.0, 1.0)
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Analisa o que foi capturado. Nunca devolve `Outcome::Aborted`.
    pub fn finish(&self) -> Outcome {
        match self.kind {
            CaptureKind::Drift(_) => self.finish_drift(),
            CaptureKind::Range(_) => self.finish_range(),
            CaptureKind::Triggers => self.finish_triggers(),
        }
    }

    fn finish_drift(&self) -> Outcome {
        if self.samples.is_empty() {
            return Outcome::Incomplete(MSG_NO_SAMPLES);
        }
        let n = self.samples.len() as f32;
        let sum = self.samples.iter().fold(Vec2::default(), |a, s| Vec2 {
            x: a.x + s.x,
            y: a.y + s.y,
        });
        let center = sum.scale(1.0 / n);
        let mut dist: Vec<f32> = self
            .samples
            .iter()
            .map(|s| sub(*s, center).length())
            .collect();
        dist.sort_unstable_by(f32::total_cmp);
        let p99 = ((dist.len() as f32 * 0.99).ceil() as usize).clamp(1, dist.len()) - 1;
        let noise = dist[p99];
        if noise > 0.25 || center.length() >= 0.5 {
            return Outcome::Rejected(MSG_DRIFT_MOVED);
        }
        let radius = (noise * 1.25 + 0.02).clamp(0.03, 0.5);
        Outcome::Drift(DriftReport {
            center,
            noise,
            radius,
            worn: center.length() + radius > 0.15,
            samples: self.samples.len() as u32,
        })
    }

    fn finish_range(&self) -> Outcome {
        if !self.range_complete() {
            return Outcome::Incomplete(MSG_RANGE);
        }
        let mut outer = [0.0f32; SECTORS];
        for s in &self.samples {
            let v = sub(*s, self.center);
            let m = v.length();
            if m > RANGE_MIN_MAGNITUDE {
                let i = sector(angle(v));
                outer[i] = outer[i].max(m);
            }
        }
        for r in &mut outer {
            *r = r.clamp(0.3, 1.5);
        }
        let mean = outer.iter().sum::<f32>() / SECTORS as f32;
        let dev = outer.iter().map(|r| (r - mean).abs()).sum::<f32>() / SECTORS as f32;
        Outcome::Range(RangeReport {
            outer_radius: outer,
            circularity_pct: dev / mean * 100.0,
        })
    }

    fn finish_triggers(&self) -> Outcome {
        let l2 = trigger_calibration(&self.l2, self.samples.iter().map(|s| s.x));
        let r2 = trigger_calibration(&self.r2, self.samples.iter().map(|s| s.y));
        if l2.is_none() && r2.is_none() {
            return Outcome::Incomplete(MSG_TRIGGERS);
        }
        Outcome::Triggers(TriggerReport {
            l2,
            r2,
            l2_presses: self.l2.presses,
            r2_presses: self.r2.presses,
        })
    }
}

fn trigger_calibration(
    track: &TriggerTrack,
    values: impl Iterator<Item = f32>,
) -> Option<TriggerCalibration> {
    if track.presses < PRESSES {
        return None;
    }
    let span = track.max - track.min;
    let threshold = track.min + span * PRESS_LOW;
    let rest_peak = values.filter(|v| *v < threshold).fold(track.min, f32::max);
    let rest = (rest_peak + 0.01).clamp(0.0, 1.0);
    let max = (track.max - 0.01).clamp(0.0, 1.0);
    (max - rest >= MIN_TRIGGER_SPAN).then_some(TriggerCalibration { rest, max })
}

fn stick(input: &InputState, s: Stick) -> Vec2 {
    match s {
        Stick::Left => input.left,
        Stick::Right => input.right,
    }
}

/// Mesmo recorte de `calibrate`: cada eixo cru em [-1, 1].
fn clamp_stick(v: Vec2) -> Vec2 {
    Vec2 {
        x: v.x.clamp(-1.0, 1.0),
        y: v.y.clamp(-1.0, 1.0),
    }
}

fn sub(a: Vec2, b: Vec2) -> Vec2 {
    Vec2 {
        x: a.x - b.x,
        y: a.y - b.y,
    }
}

/// Ângulo em [0, 2π), como em `calibrate`.
fn angle(v: Vec2) -> f32 {
    let a = v.y.atan2(v.x);
    if a < 0.0 { a + TAU } else { a }
}

/// Índice de setor como em `calibrate`.
fn sector(a: f32) -> usize {
    (a / (TAU / SECTORS as f32)).floor() as usize % SECTORS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{InputState, Vec2};
    use std::f32::consts::TAU;

    const HZ: f32 = 250.0;

    /// Gerador pseudoaleatório determinístico em [-1, 1].
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        }
    }

    fn left(x: f32, y: f32) -> InputState {
        InputState {
            left: Vec2 { x, y },
            ..Default::default()
        }
    }

    fn triggers(l2: f32, r2: f32) -> InputState {
        InputState {
            l2,
            r2,
            ..Default::default()
        }
    }

    fn drift_capture(cx: f32, cy: f32, noise: f32, seed: u32) -> Capture {
        let mut c = Capture::new(CaptureKind::Drift(Stick::Left), Vec2::default());
        let mut rng = Lcg(seed);
        for i in 0..=750 {
            let s = left(cx + rng.next() * noise, cy + rng.next() * noise);
            c.push(&s, i as f32 / HZ);
        }
        c
    }

    #[test]
    fn drift_finds_offset_and_noise() {
        let c = drift_capture(0.04, -0.03, 0.01, 7);
        assert!(c.is_done());
        assert_eq!(c.progress(), 1.0);
        let Outcome::Drift(r) = c.finish() else {
            panic!("esperava Drift");
        };
        assert!((r.center.x - 0.04).abs() <= 0.005, "{r:?}");
        assert!((r.center.y + 0.03).abs() <= 0.005, "{r:?}");
        assert!((0.03..=0.06).contains(&r.radius), "{r:?}");
        assert!(!r.worn);
        assert_eq!(r.samples, 751);
    }

    #[test]
    fn drift_is_not_done_before_three_seconds() {
        let mut c = Capture::new(CaptureKind::Drift(Stick::Left), Vec2::default());
        c.push(&left(0.0, 0.0), 0.0);
        c.push(&left(0.0, 0.0), 1.5);
        assert!(!c.is_done());
        assert!((c.progress() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn drift_flags_worn_stick() {
        let c = drift_capture(0.12, 0.0, 0.03, 11);
        let Outcome::Drift(r) = c.finish() else {
            panic!("esperava Drift");
        };
        assert!(r.worn, "{r:?}");
    }

    #[test]
    fn drift_rejects_moving_stick() {
        let mut c = Capture::new(CaptureKind::Drift(Stick::Left), Vec2::default());
        for i in 0..=750 {
            let x = if i % 2 == 0 { 0.6 } else { 0.0 };
            c.push(&left(x, 0.0), i as f32 / HZ);
        }
        assert_eq!(
            c.finish(),
            Outcome::Rejected("O analógico mexeu durante a medição. Solte e tente de novo.")
        );
    }

    #[test]
    fn drift_uses_the_requested_stick() {
        let mut c = Capture::new(CaptureKind::Drift(Stick::Right), Vec2::default());
        for i in 0..=750 {
            let s = InputState {
                left: Vec2 { x: 0.9, y: 0.0 },
                right: Vec2 { x: 0.02, y: 0.01 },
                ..Default::default()
            };
            c.push(&s, i as f32 / HZ);
        }
        let Outcome::Drift(r) = c.finish() else {
            panic!("esperava Drift");
        };
        assert!((r.center.x - 0.02).abs() < 1e-4 && (r.center.y - 0.01).abs() < 1e-4);
    }

    /// Gira `turns` voltas com raio dado pela função do ângulo; 2 s por volta.
    fn spin(c: &mut Capture, turns: f32, offset: Vec2, radius: impl Fn(f32) -> f32) -> f32 {
        let n = (turns * 2.0 * HZ) as usize;
        let mut t = 0.0;
        for i in 0..=n {
            let a = i as f32 / (2.0 * HZ) * TAU;
            let r = radius(a);
            t = i as f32 / HZ;
            c.push(&left(offset.x + r * a.cos(), offset.y + r * a.sin()), t);
        }
        t
    }

    fn range_report(c: &Capture) -> RangeReport {
        match c.finish() {
            Outcome::Range(r) => r,
            other => panic!("esperava Range, veio {other:?}"),
        }
    }

    #[test]
    fn range_measures_sectors_and_circularity() {
        let mut c = Capture::new(CaptureKind::Range(Stick::Left), Vec2::default());
        spin(&mut c, 3.05, Vec2::default(), |a| {
            (1.0 / a.cos().abs().max(a.sin().abs())).min(1.3)
        });
        assert!(c.is_done());
        assert_eq!(c.progress(), 1.0);
        let r = range_report(&c);
        let diagonal = [1, 2, 5, 6, 9, 10, 13, 14];
        let cardinal = [0, 3, 4, 7, 8, 11, 12, 15];
        let min_diag = diagonal
            .iter()
            .map(|&i| r.outer_radius[i])
            .fold(f32::MAX, f32::min);
        let max_card = cardinal
            .iter()
            .map(|&i| r.outer_radius[i])
            .fold(0.0, f32::max);
        assert!(min_diag > max_card, "{r:?}");
        assert!(r.circularity_pct > 5.0, "{r:?}");

        let mut c = Capture::new(CaptureKind::Range(Stick::Left), Vec2::default());
        spin(&mut c, 3.05, Vec2::default(), |_| 0.95);
        let r = range_report(&c);
        for v in r.outer_radius {
            assert!((v - 0.95).abs() < 0.01, "{r:?}");
        }
        assert!(r.circularity_pct < 1.0, "{r:?}");
    }

    #[test]
    fn range_incomplete_without_full_turns() {
        let mut c = Capture::new(CaptureKind::Range(Stick::Left), Vec2::default());
        let mut t = spin(&mut c, 1.5, Vec2::default(), |_| 0.95);
        assert!(!c.is_done());
        while t < 30.0 {
            t += 1.0 / HZ;
            c.push(&left(0.0, 0.0), t.min(30.0));
        }
        assert!(c.is_done());
        assert_eq!(
            c.finish(),
            Outcome::Incomplete("Faltou girar até a borda em todas as direções.")
        );
    }

    #[test]
    fn range_subtracts_center() {
        let center = Vec2 { x: 0.05, y: 0.0 };
        let mut c = Capture::new(CaptureKind::Range(Stick::Left), center);
        spin(&mut c, 3.05, center, |_| 0.9);
        let r = range_report(&c);
        for v in r.outer_radius {
            assert!((v - 0.9).abs() < 0.01, "{r:?}");
        }
    }

    /// Ciclos de 100 amostras: 90 em repouso, 10 apertado no pico; `presses` ciclos com aperto.
    fn press_pattern(i: usize, presses: usize, rest: f32, peak: f32) -> f32 {
        if i / 100 < presses && i % 100 >= 90 {
            peak
        } else {
            rest
        }
    }

    #[test]
    fn triggers_count_presses_and_measure_rest_max() {
        let mut c = Capture::new(CaptureKind::Triggers, Vec2::default());
        let n = 30 * HZ as usize;
        for i in 0..=n {
            let l2 = press_pattern(i, 3, 0.04, 0.97);
            let r2 = press_pattern(i, 2, 0.03, 0.98);
            c.push(&triggers(l2, r2), i as f32 / HZ);
        }
        assert!(c.is_done());
        let Outcome::Triggers(r) = c.finish() else {
            panic!("esperava Triggers");
        };
        assert_eq!(r.l2_presses, 3);
        assert_eq!(r.r2_presses, 2);
        let l2 = r.l2.expect("L2 calibrado");
        assert!((l2.rest - 0.05).abs() < 1e-4, "{l2:?}");
        assert!((l2.max - 0.96).abs() < 1e-4, "{l2:?}");
        assert_eq!(r.r2, None);
    }

    #[test]
    fn triggers_finish_early_after_three_presses_each() {
        let mut c = Capture::new(CaptureKind::Triggers, Vec2::default());
        let mut i = 0;
        while !c.is_done() && i < 10_000 {
            let v = press_pattern(i, 3, 0.0, 1.0);
            c.push(&triggers(v, v), i as f32 / HZ);
            i += 1;
        }
        assert!(c.is_done());
        assert!(i < 400, "terminou só em {i}");
        assert!(matches!(c.finish(), Outcome::Triggers(_)));
    }

    #[test]
    fn triggers_without_presses_are_incomplete() {
        let mut c = Capture::new(CaptureKind::Triggers, Vec2::default());
        for i in 0..=(30 * HZ as usize) {
            c.push(&triggers(0.02, 0.02), i as f32 / HZ);
        }
        assert!(c.is_done());
        assert_eq!(
            c.finish(),
            Outcome::Incomplete("Aperte cada gatilho até o fim e solte, 3 vezes.")
        );
    }

    #[test]
    fn push_never_grows_beyond_capacity() {
        let mut c = Capture::new(CaptureKind::Drift(Stick::Left), Vec2::default());
        for i in 0..50_000 {
            c.push(&left(0.01, 0.0), i as f32 * 1e-5);
        }
        assert!(matches!(c.finish(), Outcome::Drift(r) if r.samples == 4096));
    }

    #[test]
    fn nan_samples_are_ignored() {
        let clean = drift_capture(0.04, -0.03, 0.01, 7).finish();
        let mut c = Capture::new(CaptureKind::Drift(Stick::Left), Vec2::default());
        let mut rng = Lcg(7);
        for i in 0..=750 {
            let s = left(0.04 + rng.next() * 0.01, -0.03 + rng.next() * 0.01);
            c.push(&s, i as f32 / HZ);
            if i == 300 {
                c.push(&left(f32::NAN, 0.0), i as f32 / HZ);
                c.push(&left(0.0, f32::INFINITY), i as f32 / HZ);
                c.push(&left(0.0, 0.0), f32::NAN);
            }
        }
        assert_eq!(c.finish(), clean);

        let mut c = Capture::new(CaptureKind::Triggers, Vec2::default());
        c.push(&triggers(f32::NAN, 0.5), 0.0);
        c.push(&triggers(0.0, f32::NEG_INFINITY), 0.0);
        assert_eq!(c.progress(), 0.0);
    }
}
