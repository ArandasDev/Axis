//! Instância única do processo, via mutex nomeado do Windows.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

pub struct SingleInstance {
    handle: HANDLE,
}

// SAFETY: a mutex HANDLE is just a kernel object reference; it may be used and
// closed from any thread.
unsafe impl Send for SingleInstance {}

impl SingleInstance {
    /// `None` se outra instância já detém o nome (ou se o mutex não pôde ser criado).
    pub fn acquire(name: &str) -> Option<SingleInstance> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call;
        // null security attributes are allowed.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: reads the thread's last error right after CreateMutexW.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is a valid handle we own.
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(SingleInstance { handle })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: `handle` is a valid handle owned by this value, closed once.
        unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_first_is_dropped() {
        let name = format!("Local\\AxisTest-{}", std::process::id());
        let first = SingleInstance::acquire(&name);
        assert!(first.is_some());
        assert!(SingleInstance::acquire(&name).is_none());
        drop(first);
        assert!(SingleInstance::acquire(&name).is_some());
    }
}
