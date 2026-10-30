//! Small, versioned desktop preferences. Provider state remains in magpie-core.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{localization::Locale, theme::Appearance};

const MAX_BYTES: u64 = 4096;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    version: u8,
    locale: String,
    appearance: String,
}

impl Preferences {
    pub fn new(locale: Locale, appearance: Appearance) -> Self {
        Self {
            version: 1,
            locale: locale.id().to_owned(),
            appearance: match appearance {
                Appearance::System => "system",
                Appearance::Light => "light",
                Appearance::Dark => "dark",
            }
            .to_owned(),
        }
    }

    pub fn values(&self) -> Result<(Locale, Appearance), &'static str> {
        if self.version != 1 {
            return Err("preferences_version_unsupported");
        }
        let locale = Locale::ALL
            .into_iter()
            .find(|locale| locale.id() == self.locale)
            .ok_or("preferences_invalid")?;
        let appearance = match self.appearance.as_str() {
            "system" => Appearance::System,
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => return Err("preferences_invalid"),
        };
        Ok((locale, appearance))
    }
}

pub(crate) fn path() -> Result<PathBuf, &'static str> {
    magpie_core::paths::config_dir()
        .map(|path| path.join("desktop-preferences.json"))
        .map_err(|_| "preferences_path_unavailable")
}

fn inspect(path: &Path, allow_missing: bool) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("preferences_path_invalid");
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("preferences_symlink");
            }
            Ok(metadata) if ancestor != path && !metadata.is_dir() => {
                return Err("preferences_path_invalid");
            }
            Ok(_) => {}
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("preferences_unreadable"),
        }
    }
    Ok(())
}

pub(crate) fn load(path: &Path) -> Result<Option<Preferences>, &'static str> {
    inspect(path, true)?;
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("preferences_unreadable"),
    };
    let metadata = file.metadata().map_err(|_| "preferences_unreadable")?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("preferences_invalid");
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "preferences_unreadable")?;
    if bytes.len() > MAX_BYTES as usize {
        return Err("preferences_invalid");
    }
    let preferences: Preferences =
        serde_json::from_slice(&bytes).map_err(|_| "preferences_invalid")?;
    preferences.values()?;
    Ok(Some(preferences))
}

/// Reject stale snapshots before replacing. Unix writers additionally share an
/// advisory lock; other platforms retain a final compare-before-rename check.
/// Failed writes leave the preceding file intact.
pub(crate) fn save(
    path: &Path,
    expected: Option<&Preferences>,
    value: &Preferences,
) -> Result<(), &'static str> {
    value.values()?;
    inspect(path, true)?;
    let parent = path.parent().ok_or("preferences_path_invalid")?;
    fs::create_dir_all(parent).map_err(|_| "preferences_write_failed")?;
    inspect(path, true)?;
    #[cfg(unix)]
    let _lock = preference_lock(parent)?;
    if load(path)?.as_ref() != expected {
        return Err("preferences_changed");
    }
    let parent = path.parent().ok_or("preferences_path_invalid")?;
    fs::create_dir_all(parent).map_err(|_| "preferences_write_failed")?;
    inspect(path, true)?;
    let temporary = parent.join(format!(
        ".desktop-preferences.{}.{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| "preferences_write_failed")?;
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(value).map_err(|_| "preferences_invalid")?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .map_err(|_| "preferences_write_failed")?;
        drop(file);
        if load(path)?.as_ref() != expected {
            return Err("preferences_changed");
        }
        fs::rename(&temporary, path).map_err(|_| "preferences_write_failed")
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(unix)]
fn preference_lock(parent: &Path) -> Result<std::fs::File, &'static str> {
    use rustix::fs::{FlockOperation, OFlags, flock};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = parent.join(".desktop-preferences.lock");
    inspect(&path, true)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(&path)
        .map_err(|_| "preferences_write_failed")?;
    let metadata = file.metadata().map_err(|_| "preferences_write_failed")?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err("preferences_path_invalid");
    }
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|_| "preferences_busy")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "magpie-desktop-preferences-test-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn persists_preferences_and_rejects_stale_window() {
        let root = directory();
        let path = root.join("preferences.json");
        assert_eq!(load(&path).unwrap(), None);
        let dark = Preferences::new(Locale::SimplifiedChinese, Appearance::Dark);
        save(&path, None, &dark).unwrap();
        assert_eq!(load(&path).unwrap(), Some(dark.clone()));
        let light = Preferences::new(Locale::German, Appearance::Light);
        assert_eq!(save(&path, None, &light), Err("preferences_changed"));
        assert_eq!(load(&path).unwrap(), Some(dark.clone()));
        save(&path, Some(&dark), &light).unwrap();
        assert_eq!(
            load(&path).unwrap().unwrap().values().unwrap(),
            (Locale::German, Appearance::Light)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_or_future_settings_are_preserved() {
        let root = directory();
        let path = root.join("preferences.json");
        let bytes = b"{\"version\":2,\"locale\":\"locale-en\",\"appearance\":\"dark\"}";
        fs::write(&path, bytes).unwrap();
        assert_eq!(load(&path), Err("preferences_version_unsupported"));
        assert!(
            save(
                &path,
                None,
                &Preferences::new(Locale::English, Appearance::Dark)
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_preferences_symlink() {
        let root = directory();
        let target = root.join("target");
        fs::write(&target, b"private fixture").unwrap();
        let path = root.join("preferences.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(load(&path), Err("preferences_symlink"));
        assert!(
            save(
                &path,
                None,
                &Preferences::new(Locale::English, Appearance::Dark)
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"private fixture");
        fs::remove_dir_all(root).unwrap();
    }
}
