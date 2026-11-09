#![allow(clippy::field_reassign_with_default)]
use axis_core::stages::deadzone::DeadzoneMode;
use axis_core::*;
use proptest::prelude::*;

fn curve_spec() -> impl Strategy<Value = CurveSpec> {
    prop_oneof![
        Just(CurveSpec::Linear),
        (0.2f32..=5.0).prop_map(|e| CurveSpec::Exponent { e }),
    ]
}

fn mode() -> impl Strategy<Value = DeadzoneMode> {
    prop_oneof![Just(DeadzoneMode::Radial), Just(DeadzoneMode::Axial)]
}

fn deadzone_spec(m: impl Strategy<Value = DeadzoneMode>) -> impl Strategy<Value = DeadzoneSpec> {
    (m, 0.0f32..=0.4, 0.0f32..=0.5, 0.0f32..=1.0).prop_map(|(mode, inner, anti, k)| {
        let lo = inner + 0.05;
        DeadzoneSpec {
            mode,
            inner,
            outer: (lo + (1.0 - lo) * k).min(1.0),
            anti,
        }
    })
}

fn stick(m: impl Strategy<Value = DeadzoneMode>) -> impl Strategy<Value = StickSettings> {
    (deadzone_spec(m), curve_spec()).prop_map(|(deadzone, curve)| StickSettings { deadzone, curve })
}

fn trigger_settings() -> impl Strategy<Value = TriggerSettings> {
    (
        0.0f32..=0.4,
        curve_spec(),
        prop_oneof![Just(None), (0.05f32..=1.0).prop_map(Some)],
    )
        .prop_map(|(inner, curve, hair_trigger)| TriggerSettings {
            inner,
            curve,
            hair_trigger,
        })
}

fn valid_profile() -> impl Strategy<Value = Profile> {
    (
        stick(Just(DeadzoneMode::Radial)),
        stick(mode()),
        trigger_settings(),
        trigger_settings(),
    )
        .prop_map(|(left_stick, right_stick, l2, r2)| Profile {
            left_stick,
            right_stick,
            l2,
            r2,
            ..Profile::default()
        })
}

fn stick_correction() -> impl Strategy<Value = StickCorrection> {
    (
        -0.2f32..=0.2,
        -0.2f32..=0.2,
        proptest::array::uniform16(0.5f32..=1.2),
        0.0f32..=0.3,
    )
        .prop_map(|(cx, cy, outer_radius, drift_radius)| StickCorrection {
            center: [cx, cy],
            outer_radius,
            drift_radius,
        })
}

fn trigger_calibration() -> impl Strategy<Value = TriggerCalibration> {
    (0.0f32..=0.4, 0.6f32..=1.0).prop_map(|(rest, max)| TriggerCalibration { rest, max })
}

fn valid_corrections() -> impl Strategy<Value = Corrections> {
    (
        stick_correction(),
        stick_correction(),
        trigger_calibration(),
        trigger_calibration(),
    )
        .prop_map(|(left, right, l2, r2)| Corrections {
            left,
            right,
            l2,
            r2,
        })
}

fn any_input() -> impl Strategy<Value = InputState> {
    (
        -1.5f32..=1.5,
        -1.5f32..=1.5,
        -1.5f32..=1.5,
        -1.5f32..=1.5,
        -0.5f32..=1.5,
        -0.5f32..=1.5,
        any::<u16>(),
    )
        .prop_map(|(lx, ly, rx, ry, l2, r2, bits)| InputState {
            left: Vec2 { x: lx, y: ly },
            right: Vec2 { x: rx, y: ry },
            l2,
            r2,
            buttons: Buttons(bits as u32),
        })
}

proptest! {
    #[test]
    fn axes_and_triggers_stay_in_range(p in valid_profile(), c in valid_corrections(), i in any_input()) {
        let o = Pipeline::compile(&c, &p).unwrap().process(&i);
        for v in [o.left.x, o.left.y, o.right.x, o.right.y] {
            prop_assert!((-1.0..=1.0).contains(&v));
        }
        for t in [o.l2, o.r2] {
            prop_assert!((0.0..=1.0).contains(&t));
        }
    }

    #[test]
    fn deadzone_and_curve_never_flip_direction(p in valid_profile(), i in any_input()) {
        let o = Pipeline::compile(&Corrections::default(), &p).unwrap().process(&i);
        for (a, b) in [
            (i.left.x, o.left.x),
            (i.left.y, o.left.y),
            (i.right.x, o.right.x),
            (i.right.y, o.right.y),
        ] {
            prop_assert!(b == 0.0 || a.signum() == b.signum());
        }
    }

    #[test]
    fn inside_inner_is_exactly_zero(p in valid_profile(), x in -1.0f32..1.0, y in -1.0f32..1.0) {
        let r = p.left_stick.deadzone.inner * 0.99;
        let len = (x * x + y * y).sqrt().max(1e-6);
        let mut i = InputState::default();
        i.left = Vec2 { x: x / len * r, y: y / len * r };
        let o = Pipeline::compile(&Corrections::default(), &p).unwrap().process(&i);
        prop_assert_eq!(o.left, Vec2::default());
        prop_assert!(o.left.x.to_bits() == 0 && o.left.y.to_bits() == 0);
    }

    #[test]
    fn deterministic(p in valid_profile(), c in valid_corrections(), i in any_input()) {
        let pl = Pipeline::compile(&c, &p).unwrap();
        prop_assert_eq!(pl.process(&i), pl.process(&i));
    }
}
