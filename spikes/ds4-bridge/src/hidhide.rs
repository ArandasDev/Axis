//! Cliente mínimo do HidHide via DeviceIoControl em `\\.\HidHide`.
//! Listas são multi-strings UTF-16 (terminadas por dois NULs).

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, QueryDosDeviceW};
use windows_sys::Win32::System::IO::DeviceIoControl;

const fn ctl(function: u32) -> u32 {
    // CTL_CODE(0x8001, function, METHOD_BUFFERED=0, FILE_READ_DATA=1)
    (0x8001 << 16) | (1 << 14) | (function << 2)
}
const GET_WHITELIST: u32 = ctl(2048);
const SET_WHITELIST: u32 = ctl(2049);
const GET_BLACKLIST: u32 = ctl(2050);
const SET_BLACKLIST: u32 = ctl(2051);
const GET_ACTIVE: u32 = ctl(2052);
const SET_ACTIVE: u32 = ctl(2053);

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open() -> Result<Handle, u32> {
    let name = wide(r"\\.\HidHide");
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
        Err(unsafe { GetLastError() })
    } else {
        Ok(Handle(h))
    }
}

fn ioctl(h: &Handle, code: u32, input: &[u8], output: &mut [u8]) -> Result<u32, u32> {
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            code,
            if input.is_empty() { std::ptr::null() } else { input.as_ptr().cast() },
            input.len() as u32,
            if output.is_empty() { std::ptr::null_mut() } else { output.as_mut_ptr().cast() },
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { Err(unsafe { GetLastError() }) } else { Ok(returned) }
}

fn get_list(h: &Handle, code: u32) -> Vec<String> {
    // Primeiro pergunta o tamanho necessário.
    let needed = ioctl(h, code, &[], &mut []).unwrap_or(0) as usize;
    let mut buf = vec![0u8; needed.max(2)];
    let n = ioctl(h, code, &[], &mut buf).unwrap_or(0) as usize;
    let words: Vec<u16> = buf[..n].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    words
        .split(|&w| w == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn set_list(h: &Handle, code: u32, items: &[String]) -> Result<(), u32> {
    let mut words: Vec<u16> = Vec::new();
    for s in items {
        words.extend(s.encode_utf16());
        words.push(0);
    }
    words.push(0);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ioctl(h, code, &bytes, &mut []).map(|_| ())
}

fn get_active(h: &Handle) -> Option<bool> {
    let mut b = [0u8; 1];
    ioctl(h, GET_ACTIVE, &[], &mut b).ok().map(|_| b[0] != 0)
}

/// `\\?\HID#VID_054C&PID_05C4#7&abc&0&0000#{guid}` → `HID\VID_054C&PID_05C4\7&ABC&0&0000`
pub fn instance_id_from_path(path: &str) -> String {
    let p = path.trim_start_matches(r"\\?\");
    let p = match p.rfind("#{") {
        Some(i) => &p[..i],
        None => p,
    };
    p.replace('#', "\\").to_uppercase()
}

/// `C:\dir\app.exe` → `\Device\HarddiskVolume3\dir\app.exe` (formato que o HidHide usa na whitelist).
fn dos_device_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let drive = &s[..2];
    let mut target = [0u16; 512];
    let n = unsafe { QueryDosDeviceW(wide(drive).as_ptr(), target.as_mut_ptr(), target.len() as u32) };
    if n == 0 {
        return s;
    }
    let dev = String::from_utf16_lossy(&target[..target.iter().position(|&c| c == 0).unwrap_or(0)]);
    format!("{dev}{}", &s[2..])
}

pub fn status() {
    match open() {
        Ok(h) => {
            println!("\\\\.\\HidHide aberto com sucesso (sem precisar de elevação)");
            println!("ativo: {:?}", get_active(&h));
            println!("blacklist: {:#?}", get_list(&h, GET_BLACKLIST));
            println!("whitelist: {:#?}", get_list(&h, GET_WHITELIST));
        }
        Err(e) => println!("falha ao abrir \\\\.\\HidHide: erro Win32 {e} (5 = acesso negado → exige admin)"),
    }
}

pub fn hide_device(instance: &str) {
    let h = match open() {
        Ok(h) => h,
        Err(e) => return println!("falha ao abrir \\\\.\\HidHide: erro Win32 {e}"),
    };
    let me = dos_device_path(&std::env::current_exe().expect("exe"));
    let mut wl = get_list(&h, GET_WHITELIST);
    if !wl.iter().any(|w| w.eq_ignore_ascii_case(&me)) {
        wl.push(me.clone());
    }
    let mut bl = get_list(&h, GET_BLACKLIST);
    if !bl.iter().any(|b| b.eq_ignore_ascii_case(instance)) {
        bl.push(instance.to_string());
    }
    println!("whitelist += {me}: {:?}", set_list(&h, SET_WHITELIST, &wl));
    println!("blacklist += {instance}: {:?}", set_list(&h, SET_BLACKLIST, &bl));
    println!("ativar: {:?}", ioctl(&h, SET_ACTIVE, &[1u8], &mut []));
    println!("ativo agora: {:?}", get_active(&h));
}

pub fn unhide_device(instance: &str) {
    let h = match open() {
        Ok(h) => h,
        Err(e) => return println!("falha ao abrir \\\\.\\HidHide: erro Win32 {e}"),
    };
    let bl: Vec<String> = get_list(&h, GET_BLACKLIST)
        .into_iter()
        .filter(|b| !b.eq_ignore_ascii_case(instance))
        .collect();
    println!("blacklist -= {instance}: {:?}", set_list(&h, SET_BLACKLIST, &bl));
    let me = dos_device_path(&std::env::current_exe().expect("exe"));
    let wl: Vec<String> = get_list(&h, GET_WHITELIST)
        .into_iter()
        .filter(|w| !w.eq_ignore_ascii_case(&me))
        .collect();
    println!("whitelist -= {me}: {:?}", set_list(&h, SET_WHITELIST, &wl));
    if bl.is_empty() {
        println!("desativar: {:?}", ioctl(&h, SET_ACTIVE, &[0u8], &mut []));
    }
    println!(
        "estado final: ativo {:?}, blacklist {:?}, whitelist {:?}",
        get_active(&h),
        get_list(&h, GET_BLACKLIST),
        get_list(&h, GET_WHITELIST)
    );
}
