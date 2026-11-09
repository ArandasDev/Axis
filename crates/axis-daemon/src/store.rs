//! Armazenamento TOML robusto: gravação atômica com `.bak` e arquivos corrompidos preservados.

use axis_core::{SchemaError, SchemaProbe, check_schema};
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Resultado de carregar um arquivo TOML.
///
/// Atenção: quem recebe `TooNew` ou `Unreadable` NÃO deve gravar por cima do arquivo.
#[derive(Debug, PartialEq)]
pub enum Loaded<T> {
    Fresh(T),
    FromBackup(T),
    Missing,
    Corrupt {
        preserved_as: PathBuf,
    },
    TooNew {
        found: u32,
    },
    /// Erro de E/S ao ler (ou ao preservar o arquivo corrompido); nada foi movido nem declarado preservado.
    Unreadable {
        error: String,
    },
}

/// `path` com `suffix` anexado ao nome completo (`settings.toml` -> `settings.toml.bak`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Grava `bytes` em `path` de forma atômica, guardando o conteúdo anterior em `.bak`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if path.exists() {
        fs::copy(path, with_suffix(path, ".bak"))?;
    }
    let tmp = with_suffix(path, ".tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

enum Attempt<T> {
    Ok(T),
    TooNew(u32),
    Bad,
    Io(String),
}

fn try_parse<T: DeserializeOwned>(
    path: &Path,
    check: &impl Fn(&T) -> Result<(), String>,
) -> Attempt<T> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => return Attempt::Io(e.to_string()),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Attempt::Bad;
    };
    let Ok(probe) = toml::from_str::<SchemaProbe>(&text) else {
        return Attempt::Bad;
    };
    match check_schema(probe.schema_version) {
        Ok(()) => {}
        Err(SchemaError::TooNew { found, .. }) => return Attempt::TooNew(found),
        Err(SchemaError::Zero) => return Attempt::Bad,
    }
    match toml::from_str::<T>(&text) {
        Ok(value) if check(&value).is_ok() => Attempt::Ok(value),
        _ => Attempt::Bad,
    }
}

/// Move o arquivo corrompido para `.corrupt` (com sufixo numérico em colisão).
fn preserve_corrupt(path: &Path) -> Result<PathBuf, String> {
    let mut target = with_suffix(path, ".corrupt");
    let mut n = 0u32;
    while target.exists() {
        n += 1;
        target = with_suffix(path, &format!(".corrupt.{n}"));
    }
    if fs::rename(path, &target).is_ok() {
        return Ok(target);
    }
    match fs::copy(path, &target) {
        Ok(_) => {
            let _ = fs::remove_file(path);
            Ok(target)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Carrega `path`; se inválido, tenta o `.bak`; se também falhar, preserva como `.corrupt`.
pub fn load_toml<T: DeserializeOwned>(
    path: &Path,
    check: impl Fn(&T) -> Result<(), String>,
) -> Loaded<T> {
    if !path.exists() {
        return Loaded::Missing;
    }
    match try_parse(path, &check) {
        Attempt::Ok(value) => return Loaded::Fresh(value),
        Attempt::TooNew(found) => return Loaded::TooNew { found },
        Attempt::Io(error) => return Loaded::Unreadable { error },
        Attempt::Bad => {}
    }
    let bak = with_suffix(path, ".bak");
    let backup = match try_parse(&bak, &check) {
        Attempt::Ok(value) => Some(value),
        _ => None,
    };
    match (preserve_corrupt(path), backup) {
        (Ok(_), Some(value)) => {
            // Restaura o backup como arquivo principal: o próximo `write_atomic`
            // não deve sobrescrever o único `.bak` bom com o arquivo corrompido.
            let _ = fs::copy(&bak, path);
            Loaded::FromBackup(value)
        }
        (Ok(preserved_as), None) => Loaded::Corrupt { preserved_as },
        (Err(error), _) => Loaded::Unreadable { error },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir =
                std::env::temp_dir().join(format!("axis-test-{}-{}", std::process::id(), name));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Doc {
        schema_version: u32,
        value: u32,
    }

    fn doc(value: u32) -> Doc {
        Doc {
            schema_version: 1,
            value,
        }
    }

    fn write_doc(path: &Path, value: u32) {
        write_atomic(path, toml::to_string(&doc(value)).unwrap().as_bytes()).unwrap();
    }

    fn ok(_: &Doc) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn write_then_load_is_fresh() {
        let t = TempDir::new("fresh");
        let p = t.path().join("settings.toml");
        write_doc(&p, 1);
        assert_eq!(load_toml::<Doc>(&p, ok), Loaded::Fresh(doc(1)));
    }

    #[test]
    fn second_write_keeps_previous_as_bak() {
        let t = TempDir::new("bak");
        let p = t.path().join("settings.toml");
        write_doc(&p, 1);
        write_doc(&p, 2);
        let bak: Doc =
            toml::from_str(&std::fs::read_to_string(t.path().join("settings.toml.bak")).unwrap())
                .unwrap();
        assert_eq!(bak, doc(1));
        assert_eq!(load_toml::<Doc>(&p, ok), Loaded::Fresh(doc(2)));
    }

    #[test]
    fn corrupt_file_falls_back_to_backup() {
        let t = TempDir::new("fallback");
        let p = t.path().join("settings.toml");
        write_doc(&p, 1);
        write_doc(&p, 2);
        std::fs::write(&p, "lixo").unwrap();
        assert_eq!(load_toml::<Doc>(&p, ok), Loaded::FromBackup(doc(1)));
    }

    #[test]
    fn corrupt_without_backup_is_preserved() {
        let t = TempDir::new("corrupt");
        let p = t.path().join("settings.toml");
        std::fs::write(&p, "lixo").unwrap();
        let expected = t.path().join("settings.toml.corrupt");
        assert_eq!(
            load_toml::<Doc>(&p, ok),
            Loaded::Corrupt {
                preserved_as: expected.clone()
            }
        );
        assert!(expected.exists());
        assert!(!p.exists());
    }

    #[test]
    fn newer_schema_is_left_untouched() {
        let t = TempDir::new("newer");
        let p = t.path().join("settings.toml");
        std::fs::write(&p, "schema_version = 2\nvalue = 1").unwrap();
        assert_eq!(load_toml::<Doc>(&p, ok), Loaded::TooNew { found: 2 });
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "schema_version = 2\nvalue = 1"
        );
        assert!(!t.path().join("settings.toml.corrupt").exists());
    }

    #[test]
    fn missing_file_is_missing() {
        let t = TempDir::new("missing");
        assert_eq!(
            load_toml::<Doc>(&t.path().join("settings.toml"), ok),
            Loaded::Missing
        );
    }

    #[test]
    fn check_failure_counts_as_corrupt() {
        let t = TempDir::new("check");
        let p = t.path().join("settings.toml");
        write_doc(&p, 1);
        let r = load_toml::<Doc>(&p, |_| Err("nope".to_string()));
        let expected = t.path().join("settings.toml.corrupt");
        assert_eq!(
            r,
            Loaded::Corrupt {
                preserved_as: expected.clone()
            }
        );
        assert!(expected.exists());
    }
}

#[cfg(test)]
mod fix_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Doc {
        schema_version: u32,
        value: u32,
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("axis-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path, v: u32) {
        let d = Doc {
            schema_version: 1,
            value: v,
        };
        write_atomic(p, toml::to_string(&d).unwrap().as_bytes()).unwrap();
    }

    #[test]
    fn backup_recovery_survives_next_save() {
        let d = dir("recovery");
        let p = d.join("settings.toml");
        put(&p, 1);
        put(&p, 2);
        fs::write(&p, "lixo").unwrap();
        let r = load_toml::<Doc>(&p, |_| Ok(()));
        assert_eq!(
            r,
            Loaded::FromBackup(Doc {
                schema_version: 1,
                value: 1
            })
        );
        assert_eq!(
            fs::read_to_string(d.join("settings.toml.corrupt")).unwrap(),
            "lixo"
        );
        let main: Doc = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(main.value, 1);
        put(&p, 3);
        let bak: Doc =
            toml::from_str(&fs::read_to_string(d.join("settings.toml.bak")).unwrap()).unwrap();
        assert_eq!(bak.value, 1);
        let main: Doc = toml::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(main.value, 3);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn read_error_is_unreadable_not_corrupt() {
        let d = dir("unreadable");
        let p = d.join("settings.toml");
        fs::create_dir_all(&p).unwrap();
        let r = load_toml::<Doc>(&p, |_| Ok(()));
        assert!(matches!(r, Loaded::Unreadable { .. }), "{r:?}");
        assert!(p.is_dir());
        assert!(!d.join("settings.toml.corrupt").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_utf8_counts_as_corrupt() {
        let d = dir("utf8");
        let p = d.join("settings.toml");
        fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        let r = load_toml::<Doc>(&p, |_| Ok(()));
        assert!(matches!(r, Loaded::Corrupt { .. }), "{r:?}");
        let _ = fs::remove_dir_all(&d);
    }
}
