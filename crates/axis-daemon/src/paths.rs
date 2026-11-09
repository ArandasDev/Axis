//! Locais dos arquivos do app (spec 5.2).

use std::path::{Path, PathBuf};

/// Diretório de configuração (`%APPDATA%\Axis`) e de estado (`%LOCALAPPDATA%\Axis`).
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config: PathBuf,
    pub state: PathBuf,
}

impl AppPaths {
    /// Caminhos reais do usuário; sem as variáveis de ambiente, cai para o diretório atual.
    pub fn from_env() -> AppPaths {
        let base = |var: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_default()
                .join("Axis")
        };
        AppPaths {
            config: base("APPDATA"),
            state: base("LOCALAPPDATA"),
        }
    }

    /// Caminhos sob uma raiz arbitrária (testes).
    pub fn under(root: &Path) -> AppPaths {
        AppPaths {
            config: root.join("config"),
            state: root.join("state"),
        }
    }

    pub fn controllers_dir(&self) -> PathBuf {
        self.config.join("controllers")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.config.join("profiles")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    pub fn hidden_file(&self) -> PathBuf {
        self.state.join("hidden.toml")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state.join("logs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_builds_the_layout() {
        let p = AppPaths::under(Path::new("r"));
        assert_eq!(p.settings_file(), Path::new("r/config/settings.toml"));
        assert_eq!(p.controllers_dir(), Path::new("r/config/controllers"));
        assert_eq!(p.profiles_dir(), Path::new("r/config/profiles"));
        assert_eq!(p.hidden_file(), Path::new("r/state/hidden.toml"));
        assert_eq!(p.logs_dir(), Path::new("r/state/logs"));
    }
}
