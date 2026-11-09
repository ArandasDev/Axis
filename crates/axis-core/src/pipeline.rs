use crate::error::ValidationError;
use crate::input::{InputState, Vec2};
use crate::settings::{
    Corrections, CurveSpec, DeadzoneSpec, Profile, StickCorrection, TriggerCalibration,
    TriggerSettings,
};
use crate::stages::anti::anti_deadzone;
use crate::stages::calibration::{CalibrationParams, calibrate};
use crate::stages::curve::{CurveParams, MAX_CURVE_POINTS, curve_stick};
use crate::stages::deadzone::{DeadzoneParams, deadzone};
use crate::stages::drift::drift;
use crate::stages::remap::{RemapParams, remap};
use crate::stages::trigger::{TriggerParams, trigger};

#[derive(Clone, Debug, PartialEq)]
struct StickPipeline {
    calibration: CalibrationParams,
    drift: f32,
    deadzone: DeadzoneParams,
    curve: CurveParams,
    anti: f32,
}

/// Compiled, allocation-free per-packet adjustment pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline {
    left: StickPipeline,
    right: StickPipeline,
    l2: TriggerParams,
    r2: TriggerParams,
    remap: RemapParams,
}

/// Stick value after each stage.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StickTrace {
    pub raw: Vec2,
    pub calibrated: Vec2,
    pub drift: Vec2,
    pub deadzone: Vec2,
    pub curve: Vec2,
    pub anti: Vec2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Trace {
    pub left: StickTrace,
    pub right: StickTrace,
}

fn compile_curve(spec: &CurveSpec) -> CurveParams {
    match spec {
        CurveSpec::Linear => CurveParams::Linear,
        CurveSpec::Exponent { e } => CurveParams::Exponent(*e),
        CurveSpec::Points { points } => {
            let mut pts = [(0.0_f32, 0.0_f32); MAX_CURVE_POINTS];
            for (slot, p) in pts.iter_mut().zip(points.iter()) {
                *slot = (p[0], p[1]);
            }
            CurveParams::Points {
                pts,
                len: points.len().min(MAX_CURVE_POINTS) as u8,
            }
        }
    }
}

fn compile_stick(c: &StickCorrection, d: &DeadzoneSpec, curve: &CurveSpec) -> StickPipeline {
    StickPipeline {
        calibration: CalibrationParams {
            center: Vec2 {
                x: c.center[0],
                y: c.center[1],
            },
            outer_radius: c.outer_radius,
        },
        drift: c.drift_radius,
        deadzone: DeadzoneParams {
            mode: d.mode,
            inner: d.inner,
            outer: d.outer,
        },
        curve: compile_curve(curve),
        anti: d.anti,
    }
}

fn compile_trigger(c: &TriggerCalibration, s: &TriggerSettings) -> TriggerParams {
    TriggerParams {
        rest: c.rest,
        max: c.max,
        inner: s.inner,
        curve: compile_curve(&s.curve),
        hair: s.hair_trigger,
    }
}

fn finite_or_zero(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn sanitize(v: Vec2) -> Vec2 {
    Vec2 {
        x: finite_or_zero(v.x),
        y: finite_or_zero(v.y),
    }
}

impl StickPipeline {
    fn run(&self, raw: Vec2, trace: Option<&mut StickTrace>) -> Vec2 {
        let calibrated = calibrate(raw, &self.calibration);
        let drifted = drift(calibrated, self.drift);
        let dead = deadzone(drifted, &self.deadzone);
        let curved = curve_stick(dead, &self.curve);
        let out = anti_deadzone(curved, self.anti);
        if let Some(t) = trace {
            *t = StickTrace {
                raw,
                calibrated,
                drift: drifted,
                deadzone: dead,
                curve: curved,
                anti: out,
            };
        }
        out
    }
}

impl Pipeline {
    pub fn compile(c: &Corrections, p: &Profile) -> Result<Pipeline, ValidationError> {
        c.validate()?;
        p.validate()?;
        let mut remap_params = RemapParams::default();
        for (src, dst) in &p.button_map {
            remap_params.target[*src as usize] = Some(*dst);
        }
        for b in &p.disabled_buttons {
            remap_params.target[*b as usize] = None;
        }
        Ok(Pipeline {
            left: compile_stick(&c.left, &p.left_stick.deadzone, &p.left_stick.curve),
            right: compile_stick(&c.right, &p.right_stick.deadzone, &p.right_stick.curve),
            l2: compile_trigger(&c.l2, &p.l2),
            r2: compile_trigger(&c.r2, &p.r2),
            remap: remap_params,
        })
    }

    pub fn process(&self, input: &InputState) -> InputState {
        self.run(input, None)
    }

    pub fn process_traced(&self, input: &InputState) -> (InputState, Trace) {
        let mut trace = Trace::default();
        let out = self.run(input, Some(&mut trace));
        (out, trace)
    }

    fn run(&self, input: &InputState, trace: Option<&mut Trace>) -> InputState {
        let left = sanitize(input.left);
        let right = sanitize(input.right);
        let (lt, rt) = match trace {
            Some(t) => (Some(&mut t.left), Some(&mut t.right)),
            None => (None, None),
        };
        InputState {
            left: self.left.run(left, lt),
            right: self.right.run(right, rt),
            l2: trigger(finite_or_zero(input.l2), &self.l2),
            r2: trigger(finite_or_zero(input.r2), &self.r2),
            buttons: remap(input.buttons, &self.remap),
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::input::Button;

    #[test]
    fn default_pipeline_passes_through_outside_deadzone() {
        let pl = Pipeline::compile(&Corrections::default(), &Profile::default()).unwrap();
        let mut i = InputState::default();
        i.left = Vec2 { x: 1.0, y: 0.0 };
        i.r2 = 1.0;
        i.buttons.set(Button::South, true);
        let o = pl.process(&i);
        assert!((o.left.x - 1.0).abs() < 1e-6);
        assert_eq!(o.r2, 1.0);
        assert!(o.buttons.is_pressed(Button::South));
    }

    #[test]
    fn compile_rejects_newer_schema_profile() {
        let p = Profile {
            schema_version: 2,
            ..Profile::default()
        };
        assert!(Pipeline::compile(&Corrections::default(), &p).is_err());
    }

    #[test]
    fn compile_rejects_invalid_profile() {
        let mut p = Profile::default();
        p.vibration = 2.0;
        assert!(Pipeline::compile(&Corrections::default(), &p).is_err());
    }

    #[test]
    fn trace_records_every_stage_and_matches_output() {
        let mut c = Corrections::default();
        c.left.drift_radius = 0.1;
        let pl = Pipeline::compile(&c, &Profile::default()).unwrap();
        let mut i = InputState::default();
        i.left = Vec2 { x: 0.6, y: 0.0 };
        let (o, t) = pl.process_traced(&i);
        assert_eq!(t.left.raw, i.left);
        assert!(t.left.drift.x < t.left.calibrated.x);
        assert_eq!(t.left.anti, o.left);
        assert_eq!(pl.process(&i), o);
    }

    #[test]
    fn anti_deadzone_is_the_output_floor_after_the_curve() {
        let mut p = Profile::default();
        p.left_stick.deadzone.inner = 0.05;
        p.left_stick.deadzone.outer = 1.0;
        p.left_stick.deadzone.anti = 0.2;
        p.left_stick.curve = CurveSpec::Exponent { e: 2.0 };
        let pl = Pipeline::compile(&Corrections::default(), &p).unwrap();
        let mut i = InputState::default();
        i.left = Vec2 { x: 0.06, y: 0.0 };
        let (o, t) = pl.process_traced(&i);
        assert!(o.left.x >= 0.2 && o.left.x < 0.21, "got {}", o.left.x);
        assert!(
            t.left.curve.x < 0.01,
            "curve must see no anti: {}",
            t.left.curve.x
        );
    }

    #[test]
    fn non_finite_input_never_reaches_output() {
        let pl = Pipeline::compile(&Corrections::default(), &Profile::default()).unwrap();
        let mut i = InputState::default();
        i.left = Vec2 {
            x: f32::NAN,
            y: f32::INFINITY,
        };
        i.l2 = f32::NAN;
        let o = pl.process(&i);
        assert_eq!(o.left, Vec2::default());
        assert_eq!(o.l2, 0.0);
    }

    #[test]
    fn remap_applies_map_then_disable() {
        let mut p = Profile::default();
        p.button_map.insert(Button::South, Button::East);
        p.disabled_buttons.insert(Button::North);
        let pl = Pipeline::compile(&Corrections::default(), &p).unwrap();
        let mut i = InputState::default();
        i.buttons.set(Button::South, true);
        i.buttons.set(Button::North, true);
        let o = pl.process(&i);
        assert!(o.buttons.is_pressed(Button::East));
        assert!(!o.buttons.is_pressed(Button::South));
        assert!(!o.buttons.is_pressed(Button::North));
    }

    #[test]
    fn curve_points_are_copied_into_fixed_array() {
        let mut p = Profile::default();
        p.l2.curve = CurveSpec::Points {
            points: vec![[0.5, 0.25]],
        };
        let pl = Pipeline::compile(&Corrections::default(), &p).unwrap();
        match pl.l2.curve {
            CurveParams::Points { pts, len } => {
                assert_eq!(len, 1);
                assert_eq!(pts[0], (0.5, 0.25));
            }
            other => panic!("unexpected curve {other:?}"),
        }
    }
}
