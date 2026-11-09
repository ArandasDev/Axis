use crate::input::Vec2;

/// Radial drift cut: magnitudes `<= radius` become zero; the rest is
/// rescaled from `[radius, 1]` to `[0, 1]`, keeping direction.
pub fn drift(v: Vec2, radius: f32) -> Vec2 {
    let m = v.length();
    if m <= radius || m == 0.0 {
        return Vec2::default();
    }
    let rescaled = ((m - radius) / (1.0 - radius)).min(1.0);
    v.scale(rescaled / m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Vec2;

    #[test]
    fn drift_zeroes_inside_radius_and_rescales_outside() {
        assert_eq!(drift(Vec2 { x: 0.05, y: 0.0 }, 0.08), Vec2::default());
        let out = drift(Vec2 { x: 0.54, y: 0.0 }, 0.08);
        assert!((out.x - 0.5).abs() < 1e-5); // (0.54 - 0.08) / 0.92
        assert!((drift(Vec2 { x: 0.0, y: 1.0 }, 0.08).y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn drift_never_exceeds_unit_length() {
        let out = drift(Vec2 { x: 1.0, y: 1.0 }, 0.08);
        assert!(out.length() <= 1.0 + 1e-6);
    }
}
