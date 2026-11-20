// Vendor-neutral native configuration transaction IO and recovery protections.
use super::*;
use rustix::fs::{FlockOperation, flock};
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Component;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STATE_ENTRIES: usize = 256;
const LOCK_NAME: &str = "lock-v2";
const LEGACY_LOCK_NAME: &str = "lock";
fn read_flags() -> Result<i32, Error> {
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        any(target_arch = "arm", target_arch = "aarch64")
    ))]
    return Ok(0x8000 | 0x800);
    #[cfg(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(target_arch = "x86", target_arch = "x86_64")
        ),
        all(target_os = "linux", target_arch = "riscv64")
    ))]
    return Ok(0x20000 | 0x800);
    #[cfg(target_os = "macos")]
    return Ok(0x100 | 0x4);
    #[cfg(not(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(
                target_arch = "arm",
                target_arch = "aarch64",
                target_arch = "x86",
                target_arch = "x86_64"
            )
        ),
        all(target_os = "linux", target_arch = "riscv64"),
        target_os = "macos"
    )))]
    Err(Error::UnsupportedPlatform)
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Target {
    path: PathBuf,
    parent: PathBuf,
    state: PathBuf,
    parent_identity: (u64, u64),
    binding: String,
}

pub(super) struct Snapshot {
    pub(super) bytes: Option<Vec<u8>>,
    identity: Option<Identity>,
}

#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    mode: u32,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    nlink: u64,
}

impl Identity {
    fn of(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            mode: meta.mode(),
            size: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            nlink: meta.nlink(),
        }
    }
}

struct Lock {
    path: PathBuf,
    identity: (u64, u64),
    _file: File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Another thread's child may still hold the same description after
        // fork; close alone would leave our completed transaction locked.
        let _ = flock(&self._file, FlockOperation::NonBlockingUnlock);
    }
}

struct Stage {
    path: PathBuf,
    identity: (u64, u64),
    active: bool,
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.active {
            if let Ok(meta) = fs::symlink_metadata(&self.path) {
                if !meta.file_type().is_symlink() && (meta.dev(), meta.ino()) == self.identity {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

fn validate_model(model: &str) -> Result<(), Error> {
    if model.len() > 512
        || model.is_empty()
        || !model.is_ascii()
        || model
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Error::InvalidModel);
    }
    let Some((provider, name)) = model.split_once('/') else {
        return Err(Error::InvalidModel);
    };
    if provider.is_empty()
        || name.is_empty()
        || provider.len() > 128
        || !provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || name.bytes().any(|b| b"\\\"'?#".contains(&b))
    {
        return Err(Error::InvalidModel);
    }
    Ok(())
}

fn validate_gateway(gateway: SocketAddr) -> Result<(), Error> {
    if gateway.port() == 0 || !gateway.ip().is_loopback() {
        return Err(Error::InvalidGateway);
    }
    // Scoped IPv6 is unnecessary for ::1 and produces a URL with additional
    // platform semantics. Refuse it rather than constructing an invalid URL.
    if let SocketAddr::V6(address) = gateway {
        if address.scope_id() != 0 || address.flowinfo() != 0 {
            return Err(Error::InvalidGateway);
        }
    }
    Ok(())
}

fn target(path: &Path, writable: bool) -> Result<Target, Error> {
    let _ = read_flags()?;
    if !path.is_absolute() {
        return Err(Error::UnsafePath);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| Error::UnsafePath)?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir => (),
            _ => return Err(Error::UnsafePath),
        }
    }
    let name = normalized
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(Error::UnsafePath)?;
    if name.is_empty() || name.len() > 180 || name.chars().any(char::is_control) {
        return Err(Error::UnsafePath);
    }
    let parent = normalized.parent().ok_or(Error::UnsafePath)?.to_path_buf();
    validate_directories(&parent, writable)?;
    let meta = fs::symlink_metadata(&parent).map_err(|_| Error::Read)?;
    let state = parent.join(format!(".{name}.magpie-agent-state"));
    let binding = hex(normalized.as_os_str().as_bytes());
    Ok(Target {
        path: normalized,
        parent,
        state,
        parent_identity: (meta.dev(), meta.ino()),
        binding,
    })
}

fn validate_directories(parent: &Path, writable: bool) -> Result<(), Error> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        let meta = fs::symlink_metadata(&current).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::MissingParent
            } else {
                Error::Read
            }
        })?;
        if meta.file_type().is_symlink() {
            return Err(Error::Symlink);
        }
        if !meta.is_dir() {
            return Err(Error::UnsafePath);
        }
        if meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0 {
            return Err(Error::UnsafePath);
        }
    }
    let meta = fs::symlink_metadata(parent).map_err(|_| Error::Read)?;
    // Ancestors such as /tmp may be sticky, but the immediate container must
    // itself be a trusted directory. We never create it on the user's behalf.
    if meta.mode() & 0o022 != 0 {
        return Err(Error::UnsafePath);
    }
    if writable && meta.mode() & 0o200 == 0 {
        return Err(Error::ReadOnly);
    }
    Ok(())
}

fn recheck_parent(target: &Target, writable: bool) -> Result<(), Error> {
    validate_directories(&target.parent, writable)?;
    let meta = fs::symlink_metadata(&target.parent).map_err(|_| Error::Read)?;
    if (meta.dev(), meta.ino()) != target.parent_identity {
        return Err(Error::OriginalChanged);
    }
    Ok(())
}

fn checked_meta(meta: &Metadata, private: bool, writable: bool, limit: u64) -> Result<(), Error> {
    if meta.file_type().is_symlink() {
        return Err(Error::Symlink);
    }
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(Error::NotAFile);
    }
    if meta.len() > limit {
        return Err(Error::TooLarge);
    }
    if private && meta.mode() & 0o077 != 0 {
        return Err(Error::StateInvalid);
    }
    if meta.mode() & 0o400 == 0 {
        return Err(Error::Read);
    }
    if writable && meta.mode() & 0o200 == 0 {
        return Err(Error::ReadOnly);
    }
    if writable && meta.mode() & 0o022 != 0 {
        return Err(Error::UnsafePath);
    }
    Ok(())
}

fn read_file(path: &Path, private: bool, writable: bool, limit: u64) -> Result<Snapshot, Error> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                bytes: None,
                identity: None,
            });
        }
        Err(_) => return Err(Error::Read),
    };
    checked_meta(&meta, private, writable, limit)?;
    let before = Identity::of(&meta);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(read_flags()?)
        .open(path)
        .map_err(|_| Error::Read)?;
    let opened = file.metadata().map_err(|_| Error::Read)?;
    checked_meta(&opened, private, writable, limit)?;
    if Identity::of(&opened) != before {
        return Err(Error::OriginalChanged);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Read)?;
    if bytes.len() as u64 > limit {
        return Err(Error::TooLarge);
    }
    let after = file.metadata().map_err(|_| Error::Read)?;
    let named = fs::symlink_metadata(path).map_err(|_| Error::OriginalChanged)?;
    if Identity::of(&after) != before
        || Identity::of(&named) != before
        || bytes.len() as u64 != meta.len()
    {
        return Err(Error::OriginalChanged);
    }
    Ok(Snapshot {
        bytes: Some(bytes),
        identity: Some(before),
    })
}

fn read_config(path: &Path, writable: bool) -> Result<Snapshot, Error> {
    read_file(path, false, writable, MAX_CONFIG_BYTES)
}

fn expect_current(target: &Target, expected: &Snapshot) -> Result<(), Error> {
    recheck_parent(target, true)?;
    let current = read_config(&target.path, true)?;
    if current.bytes != expected.bytes || current.identity != expected.identity {
        return Err(Error::OriginalChanged);
    }
    Ok(())
}

fn state_exists(target: &Target) -> Result<bool, Error> {
    match fs::symlink_metadata(&target.state) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::Read),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(Error::Symlink);
            }
            if !meta.is_dir() || meta.mode() & 0o077 != 0 {
                return Err(Error::StateInvalid);
            }
            if meta.dev() != target.parent_identity.0 {
                return Err(Error::UnsafePath);
            }
            if meta.mode() & 0o700 != 0o700 {
                return Err(Error::ReadOnly);
            }
            Ok(true)
        }
    }
}

fn acquire_lock(target: &Target, create: bool) -> Result<Lock, Error> {
    if !state_exists(target)? {
        if !create {
            return Err(Error::NotManaged);
        }
        match DirBuilder::new().mode(0o700).create(&target.state) {
            Ok(()) => sync_dir(&target.parent)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Error::Write),
        }
        state_exists(target)?;
    }
    let guard = if let Some(lock) = existing_lock(target)? {
        lock
    } else {
        // Count both permanent directory entries before creating either.
        // A crash here can leave a valid, empty lock-v2 without its alias.
        ensure_budget(target, 0, 2)?;
        let path = target.state.join(LOCK_NAME);
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                #[cfg(test)]
                crate::test_support::checkpoint(
                    crate::test_support::Point::LockCreated,
                    &target.state,
                )
                .map_err(|_| Error::Write)?;
                lock_file(target, file)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                existing_lock(target)?.ok_or(Error::OriginalChanged)?
            }
            Err(_) => return Err(Error::Write),
        }
    };
    #[cfg(test)]
    crate::test_support::checkpoint(crate::test_support::Point::LockAcquired, &target.state)
        .map_err(|_| Error::Write)?;
    if !check_lock_names(target, guard.identity)? {
        ensure_budget(target, 0, 1)?;
        // link(2) publishes the legacy gate atomically without replacing
        // any old lock. Both names must bind the inode we actually locked.
        fs::hard_link(&guard.path, target.state.join(LEGACY_LOCK_NAME)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::LockBusy
            } else {
                Error::Write
            }
        })?;
        #[cfg(test)]
        crate::test_support::checkpoint(
            crate::test_support::Point::LegacyLockLinked,
            &target.state,
        )
        .map_err(|_| Error::Write)?;
    }
    if !check_lock_names(target, guard.identity)? {
        return Err(Error::OriginalChanged);
    }
    guard._file.sync_all().map_err(|_| Error::Write)?;
    sync_dir(&target.state)?;
    Ok(guard)
}

fn optional_meta(path: &Path) -> Result<Option<Metadata>, Error> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::Read),
    }
}

fn checked_lock_meta(meta: &Metadata) -> Result<(), Error> {
    if meta.file_type().is_symlink() {
        return Err(Error::Symlink);
    }
    if !meta.is_file() || !(1..=2).contains(&meta.nlink()) {
        return Err(Error::NotAFile);
    }
    if meta.mode() & 0o077 != 0 || meta.len() != 0 {
        return Err(Error::StateInvalid);
    }
    if meta.mode() & 0o600 != 0o600 {
        return Err(Error::ReadOnly);
    }
    Ok(())
}

// The only accepted hardlinks in private state are this exact protocol
// pair. An old empty lock has no proof of ownership or process lifetime.
fn check_lock_names(target: &Target, identity: (u64, u64)) -> Result<bool, Error> {
    let named = optional_meta(&target.state.join(LOCK_NAME))?.ok_or(Error::OriginalChanged)?;
    checked_lock_meta(&named)?;
    if (named.dev(), named.ino()) != identity {
        return Err(Error::OriginalChanged);
    }
    match optional_meta(&target.state.join(LEGACY_LOCK_NAME))? {
        Some(legacy)
            if !legacy.file_type().is_symlink() && (legacy.dev(), legacy.ino()) == identity =>
        {
            checked_lock_meta(&legacy)?;
            if named.nlink() != 2 || legacy.nlink() != 2 {
                return Err(Error::StateInvalid);
            }
            Ok(true)
        }
        Some(_) => Err(Error::LockBusy),
        None if named.nlink() == 1 => Ok(false),
        None => Err(Error::StateInvalid),
    }
}

fn lock_file(target: &Target, file: File) -> Result<Lock, Error> {
    let meta = file.metadata().map_err(|_| Error::Read)?;
    checked_lock_meta(&meta)?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            Error::LockBusy
        } else {
            Error::Write
        }
    })?;
    let guard = Lock {
        path: target.state.join(LOCK_NAME),
        identity: (meta.dev(), meta.ino()),
        _file: file,
    };
    check_lock_names(target, guard.identity)?;
    Ok(guard)
}

fn existing_lock(target: &Target) -> Result<Option<Lock>, Error> {
    if !state_exists(target)? {
        return Ok(None);
    }
    let path = target.state.join(LOCK_NAME);
    let Some(meta) = optional_meta(&path)? else {
        return if optional_meta(&target.state.join(LEGACY_LOCK_NAME))?.is_some() {
            Err(Error::LockBusy)
        } else {
            Ok(None)
        };
    };
    checked_lock_meta(&meta)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(read_flags()?)
        .open(&path)
        .map_err(|_| Error::Read)?;
    let opened = file.metadata().map_err(|_| Error::Read)?;
    if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
        return Err(Error::OriginalChanged);
    }
    lock_file(target, file).map(Some)
}

fn recheck_read_lock(target: &Target, lock: Option<&Lock>) -> Result<(), Error> {
    if let Some(lock) = lock {
        check_lock_names(target, lock.identity)?;
    } else if existing_lock(target)?.is_some() {
        // Read-only preparation/status must not create lock files. If a
        // writer established the protocol meanwhile, discard that read.
        return Err(Error::OriginalChanged);
    }
    Ok(())
}

fn stage(target: &Target, bytes: &[u8]) -> Result<Stage, Error> {
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(Error::TooLarge);
    }
    ensure_budget(target, bytes.len(), 1)?;
    for _ in 0..8 {
        let path = target.state.join(format!(".magpie-stage-{}", nonce()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(Error::Write),
        };
        let meta = file.metadata().map_err(|_| Error::Write)?;
        let guard = Stage {
            path,
            identity: (meta.dev(), meta.ino()),
            active: true,
        };
        file.write_all(bytes).map_err(|_| Error::Write)?;
        file.sync_all().map_err(|_| Error::Write)?;
        #[cfg(test)]
        crate::test_support::checkpoint(crate::test_support::Point::StageSynced, &target.state)
            .map_err(|_| Error::Write)?;
        return Ok(guard);
    }
    Err(Error::Write)
}

fn install_stage(stage: &mut Stage, target: &Path) -> Result<(), Error> {
    let meta = fs::symlink_metadata(&stage.path).map_err(|_| Error::Write)?;
    checked_meta(&meta, true, true, MAX_CONFIG_BYTES)?;
    if (meta.dev(), meta.ino()) != stage.identity {
        return Err(Error::OriginalChanged);
    }
    #[cfg(test)]
    crate::test_support::checkpoint(crate::test_support::Point::BeforeInstall, target)
        .map_err(|_| Error::Write)?;
    fs::rename(&stage.path, target).map_err(|_| Error::Write)?;
    stage.active = false;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), Error> {
    #[cfg(test)]
    crate::test_support::checkpoint(crate::test_support::Point::BeforeDirectorySync, path)
        .map_err(|_| Error::Write)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(read_flags()?)
        .open(path)
        .map_err(|_| Error::Write)?;
    if !file.metadata().map_err(|_| Error::Write)?.is_dir() {
        return Err(Error::UnsafePath);
    }
    file.sync_all().map_err(|_| Error::Write)
}

fn ensure_budget(
    target: &Target,
    additional_bytes: usize,
    additional_entries: usize,
) -> Result<(), Error> {
    if !state_exists(target)? {
        return Err(Error::StateInvalid);
    }
    let mut total = additional_bytes as u64;
    if total > MAX_STATE_BYTES || additional_entries > MAX_STATE_ENTRIES {
        return Err(Error::StorageLimit);
    }
    let entries = fs::read_dir(&target.state).map_err(|_| Error::Read)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_STATE_ENTRIES - additional_entries {
            return Err(Error::StorageLimit);
        }
        let entry = entry.map_err(|_| Error::Read)?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|_| Error::Read)?;
        let lock_entry = entry.file_name() == LOCK_NAME || entry.file_name() == LEGACY_LOCK_NAME;
        if lock_entry {
            checked_lock_meta(&meta)?;
            let identity = (meta.dev(), meta.ino());
            if !check_lock_names(target, identity)? && entry.file_name() == LEGACY_LOCK_NAME {
                return Err(Error::StateInvalid);
            }
        }
        if meta.file_type().is_symlink()
            || !meta.is_file()
            || meta.mode() & 0o077 != 0
            || (!lock_entry && meta.nlink() != 1)
        {
            return Err(Error::StateInvalid);
        }
        total = total.checked_add(meta.len()).ok_or(Error::StorageLimit)?;
        if total > MAX_STATE_BYTES {
            return Err(Error::StorageLimit);
        }
    }
    Ok(())
}

fn nonce() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{time:032x}-{:08x}-{:016x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(DIGITS[(byte >> 4) as usize]));
        result.push(char::from(DIGITS[(byte & 15) as usize]));
    }
    result
}

// Journals contain private recovery bytes and are never exposed in Debug/errors.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    original: String,
    canonical: String,
    owned: Vec<Vec<String>>,
    gateway: SocketAddr,
    revision: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "phase", rename_all = "kebab-case", deny_unknown_fields)]
enum Phase {
    Stable {
        record: Option<Record>,
    },
    Pending {
        before: String,
        after: String,
        previous: Option<Record>,
        next: Option<Record>,
    },
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    target: String,
    definition: String,
    state_checksum: String,
    state: Phase,
}

pub(super) struct Plan {
    definition: AgentDefinition,
    target: Target,
    snapshot: Snapshot,
    state_snapshot: Snapshot,
    previous: Option<Record>,
    next: Record,
    after: Vec<u8>,
}
fn state_checksum(state: &Phase) -> Result<String, Error> {
    Ok(hex(&Sha256::digest(
        serde_json::to_vec(state).map_err(|_| Error::StateInvalid)?,
    )))
}
fn make_journal(
    target: &Target,
    definition: &AgentDefinition,
    state: Phase,
) -> Result<Journal, Error> {
    Ok(Journal {
        version: 1,
        target: target.binding.clone(),
        definition: definition_hash(definition)?,
        state_checksum: state_checksum(&state)?,
        state,
    })
}
fn definition_hash(definition: &AgentDefinition) -> Result<String, Error> {
    definition.validate()?;
    let bytes = serde_json::to_vec(definition).map_err(|_| Error::InvalidDefinition)?;
    Ok(hex(&Sha256::digest(bytes)))
}
fn journal_path(target: &Target) -> PathBuf {
    target.state.join("journal.json")
}
fn state_snapshot(target: &Target) -> Result<Snapshot, Error> {
    if !state_exists(target)? {
        return Ok(Snapshot {
            bytes: None,
            identity: None,
        });
    }
    read_file(&journal_path(target), true, false, MAX_CONFIG_BYTES)
}
fn read_journal(
    target: &Target,
    definition: &AgentDefinition,
    snapshot: &Snapshot,
) -> Result<Option<Journal>, Error> {
    let Some(bytes) = snapshot.bytes.as_deref() else {
        return Ok(None);
    };
    document::parse(bytes, ConfigFormat::Json).map_err(|_| Error::StateInvalid)?;
    let journal: Journal = serde_json::from_slice(bytes).map_err(|_| Error::StateInvalid)?;
    if journal.version != 1
        || journal.target != target.binding
        || journal.definition != definition_hash(definition)?
        || journal.state_checksum != state_checksum(&journal.state)?
    {
        return Err(Error::StateInvalid);
    }
    let records: Vec<&Record> = match &journal.state {
        Phase::Stable { record } => record.iter().collect(),
        Phase::Pending {
            before,
            after,
            previous,
            next,
        } => {
            document::parse(before.as_bytes(), definition.format)
                .map_err(|_| Error::StateInvalid)?;
            document::parse(after.as_bytes(), definition.format)
                .map_err(|_| Error::StateInvalid)?;
            previous.iter().chain(next.iter()).collect()
        }
    };
    for record in records {
        validate_gateway(record.gateway).map_err(|_| Error::StateInvalid)?;
        if record.owned.len() > 512 || record.revision.is_empty() {
            return Err(Error::StateInvalid);
        }
        document::parse(record.original.as_bytes(), definition.format)
            .map_err(|_| Error::StateInvalid)?;
        let canonical = document::parse(record.canonical.as_bytes(), definition.format)
            .map_err(|_| Error::StateInvalid)?;
        for path in &record.owned {
            if !definition.fields.iter().any(|field| {
                field.path.len() == path.len()
                    && field.path.iter().zip(path).all(|(a, b)| {
                        a == b
                            || a.split_once("${model}").is_some_and(|(prefix, suffix)| {
                                b.strip_prefix(prefix)
                                    .and_then(|b| b.strip_suffix(suffix))
                                    .is_some_and(|m| validate_model(m).is_ok())
                            })
                    })
            }) || canonical.value.pointer(&pointer(path)).is_none()
            {
                return Err(Error::StateInvalid);
            }
        }
    }
    Ok(Some(journal))
}
fn stable(journal: Option<&Journal>) -> Result<Option<Record>, Error> {
    match journal.map(|j| &j.state) {
        None => Ok(None),
        Some(Phase::Stable { record }) => Ok(record.clone()),
        Some(Phase::Pending { .. }) => Err(Error::RecoveryRequired),
    }
}
fn persist(target: &Target, definition: &AgentDefinition, state: Phase) -> Result<(), Error> {
    let journal = make_journal(target, definition, state)?;
    let bytes = serde_json::to_vec(&journal).map_err(|_| Error::StateInvalid)?;
    let mut staged = stage(target, &bytes)?;
    let _ = read_file(&journal_path(target), true, true, MAX_CONFIG_BYTES)?;
    install_stage(&mut staged, &journal_path(target))?;
    sync_dir(&target.state)
}
fn required(snapshot: &Snapshot) -> Result<&[u8], Error> {
    snapshot.bytes.as_deref().ok_or(Error::MissingConfig)
}
fn same(a: &Snapshot, b: &Snapshot) -> bool {
    a.bytes == b.bytes && a.identity == b.identity
}
fn ensure_owned(
    definition: &AgentDefinition,
    record: &Record,
    current: &[u8],
) -> Result<(), Error> {
    let current = document::parse(current, definition.format)?;
    let written = document::parse(record.canonical.as_bytes(), definition.format)?;
    if record
        .owned
        .iter()
        .any(|p| current.value.pointer(&pointer(p)) != written.value.pointer(&pointer(p)))
    {
        return Err(Error::OriginalChanged);
    }
    Ok(())
}
fn revision(
    snapshot: &Snapshot,
    state: &Snapshot,
    definition: &AgentDefinition,
) -> Result<String, Error> {
    let mut hash = Sha256::new();
    hash.update(b"magpie-plugin-preview-v1\0");
    hash.update(definition_hash(definition)?);
    for s in [snapshot, state] {
        if let Some(i) = &s.identity {
            for n in [
                i.dev,
                i.ino,
                u64::from(i.mode),
                i.size,
                i.mtime as u64,
                i.mtime_nsec as u64,
                i.nlink,
            ] {
                hash.update(n.to_le_bytes());
            }
        }
        if let Some(bytes) = &s.bytes {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        } else {
            hash.update([0]);
        }
    }
    Ok(hex(&hash.finalize()))
}
pub(super) fn selection(definition: &AgentDefinition, path: &Path) -> Result<Selection, Error> {
    #[cfg(test)]
    let _scope = crate::test_support::descriptor_scope();
    definition.validate()?;
    let target = target(path, false)?;
    let lock = existing_lock(&target)?;
    let snapshot = read_config(path, false)?;
    let document = document::parse(required(&snapshot)?, definition.format)?;
    let state = state_snapshot(&target)?;
    let journal = read_journal(&target, definition, &state)?;
    let status = match stable(journal.as_ref()) {
        Err(Error::RecoveryRequired) => ConnectionStatus::RecoveryRequired,
        Err(e) => return Err(e),
        Ok(None) => ConnectionStatus::Unmanaged,
        Ok(Some(record)) => {
            if ensure_owned(definition, &record, required(&snapshot)?).is_ok() {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Drifted
            }
        }
    };
    let model = match document.value.pointer(&pointer(&definition.selection.path)) {
        Some(Value::String(s)) => Some(if status == ConnectionStatus::Connected {
            definition
                .selection
                .strip_prefix
                .as_ref()
                .and_then(|p| s.strip_prefix(p))
                .unwrap_or(s)
                .to_owned()
        } else {
            s.to_owned()
        }),
        None => None,
        _ => return Err(Error::UnsupportedShape),
    };
    recheck_read_lock(&target, lock.as_ref())?;
    Ok(Selection { model, status })
}
pub(super) fn prepare(
    definition: &AgentDefinition,
    path: &Path,
    model: &str,
    gateway: Option<SocketAddr>,
) -> Result<PreparedConnect, Error> {
    #[cfg(test)]
    let _scope = crate::test_support::descriptor_scope();
    definition.validate()?;
    validate_model(model)?;
    let target = target(path, true)?;
    let lock = existing_lock(&target)?;
    let snapshot = read_config(path, true)?;
    let bytes = required(&snapshot)?;
    let document = document::parse(bytes, definition.format)?;
    let state_snapshot = state_snapshot(&target)?;
    let journal = read_journal(&target, definition, &state_snapshot)?;
    let previous = stable(journal.as_ref())?;
    let gateway = match (gateway, previous.as_ref()) {
        (Some(_), Some(_)) => return Err(Error::AlreadyManaged),
        (Some(gateway), None) => {
            for p in &definition.reject_existing {
                if document.value.pointer(&pointer(p)).is_some() {
                    return Err(Error::UnknownProvider);
                }
            }
            gateway
        }
        (None, Some(record)) => {
            ensure_owned(definition, record, bytes)?;
            record.gateway
        }
        (None, None) => return Err(Error::NotManaged),
    };
    validate_gateway(gateway)?;
    let original = previous.as_ref().map_or_else(
        || String::from_utf8(bytes.to_vec()).map_err(|_| Error::InvalidUtf8),
        |r| Ok(r.original.clone()),
    )?;
    let mut canonical = previous.as_ref().map_or_else(
        || original.as_bytes().to_vec(),
        |r| r.canonical.as_bytes().to_vec(),
    );
    let mut owned = previous.as_ref().map_or_else(Vec::new, |r| r.owned.clone());
    let mut after = bytes.to_vec();
    let mut changed_fields = Vec::new();
    let fields = fields(definition, model, gateway);
    for (index, (path, value)) in fields.iter().enumerate() {
        if fields
            .iter()
            .enumerate()
            .any(|(other, (p, _))| index != other && (path.starts_with(p) || p.starts_with(path)))
        {
            return Err(Error::InvalidDefinition);
        }
        if document
            .value
            .pointer(&pointer(path))
            .is_some_and(|old| match value {
                Value::String(_) => !old.is_string(),
                Value::Bool(_) => !old.is_boolean(),
                Value::Number(_) => !old.is_number(),
                Value::Null => !old.is_null(),
                _ => true,
            })
        {
            return Err(Error::UnsupportedShape);
        }
    }
    for (p, value) in fields {
        if !owned.contains(&p) {
            owned.push(p.clone());
        }
        if document.value.pointer(&pointer(&p)) != Some(&value) {
            changed_fields.push(pointer(&p));
        }
        after = document::set(&after, definition.format, &p, Some(&value))?;
        canonical = document::set(&canonical, definition.format, &p, Some(&value))?;
    }
    if owned.len() > 512 {
        return Err(Error::StorageLimit);
    }
    let next = Record {
        original,
        canonical: String::from_utf8(canonical).map_err(|_| Error::InvalidUtf8)?,
        owned,
        gateway,
        revision: nonce(),
    };
    let revision = revision(&snapshot, &state_snapshot, definition)?;
    recheck_read_lock(&target, lock.as_ref())?;
    Ok(PreparedConnect {
        revision,
        changed_fields,
        inner: Plan {
            definition: definition.clone(),
            target,
            snapshot,
            state_snapshot,
            previous,
            next,
            after,
        },
    })
}
fn transact(
    target: &Target,
    definition: &AgentDefinition,
    before: &Snapshot,
    after: &[u8],
    previous: Option<Record>,
    next: Option<Record>,
) -> Result<(), Error> {
    expect_current(target, before)?;
    let before_text =
        String::from_utf8(required(before)?.to_vec()).map_err(|_| Error::InvalidUtf8)?;
    let after_text = String::from_utf8(after.to_vec()).map_err(|_| Error::InvalidUtf8)?;
    let pending = Phase::Pending {
        before: before_text,
        after: after_text,
        previous,
        next: next.clone(),
    };
    let prospective = make_journal(target, definition, pending.clone())?;
    let journal_size = serde_json::to_vec(&prospective)
        .map_err(|_| Error::StateInvalid)?
        .len();
    if journal_size as u64 > MAX_CONFIG_BYTES {
        return Err(Error::TooLarge);
    }
    // Reserve pending/stable publications and the target staging file up front.
    ensure_budget(target, journal_size * 2 + after.len(), 3)?;
    let mut staged = stage(target, after)?;
    persist(target, definition, pending).map_err(|_| Error::RecoveryRequired)?;
    expect_current(target, before).map_err(|_| Error::RecoveryRequired)?;
    install_stage(&mut staged, &target.path).map_err(|_| Error::RecoveryRequired)?;
    sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
    persist(target, definition, Phase::Stable { record: next }).map_err(|_| Error::CommitUncertain)
}
pub(super) fn commit(plan: Plan) -> Result<ConnectionStatus, Error> {
    #[cfg(test)]
    let _scope = crate::test_support::descriptor_scope();
    recheck_parent(&plan.target, true)?;
    let _lock = acquire_lock(&plan.target, true)?;
    if !same(&state_snapshot(&plan.target)?, &plan.state_snapshot) {
        return Err(Error::OriginalChanged);
    }
    expect_current(&plan.target, &plan.snapshot)?;
    if plan.after != *required(&plan.snapshot)? || plan.previous.is_none() {
        transact(
            &plan.target,
            &plan.definition,
            &plan.snapshot,
            &plan.after,
            plan.previous,
            Some(plan.next),
        )?;
    }
    Ok(ConnectionStatus::Connected)
}
fn recover(
    target: &Target,
    definition: &AgentDefinition,
    journal: &Journal,
    current: &[u8],
) -> Result<Option<Record>, Error> {
    match &journal.state {
        Phase::Stable { record } => Ok(record.clone()),
        Phase::Pending {
            before,
            after,
            previous,
            next,
        } => {
            let record = if current == after.as_bytes() {
                next.clone()
            } else if current == before.as_bytes() {
                previous.clone()
            } else {
                return Err(Error::RecoveryRequired);
            };
            // Confirm the observed target name is durable before finalizing an
            // interrupted install whose original parent sync may never have run.
            sync_dir(&target.parent).map_err(|_| Error::RecoveryRequired)?;
            persist(
                target,
                definition,
                Phase::Stable {
                    record: record.clone(),
                },
            )
            .map_err(|_| Error::RecoveryRequired)?;
            Ok(record)
        }
    }
}
pub(super) fn disconnect(
    definition: &AgentDefinition,
    path: &Path,
) -> Result<DisconnectOutcome, Error> {
    #[cfg(test)]
    let _scope = crate::test_support::descriptor_scope();
    definition.validate()?;
    let target = target(path, true)?;
    let _lock = acquire_lock(&target, false)?;
    let snapshot = read_config(path, true)?;
    let bytes = required(&snapshot)?;
    let state = state_snapshot(&target)?;
    let journal = read_journal(&target, definition, &state)?.ok_or(Error::NotManaged)?;
    let record = recover(&target, definition, &journal, bytes)?;
    let Some(record) = record else {
        return Ok(DisconnectOutcome {
            restored_fields: 0,
            preserved_fields: 0,
            config_removed: false,
        });
    };
    let original = document::parse(record.original.as_bytes(), definition.format)?;
    let written = document::parse(record.canonical.as_bytes(), definition.format)?;
    let current = document::parse(bytes, definition.format)?;
    let mut after = bytes.to_vec();
    let mut restored = 0;
    if bytes == record.canonical.as_bytes() {
        after = record.original.as_bytes().to_vec();
        restored = record.owned.len();
    } else {
        for p in &record.owned {
            if current.value.pointer(&pointer(p)) == written.value.pointer(&pointer(p)) {
                after = document::set(
                    &after,
                    definition.format,
                    p,
                    original.value.pointer(&pointer(p)),
                )?;
                restored += 1;
            }
        }
        // Remove only empty containers introduced by this connection, deepest first.
        let mut parents: Vec<Vec<String>> = record
            .owned
            .iter()
            .flat_map(|p| (1..p.len()).map(|i| p[..i].to_vec()))
            .collect();
        parents.sort_by_key(|p| std::cmp::Reverse(p.len()));
        parents.dedup();
        for p in parents {
            if original.value.pointer(&pointer(&p)).is_none()
                && document::parse(&after, definition.format)?
                    .value
                    .pointer(&pointer(&p))
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.is_empty())
            {
                after = document::set(&after, definition.format, &p, None)?;
            }
        }
    }
    transact(
        &target,
        definition,
        &snapshot,
        &after,
        Some(record.clone()),
        None,
    )?;
    Ok(DisconnectOutcome {
        restored_fields: restored,
        preserved_fields: record.owned.len() - restored,
        config_removed: false,
    })
}
