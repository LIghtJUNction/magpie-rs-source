//! Private descriptor-relative state transactions.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod platform {
    use super::super::{MAX_BYTES, PluginError};
    use rustix::fd::OwnedFd;
    use rustix::fs::{
        AtFlags, FileType, FlockOperation, Mode, OFlags, flock, fstat, fsync, mkdirat, open,
        openat, renameat, unlinkat,
    };
    use rustix::io::Errno;
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::path::{Component, Path};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const FILE_FLAGS: OFlags = OFlags::NOFOLLOW
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);

    struct Directory {
        descriptor: OwnedFd,
        filename: OsString,
    }

    impl Directory {
        fn open(path: &Path, create: bool) -> Result<Option<Self>, PluginError> {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|_| PluginError::UnsafePath)?
                    .join(path)
            };
            let filename = absolute
                .file_name()
                .ok_or(PluginError::UnsafePath)?
                .to_owned();
            let parent = absolute.parent().ok_or(PluginError::UnsafePath)?;
            if parent.components().any(|component| {
                !matches!(
                    component,
                    Component::RootDir | Component::CurDir | Component::Normal(_)
                )
            }) {
                return Err(PluginError::UnsafePath);
            }
            let mut directory =
                open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| PluginError::UnsafePath)?;
            for component in parent.components() {
                let name = match component {
                    Component::RootDir | Component::CurDir => continue,
                    Component::Normal(name) => name,
                    _ => return Err(PluginError::UnsafePath),
                };
                match openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()) {
                    Ok(next) => directory = next,
                    Err(Errno::NOENT) if create => {
                        match mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
                            Ok(()) => fsync(&directory).map_err(|_| PluginError::Write)?,
                            Err(Errno::EXIST) => {}
                            Err(_) => return Err(PluginError::Write),
                        }
                        directory = openat(&directory, name, DIRECTORY_FLAGS, Mode::empty())
                            .map_err(|_| PluginError::UnsafePath)?;
                    }
                    Err(Errno::NOENT) => return Ok(None),
                    Err(_) => return Err(PluginError::UnsafePath),
                }
            }
            let metadata = fstat(&directory).map_err(|_| PluginError::Read)?;
            if create && (metadata.st_mode & 0o022 != 0 || metadata.st_mode & 0o200 == 0) {
                return Err(PluginError::UnsafePath);
            }
            Ok(Some(Self {
                descriptor: directory,
                filename,
            }))
        }

        fn read(&self, private: bool) -> Result<Option<Vec<u8>>, PluginError> {
            let descriptor = match openat(
                &self.descriptor,
                &self.filename,
                OFlags::RDONLY | FILE_FLAGS,
                Mode::empty(),
            ) {
                Ok(descriptor) => descriptor,
                Err(Errno::NOENT) => return Ok(None),
                Err(Errno::LOOP) => return Err(PluginError::UnsafePath),
                Err(_) => return Err(PluginError::Read),
            };
            regular_file(&descriptor, private)?;
            let mut bytes = Vec::new();
            File::from(descriptor)
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| PluginError::Read)?;
            if bytes.len() > MAX_BYTES {
                return Err(PluginError::TooLarge);
            }
            Ok(Some(bytes))
        }
    }

    fn regular_file(descriptor: &OwnedFd, private: bool) -> Result<(), PluginError> {
        let metadata = fstat(descriptor).map_err(|_| PluginError::Read)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
            || metadata.st_nlink != 1
        {
            return Err(PluginError::NotAFile);
        }
        if private && metadata.st_mode & 0o077 != 0 {
            return Err(PluginError::UnsafePath);
        }
        if metadata.st_size < 0 || metadata.st_size as u64 > MAX_BYTES as u64 {
            return Err(PluginError::TooLarge);
        }
        Ok(())
    }

    struct Lock(OwnedFd);

    impl Drop for Lock {
        fn drop(&mut self) {
            // Explicit unlock also releases a lock inherited by an unrelated
            // fork before this process has had the opportunity to close it.
            let _ = flock(&self.0, FlockOperation::Unlock);
        }
    }

    pub(in crate::plugins) fn read(path: &Path) -> Result<Option<Vec<u8>>, PluginError> {
        match Directory::open(path, false)? {
            Some(directory) => directory.read(true),
            None => Ok(None),
        }
    }

    pub(in crate::plugins) fn read_manifest(path: &Path) -> Result<Option<Vec<u8>>, PluginError> {
        match Directory::open(path, false)? {
            Some(directory) => directory.read(false),
            None => Ok(None),
        }
    }

    pub(in crate::plugins) fn update<T>(
        path: &Path,
        operation: impl FnOnce(Option<&[u8]>) -> Result<(T, Vec<u8>), PluginError>,
    ) -> Result<T, PluginError> {
        let directory = Directory::open(path, true)?.ok_or(PluginError::UnsafePath)?;
        let mut lock_name = OsString::from(".");
        lock_name.push(&directory.filename);
        lock_name.push(".lock");
        let descriptor = openat(
            &directory.descriptor,
            &lock_name,
            OFlags::RDWR | OFlags::CREATE | FILE_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| PluginError::UnsafePath)?;
        regular_file(&descriptor, true)?;
        flock(&descriptor, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == Errno::WOULDBLOCK {
                PluginError::Busy
            } else {
                PluginError::Write
            }
        })?;
        let _lock = Lock(descriptor);
        let original = directory.read(true)?;
        let (result, bytes) = operation(original.as_deref())?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PluginError::Write)?
            .as_nanos();
        let temporary = format!(
            ".magpie-plugin-{}-{timestamp}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        );
        let staged = openat(
            &directory.descriptor,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | FILE_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| PluginError::Write)?;
        let write = (|| {
            let mut staged = File::from(staged);
            staged.write_all(&bytes).map_err(|_| PluginError::Write)?;
            staged.sync_all().map_err(|_| PluginError::Write)?;
            if directory.read(true)? != original {
                return Err(PluginError::Conflict);
            }
            renameat(
                &directory.descriptor,
                temporary.as_str(),
                &directory.descriptor,
                &directory.filename,
            )
            .map_err(|_| PluginError::Write)?;
            fsync(&directory.descriptor).map_err(|_| PluginError::CommitUncertain)?;
            Ok(result)
        })();
        if write.is_err() {
            // This exact file was created by this call; no name-based sweep.
            let _ = unlinkat(&directory.descriptor, temporary.as_str(), AtFlags::empty());
        }
        write
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
mod platform {
    use super::super::PluginError;
    use std::path::Path;
    pub(in crate::plugins) fn read(_: &Path) -> Result<Option<Vec<u8>>, PluginError> {
        Err(PluginError::UnsupportedPlatform)
    }
    pub(in crate::plugins) fn read_manifest(_: &Path) -> Result<Option<Vec<u8>>, PluginError> {
        Err(PluginError::UnsupportedPlatform)
    }
    pub(in crate::plugins) fn update<T>(
        _: &Path,
        _: impl FnOnce(Option<&[u8]>) -> Result<(T, Vec<u8>), PluginError>,
    ) -> Result<T, PluginError> {
        Err(PluginError::UnsupportedPlatform)
    }
}

pub(super) use platform::{read, read_manifest, update};
