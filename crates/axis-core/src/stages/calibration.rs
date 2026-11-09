use crate::input::Vec2;
use std::f32::consts::TAU;

pub const SECTORS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalibrationParams {
    pub center: Vec2,
    pub outer_radius: [f32; SECTORS],
}

impl Default for CalibrationParams {
    fn default() -> Self {
        Self {
            center: Vec2 { x: 0.0, y: 0.0 },
            outer_radius: [1.0; SECTORS],
        }
    }
}

pub fn calibrate(raw: Vec2, p: &CalibrationParams) -> Vec2 {
    let v = Vec2 {
        x: raw.x.clamp(-1.0, 1.0) - p.center.x,
        y: raw.y.clamp(-1.0, 1.0) - p.center.y,
    };
    if v.length() == 0.0 {
        return Vec2 { x: 0.0, y: 0.0 };
    }

    let mut a = v.y.atan2(v.x);
    if a < 0.0 {
        a += TAU;
    }
    let s = a / (TAU / SECTORS as f32);
    let floor = s.floor();
    let i0 = floor as usize % SECTORS;
    let i1 = (i0 + 1) % SECTORS;
    let t = s - floor;
    let r = p.outer_radius[i0] + (p.outer_radius[i1] - p.outer_radius[i0]) * t;

    let v = v.scale(1.0 / r);
    let len = v.length();
    if len > 1.0 { v.scale(1.0 / len) } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Vec2;

    #[test]
    fn default_params_are_identity_inside_unit_circle() {
        let p = CalibrationParams::default();
        let v = Vec2 { x: 0.3, y: -0.4 };
        assert_eq!(calibrate(v, &p), v);
    }

    #[test]
    fn center_offset_is_removed() {
        let p = CalibrationParams {
            center: Vec2 { x: 0.05, y: -0.03 },
            ..Default::default()
        };
        assert_eq!(calibrate(Vec2 { x: 0.05, y: -0.03 }, &p), Vec2::default());
    }

    #[test]
    fn short_direction_reaches_full_after_calibration() {
        let mut p = CalibrationParams::default();
        p.outer_radius[8] = 0.8; // setor 8 = 180° (esquerda)
        let out = calibrate(Vec2 { x: -0.8, y: 0.0 }, &p);
        assert!((out.x + 1.0).abs() < 1e-5 && out.y.abs() < 1e-5);
    }

    #[test]
    fn angle_wraps_between_last_and_first_sector() {
        let mut p = CalibrationParams::default();
        p.outer_radius[15] = 0.5;
        p.outer_radius[0] = 0.5;
        // ângulo ligeiramente abaixo de 0 (≈ 359°), entre os setores 15 e 0
        let out = calibrate(Vec2 { x: 0.5, y: -0.001 }, &p);
        assert!((out.length() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn out_of_range_raw_is_clamped_and_output_never_exceeds_unit_length() {
        let out = calibrate(Vec2 { x: 3.0, y: 3.0 }, &CalibrationParams::default());
        assert!(out.length() <= 1.0 + 1e-6);
    }
}
