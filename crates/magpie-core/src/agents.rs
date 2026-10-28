use std::env;
#[cfg(unix)]
use std::fs;
use std::path::PathBuf;

use crate::codex_config::{self, CodexConfigError};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFileState {
    Unavailable,
    Missing,
    ValidToml,
    InvalidToml,
    TooLarge,
    InvalidUtf8,
    Unreadable,
    NotAFile,
    Symlink,
}

impl ConfigFileState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Missing => "missing",
            Self::ValidToml => "valid_toml",
            Self::InvalidToml => "invalid_toml",
            Self::TooLarge => "too_large",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::Unreadable => "unreadable",
            Self::NotAFile => "not_a_file",
            Self::Symlink => "symlink",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDiscovery {
    pub executable: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub config_state: ConfigFileState,
    pub config_error_line: Option<usize>,
}

pub fn discover_codex() -> CodexDiscovery {
    let executable = find_codex_executable();
    let config_path = codex_config_path();
    let (config_state, config_error_line) = config_path
        .as_deref()
        .map(inspect_config_file)
        .unwrap_or((ConfigFileState::Unavailable, None));

    CodexDiscovery {
        executable,
        config_path,
        config_state,
        config_error_line,
    }
}

pub fn find_codex_executable() -> Option<PathBuf> {
    find_executable("codex")
}

pub fn codex_config_path() -> Option<PathBuf> {
    let home = env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| paths::user_home_dir().map(|home| home.join(".codex")))?;

    let home = if home.is_absolute() {
        home
    } else {
        env::current_dir().ok()?.join(home)
    };

    Some(home.join("config.toml"))
}

fn inspect_config_file(path: &std::path::Path) -> (ConfigFileState, Option<usize>) {
    match codex_config::inspect_file(path) {
        Ok(true) => (ConfigFileState::ValidToml, None),
        Ok(false) => (ConfigFileState::Missing, None),
        Err(CodexConfigError::InvalidToml { line }) => (ConfigFileState::InvalidToml, line),
        Err(CodexConfigError::TooLarge) => (ConfigFileState::TooLarge, None),
        Err(CodexConfigError::InvalidUtf8) => (ConfigFileState::InvalidUtf8, None),
        Err(CodexConfigError::NotAFile) => (ConfigFileState::NotAFile, None),
        Err(CodexConfigError::Symlink) => (ConfigFileState::Symlink, None),
        Err(_) => (ConfigFileState::Unreadable, None),
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        #[cfg(windows)]
        let candidates = [directory.join(format!("{name}.exe")), directory.join(name)];
        #[cfg(not(windows))]
        let candidates = [directory.join(name)];

        if let Some(candidate) = candidates
            .into_iter()
            .find(|candidate| is_executable(candidate))
        {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    {
        true
    }
}
