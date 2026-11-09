use super::curve::{CurveParams, curve_scalar};
use super::deadzone::shape;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriggerParams {
    pub rest: f32,
    pub max: f32,
    pub inner: f32,
    pub curve: CurveParams,
    pub hair: Option<f32>,
}

/// Preconditions (guaranteed by validation): `rest < max`, `0 <= inner < 1`.
pub fn trigger(raw: f32, p: &TriggerParams) -> f32 {
    let raw = raw.clamp(0.0, 1.0);
    let t = ((raw - p.rest) / (p.max - p.rest)).clamp(0.0, 1.0);
    let t = shape(t, p.inner, 1.0, 0.0);
    let t = curve_scalar(t, &p.curve);
    match p.hair {
        Some(h) => {
            if t >= h {
                1.0
            } else {
                0.0
            }
        }
        None => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp() -> TriggerParams {
        TriggerParams {
            rest: 0.0,
            max: 1.0,
            inner: 0.0,
            curve: CurveParams::Linear,
            hair: None,
        }
    }

    #[test]
    fn trigger_calibration_maps_rest_and_max() {
        let p = TriggerParams {
            rest: 0.04,
            max: 0.9,
            ..tp()
        };
        assert_eq!(trigger(0.03, &p), 0.0);
        assert_eq!(trigger(0.95, &p), 1.0);
        assert!((trigger(0.47, &p) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn trigger_hair_is_digital() {
        let p = TriggerParams {
            hair: Some(0.1),
            ..tp()
        };
        assert_eq!(trigger(0.09, &p), 0.0);
        assert_eq!(trigger(0.11, &p), 1.0);
    }

    #[test]
    fn trigger_out_of_range_raw_is_clamped() {
        assert_eq!(trigger(1.7, &tp()), 1.0);
        assert_eq!(trigger(-0.2, &tp()), 0.0);
    }
}
