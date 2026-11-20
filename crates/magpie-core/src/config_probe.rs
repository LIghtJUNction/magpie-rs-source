//! Vendor-neutral configuration states and metadata-only executable probes.
#[cfg(unix)]
use std::fs;
use std::{env, ffi::OsStr, path::PathBuf};

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

/// Resolves the platform home directory without reading any account files.
pub fn user_home_dir() -> Option<PathBuf> {
    crate::paths::user_home_dir()
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    find_executable_in_path(name, &path)
}

pub fn find_executable_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    for directory in env::split_paths(path) {
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
