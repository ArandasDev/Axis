//! Início automático com o Windows (valor na chave Run do usuário).

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

pub struct RunKey {
    subkey: Vec<u16>,
    value: Vec<u16>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Handle de chave fechado no Drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by us and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

impl RunKey {
    pub fn user_run(value: &str) -> RunKey {
        RunKey::custom("Software\\Microsoft\\Windows\\CurrentVersion\\Run", value)
    }

    pub fn custom(subkey: &str, value: &str) -> RunKey {
        RunKey {
            subkey: wide(subkey),
            value: wide(value),
        }
    }

    fn open(&self, access: u32) -> Result<Key, u32> {
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: subkey is NUL-terminated; `hkey` is a valid out pointer.
        let code = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                self.subkey.as_ptr(),
                0,
                access,
                &mut hkey,
            )
        };
        if code == ERROR_SUCCESS {
            Ok(Key(hkey))
        } else {
            Err(code)
        }
    }

    pub fn is_enabled(&self) -> bool {
        let Ok(key) = self.open(KEY_QUERY_VALUE) else {
            return false;
        };
        let mut kind = 0u32;
        let mut size = 0u32;
        // SAFETY: value is NUL-terminated; a null data pointer only queries type/size.
        let code = unsafe {
            RegQueryValueExW(
                key.0,
                self.value.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            )
        };
        code == ERROR_SUCCESS && kind == REG_SZ
    }

    pub fn enable(&self, exe: &Path) -> io::Result<()> {
        let mut mkey: HKEY = std::ptr::null_mut();
        // SAFETY: subkey is NUL-terminated; null class/security/disposition are allowed.
        check(unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                self.subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut mkey,
                std::ptr::null_mut(),
            )
        })?;
        let key = Key(mkey);

        let mut data: Vec<u16> = "\"".encode_utf16().collect();
        data.extend(exe.as_os_str().encode_wide());
        data.extend("\" --tray".encode_utf16());
        data.push(0);
        let bytes = (data.len() * 2) as u32;
        // SAFETY: `data` holds `bytes` valid bytes of NUL-terminated UTF-16.
        check(unsafe {
            RegSetValueExW(
                key.0,
                self.value.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                bytes,
            )
        })
    }

    /// Remove o valor; se ele (ou a chave) não existe, é sucesso.
    pub fn disable(&self) -> io::Result<()> {
        let key = match self.open(KEY_SET_VALUE) {
            Ok(k) => k,
            Err(ERROR_FILE_NOT_FOUND) => return Ok(()),
            Err(code) => return check(code),
        };
        // SAFETY: value is NUL-terminated.
        match unsafe { RegDeleteValueW(key.0, self.value.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => Ok(()),
            code => check(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cleanup(Vec<u16>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Registry::RegDeleteTreeW;
            // SAFETY: the subkey is a NUL-terminated UTF-16 string owned by self.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, self.0.as_ptr()) };
        }
    }

    #[test]
    fn run_key_enable_disable_round_trip() {
        let sub = format!("Software\\AxisTest-{}", std::process::id());
        let key = RunKey::custom(&sub, "Axis");
        let _guard = Cleanup(key.subkey.clone());
        assert!(!key.is_enabled());
        key.enable(Path::new("C:\\Apps\\axis.exe")).unwrap();
        assert!(key.is_enabled());
        key.disable().unwrap();
        assert!(!key.is_enabled());
        key.disable().unwrap();
    }
}
