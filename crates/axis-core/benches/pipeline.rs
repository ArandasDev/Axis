use std::hint::black_box;

use axis_core::{
    Button, Buttons, Corrections, CurveSpec, DeadzoneSpec, InputState, Pipeline, Profile,
    StickCorrection, StickSettings, TriggerSettings, Vec2,
};
use criterion::{Criterion, criterion_group, criterion_main};

const INPUT_COUNT: usize = 1024;

/// Deterministic pseudo-random generator (LCG); only used to build the input set.
struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn next_signed(&mut self) -> f32 {
        self.next_unit() * 2.0 - 1.0
    }
}

fn realistic_pipeline() -> Pipeline {
    let corrections = Corrections {
        left: StickCorrection {
            drift_radius: 0.06,
            ..StickCorrection::default()
        },
        right: StickCorrection {
            drift_radius: 0.06,
            ..StickCorrection::default()
        },
        ..Corrections::default()
    };
    let stick = StickSettings {
        deadzone: DeadzoneSpec {
            inner: 0.05,
            outer: 0.95,
            anti: 0.1,
            ..DeadzoneSpec::default()
        },
        curve: CurveSpec::Points {
            points: vec![[0.2, 0.1], [0.5, 0.4], [0.8, 0.8], [0.95, 0.95]],
        },
    };
    let mut profile = Profile {
        left_stick: stick.clone(),
        right_stick: stick,
        r2: TriggerSettings {
            hair_trigger: Some(0.1),
            ..TriggerSettings::default()
        },
        ..Profile::default()
    };
    profile.button_map.insert(Button::South, Button::East);
    profile.button_map.insert(Button::L1, Button::R1);
    Pipeline::compile(&corrections, &profile).expect("benchmark profile must be valid")
}

fn varied_inputs() -> Vec<InputState> {
    let mut rng = Lcg(0x5EED_A515);
    (0..INPUT_COUNT)
        .map(|_| InputState {
            left: Vec2 {
                x: rng.next_signed(),
                y: rng.next_signed(),
            },
            right: Vec2 {
                x: rng.next_signed(),
                y: rng.next_signed(),
            },
            l2: rng.next_unit(),
            r2: rng.next_unit(),
            buttons: Buttons((rng.next_unit() * 65536.0) as u32 & 0xFFFF),
        })
        .collect()
}

fn bench_pipeline(c: &mut Criterion) {
    let pipeline = realistic_pipeline();
    let inputs = varied_inputs();

    c.bench_function("pipeline_process", |b| {
        b.iter(|| {
            for input in &inputs {
                black_box(pipeline.process(black_box(input)));
            }
        })
    });

    c.bench_function("pipeline_process_traced", |b| {
        b.iter(|| {
            for input in &inputs {
                black_box(pipeline.process_traced(black_box(input)));
            }
        })
    });
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
