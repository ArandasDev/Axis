//! Instância única da interface: a primeira cria o mutex `Local\AxisUi`; uma
//! segunda (aberta pela bandeja ou pelo usuário) acha a janela da primeira —
//! nível superior, visível, título "Axis" e de um processo `axis-ui.exe` —,
//! restaura se estiver minimizada, traz para frente e sai.
//!
//! Quem abre a segunda é o processo em primeiro plano (Explorer) ou o núcleo,
//! que chama `AllowSetForegroundWindow(ASFW_ANY)` antes; por isso o
//! `SetForegroundWindow` daqui é aceito pelo Windows.

use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};

/// Nome do mutex da interface.
pub const MUTEX_NAME: &str = "Local\\AxisUi";
/// Título da janela (o mesmo de `app.slint`).
const WINDOW_TITLE: &str = "Axis";
/// Nome do executável da interface.
const UI_IMAGE: &str = "axis-ui.exe";
/// A primeira instância pode ainda estar criando a janela: tentativas de achá-la.
const FIND_ATTEMPTS: u32 = 10;
const FIND_INTERVAL: Duration = Duration::from_millis(100);

/// Posse do mutex da interface (solto ao sair).
pub struct SingleUi {
    /// Nulo se o mutex não pôde ser criado (a interface abre mesmo assim).
    handle: HANDLE,
}

impl SingleUi {
    /// `None` se outra interface já está aberta.
    pub fn acquire() -> Option<SingleUi> {
        Self::acquire_named(MUTEX_NAME)
    }

    fn acquire_named(name: &str) -> Option<SingleUi> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` é UTF-16 terminado em zero e vive durante a chamada;
        // atributos de segurança nulos são permitidos.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        // SAFETY: lê o último erro da thread logo após CreateMutexW.
        if !handle.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` é válido e nosso.
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(SingleUi { handle })
    }
}

impl Drop for SingleUi {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` é válido, deste valor, e é fechado uma vez.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// O caminho é de um `axis-ui.exe` (sem diferenciar maiúsculas).
fn is_ui_image(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(UI_IMAGE))
}

/// Traz para frente a janela da interface já aberta (restaurando se estiver
/// minimizada). Devolve se a achou.
pub fn focus_existing() -> bool {
    for attempt in 0..FIND_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(FIND_INTERVAL);
        }
        if let Some(hwnd) = find_ui_window() {
            // SAFETY: `hwnd` veio de EnumWindows; se a janela sumiu, as
            // chamadas só falham.
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
            }
            return true;
        }
    }
    false
}

fn find_ui_window() -> Option<HWND> {
    let mut found: HWND = std::ptr::null_mut();
    // SAFETY: o callback só usa `lparam` como `*mut HWND` para `found`, que
    // vive até EnumWindows retornar.
    unsafe { EnumWindows(Some(enum_proc), &mut found as *mut HWND as LPARAM) };
    (!found.is_null()).then_some(found)
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    if is_ui_window(hwnd) {
        // SAFETY: `lparam` é o `*mut HWND` de `find_ui_window`.
        unsafe { *(lparam as *mut HWND) = hwnd };
        return 0; // para a enumeração
    }
    1
}

/// Janela visível, com o título da interface, de outro processo `axis-ui.exe`.
fn is_ui_window(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` veio de EnumWindows; os buffers são locais e os tamanhos
    // passados correspondem a eles; o handle do processo é fechado aqui.
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return false;
        }
        let mut title = [0u16; 16];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        if n <= 0 || String::from_utf16_lossy(&title[..n as usize]) != WINDOW_TITLE {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == GetCurrentProcessId() {
            return false;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len);
        CloseHandle(process);
        ok != 0
            && is_ui_image(&String::from_utf16_lossy(
                &path[..(len as usize).min(path.len())],
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_first_is_dropped() {
        let name = format!("Local\\AxisUiTest-{}", std::process::id());
        let first = SingleUi::acquire_named(&name);
        assert!(first.is_some());
        assert!(SingleUi::acquire_named(&name).is_none());
        drop(first);
        assert!(SingleUi::acquire_named(&name).is_some());
    }

    #[test]
    fn only_axis_ui_images_count() {
        assert!(is_ui_image(r"C:\Program Files\Axis\axis-ui.exe"));
        assert!(is_ui_image(r"C:\dev\AXIS-UI.EXE"));
        assert!(!is_ui_image(r"C:\Program Files\Axis\axis.exe"));
        assert!(!is_ui_image(r"C:\x\not-axis-ui.exe"));
        assert!(!is_ui_image(""));
    }
}
