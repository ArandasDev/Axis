//! Configurações, registros de controle e perfis (spec 5.2).

use crate::paths::AppPaths;
use crate::store::{Loaded, load_toml, write_atomic};
use axis_core::{
    ControllerRecord, Corrections, DeviceFamily, Profile, SCHEMA_VERSION, SchemaError, SchemaProbe,
    check_schema,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Nome do perfil criado automaticamente.
const DEFAULT_PROFILE: &str = "Padrão";

/// Cores atribuídas aos donos dos controles, em ciclo.
pub const OWNER_PALETTE: [[u8; 3]; 6] = [
    [95, 179, 122],
    [155, 122, 216],
    [224, 176, 74],
    [91, 160, 220],
    [217, 119, 87],
    [210, 110, 160],
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub schema_version: u32,
    pub autostart: bool,
    pub grace_secs: u64,
    pub theme: String,
    pub accent: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            schema_version: SCHEMA_VERSION,
            autostart: true,
            grace_secs: 5,
            theme: "dark".into(),
            accent: "laranja".into(),
        }
    }
}

impl AppSettings {
    fn validate(&self) -> Result<(), String> {
        if !(1..=60).contains(&self.grace_secs) {
            return Err("grace_secs: deve estar entre 1 e 60".into());
        }
        Ok(())
    }
}

/// Torna `clean` (sem pontos nem separadores) um nome de arquivo seguro no Windows.
fn safe_file_name(clean: &str) -> String {
    // O Windows ignora espaços e pontos finais.
    let stem = clean.trim_end_matches([' ', '.']);
    if stem.is_empty() {
        return "_.toml".into();
    }
    let upper = stem.to_ascii_uppercase();
    let numbered = |prefix: &str| {
        upper
            .strip_prefix(prefix)
            .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    };
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || numbered("COM")
        || numbered("LPT");
    if reserved {
        format!("_{stem}.toml")
    } else {
        format!("{stem}.toml")
    }
}

/// Nome do arquivo de um controle: só `[A-Za-z0-9_-]`; `:` vira `-`, o resto vira `_`.
fn record_file_name(id: &str) -> String {
    let clean: String = id
        .chars()
        .map(|c| match c {
            ':' => '-',
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => c,
            _ => '_',
        })
        .collect();
    safe_file_name(&clean)
}

/// Nome do arquivo de um perfil: mantém letras (inclusive acentuadas), dígitos, ' ', '_' e '-'.
fn profile_file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    safe_file_name(&clean)
}

/// Leitura e gravação das configurações do app, dos controles e dos perfis.
pub struct ConfigStore {
    paths: AppPaths,
    notices: Mutex<Vec<String>>,
    /// Arquivos de versão mais nova (ou ilegíveis): nunca são sobrescritos.
    read_only: Mutex<HashSet<PathBuf>>,
    warned: Mutex<HashSet<PathBuf>>,
}

impl ConfigStore {
    pub fn new(paths: AppPaths) -> ConfigStore {
        ConfigStore {
            paths,
            notices: Mutex::new(Vec::new()),
            read_only: Mutex::new(HashSet::new()),
            warned: Mutex::new(HashSet::new()),
        }
    }

    fn notify(&self, msg: String) {
        self.notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(msg);
    }

    /// Carrega `path`, convertendo cada resultado em valor + avisos.
    /// Devolve `(valor, ausente)`.
    fn load<T: DeserializeOwned>(
        &mut self,
        path: &Path,
        check: impl Fn(&T) -> Result<(), String>,
    ) -> (Option<T>, bool) {
        let shown = path.display();
        match load_toml(path, check) {
            Loaded::Fresh(v) => (Some(v), false),
            Loaded::FromBackup(v) => {
                self.notify(format!("Configuração restaurada do backup: {shown}"));
                (Some(v), false)
            }
            Loaded::Missing => (None, true),
            Loaded::Corrupt { preserved_as } => {
                self.notify(format!(
                    "Arquivo corrompido preservado em {}",
                    preserved_as.display()
                ));
                (None, false)
            }
            Loaded::TooNew { .. } => {
                self.notify(format!(
                    "{shown} é de uma versão mais nova do Axis; não será alterado"
                ));
                self.mark_read_only(path);
                (None, false)
            }
            Loaded::Unreadable { error } => {
                self.notify(format!(
                    "Não foi possível ler {shown} ({error}); não será alterado"
                ));
                self.mark_read_only(path);
                (None, false)
            }
        }
    }

    fn mark_read_only(&self, path: &Path) {
        self.read_only
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf());
    }

    fn is_read_only(&self, path: &Path) -> bool {
        if self
            .read_only
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(path)
        {
            return true;
        }
        if !path.exists() {
            return false;
        }
        // Arquivo existente que esta instância ainda não leu: nunca sobrescrever
        // um arquivo de versão mais nova ou ilegível.
        let protect = match std::fs::read(path) {
            Err(_) => true,
            Ok(bytes) => String::from_utf8(bytes)
                .ok()
                .and_then(|text| toml::from_str::<SchemaProbe>(&text).ok())
                .is_some_and(|probe| {
                    matches!(
                        check_schema(probe.schema_version),
                        Err(SchemaError::TooNew { .. })
                    )
                }),
        };
        if protect {
            self.mark_read_only(path);
        }
        protect
    }

    fn save<T: Serialize>(&self, path: &Path, value: &T) -> io::Result<()> {
        if self.is_read_only(path) {
            let first = self
                .warned
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(path.to_path_buf());
            if first {
                self.notify(format!(
                    "{} é de uma versão mais nova do Axis ou está ilegível; alterações não serão gravadas",
                    path.display()
                ));
            }
            return Ok(());
        }
        let text = toml::to_string_pretty(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        write_atomic(path, text.as_bytes())
    }

    pub fn load_settings(&mut self) -> AppSettings {
        let path = self.paths.settings_file();
        self.load(&path, AppSettings::validate)
            .0
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &AppSettings) -> io::Result<()> {
        self.save(&self.paths.settings_file(), s)
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.paths.controllers_dir().join(record_file_name(id))
    }

    pub fn load_record(&mut self, id: &str) -> Option<ControllerRecord> {
        let path = self.record_path(id);
        self.load(&path, |r: &ControllerRecord| {
            r.validate().map_err(|e| e.to_string())
        })
        .0
    }

    pub fn save_record(&self, r: &ControllerRecord) -> io::Result<()> {
        self.save(&self.record_path(&r.id), r)
    }

    pub fn new_record(id: &str, family: DeviceFamily, palette_index: usize) -> ControllerRecord {
        ControllerRecord {
            schema_version: SCHEMA_VERSION,
            id: id.to_string(),
            family,
            owner_name: format!("Controle {}", palette_index + 1),
            owner_color: OWNER_PALETTE[palette_index % OWNER_PALETTE.len()],
            active_profile: DEFAULT_PROFILE.to_string(),
            corrections: Corrections::default(),
        }
    }

    pub fn load_profile(&mut self, name: &str) -> Profile {
        let path = self.paths.profiles_dir().join(profile_file_name(name));
        let (loaded, missing) =
            self.load(&path, |p: &Profile| p.validate().map_err(|e| e.to_string()));
        if let Some(p) = loaded {
            return p;
        }
        let profile = Profile::default();
        if name == DEFAULT_PROFILE {
            if self.is_read_only(&path) {
                return profile;
            }
            // Falha ao gravar não impede o uso do perfil padrão em memória.
            let _ = self.save(&path, &profile);
        } else if missing {
            self.notify(format!("Perfil \"{name}\" não encontrado; usando o padrão"));
        }
        profile
    }

    pub fn export_profile(p: &Profile, dest: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(p)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(dest, text)
    }

    pub fn import_profile(src: &Path) -> Result<Profile, String> {
        let text = std::fs::read_to_string(src)
            .map_err(|e| format!("não foi possível ler o arquivo: {e}"))?;
        let probe: SchemaProbe =
            toml::from_str(&text).map_err(|e| format!("arquivo de perfil inválido: {e}"))?;
        match check_schema(probe.schema_version) {
            Ok(()) => {}
            Err(SchemaError::TooNew { .. }) => {
                return Err("perfil de uma versão mais nova do Axis".into());
            }
            Err(SchemaError::Zero) => {
                return Err("perfil inválido: schema_version: não pode ser 0".into());
            }
        }
        let profile: Profile =
            toml::from_str(&text).map_err(|e| format!("arquivo de perfil inválido: {e}"))?;
        profile
            .validate()
            .map_err(|e| format!("perfil inválido: {}: {}", e.field, e.reason))?;
        Ok(profile)
    }

    /// Avisos acumulados desde a última chamada.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.notices.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use axis_core::{DeviceFamily, Profile};
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "axis-test-{}-config-{}",
                std::process::id(),
                name
            ));
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

    fn store(t: &TempDir) -> ConfigStore {
        ConfigStore::new(AppPaths::under(t.path()))
    }

    #[test]
    fn settings_default_when_missing_and_round_trip() {
        let t = TempDir::new("settings");
        let mut s = store(&t);
        assert_eq!(s.load_settings(), AppSettings::default());
        let custom = AppSettings {
            grace_secs: 12,
            theme: "claro".into(),
            ..AppSettings::default()
        };
        s.save_settings(&custom).unwrap();
        assert_eq!(s.load_settings(), custom);
        assert!(s.take_notices().is_empty());
    }

    #[test]
    fn settings_grace_out_of_bounds_is_corrupt() {
        let t = TempDir::new("grace");
        let mut s = store(&t);
        let bad = AppSettings {
            grace_secs: 0,
            ..AppSettings::default()
        };
        s.save_settings(&bad).unwrap();
        assert_eq!(s.load_settings(), AppSettings::default());
        assert!(!s.take_notices().is_empty());
    }

    #[test]
    fn new_record_cycles_palette_and_names() {
        let r = ConfigStore::new_record("a", DeviceFamily::DualShock4, 0);
        assert_eq!(r.owner_name, "Controle 1");
        assert_eq!(r.owner_color, OWNER_PALETTE[0]);
        assert_eq!(r.active_profile, "Padrão");
        let r = ConfigStore::new_record("a", DeviceFamily::Xbox, 7);
        assert_eq!(r.owner_name, "Controle 8");
        assert_eq!(r.owner_color, OWNER_PALETTE[1]);
    }

    #[test]
    fn record_file_name_replaces_colons() {
        let t = TempDir::new("record");
        let mut s = store(&t);
        let r = ConfigStore::new_record("41:42:00:00:84:b6", DeviceFamily::DualShock4, 2);
        s.save_record(&r).unwrap();
        assert!(
            t.path()
                .join("config/controllers/41-42-00-00-84-b6.toml")
                .exists()
        );
        assert_eq!(s.load_record("41:42:00:00:84:b6"), Some(r));
        assert_eq!(s.load_record("outro"), None);
    }

    #[test]
    fn default_profile_is_created_on_first_load() {
        let t = TempDir::new("profile");
        let mut s = store(&t);
        assert_eq!(s.load_profile("Padrão"), Profile::default());
        assert!(t.path().join("config/profiles/Padrão.toml").exists());
    }

    #[test]
    fn missing_named_profile_falls_back_with_notice() {
        let t = TempDir::new("missing-profile");
        let mut s = store(&t);
        assert_eq!(s.load_profile("Sniper"), Profile::default());
        assert!(!t.path().join("config/profiles/Sniper.toml").exists());
        assert_eq!(s.take_notices().len(), 1);
    }

    #[test]
    fn profile_file_name_is_sanitized() {
        assert_eq!(profile_file_name("Padrão"), "Padrão.toml");
        assert_eq!(profile_file_name("a/b:c*d"), "a_b_c_d.toml");
        assert_eq!(profile_file_name("Meu-perfil_1 x"), "Meu-perfil_1 x.toml");
        assert_eq!(profile_file_name(""), "_.toml");
    }

    #[test]
    fn export_import_round_trip_and_rejects_invalid() {
        let t = TempDir::new("export");
        let p = Profile {
            vibration: 0.5,
            ..Profile::default()
        };
        let file = t.path().join("p.toml");
        ConfigStore::export_profile(&p, &file).unwrap();
        assert_eq!(ConfigStore::import_profile(&file), Ok(p));

        let mut bad = Profile::default();
        bad.left_stick.deadzone.inner = 0.9;
        bad.left_stick.deadzone.outer = 0.5;
        let bad_file = t.path().join("bad.toml");
        std::fs::write(&bad_file, toml::to_string(&bad).unwrap()).unwrap();
        let err = ConfigStore::import_profile(&bad_file).unwrap_err();
        assert!(err.starts_with("perfil inválido: "), "{err}");

        let newer = t.path().join("new.toml");
        std::fs::write(&newer, "schema_version = 99\n").unwrap();
        let err = ConfigStore::import_profile(&newer).unwrap_err();
        assert!(err.contains("mais nova"), "{err}");

        let junk = t.path().join("junk.toml");
        std::fs::write(&junk, "lixo").unwrap();
        assert!(ConfigStore::import_profile(&junk).is_err());
        assert!(ConfigStore::import_profile(&t.path().join("nao-existe.toml")).is_err());
    }

    #[test]
    fn too_new_record_is_not_overwritten() {
        let t = TempDir::new("toonew");
        let mut s = store(&t);
        let dir = t.path().join("config/controllers");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("aa-bb.toml");
        std::fs::write(&file, "schema_version = 99\n").unwrap();
        assert_eq!(s.load_record("aa:bb"), None);
        let notices = s.take_notices();
        assert!(!notices.is_empty());
        assert!(notices[0].contains("mais nova"), "{notices:?}");

        let r = ConfigStore::new_record("aa:bb", DeviceFamily::Xbox, 0);
        s.save_record(&r).unwrap();
        s.save_record(&r).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "schema_version = 99\n"
        );
        assert!(!dir.join("aa-bb.toml.bak").exists());
        assert_eq!(s.take_notices().len(), 1, "aviso de gravação só uma vez");
    }

    #[test]
    fn backup_and_corrupt_produce_notices() {
        let t = TempDir::new("notices");
        let mut s = store(&t);
        let r = ConfigStore::new_record("aa:bb", DeviceFamily::Xbox, 0);
        s.save_record(&r).unwrap();
        s.save_record(&r).unwrap();
        let file = t.path().join("config/controllers/aa-bb.toml");
        std::fs::write(&file, "lixo").unwrap();
        assert_eq!(s.load_record("aa:bb"), Some(r));
        let n = s.take_notices();
        assert_eq!(n.len(), 1);
        assert!(
            n[0].starts_with("Configuração restaurada do backup: "),
            "{n:?}"
        );

        let _ = std::fs::remove_file(t.path().join("config/controllers/aa-bb.toml.bak"));
        std::fs::write(&file, "lixo").unwrap();
        assert_eq!(s.load_record("aa:bb"), None);
        let n = s.take_notices();
        assert!(
            n[0].starts_with("Arquivo corrompido preservado em "),
            "{n:?}"
        );
    }

    #[test]
    fn save_without_prior_load_never_overwrites_newer_file() {
        let t = TempDir::new("noload");
        let dir = t.path().join("config/controllers");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("aa-bb.toml");
        std::fs::write(&file, "schema_version = 99\n").unwrap();
        let mut s = store(&t);
        let r = ConfigStore::new_record("aa:bb", DeviceFamily::Xbox, 0);
        s.save_record(&r).unwrap();
        s.save_record(&r).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "schema_version = 99\n"
        );
        assert!(!dir.join("aa-bb.toml.bak").exists());
        assert_eq!(s.take_notices().len(), 1);
    }

    #[test]
    fn reserved_windows_names_are_prefixed() {
        assert_eq!(profile_file_name("CON"), "_CON.toml");
        assert_eq!(profile_file_name("lpt1"), "_lpt1.toml");
        assert_eq!(profile_file_name("Com9"), "_Com9.toml");
        assert_eq!(profile_file_name("console"), "console.toml");
        assert_eq!(profile_file_name("a."), "a_.toml");
        assert_eq!(profile_file_name("a "), "a.toml");
        assert_eq!(profile_file_name("   "), "_.toml");
        assert_eq!(record_file_name("nul"), "_nul.toml");
        assert_eq!(record_file_name("a*b:c.d"), "a_b-c_d.toml");
    }

    #[test]
    fn too_new_settings_untouched() {
        let t = TempDir::new("settings-new");
        let file = t.path().join("config/settings.toml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "schema_version = 99\n").unwrap();
        let mut s = store(&t);
        assert_eq!(s.load_settings(), AppSettings::default());
        s.save_settings(&AppSettings::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "schema_version = 99\n"
        );
    }

    #[test]
    fn too_new_default_profile_untouched_with_single_notice() {
        let t = TempDir::new("profile-new");
        let file = t.path().join("config/profiles/Padrão.toml");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "schema_version = 99\n").unwrap();
        let mut s = store(&t);
        assert_eq!(s.load_profile("Padrão"), Profile::default());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "schema_version = 99\n"
        );
        assert_eq!(s.take_notices().len(), 1);
    }

    #[test]
    fn unreadable_record_is_not_written() {
        let t = TempDir::new("unreadable");
        let path = t.path().join("config/controllers/aa-bb.toml");
        std::fs::create_dir_all(&path).unwrap();
        let mut s = store(&t);
        assert_eq!(s.load_record("aa:bb"), None);
        assert_eq!(s.take_notices().len(), 1);
        let r = ConfigStore::new_record("aa:bb", DeviceFamily::Xbox, 0);
        s.save_record(&r).unwrap();
        assert!(path.is_dir());
    }
}
