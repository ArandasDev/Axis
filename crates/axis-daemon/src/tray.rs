//! Ícone na bandeja do Windows: menu de contexto e notificações em balão.
//!
//! Uma thread própria é dona de uma janela de nível superior oculta (nunca exibida)
//! e do ícone (`Shell_NotifyIconW`). Não é uma janela só de mensagens porque esta
//! precisa receber difusões (`TaskbarCreated`, fim de sessão) e conseguir o
//! primeiro plano para o menu fechar direito. Tudo chega por mensagem de janela:
//! o clique do ícone (botão direito: menu; duplo clique: abrir a interface), as
//! notificações (`WM_APP + 2`) e o fechamento interno (`WM_APP + 3`). Um
//! `WM_CLOSE` vindo de fora (ex.: `taskkill` sem `/F`) vira
//! `Quit` no gerente; a janela só é destruída quando `main` fecha a bandeja.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CS_DBLCLKS, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW,
    GetWindowLongPtrW, IDI_APPLICATION, LoadIconW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    MSG, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
    SetForegroundWindow, SetMenuDefaultItem, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

use crate::manager::SessionView;
use crate::ports::Notifier;

/// Mensagem de retorno do ícone (cliques).
const WM_TRAY: u32 = WM_APP + 1;
/// Notificação pendente: `lParam` é um `Box<(String, String)>` (título, corpo).
const WM_NOTIFY_BALLOON: u32 = WM_APP + 2;
/// Fechamento pedido por `TrayHandle::close` (depois que o gerente terminou).
const WM_TRAY_CLOSE: u32 = WM_APP + 3;
const ICON_ID: u32 = 1;
/// Novas tentativas de `NIM_ADD` na criação (o Explorer pode ainda não estar pronto no logon).
const ICON_ADD_RETRIES: u32 = 5;
const ICON_ADD_INTERVAL: Duration = Duration::from_millis(500);
/// Quanto o fim de sessão espera o gerente terminar o `Quit`.
const ENDSESSION_WAIT: Duration = Duration::from_secs(5);

/// Sinal "o gerente terminou o `Quit`": `main` marca depois que `handle(Quit)`
/// retorna; a bandeja espera por ele no fim da sessão do Windows.
pub type QuitDone = Arc<(Mutex<bool>, Condvar)>;

/// Marca o `Quit` como concluído e acorda quem espera.
pub fn signal_quit_done(done: &QuitDone) {
    let (lock, cvar) = &**done;
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
    cvar.notify_all();
}

/// Espera o sinal por até `timeout`; devolve se ele chegou.
pub(crate) fn wait_quit_done(done: &QuitDone, timeout: Duration) -> bool {
    let (lock, cvar) = &**done;
    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let (guard, _) = cvar
        .wait_timeout_while(guard, timeout, |done| !*done)
        .unwrap_or_else(|e| e.into_inner());
    *guard
}

/// `WM_ENDSESSION`: `wParam != 0` significa que a sessão vai mesmo terminar.
pub(crate) fn endsession_should_quit(wparam: usize) -> bool {
    wparam != 0
}

pub(crate) const ID_PAUSE: u32 = 1;
pub(crate) const ID_RESTORE: u32 = 2;
pub(crate) const ID_AUTOSTART: u32 = 3;
pub(crate) const ID_QUIT: u32 = 4;
pub(crate) const ID_OPEN: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Pause,
    Resume,
    Restore,
    ToggleAutostart,
    Quit,
    /// Abrir a interface (ou trazê-la para frente, se já estiver aberta).
    OpenUi,
}

/// Linha do menu de contexto (independente do Win32, para poder testar).
#[derive(Debug, PartialEq)]
pub(crate) enum MenuItem {
    Separator,
    Entry {
        label: String,
        /// `0` = linha informativa, desabilitada.
        id: u32,
        checked: bool,
        /// Item padrão do menu (em negrito, `MFS_DEFAULT`).
        default: bool,
    },
}

/// O que um evento do mouse no ícone da bandeja faz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClickAction {
    /// Mostra o menu de contexto.
    Menu,
    /// Abre a interface (o mesmo que o item padrão do menu).
    Open,
}

/// Traduz o evento do mouse recebido em `WM_TRAY` (`lParam` baixo).
pub(crate) fn click_action(event: u32) -> Option<ClickAction> {
    match event {
        WM_RBUTTONUP => Some(ClickAction::Menu),
        WM_LBUTTONDBLCLK => Some(ClickAction::Open),
        _ => None,
    }
}

/// Texto da linha de uma sessão no menu.
pub fn session_label(s: &SessionView) -> String {
    let state = if !s.connected {
        "reconectando…".to_string()
    } else {
        match s.player {
            Some(n) if !s.view_only => format!("Player {n}"),
            _ => "só visualização".to_string(),
        }
    };
    format!("{} — {state}", s.owner)
}

pub(crate) fn menu_items(sessions: &[SessionView], paused: bool, autostart: bool) -> Vec<MenuItem> {
    let info = |label: String| MenuItem::Entry {
        label,
        id: 0,
        checked: false,
        default: false,
    };
    let action = |label: &str, id: u32, checked: bool| MenuItem::Entry {
        label: label.to_string(),
        id,
        checked,
        default: false,
    };
    let mut items = vec![
        info("Axis".to_string()),
        MenuItem::Entry {
            label: "Abrir o Axis".to_string(),
            id: ID_OPEN,
            checked: false,
            default: true,
        },
    ];
    items.extend(sessions.iter().map(|s| info(session_label(s))));
    items.push(MenuItem::Separator);
    let pause = if paused {
        "Retomar emulação"
    } else {
        "Pausar emulação"
    };
    items.push(action(pause, ID_PAUSE, false));
    items.push(action("Restaurar controles", ID_RESTORE, false));
    items.push(action("Iniciar com o Windows", ID_AUTOSTART, autostart));
    items.push(MenuItem::Separator);
    items.push(action("Sair", ID_QUIT, false));
    items
}

pub(crate) fn command_for(id: u32, paused: bool) -> Option<TrayCommand> {
    match id {
        ID_PAUSE if paused => Some(TrayCommand::Resume),
        ID_PAUSE => Some(TrayCommand::Pause),
        ID_RESTORE => Some(TrayCommand::Restore),
        ID_AUTOSTART => Some(TrayCommand::ToggleAutostart),
        ID_QUIT => Some(TrayCommand::Quit),
        ID_OPEN => Some(TrayCommand::OpenUi),
        _ => None,
    }
}

/// Copia `s` para `dst` como UTF-16 terminado em zero, truncando sem partir
/// pares substitutos. O restante do buffer é zerado.
pub(crate) fn fill_wide(dst: &mut [u16], s: &str) {
    dst.fill(0);
    let Some(room) = dst.len().checked_sub(1) else {
        return;
    };
    let mut n = 0;
    for c in s.chars() {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        if n + units.len() > room {
            break;
        }
        dst[n..n + units.len()].copy_from_slice(units);
        n += units.len();
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Alça da bandeja: pode ser clonada e enviada entre threads.
#[derive(Clone, Debug)]
pub struct TrayHandle {
    /// `HWND` da janela da bandeja (0 se ela não pôde ser criada).
    hwnd: isize,
}

impl TrayHandle {
    fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }

    /// Pede o fechamento da bandeja; a thread termina em seguida.
    pub fn close(&self) {
        if self.hwnd == 0 {
            return;
        }
        // SAFETY: PostMessageW aceita qualquer HWND; se a janela não existe mais,
        // a chamada só falha.
        unsafe { PostMessageW(self.hwnd(), WM_TRAY_CLOSE, 0, 0) };
    }
}

impl Notifier for TrayHandle {
    fn notify(&self, title: &str, body: &str) {
        log::info!("notificação: {body}");
        if self.hwnd == 0 {
            return;
        }
        let msg = Box::into_raw(Box::new((title.to_string(), body.to_string())));
        // SAFETY: a janela retoma a posse da caixa ao tratar WM_NOTIFY_BALLOON.
        let posted = unsafe { PostMessageW(self.hwnd(), WM_NOTIFY_BALLOON, 0, msg as LPARAM) };
        if posted == 0 {
            // SAFETY: a mensagem não foi postada, então ninguém mais tem o ponteiro.
            drop(unsafe { Box::from_raw(msg) });
        }
    }
}

/// Estado da janela, apontado por `GWLP_USERDATA`.
struct TrayState {
    on_command: Box<dyn Fn(TrayCommand)>,
    sessions: Arc<Mutex<Vec<SessionView>>>,
    autostart: Arc<AtomicBool>,
    paused: Cell<bool>,
    quit_done: QuitDone,
    /// Mensagem `TaskbarCreated` (0 se não pôde ser registrada).
    taskbar_created: u32,
}

pub struct Tray;

impl Tray {
    /// Cria a bandeja numa thread própria. Se a janela não puder ser criada, o
    /// erro vai para o log e `on_command(Quit)` é chamado (sem bandeja não há
    /// como controlar nem encerrar o Axis).
    pub fn spawn(
        on_command: impl Fn(TrayCommand) + Send + 'static,
        sessions: Arc<Mutex<Vec<SessionView>>>,
        autostart: Arc<AtomicBool>,
        quit_done: QuitDone,
    ) -> (TrayHandle, JoinHandle<()>) {
        let (ready_tx, ready_rx) = mpsc::channel::<isize>();
        let join = std::thread::Builder::new()
            .name("axis-tray".into())
            .spawn(move || {
                let state = Box::new(TrayState {
                    on_command: Box::new(on_command),
                    sessions,
                    autostart,
                    paused: Cell::new(false),
                    quit_done,
                    // SAFETY: string terminada em zero, viva durante a chamada.
                    taskbar_created: unsafe {
                        RegisterWindowMessageW(wide("TaskbarCreated").as_ptr())
                    },
                });
                run(state, ready_tx);
            })
            .expect("não foi possível criar a thread da bandeja");
        let hwnd = ready_rx.recv().unwrap_or(0);
        (TrayHandle { hwnd }, join)
    }
}

fn run(state: Box<TrayState>, ready: mpsc::Sender<isize>) {
    let class = wide("AxisTrayWindow");
    // SAFETY: chamadas Win32 com ponteiros para buffers vivos durante a chamada.
    let (hwnd, ptr) = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            // Duplo clique no ícone (o Shell repassa WM_LBUTTONDBLCLK em WM_TRAY).
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassExW(&wc);
        let ptr = Box::into_raw(state);
        // Nível superior (sem pai) e nunca exibida: não há ShowWindow.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            ptr.cast(),
        );
        if hwnd.is_null() {
            let state = Box::from_raw(ptr);
            log::error!(
                "não foi possível criar a janela da bandeja: {}",
                std::io::Error::last_os_error()
            );
            let _ = ready.send(0);
            let _ = catch_unwind(AssertUnwindSafe(|| (state.on_command)(TrayCommand::Quit)));
            return;
        }
        (hwnd, ptr)
    };

    let _ = ready.send(hwnd as isize);
    let mut added = add_icon(hwnd);
    for attempt in 1..=ICON_ADD_RETRIES {
        if added {
            break;
        }
        log::warn!("não foi possível adicionar o ícone à bandeja (tentativa {attempt})");
        std::thread::sleep(ICON_ADD_INTERVAL);
        added = add_icon(hwnd);
    }
    if !added {
        log::error!("ícone da bandeja não adicionado; aguardando o Explorer (TaskbarCreated)");
    }

    // SAFETY: laço de mensagens padrão; `msg` é um buffer local.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // A janela já foi destruída (WM_DESTROY → PostQuitMessage): nada mais usa o estado.
        drop(Box::from_raw(ptr));
    }
    log::info!("bandeja encerrada");
}

/// `NIM_ADD` do ícone desta janela; devolve se deu certo.
fn add_icon(hwnd: HWND) -> bool {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    fill_wide(&mut nid.szTip, "Axis");
    // SAFETY: ícone padrão do sistema; `nid` é uma estrutura local válida.
    unsafe {
        nid.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
        Shell_NotifyIconW(NIM_ADD, &nid) != 0
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: em WM_NCCREATE, lParam aponta para o CREATESTRUCTW da criação,
        // cujo lpCreateParams é o `*mut TrayState` passado a CreateWindowExW.
        unsafe {
            let cs = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    // SAFETY: GWLP_USERDATA é 0 ou o `TrayState` desta janela, que vive até o fim do laço.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState).as_ref() };
    let Some(state) = state else {
        // SAFETY: repasse padrão.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_TRAY => {
            match click_action((lparam as u32) & 0xFFFF) {
                Some(ClickAction::Menu) => show_menu(hwnd, state),
                Some(ClickAction::Open) => run_command(state, ID_OPEN),
                None => {}
            }
            0
        }
        // Intencional: os ids do menu também são aceitos via `WM_COMMAND` (LOWORD
        // de wParam), para controle por script (ex.: fumaça sem clique).
        WM_COMMAND => {
            run_command(state, (wparam & 0xFFFF) as u32);
            0
        }
        WM_NOTIFY_BALLOON => {
            // SAFETY: o ponteiro veio de Box::into_raw em `TrayHandle::notify`, e
            // cada mensagem postada é tratada uma única vez.
            let boxed = unsafe { Box::from_raw(lparam as *mut (String, String)) };
            show_balloon(hwnd, &boxed.0, &boxed.1);
            0
        }
        m if m != 0 && m == state.taskbar_created => {
            // O Explorer reiniciou: o ícone sumiu e precisa ser adicionado de novo.
            if !add_icon(hwnd) {
                log::warn!("não foi possível recriar o ícone da bandeja");
            }
            0
        }
        WM_CLOSE => {
            // Fechamento vindo de fora: encerra o gerente, que depois fecha a
            // bandeja por `WM_TRAY_CLOSE`. Não destrói a janela aqui.
            log::info!("pedido de fechamento da janela: encerrando");
            if catch_unwind(AssertUnwindSafe(|| (state.on_command)(TrayCommand::Quit))).is_err() {
                log::error!("pânico ao pedir o encerramento pelo fechamento da janela");
            }
            0
        }
        WM_TRAY_CLOSE => {
            // SAFETY: destrói a janela desta thread (dona dela); WM_DESTROY encerra o laço.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if endsession_should_quit(wparam) {
                log::info!("fim da sessão do Windows: encerrando");
                let quit = AssertUnwindSafe(|| (state.on_command)(TrayCommand::Quit));
                if catch_unwind(quit).is_err() {
                    log::error!("pânico ao pedir o encerramento no fim da sessão");
                }
                if !wait_quit_done(&state.quit_done, ENDSESSION_WAIT) {
                    log::warn!("o encerramento não terminou a tempo do fim da sessão");
                }
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: remove o ícone desta janela e encerra o laço desta thread.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: repasse padrão.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_balloon(hwnd: HWND, title: &str, body: &str) {
    let mut nid = icon_data(hwnd);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    fill_wide(&mut nid.szInfoTitle, title);
    fill_wide(&mut nid.szInfo, body);
    // SAFETY: `nid` é uma estrutura local válida para o ícone desta janela.
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

fn show_menu(hwnd: HWND, state: &TrayState) {
    let sessions = state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let paused = state.paused.get();
    let items = menu_items(&sessions, paused, state.autostart.load(Ordering::SeqCst));
    // SAFETY: o menu é criado, usado e destruído nesta função; os textos vivem
    // até o fim do AppendMenuW (o menu copia as strings).
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for item in &items {
            match item {
                MenuItem::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                }
                MenuItem::Entry {
                    label,
                    id,
                    checked,
                    default,
                } => {
                    let mut flags = MF_STRING;
                    if *id == 0 {
                        flags |= MF_GRAYED;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    let text = wide(label);
                    AppendMenuW(menu, flags, *id as usize, text.as_ptr());
                    if *default {
                        // Negrito (MFS_DEFAULT); 0 = `id` é um identificador, não posição.
                        SetMenuDefaultItem(menu, *id, 0);
                    }
                }
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        // Recomendado após TrackPopupMenu para o menu fechar corretamente.
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen
    };
    run_command(state, chosen as u32);
}

/// Executa o item `id` do menu (escolhido no menu ou recebido em `WM_COMMAND`).
fn run_command(state: &TrayState, id: u32) {
    let Some(cmd) = command_for(id, state.paused.get()) else {
        return;
    };
    match cmd {
        TrayCommand::Pause => state.paused.set(true),
        TrayCommand::Resume => state.paused.set(false),
        _ => {}
    }
    if catch_unwind(AssertUnwindSafe(|| (state.on_command)(cmd))).is_err() {
        log::error!("pânico ao tratar o comando {cmd:?} da bandeja");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axis_core::DeviceFamily;
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_LBUTTONUP, WM_MOUSEMOVE};

    fn view(player: Option<u8>, connected: bool, view_only: bool) -> SessionView {
        SessionView {
            id: "ds4-1".into(),
            owner: "Maria".into(),
            color: [0, 255, 0],
            family: DeviceFamily::DualShock4,
            player,
            connected,
            experimental: false,
            view_only,
        }
    }

    #[test]
    fn session_labels() {
        assert_eq!(
            session_label(&view(Some(1), true, false)),
            "Maria — Player 1"
        );
        assert_eq!(
            session_label(&view(Some(1), false, false)),
            "Maria — reconectando…"
        );
        assert_eq!(
            session_label(&view(None, true, true)),
            "Maria — só visualização"
        );
    }

    #[test]
    fn menu_layout_follows_state() {
        let items = menu_items(&[view(Some(2), true, false)], false, true);
        let labels: Vec<String> = items
            .iter()
            .map(|i| match i {
                MenuItem::Separator => "-".to_string(),
                MenuItem::Entry { label, .. } => label.clone(),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "Axis",
                "Abrir o Axis",
                "Maria — Player 2",
                "-",
                "Pausar emulação",
                "Restaurar controles",
                "Iniciar com o Windows",
                "-",
                "Sair"
            ]
        );
        let paused = menu_items(&[], true, false);
        assert!(
            paused
                .iter()
                .any(|i| matches!(i, MenuItem::Entry { label, .. } if label == "Retomar emulação"))
        );
        assert!(paused.iter().any(|i| matches!(i, MenuItem::Entry { label, checked: false, .. } if label == "Iniciar com o Windows")));
        assert!(items.iter().any(|i| matches!(i, MenuItem::Entry { label, checked: true, .. } if label == "Iniciar com o Windows")));
    }

    #[test]
    fn menu_items_include_open_first() {
        for sessions in [vec![], vec![view(Some(1), true, false)]] {
            let items = menu_items(&sessions, false, false);
            assert!(matches!(&items[0], MenuItem::Entry { label, id: 0, .. } if label == "Axis"));
            assert_eq!(
                items[1],
                MenuItem::Entry {
                    label: "Abrir o Axis".into(),
                    id: ID_OPEN,
                    checked: false,
                    default: true,
                }
            );
            // Só a abertura é o item padrão (negrito).
            let defaults = items
                .iter()
                .filter(|i| matches!(i, MenuItem::Entry { default: true, .. }))
                .count();
            assert_eq!(defaults, 1);
        }
    }

    #[test]
    fn double_click_maps_to_open() {
        assert_eq!(click_action(WM_LBUTTONDBLCLK), Some(ClickAction::Open));
        assert_eq!(click_action(WM_RBUTTONUP), Some(ClickAction::Menu));
        assert_eq!(click_action(WM_LBUTTONUP), None);
        assert_eq!(click_action(WM_MOUSEMOVE), None);
    }

    #[test]
    fn command_ids_map_with_pause_state() {
        assert_eq!(command_for(ID_OPEN, false), Some(TrayCommand::OpenUi));
        assert_eq!(command_for(ID_OPEN, true), Some(TrayCommand::OpenUi));
        assert_eq!(command_for(ID_PAUSE, false), Some(TrayCommand::Pause));
        assert_eq!(command_for(ID_PAUSE, true), Some(TrayCommand::Resume));
        assert_eq!(command_for(ID_RESTORE, false), Some(TrayCommand::Restore));
        assert_eq!(
            command_for(ID_AUTOSTART, false),
            Some(TrayCommand::ToggleAutostart)
        );
        assert_eq!(command_for(ID_QUIT, false), Some(TrayCommand::Quit));
        assert_eq!(command_for(0, false), None);
    }

    #[test]
    fn endsession_quits_only_when_the_session_really_ends() {
        assert!(endsession_should_quit(1));
        assert!(!endsession_should_quit(0));
    }

    #[test]
    fn quit_done_wait_returns_after_signal_and_times_out_without_it() {
        let done = QuitDone::default();
        assert!(!wait_quit_done(&done, Duration::from_millis(20)));
        let other = done.clone();
        let t = std::thread::spawn(move || signal_quit_done(&other));
        assert!(wait_quit_done(&done, Duration::from_secs(5)));
        t.join().unwrap();
    }

    #[test]
    fn fill_wide_truncates_without_splitting_surrogates() {
        let mut buf = [0xFFFFu16; 4];
        fill_wide(&mut buf, "abcdef");
        assert_eq!(buf, [b'a' as u16, b'b' as u16, b'c' as u16, 0]);
        let mut buf = [0xFFFFu16; 4];
        fill_wide(&mut buf, "ab😀");
        assert_eq!(buf, [b'a' as u16, b'b' as u16, 0, 0]);
    }
}
