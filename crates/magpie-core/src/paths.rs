use std::env;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigPathError;

impl fmt::Display for ConfigPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cannot find a user configuration directory; set MAGPIE_CONFIG_DIR")
    }
}

impl Error for ConfigPathError {}

pub fn config_dir() -> Result<PathBuf, ConfigPathError> {
    if let Some(path) = non_empty_var("MAGPIE_CONFIG_DIR") {
        return Ok(path);
    }

    let path = match env::consts::OS {
        "windows" => windows_config_dir(),
        "macos" => user_home_dir().map(|home| home.join("Library/Application Support/Magpie")),
        _ => unix_config_dir(),
    };

    path.ok_or(ConfigPathError)
}

fn unix_config_dir() -> Option<PathBuf> {
    let root = non_empty_var("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| user_home_dir().map(|home| home.join(".config")))?;
    Some(root.join("magpie"))
}

fn windows_config_dir() -> Option<PathBuf> {
    non_empty_var("APPDATA")
        .or_else(|| user_home_dir().map(|home| home.join("AppData/Roaming")))
        .map(|root| root.join("Magpie"))
}

pub(crate) fn user_home_dir() -> Option<PathBuf> {
    if env::consts::OS == "windows" {
        non_empty_var("USERPROFILE").or_else(windows_home_from_drive)
    } else {
        non_empty_var("HOME")
    }
}

fn windows_home_from_drive() -> Option<PathBuf> {
    let drive = non_empty_var("HOMEDRIVE")?;
    let path = non_empty_var("HOMEPATH")?;
    Some(drive.join(path))
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
