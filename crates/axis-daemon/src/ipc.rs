//! Servidor do named pipe: a interface conversa com o núcleo por aqui.
//!
//! Uma thread aceita conexões (até 4 instâncias); cada cliente ganha uma
//! thread leitora e uma escritora sobre o mesmo HANDLE. O pipe é aberto com
//! `FILE_FLAG_OVERLAPPED`: num HANDLE síncrono o Windows serializa as operações
//! do mesmo objeto de arquivo, e um `ReadFile` bloqueado travaria o `WriteFile`
//! da outra thread. Cada operação espera o próprio evento junto com o evento
//! de fechamento da conexão, então qualquer lado encerra o outro na hora.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axis_protocol::{
    ClientMsg, LineReader, LiveFrame, PROTOCOL_VERSION, ServerMsg, decode_client, encode,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

use crate::live::{LiveSample, LiveTap, UiRegistry, to_pad_state, to_stages};
use crate::manager::Event;

const MAX_INSTANCES: u32 = 4;
const BUFFER: u32 = 64 * 1024;
/// Espera entre tentativas quando as 4 instâncias estão ocupadas.
const BUSY_BACKOFF: Duration = Duration::from_millis(200);
/// Espera entre tentativas após outra falha ao criar a instância.
const ERROR_BACKOFF: Duration = Duration::from_secs(1);
/// Espera após falhas seguidas do `ConnectNamedPipe` (exceto `ERROR_NO_DATA`).
const CONNECT_BACKOFF: Duration = Duration::from_millis(50);
/// Prazo para o cliente mandar o `Hello`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Cliente que não lê por este tempo (buffer cheio) é desconectado.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Espera da escritora sem inspeção: mudanças e `wake` a acordam antes. É o
/// limite de uma corrida de acordar perdido: se a leitora chama `close()` e
/// `wake()` entre o `is_closed()` da escritora e a entrada em `wait_change`, o
/// aviso se perde e a escritora só nota o fechamento ao fim desta espera.
const IDLE_WAIT: Duration = Duration::from_millis(500);
/// Espera da escritora com inspeção. O Windows arredonda a espera da condvar
/// para o tick do relógio (~15,6 ms): pedir 16 ms custa dois ticks (~31 ms,
/// ~32 quadros por segundo). Com 6 ms a espera cabe num tick e a escritora
/// acompanha o worker (no máximo uma amostra a cada 17 ms, ~59 por segundo);
/// amostra repetida não é reenviada (`seq`).
const LIVE_WAIT: Duration = Duration::from_millis(6);
/// Janela da média móvel de `rate_hz`.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// HANDLE do Win32 fechado uma única vez, no `Drop`.
struct OwnedHandle(HANDLE);

// SAFETY: um HANDLE de objeto do kernel pode ser usado de qualquer thread; as
// operações concorrentes sobre ele são sincronizadas pelo próprio kernel.
unsafe impl Send for OwnedHandle {}
// SAFETY: idem; `&OwnedHandle` só expõe o valor do HANDLE.
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: o HANDLE é válido e pertence só a este valor; fecha uma vez.
        unsafe { CloseHandle(self.0) };
    }
}

/// Evento manual do Win32 usado por uma thread nas suas operações overlapped.
pub(crate) struct WinEvent(OwnedHandle);

impl WinEvent {
    pub(crate) fn new() -> io::Result<WinEvent> {
        // SAFETY: sem atributos nem nome; reset manual, começa desligado.
        let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(WinEvent(OwnedHandle(h)))
    }

    fn raw(&self) -> HANDLE {
        self.0.0
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win_err(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

fn closed_err() -> io::Error {
    io::Error::new(ErrorKind::ConnectionAborted, "conexão encerrada")
}

/// Uma ponta do pipe (servidor ou cliente) aberta em modo overlapped.
pub(crate) struct Conn {
    pipe: OwnedHandle,
    /// Ligado por `close`: aborta a operação em curso de qualquer thread.
    close_ev: WinEvent,
    closed: AtomicBool,
    /// Prazo da leitura enquanto o `Hello` não chegou.
    read_deadline: Mutex<Option<Instant>>,
}

impl Conn {
    fn from_handle(h: HANDLE) -> io::Result<Conn> {
        // Assume o HANDLE já aqui: se o evento falhar, ele é fechado.
        let pipe = OwnedHandle(h);
        Ok(Conn {
            pipe,
            close_ev: WinEvent::new()?,
            closed: AtomicBool::new(false),
            read_deadline: Mutex::new(None),
        })
    }

    /// Cria uma instância do servidor; `Err(código)` do Win32 na falha. Com
    /// `first`, falha (`ERROR_ACCESS_DENIED`) se o nome já existir: o pipe não
    /// é dividido com outro processo.
    fn create_server(name: &[u16], first: bool) -> Result<Conn, u32> {
        let first_flag = if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
        // SAFETY: `name` termina em 0; segurança padrão (usuário local).
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first_flag,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                MAX_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                std::ptr::null(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: lido logo após a falha, na mesma thread.
            return Err(unsafe { GetLastError() });
        }
        Conn::from_handle(h).map_err(|e| e.raw_os_error().unwrap_or(0) as u32)
    }

    /// Abre o pipe como cliente (usado pelos testes).
    #[cfg(test)]
    pub(crate) fn open_client(name: &str) -> io::Result<Conn> {
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
        let name = wide(name);
        // SAFETY: `name` termina em 0; sem compartilhamento, atributos ou modelo.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Conn::from_handle(h)
    }

    /// Encerra a conexão: aborta a operação em curso de qualquer thread.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        // SAFETY: evento válido enquanto `self` existe.
        unsafe { SetEvent(self.close_ev.raw()) };
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Roda uma operação overlapped iniciada por `start` e espera seu fim, o
    /// fechamento da conexão ou o prazo. Só volta depois que o kernel terminou
    /// de usar o `OVERLAPPED` e o buffer. `Err` traz o código do Win32.
    fn overlapped(
        &self,
        ev: &WinEvent,
        timeout: Option<Duration>,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
    ) -> Result<u32, u32> {
        if self.is_closed() {
            return Err(ERROR_OPERATION_ABORTED);
        }
        // SAFETY: OVERLAPPED é uma struct C simples; zero é o estado inicial.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.raw();
        if start(&mut ov) == 0 {
            // SAFETY: lido logo após a falha, na mesma thread.
            let e = unsafe { GetLastError() };
            if e != ERROR_IO_PENDING {
                return Err(e);
            }
        }
        let ms = timeout.map_or(INFINITE, |d| {
            d.as_millis().min(u128::from(INFINITE - 1)) as u32
        });
        let handles = [ev.raw(), self.close_ev.raw()];
        // SAFETY: dois HANDLEs de evento válidos.
        let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, ms) };
        if w != WAIT_OBJECT_0 {
            // Fechamento, prazo ou falha da espera: cancela e espera a conclusão.
            // SAFETY: `ov` é o OVERLAPPED da operação em curso neste HANDLE.
            unsafe { CancelIoEx(self.pipe.0, &ov) };
        }
        let mut n = 0u32;
        // SAFETY: espera (bWait = TRUE) a operação de `ov` terminar de vez.
        let ok = unsafe { GetOverlappedResult(self.pipe.0, &ov, &mut n, 1) };
        if ok == 0 {
            // SAFETY: lido logo após a falha, na mesma thread.
            return Err(unsafe { GetLastError() });
        }
        Ok(n)
    }

    /// Espera um cliente conectar nesta instância do servidor.
    fn connect(&self, ev: &WinEvent) -> Result<(), u32> {
        let h = self.pipe.0;
        // SAFETY: HANDLE de servidor válido; `ov` vive até o fim da operação.
        match self.overlapped(ev, None, |ov| unsafe { ConnectNamedPipe(h, ov) }) {
            Ok(_) | Err(ERROR_PIPE_CONNECTED) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn read(&self, ev: &WinEvent, buf: &mut [u8]) -> io::Result<usize> {
        let deadline = *self.read_deadline.lock().unwrap_or_else(|e| e.into_inner());
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let (h, len) = (self.pipe.0, buf.len().min(u32::MAX as usize) as u32);
        let ptr = buf.as_mut_ptr();
        // SAFETY: `buf` é emprestado por toda a chamada e `overlapped` só volta
        // depois que o kernel terminou de escrever nele.
        match self.overlapped(ev, timeout, |ov| unsafe {
            ReadFile(h, ptr, len, std::ptr::null_mut(), ov)
        }) {
            Ok(n) => Ok(n as usize),
            Err(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA) => Ok(0),
            Err(_) if self.is_closed() => Err(closed_err()),
            Err(e) => Err(win_err(e)),
        }
    }

    pub(crate) fn write_all(&self, ev: &WinEvent, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let (h, len) = (self.pipe.0, buf.len().min(u32::MAX as usize) as u32);
            let ptr = buf.as_ptr();
            // SAFETY: `buf` é emprestado por toda a chamada e `overlapped` só
            // volta depois que o kernel terminou de ler dele.
            let n = match self.overlapped(ev, Some(WRITE_TIMEOUT), |ov| unsafe {
                WriteFile(h, ptr, len, std::ptr::null_mut(), ov)
            }) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "o pipe não aceitou dados",
                    ));
                }
                Ok(n) => n as usize,
                Err(_) if self.is_closed() => return Err(closed_err()),
                Err(e) => return Err(win_err(e)),
            };
            buf = &buf[n.min(buf.len())..];
        }
        Ok(())
    }

    fn send(&self, ev: &WinEvent, msg: &ServerMsg) -> io::Result<()> {
        self.write_all(ev, encode(msg).as_bytes())
    }
}

/// `Read` sobre uma `Conn`, com evento próprio (uma thread por leitor).
pub(crate) struct PipeReader {
    conn: Arc<Conn>,
    ev: WinEvent,
}

impl PipeReader {
    pub(crate) fn new(conn: Arc<Conn>) -> io::Result<PipeReader> {
        Ok(PipeReader {
            conn,
            ev: WinEvent::new()?,
        })
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.conn.read(&self.ev, buf)
    }
}

/// Inicia o servidor do pipe `pipe_name` (ex.: [`axis_protocol::PIPE_NAME`]).
pub fn spawn_ipc_server(
    pipe_name: &str,
    ui: Arc<UiRegistry>,
    events: Sender<Event>,
) -> JoinHandle<()> {
    let name = wide(pipe_name);
    std::thread::Builder::new()
        .name("axis-ipc".into())
        .spawn(move || accept_loop(&name, &ui, &events))
        .expect("criar a thread do servidor IPC")
}

fn accept_loop(name: &[u16], ui: &Arc<UiRegistry>, events: &Sender<Event>) {
    let ev = match WinEvent::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("servidor da interface sem evento: {e}");
            return;
        }
    };
    // Até a primeira instância sair, o nome não pode existir: outro processo
    // (ou o núcleo anterior ainda saindo) não divide o pipe com este.
    let mut owned = false;
    let mut busy_logged = false;
    let mut last_err = 0;
    let mut connect_failures = 0u32;
    loop {
        let conn = match Conn::create_server(name, !owned) {
            Ok(c) => c,
            Err(ERROR_PIPE_BUSY) => {
                // As 4 instâncias estão em uso: tenta de novo em breve.
                if !busy_logged {
                    log::warn!("as 4 conexões da interface estão em uso; aguardando uma livre");
                    busy_logged = true;
                }
                std::thread::sleep(BUSY_BACKOFF);
                continue;
            }
            Err(e) => {
                if e != last_err {
                    if e == ERROR_ACCESS_DENIED && !owned {
                        log::warn!(
                            "nome do pipe da interface já em uso por outro processo; tentando de novo"
                        );
                    } else {
                        log::warn!("não foi possível criar o pipe da interface: {}", win_err(e));
                    }
                    last_err = e;
                }
                std::thread::sleep(ERROR_BACKOFF);
                continue;
            }
        };
        owned = true;
        busy_logged = false;
        last_err = 0;
        match conn.connect(&ev) {
            Ok(()) => connect_failures = 0,
            // O cliente entrou e saiu antes do ConnectNamedPipe: comum, sem espera.
            Err(ERROR_NO_DATA) => continue,
            Err(e) => {
                log::info!("conexão da interface falhou: {}", win_err(e));
                connect_failures += 1;
                if connect_failures > 1 {
                    // Falha repetida: não gira a CPU criando instâncias.
                    std::thread::sleep(CONNECT_BACKOFF);
                }
                continue;
            }
        }
        *conn.read_deadline.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Instant::now() + HANDSHAKE_TIMEOUT);
        let client = Arc::new(Client {
            conn: Arc::new(conn),
            inspected: Mutex::new(None),
        });
        let (ui, events) = (ui.clone(), events.clone());
        if let Err(e) = std::thread::Builder::new()
            .name("axis-ipc-leitora".into())
            .spawn(move || serve(client, ui, events))
        {
            log::warn!("sem thread para o cliente da interface: {e}");
        }
    }
}

/// Estado de um cliente conectado, dividido entre leitora e escritora.
struct Client {
    conn: Arc<Conn>,
    /// Controle que este cliente inspeciona.
    inspected: Mutex<Option<String>>,
}

impl Client {
    fn inspected(&self) -> Option<String> {
        self.inspected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Troca o controle inspecionado (desliga a inspeção anterior deste cliente).
    fn set_inspect(&self, ui: &UiRegistry, id: Option<String>) {
        let mut cur = self.inspected.lock().unwrap_or_else(|e| e.into_inner());
        if *cur == id {
            return;
        }
        if let Some(old) = cur.take() {
            ui.inspect(&old, false);
        }
        if let Some(new) = &id {
            ui.inspect(new, true);
        }
        *cur = id;
        drop(cur);
        // A escritora troca de ritmo na hora.
        ui.wake();
    }
}

/// Thread leitora de um cliente: handshake, comandos e limpeza no fim.
fn serve(client: Arc<Client>, ui: Arc<UiRegistry>, events: Sender<Event>) {
    let conn = &client.conn;
    let (reader, wev) = match (PipeReader::new(conn.clone()), WinEvent::new()) {
        (Ok(r), Ok(e)) => (r, e),
        _ => {
            log::warn!("sem eventos para o cliente da interface");
            return;
        }
    };
    let mut lines = LineReader::new(reader);
    let first = match lines.next_line() {
        Ok(Some(l)) => l,
        Ok(None) => return,
        Err(e) => {
            log::info!("cliente da interface sem Hello: {e}");
            return;
        }
    };
    match decode_client(&first) {
        Ok(ClientMsg::Hello {
            version: PROTOCOL_VERSION,
        }) => {}
        Ok(ClientMsg::Hello { version }) => {
            log::info!("interface com protocolo {version}; o núcleo fala {PROTOCOL_VERSION}");
            let _ = conn.send(
                &wev,
                &ServerMsg::Incompatible {
                    server_version: PROTOCOL_VERSION,
                },
            );
            return;
        }
        _ => {
            log::info!("cliente da interface não começou com Hello");
            return;
        }
    }
    *conn.read_deadline.lock().unwrap_or_else(|e| e.into_inner()) = None;

    let snap = ui.snapshot();
    let greeting = conn
        .send(
            &wev,
            &ServerMsg::Welcome {
                version: PROTOCOL_VERSION,
                paused: snap.paused,
                emulation: snap.emulation,
            },
        )
        .and_then(|()| {
            conn.send(
                &wev,
                &ServerMsg::Sessions {
                    sessions: snap.sessions,
                },
            )
        });
    if let Err(e) = greeting {
        log::info!("cliente da interface saiu no handshake: {e}");
        return;
    }
    log::info!("interface conectada");

    let writer = {
        let (client, ui) = (client.clone(), ui.clone());
        let start = WriterStart {
            generation: snap.generation,
            notices_sent: snap.notices_total,
            state: (snap.paused, snap.emulation),
        };
        std::thread::Builder::new()
            .name("axis-ipc-escritora".into())
            .spawn(move || {
                if let Err(e) = write_loop(&client, &ui, &wev, start) {
                    log::info!("interface parou de receber: {e}");
                }
                // Acorda a leitora, que faz a limpeza.
                client.conn.close();
            })
    };
    let writer = match writer {
        Ok(w) => Some(w),
        Err(e) => {
            log::warn!("sem thread de escrita para a interface: {e}");
            conn.close();
            None
        }
    };

    read_loop(&client, &ui, &events, &mut lines);

    client.set_inspect(&ui, None);
    conn.close();
    ui.wake();
    if let Some(w) = writer {
        let _ = w.join();
    }
    log::info!("interface desconectada");
}

fn read_loop(
    client: &Client,
    ui: &UiRegistry,
    events: &Sender<Event>,
    lines: &mut LineReader<PipeReader>,
) {
    loop {
        let line = match lines.next_line() {
            Ok(Some(l)) => l,
            Ok(None) => return,
            Err(e) => {
                if !client.conn.is_closed() {
                    log::info!("cliente da interface descartado: {e}");
                }
                return;
            }
        };
        let msg = match decode_client(&line) {
            Ok(m) => m,
            Err(e) => {
                log::info!("cliente da interface descartado: {e}");
                return;
            }
        };
        let ev = match msg {
            ClientMsg::Hello { .. } => None,
            ClientMsg::Inspect { id } => {
                client.set_inspect(ui, id);
                None
            }
            ClientMsg::SetPaused { paused } => Some(Event::SetPaused(paused)),
            ClientMsg::Restore => Some(Event::RestoreControllers),
        };
        if let Some(ev) = ev
            && events.send(ev).is_err()
        {
            return; // o gerente terminou
        }
    }
}

/// O que o cliente já recebeu no handshake (ponto de partida da escritora).
struct WriterStart {
    generation: u64,
    notices_sent: u64,
    /// `(paused, emulation)` do `Welcome`.
    state: (bool, bool),
}

fn write_loop(
    client: &Client,
    ui: &UiRegistry,
    ev: &WinEvent,
    start: WriterStart,
) -> io::Result<()> {
    let WriterStart {
        mut generation,
        mut notices_sent,
        mut state,
    } = start;
    let conn = &client.conn;
    let mut live = LiveCursor::default();
    loop {
        let inspected = client.inspected();
        let wait = if inspected.is_some() {
            LIVE_WAIT
        } else {
            IDLE_WAIT
        };
        let g = ui.wait_change(generation, Some(wait));
        if conn.is_closed() {
            return Ok(());
        }
        if g != generation {
            let snap = ui.snapshot();
            generation = snap.generation;
            // Pausa/emulação antes da lista: a interface já a mostra no estado novo.
            if (snap.paused, snap.emulation) != state {
                state = (snap.paused, snap.emulation);
                conn.send(
                    ev,
                    &ServerMsg::State {
                        paused: snap.paused,
                        emulation: snap.emulation,
                    },
                )?;
            }
            conn.send(
                ev,
                &ServerMsg::Sessions {
                    sessions: snap.sessions,
                },
            )?;
            // A janela guarda só os 20 últimos: o que saiu dela se perdeu.
            let fresh = snap
                .notices_total
                .saturating_sub(notices_sent)
                .min(snap.notices.len() as u64) as usize;
            for text in &snap.notices[snap.notices.len() - fresh..] {
                conn.send(ev, &ServerMsg::Notice { text: text.clone() })?;
            }
            notices_sent = snap.notices_total;
        }
        match inspected {
            Some(id) => {
                if let Some(frame) = live.poll(ui, &id, Instant::now()) {
                    conn.send(ev, &ServerMsg::Live { frame })?;
                }
            }
            None => live = LiveCursor::default(),
        }
    }
}

/// O que a escritora já enviou do controle inspecionado.
#[derive(Default)]
struct LiveCursor {
    id: Option<String>,
    tap: Option<Arc<LiveTap>>,
    seq: u64,
    rate: RateMeter,
}

impl LiveCursor {
    /// Próximo quadro, se o worker publicou uma amostra nova. Ao começar (ou
    /// ao trocar de torneira) a amostra presente pode ser de uma inspeção
    /// antiga: só marca o `seq` dela e espera a próxima.
    fn poll(&mut self, ui: &UiRegistry, id: &str, now: Instant) -> Option<LiveFrame> {
        let tap = ui.tap(id)?;
        let same = self.id.as_deref() == Some(id)
            && self.tap.as_ref().is_some_and(|t| Arc::ptr_eq(t, &tap));
        let s = tap.latest();
        if !same {
            self.id = Some(id.to_string());
            self.tap = Some(tap);
            self.seq = s.seq;
            self.rate.reset();
            return None;
        }
        if s.seq == self.seq {
            return None;
        }
        self.seq = s.seq;
        let rate_hz = self.rate.sample(now, s.reports);
        Some(frame(id, &s, rate_hz))
    }
}

fn frame(id: &str, s: &LiveSample, rate_hz: u32) -> LiveFrame {
    LiveFrame {
        id: id.to_string(),
        input: to_pad_state(&s.input),
        output: to_pad_state(&s.output),
        left: to_stages(&s.trace.left),
        right: to_stages(&s.trace.right),
        rate_hz,
        latency_p99_us: s.latency_p99_us,
        battery: s.status.battery_percent,
        charging: s.status.charging,
    }
}

/// Reports por segundo entre as amostras enviadas, em média móvel de ~1 s.
#[derive(Default)]
struct RateMeter {
    points: VecDeque<(Instant, u64)>,
}

impl RateMeter {
    fn reset(&mut self) {
        self.points.clear();
    }

    fn sample(&mut self, now: Instant, reports: u64) -> u32 {
        // Contador que volta (worker novo): recomeça a média.
        if self.points.back().is_some_and(|&(_, r)| reports < r) {
            self.points.clear();
        }
        self.points.push_back((now, reports));
        // Mantém como base o ponto mais novo com pelo menos 1 s de idade.
        while self.points.len() > 2
            && now.saturating_duration_since(self.points[1].0) >= RATE_WINDOW
        {
            self.points.pop_front();
        }
        let (t0, r0) = self.points[0];
        let dt = now.saturating_duration_since(t0).as_secs_f64();
        if dt <= 0.0 {
            return 0;
        }
        ((reports - r0) as f64 / dt)
            .round()
            .min(f64::from(u32::MAX)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    use axis_core::Vec2;
    use axis_devices::DeviceStatus;
    use axis_protocol::{
        ClientMsg, Family, LineReader, ServerMsg, SessionInfo, decode_server, encode,
    };

    use crate::live::{LiveSample, LiveTap, UiRegistry};
    use crate::manager::Event;

    const RECV: Duration = Duration::from_millis(1500);

    static NEXT: AtomicU32 = AtomicU32::new(0);

    fn pipe_name() -> String {
        format!(
            r"\\.\pipe\axis-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        )
    }

    fn info(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            owner: "Controle 1".into(),
            color: [1, 2, 3],
            family: Family::DualShock4,
            player: Some(1),
            connected: true,
            experimental: false,
            view_only: false,
            battery: None,
            charging: false,
        }
    }

    /// Cliente real: abre o pipe e lê as linhas numa thread própria.
    struct TestClient {
        conn: Arc<Conn>,
        ev: WinEvent,
        rx: Receiver<Option<ServerMsg>>,
        reader: Option<thread::JoinHandle<()>>,
    }

    impl TestClient {
        fn connect(name: &str) -> TestClient {
            TestClient::connect_within(name, Duration::from_secs(1))
        }

        fn connect_within(name: &str, wait: Duration) -> TestClient {
            let deadline = Instant::now() + wait;
            let conn = loop {
                match Conn::open_client(name) {
                    Ok(c) => break Arc::new(c),
                    Err(e) if Instant::now() >= deadline => panic!("sem conexão: {e}"),
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            };
            let (tx, rx) = mpsc::channel();
            let c2 = conn.clone();
            let reader = thread::spawn(move || {
                let mut lines = LineReader::new(PipeReader::new(c2).unwrap());
                loop {
                    match lines.next_line() {
                        Ok(Some(l)) => {
                            let m = decode_server(&l).expect("servidor mandou lixo");
                            if tx.send(Some(m)).is_err() {
                                return;
                            }
                        }
                        _ => {
                            let _ = tx.send(None);
                            return;
                        }
                    }
                }
            });
            TestClient {
                conn,
                ev: WinEvent::new().unwrap(),
                rx,
                reader: Some(reader),
            }
        }

        fn hello(name: &str) -> TestClient {
            let c = TestClient::connect(name);
            c.send(&ClientMsg::Hello { version: 1 });
            assert!(matches!(c.recv(), Some(ServerMsg::Welcome { .. })));
            assert!(matches!(c.recv(), Some(ServerMsg::Sessions { .. })));
            c
        }

        fn send(&self, m: &ClientMsg) {
            self.conn.write_all(&self.ev, encode(m).as_bytes()).unwrap();
        }

        fn send_raw(&self, b: &[u8]) -> io::Result<()> {
            self.conn.write_all(&self.ev, b)
        }

        /// Próxima mensagem; `None` = o servidor fechou.
        fn recv(&self) -> Option<ServerMsg> {
            self.rx.recv_timeout(RECV).expect("nada chegou a tempo")
        }

        /// Pula mensagens até uma que satisfaça `f`.
        fn recv_until(&self, mut f: impl FnMut(&ServerMsg) -> bool) -> ServerMsg {
            let deadline = Instant::now() + RECV;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(Some(m)) if f(&m) => return m,
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("servidor fechou"),
                    Err(_) => panic!("nada chegou a tempo"),
                }
            }
        }

        /// Espera o fim do fluxo, ignorando o que vier antes.
        fn expect_closed(&self) {
            let deadline = Instant::now() + RECV;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => panic!("cliente não foi desconectado"),
                }
            }
        }
    }

    impl Drop for TestClient {
        fn drop(&mut self) {
            self.conn.close();
            if let Some(r) = self.reader.take() {
                let _ = r.join();
            }
        }
    }

    fn server(ui: &Arc<UiRegistry>) -> (String, Receiver<Event>) {
        let name = pipe_name();
        let (tx, rx) = mpsc::channel();
        spawn_ipc_server(&name, ui.clone(), tx);
        (name, rx)
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !f() {
            assert!(Instant::now() < deadline, "tempo esgotado: {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn handshake_welcome_then_sessions() {
        let ui = UiRegistry::new();
        ui.publish(vec![info("aa:01")], HashMap::new(), true, true);
        // Janela de avisos cheia antes da conexão: nada disso é reenviado.
        for i in 0..20 {
            ui.push_notice(&format!("velho {i}"));
        }
        let (name, _rx) = server(&ui);
        let c = TestClient::connect(&name);
        c.send(&ClientMsg::Hello { version: 1 });
        assert_eq!(
            c.recv(),
            Some(ServerMsg::Welcome {
                version: 1,
                paused: true,
                emulation: true
            })
        );
        assert_eq!(
            c.recv(),
            Some(ServerMsg::Sessions {
                sessions: vec![info("aa:01")]
            })
        );

        ui.publish(
            vec![info("aa:01"), info("bb:02")],
            HashMap::new(),
            true,
            true,
        );
        assert_eq!(
            c.recv_until(|m| matches!(m, ServerMsg::Sessions { .. })),
            ServerMsg::Sessions {
                sessions: vec![info("aa:01"), info("bb:02")]
            }
        );

        // A janela desliza (continua com 20), mas cada aviso novo sai uma vez.
        ui.push_notice("novo");
        assert_eq!(
            c.recv_until(|m| matches!(m, ServerMsg::Notice { .. })),
            ServerMsg::Notice {
                text: "novo".into()
            }
        );
        ui.push_notice("outro");
        assert_eq!(
            c.recv_until(|m| matches!(m, ServerMsg::Notice { .. })),
            ServerMsg::Notice {
                text: "outro".into()
            }
        );
    }

    #[test]
    fn pause_and_emulation_changes_send_state_before_sessions() {
        let ui = UiRegistry::new();
        ui.publish(vec![info("aa:01")], HashMap::new(), false, true);
        let (name, _rx) = server(&ui);
        let c = TestClient::hello(&name);

        ui.publish(vec![info("aa:01")], HashMap::new(), true, true);
        assert_eq!(
            c.recv(),
            Some(ServerMsg::State {
                paused: true,
                emulation: true
            })
        );
        assert!(matches!(c.recv(), Some(ServerMsg::Sessions { .. })));

        // Mesmo par: só a lista nova, sem outro State.
        ui.publish(
            vec![info("aa:01"), info("bb:02")],
            HashMap::new(),
            true,
            true,
        );
        assert!(matches!(c.recv(), Some(ServerMsg::Sessions { .. })));
        ui.push_notice("aviso");
        assert!(matches!(c.recv(), Some(ServerMsg::Sessions { .. })));
        assert!(matches!(c.recv(), Some(ServerMsg::Notice { .. })));

        ui.publish(
            vec![info("aa:01"), info("bb:02")],
            HashMap::new(),
            true,
            false,
        );
        assert_eq!(
            c.recv(),
            Some(ServerMsg::State {
                paused: true,
                emulation: false
            })
        );
        assert!(matches!(c.recv(), Some(ServerMsg::Sessions { .. })));
    }

    #[test]
    fn wrong_version_gets_incompatible_and_close() {
        let ui = UiRegistry::new();
        let (name, _rx) = server(&ui);
        let c = TestClient::connect(&name);
        c.send(&ClientMsg::Hello { version: 99 });
        assert_eq!(
            c.recv(),
            Some(ServerMsg::Incompatible { server_version: 1 })
        );
        assert_eq!(c.recv(), None);
    }

    #[test]
    fn inspect_enables_tap_and_streams_live_then_disconnect_disables() {
        let ui = UiRegistry::new();
        let tap = LiveTap::new();
        // Amostra velha de uma inspeção anterior: não pode aparecer.
        tap.publish(LiveSample {
            input: axis_core::InputState {
                left: Vec2 { x: 0.9, y: 0.0 },
                ..Default::default()
            },
            seq: 5,
            ..Default::default()
        });
        ui.publish(
            vec![info("aa:01")],
            HashMap::from([("aa:01".to_string(), tap.clone())]),
            false,
            true,
        );
        let (name, _rx) = server(&ui);
        let a = TestClient::hello(&name);
        let b = TestClient::hello(&name);
        a.send(&ClientMsg::Inspect {
            id: Some("aa:01".into()),
        });
        b.send(&ClientMsg::Inspect {
            id: Some("aa:01".into()),
        });
        wait_until("torneira ligada", || tap.enabled());

        // Publica amostras novas até chegar um Live.
        let stop = Arc::new(AtomicBool::new(false));
        let (t2, s2) = (tap.clone(), stop.clone());
        let feeder = thread::spawn(move || {
            let mut seq = 6;
            while !s2.load(Ordering::SeqCst) {
                t2.publish(LiveSample {
                    input: axis_core::InputState {
                        left: Vec2 { x: 0.1, y: 0.0 },
                        ..Default::default()
                    },
                    status: DeviceStatus {
                        battery_percent: Some(70),
                        charging: true,
                    },
                    reports: seq * 4,
                    latency_p99_us: 800,
                    seq,
                    ..Default::default()
                });
                seq += 1;
                thread::sleep(Duration::from_millis(5));
            }
        });
        let ServerMsg::Live { frame } = a.recv_until(|m| matches!(m, ServerMsg::Live { .. }))
        else {
            unreachable!()
        };
        assert_eq!(frame.id, "aa:01");
        assert_eq!(frame.input.lx, 0.1, "nunca a amostra velha");
        assert_eq!(frame.latency_p99_us, 800);
        assert_eq!(frame.battery, Some(70));
        assert!(frame.charging);
        b.recv_until(|m| matches!(m, ServerMsg::Live { .. }));
        stop.store(true, Ordering::SeqCst);
        feeder.join().unwrap();

        // A sai: B ainda inspeciona, a torneira segue ligada.
        drop(a);
        thread::sleep(Duration::from_millis(100));
        assert!(tap.enabled());
        // B sai: desliga em até 1 s.
        drop(b);
        wait_until("torneira desligada", || !tap.enabled());
    }

    #[test]
    fn live_cadence_is_close_to_the_worker_rate() {
        let ui = UiRegistry::new();
        let tap = LiveTap::new();
        ui.publish(
            vec![info("aa:01")],
            HashMap::from([("aa:01".to_string(), tap.clone())]),
            false,
            true,
        );
        let (name, _rx) = server(&ui);
        let c = TestClient::hello(&name);
        c.send(&ClientMsg::Inspect {
            id: Some("aa:01".into()),
        });
        wait_until("torneira ligada", || tap.enabled());

        // O worker publica no máximo a cada 17 ms (~58 por segundo).
        let stop = Arc::new(AtomicBool::new(false));
        let (t2, s2) = (tap.clone(), stop.clone());
        let feeder = thread::spawn(move || {
            let mut seq = 1;
            while !s2.load(Ordering::SeqCst) {
                t2.publish(LiveSample {
                    seq,
                    ..Default::default()
                });
                seq += 1;
                thread::sleep(Duration::from_millis(17));
            }
        });
        c.recv_until(|m| matches!(m, ServerMsg::Live { .. }));
        let start = Instant::now();
        let mut frames = 0u32;
        while start.elapsed() < Duration::from_secs(1) {
            if let Ok(Some(ServerMsg::Live { .. })) = c.rx.recv_timeout(Duration::from_millis(100))
            {
                frames += 1;
            }
        }
        stop.store(true, Ordering::SeqCst);
        feeder.join().unwrap();
        // Folga larga: só pega o ritmo de ~32 por segundo (espera de 16 ms
        // arredondada para dois ticks de 15,6 ms).
        assert!(frames >= 45, "só {frames} quadros em 1 s");
    }

    #[test]
    fn oversized_or_garbage_line_drops_only_that_client() {
        let ui = UiRegistry::new();
        let (name, _rx) = server(&ui);
        let a = TestClient::hello(&name);
        let b = TestClient::hello(&name);
        let c = TestClient::hello(&name);
        // 70 KiB sem '\n': a escrita pode falhar quando o servidor fecha.
        let _ = a.send_raw(&vec![b'x'; 70 * 1024]);
        a.expect_closed();
        let _ = c.send_raw(b"lixo\n");
        c.expect_closed();

        ui.publish(vec![info("aa:01")], HashMap::new(), false, true);
        assert_eq!(
            b.recv_until(|m| matches!(m, ServerMsg::Sessions { .. })),
            ServerMsg::Sessions {
                sessions: vec![info("aa:01")]
            }
        );
        // Um cliente novo ainda é aceito.
        let _d = TestClient::hello(&name);
    }

    #[test]
    fn fifth_client_waits_for_a_free_instance() {
        let ui = UiRegistry::new();
        let (name, _rx) = server(&ui);
        let mut clients: Vec<TestClient> = (0..4).map(|_| TestClient::hello(&name)).collect();
        // As 4 instâncias estão em uso: o quinto ouve PIPE_BUSY.
        let busy = Conn::open_client(&name)
            .err()
            .and_then(|e| e.raw_os_error());
        assert_eq!(busy, Some(ERROR_PIPE_BUSY as i32));
        // Um sai: o servidor cria outra instância e o quinto entra.
        drop(clients.remove(0));
        let fifth = TestClient::connect_within(&name, Duration::from_secs(2));
        fifth.send(&ClientMsg::Hello { version: 1 });
        assert!(matches!(fifth.recv(), Some(ServerMsg::Welcome { .. })));
    }

    #[test]
    fn name_held_by_another_server_is_not_shared_then_taken_over() {
        let name = pipe_name();
        // Outro servidor já tem o nome (ex.: o núcleo anterior ainda saindo).
        let squatter = Conn::create_server(&wide(&name), true).expect("instância alheia");
        let ui = UiRegistry::new();
        let (tx, _rx) = mpsc::channel();
        spawn_ipc_server(&name, ui.clone(), tx);
        thread::sleep(Duration::from_millis(150));
        // Só a instância alheia existe: o primeiro cliente a ocupa e o segundo
        // não acha outra (o servidor não divide o nome).
        let a = Conn::open_client(&name).expect("instância alheia livre");
        let second = Conn::open_client(&name)
            .err()
            .and_then(|e| e.raw_os_error());
        assert_eq!(second, Some(ERROR_PIPE_BUSY as i32));
        // O dono anterior sai: o servidor assume o nome com a nova tentativa.
        drop(a);
        drop(squatter);
        let c = TestClient::connect_within(&name, Duration::from_secs(3));
        c.send(&ClientMsg::Hello { version: 1 });
        assert!(matches!(c.recv(), Some(ServerMsg::Welcome { .. })));
    }

    #[test]
    fn set_paused_and_restore_become_events() {
        let ui = UiRegistry::new();
        let (name, rx) = server(&ui);
        let c = TestClient::hello(&name);
        c.send(&ClientMsg::SetPaused { paused: true });
        c.send(&ClientMsg::Restore);
        assert!(matches!(rx.recv_timeout(RECV), Ok(Event::SetPaused(true))));
        assert!(matches!(
            rx.recv_timeout(RECV),
            Ok(Event::RestoreControllers)
        ));
    }

    #[test]
    fn rate_meter_smooths_over_one_second() {
        let t0 = Instant::now();
        let mut m = RateMeter::default();
        assert_eq!(m.sample(t0, 0), 0);
        // 250 reports por segundo, amostrados a cada 100 ms.
        let mut last = 0;
        for i in 1..=30u64 {
            last = m.sample(t0 + Duration::from_millis(100 * i), 25 * i);
        }
        assert_eq!(last, 250);
        // Contador que volta (torneira nova) não estoura.
        m.reset();
        assert_eq!(m.sample(t0, 10), 0);
        assert_eq!(m.sample(t0 + Duration::from_millis(500), 5), 0);
    }
}
