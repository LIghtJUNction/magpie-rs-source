//! Descriptor-relative access keeps reads, locks, tail repair and rotation anchored.
use super::{FILE_LIMIT, RECORD_LIMIT, invalid};
use rustix::{
    fd::OwnedFd,
    fs::{self, FileType, FlockOperation, Mode, OFlags},
    io::Errno,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path},
    time::{Duration, Instant},
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE: OFlags = OFlags::NOFOLLOW
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

fn directory(path: &Path, create: bool) -> io::Result<Option<OwnedFd>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid("usage_unsafe_path"));
    }
    let mut fd = fs::open("/", DIRECTORY, Mode::empty())?;
    let parent = path.parent().ok_or_else(|| invalid("usage_unsafe_path"))?;
    for component in parent.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        fd = match fs::openat(&fd, name, DIRECTORY, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) if !create => return Ok(None),
            Err(_) => return Err(invalid("usage_unsafe_path")),
        };
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid("usage_unsafe_path"))?;
    let next = match fs::openat(&fd, name, DIRECTORY, Mode::empty()) {
        Ok(next) => next,
        Err(Errno::NOENT) if create => {
            let stat = fs::fstat(&fd)?;
            if stat.st_mode & 0o022 != 0 || stat.st_uid != rustix::process::geteuid().as_raw() {
                return Err(invalid("usage_parent_not_private"));
            }
            match fs::mkdirat(&fd, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => fs::fsync(&fd)?,
                Err(Errno::EXIST) => {}
                Err(_) => return Err(invalid("usage_create_failed")),
            }
            fs::openat(&fd, name, DIRECTORY, Mode::empty())?
        }
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(invalid("usage_unsafe_path")),
    };
    let stat = fs::fstat(&next)?;
    if stat.st_mode & 0o077 != 0 || stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(invalid("usage_directory_not_private"));
    }
    Ok(Some(next))
}

fn file(dir: &OwnedFd, name: &str, flags: OFlags) -> io::Result<Option<File>> {
    let fd = match fs::openat(dir, name, flags | FILE, Mode::from_raw_mode(0o600)) {
        Ok(fd) => fd,
        Err(Errno::NOENT) if !flags.contains(OFlags::CREATE) => return Ok(None),
        Err(_) => return Err(invalid("usage_unsafe_file")),
    };
    let stat = fs::fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o077 != 0
    {
        return Err(invalid("usage_file_not_private"));
    }
    if stat.st_size < 0 || stat.st_size as u64 > FILE_LIMIT {
        return Err(invalid("usage_file_too_large"));
    }
    Ok(Some(fd.into()))
}

fn lock(dir: &OwnedFd, create: bool) -> io::Result<File> {
    let flags = OFlags::RDWR
        | if create {
            OFlags::CREATE
        } else {
            OFlags::empty()
        };
    let lock = file(dir, "lock", flags)?.ok_or_else(|| invalid("usage_lock_missing"))?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(lock),
            Err(Errno::WOULDBLOCK) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => return Err(invalid("usage_lock_busy")),
        }
    }
}

pub(super) fn append(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let dir = directory(path, true)?.ok_or_else(|| invalid("usage_directory_missing"))?;
    let _lock = lock(&dir, true)?;
    let mut current = file(
        &dir,
        "current.jsonl",
        OFlags::RDWR | OFlags::CREATE | OFlags::APPEND,
    )?
    .ok_or_else(|| invalid("usage_file_missing"))?;
    // A killed append can leave a bounded partial tail. Remove only that tail,
    // under the same lock as all readers/writers, so a new record cannot join it.
    let length = current.metadata()?.len();
    let tail_len = length.min(RECORD_LIMIT as u64) as usize;
    let mut tail = vec![0; tail_len];
    current.seek(SeekFrom::End(-(tail_len as i64)))?;
    current.read_exact(&mut tail)?;
    let repaired = tail.last().is_some_and(|b| *b != b'\n');
    if repaired {
        let keep = tail
            .iter()
            .rposition(|b| *b == b'\n')
            .map(|index| length - tail_len as u64 + index as u64 + 1)
            .or_else(|| (length <= RECORD_LIMIT as u64).then_some(0))
            .ok_or_else(|| invalid("usage_invalid_tail"))?;
        current.set_len(keep)?;
    }
    if current.metadata()?.len() + bytes.len() as u64 > FILE_LIMIT {
        // A destination symlink is rejected as well, even though rename itself
        // would replace that link rather than follow it.
        let _previous = file(&dir, "previous.jsonl", OFlags::RDONLY)?;
        fs::renameat(&dir, "current.jsonl", &dir, "previous.jsonl")?;
        fs::fsync(&dir)?;
        current = file(
            &dir,
            "current.jsonl",
            OFlags::RDWR | OFlags::CREATE | OFlags::APPEND,
        )?
        .ok_or_else(|| invalid("usage_file_missing"))?;
    }
    current.write_all(bytes)?;
    current.sync_data()?;
    fs::fsync(&dir)?;
    Ok(repaired)
}

pub(super) fn read(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let Some(dir) = directory(path, false)? else {
        return Ok(Vec::new());
    };
    let _lock = lock(&dir, false)?;
    let mut segments = Vec::new();
    for name in ["previous.jsonl", "current.jsonl"] {
        if let Some(file) = file(&dir, name, OFlags::RDONLY)? {
            let mut bytes = Vec::new();
            file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > FILE_LIMIT {
                return Err(invalid("usage_file_too_large"));
            }
            segments.push(bytes);
        }
    }
    Ok(segments)
}
