//! Private, bounded read-only observation used to invalidate stale previews.
//! The transaction CLI remains responsible for atomic writes and restoration.

use crate::cli_bridge::{BridgeError, ErrorKind};
use std::{
    fmt,
    fs::{self, File, Metadata},
    io::Read,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    readonly: bool,
    #[cfg(unix)]
    unix: (u64, u64, u32, u32, u32),
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.mode(),
                    metadata.uid(),
                    metadata.gid(),
                )
            },
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct FileState {
    identity: Identity,
    len: u64,
    modified: SystemTime,
}

impl FileState {
    fn read(metadata: &Metadata) -> Result<Self, BridgeError> {
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid());
        }
        Ok(Self {
            identity: Identity::from_metadata(metadata),
            len: metadata.len(),
            modified: metadata.modified().map_err(|_| invalid())?,
        })
    }
}

/// Contains private configuration bytes. Debug intentionally reveals neither
/// bytes, paths nor file metadata. Rendering must borrow the containing ticket.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigStamp {
    parents: Vec<(PathBuf, Identity)>,
    file: Option<FileState>,
    bytes: Vec<u8>,
}

impl fmt::Debug for ConfigStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigStamp([private observation])")
    }
}

fn invalid() -> BridgeError {
    BridgeError::new(ErrorKind::ConfigInvalid)
}
fn conflict() -> BridgeError {
    BridgeError::new(ErrorKind::ConfigConflict)
}

fn parents(path: &Path) -> Result<Vec<(PathBuf, Identity)>, BridgeError> {
    let mut observed = Vec::new();
    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(ancestor).map_err(|_| invalid())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid());
        }
        observed.push((ancestor.to_path_buf(), Identity::from_metadata(&metadata)));
    }
    Ok(observed)
}

impl ConfigStamp {
    pub fn exists(&self) -> bool {
        self.file.is_some()
    }

    pub fn capture(path: &Path) -> Result<Self, BridgeError> {
        if !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|value| value.len() > 4096 || value.contains('\0'))
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(BridgeError::new(ErrorKind::InvalidConfigPath));
        }
        let before_parents = parents(path)?;
        let named = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                    || parents(path)? != before_parents
                {
                    return Err(conflict());
                }
                return Ok(Self {
                    parents: before_parents,
                    file: None,
                    bytes: Vec::new(),
                });
            }
            Err(_) => return Err(invalid()),
        };
        if named.file_type().is_symlink() {
            return Err(invalid());
        }
        let before = FileState::read(&named)?;
        #[cfg(unix)]
        let file = {
            use rustix::fs::{Mode, OFlags, open};
            let fd = open(
                path,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| invalid())?;
            File::from(fd)
        };
        #[cfg(not(unix))]
        let file = File::open(path).map_err(|_| invalid())?;
        if FileState::read(&file.metadata().map_err(|_| invalid())?)? != before {
            return Err(conflict());
        }
        let mut bytes = Vec::with_capacity(before.len as usize);
        (&file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        let after = FileState::read(&file.metadata().map_err(|_| invalid())?)?;
        let named_after = fs::symlink_metadata(path).map_err(|_| conflict())?;
        if bytes.len() as u64 != before.len
            || named_after.file_type().is_symlink()
            || after != before
            || FileState::read(&named_after)? != before
            || parents(path)? != before_parents
        {
            return Err(conflict());
        }
        Ok(Self {
            parents: before_parents,
            file: Some(before),
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "magpie-stamp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_existing_bytes_and_redacted_debug() {
        let root = Fixture::new();
        let path = root.0.join("config.toml");
        let missing = ConfigStamp::capture(&path).unwrap();
        assert!(!missing.exists());
        fs::write(&path, "secret = 'PRIVATE_STAMP_CANARY'\n").unwrap();
        let present = ConfigStamp::capture(&path).unwrap();
        assert!(present.exists());
        assert_ne!(present, missing);
        assert_eq!(present, ConfigStamp::capture(&path).unwrap());
        assert!(!format!("{present:?}").contains("PRIVATE_STAMP_CANARY"));
        fs::write(&path, "secret = 'different'\n").unwrap();
        assert_ne!(present, ConfigStamp::capture(&path).unwrap());
    }

    #[test]
    fn rejects_bad_paths_nonfiles_and_oversized_files() {
        let root = Fixture::new();
        let path = root.0.join("config.toml");
        assert!(ConfigStamp::capture(Path::new("relative.toml")).is_err());
        assert!(ConfigStamp::capture(&root.0.join("../config.toml")).is_err());
        assert!(ConfigStamp::capture(&root.0).is_err());
        assert!(ConfigStamp::capture(&root.0.join("absent/config.toml")).is_err());
        File::create(&path).unwrap().set_len(MAX_BYTES + 1).unwrap();
        assert!(ConfigStamp::capture(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_and_same_bytes_replacement_changes_identity() {
        use std::os::unix::fs::symlink;
        let root = Fixture::new();
        let path = root.0.join("config.toml");
        fs::write(&path, "model = 'old'\n").unwrap();
        let first = ConfigStamp::capture(&path).unwrap();
        let replacement = root.0.join("replacement");
        fs::write(&replacement, "model = 'old'\n").unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_ne!(first, ConfigStamp::capture(&path).unwrap());
        let link = root.0.join("link");
        symlink(&path, &link).unwrap();
        assert!(ConfigStamp::capture(&link).is_err());
        let directory = root.0.join("directory-link");
        symlink(&root.0, &directory).unwrap();
        assert!(ConfigStamp::capture(&directory.join("config.toml")).is_err());
    }
}
