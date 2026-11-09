//! Thread trabalhadora por controle: leitura bloqueante, pipeline e virtual.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axis_core::{InputState, Pipeline};
use axis_devices::{Controller, DeviceError, DeviceStatus, OutputState};
use axis_virtual::{Rumble, VirtualPad};

pub type SharedPad = Arc<Mutex<Box<dyn VirtualPad>>>;

pub enum WorkerCommand {
    SetPipeline(Box<Pipeline>),
    SetLight {
        lightbar: [u8; 3],
        player: Option<u8>,
    },
    Rumble(Rumble),
    SetPaused(bool),
    Shutdown,
}

#[derive(Debug, PartialEq)]
pub enum WorkerExit {
    Disconnected,
    Error(String),
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorkerStatus {
    pub status: DeviceStatus,
    pub reports: u64,
}

const READ_TIMEOUT_MS: i32 = 50;
const STATUS_EVERY: u64 = 64;
const WARN_INTERVAL: Duration = Duration::from_secs(1);
const LATENCY_LOG_INTERVAL: Duration = Duration::from_secs(60);
const BUCKETS: usize = 101;

/// Histograma de latência em buckets de 10 µs; o último acumula >= 1000 µs.
pub(crate) struct LatencyHistogram {
    buckets: [u32; BUCKETS],
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; BUCKETS],
        }
    }
}

impl LatencyHistogram {
    pub(crate) fn record(&mut self, us: u64) {
        let i = ((us / 10) as usize).min(BUCKETS - 1);
        self.buckets[i] = self.buckets[i].saturating_add(1);
    }

    fn total(&self) -> u64 {
        self.buckets.iter().map(|&c| u64::from(c)).sum()
    }

    /// Limite superior do bucket onde a contagem acumulada atinge `p` (0 se vazio).
    pub(crate) fn percentile(&self, p: f64) -> u64 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let target = ((p * total as f64).ceil() as u64).clamp(1, total);
        let mut acc = 0u64;
        for (i, &c) in self.buckets.iter().enumerate() {
            acc += u64::from(c);
            if acc >= target {
                return 10 * (i as u64 + 1);
            }
        }
        10 * BUCKETS as u64
    }

    /// Percentil para o log: no bucket de estouro, "≥1000" em vez do limite artificial.
    pub(crate) fn format_percentile(&self, p: f64) -> String {
        let v = self.percentile(p);
        if v > 10 * (BUCKETS as u64 - 1) {
            "≥1000".to_string()
        } else {
            v.to_string()
        }
    }

    pub(crate) fn reset(&mut self) {
        self.buckets = [0; BUCKETS];
    }
}

/// Verdadeiro no máximo uma vez por `WARN_INTERVAL`.
fn rate_limited_ok(last: &mut Option<Instant>) -> bool {
    let now = Instant::now();
    if last.is_none_or(|t| now - t >= WARN_INTERVAL) {
        *last = Some(now);
        true
    } else {
        false
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Worker {
    controller: Box<dyn Controller>,
    pad: Option<SharedPad>,
    pipeline: Pipeline,
    status: Arc<Mutex<WorkerStatus>>,
    out: OutputState,
    written: OutputState,
    paused: bool,
    reports: u64,
    last_status: DeviceStatus,
    last_warn: Option<Instant>,
    last_out_warn: Option<Instant>,
    latency: LatencyHistogram,
    latency_since: Instant,
}

impl Worker {
    fn send_pad(&mut self, input: &InputState) {
        let Some(pad) = &self.pad else { return };
        let result = lock(pad).update(input);
        if let Err(e) = result {
            if rate_limited_ok(&mut self.last_warn) {
                log::warn!("falha ao atualizar o controle virtual: {e}");
            }
        }
    }

    /// Devolve `Some(saída)` quando o laço deve terminar.
    fn drain_commands(&mut self, rx: &Receiver<WorkerCommand>) -> Option<WorkerExit> {
        loop {
            match rx.try_recv() {
                Ok(WorkerCommand::SetPipeline(p)) => self.pipeline = *p,
                Ok(WorkerCommand::SetLight { lightbar, player }) => {
                    self.out.lightbar = lightbar;
                    self.out.player = player;
                }
                Ok(WorkerCommand::Rumble(r)) => {
                    self.out.rumble_strong = r.strong;
                    self.out.rumble_weak = r.weak;
                }
                Ok(WorkerCommand::SetPaused(p)) => {
                    if p && !self.paused {
                        self.send_pad(&InputState::default());
                    }
                    self.paused = p;
                }
                Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    return Some(WorkerExit::Shutdown);
                }
                Err(TryRecvError::Empty) => return None,
            }
        }
    }

    fn flush_output(&mut self) -> Option<WorkerExit> {
        if self.out == self.written {
            return None;
        }
        match self.controller.write_output(&self.out) {
            Ok(()) => {
                self.written = self.out;
                None
            }
            Err(DeviceError::Disconnected) => Some(WorkerExit::Disconnected),
            Err(e) => {
                self.written = self.out;
                if rate_limited_ok(&mut self.last_out_warn) {
                    log::warn!("falha ao escrever a saída do controle: {e}");
                }
                None
            }
        }
    }

    fn publish_status(&self) {
        *lock(&self.status) = WorkerStatus {
            status: self.last_status,
            reports: self.reports,
        };
    }

    fn run(&mut self, rx: &Receiver<WorkerCommand>) -> WorkerExit {
        loop {
            if let Some(exit) = self.drain_commands(rx) {
                return exit;
            }
            if let Some(exit) = self.flush_output() {
                return exit;
            }
            match self.controller.read(READ_TIMEOUT_MS) {
                Ok(Some(report)) => {
                    let started = Instant::now();
                    self.reports += 1;
                    self.last_status = report.status;
                    if !self.paused && self.pad.is_some() {
                        let out = self.pipeline.process(&report.input);
                        self.send_pad(&out);
                        self.latency.record(started.elapsed().as_micros() as u64);
                    }
                    if self.reports % STATUS_EVERY == 0 {
                        self.publish_status();
                    }
                    if started - self.latency_since >= LATENCY_LOG_INTERVAL {
                        log::info!(
                            "latência p50 {} µs, p99 {} µs, {} reports",
                            self.latency.format_percentile(0.5),
                            self.latency.format_percentile(0.99),
                            self.latency.total()
                        );
                        self.latency.reset();
                        self.latency_since = started;
                    }
                }
                Ok(None) => {}
                Err(e) => return exit_for(e),
            }
        }
    }
}

fn exit_for(e: DeviceError) -> WorkerExit {
    match e {
        DeviceError::Disconnected => WorkerExit::Disconnected,
        DeviceError::Hid(m) => WorkerExit::Error(m),
        other => WorkerExit::Error(other.to_string()),
    }
}

/// Inicia a thread do controle. `pad = None` é o modo visualização (sem virtual).
pub fn spawn_worker(
    controller: Box<dyn Controller>,
    pad: Option<SharedPad>,
    pipeline: Pipeline,
    status: Arc<Mutex<WorkerStatus>>,
    on_exit: impl FnOnce(WorkerExit) + Send + 'static,
) -> (Sender<WorkerCommand>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut w = Worker {
            controller,
            pad,
            pipeline,
            status,
            out: OutputState::default(),
            written: OutputState::default(),
            paused: false,
            reports: 0,
            last_status: DeviceStatus::default(),
            last_warn: None,
            last_out_warn: None,
            latency: LatencyHistogram::default(),
            latency_since: Instant::now(),
        };
        let exit = w.run(&rx);
        w.publish_status();
        w.send_pad(&InputState::default());
        on_exit(exit);
    });
    (tx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{FakeController, FakePad, ReadQueue};
    use axis_core::{Corrections, InputState, Pipeline, Profile, Vec2};
    use axis_devices::{DeviceError, OutputState, Report};
    use axis_virtual::{Rumble, VirtualPad};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    fn report_x(x: f32) -> Result<Option<Report>, DeviceError> {
        let mut r = Report::default();
        r.input.left = Vec2 { x, y: 0.0 };
        Ok(Some(r))
    }

    fn default_pipeline() -> Pipeline {
        Pipeline::compile(&Corrections::default(), &Profile::default()).unwrap()
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..400 {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("condição não atingida");
    }

    struct Harness {
        tx: Sender<WorkerCommand>,
        handle: JoinHandle<()>,
        received: Arc<Mutex<Vec<InputState>>>,
        writes: Arc<Mutex<Vec<OutputState>>>,
        reads: ReadQueue,
        status: Arc<Mutex<WorkerStatus>>,
        exit: Arc<Mutex<Option<WorkerExit>>>,
    }

    fn start(script: Vec<Result<Option<Report>, DeviceError>>, with_pad: bool) -> Harness {
        start_with(FakeController::new(script), with_pad)
    }

    fn start_with(ctl: FakeController, with_pad: bool) -> Harness {
        let writes = ctl.writes.clone();
        let reads = ctl.reads.clone();
        let (pad, received) = FakePad::new();
        let shared: Option<SharedPad> = if with_pad {
            Some(Arc::new(Mutex::new(Box::new(pad) as Box<dyn VirtualPad>)))
        } else {
            None
        };
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        let exit = Arc::new(Mutex::new(None));
        let exit2 = exit.clone();
        let (tx, handle) = spawn_worker(
            Box::new(ctl),
            shared,
            default_pipeline(),
            status.clone(),
            move |e| {
                let mut slot = exit2.lock().unwrap();
                assert!(slot.is_none(), "on_exit chamado mais de uma vez");
                *slot = Some(e);
            },
        );
        Harness {
            tx,
            handle,
            received,
            writes,
            reads,
            status,
            exit,
        }
    }

    impl Harness {
        fn finish(self) -> Option<WorkerExit> {
            let _ = self.tx.send(WorkerCommand::Shutdown);
            self.handle.join().unwrap();
            self.exit.lock().unwrap().take()
        }
    }

    #[test]
    fn reports_go_through_pipeline_to_the_pad() {
        let h = start(vec![report_x(0.03), report_x(1.0)], true);
        wait_until(|| h.received.lock().unwrap().len() == 2);
        {
            let got = h.received.lock().unwrap();
            assert_eq!(got[0].left.x, 0.0);
            assert!((got[1].left.x - 1.0).abs() < 1e-3);
        }
        assert_eq!(h.finish(), Some(WorkerExit::Shutdown));
    }

    #[test]
    fn disconnect_ends_the_worker_with_disconnected() {
        let h = start(vec![report_x(1.0), Err(DeviceError::Disconnected)], true);
        h.handle.join().unwrap();
        assert_eq!(*h.exit.lock().unwrap(), Some(WorkerExit::Disconnected));
        let got = h.received.lock().unwrap();
        assert!((got[0].left.x - 1.0).abs() < 1e-3);
        assert_eq!(*got.last().unwrap(), InputState::default());
    }

    #[test]
    fn output_write_failure_does_not_stop_input() {
        let mut ctl = FakeController::new(vec![]);
        ctl.fail_writes = true;
        let h = start_with(ctl, true);
        h.tx.send(WorkerCommand::SetLight {
            lightbar: [1, 2, 3],
            player: None,
        })
        .unwrap();
        wait_until(|| !h.writes.lock().unwrap().is_empty());
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.received.lock().unwrap().len() == 1);
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.received.lock().unwrap().len() == 2);
        assert_eq!(h.writes.lock().unwrap().len(), 1);
        assert_eq!(h.finish(), Some(WorkerExit::Shutdown));
    }

    #[test]
    fn hid_error_ends_with_error() {
        let h = start(vec![Err(DeviceError::Hid("x".to_string()))], true);
        h.handle.join().unwrap();
        assert_eq!(
            *h.exit.lock().unwrap(),
            Some(WorkerExit::Error("x".to_string()))
        );
    }

    #[test]
    fn pause_sends_neutral_once_and_stops_forwarding() {
        let h = start(vec![report_x(1.0)], true);
        wait_until(|| h.received.lock().unwrap().len() == 1);
        h.tx.send(WorkerCommand::SetPaused(true)).unwrap();
        wait_until(|| h.received.lock().unwrap().len() == 2);
        h.tx.send(WorkerCommand::SetPaused(true)).unwrap();
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.reads.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(h.received.lock().unwrap().len(), 2);
        assert_eq!(h.received.lock().unwrap()[1], InputState::default());
        h.tx.send(WorkerCommand::SetPaused(false)).unwrap();
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.received.lock().unwrap().len() == 3);
        assert!((h.received.lock().unwrap()[2].left.x - 1.0).abs() < 1e-3);
        h.finish();
    }

    #[test]
    fn light_and_rumble_write_output_only_on_change() {
        let h = start(vec![], true);
        for _ in 0..2 {
            h.tx.send(WorkerCommand::SetLight {
                lightbar: [1, 2, 3],
                player: Some(2),
            })
            .unwrap();
        }
        wait_until(|| !h.writes.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(h.writes.lock().unwrap().len(), 1);
        h.tx.send(WorkerCommand::Rumble(Rumble {
            strong: 200,
            weak: 50,
        }))
        .unwrap();
        wait_until(|| h.writes.lock().unwrap().len() == 2);
        let w = h.writes.lock().unwrap().clone();
        assert_eq!(w[0].lightbar, [1, 2, 3]);
        assert_eq!(w[0].player, Some(2));
        assert_eq!((w[1].rumble_strong, w[1].rumble_weak), (200, 50));
        assert_eq!(w[1].lightbar, [1, 2, 3]);
        h.finish();
    }

    #[test]
    fn pipeline_swap_takes_effect() {
        let h = start(vec![], true);
        let mut p = Profile::default();
        p.left_stick.deadzone.inner = 0.5;
        let pl = Pipeline::compile(&Corrections::default(), &p).unwrap();
        h.tx.send(WorkerCommand::SetPipeline(Box::new(pl))).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        h.reads.lock().unwrap().push_back(report_x(0.3));
        wait_until(|| h.received.lock().unwrap().len() == 1);
        assert_eq!(h.received.lock().unwrap()[0].left.x, 0.0);
        h.finish();
    }

    #[test]
    fn view_only_mode_reads_without_pad() {
        let script = (0..64).map(|_| report_x(0.5)).collect();
        let h = start(script, false);
        wait_until(|| h.status.lock().unwrap().reports >= 64);
        assert!(h.received.lock().unwrap().is_empty());
        h.finish();
    }

    #[test]
    fn latency_histogram_percentiles() {
        let mut h = LatencyHistogram::default();
        assert_eq!(h.percentile(0.5), 0);
        for us in [5, 15, 15, 2000] {
            h.record(us);
        }
        assert_eq!(h.percentile(0.5), 20);
        assert!(h.percentile(0.99) >= 1000);
        h.reset();
        assert_eq!(h.percentile(0.5), 0);
    }

    #[test]
    fn overflow_percentile_is_formatted_as_at_least_1000() {
        let mut h = LatencyHistogram::default();
        for _ in 0..10 {
            h.record(2000);
        }
        assert_eq!(h.format_percentile(0.99), "≥1000");
        h.reset();
        h.record(15);
        assert_eq!(h.format_percentile(0.5), "20");
    }
}
