//! Thread trabalhadora por controle: leitura bloqueante, pipeline e virtual.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axis_core::repair::{Capture, Outcome};
use axis_core::{InputState, Pipeline, Trace};
use axis_devices::{Controller, DeviceError, DeviceStatus, OutputState};
use axis_virtual::{Rumble, VirtualPad};

use crate::live::{CaptureSnapshot, LiveSample, LiveTap};

pub type SharedPad = Arc<Mutex<Box<dyn VirtualPad>>>;

/// Relógio do worker; os testes injetam um tempo simulado.
pub(crate) type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub enum WorkerCommand {
    SetPipeline(Box<Pipeline>),
    SetLight {
        lightbar: [u8; 3],
        player: Option<u8>,
    },
    Rumble(Rumble),
    SetPaused(bool),
    /// Troca (ou solta, com `None`) o controle virtual sem reiniciar o worker.
    SetPad(Option<SharedPad>),
    /// Começa uma captura do kit de correção (o buffer já vem alocado); substitui
    /// qualquer captura ou resultado anterior.
    StartCapture(Box<Capture>),
    /// Solta a captura em andamento e limpa o resultado.
    CancelCapture,
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
/// Sem reports por este tempo = controle sem sinal (o DS4 manda ~200 reports/s parado).
pub const SILENCE_THRESHOLD: Duration = Duration::from_millis(500);
const STATUS_EVERY: u64 = 64;
const WARN_INTERVAL: Duration = Duration::from_secs(1);
const LATENCY_LOG_INTERVAL: Duration = Duration::from_secs(60);
const BUCKETS: usize = 101;
/// Intervalo mínimo entre amostras ao vivo: 17 ms garante no máximo ~59 por
/// segundo (16 ms passaria de 60).
const LIVE_INTERVAL: Duration = Duration::from_millis(17);

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

const JITTER_WINDOW: usize = 256;

/// Anel fixo com os últimos intervalos entre reports, em µs.
pub(crate) struct IntervalRing {
    buf: [u32; JITTER_WINDOW],
    len: usize,
    next: usize,
}

impl Default for IntervalRing {
    fn default() -> Self {
        Self {
            buf: [0; JITTER_WINDOW],
            len: 0,
            next: 0,
        }
    }
}

impl IntervalRing {
    pub(crate) fn record(&mut self, us: u32) {
        self.buf[self.next] = us;
        self.next = (self.next + 1) % JITTER_WINDOW;
        self.len = (self.len + 1).min(JITTER_WINDOW);
    }

    /// p99 de |intervalo − média| (0 se vazio). Sem alocação: ordena uma cópia na pilha.
    pub(crate) fn jitter_p99(&self) -> u32 {
        let n = self.len;
        if n == 0 {
            return 0;
        }
        // Antes de encher, os válidos são `buf[..len]`; depois, o anel inteiro.
        let vals = &self.buf[..n];
        let mean = vals.iter().map(|&v| u64::from(v)).sum::<u64>() as f64 / n as f64;
        let mut dev = [0u32; JITTER_WINDOW];
        for (d, &v) in dev.iter_mut().zip(vals) {
            *d = (f64::from(v) - mean).abs().round() as u32;
        }
        let dev = &mut dev[..n];
        dev.sort_unstable();
        let i = ((n as f64 * 0.99).ceil() as usize).clamp(1, n) - 1;
        dev[i]
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
    /// Última saída escrita; `None` força reescrever (o controle religado perdeu a luz).
    written: Option<OutputState>,
    paused: bool,
    on_link: Box<dyn Fn(bool) + Send>,
    /// Só controles que transmitem sempre (DS4/DualSense) usam o silêncio como sinal.
    detect_silence: bool,
    link_up: bool,
    last_report: Instant,
    reports: u64,
    last_status: DeviceStatus,
    last_warn: Option<Instant>,
    last_out_warn: Option<Instant>,
    latency: LatencyHistogram,
    latency_since: Instant,
    tap: Arc<LiveTap>,
    last_publish: Option<Instant>,
    /// Último report barrado pelo limitador de 17 ms; sai na próxima chance.
    pending: Option<(InputState, InputState, Trace)>,
    live_seq: u64,
    /// p99 da última janela de 60 s do histograma (0 antes da primeira).
    latency_p99_us: u32,
    /// Última amostra publicada, para republicar quando só a captura muda.
    last_live: (InputState, InputState, Trace),
    clock: Clock,
    /// Captura em andamento e o instante em que começou.
    capture: Option<(Box<Capture>, Instant)>,
    /// O que a amostra ao vivo mostra da captura (inclui o resultado final).
    capture_snapshot: Option<CaptureSnapshot>,
    intervals: IntervalRing,
    /// Report anterior para o jitter; `None` no início e após o link cair.
    prev_report: Option<Instant>,
    /// Entrada crua do último report: o XInput só entrega report quando algo muda,
    /// então a captura segue com ela nas leituras ociosas. `None` após o link cair.
    last_input: Option<InputState>,
}

impl Worker {
    fn now(&self) -> Instant {
        (self.clock)()
    }

    fn publish_live(&mut self, input: InputState, output: InputState, trace: Trace) {
        self.live_seq += 1;
        self.last_live = (input, output, trace);
        self.tap.publish(LiveSample {
            input,
            output,
            trace,
            status: self.last_status,
            reports: self.reports,
            latency_p99_us: self.latency_p99_us,
            seq: self.live_seq,
            capture: self.capture_snapshot.clone(),
            jitter_us: self.intervals.jitter_p99(),
        });
    }

    /// A captura mudou por comando: republica a última amostra (respeitando os 17 ms).
    fn capture_changed(&mut self) {
        if self.tap.enabled() && self.pending.is_none() {
            self.pending = Some(self.last_live);
        }
    }

    /// Entrega o report cru à captura; ao terminar, guarda o resultado. A captura
    /// terminada fica guardada (sem efeito) e só é solta no próximo comando, na queda
    /// do link ou na saída — nunca no caminho do report.
    fn feed_capture(&mut self, input: &InputState, now: Instant) {
        let Some((cap, since)) = self.capture.as_mut() else {
            return;
        };
        if cap.is_done() {
            return;
        }
        cap.push(input, now.saturating_duration_since(*since).as_secs_f32());
        let done = cap.is_done();
        let snap = CaptureSnapshot {
            kind: cap.kind(),
            progress: cap.progress(),
            outcome: done.then(|| cap.finish()),
        };
        self.capture_snapshot = Some(snap);
    }

    /// Link caído ou saída com captura ativa: resultado `Aborted` (um resultado já
    /// pronto fica).
    fn abort_capture(&mut self) {
        if self.capture.take().is_some_and(|(c, _)| !c.is_done()) {
            if let Some(s) = &mut self.capture_snapshot {
                s.outcome = Some(Outcome::Aborted);
            }
        }
    }

    /// Amostra neutra (palco não congela), fora do limite de 17 ms; só com o tap ligado.
    fn publish_neutral(&mut self) {
        self.pending = None;
        if self.tap.enabled() {
            self.publish_live(
                InputState::default(),
                InputState::default(),
                Trace::default(),
            );
        }
    }

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
                Ok(WorkerCommand::SetPad(p)) => {
                    self.pad = p;
                    self.stop_rumble();
                }
                Ok(WorkerCommand::StartCapture(c)) => {
                    self.capture_snapshot = Some(CaptureSnapshot {
                        kind: c.kind(),
                        progress: c.progress(),
                        outcome: None,
                    });
                    self.capture = Some((c, self.now()));
                    self.capture_changed();
                }
                Ok(WorkerCommand::CancelCapture) => {
                    self.capture = None;
                    self.capture_snapshot = None;
                    self.capture_changed();
                }
                Ok(WorkerCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    return Some(WorkerExit::Shutdown);
                }
                Err(TryRecvError::Empty) => return None,
            }
        }
    }

    /// Zera a vibração: nenhum pedido antigo deve voltar quando o controle religar.
    fn stop_rumble(&mut self) {
        self.out.rumble_strong = 0;
        self.out.rumble_weak = 0;
    }

    fn flush_output(&mut self) -> Option<WorkerExit> {
        if !self.link_up || self.written == Some(self.out) {
            return None;
        }
        match self.controller.write_output(&self.out) {
            Ok(()) => {
                self.written = Some(self.out);
                None
            }
            Err(DeviceError::Disconnected) => Some(WorkerExit::Disconnected),
            Err(e) => {
                self.written = Some(self.out);
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
                    let started = self.now();
                    self.last_report = started;
                    if !self.link_up {
                        self.link_up = true;
                        (self.on_link)(true);
                    }
                    if let Some(prev) = self.prev_report {
                        let us = started.saturating_duration_since(prev).as_micros();
                        self.intervals.record(us.min(u128::from(u32::MAX)) as u32);
                    }
                    self.prev_report = Some(started);
                    self.reports += 1;
                    self.last_status = report.status;
                    self.last_input = Some(report.input);
                    // Entrada crua: vale pausado, só visualização e sem inspeção.
                    self.feed_capture(&report.input, started);
                    let live = self.tap.enabled();
                    let forward = !self.paused && self.pad.is_some();
                    if live || forward {
                        let (out, trace) = if live {
                            self.pipeline.process_traced(&report.input)
                        } else {
                            (self.pipeline.process(&report.input), Trace::default())
                        };
                        if forward {
                            self.send_pad(&out);
                            let took = self.now().saturating_duration_since(started);
                            self.latency.record(took.as_micros() as u64);
                        }
                        if live {
                            if self
                                .last_publish
                                .is_none_or(|t| started - t >= LIVE_INTERVAL)
                            {
                                self.last_publish = Some(started);
                                self.pending = None;
                                self.publish_live(report.input, out, trace);
                            } else {
                                self.pending = Some((report.input, out, trace));
                            }
                        }
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
                        self.latency_p99_us = self.latency.percentile(0.99) as u32;
                        self.latency.reset();
                        self.latency_since = started;
                    }
                }
                Ok(None) => {
                    // Analógico parado no XInput: sem report novo, o tempo da captura anda.
                    // No HID (`detect_silence`), `Ok(None)` é o controle calando: o último
                    // report não vale como leitura nova.
                    if !self.detect_silence
                        && self.link_up
                        && self.capture.as_ref().is_some_and(|(c, _)| !c.is_done())
                    {
                        if let Some(input) = self.last_input {
                            let now = self.now();
                            self.feed_capture(&input, now);
                            self.capture_changed();
                        }
                    }
                    if let Some((input, out, trace)) = self.pending {
                        let now = self.now();
                        if self.last_publish.is_none_or(|t| now - t >= LIVE_INTERVAL) {
                            self.pending = None;
                            self.last_publish = Some(now);
                            if self.tap.enabled() {
                                self.publish_live(input, out, trace);
                            }
                        }
                    }
                    if self.detect_silence
                        && self.link_up
                        && self.now().saturating_duration_since(self.last_report)
                            >= SILENCE_THRESHOLD
                    {
                        self.link_up = false;
                        self.written = None;
                        self.prev_report = None;
                        self.last_input = None;
                        self.stop_rumble();
                        self.send_pad(&InputState::default());
                        // O neutro já leva o `Aborted`: é a amostra que fica.
                        self.abort_capture();
                        self.publish_neutral();
                        (self.on_link)(false);
                    }
                }
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
/// `detect_silence` liga a detecção de silêncio (só para quem transmite sempre).
/// `on_link` é chamado na própria thread (não pode bloquear): `false` após
/// `SILENCE_THRESHOLD` sem reports, `true` quando os reports voltam.
#[allow(clippy::too_many_arguments)]
pub fn spawn_worker(
    controller: Box<dyn Controller>,
    pad: Option<SharedPad>,
    pipeline: Pipeline,
    status: Arc<Mutex<WorkerStatus>>,
    detect_silence: bool,
    on_exit: impl FnOnce(WorkerExit) + Send + 'static,
    on_link: impl Fn(bool) + Send + 'static,
    tap: Arc<LiveTap>,
) -> (Sender<WorkerCommand>, JoinHandle<()>) {
    spawn_clocked(
        controller,
        pad,
        pipeline,
        status,
        detect_silence,
        on_exit,
        on_link,
        tap,
        Arc::new(Instant::now),
    )
}

/// `spawn_worker` com relógio injetável (os testes simulam o tempo).
#[allow(clippy::too_many_arguments)]
fn spawn_clocked(
    controller: Box<dyn Controller>,
    pad: Option<SharedPad>,
    pipeline: Pipeline,
    status: Arc<Mutex<WorkerStatus>>,
    detect_silence: bool,
    on_exit: impl FnOnce(WorkerExit) + Send + 'static,
    on_link: impl Fn(bool) + Send + 'static,
    tap: Arc<LiveTap>,
    clock: Clock,
) -> (Sender<WorkerCommand>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let now = clock();
        let mut w = Worker {
            controller,
            pad,
            pipeline,
            status,
            out: OutputState::default(),
            written: Some(OutputState::default()),
            paused: false,
            on_link: Box::new(on_link),
            detect_silence,
            link_up: true,
            last_report: now,
            reports: 0,
            last_status: DeviceStatus::default(),
            last_warn: None,
            last_out_warn: None,
            latency: LatencyHistogram::default(),
            latency_since: now,
            tap,
            last_publish: None,
            pending: None,
            live_seq: 0,
            latency_p99_us: 0,
            last_live: Default::default(),
            clock,
            capture: None,
            capture_snapshot: None,
            intervals: IntervalRing::default(),
            prev_report: None,
            last_input: None,
        };
        let exit = w.run(&rx);
        w.abort_capture();
        w.publish_status();
        w.publish_neutral();
        w.send_pad(&InputState::default());
        on_exit(exit);
    });
    (tx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{CaptureSnapshot, LiveTap};
    use crate::testkit::{FakeController, FakePad, ReadQueue};
    use axis_core::repair::{Capture, CaptureKind, Outcome, Stick};
    use axis_core::{Corrections, InputState, Pipeline, Profile, Trace, Vec2};
    use axis_devices::{Controller, DeviceError, OutputState, Report};
    use axis_virtual::{Rumble, VirtualPad};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;
    use std::time::Instant;

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
        links: Arc<Mutex<Vec<bool>>>,
        tap: Arc<LiveTap>,
    }

    fn start(script: Vec<Result<Option<Report>, DeviceError>>, with_pad: bool) -> Harness {
        start_with(FakeController::new(script), with_pad)
    }

    fn start_with(ctl: FakeController, with_pad: bool) -> Harness {
        start_opts(ctl, with_pad, true)
    }

    fn start_opts(ctl: FakeController, with_pad: bool, detect_silence: bool) -> Harness {
        let writes = ctl.writes.clone();
        let reads = ctl.reads.clone();
        launch(
            Box::new(ctl),
            writes,
            reads,
            with_pad,
            detect_silence,
            Arc::new(Instant::now),
        )
    }

    /// Relógio simulado: só anda quando o controle falso lê.
    #[derive(Clone)]
    struct FakeClock {
        base: Instant,
        offset: Arc<Mutex<Duration>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset: Arc::new(Mutex::new(Duration::ZERO)),
            }
        }

        fn advance(&self, d: Duration) {
            *self.offset.lock().unwrap() += d;
        }

        fn clock(&self) -> Clock {
            let c = self.clone();
            Arc::new(move || c.base + *c.offset.lock().unwrap())
        }
    }

    /// Controle falso com tempo simulado: cada report avança o relógio pelo
    /// próximo passo (em ciclo); cada leitura ociosa avança o timeout.
    struct ClockedController {
        inner: FakeController,
        clock: FakeClock,
        steps: Vec<Duration>,
        n: usize,
        /// Leitura ociosa sem dormir (só o relógio simulado anda): 30 s simulados em ms.
        fast_idle: bool,
    }

    impl Controller for ClockedController {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn family(&self) -> axis_core::DeviceFamily {
            self.inner.family()
        }
        fn transport(&self) -> axis_devices::Transport {
            self.inner.transport()
        }
        fn experimental(&self) -> bool {
            self.inner.experimental()
        }
        fn read(&mut self, timeout_ms: i32) -> Result<Option<Report>, DeviceError> {
            if self.fast_idle && self.inner.reads.lock().unwrap().is_empty() {
                std::thread::yield_now();
                self.clock
                    .advance(Duration::from_millis(timeout_ms.max(0) as u64));
                return Ok(None);
            }
            let r = self.inner.read(timeout_ms);
            match &r {
                Ok(Some(_)) => {
                    self.clock.advance(self.steps[self.n % self.steps.len()]);
                    self.n += 1;
                }
                Ok(None) => self
                    .clock
                    .advance(Duration::from_millis(timeout_ms.max(0) as u64)),
                Err(_) => {}
            }
            r
        }
        fn write_output(&mut self, out: &OutputState) -> Result<(), DeviceError> {
            self.inner.write_output(out)
        }
    }

    fn start_clocked(steps: Vec<Duration>, with_pad: bool) -> Harness {
        start_clocked_opts(steps, with_pad, true, false)
    }

    /// `detect_silence = false` + `fast_idle`: um controle XInput, que só entrega report
    /// quando algo muda.
    fn start_clocked_opts(
        steps: Vec<Duration>,
        with_pad: bool,
        detect_silence: bool,
        fast_idle: bool,
    ) -> Harness {
        let inner = FakeController::new(vec![]);
        let writes = inner.writes.clone();
        let reads = inner.reads.clone();
        let clock = FakeClock::new();
        let c = clock.clock();
        let ctl = ClockedController {
            inner,
            clock,
            steps,
            n: 0,
            fast_idle,
        };
        launch(Box::new(ctl), writes, reads, with_pad, detect_silence, c)
    }

    fn launch(
        ctl: Box<dyn Controller>,
        writes: Arc<Mutex<Vec<OutputState>>>,
        reads: ReadQueue,
        with_pad: bool,
        detect_silence: bool,
        clock: Clock,
    ) -> Harness {
        let (pad, received) = FakePad::new();
        let shared: Option<SharedPad> = if with_pad {
            Some(Arc::new(Mutex::new(Box::new(pad) as Box<dyn VirtualPad>)))
        } else {
            None
        };
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        let exit = Arc::new(Mutex::new(None));
        let exit2 = exit.clone();
        let links = Arc::new(Mutex::new(Vec::new()));
        let links2 = links.clone();
        let tap = LiveTap::new();
        let (tx, handle) = spawn_clocked(
            ctl,
            shared,
            default_pipeline(),
            status.clone(),
            detect_silence,
            move |e| {
                let mut slot = exit2.lock().unwrap();
                assert!(slot.is_none(), "on_exit chamado mais de uma vez");
                *slot = Some(e);
            },
            move |up| links2.lock().unwrap().push(up),
            tap.clone(),
            clock,
        );
        Harness {
            tx,
            handle,
            received,
            writes,
            reads,
            status,
            exit,
            links,
            tap,
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
    fn silence_sends_neutral_once_and_signals_link_down_then_up() {
        let h = start(vec![report_x(1.0), report_x(1.0)], true);
        wait_until(|| h.received.lock().unwrap().len() == 2);
        // Fila vazia: cada leitura dorme o timeout; passa do limiar de silêncio.
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        std::thread::sleep(SILENCE_THRESHOLD + Duration::from_millis(100));
        {
            let got = h.received.lock().unwrap();
            assert_eq!(got.len(), 3, "neutro uma única vez");
            assert_eq!(got[2], InputState::default());
        }
        assert_eq!(*h.links.lock().unwrap(), vec![false]);
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.received.lock().unwrap().len() == 4);
        assert!((h.received.lock().unwrap()[3].left.x - 1.0).abs() < 1e-3);
        assert_eq!(*h.links.lock().unwrap(), vec![false, true]);
        h.finish();
    }

    #[test]
    fn non_streaming_controller_never_signals_link_down() {
        let h = start_opts(FakeController::new(vec![report_x(1.0)]), true, false);
        wait_until(|| h.received.lock().unwrap().len() == 1);
        std::thread::sleep(SILENCE_THRESHOLD + Duration::from_millis(200));
        assert!(h.links.lock().unwrap().is_empty());
        assert_eq!(
            h.received.lock().unwrap().len(),
            1,
            "sem neutro por silêncio"
        );
        h.finish();
    }

    #[test]
    fn rumble_is_zeroed_when_the_link_drops_and_on_set_pad() {
        let h = start(vec![report_x(1.0)], true);
        h.tx.send(WorkerCommand::SetLight {
            lightbar: [1, 2, 3],
            player: Some(1),
        })
        .unwrap();
        h.tx.send(WorkerCommand::Rumble(Rumble {
            strong: 200,
            weak: 50,
        }))
        .unwrap();
        wait_until(|| {
            h.writes
                .lock()
                .unwrap()
                .iter()
                .any(|w| w.rumble_strong == 200)
        });
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        h.tx.send(WorkerCommand::SetPad(None)).unwrap();
        let before = h.writes.lock().unwrap().len();
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.writes.lock().unwrap().len() > before);
        let w = *h.writes.lock().unwrap().last().unwrap();
        assert_eq!((w.rumble_strong, w.rumble_weak), (0, 0));
        assert_eq!((w.lightbar, w.player), ([1, 2, 3], Some(1)));
        h.finish();
    }

    #[test]
    fn set_pad_none_stops_forwarding_and_some_resumes() {
        let h = start(vec![report_x(1.0)], true);
        wait_until(|| h.received.lock().unwrap().len() == 1);
        h.tx.send(WorkerCommand::SetPad(None)).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.reads.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(h.received.lock().unwrap().len(), 1);
        let (pad2, received2) = FakePad::new();
        let shared: SharedPad = Arc::new(Mutex::new(Box::new(pad2) as Box<dyn VirtualPad>));
        h.tx.send(WorkerCommand::SetPad(Some(shared))).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| received2.lock().unwrap().len() == 1);
        assert!((received2.lock().unwrap()[0].left.x - 1.0).abs() < 1e-3);
        assert_eq!(h.received.lock().unwrap().len(), 1);
        h.finish();
    }

    #[test]
    fn tap_disabled_publishes_nothing() {
        let h = start(vec![], true);
        for _ in 0..5 {
            h.reads.lock().unwrap().push_back(report_x(1.0));
        }
        wait_until(|| h.received.lock().unwrap().len() == 5);
        assert_eq!(h.tap.latest().seq, 0);
        h.finish();
    }

    #[test]
    fn tap_enabled_publishes_traced_samples_throttled() {
        let h = start(vec![], true);
        h.tap.set_enabled(true);
        {
            let mut q = h.reads.lock().unwrap();
            for _ in 0..20 {
                q.push_back(report_x(0.5));
            }
        }
        wait_until(|| h.received.lock().unwrap().len() == 20);
        let s = h.tap.latest();
        assert_eq!(s.seq, 1);
        assert_eq!(s.reports, 1);
        assert_eq!(s.trace.left.raw, s.input.left);
        assert!((s.input.left.x - 0.5).abs() < 1e-3);
        // O 20º report, barrado pelo limitador, sai na leitura ociosa seguinte.
        wait_until(|| h.tap.latest().seq == 2);
        assert_eq!(h.tap.latest().reports, 20);
        std::thread::sleep(Duration::from_millis(20));
        h.reads.lock().unwrap().push_back(report_x(0.5));
        wait_until(|| h.tap.latest().seq == 3);
        assert_eq!(h.tap.latest().reports, 21);
        h.finish();
    }

    #[test]
    fn throttled_last_report_is_flushed_later() {
        let h = start_opts(FakeController::new(vec![]), true, false);
        h.tap.set_enabled(true);
        {
            let mut q = h.reads.lock().unwrap();
            q.push_back(report_x(1.0));
            q.push_back(report_x(0.0));
        }
        wait_until(|| h.received.lock().unwrap().len() == 2);
        wait_until(|| h.tap.latest().output.left.x == 0.0 && h.tap.latest().seq >= 1);
        assert_eq!(h.tap.latest().reports, 2);
        h.finish();
    }

    #[test]
    fn silence_or_exit_publishes_neutral_sample() {
        let h = start(vec![], true);
        h.tap.set_enabled(true);
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| h.tap.latest().seq >= 1);
        assert!((h.tap.latest().input.left.x - 1.0).abs() < 1e-3);
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        let s = h.tap.latest();
        assert_eq!(s.output, InputState::default());
        assert_eq!(s.input, InputState::default());
        assert_eq!(s.trace, Trace::default());
        assert!(s.reports >= 1);
        // Volta o sinal e depois o worker sai: a saída também zera o palco.
        let tap = h.tap.clone();
        h.reads.lock().unwrap().push_back(report_x(1.0));
        wait_until(|| tap.latest().input.left.x > 0.5);
        h.finish();
        assert_eq!(tap.latest().output, InputState::default());
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn drift_left() -> Box<Capture> {
        Box::new(Capture::new(
            CaptureKind::Drift(Stick::Left),
            Vec2::default(),
        ))
    }

    fn push_reports(h: &Harness, n: usize, x: f32) {
        let mut q = h.reads.lock().unwrap();
        for _ in 0..n {
            q.push_back(report_x(x));
        }
    }

    fn snapshot(h: &Harness) -> Option<CaptureSnapshot> {
        h.tap.latest().capture
    }

    #[test]
    fn capture_collects_raw_input_and_reports_outcome() {
        // Só visualização: a captura não depende do virtual nem da saída.
        let h = start_clocked(vec![ms(4)], false);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        // 776 reports a cada 4 ms = 3,1 s de tempo simulado.
        push_reports(&h, 776, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.outcome.is_some()));
        let s = h.tap.latest();
        assert_eq!(s.output.left.x, 0.0, "a deadzone padrão zera 0,04 na saída");
        let snap = s.capture.unwrap();
        assert_eq!(snap.kind, CaptureKind::Drift(Stick::Left));
        assert_eq!(snap.progress, 1.0);
        let Some(Outcome::Drift(r)) = snap.outcome else {
            panic!("esperava Drift, veio {:?}", snap.outcome);
        };
        assert!((r.center.x - 0.04).abs() < 1e-3, "{r:?}");
        assert!(r.center.y.abs() < 1e-3, "{r:?}");
        // O resultado fica na amostra enquanto não vier outro comando.
        let before = h.tap.latest().reports;
        push_reports(&h, 10, 0.04);
        wait_until(|| h.tap.latest().reports >= before + 10);
        assert!(matches!(
            snapshot(&h).and_then(|s| s.outcome),
            Some(Outcome::Drift(_))
        ));
        h.finish();
    }

    #[test]
    fn capture_progress_is_published_while_running() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        // Sem reports: o início da captura já sai na amostra.
        wait_until(|| snapshot(&h).is_some());
        assert_eq!(snapshot(&h).unwrap().progress, 0.0);
        push_reports(&h, 376, 0.0); // 1,5 s
        // A amostra pode sair um pouco depois do último report (limite de 17 ms).
        wait_until(|| {
            h.tap.latest().reports == 376 && snapshot(&h).is_some_and(|s| s.progress >= 0.49)
        });
        let s = snapshot(&h).unwrap();
        assert!((s.progress - 0.5).abs() < 0.01, "{s:?}");
        assert_eq!(s.outcome, None);
        h.finish();
    }

    #[test]
    fn capture_aborted_on_link_down() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 50, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.progress > 0.0));
        // Fila vazia: o relógio simulado passa dos 500 ms de silêncio.
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        let s = h.tap.latest();
        assert_eq!(s.input, InputState::default(), "neutro publicado");
        assert_eq!(
            s.capture.as_ref().and_then(|c| c.outcome.clone()),
            Some(Outcome::Aborted)
        );
        // O sinal volta: a captura não recomeça sozinha.
        push_reports(&h, 5, 0.04);
        wait_until(|| h.tap.latest().reports == 55);
        assert_eq!(snapshot(&h).and_then(|c| c.outcome), Some(Outcome::Aborted));
        h.finish();
    }

    #[test]
    fn capture_aborted_on_worker_exit() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 20, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.progress > 0.0));
        let tap = h.tap.clone();
        h.finish();
        let s = tap.latest();
        assert_eq!(s.output, InputState::default());
        assert_eq!(s.capture.and_then(|c| c.outcome), Some(Outcome::Aborted));
    }

    #[test]
    fn finished_outcome_survives_link_down() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 776, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.outcome.is_some()));
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        assert!(matches!(
            snapshot(&h).and_then(|s| s.outcome),
            Some(Outcome::Drift(_))
        ));
        h.finish();
    }

    #[test]
    fn start_capture_replaces_a_running_one() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 50, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.progress > 0.0));
        let range = Capture::new(CaptureKind::Range(Stick::Left), Vec2::default());
        h.tx.send(WorkerCommand::StartCapture(Box::new(range)))
            .unwrap();
        wait_until(|| snapshot(&h).is_some_and(|s| s.kind == CaptureKind::Range(Stick::Left)));
        // 3,1 s parado: o drift teria terminado; o alcance segue sem resultado.
        push_reports(&h, 776, 0.04);
        wait_until(|| h.tap.latest().reports == 826);
        let s = snapshot(&h).unwrap();
        assert_eq!(s.kind, CaptureKind::Range(Stick::Left));
        assert_eq!((s.progress, s.outcome), (0.0, None));
        h.finish();
    }

    #[test]
    fn capture_runs_while_paused() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::SetPaused(true)).unwrap();
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 776, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.outcome.is_some()));
        let Some(Outcome::Drift(r)) = snapshot(&h).and_then(|s| s.outcome) else {
            panic!("esperava Drift");
        };
        assert!((r.center.x - 0.04).abs() < 1e-3, "{r:?}");
        let got = h.received.lock().unwrap().clone();
        assert_eq!(got, vec![InputState::default()], "pausado: só o neutro");
        h.finish();
    }

    #[test]
    fn cancel_clears_capture() {
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 50, 0.04);
        wait_until(|| snapshot(&h).is_some_and(|s| s.progress > 0.0));
        h.tx.send(WorkerCommand::CancelCapture).unwrap();
        // Sem reports novos: o cancelamento sai na leitura ociosa seguinte.
        wait_until(|| snapshot(&h).is_none());
        // A captura foi solta: 3,1 s de reports depois, nada de resultado.
        push_reports(&h, 776, 0.04);
        wait_until(|| h.tap.latest().reports == 826);
        assert_eq!(snapshot(&h), None);
        h.finish();
    }

    /// XInput parado: um único report e depois só `Ok(None)`; devolve o resultado.
    fn still_xinput_outcome(capture: Box<Capture>, first: Report) -> Outcome {
        let h = start_clocked_opts(vec![ms(4)], false, false, true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(capture)).unwrap();
        wait_until(|| snapshot(&h).is_some());
        h.reads.lock().unwrap().push_back(Ok(Some(first)));
        wait_until(|| snapshot(&h).is_some_and(|s| s.outcome.is_some()));
        let outcome = snapshot(&h).and_then(|s| s.outcome).unwrap();
        assert_eq!(h.tap.latest().reports, 1, "nenhum report novo");
        h.finish();
        outcome
    }

    #[test]
    fn hid_silence_near_the_end_of_drift_aborts() {
        // HID: `Ok(None)` só quando o controle está calando; o último report não
        // pode empurrar o tempo da captura até o fim.
        let h = start_clocked(vec![ms(4)], true);
        h.tap.set_enabled(true);
        h.tx.send(WorkerCommand::StartCapture(drift_left()))
            .unwrap();
        wait_until(|| snapshot(&h).is_some());
        push_reports(&h, 676, 0.04); // 2,7 s
        wait_until(|| *h.links.lock().unwrap() == vec![false]);
        assert_eq!(snapshot(&h).and_then(|s| s.outcome), Some(Outcome::Aborted));
        h.finish();
    }

    #[test]
    fn still_stick_without_new_reports_finishes_drift() {
        let mut r = Report::default();
        r.input.left = Vec2 { x: 0.04, y: 0.0 };
        let Outcome::Drift(d) = still_xinput_outcome(drift_left(), r) else {
            panic!("esperava Drift");
        };
        assert!((d.center.x - 0.04).abs() < 1e-3, "{d:?}");
        // ~3 s a 50 ms por leitura ociosa
        assert!((55..=70).contains(&d.samples), "{d:?}");
    }

    #[test]
    fn still_stick_without_new_reports_times_out_range_and_triggers() {
        let range = Box::new(Capture::new(
            CaptureKind::Range(Stick::Left),
            Vec2::default(),
        ));
        assert!(matches!(
            still_xinput_outcome(range, Report::default()),
            Outcome::Incomplete(_)
        ));
        let triggers = Box::new(Capture::new(CaptureKind::Triggers, Vec2::default()));
        assert!(matches!(
            still_xinput_outcome(triggers, Report::default()),
            Outcome::Incomplete(_)
        ));
    }

    #[test]
    fn jitter_reflects_interval_spread() {
        let h = start_clocked(vec![ms(4), ms(6)], true);
        h.tap.set_enabled(true);
        push_reports(&h, 300, 0.0);
        wait_until(|| h.tap.latest().reports == 300);
        let j = h.tap.latest().jitter_us;
        assert!((800..=1200).contains(&j), "jitter {j}");
        h.finish();

        let h = start_clocked(vec![ms(5)], true);
        h.tap.set_enabled(true);
        push_reports(&h, 300, 0.0);
        wait_until(|| h.tap.latest().reports == 300);
        let j = h.tap.latest().jitter_us;
        assert!(j < 100, "jitter {j}");
        h.finish();
    }

    #[test]
    fn interval_ring_p99_of_deviation() {
        let mut r = IntervalRing::default();
        assert_eq!(r.jitter_p99(), 0);
        for i in 0..1000 {
            r.record(if i % 2 == 0 { 4000 } else { 6000 });
        }
        assert_eq!(r.jitter_p99(), 1000);
        for _ in 0..256 {
            r.record(5000);
        }
        assert_eq!(r.jitter_p99(), 0, "a janela só guarda os 256 últimos");
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
