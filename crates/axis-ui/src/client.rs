//! Cliente do named pipe do núcleo.
//!
//! Uma thread conecta, manda o `Hello`, lê as linhas do núcleo e as traduz em
//! [`UiUpdate`]; caiu, avisa e tenta de novo a cada 1 s (a única espera
//! periódica do app, e só enquanto desconectado). Outra thread escreve as
//! mensagens de saída no mesmo HANDLE. Por isso o pipe é aberto com
//! `FILE_FLAG_OVERLAPPED`: num HANDLE síncrono o Windows serializa as operações
//! do mesmo objeto de arquivo, e o `ReadFile` bloqueado travaria o `WriteFile`.
//! Cada operação espera o próprio evento junto com o evento de fechamento da
//! conexão, e só volta depois que o kernel soltou o buffer.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use axis_protocol::{
    ClientMsg, LineReader, LiveFrame, PIPE_NAME, PROTOCOL_VERSION, ServerMsg, SessionInfo,
    decode_server, encode,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

/// Espera entre tentativas enquanto o núcleo não responde.
const RETRY: Duration = Duration::from_secs(1);
/// Espera por uma instância livre quando todas estão ocupadas.
const BUSY_WAIT_MS: u32 = 2_000;
/// Núcleo que não lê por este tempo derruba a conexão.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// `CREATE_NO_WINDOW` do Win32.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// O que a thread do cliente entrega à interface.
#[derive(Clone, Debug, PartialEq)]
pub enum UiUpdate {
    Connected { paused: bool, emulation: bool },
    Incompatible,
    Disconnected,
    Sessions(Vec<SessionInfo>),
    Live(LiveFrame),
    Notice(String),
}

/// Traduz uma mensagem do núcleo (o único lugar a mudar quando o protocolo
/// ganhar mensagens novas).
fn translate(msg: ServerMsg) -> UiUpdate {
    match msg {
        ServerMsg::Welcome {
            version: PROTOCOL_VERSION,
            paused,
            emulation,
        } => UiUpdate::Connected { paused, emulation },
        ServerMsg::Welcome { .. } | ServerMsg::Incompatible { .. } => UiUpdate::Incompatible,
        ServerMsg::Sessions { sessions } => UiUpdate::Sessions(sessions),
        ServerMsg::Live { frame } => UiUpdate::Live(frame),
        ServerMsg::Notice { text } => UiUpdate::Notice(text),
    }
}

/// Nome do pipe: `AXIS_PIPE_NAME` (só testes manuais a definem) ou o padrão.
pub fn pipe_name() -> String {
    resolve_pipe_name(std::env::var_os("AXIS_PIPE_NAME"))
}

fn resolve_pipe_name(var: Option<OsString>) -> String {
    match var.and_then(|v| v.into_string().ok()) {
        Some(name) if !name.is_empty() => name,
        _ => PIPE_NAME.to_string(),
    }
}

/// Inicia `axis.exe` (da pasta do `axis-ui.exe`) na bandeja, sem janela.
/// `false` se o arquivo não existe ou não pôde ser iniciado.
pub fn start_daemon() -> bool {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return false;
    };
    let exe = dir.join("axis.exe");
    if !exe.is_file() {
        return false;
    }
    Command::new(exe)
        .arg("--tray")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}

/// Conexão com o núcleo. Soltar o `Client` encerra as duas threads e fecha o pipe.
pub struct Client {
    tx: Sender<ClientMsg>,
}

impl Client {
    /// Conecta ao núcleo em segundo plano. `on_update` roda na thread do
    /// cliente (nunca na da interface).
    pub fn spawn(on_update: impl Fn(UiUpdate) + Send + 'static) -> Client {
        Client::spawn_named(pipe_name(), on_update)
    }

    /// Como [`Client::spawn`], com o nome do pipe explícito (os testes usam
    /// nomes únicos sem mexer em variáveis de ambiente do processo).
    fn spawn_named(name: String, on_update: impl Fn(UiUpdate) + Send + 'static) -> Client {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        std::thread::Builder::new()
            .name("axis-ui-cliente".into())
            .spawn(move || run(&wide(&name), &s, &on_update))
            .expect("criar a thread do cliente do núcleo");
        std::thread::Builder::new()
            .name("axis-ui-escritora".into())
            .spawn(move || write_loop(&shared, &rx))
            .expect("criar a thread de escrita do cliente");
        Client { tx }
    }

    /// Envia ao núcleo; desconectado, a mensagem é descartada.
    #[allow(dead_code)] // usado pelas telas de controle (próximas tarefas)
    pub fn send(&self, m: ClientMsg) {
        let _ = self.tx.send(m);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Estado dividido entre a thread do cliente e a escritora.
#[derive(Default)]
struct Shared {
    /// Conexão já cumprimentada, onde a escritora escreve.
    current: Mutex<Option<Arc<Conn>>>,
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Shared {
    /// O `Client` foi solto: encerra tudo, inclusive a leitura em curso.
    fn stop(&self) {
        *lock(&self.stopped) = true;
        self.wake.notify_all();
        if let Some(c) = lock(&self.current).as_ref() {
            c.close();
        }
    }

    fn is_stopped(&self) -> bool {
        *lock(&self.stopped)
    }

    /// Espera o intervalo entre tentativas; `true` se foi encerrado.
    fn wait_retry(&self) -> bool {
        let g = lock(&self.stopped);
        let (g, _) = self
            .wake
            .wait_timeout_while(g, RETRY, |stopped| !*stopped)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }

    /// Troca a conexão da escritora; `false` se já foi encerrado. Com `stop`
    /// gravando a flag antes de olhar `current`, um dos dois sempre fecha a
    /// conexão nova.
    fn set_current(&self, conn: Option<Arc<Conn>>) -> bool {
        *lock(&self.current) = conn;
        !self.is_stopped()
    }

    fn current(&self) -> Option<Arc<Conn>> {
        lock(&self.current).clone()
    }
}

/// O último estado de conexão avisado à interface.
#[derive(Clone, Copy, PartialEq)]
enum Reported {
    Connected,
    Incompatible,
    Disconnected,
}

fn run(name: &[u16], shared: &Shared, on_update: &dyn Fn(UiUpdate)) {
    let mut reported: Option<Reported> = None;
    loop {
        let down = match Conn::open(name) {
            Ok(conn) => {
                let conn = Arc::new(conn);
                session(&conn, shared, on_update, &mut reported);
                conn.close();
                shared.set_current(None);
                // A queda de uma conexão incompatível não é "núcleo parado".
                matches!(reported, None | Some(Reported::Connected))
            }
            Err(_) => reported != Some(Reported::Disconnected),
        };
        if shared.is_stopped() {
            return;
        }
        if down {
            reported = Some(Reported::Disconnected);
            on_update(UiUpdate::Disconnected);
        }
        if shared.wait_retry() {
            return;
        }
    }
}

/// Uma conexão do começo ao fim: `Hello`, depois lê até cair. Linha
/// malformada ou longa demais derruba a conexão.
fn session(
    conn: &Arc<Conn>,
    shared: &Shared,
    on_update: &dyn Fn(UiUpdate),
    reported: &mut Option<Reported>,
) {
    let hello = encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
    });
    let Ok(ev) = WinEvent::new() else { return };
    if conn.write_all(&ev, hello.as_bytes()).is_err() {
        return;
    }
    let Ok(reader) = PipeReader::new(conn.clone()) else {
        return;
    };
    if !shared.set_current(Some(conn.clone())) {
        return;
    }
    let mut lines = LineReader::new(reader);
    while let Ok(Some(line)) = lines.next_line() {
        let Ok(msg) = decode_server(&line) else {
            return;
        };
        let update = translate(msg);
        match update {
            UiUpdate::Connected { .. } => *reported = Some(Reported::Connected),
            UiUpdate::Incompatible => {
                *reported = Some(Reported::Incompatible);
                on_update(update);
                return;
            }
            _ => {}
        }
        on_update(update);
    }
}

/// Thread escritora: dura o `Client` inteiro; ao soltá-lo, encerra o resto.
fn write_loop(shared: &Shared, rx: &Receiver<ClientMsg>) {
    let mut ev: Option<WinEvent> = None;
    for msg in rx.iter() {
        let Some(conn) = shared.current() else {
            continue; // desconectado: descarta
        };
        if ev.is_none() {
            ev = WinEvent::new().ok();
        }
        let Some(ev) = &ev else { continue };
        if conn.write_all(ev, encode(&msg).as_bytes()).is_err() {
            // Derruba a conexão; a thread do cliente avisa e reconecta.
            conn.close();
        }
    }
    shared.stop();
}

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
struct WinEvent(OwnedHandle);

impl WinEvent {
    fn new() -> io::Result<WinEvent> {
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

fn closed_err() -> io::Error {
    io::Error::new(ErrorKind::ConnectionAborted, "conexão encerrada")
}

/// Uma ponta do pipe aberta em modo overlapped.
struct Conn {
    pipe: OwnedHandle,
    /// Ligado por `close`: aborta a operação em curso de qualquer thread.
    close_ev: WinEvent,
    closed: AtomicBool,
}

impl Conn {
    fn from_handle(h: HANDLE) -> io::Result<Conn> {
        // Assume o HANDLE já aqui: se o evento falhar, ele é fechado.
        let pipe = OwnedHandle(h);
        Ok(Conn {
            pipe,
            close_ev: WinEvent::new()?,
            closed: AtomicBool::new(false),
        })
    }

    /// Abre o pipe do núcleo; `Err(código)` do Win32 na falha. Com todas as
    /// instâncias ocupadas, espera uma livre (até 2 s) e tenta mais uma vez.
    fn open(name: &[u16]) -> Result<Conn, u32> {
        let mut waited = false;
        loop {
            // SAFETY: `name` termina em 0; sem compartilhamento, atributos ou
            // modelo. SQOS de identificação: o servidor não age como nós.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                return Conn::from_handle(h).map_err(|e| e.raw_os_error().unwrap_or(0) as u32);
            }
            // SAFETY: lido logo após a falha, na mesma thread.
            let e = unsafe { GetLastError() };
            if e != ERROR_PIPE_BUSY || waited {
                return Err(e);
            }
            waited = true;
            // SAFETY: `name` termina em 0.
            unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) };
        }
    }

    /// Encerra a conexão: aborta a operação em curso de qualquer thread.
    fn close(&self) {
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

    fn read(&self, ev: &WinEvent, buf: &mut [u8]) -> io::Result<usize> {
        let (h, len) = (self.pipe.0, buf.len().min(u32::MAX as usize) as u32);
        let ptr = buf.as_mut_ptr();
        // SAFETY: `buf` é emprestado por toda a chamada e `overlapped` só volta
        // depois que o kernel terminou de escrever nele.
        match self.overlapped(ev, None, |ov| unsafe {
            ReadFile(h, ptr, len, std::ptr::null_mut(), ov)
        }) {
            Ok(n) => Ok(n as usize),
            Err(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA) => Ok(0),
            Err(_) if self.is_closed() => Err(closed_err()),
            Err(e) => Err(io::Error::from_raw_os_error(e as i32)),
        }
    }

    fn write_all(&self, ev: &WinEvent, mut buf: &[u8]) -> io::Result<()> {
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
                Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
            };
            buf = &buf[n.min(buf.len())..];
        }
        Ok(())
    }
}

/// `Read` sobre uma `Conn`, com evento próprio (uma thread por leitor).
struct PipeReader {
    conn: Arc<Conn>,
    ev: WinEvent,
}

impl PipeReader {
    fn new(conn: Arc<Conn>) -> io::Result<PipeReader> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Instant;

    use axis_protocol::{Family, SessionInfo, decode_client};
    use windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    const RECV: Duration = Duration::from_secs(3);

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// Nome único por teste: nunca o pipe real do núcleo.
    fn test_pipe() -> String {
        format!(
            r"\\.\pipe\axis-ui-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        )
    }

    fn info() -> SessionInfo {
        SessionInfo {
            id: "aa:01".into(),
            owner: "Maria".into(),
            color: [1, 2, 3],
            family: Family::DualShock4,
            player: Some(1),
            connected: true,
            experimental: false,
            view_only: false,
            battery: Some(64),
            charging: false,
        }
    }

    fn welcome() -> ServerMsg {
        ServerMsg::Welcome {
            version: PROTOCOL_VERSION,
            paused: true,
            emulation: false,
        }
    }

    /// Servidor falso: uma instância do pipe, aberta em modo overlapped.
    struct FakeServer {
        conn: Arc<Conn>,
        ev: WinEvent,
        lines: Option<LineReader<PipeReader>>,
    }

    impl FakeServer {
        fn listen(name: &str) -> FakeServer {
            let name = wide(name);
            // SAFETY: `name` termina em 0; segurança padrão (usuário local).
            let h = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    65_536,
                    65_536,
                    0,
                    std::ptr::null(),
                )
            };
            assert_ne!(h, INVALID_HANDLE_VALUE, "criar o pipe de teste");
            FakeServer {
                conn: Arc::new(Conn::from_handle(h).unwrap()),
                ev: WinEvent::new().unwrap(),
                lines: None,
            }
        }

        /// Espera o cliente conectar e lê o `Hello`.
        fn accept(&mut self) {
            let h = self.conn.pipe.0;
            // SAFETY: HANDLE de servidor válido; `ov` vive até o fim da operação.
            match self.conn.overlapped(&self.ev, Some(RECV), |ov| unsafe {
                ConnectNamedPipe(h, ov)
            }) {
                Ok(_) | Err(ERROR_PIPE_CONNECTED) => {}
                Err(e) => panic!("cliente não conectou: {e}"),
            }
            self.lines = Some(LineReader::new(PipeReader::new(self.conn.clone()).unwrap()));
            assert_eq!(
                self.recv(),
                ClientMsg::Hello {
                    version: PROTOCOL_VERSION
                }
            );
        }

        fn recv(&mut self) -> ClientMsg {
            let line = self
                .lines
                .as_mut()
                .expect("accept antes")
                .next_line()
                .expect("ler do cliente")
                .expect("cliente fechou");
            decode_client(&line).expect("cliente mandou lixo")
        }

        fn send(&self, m: &ServerMsg) {
            self.send_raw(encode(m).as_bytes());
        }

        fn send_raw(&self, b: &[u8]) {
            self.conn
                .write_all(&self.ev, b)
                .expect("escrever no cliente");
        }

        /// `true` se o cliente fechou a conexão.
        fn sees_eof(&mut self) -> bool {
            matches!(
                self.lines.as_mut().expect("accept antes").next_line(),
                Ok(None)
            )
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.conn.close();
        }
    }

    fn client(name: &str) -> (Client, Receiver<UiUpdate>) {
        let (tx, rx) = mpsc::channel();
        let c = Client::spawn_named(name.to_string(), move |u| {
            let _ = tx.send(u);
        });
        (c, rx)
    }

    fn next(rx: &Receiver<UiUpdate>) -> UiUpdate {
        rx.recv_timeout(RECV).expect("nenhuma atualização a tempo")
    }

    #[test]
    fn client_connects_hello_and_receives_sessions() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let (c, rx) = client(&name);
        server.accept();
        server.send(&welcome());
        server.send(&ServerMsg::Sessions {
            sessions: vec![info()],
        });
        assert_eq!(
            next(&rx),
            UiUpdate::Connected {
                paused: true,
                emulation: false
            }
        );
        assert_eq!(next(&rx), UiUpdate::Sessions(vec![info()]));

        // A escritora manda enquanto a leitora está parada no ReadFile.
        let inspect = ClientMsg::Inspect {
            id: Some("aa:01".into()),
        };
        c.send(inspect.clone());
        assert_eq!(server.recv(), inspect);

        server.send(&ServerMsg::Notice {
            text: "olá".into()
        });
        assert_eq!(next(&rx), UiUpdate::Notice("olá".into()));
    }

    #[test]
    fn incompatible_server_is_reported() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let (_c, rx) = client(&name);
        server.accept();
        server.send(&ServerMsg::Incompatible { server_version: 9 });
        assert_eq!(next(&rx), UiUpdate::Incompatible);
        drop(server);
        // O fim da conexão incompatível não vira "núcleo parado" na hora.
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn server_gone_reports_disconnected_then_reconnects() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let (_c, rx) = client(&name);
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));

        drop(server);
        assert_eq!(next(&rx), UiUpdate::Disconnected);

        let mut server = FakeServer::listen(&name);
        let t0 = Instant::now();
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));
        assert!(t0.elapsed() <= Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn no_server_reports_disconnected_once() {
        let name = test_pipe();
        let (_c, rx) = client(&name);
        assert_eq!(next(&rx), UiUpdate::Disconnected);
        // Novas tentativas a cada 1 s, sem repetir o aviso.
        assert!(rx.recv_timeout(Duration::from_millis(1500)).is_err());
    }

    #[test]
    fn garbage_from_server_drops_and_reconnects() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let (_c, rx) = client(&name);
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));
        server.send_raw(b"{\"t\":\"explode\"}\n");
        assert_eq!(next(&rx), UiUpdate::Disconnected);
        assert!(server.sees_eof(), "o cliente larga a conexão");
        drop(server);

        let mut server = FakeServer::listen(&name);
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));
    }

    #[test]
    fn oversized_line_drops_connection() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let (_c, rx) = client(&name);
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));
        server.send_raw(&vec![b'x'; 70 * 1024]);
        assert_eq!(next(&rx), UiUpdate::Disconnected);
    }

    #[test]
    fn dropping_client_closes_pipe_and_ends_threads() {
        let name = test_pipe();
        let mut server = FakeServer::listen(&name);
        let token = Arc::new(());
        let (tx, rx) = mpsc::channel();
        let held = token.clone();
        let c = Client::spawn_named(name.clone(), move |u| {
            let _keep = &held;
            let _ = tx.send(u);
        });
        server.accept();
        server.send(&welcome());
        assert!(matches!(next(&rx), UiUpdate::Connected { .. }));

        drop(c);
        assert!(server.sees_eof(), "o pipe fecha");
        // A thread do cliente terminou: o callback (e o que ele guarda) foi solto.
        let deadline = Instant::now() + RECV;
        while Arc::strong_count(&token) > 1 {
            assert!(Instant::now() < deadline, "thread do cliente não terminou");
            std::thread::sleep(Duration::from_millis(5));
        }
        // E não houve "núcleo parado" falso no encerramento.
        assert!(rx.try_iter().all(|u| u != UiUpdate::Disconnected));
    }

    #[test]
    fn pipe_name_override() {
        assert_eq!(resolve_pipe_name(None), PIPE_NAME);
        assert_eq!(resolve_pipe_name(Some("".into())), PIPE_NAME);
        assert_eq!(
            resolve_pipe_name(Some(r"\\.\pipe\outro".into())),
            r"\\.\pipe\outro"
        );
    }
}
