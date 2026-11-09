use crate::input::Vec2;

pub const MAX_CURVE_POINTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveParams {
    Linear,
    Exponent(f32),
    /// Only the first `len` entries are meaningful, sorted by strictly
    /// increasing x in (0, 1). (0, 0) and (1, 1) are implicit at the ends.
    Points {
        pts: [(f32, f32); MAX_CURVE_POINTS],
        len: u8,
    },
}

fn lerp_segment(m: f32, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> f32 {
    let span = x1 - x0;
    if span <= 0.0 {
        return y1;
    }
    y0 + (y1 - y0) * ((m - x0) / span)
}

pub fn curve_scalar(m: f32, c: &CurveParams) -> f32 {
    let m = m.clamp(0.0, 1.0);
    let out = match c {
        CurveParams::Linear => m,
        CurveParams::Exponent(e) => m.powf(*e),
        CurveParams::Points { pts, len } => {
            let n = usize::from(*len).min(MAX_CURVE_POINTS);
            let mut prev = (0.0, 0.0);
            let mut result = None;
            for &pt in &pts[..n] {
                if m <= pt.0 {
                    result = Some(lerp_segment(m, prev, pt));
                    break;
                }
                prev = pt;
            }
            result.unwrap_or_else(|| lerp_segment(m, prev, (1.0, 1.0)))
        }
    };
    out.clamp(0.0, 1.0)
}

pub fn curve_stick(v: Vec2, c: &CurveParams) -> Vec2 {
    let len = v.length();
    if len == 0.0 {
        return Vec2 { x: 0.0, y: 0.0 };
    }
    let k = curve_scalar(len, c) / len;
    // `+ 0.0` turns -0.0 into +0.0 (scaling a negative by zero yields -0.0).
    Vec2 {
        x: v.x * k + 0.0,
        y: v.y * k + 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity() {
        assert_eq!(curve_scalar(0.37, &CurveParams::Linear), 0.37);
    }

    #[test]
    fn exponent_softens_low_range() {
        assert!((curve_scalar(0.5, &CurveParams::Exponent(2.0)) - 0.25).abs() < 1e-6);
        assert_eq!(curve_scalar(1.0, &CurveParams::Exponent(2.0)), 1.0);
    }

    #[test]
    fn points_interpolate_with_implicit_ends() {
        let mut pts = [(0.0, 0.0); MAX_CURVE_POINTS];
        pts[0] = (0.5, 0.2);
        let c = CurveParams::Points { pts, len: 1 };
        assert!((curve_scalar(0.25, &c) - 0.1).abs() < 1e-6);
        assert!((curve_scalar(0.75, &c) - 0.6).abs() < 1e-6);
        assert_eq!(curve_scalar(1.0, &c), 1.0);
    }

    #[test]
    fn curve_stick_keeps_direction() {
        let out = curve_stick(Vec2 { x: -0.6, y: 0.8 }, &CurveParams::Exponent(2.0));
        assert!((out.x / out.y + 0.75).abs() < 1e-5);
        assert!(out.x < 0.0 && out.y > 0.0);
    }

    #[test]
    fn zero_vector_gives_positive_zero() {
        let out = curve_stick(Vec2 { x: 0.0, y: -0.0 }, &CurveParams::Exponent(2.0));
        assert!(out.x.is_sign_positive() && out.y.is_sign_positive());
        assert_eq!(out, Vec2 { x: 0.0, y: 0.0 });
    }

    #[test]
    fn zero_result_has_no_negative_zero() {
        let out = curve_stick(
            Vec2 { x: -0.5, y: -0.0 },
            &CurveParams::Exponent(f32::INFINITY),
        );
        assert!(out.x.is_sign_positive() && out.y.is_sign_positive());
    }
}
