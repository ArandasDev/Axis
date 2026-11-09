use axis_core::Button;
use axis_devices::{Recording, parse_frame};

#[test]
fn synthetic_ds4_fixture_replays_within_ranges() {
    let rec = Recording::from_text(include_str!("fixtures/ds4-usb-synthetic.rec")).unwrap();
    assert!(rec.frames.len() >= 4);
    for f in &rec.frames {
        let r = parse_frame(rec.family, &f.data).expect("todo frame do fixture decodifica");
        for v in [
            r.input.left.x,
            r.input.left.y,
            r.input.right.x,
            r.input.right.y,
        ] {
            assert!((-1.0..=1.0).contains(&v));
        }
        for t in [r.input.l2, r.input.r2] {
            assert!((0.0..=1.0).contains(&t));
        }
    }
}

#[test]
fn real_ds4_bt_fixture_covers_extremes() {
    let rec = Recording::from_text(include_str!("fixtures/ds4-bt-real.rec")).unwrap();
    let reports: Vec<_> = rec
        .frames
        .iter()
        .filter_map(|f| parse_frame(rec.family, &f.data))
        .collect();
    assert!(
        reports.len() * 10 >= rec.frames.len() * 9,
        "≥ 90% dos frames decodificam"
    );
    assert!(
        reports.iter().any(|r| r.input.left.x > 0.9)
            && reports.iter().any(|r| r.input.left.x < -0.9)
    );
    assert!(reports.iter().any(|r| r.input.l2 > 0.95) && reports.iter().any(|r| r.input.r2 > 0.95));
    assert!(
        reports
            .iter()
            .any(|r| r.input.buttons.is_pressed(Button::South))
    );
}
