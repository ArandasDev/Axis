use crate::input::Vec2;

/// Anti-deadzone, applied after the response curve: any non-zero magnitude `m`
/// becomes `anti + (1 − anti) · m`, so `anti` is the smallest output the game sees
/// once the stick leaves the inner zone. Direction is kept; zero stays +0.0.
/// Precondition: `0 ≤ anti < 1` (guaranteed by validation).
pub fn anti_deadzone(v: Vec2, anti: f32) -> Vec2 {
    let m = v.length();
    if m == 0.0 {
        return Vec2::default();
    }
    let lifted = anti + (1.0 - anti) * m.min(1.0);
    v.scale(lifted / m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anti_lifts_small_output_to_the_floor() {
        let out = anti_deadzone(Vec2 { x: 0.01, y: 0.0 }, 0.2);
        assert!((out.x - 0.208).abs() < 1e-6);
        assert_eq!(out.y, 0.0);
    }

    #[test]
    fn zero_stays_positive_zero() {
        let out = anti_deadzone(Vec2 { x: -0.0, y: 0.0 }, 0.2);
        assert!(out.x.to_bits() == 0 && out.y.to_bits() == 0);
    }

    #[test]
    fn full_deflection_stays_full_and_keeps_direction() {
        let out = anti_deadzone(Vec2 { x: -0.6, y: 0.8 }, 0.3);
        assert!((out.length() - 1.0).abs() < 1e-6);
        assert!((out.x / out.y + 0.75).abs() < 1e-5);
    }

    #[test]
    fn zero_anti_is_identity() {
        let v = Vec2 { x: 0.3, y: -0.4 };
        assert_eq!(anti_deadzone(v, 0.0), v);
    }
}
