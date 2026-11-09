//! Logging em arquivo: formata a linha no chamador, envia por um canal e uma
//! thread escritora grava em `axis.log`, girando o arquivo ao passar de 1 MiB.

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1_048_576;
const KEEP: usize = 3;

struct FileLogger {
    // Sender is not Sync on older toolchains; the mutex keeps the logger Sync.
    tx: Mutex<Sender<String>>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LevelFilter::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {} {}: {}\n",
            format_utc(now_secs()),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(tx) = self.tx.lock() {
            // Writer gone: drop the line silently.
            let _ = tx.send(line);
        }
    }

    fn flush(&self) {}
}

/// Instala o logger global gravando em `<dir>/axis.log`.
/// Chamar duas vezes devolve erro (nunca entra em pânico).
pub fn init_logging(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join("axis.log");
    let (file, size) = open_log(&path)?;
    let (tx, rx) = mpsc::channel::<String>();
    log::set_boxed_logger(Box::new(FileLogger { tx: Mutex::new(tx) }))
        .map_err(|e| io::Error::other(e.to_string()))?;
    log::set_max_level(LevelFilter::Info);

    let dir: PathBuf = dir.to_path_buf();
    std::thread::Builder::new()
        .name("axis-log".into())
        .spawn(move || writer_loop(rx, dir, file, size))?;
    Ok(())
}

fn open_log(path: &Path) -> io::Result<(BufWriter<File>, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let size = file.metadata()?.len();
    Ok((BufWriter::new(file), size))
}

fn writer_loop(rx: mpsc::Receiver<String>, dir: PathBuf, mut out: BufWriter<File>, mut size: u64) {
    let path = dir.join("axis.log");
    for line in rx {
        let len = line.len() as u64;
        if size + len > MAX_BYTES && size > 0 {
            let _ = out.flush();
            // Close the current file before renaming it.
            drop(out);
            let _ = rotate(&dir, KEEP);
            match open_log(&path) {
                Ok((o, s)) => {
                    out = o;
                    size = s;
                }
                Err(_) => return,
            }
        }
        if out.write_all(line.as_bytes()).is_ok() {
            size += len;
        }
        let _ = out.flush();
    }
}

/// Gira `axis.log` -> `axis.1.log` -> ... -> `axis.{keep}.log`; o mais antigo é apagado.
pub(crate) fn rotate(dir: &Path, keep: usize) -> io::Result<()> {
    let numbered = |i: usize| dir.join(format!("axis.{i}.log"));
    let oldest = numbered(keep);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for i in (1..keep).rev() {
        let from = numbered(i);
        if from.exists() {
            fs::rename(&from, numbered(i + 1))?;
        }
    }
    let current = dir.join("axis.log");
    if current.exists() {
        fs::rename(&current, numbered(1))?;
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ` a partir de segundos desde a época Unix (UTC).
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_shifts_files_and_keeps_three() {
        let dir = std::env::temp_dir().join(format!("axis-rot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("axis.log"), "0").unwrap();
        for i in 1..=3 {
            fs::write(dir.join(format!("axis.{i}.log")), i.to_string()).unwrap();
        }
        rotate(&dir, 3).unwrap();
        assert_eq!(fs::read_to_string(dir.join("axis.1.log")).unwrap(), "0");
        assert_eq!(fs::read_to_string(dir.join("axis.2.log")).unwrap(), "1");
        assert_eq!(fs::read_to_string(dir.join("axis.3.log")).unwrap(), "2");
        assert!(!dir.join("axis.4.log").exists());
        assert!(!dir.join("axis.log").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_timestamp_known_values() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_794_233_340), "2026-11-09T14:09:00Z");
    }
}
