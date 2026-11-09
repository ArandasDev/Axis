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
