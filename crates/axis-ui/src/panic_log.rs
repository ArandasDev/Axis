//! Pânicos da interface vão para `%LOCALAPPDATA%\Axis\logs\axis-ui-panic.log`
//! (sem console: `windows_subsystem = "windows"` engoliria a mensagem).

use std::backtrace::Backtrace;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Instala o gancho: grava mensagem e backtrace no log e chama o gancho
/// anterior. Nada aqui pode entrar em pânico (seria um aborto); cada falha de
/// E/S é ignorada.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = log_path(std::env::var_os("LOCALAPPDATA")) {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backtrace = Backtrace::force_capture().to_string();
            let _ = append(&path, &entry(secs, &info.to_string(), &backtrace));
        }
        previous(info);
    }));
}

/// `None` sem `LOCALAPPDATA` (ou vazio): sem lugar seguro para gravar.
fn log_path(local_app_data: Option<OsString>) -> Option<PathBuf> {
    let base = local_app_data.filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(base)
            .join("Axis")
            .join("logs")
            .join("axis-ui-panic.log"),
    )
}

fn entry(unix_secs: u64, message: &str, backtrace: &str) -> String {
    format!("--- pânico (unix {unix_secs}) ---\n{message}\n{backtrace}\n")
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn path_is_under_local_app_data() {
        let p = log_path(Some(OsString::from(r"C:\Users\x\AppData\Local"))).unwrap();
        assert_eq!(
            p,
            std::path::Path::new(r"C:\Users\x\AppData\Local\Axis\logs\axis-ui-panic.log")
        );
        assert_eq!(log_path(None), None);
        assert_eq!(log_path(Some(OsString::new())), None);
    }

    #[test]
    fn append_creates_dirs_and_keeps_old_entries() {
        let dir = std::env::temp_dir().join(format!("axis-ui-panic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Axis").join("logs").join("axis-ui-panic.log");
        append(&path, "primeiro\n").unwrap();
        append(&path, "segundo\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "primeiro\nsegundo\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entry_has_time_message_and_backtrace() {
        let e = entry(1_700_000_000, "estourou em main.rs:1:1", "0: axis_ui::main");
        assert!(e.starts_with("--- pânico (unix 1700000000) ---\n"), "{e}");
        assert!(e.contains("estourou em main.rs:1:1\n"), "{e}");
        assert!(e.ends_with("0: axis_ui::main\n"), "{e}");
    }
}
