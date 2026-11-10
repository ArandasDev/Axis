//! HidHide client (driver control device `\\.\HidHide`) used to hide physical
//! controllers from other applications while Axis exposes a virtual one.
//! Lists are UTF-16 multi-strings (terminated by two NULs).

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, QueryDosDeviceW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// Errors from the HidHide driver.
#[derive(Debug, thiserror::Error)]
pub enum HideError {
    #[error("HidHide não encontrado")]
    NotInstalled,
    #[error("erro do HidHide: código {0}")]
    Win32(u32),
}

/// Hides physical devices from other applications.
pub trait DeviceHider: Send + Sync {
    /// Hides `instance_id`. On failure the implementation makes a best-effort
    /// attempt to undo its blacklist change, but callers should record the ID
    /// BEFORE calling and include it in `unhide_all` regardless of the result.
    fn hide(&self, instance_id: &str) -> Result<(), HideError>;
    fn unhide(&self, instance_id: &str) -> Result<(), HideError>;
    /// Unhides all given IDs; if the blacklist ends up empty, also removes
    /// this process from the whitelist and deactivates HidHide.
    fn unhide_all(&self, instance_ids: &[String]) -> Result<(), HideError>;
}

/// `CTL_CODE(0x8001, function, METHOD_BUFFERED, FILE_READ_DATA)`.
pub(crate) const fn ctl_code(function: u32) -> u32 {
    (0x8001 << 16) | (1 << 14) | (function << 2)
}

const GET_WHITELIST: u32 = ctl_code(2048);
const SET_WHITELIST: u32 = ctl_code(2049);
const GET_BLACKLIST: u32 = ctl_code(2050);
const SET_BLACKLIST: u32 = ctl_code(2051);
const SET_ACTIVE: u32 = ctl_code(2053);

/// Encodes a UTF-16LE multi-string (each item NUL-terminated, plus a final NUL).
pub(crate) fn encode_multi_sz(items: &[String]) -> Vec<u8> {
    let mut words: Vec<u16> = Vec::new();
    for s in items {
        words.extend(s.encode_utf16());
        words.push(0);
    }
    words.push(0);
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Decodes a UTF-16LE multi-string. Tolerates a trailing odd byte and a
/// missing terminator; never panics.
pub(crate) fn decode_multi_sz(bytes: &[u8]) -> Vec<String> {
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    words
        .split(|&w| w == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// `\\?\HID#VID_054C&PID_05C4#7&abc&0&0000#{guid}` -> `HID\VID_054C&PID_05C4\7&ABC&0&0000`.
pub fn instance_id_from_path(path: &str) -> String {
    let p = path.trim_start_matches(r"\\?\");
    let p = match p.rfind("#{") {
        Some(i) => &p[..i],
        None => p,
    };
    p.replace('#', "\\").to_uppercase()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// RAII wrapper closing the device handle.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a successful CreateFileW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_device() -> Result<Handle, HideError> {
    let name = wide(r"\\.\HidHide");
    // SAFETY: `name` is a valid NUL-terminated UTF-16 string that outlives the call.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: trivial FFI call reading the thread's last error.
        let e = unsafe { GetLastError() };
        Err(if e == ERROR_FILE_NOT_FOUND {
            HideError::NotInstalled
        } else {
            HideError::Win32(e)
        })
    } else {
        Ok(Handle(h))
    }
}

fn ioctl(h: &Handle, code: u32, input: &[u8], output: &mut [u8]) -> Result<u32, HideError> {
    let mut returned = 0u32;
    // SAFETY: pointers/lengths come from live slices (null when empty); `returned` is a valid out pointer.
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                std::ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: trivial FFI call reading the thread's last error.
        Err(HideError::Win32(unsafe { GetLastError() }))
    } else {
        Ok(returned)
    }
}

fn get_list(h: &Handle, code: u32) -> Result<Vec<String>, HideError> {
    // Zero-size probe returns the required size.
    let needed = ioctl(h, code, &[], &mut [])? as usize;
    let mut buf = vec![0u8; needed.max(2)];
    let n = ioctl(h, code, &[], &mut buf)? as usize;
    Ok(decode_multi_sz(&buf[..n.min(buf.len())]))
}

fn set_list(h: &Handle, code: u32, items: &[String]) -> Result<(), HideError> {
    ioctl(h, code, &encode_multi_sz(items), &mut []).map(|_| ())
}

fn set_active(h: &Handle, active: bool) -> Result<(), HideError> {
    ioctl(h, SET_ACTIVE, &[u8::from(active)], &mut []).map(|_| ())
}

/// `C:\dir\app.exe` -> `\Device\HarddiskVolume3\dir\app.exe` (the form HidHide expects).
/// Falls back to the original string when there is no drive letter.
fn dos_device_path(path: &Path) -> String {
    let s = path
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return s;
    }
    let drive = wide(&s[..2]);
    let mut target = [0u16; 512];
    // SAFETY: `drive` is NUL-terminated; `target` is a valid buffer of the stated length.
    let n = unsafe { QueryDosDeviceW(drive.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
    if n == 0 {
        return s;
    }
    let end = target.iter().position(|&c| c == 0).unwrap_or(0);
    format!("{}{}", String::from_utf16_lossy(&target[..end]), &s[2..])
}

/// HidHide client. Holds no handle: the device is opened per operation.
///
/// Operations are serialized by an internal lock, so concurrent calls on one
/// instance cannot interleave their read-modify-write cycles. Races with
/// OTHER HidHide client processes cannot be prevented.
pub struct HidHide {
    self_image: String,
    lock: Mutex<()>,
}

impl HidHide {
    /// Verifies that `\\.\HidHide` opens and records `self_exe` in NT device form.
    pub fn open(self_exe: &Path) -> Result<HidHide, HideError> {
        drop(open_device()?);
        Ok(HidHide {
            self_image: dos_device_path(self_exe),
            lock: Mutex::new(()),
        })
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn contains(items: &[String], item: &str) -> bool {
    items.iter().any(|i| i.eq_ignore_ascii_case(item))
}

/// Returns `items` without entries equal (case-insensitively) to any of `remove`.
fn without(items: &[String], remove: &[&str]) -> Vec<String> {
    items
        .iter()
        .filter(|i| !remove.iter().any(|r| i.eq_ignore_ascii_case(r)))
        .cloned()
        .collect()
}

/// Plans `hide`: returns the new (blacklist, whitelist), without duplicates.
pub(crate) fn plan_hide(
    bl: &[String],
    wl: &[String],
    id: &str,
    me: &str,
) -> (Vec<String>, Vec<String>) {
    let mut bl = bl.to_vec();
    let mut wl = wl.to_vec();
    if !contains(&bl, id) {
        bl.push(id.to_string());
    }
    if !contains(&wl, me) {
        wl.push(me.to_string());
    }
    (bl, wl)
}

/// Plans `unhide_all`: returns the new (blacklist, whitelist, deactivate).
/// Only the given IDs are removed; `me` leaves the whitelist and HidHide is
/// deactivated only when the blacklist ends up empty.
pub(crate) fn plan_unhide_all(
    bl: &[String],
    wl: &[String],
    ids: &[String],
    me: &str,
) -> (Vec<String>, Vec<String>, bool) {
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let bl = without(bl, &ids);
    if bl.is_empty() {
        (bl, without(wl, &[me]), true)
    } else {
        (bl, wl.to_vec(), false)
    }
}

impl DeviceHider for HidHide {
    fn hide(&self, instance_id: &str) -> Result<(), HideError> {
        let _g = self.guard();
        let h = open_device()?;
        let old_bl = get_list(&h, GET_BLACKLIST)?;
        let old_wl = get_list(&h, GET_WHITELIST)?;
        let (bl, wl) = plan_hide(&old_bl, &old_wl, instance_id, &self.self_image);
        if wl.len() != old_wl.len() {
            set_list(&h, SET_WHITELIST, &wl)?;
        }
        if bl.len() == old_bl.len() {
            return set_active(&h, true);
        }
        // From the blacklist write on, any failure must not leave the ID hidden.
        let result = set_list(&h, SET_BLACKLIST, &bl).and_then(|()| set_active(&h, true));
        if result.is_err() {
            // Best effort: re-read and drop the ID again; the original error wins.
            if let Ok(current) = get_list(&h, GET_BLACKLIST) {
                let _ = set_list(&h, SET_BLACKLIST, &without(&current, &[instance_id]));
            }
        }
        result
    }

    fn unhide(&self, instance_id: &str) -> Result<(), HideError> {
        let _g = self.guard();
        let h = open_device()?;
        let bl = get_list(&h, GET_BLACKLIST)?;
        let kept = without(&bl, &[instance_id]);
        if kept.len() != bl.len() {
            set_list(&h, SET_BLACKLIST, &kept)?;
        }
        Ok(())
    }

    fn unhide_all(&self, instance_ids: &[String]) -> Result<(), HideError> {
        let _g = self.guard();
        let h = open_device()?;
        let old_bl = get_list(&h, GET_BLACKLIST)?;
        let old_wl = get_list(&h, GET_WHITELIST)?;
        let (bl, wl, deactivate) =
            plan_unhide_all(&old_bl, &old_wl, instance_ids, &self.self_image);
        if bl.len() != old_bl.len() {
            set_list(&h, SET_BLACKLIST, &bl)?;
        }
        if wl.len() != old_wl.len() {
            set_list(&h, SET_WHITELIST, &wl)?;
        }
        if deactivate {
            set_active(&h, false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_derived_from_hid_path() {
        let p = r"\\?\HID#{00001124-0000-1000-8000-00805f9b34fb}_VID&0002054c_PID&09cc#8&2a4cafad&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(
            instance_id_from_path(p),
            r"HID\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&09CC\8&2A4CAFAD&0&0000"
        );
    }

    #[test]
    fn multi_sz_round_trips_and_handles_empty() {
        let items = vec![r"HID\A".to_string(), r"HID\B".to_string()];
        let bytes = encode_multi_sz(&items);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
        assert_eq!(decode_multi_sz(&bytes), items);
        assert_eq!(decode_multi_sz(&encode_multi_sz(&[])), Vec::<String>::new());
        assert_eq!(decode_multi_sz(&[0x41]), Vec::<String>::new());
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plan_hide_adds_without_duplicating_case_insensitively() {
        let (bl, wl) = plan_hide(
            &v(&["HID\\A"]),
            &v(&["\\Device\\X\\me.exe"]),
            "hid\\a",
            "\\DEVICE\\x\\ME.EXE",
        );
        assert_eq!(bl, v(&["HID\\A"]));
        assert_eq!(wl, v(&["\\Device\\X\\me.exe"]));
        let (bl, wl) = plan_hide(&v(&["HID\\A"]), &v(&["other"]), "HID\\B", "me");
        assert_eq!(bl, v(&["HID\\A", "HID\\B"]));
        assert_eq!(wl, v(&["other", "me"]));
    }

    #[test]
    fn plan_unhide_all_removes_only_owned_and_deactivates_when_empty() {
        let (bl, wl, off) = plan_unhide_all(
            &v(&["HID\\A", "HID\\B"]),
            &v(&["other", "me"]),
            &v(&["hid\\a", "HID\\B"]),
            "ME",
        );
        assert!(bl.is_empty());
        assert_eq!(wl, v(&["other"]));
        assert!(off);
    }

    #[test]
    fn plan_unhide_all_keeps_foreign_entries_and_stays_active() {
        let (bl, wl, off) = plan_unhide_all(
            &v(&["HID\\A", "FOREIGN"]),
            &v(&["me"]),
            &v(&["HID\\A"]),
            "me",
        );
        assert_eq!(bl, v(&["FOREIGN"]));
        assert_eq!(wl, v(&["me"]));
        assert!(!off);
    }

    #[test]
    fn ioctl_codes_match_hidhide() {
        assert_eq!(ctl_code(2048), 0x8001_6000);
        assert_eq!(ctl_code(2053), 0x8001_6014);
    }

    #[test]
    #[ignore = "hardware: HidHide instalado e um DS4 ligado"]
    fn hide_then_unhide_all_restores_original_state() {
        let api = hidapi::HidApi::new().unwrap();
        let c = axis_devices::enumerate_hid(&api)
            .into_iter()
            .next()
            .expect("controle");
        let id = instance_id_from_path(&c.path);
        let h = HidHide::open(&std::env::current_exe().unwrap()).unwrap();
        h.hide(&id).unwrap();
        h.unhide_all(&[id]).unwrap();
    }
}
