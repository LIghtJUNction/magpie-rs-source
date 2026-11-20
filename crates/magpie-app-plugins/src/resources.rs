//! Bounded descriptor-relative resources and private recovery-definition bindings.
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod native {
    use rustix::{
        fd::OwnedFd,
        fs::{
            AtFlags, FileType, FlockOperation, Mode, OFlags, flock, fstat, fsync, mkdirat, open,
            openat, renameat, unlinkat,
        },
        io::Errno,
    };
    use std::{
        fs::File,
        io::{Read, Write},
        path::{Component, Path},
        sync::atomic::{AtomicU64, Ordering},
    };
    const FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NONBLOCK);
    fn directory(path: &Path, create: bool) -> Result<Option<OwnedFd>, ()> {
        if !path.is_absolute() {
            return Err(());
        }
        let mut fd = open("/", FLAGS | OFlags::DIRECTORY, Mode::empty()).map_err(|_| ())?;
        for component in path.parent().ok_or(())?.components() {
            let name = match component {
                Component::RootDir => continue,
                Component::Normal(name) => name,
                _ => return Err(()),
            };
            fd = match openat(&fd, name, FLAGS | OFlags::DIRECTORY, Mode::empty()) {
                Ok(next) => next,
                Err(Errno::NOENT) if create => {
                    match mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(_) => return Err(()),
                    }
                    fsync(&fd).map_err(|_| ())?;
                    openat(&fd, name, FLAGS | OFlags::DIRECTORY, Mode::empty()).map_err(|_| ())?
                }
                Err(Errno::NOENT) => return Ok(None),
                Err(_) => return Err(()),
            };
        }
        Ok(Some(fd))
    }
    fn regular(fd: &OwnedFd, limit: usize, private: bool) -> Result<(), ()> {
        let stat = fstat(fd).map_err(|_| ())?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_size < 0
            || stat.st_size as u64 > limit as u64
            || (private && stat.st_mode & 0o077 != 0)
        {
            return Err(());
        }
        Ok(())
    }
    fn read_at(
        directory: &OwnedFd,
        name: &std::ffi::OsStr,
        limit: usize,
        private: bool,
    ) -> Result<Option<String>, ()> {
        let fd = match openat(directory, name, FLAGS, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(()),
        };
        regular(&fd, limit, private)?;
        let mut text = String::new();
        File::from(fd)
            .take(limit as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| ())?;
        if text.len() > limit {
            return Err(());
        }
        Ok(Some(text))
    }
    pub(super) fn read_text(
        path: &Path,
        limit: usize,
        private: bool,
    ) -> Result<Option<String>, ()> {
        match directory(path, false)? {
            Some(dir) => read_at(&dir, path.file_name().ok_or(())?, limit, private),
            None => Ok(None),
        }
    }
    struct Lock(OwnedFd);
    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = flock(&self.0, FlockOperation::Unlock);
        }
    }
    pub(super) fn edit_private(
        path: &Path,
        edit: impl FnOnce(Option<&str>) -> Result<String, ()>,
    ) -> Result<(), ()> {
        const LIMIT: usize = 256 * 1024;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = directory(path, true)?.ok_or(())?;
        let meta = fstat(&dir).map_err(|_| ())?;
        if meta.st_mode & 0o077 != 0 || meta.st_mode & 0o200 == 0 {
            return Err(());
        }
        let name = path.file_name().ok_or(())?;
        let lock = openat(
            &dir,
            ".bindings.lock",
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| ())?;
        regular(&lock, LIMIT, true)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| ())?;
        let _lock = Lock(lock);
        let previous = read_at(&dir, name, LIMIT, true)?;
        let next = edit(previous.as_deref())?;
        if next.len() > LIMIT {
            return Err(());
        }
        let temp = format!(
            ".binding-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let file = openat(
            &dir,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| ())?;
        let result = (|| {
            let mut file = File::from(file);
            file.write_all(next.as_bytes()).map_err(|_| ())?;
            file.sync_all().map_err(|_| ())?;
            if read_at(&dir, name, LIMIT, true)? != previous {
                return Err(());
            }
            renameat(&dir, temp.as_str(), &dir, name).map_err(|_| ())?;
            fsync(&dir).map_err(|_| ())
        })();
        if result.is_err() {
            let _ = unlinkat(&dir, temp.as_str(), AtFlags::empty());
        }
        result
    }
}
pub(super) fn read_text(path: &Path, limit: usize, private: bool) -> Result<Option<String>, ()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        native::read_text(path, limit, private)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, limit, private);
        Err(())
    }
}
pub(super) fn edit_private(
    path: &Path,
    edit: impl FnOnce(Option<&str>) -> Result<String, ()>,
) -> Result<(), ()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        native::edit_private(path, edit)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, edit);
        Err(())
    }
}
