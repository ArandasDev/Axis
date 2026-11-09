//! SPIKE (Fase 0, c) — descartável. Pergunta: o Slint reproduz o palco da
//! "Visão geral" do design (ilustração em linha, grade, chamadas, animação) e
//! quanto custa em RAM e tempo de abertura?

use std::time::{Duration, Instant};

slint::include_modules!();

fn main() {
    let t0 = Instant::now();
    let ui = AppWindow::new().expect("criar janela");
    let created = t0.elapsed();

    // Entrada simulada a ~60 Hz.
    let weak = ui.as_weak();
    let start = Instant::now();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let t = start.elapsed().as_secs_f32();
        if let Some(ui) = weak.upgrade() {
            ui.set_lx((t * 1.3).cos() * 0.8);
            ui.set_ly((t * 1.3).sin() * 0.62);
            ui.set_rx((t * 0.7).sin() * 0.64);
            ui.set_ry((t * 1.9).cos() * 0.22);
            ui.set_l2(((t * 0.9).sin() * 0.5 + 0.5).clamp(0.0, 1.0));
            ui.set_r2(((t * 0.6).cos() * 0.5 + 0.5).clamp(0.0, 1.0));
            ui.set_cross((t * 2.0).sin() > 0.6);
        }
    });

    // Primeiro quadro: o timer de 0 ms só dispara depois do loop de eventos começar.
    slint::Timer::single_shot(Duration::ZERO, move || {
        println!(
            "SPIKE startup: janela criada em {:?}, loop de eventos em {:?}",
            created,
            t0.elapsed()
        );
    });

    // Para medição automatizada: AXIS_SPIKE_EXIT_AFTER=<segundos>.
    if let Some(secs) = std::env::var("AXIS_SPIKE_EXIT_AFTER")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        slint::Timer::single_shot(Duration::from_secs(secs), || {
            let _ = slint::quit_event_loop();
        });
    }

    ui.run().expect("loop de eventos");
}
