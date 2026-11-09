use serde::{Deserialize, Serialize};

use crate::input::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadzoneMode {
    Radial,
    Axial,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeadzoneParams {
    pub mode: DeadzoneMode,
    pub inner: f32,
    pub outer: f32,
    pub anti: f32,
}

/// Maps a magnitude `m >= 0` through inner/outer deadzone with anti-deadzone.
/// Precondition: `inner < outer` (guaranteed by validation).
pub(crate) fn shape(m: f32, inner: f32, outer: f32, anti: f32) -> f32 {
    if m <= inner {
        return 0.0;
    }
    let t = ((m - inner) / (outer - inner)).clamp(0.0, 1.0);
    anti + (1.0 - anti) * t
}

pub fn deadzone(v: Vec2, p: &DeadzoneParams) -> Vec2 {
    match p.mode {
        DeadzoneMode::Radial => {
            let m = v.length();
            if m == 0.0 {
                return Vec2::default();
            }
            v.scale(shape(m, p.inner, p.outer, p.anti) / m)
        }
        DeadzoneMode::Axial => Vec2 {
            x: shape(v.x.abs(), p.inner, p.outer, p.anti).copysign(v.x),
            y: shape(v.y.abs(), p.inner, p.outer, p.anti).copysign(v.y),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Vec2;

    fn radial(inner: f32, outer: f32, anti: f32) -> DeadzoneParams {
        DeadzoneParams {
            mode: DeadzoneMode::Radial,
            inner,
            outer,
            anti,
        }
    }

    #[test]
    fn inside_inner_is_exactly_zero() {
        assert_eq!(
            deadzone(Vec2 { x: 0.03, y: 0.03 }, &radial(0.05, 1.0, 0.0)),
            Vec2::default()
        );
    }

    #[test]
    fn outer_saturates_to_full() {
        let out = deadzone(Vec2 { x: 0.95, y: 0.0 }, &radial(0.05, 0.9, 0.0));
        assert!((out.x - 1.0).abs() < 1e-6);
    }

    #[test]
    fn anti_deadzone_jumps_to_minimum_just_outside_inner() {
        let out = deadzone(Vec2 { x: 0.0501, y: 0.0 }, &radial(0.05, 1.0, 0.2));
        assert!(out.x >= 0.2 && out.x < 0.21);
    }

    #[test]
    fn axial_preserves_signs_per_axis() {
        let p = DeadzoneParams {
            mode: DeadzoneMode::Axial,
            inner: 0.1,
            outer: 1.0,
            anti: 0.0,
        };
        let out = deadzone(Vec2 { x: -0.55, y: 0.05 }, &p);
        assert!((out.x + 0.5).abs() < 1e-5);
        assert_eq!(out.y, 0.0);
    }
}
