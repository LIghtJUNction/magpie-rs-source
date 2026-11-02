//! Reversible, explicitly requested Codex configuration transactions.
//!
//! This candidate has a native mutation backend for Linux, Android and macOS.
//! Other platforms return [`TransactionError::UnsupportedPlatform`]. It never
//! discovers or reads a user's home directory: callers must supply the target.
//!
//! A private adjacent state directory contains plaintext recovery images. These
//! are local transaction data, **not** an export or an encrypted backup product.
//! The containing directory must already exist and must not be writable by
//! another Unix user/group. Symlink components and nonregular files are refused.
//!
//! A persistent `lock-v2` inode is locked through the OS and hardlinked as `lock`
//! before mutation. Normal guard release explicitly unlocks it. On process death,
//! the lock remains until all inherited descriptors close. Neither name is ever
//! unlinked by this protocol. The legacy name
//! prevents older `create_new` clients from entering. An existing legacy lock
//! without that exact hardlink relationship remains busy and is never adopted.
//!
//! Exact bytes AND file identity are checked again before atomic replacement.
//! Portable filesystem
//! rename is not compare-and-swap: a noncooperating writer can still win the
//! interval between that check and rename. Agent quiescence and native filesystem
//! integration tests remain required before shipping automatic configuration
//! mutation.
//!
//! All new plaintext fragments, including staging files, stay in the private
//! same-filesystem state directory and count against its storage limit. Abandoned
//! fragments are retained rather than deleting files based on their names.

use std::fmt;
use std::net::SocketAddr;
use std::path::Path;

const OWNED_FIELDS: [&str; 7] = [
    "model",
    "model_provider",
    "model_providers.magpie.name",
    "model_providers.magpie.base_url",
    "model_providers.magpie.wire_api",
    "model_providers.magpie.requires_openai_auth",
    "model_providers.magpie.supports_websockets",
];

/// Deliberately contains no underlying IO/parser messages or configuration data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    UnsupportedPlatform,
    UnsafePath,
    MissingParent,
    NotAFile,
    Symlink,
    TooLarge,
    InvalidUtf8,
    InvalidToml,
    ReadOnly,
    Read,
    Write,
    LockBusy,
    UnknownProvider,
    UnsupportedShape,
    InvalidModel,
    InvalidGateway,
    OriginalChanged,
    AlreadyManaged,
    NotManaged,
    StateInvalid,
    RecoveryRequired,
    CommitUncertain,
    StorageLimit,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedPlatform => {
                "secure native configuration mutation is not implemented on this platform"
            }
            Self::UnsafePath => "configuration path or parent directory is unsafe",
            Self::MissingParent => "configuration parent directory does not exist",
            Self::NotAFile => "configuration or recovery image is not a regular single-link file",
            Self::Symlink => "symlink configuration paths or recovery files are refused",
            Self::TooLarge => "configuration exceeds the transaction size limit",
            Self::InvalidUtf8 => "configuration is not valid UTF-8",
            Self::InvalidToml => "configuration is not valid TOML; original bytes were preserved",
            Self::ReadOnly => "configuration or containing directory is read-only",
            Self::Read => "configuration or recovery state could not be read",
            Self::Write => "transaction data could not be persisted before target replacement",
            Self::LockBusy => {
                "another transaction or an unrecognized legacy lock blocks this operation"
            }
            Self::UnknownProvider => {
                "an unowned Magpie provider already exists; explicit reconciliation is required"
            }
            Self::UnsupportedShape => "a managed configuration field has an unsupported TOML shape",
            Self::InvalidModel => {
                "model must be a bounded provider/model identifier without whitespace or control characters"
            }
            Self::InvalidGateway => {
                "gateway must be a literal loopback address with a nonzero port"
            }
            Self::OriginalChanged => {
                "configuration changed since it was read; no stale snapshot was applied"
            }
            Self::AlreadyManaged => {
                "this configuration is already managed; disconnect before connecting again"
            }
            Self::NotManaged => "no active Magpie ownership record exists for this configuration",
            Self::StateInvalid => {
                "private transaction state is invalid or does not belong to this target"
            }
            Self::RecoveryRequired => {
                "a pending transaction requires recovery; keep the private recovery files"
            }
            Self::CommitUncertain => {
                "target replacement may have completed; inspect recovery status before retrying"
            }
            Self::StorageLimit => {
                "private transaction storage limit reached; review retained recovery files"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for TransactionError {}

/// Configuration ownership only; this is never a gateway/network health claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Unmanaged,
    Connected,
    Drifted,
    RecoveryRequired,
}

impl ConnectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::Connected => "connected",
            Self::Drifted => "drifted",
            Self::RecoveryRequired => "recovery-required",
        }
    }
}

/// Counts describe the seven owned scalar fields, including externally deleted
/// fields that are deliberately preserved as absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisconnectOutcome {
    pub restored_fields: usize,
    pub preserved_fields: usize,
    pub config_removed: bool,
}

/// Opaque, single-use plan. Original bytes are held privately for the final
/// precondition check; neither Debug nor public metadata exposes their values.
pub struct PreparedConnect {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    inner: native::Plan,
}

impl PreparedConnect {
    pub fn config_existed(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        return self.inner.original.bytes.is_some();
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        false
    }

    /// Field names only. Values may be private and are intentionally omitted.
    pub fn changed_fields(&self) -> &[&'static str] {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        return &self.inner.changed_fields;
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        &[]
    }
}

impl fmt::Debug for PreparedConnect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedConnect")
            .field("config_existed", &self.config_existed())
            .field("changed_fields", &self.changed_fields())
            .finish_non_exhaustive()
    }
}

/// A read-only model-change preview for an existing, unchanged connection.
/// The first connection's original configuration remains the recovery point.
pub struct PreparedModelUpdate {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    inner: native::UpdatePlan,
}

impl PreparedModelUpdate {
    /// Only `model` can change; private values are never included.
    pub fn changed_fields(&self) -> &[&'static str] {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        return &self.inner.changed_fields;
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        &[]
    }

    /// Opaque connection version for a caller's preview/apply precondition.
    pub fn revision(&self) -> &str {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        return &self.inner.revision;
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        ""
    }

    /// The existing owned loopback endpoint. Updating a model cannot replace it.
    pub fn gateway_addr(&self) -> SocketAddr {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        return self.inner.gateway;
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        SocketAddr::from(([127, 0, 0, 1], 0))
    }
}

impl fmt::Debug for PreparedModelUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedModelUpdate")
            .field("changed_fields", &self.changed_fields())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUpdateOutcome {
    pub status: ConnectionStatus,
    pub revision: String,
    pub changed: bool,
}

/// Preview a change to the active model without changing any files. A missing,
/// drifted, or interrupted ownership record never authorizes this operation.
pub fn prepare_model_update(
    path: &Path,
    model: &str,
) -> Result<PreparedModelUpdate, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::prepare_update(path, model).map(|inner| PreparedModelUpdate { inner });
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, model);
        Err(TransactionError::UnsupportedPlatform)
    }
}

/// Apply a single-use preview only while both its ownership revision and exact
/// target snapshot still match. A successful no-op does not write any files.
pub fn commit_model_update(
    plan: PreparedModelUpdate,
) -> Result<ModelUpdateOutcome, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::commit_update(plan.inner);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = plan;
        Err(TransactionError::UnsupportedPlatform)
    }
}

/// Read-only preparation. The existing parent directory is required; this
/// function does not create configuration, locks, state or backup files.
pub fn prepare_connect(
    path: &Path,
    model: &str,
    gateway: SocketAddr,
) -> Result<PreparedConnect, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::prepare(path, model, gateway).map(|inner| PreparedConnect { inner });
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, model, gateway);
        Err(TransactionError::UnsupportedPlatform)
    }
}

/// Persist recovery images and a pending journal before atomically replacing
/// the target. Replacement files are private (0600), including on disconnect.
pub fn commit_connect(plan: PreparedConnect) -> Result<ConnectionStatus, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::commit(plan.inner);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = plan;
        Err(TransactionError::UnsupportedPlatform)
    }
}

/// Restore only values that still equal the most recent Magpie values. Unrelated
/// changes and drifted owned fields remain. Also finalizes an interrupted
/// transaction when the target exactly matches a recorded before/after image;
/// ambiguous recovery never overwrites the target.
pub fn disconnect(path: &Path) -> Result<DisconnectOutcome, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::disconnect(path);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = path;
        Err(TransactionError::UnsupportedPlatform)
    }
}

pub fn connection_status(path: &Path) -> Result<ConnectionStatus, TransactionError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::status(path);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = path;
        Err(TransactionError::UnsupportedPlatform)
    }
}

#[cfg(test)]
#[doc(hidden)]
pub mod test_support {
    use super::TransactionError;
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

    pub const CRASH_EXIT: i32 = 86;

    // CLOEXEC closes inherited descriptors at exec, not fork. Keep test-worker
    // spawning out of parent descriptor lifetimes without serializing
    // independent transactions or the lifetime of a paused worker.
    static FORK_GATE: RwLock<()> = RwLock::new(());

    struct DescriptorScopes {
        depth: usize,
        guard: Option<RwLockReadGuard<'static, ()>>,
    }

    thread_local! {
        static DESCRIPTOR_SCOPES: RefCell<DescriptorScopes> = const {
            RefCell::new(DescriptorScopes { depth: 0, guard: None })
        };
    }

    /// Prevent unrelated test workers from inheriting this thread's lock FDs.
    #[derive(Debug)]
    pub struct DescriptorScope(PhantomData<Rc<()>>);

    /// Nestable on one thread; operations on different threads share the gate.
    pub fn descriptor_scope() -> DescriptorScope {
        DESCRIPTOR_SCOPES.with(|scopes| {
            let mut scopes = scopes.borrow_mut();
            if scopes.depth == 0 {
                scopes.guard = Some(FORK_GATE.read().expect("test fork gate poisoned"));
            }
            scopes.depth += 1;
        });
        DescriptorScope(PhantomData)
    }

    impl Drop for DescriptorScope {
        fn drop(&mut self) {
            DESCRIPTOR_SCOPES.with(|scopes| {
                let mut scopes = scopes.borrow_mut();
                scopes.depth -= 1;
                if scopes.depth == 0 {
                    scopes.guard = None;
                }
            });
        }
    }

    /// Hold only through Command::spawn, releasing before waiting on the worker.
    pub fn spawn_scope() -> RwLockWriteGuard<'static, ()> {
        DESCRIPTOR_SCOPES.with(|scopes| {
            assert_eq!(
                scopes.borrow().depth,
                0,
                "a test must close its transaction descriptors before spawning a worker"
            );
        });
        FORK_GATE.write().expect("test fork gate poisoned")
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Point {
        LockCreated,
        LockAcquired,
        LegacyLockLinked,
        StageSynced,
        ImageSynced,
        BeforeInstall,
        BeforeDirectorySync,
    }

    pub enum Action {
        Fail,
        Exit,
        /// Notify the parent, which kills this process without running Drop.
        Pause,
    }

    struct Fault {
        point: Point,
        path: PathBuf,
        remaining: usize,
        action: Action,
    }

    thread_local! {
        static FAULT: RefCell<Option<Fault>> = const { RefCell::new(None) };
    }

    pub fn arm(point: Point, path: &Path, occurrence: usize, action: Action) {
        assert!(occurrence > 0, "fault occurrence must be nonzero");
        FAULT.with(|fault| {
            *fault.borrow_mut() = Some(Fault {
                point,
                path: path.to_path_buf(),
                remaining: occurrence,
                action,
            });
        });
    }

    pub fn checkpoint(point: Point, path: &Path) -> Result<(), TransactionError> {
        let action = FAULT.with(|fault| {
            let mut fault = fault.borrow_mut();
            let armed = fault.as_mut()?;
            if armed.point != point || armed.path != path {
                return None;
            }
            armed.remaining -= 1;
            if armed.remaining == 0 {
                fault.take().map(|fault| fault.action)
            } else {
                None
            }
        });
        match action {
            Some(Action::Fail) => Err(TransactionError::Write),
            Some(Action::Exit) => std::process::exit(CRASH_EXIT),
            Some(Action::Pause) => {
                use std::io::Write;
                println!("magpie-transaction-checkpoint");
                std::io::stdout().flush().expect("flush fixture checkpoint");
                loop {
                    std::thread::park();
                }
            }
            None => Ok(()),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod native {
    use super::{
        ConnectionStatus, DisconnectOutcome, ModelUpdateOutcome, OWNED_FIELDS,
        TransactionError as Error,
    };
    use rustix::fs::{FlockOperation, flock};
    use sha2::{Digest, Sha256};
    use std::ffi::OsStr;
    use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use toml_edit::{DocumentMut, Item, Table, Value, value};

    const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
    const MAX_JOURNAL_BYTES: u64 = 16 * 1024;
    const MAX_STATE_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_STATE_ENTRIES: usize = 256;
    const LOCK_NAME: &str = "lock-v2";
    const LEGACY_LOCK_NAME: &str = "lock";
    const PROVIDER_KEYS: [&str; 5] = [
        "name",
        "base_url",
        "wire_api",
        "requires_openai_auth",
        "supports_websockets",
    ];

    // O_NOFOLLOW | O_NONBLOCK. O_NONBLOCK prevents an adversarially replaced
    // FIFO/device from hanging open before its descriptor can be validated.
    // These flags are platform ABI values; all unsupported targets fail closed.
    // ARM/AArch64 Linux use a different O_NOFOLLOW value from x86/RISC-V.
    // Audited against corresponding rust-lang/libc platform definitions.
    // Another CPU needs an explicit ABI entry, never an assumed generic value.
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

    pub(super) struct Plan {
        target: Target,
        pub(super) original: Snapshot,
        connected: Vec<u8>,
        pub(super) changed_fields: Vec<&'static str>,
    }

    pub(super) struct UpdatePlan {
        target: Target,
        previous: Journal,
        before: Snapshot,
        after: Vec<u8>,
        connected: Vec<u8>,
        pub(super) gateway: SocketAddr,
        pub(super) revision: String,
        pub(super) changed_fields: Vec<&'static str>,
    }

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

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Prepared,
        Connected,
        UpdatePrepared,
        DisconnectPrepared,
        Disconnected,
        Aborted,
    }

    #[derive(Clone, PartialEq, Eq)]
    struct Journal {
        session: String,
        connected_session: String,
        update_session: String,
        disconnect_session: String,
        phase: Phase,
        original_exists: bool,
        disconnect_before_exists: bool,
        disconnected_exists: bool,
        restored: usize,
        preserved: usize,
        removed: bool,
    }

    impl Journal {
        fn image_path(&self, target: &Target, suffix: &str) -> PathBuf {
            let session = match suffix {
                "disconnect-before" | "disconnected" => &self.disconnect_session,
                "update-before" | "update-after" => &self.update_session,
                "connected" => &self.connected_session,
                _ => &self.session,
            };
            target.state.join(format!("{session}-{suffix}.toml"))
        }

        fn outcome(&self) -> DisconnectOutcome {
            DisconnectOutcome {
                restored_fields: self.restored,
                preserved_fields: self.preserved,
                config_removed: self.removed,
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

    pub(super) fn prepare(path: &Path, model: &str, gateway: SocketAddr) -> Result<Plan, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        validate_model(model)?;
        validate_gateway(gateway)?;
        let target = target(path, true)?;
        let lock = existing_lock(&target)?;
        require_inactive(read_journal(&target)?.as_ref())?;
        let original = read_config(&target.path, true)?;
        let connected = build_connected(original.bytes.as_deref(), model, gateway)?;
        let before = parse_optional(original.bytes.as_deref())?;
        let after = parse(&connected)?;
        let changed_fields = OWNED_FIELDS
            .iter()
            .enumerate()
            .filter_map(|(index, name)| (!owned_equal(&before, &after, index)).then_some(*name))
            .collect();
        recheck_read_lock(&target, lock.as_ref())?;
        Ok(Plan {
            target,
            original,
            connected,
            changed_fields,
        })
    }

    pub(super) fn commit(plan: Plan) -> Result<ConnectionStatus, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        let target = plan.target;
        recheck_parent(&target, true)?;
        let _lock = acquire_lock(&target, true)?;
        let previous = read_journal(&target)?;
        require_inactive(previous.as_ref())?;
        expect_current(&target, &plan.original)?;
        // Include both images, the config stage and both journal publications
        // in the logical budget before replacing the target.
        ensure_budget(
            &target,
            plan.original.bytes.as_ref().map_or(0, Vec::len)
                + 2 * plan.connected.len()
                + 2 * MAX_JOURNAL_BYTES as usize,
            5,
        )?;
        let mut staged = stage(&target, &plan.connected)?;
        let session = nonce();
        let mut journal = Journal {
            connected_session: session.clone(),
            session,
            update_session: String::new(),
            disconnect_session: String::new(),
            phase: Phase::Prepared,
            original_exists: plan.original.bytes.is_some(),
            disconnect_before_exists: false,
            disconnected_exists: false,
            restored: 0,
            preserved: 0,
            removed: false,
        };
        write_new_private(
            &target,
            &journal.image_path(&target, "original"),
            plan.original.bytes.as_deref().unwrap_or_default(),
        )?;
        write_new_private(
            &target,
            &journal.image_path(&target, "connected"),
            &plan.connected,
        )?;
        sync_dir(&target.state)?;
        persist_journal(&target, &journal)?;
        if let Err(error) = expect_current(&target, &plan.original) {
            journal.phase = Phase::Aborted;
            persist_journal(&target, &journal).map_err(|_| Error::RecoveryRequired)?;
            return Err(error);
        }
        // This is intentionally the last check before rename, not a claim of
        // filesystem compare-and-swap against noncooperating writers.
        if let Err(error) = install_stage(&mut staged, &target.path) {
            journal.phase = Phase::Aborted;
            persist_journal(&target, &journal).map_err(|_| Error::RecoveryRequired)?;
            return Err(error);
        }
        sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
        journal.phase = Phase::Connected;
        persist_journal(&target, &journal).map_err(|_| Error::CommitUncertain)?;
        if let Some(previous) = previous {
            discard_finished_images(&target, &previous);
        }
        inspect_connected(&target, &plan.connected).map_err(|_| Error::CommitUncertain)
    }

    pub(super) fn prepare_update(path: &Path, model: &str) -> Result<UpdatePlan, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        validate_model(model)?;
        let target = target(path, true)?;
        let lock = existing_lock(&target)?;
        let previous = read_journal(&target)?.ok_or(Error::NotManaged)?;
        match previous.phase {
            Phase::Connected => (),
            Phase::Disconnected | Phase::Aborted => return Err(Error::NotManaged),
            _ => return Err(Error::RecoveryRequired),
        }
        let (original, connected) = connection_images(&target, &previous)?;
        let written = parse(&connected).map_err(|_| Error::StateInvalid)?;
        let gateway = connected_gateway(&written)?;
        let before = read_config(&target.path, true)?;
        let bytes = before.bytes.as_deref().ok_or(Error::OriginalChanged)?;
        let document = parse(bytes)?;
        if !(0..OWNED_FIELDS.len()).all(|i| owned_equal(&document, &written, i)) {
            return Err(Error::OriginalChanged);
        }
        let changed = document.get("model").and_then(Item::as_str) != Some(model);
        // Actual edits retain unrelated user changes. The canonical ownership
        // image remains reproducible from the first-connect recovery image.
        let after = replace_model(bytes, model)?;
        let connected = build_connected(original.as_deref(), model, gateway)?;
        recheck_read_lock(&target, lock.as_ref())?;
        Ok(UpdatePlan {
            target,
            revision: connection_revision(&previous, &before),
            previous,
            before,
            after,
            connected,
            gateway,
            changed_fields: if changed { vec!["model"] } else { Vec::new() },
        })
    }

    pub(super) fn commit_update(plan: UpdatePlan) -> Result<ModelUpdateOutcome, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        let target = plan.target;
        recheck_parent(&target, true)?;
        let _lock = acquire_lock(&target, false)?;
        if read_journal(&target)?.as_ref() != Some(&plan.previous) {
            return Err(Error::OriginalChanged);
        }
        expect_current(&target, &plan.before)?;
        if plan.changed_fields.is_empty() {
            return Ok(ModelUpdateOutcome {
                status: ConnectionStatus::Connected,
                revision: plan.revision,
                changed: false,
            });
        }
        ensure_budget(
            &target,
            plan.before.bytes.as_ref().map_or(0, Vec::len)
                + 2 * plan.after.len()
                + plan.connected.len()
                + 2 * MAX_JOURNAL_BYTES as usize,
            6,
        )?;
        let mut staged = stage(&target, &plan.after)?;
        let mut pending = plan.previous.clone();
        pending.phase = Phase::UpdatePrepared;
        pending.update_session = nonce();
        let mut next = pending.clone();
        next.connected_session = pending.update_session.clone();
        next.update_session.clear();
        next.phase = Phase::Connected;
        write_new_private(
            &target,
            &next.image_path(&target, "connected"),
            &plan.connected,
        )?;
        write_new_private(
            &target,
            &pending.image_path(&target, "update-before"),
            plan.before.bytes.as_deref().ok_or(Error::StateInvalid)?,
        )?;
        write_new_private(
            &target,
            &pending.image_path(&target, "update-after"),
            &plan.after,
        )?;
        sync_dir(&target.state)?;
        // Publication may have reached rename even if its directory sync fails.
        // Keep every referenced image for explicit recovery in either case.
        persist_journal(&target, &pending).map_err(|_| Error::RecoveryRequired)?;
        if let Err(error) = expect_current(&target, &plan.before)
            .and_then(|()| install_stage(&mut staged, &target.path))
        {
            persist_journal(&target, &plan.previous).map_err(|_| Error::RecoveryRequired)?;
            discard_update_images(&target, &pending, false);
            return Err(error);
        }
        sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
        persist_journal(&target, &next).map_err(|_| Error::CommitUncertain)?;
        discard_update_images(&target, &pending, true);
        let current = read_config(&target.path, false).map_err(|_| Error::CommitUncertain)?;
        Ok(ModelUpdateOutcome {
            status: inspect_snapshot_connected(&current, &plan.connected)
                .map_err(|_| Error::CommitUncertain)?,
            revision: connection_revision(&next, &current),
            changed: true,
        })
    }

    pub(super) fn status(path: &Path) -> Result<ConnectionStatus, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        let target = target(path, false)?;
        let lock = existing_lock(&target)?;
        let result = match read_journal(&target)? {
            None => ConnectionStatus::Unmanaged,
            Some(journal) => match journal.phase {
                Phase::Disconnected | Phase::Aborted => ConnectionStatus::Unmanaged,
                Phase::Prepared | Phase::UpdatePrepared | Phase::DisconnectPrepared => {
                    ConnectionStatus::RecoveryRequired
                }
                Phase::Connected => {
                    let (_, connected) = connection_images(&target, &journal)?;
                    inspect_connected(&target, &connected)?
                }
            },
        };
        recheck_read_lock(&target, lock.as_ref())?;
        Ok(result)
    }

    pub(super) fn disconnect(path: &Path) -> Result<DisconnectOutcome, Error> {
        #[cfg(test)]
        let _descriptor_scope = super::test_support::descriptor_scope();
        let target = target(path, true)?;
        let _lock = acquire_lock(&target, false)?;
        let mut journal = read_journal(&target)?.ok_or(Error::NotManaged)?;
        if matches!(journal.phase, Phase::Disconnected | Phase::Aborted) {
            return Err(Error::NotManaged);
        }
        if journal.phase == Phase::DisconnectPrepared {
            return resume_disconnect(&target, &mut journal);
        }
        if journal.phase == Phase::UpdatePrepared {
            resume_update(&target, &mut journal)?;
        }
        let (original, connected) = connection_images(&target, &journal)?;
        let current = read_config(&target.path, true)?;
        if journal.phase == Phase::Prepared {
            if current.bytes == original {
                journal.phase = Phase::Aborted;
                persist_journal(&target, &journal)?;
                return Ok(DisconnectOutcome {
                    restored_fields: 0,
                    preserved_fields: 0,
                    config_removed: false,
                });
            }
            if current.bytes.as_deref() != Some(connected.as_slice()) {
                return Err(Error::RecoveryRequired);
            }
        }
        let (restored, outcome) =
            restore_owned(original.as_deref(), &connected, current.bytes.as_deref())?;
        // Reserve the images, replacement stage and both journal publications.
        ensure_budget(
            &target,
            current.bytes.as_ref().map_or(0, Vec::len)
                + 2 * restored.as_ref().map_or(0, Vec::len)
                + 2 * MAX_JOURNAL_BYTES as usize,
            5,
        )?;
        // A crash before publishing the disconnect journal can leave partial
        // private images. A fresh attempt uses fresh create_new names, so those
        // unreferenced files never block or become a source of restoration.
        journal.disconnect_session = nonce();
        write_new_private(
            &target,
            &journal.image_path(&target, "disconnect-before"),
            current.bytes.as_deref().unwrap_or_default(),
        )?;
        write_new_private(
            &target,
            &journal.image_path(&target, "disconnected"),
            restored.as_deref().unwrap_or_default(),
        )?;
        sync_dir(&target.state)?;
        journal.phase = Phase::DisconnectPrepared;
        journal.disconnect_before_exists = current.bytes.is_some();
        journal.disconnected_exists = restored.is_some();
        journal.restored = outcome.restored_fields;
        journal.preserved = outcome.preserved_fields;
        journal.removed = outcome.config_removed;
        persist_journal(&target, &journal)?;
        apply_disconnect(&target, &mut journal, &current, restored.as_deref())?;
        Ok(outcome)
    }

    fn resume_update(target: &Target, journal: &mut Journal) -> Result<(), Error> {
        let pending = journal.clone();
        let (_, connected) = connection_images(target, &pending)?;
        let written = parse(&connected).map_err(|_| Error::StateInvalid)?;
        let before =
            stored_image(target, &pending, "update-before", true)?.ok_or(Error::StateInvalid)?;
        let after =
            stored_image(target, &pending, "update-after", true)?.ok_or(Error::StateInvalid)?;
        let before_document = parse(&before).map_err(|_| Error::StateInvalid)?;
        if !(0..OWNED_FIELDS.len()).all(|i| owned_equal(&before_document, &written, i)) {
            return Err(Error::StateInvalid);
        }
        let mut next = pending.clone();
        next.connected_session = pending.update_session.clone();
        next.update_session.clear();
        next.phase = Phase::Connected;
        let (_, next_connected) = connection_images(target, &next)?;
        let next_document = parse(&next_connected).map_err(|_| Error::StateInvalid)?;
        let model = next_document
            .get("model")
            .and_then(Item::as_str)
            .ok_or(Error::StateInvalid)?;
        if !(1..OWNED_FIELDS.len()).all(|i| owned_equal(&written, &next_document, i))
            || replace_model(&before, model).map_err(|_| Error::StateInvalid)? != after
        {
            return Err(Error::StateInvalid);
        }
        let current = read_config(&target.path, true)?;
        let installed = if current.bytes.as_deref() == Some(after.as_slice()) {
            true
        } else if current.bytes.as_deref() == Some(before.as_slice()) {
            false
        } else {
            return Err(Error::RecoveryRequired);
        };
        ensure_budget(target, MAX_JOURNAL_BYTES as usize, 1)?;
        if installed {
            sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
        } else {
            next.connected_session = pending.connected_session.clone();
        }
        persist_journal(target, &next).map_err(|_| Error::CommitUncertain)?;
        discard_update_images(target, &pending, installed);
        *journal = next;
        Ok(())
    }

    fn resume_disconnect(
        target: &Target,
        journal: &mut Journal,
    ) -> Result<DisconnectOutcome, Error> {
        // Both images are durable before this phase is published. Exact image
        // matching is required; ambiguous external changes need manual review.
        let before = stored_image(
            target,
            journal,
            "disconnect-before",
            journal.disconnect_before_exists,
        )?;
        let after = stored_image(target, journal, "disconnected", journal.disconnected_exists)?;
        let (original, connected) = connection_images(target, journal)?;
        let (expected, outcome) = restore_owned(original.as_deref(), &connected, before.as_deref())
            .map_err(|_| Error::StateInvalid)?;
        if expected != after || outcome != journal.outcome() {
            return Err(Error::StateInvalid);
        }
        let current = read_config(&target.path, true)?;
        if current.bytes == after {
            ensure_budget(target, MAX_JOURNAL_BYTES as usize, 1)?;
            // Observing the after-image does not prove the interrupted rename
            // or unlink reached disk. Order that sync before the final journal.
            sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
            journal.phase = Phase::Disconnected;
            persist_journal(target, journal).map_err(|_| Error::CommitUncertain)?;
            return Ok(journal.outcome());
        }
        if current.bytes != before {
            return Err(Error::RecoveryRequired);
        }
        apply_disconnect(target, journal, &current, after.as_deref())?;
        Ok(journal.outcome())
    }

    fn apply_disconnect(
        target: &Target,
        journal: &mut Journal,
        current: &Snapshot,
        after: Option<&[u8]>,
    ) -> Result<(), Error> {
        ensure_budget(
            target,
            after.map_or(0, <[u8]>::len) + MAX_JOURNAL_BYTES as usize,
            2,
        )?;
        let mut staged = after
            .filter(|_| current.bytes.as_deref() != after)
            .map(|bytes| stage(target, bytes))
            .transpose()?;
        expect_current(target, current)?;
        if current.bytes.as_deref() != after {
            if let Some(staged) = staged.as_mut() {
                install_stage(staged, &target.path)?;
            } else {
                #[cfg(test)]
                super::test_support::checkpoint(
                    super::test_support::Point::BeforeInstall,
                    &target.path,
                )?;
                fs::remove_file(&target.path).map_err(|_| Error::Write)?;
            }
            sync_dir(&target.parent).map_err(|_| Error::CommitUncertain)?;
        }
        journal.phase = Phase::Disconnected;
        persist_journal(target, journal).map_err(|_| Error::CommitUncertain)
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

    fn build_connected(
        original: Option<&[u8]>,
        model: &str,
        gateway: SocketAddr,
    ) -> Result<Vec<u8>, Error> {
        let mut document = parse_optional(original)?;
        for key in ["model", "model_provider"] {
            if document
                .get(key)
                .is_some_and(|item| item.as_str().is_none())
            {
                return Err(Error::UnsupportedShape);
            }
        }
        if let Some(providers) = document.get("model_providers") {
            let providers = providers.as_table().ok_or(Error::UnsupportedShape)?;
            if providers.contains_key("magpie") {
                return Err(Error::UnknownProvider);
            }
        }
        replace_value(document.as_table_mut(), "model", Value::from(model));
        replace_value(
            document.as_table_mut(),
            "model_provider",
            Value::from("magpie"),
        );
        if !document.contains_key("model_providers") {
            let mut providers = Table::new();
            providers.set_implicit(true);
            document["model_providers"] = Item::Table(providers);
        }
        let mut provider = Table::new();
        provider["name"] = value("magpie");
        provider["base_url"] = value(format!("http://{gateway}/v1"));
        provider["wire_api"] = value("responses");
        provider["requires_openai_auth"] = value(false);
        provider["supports_websockets"] = value(false);
        document["model_providers"]["magpie"] = Item::Table(provider);
        let bytes = document.to_string().into_bytes();
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(bytes)
    }

    fn replace_value(table: &mut Table, key: &str, mut next: Value) {
        if let Some(existing) = table.get_mut(key) {
            if let Some(previous) = existing.as_value() {
                *next.decor_mut() = previous.decor().clone();
            }
            // Table::insert formats an existing key and drops its key-level
            // decoration. Mutate its value in place to retain preceding comments,
            // quoted key spelling and spacing around the equals sign.
            *existing = Item::Value(next);
        } else {
            table.insert(key, Item::Value(next));
        }
    }

    fn replace_model(bytes: &[u8], model: &str) -> Result<Vec<u8>, Error> {
        let mut document = parse(bytes)?;
        if document.get("model").and_then(Item::as_str).is_none() {
            return Err(Error::UnsupportedShape);
        }
        replace_value(document.as_table_mut(), "model", Value::from(model));
        let bytes = document.to_string().into_bytes();
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(bytes)
    }

    fn owned_item(document: &DocumentMut, index: usize) -> Option<&Item> {
        match index {
            0 => document.get("model"),
            1 => document.get("model_provider"),
            _ => document
                .get("model_providers")?
                .as_table()?
                .get("magpie")?
                .as_table()?
                .get(PROVIDER_KEYS[index - 2]),
        }
    }

    fn owned_equal(current: &DocumentMut, written: &DocumentMut, index: usize) -> bool {
        let (Some(current), Some(written)) =
            (owned_item(current, index), owned_item(written, index))
        else {
            return false;
        };
        if let Some(value) = written.as_str() {
            current.as_str() == Some(value)
        } else if let Some(value) = written.as_bool() {
            current.as_bool() == Some(value)
        } else {
            false
        }
    }

    fn inspect_connected(target: &Target, connected: &[u8]) -> Result<ConnectionStatus, Error> {
        let current = read_config(&target.path, false)?;
        inspect_snapshot_connected(&current, connected)
    }

    fn inspect_snapshot_connected(
        current: &Snapshot,
        connected: &[u8],
    ) -> Result<ConnectionStatus, Error> {
        let Some(bytes) = current.bytes.as_deref() else {
            return Ok(ConnectionStatus::Drifted);
        };
        let current = parse(bytes)?;
        let written = parse(connected)?;
        Ok(
            if (0..OWNED_FIELDS.len()).all(|i| owned_equal(&current, &written, i)) {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Drifted
            },
        )
    }

    fn connection_revision(journal: &Journal, snapshot: &Snapshot) -> String {
        // This cross-process preview token covers unrelated changes and file
        // replacement as well as a new Magpie ownership revision. The internal
        // commit still compares the complete snapshot, not only this digest.
        let mut hash = Sha256::new();
        hash.update(b"magpie-codex-model-preview-v1\0");
        for session in [&journal.session, &journal.connected_session] {
            hash.update((session.len() as u64).to_le_bytes());
            hash.update(session.as_bytes());
        }
        if let Some(identity) = &snapshot.identity {
            hash.update([1]);
            for value in [
                identity.dev,
                identity.ino,
                u64::from(identity.mode),
                identity.size,
                identity.mtime as u64,
                identity.mtime_nsec as u64,
                identity.nlink,
            ] {
                hash.update(value.to_le_bytes());
            }
        } else {
            hash.update([0]);
        }
        if let Some(bytes) = &snapshot.bytes {
            hash.update([1]);
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        } else {
            hash.update([0]);
        }
        hex(&hash.finalize())
    }

    fn restore_owned(
        original: Option<&[u8]>,
        connected: &[u8],
        current: Option<&[u8]>,
    ) -> Result<(Option<Vec<u8>>, DisconnectOutcome), Error> {
        let Some(current) = current else {
            return Ok((
                None,
                DisconnectOutcome {
                    restored_fields: 0,
                    preserved_fields: OWNED_FIELDS.len(),
                    config_removed: false,
                },
            ));
        };
        if current == connected {
            return Ok((
                original.map(<[u8]>::to_vec),
                DisconnectOutcome {
                    restored_fields: OWNED_FIELDS.len(),
                    preserved_fields: 0,
                    config_removed: original.is_none(),
                },
            ));
        }
        let baseline = parse_optional(original)?;
        let written = parse(connected)?;
        let mut document = parse(current)?;
        let mut comments = String::new();
        let mut restored = 0;
        for (index, key) in ["model", "model_provider"].iter().enumerate() {
            if owned_equal(&document, &written, index) {
                if let Some(previous) = baseline.get(key).and_then(Item::as_value) {
                    replace_value(document.as_table_mut(), key, previous.clone());
                } else {
                    remove_preserving_comments(document.as_table_mut(), key, &mut comments);
                }
                restored += 1;
            }
        }
        let matches: Vec<bool> = (2..OWNED_FIELDS.len())
            .map(|i| owned_equal(&document, &written, i))
            .collect();
        if let Some(providers) = document
            .get_mut("model_providers")
            .and_then(Item::as_table_mut)
        {
            if let Some(provider) = providers.get_mut("magpie").and_then(Item::as_table_mut) {
                for (index, key) in PROVIDER_KEYS.iter().enumerate() {
                    if matches[index] {
                        remove_preserving_comments(provider, key, &mut comments);
                        restored += 1;
                    }
                }
                if provider.is_empty() {
                    remove_preserving_comments(providers, "magpie", &mut comments);
                }
            }
            if providers.is_empty() && !baseline.contains_key("model_providers") {
                remove_preserving_comments(
                    document.as_table_mut(),
                    "model_providers",
                    &mut comments,
                );
            }
        }
        if !comments.is_empty() {
            let mut trailing = document.trailing().as_str().unwrap_or_default().to_owned();
            if !trailing.ends_with('\n') {
                trailing.push('\n');
            }
            trailing.push_str(&comments);
            document.set_trailing(trailing);
        }
        let bytes = document.to_string().into_bytes();
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::TooLarge);
        }
        Ok((
            Some(bytes),
            DisconnectOutcome {
                restored_fields: restored,
                preserved_fields: OWNED_FIELDS.len() - restored,
                config_removed: false,
            },
        ))
    }

    fn retain_comment(raw: Option<&toml_edit::RawString>, comments: &mut String) {
        if let Some(text) = raw.and_then(toml_edit::RawString::as_str) {
            if text.contains('#') {
                comments.push_str(text);
                if !text.ends_with('\n') {
                    comments.push('\n');
                }
            }
        }
    }

    fn remove_preserving_comments(table: &mut Table, key: &str, comments: &mut String) {
        if let Some((key, item)) = table.get_key_value(key) {
            retain_comment(key.leaf_decor().prefix(), comments);
            retain_comment(key.leaf_decor().suffix(), comments);
            if let Some(value) = item.as_value() {
                retain_comment(value.decor().prefix(), comments);
                retain_comment(value.decor().suffix(), comments);
            } else if let Some(child) = item.as_table() {
                retain_comment(child.decor().prefix(), comments);
                retain_comment(child.decor().suffix(), comments);
            }
        }
        table.remove(key);
    }

    fn parse(bytes: &[u8]) -> Result<DocumentMut, Error> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)?;
        text.parse::<DocumentMut>().map_err(|_| Error::InvalidToml)
    }

    fn parse_optional(bytes: Option<&[u8]>) -> Result<DocumentMut, Error> {
        bytes.map(parse).unwrap_or_else(|| Ok(DocumentMut::new()))
    }

    fn target(path: &Path, writable: bool) -> Result<Target, Error> {
        let _ = read_flags()?;
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
        let state = parent.join(format!(".{name}.magpie-state"));
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

    fn checked_meta(
        meta: &Metadata,
        private: bool,
        writable: bool,
        limit: u64,
    ) -> Result<(), Error> {
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

    fn read_file(
        path: &Path,
        private: bool,
        writable: bool,
        limit: u64,
    ) -> Result<Snapshot, Error> {
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
                    super::test_support::checkpoint(
                        super::test_support::Point::LockCreated,
                        &target.state,
                    )?;
                    lock_file(target, file)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    existing_lock(target)?.ok_or(Error::OriginalChanged)?
                }
                Err(_) => return Err(Error::Write),
            }
        };
        #[cfg(test)]
        super::test_support::checkpoint(super::test_support::Point::LockAcquired, &target.state)?;
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
            super::test_support::checkpoint(
                super::test_support::Point::LegacyLockLinked,
                &target.state,
            )?;
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

    fn require_inactive(journal: Option<&Journal>) -> Result<(), Error> {
        match journal.map(|j| j.phase) {
            None | Some(Phase::Disconnected | Phase::Aborted) => Ok(()),
            Some(Phase::Connected) => Err(Error::AlreadyManaged),
            Some(Phase::Prepared | Phase::UpdatePrepared | Phase::DisconnectPrepared) => {
                Err(Error::RecoveryRequired)
            }
        }
    }

    fn write_new_private(target: &Target, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::TooLarge);
        }
        ensure_budget(target, bytes.len(), 1)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| Error::Write)?;
        file.write_all(bytes).map_err(|_| Error::Write)?;
        file.sync_all().map_err(|_| Error::Write)?;
        #[cfg(test)]
        super::test_support::checkpoint(super::test_support::Point::ImageSynced, &target.state)?;
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
            super::test_support::checkpoint(
                super::test_support::Point::StageSynced,
                &target.state,
            )?;
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
        super::test_support::checkpoint(super::test_support::Point::BeforeInstall, target)?;
        fs::rename(&stage.path, target).map_err(|_| Error::Write)?;
        stage.active = false;
        Ok(())
    }

    fn sync_dir(path: &Path) -> Result<(), Error> {
        #[cfg(test)]
        super::test_support::checkpoint(super::test_support::Point::BeforeDirectorySync, path)?;
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

    fn persist_journal(target: &Target, journal: &Journal) -> Result<(), Error> {
        let mut document = DocumentMut::new();
        // Old readers must refuse an updated ownership image rather than restore
        // using the first connected image. Unchanged v1 sessions remain usable.
        let updated =
            journal.connected_session != journal.session || journal.phase == Phase::UpdatePrepared;
        document["version"] = value(if updated { 2 } else { 1 });
        document["target"] = value(&target.binding);
        document["session"] = value(&journal.session);
        if updated {
            document["connected_session"] = value(&journal.connected_session);
            document["update_session"] = value(&journal.update_session);
        }
        document["disconnect_session"] = value(&journal.disconnect_session);
        document["phase"] = value(match journal.phase {
            Phase::Prepared => "prepared",
            Phase::Connected => "connected",
            Phase::UpdatePrepared => "update-prepared",
            Phase::DisconnectPrepared => "disconnect-prepared",
            Phase::Disconnected => "disconnected",
            Phase::Aborted => "aborted",
        });
        document["original_exists"] = value(journal.original_exists);
        document["disconnect_before_exists"] = value(journal.disconnect_before_exists);
        document["disconnected_exists"] = value(journal.disconnected_exists);
        document["restored_fields"] = value(journal.restored as i64);
        document["preserved_fields"] = value(journal.preserved as i64);
        document["config_removed"] = value(journal.removed);
        let bytes = document.to_string().into_bytes();
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(Error::StateInvalid);
        }
        let mut staged = stage(target, &bytes)?;
        let journal_path = target.state.join("journal.toml");
        // Never overwrite a nonregular, hardlinked or externally accessible
        // journal, even if it appeared after the previous read.
        let _ = read_file(&journal_path, true, true, MAX_JOURNAL_BYTES)?;
        install_stage(&mut staged, &journal_path)?;
        sync_dir(&target.state)
    }

    fn read_journal(target: &Target) -> Result<Option<Journal>, Error> {
        if !state_exists(target)? {
            return Ok(None);
        }
        let snapshot = read_file(
            &target.state.join("journal.toml"),
            true,
            false,
            MAX_JOURNAL_BYTES,
        )?;
        let Some(bytes) = snapshot.bytes else {
            return Ok(None);
        };
        let document = parse(&bytes).map_err(|_| Error::StateInvalid)?;
        let version = document.get("version").and_then(Item::as_integer);
        if !matches!(version, Some(1 | 2))
            || document.get("target").and_then(Item::as_str) != Some(target.binding.as_str())
        {
            return Err(Error::StateInvalid);
        }
        let session = document
            .get("session")
            .and_then(Item::as_str)
            .ok_or(Error::StateInvalid)?;
        if !valid_session(session, false) {
            return Err(Error::StateInvalid);
        }
        let phase = match document.get("phase").and_then(Item::as_str) {
            Some("prepared") => Phase::Prepared,
            Some("connected") => Phase::Connected,
            Some("update-prepared") if version == Some(2) => Phase::UpdatePrepared,
            Some("disconnect-prepared") => Phase::DisconnectPrepared,
            Some("disconnected") => Phase::Disconnected,
            Some("aborted") => Phase::Aborted,
            _ => return Err(Error::StateInvalid),
        };
        let disconnect_session = document
            .get("disconnect_session")
            .and_then(Item::as_str)
            .ok_or(Error::StateInvalid)?;
        if !valid_session(disconnect_session, true)
            || (matches!(phase, Phase::DisconnectPrepared | Phase::Disconnected)
                && disconnect_session.is_empty())
        {
            return Err(Error::StateInvalid);
        }
        let (connected_session, update_session) = if version == Some(2) {
            let connected = document
                .get("connected_session")
                .and_then(Item::as_str)
                .ok_or(Error::StateInvalid)?;
            let update = document
                .get("update_session")
                .and_then(Item::as_str)
                .ok_or(Error::StateInvalid)?;
            if !valid_session(connected, false)
                || !valid_session(update, true)
                || ((phase == Phase::UpdatePrepared) == update.is_empty())
                || (!update.is_empty() && (update == session || update == connected))
                || matches!(phase, Phase::Prepared | Phase::Aborted)
            {
                return Err(Error::StateInvalid);
            }
            (connected, update)
        } else {
            (session, "")
        };
        let boolean = |key| {
            document
                .get(key)
                .and_then(Item::as_bool)
                .ok_or(Error::StateInvalid)
        };
        let count = |key| {
            let count = document
                .get(key)
                .and_then(Item::as_integer)
                .ok_or(Error::StateInvalid)?;
            if !(0..=OWNED_FIELDS.len() as i64).contains(&count) {
                return Err(Error::StateInvalid);
            }
            Ok(count as usize)
        };
        let journal = Journal {
            session: session.to_owned(),
            connected_session: connected_session.to_owned(),
            update_session: update_session.to_owned(),
            disconnect_session: disconnect_session.to_owned(),
            phase,
            original_exists: boolean("original_exists")?,
            disconnect_before_exists: boolean("disconnect_before_exists")?,
            disconnected_exists: boolean("disconnected_exists")?,
            restored: count("restored_fields")?,
            preserved: count("preserved_fields")?,
            removed: boolean("config_removed")?,
        };
        if journal.restored + journal.preserved > OWNED_FIELDS.len()
            || (matches!(phase, Phase::DisconnectPrepared | Phase::Disconnected)
                && journal.restored + journal.preserved != OWNED_FIELDS.len())
            || (journal.removed
                && (!journal.disconnect_before_exists || journal.disconnected_exists))
        {
            return Err(Error::StateInvalid);
        }
        Ok(Some(journal))
    }

    fn valid_session(session: &str, empty_allowed: bool) -> bool {
        (empty_allowed || !session.is_empty())
            && session.len() <= 80
            && session.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    }

    fn stored_image(
        target: &Target,
        journal: &Journal,
        suffix: &str,
        exists: bool,
    ) -> Result<Option<Vec<u8>>, Error> {
        let snapshot = read_file(
            &journal.image_path(target, suffix),
            true,
            false,
            MAX_CONFIG_BYTES,
        )?;
        let bytes = snapshot.bytes.ok_or(Error::StateInvalid)?;
        if exists {
            Ok(Some(bytes))
        } else if bytes.is_empty() {
            Ok(None)
        } else {
            Err(Error::StateInvalid)
        }
    }

    fn connection_images(
        target: &Target,
        journal: &Journal,
    ) -> Result<(Option<Vec<u8>>, Vec<u8>), Error> {
        let original = stored_image(target, journal, "original", journal.original_exists)?;
        let connected =
            stored_image(target, journal, "connected", true)?.ok_or(Error::StateInvalid)?;
        let document = parse(&connected).map_err(|_| Error::StateInvalid)?;
        let model = document
            .get("model")
            .and_then(Item::as_str)
            .ok_or(Error::StateInvalid)?;
        let gateway = connected_gateway(&document)?;
        validate_model(model).map_err(|_| Error::StateInvalid)?;
        let expected = build_connected(original.as_deref(), model, gateway)
            .map_err(|_| Error::StateInvalid)?;
        if expected != connected {
            return Err(Error::StateInvalid);
        }
        Ok((original, connected))
    }

    fn connected_gateway(document: &DocumentMut) -> Result<SocketAddr, Error> {
        let base_url = owned_item(document, 3)
            .and_then(Item::as_str)
            .ok_or(Error::StateInvalid)?;
        let address = base_url
            .strip_prefix("http://")
            .and_then(|url| url.strip_suffix("/v1"))
            .ok_or(Error::StateInvalid)?;
        let gateway = address
            .parse::<SocketAddr>()
            .map_err(|_| Error::StateInvalid)?;
        validate_gateway(gateway).map_err(|_| Error::StateInvalid)?;
        Ok(gateway)
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
            let lock_entry =
                entry.file_name() == LOCK_NAME || entry.file_name() == LEGACY_LOCK_NAME;
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

    fn discard_finished_images(target: &Target, previous: &Journal) {
        if !matches!(previous.phase, Phase::Disconnected | Phase::Aborted) {
            return;
        }
        for suffix in ["original", "connected", "disconnect-before", "disconnected"] {
            if previous.disconnect_session.is_empty()
                && matches!(suffix, "disconnect-before" | "disconnected")
            {
                continue;
            }
            discard_image(&previous.image_path(target, suffix));
        }
        // A completed update keeps the original image and initial canonical
        // image. They can be retired only when a later connect owns recovery.
        if previous.connected_session != previous.session {
            discard_image(
                &target
                    .state
                    .join(format!("{}-connected.toml", previous.session)),
            );
            for suffix in ["update-before", "update-after"] {
                discard_image(
                    &target
                        .state
                        .join(format!("{}-{suffix}.toml", previous.connected_session)),
                );
            }
        }
        let _ = sync_dir(&target.state);
    }

    fn discard_update_images(target: &Target, pending: &Journal, installed: bool) {
        for suffix in ["update-before", "update-after"] {
            discard_image(&pending.image_path(target, suffix));
        }
        let unused = if installed {
            &pending.connected_session
        } else {
            &pending.update_session
        };
        if unused != &pending.session {
            discard_image(&target.state.join(format!("{unused}-connected.toml")));
        }
        let _ = sync_dir(&target.state);
    }

    fn discard_image(path: &Path) {
        if let Ok(meta) = fs::symlink_metadata(path) {
            if checked_meta(&meta, true, true, MAX_CONFIG_BYTES).is_ok() {
                let _ = fs::remove_file(path);
            }
        }
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

    #[cfg(test)]
    mod lock_tests {
        use super::super::test_support;
        use super::*;
        use std::io::{BufRead, BufReader};
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Child, Command, Stdio};
        use std::sync::mpsc;
        use std::time::Duration;

        const ORIGINAL: &[u8] = b"# isolated lock fixture\nmodel = 'user/original' # retained\nmodel_provider = 'user'\nunknown_extension = ['a', 'b']\n\n[model_providers.user]\nname = 'synthetic'\nenv_key = 'SYNTHETIC_REFERENCE'\n";
        const MODEL: &str = "fixture/transaction-lock";
        const WORKER_ROOT: &str = "MAGPIE_V2_LOCK_TEST_ROOT";
        const WORKER_OPERATION: &str = "MAGPIE_V2_LOCK_TEST_OPERATION";
        const WORKER_POINT: &str = "MAGPIE_V2_LOCK_TEST_POINT";
        const WORKER_OCCURRENCE: &str = "MAGPIE_V2_LOCK_TEST_OCCURRENCE";

        struct Fixture {
            root: PathBuf,
            config: PathBuf,
        }

        impl Fixture {
            fn new(original: Option<&[u8]>) -> Self {
                let root = std::env::temp_dir()
                    .canonicalize()
                    .expect("canonical temporary directory")
                    .join(format!("magpie-lock-v2-{}", nonce()));
                DirBuilder::new()
                    .mode(0o700)
                    .create(&root)
                    .expect("create private synthetic fixture");
                let config = root.join("config.toml");
                if let Some(bytes) = original {
                    write_private(&config, bytes);
                }
                Self { root, config }
            }

            fn target(&self) -> Target {
                target(&self.config, true).expect("valid fixture target")
            }

            fn state(&self) -> PathBuf {
                self.root.join(".config.toml.magpie-state")
            }

            fn connect(&self) {
                let plan = prepare(&self.config, MODEL, endpoint()).expect("prepare fixture");
                assert_eq!(commit(plan), Ok(ConnectionStatus::Connected));
            }

            fn bytes(&self) -> Option<Vec<u8>> {
                match fs::read(&self.config) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => panic!("read fixture: {error}"),
                }
            }

            fn usage(&self) -> (u64, usize) {
                fs::read_dir(self.state())
                    .expect("fixture state entries")
                    .fold((0, 0), |(bytes, count), entry| {
                        let meta = entry.expect("fixture entry").metadata().expect("metadata");
                        (bytes + meta.len(), count + 1)
                    })
            }

            fn assert_private(&self) {
                assert_eq!(
                    fs::metadata(self.state()).expect("state metadata").mode() & 0o777,
                    0o700
                );
                for entry in fs::read_dir(self.state()).expect("state entries") {
                    assert_eq!(
                        entry.expect("entry").metadata().expect("metadata").mode() & 0o777,
                        0o600
                    );
                }
                if self.config.exists() {
                    assert_eq!(
                        fs::metadata(&self.config).expect("config metadata").mode() & 0o777,
                        0o600
                    );
                }
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        fn write_private(path: &Path, bytes: &[u8]) {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .expect("create private fixture file");
            file.write_all(bytes).expect("write fixture file");
        }

        fn endpoint() -> SocketAddr {
            "127.0.0.1:3425"
                .parse()
                .expect("synthetic loopback endpoint")
        }

        struct Worker(Child);

        impl Worker {
            fn paused_at(
                fixture: &Fixture,
                operation: &str,
                point: &str,
                occurrence: usize,
            ) -> Self {
                // Strip the crate name: this module is also compiled by the
                // existing integration test's #[path] inclusion.
                let module = module_path!().split_once("::").expect("test module").1;
                let child = {
                    let _spawn_scope = test_support::spawn_scope();
                    Command::new(std::env::current_exe().expect("test executable"))
                        .args([
                            "--exact",
                            &format!("{module}::subprocess_worker"),
                            "--ignored",
                            "--nocapture",
                        ])
                        .env(WORKER_ROOT, &fixture.root)
                        .env(WORKER_OPERATION, operation)
                        .env(WORKER_POINT, point)
                        .env(WORKER_OCCURRENCE, occurrence.to_string())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("spawn isolated transaction worker")
                };
                let mut worker = Self(child);
                let stdout = worker.0.stdout.take().expect("worker stdout");
                let (ready_tx, ready_rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let ready = BufReader::new(stdout)
                        .lines()
                        .map_while(Result::ok)
                        .any(|line| line.contains("magpie-transaction-checkpoint"));
                    let _ = ready_tx.send(ready);
                });
                assert_eq!(
                    ready_rx.recv_timeout(Duration::from_secs(15)),
                    Ok(true),
                    "worker must reach {operation}/{point}/{occurrence}"
                );
                worker
            }

            fn kill(mut self) {
                self.0.kill().expect("SIGKILL paused transaction");
                let status = self.0.wait().expect("reap killed transaction");
                assert_eq!(status.signal(), Some(9), "worker was killed without Drop");
            }
        }

        impl Drop for Worker {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        #[test]
        #[ignore = "spawned with a private synthetic fixture by lock tests"]
        fn subprocess_worker() {
            let root = PathBuf::from(std::env::var_os(WORKER_ROOT).expect("fixture root"));
            assert_eq!(
                root.parent(),
                Some(
                    std::env::temp_dir()
                        .canonicalize()
                        .expect("temporary directory")
                        .as_path()
                )
            );
            assert!(
                root.file_name()
                    .expect("fixture name")
                    .to_str()
                    .expect("ASCII fixture name")
                    .starts_with("magpie-lock-v2-")
            );
            let config = root.join("config.toml");
            let state = root.join(".config.toml.magpie-state");
            use test_support::{Action, Point};
            let (point, path) = match std::env::var(WORKER_POINT).expect("point").as_str() {
                "created" => (Point::LockCreated, &state),
                "acquired" => (Point::LockAcquired, &state),
                "linked" => (Point::LegacyLockLinked, &state),
                "stage" => (Point::StageSynced, &state),
                "image" => (Point::ImageSynced, &state),
                "install" => (Point::BeforeInstall, &config),
                "parent-sync" => (Point::BeforeDirectorySync, &root),
                point => panic!("unknown fixture point {point}"),
            };
            let occurrence = std::env::var(WORKER_OCCURRENCE)
                .expect("occurrence")
                .parse()
                .expect("numeric occurrence");
            test_support::arm(point, path, occurrence, Action::Pause);
            match std::env::var(WORKER_OPERATION).expect("operation").as_str() {
                "connect" => {
                    let plan = prepare(&config, MODEL, endpoint()).expect("prepare worker");
                    commit(plan).expect("commit worker");
                }
                "disconnect" => {
                    disconnect(&config).expect("disconnect worker");
                }
                operation => panic!("unknown fixture operation {operation}"),
            }
            panic!("worker did not reach its checkpoint");
        }

        #[test]
        fn sigkill_connect_releases_lock_and_exposes_recoverable_journal() {
            for original in [Some(ORIGINAL), None] {
                for (point, occurrence, expected) in [
                    ("created", 1, ConnectionStatus::Unmanaged),
                    ("acquired", 1, ConnectionStatus::Unmanaged),
                    ("linked", 1, ConnectionStatus::Unmanaged),
                    ("stage", 1, ConnectionStatus::Unmanaged),
                    ("image", 1, ConnectionStatus::Unmanaged),
                    ("install", 1, ConnectionStatus::RecoveryRequired),
                    ("parent-sync", 2, ConnectionStatus::RecoveryRequired),
                ] {
                    let fixture = Fixture::new(original);
                    Worker::paused_at(&fixture, "connect", point, occurrence).kill();
                    assert_eq!(status(&fixture.config), Ok(expected), "connect/{point}");
                    if expected == ConnectionStatus::RecoveryRequired {
                        disconnect(&fixture.config).expect("recover interrupted connect");
                    }
                    assert_eq!(fixture.bytes().as_deref(), original, "connect/{point}");
                    fixture.connect();
                    disconnect(&fixture.config).expect("disconnect after killed connect");
                    assert_eq!(fixture.bytes().as_deref(), original);
                    fixture.assert_private();
                }
            }
        }

        #[test]
        fn sigkill_disconnect_can_resume_without_removing_either_lock_name() {
            for original in [Some(ORIGINAL), None] {
                for (point, occurrence, expected) in [
                    ("acquired", 1, ConnectionStatus::Connected),
                    ("image", 1, ConnectionStatus::Connected),
                    ("stage", 1, ConnectionStatus::Connected),
                    ("stage", 2, ConnectionStatus::RecoveryRequired),
                    ("install", 1, ConnectionStatus::RecoveryRequired),
                    ("parent-sync", 1, ConnectionStatus::RecoveryRequired),
                ] {
                    let fixture = Fixture::new(original);
                    fixture.connect();
                    let before = fs::metadata(fixture.state().join(LOCK_NAME)).expect("lock");
                    Worker::paused_at(&fixture, "disconnect", point, occurrence).kill();
                    assert_eq!(status(&fixture.config), Ok(expected), "disconnect/{point}");
                    disconnect(&fixture.config).expect("resume killed disconnect");
                    assert_eq!(fixture.bytes().as_deref(), original, "disconnect/{point}");
                    assert_eq!(status(&fixture.config), Ok(ConnectionStatus::Unmanaged));
                    let after = fs::metadata(fixture.state().join(LOCK_NAME)).expect("lock");
                    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
                    assert_eq!(after.nlink(), 2);
                    fixture.assert_private();
                }
            }
        }

        #[test]
        fn live_child_excludes_readers_writers_and_legacy_create_new_clients() {
            let fixture = Fixture::new(Some(ORIGINAL));
            let competing_plan =
                prepare(&fixture.config, MODEL, endpoint()).expect("prepare competing writer");
            let worker = Worker::paused_at(&fixture, "connect", "stage", 1);
            assert_eq!(status(&fixture.config), Err(Error::LockBusy));
            assert!(matches!(
                prepare(&fixture.config, MODEL, endpoint()),
                Err(Error::LockBusy)
            ));
            assert_eq!(commit(competing_plan), Err(Error::LockBusy));
            assert_eq!(disconnect(&fixture.config), Err(Error::LockBusy));
            assert_eq!(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(fixture.state().join(LEGACY_LOCK_NAME))
                    .expect_err("legacy writer must be excluded")
                    .kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
            worker.kill();
            assert_eq!(status(&fixture.config), Ok(ConnectionStatus::Unmanaged));
            fixture.connect();
        }

        #[test]
        fn delayed_open_descriptor_and_successive_owners_use_the_same_inode() {
            let fixture = Fixture::new(Some(ORIGINAL));
            let target = fixture.target();
            let _descriptor_scope = test_support::descriptor_scope();
            let first = acquire_lock(&target, true).expect("first owner");
            let delayed = OpenOptions::new()
                .read(true)
                .write(true)
                .open(target.state.join(LEGACY_LOCK_NAME))
                .expect("open the same inode before the first owner exits");
            let identity = first.identity;
            drop(first);
            for _ in 0..8 {
                let next = acquire_lock(&target, false).expect("successive owner");
                assert_eq!(next.identity, identity);
                assert_eq!(
                    flock(&delayed, FlockOperation::NonBlockingLockExclusive),
                    Err(rustix::io::Errno::WOULDBLOCK)
                );
                drop(next);
                flock(&delayed, FlockOperation::NonBlockingLockExclusive)
                    .expect("delayed descriptor becomes owner");
                assert_eq!(status(&fixture.config), Err(Error::LockBusy));
                flock(&delayed, FlockOperation::Unlock).expect("release delayed owner");
            }
            assert!(check_lock_names(&target, identity).expect("persistent aliases"));
        }

        #[test]
        fn normal_release_unlocks_a_retained_shared_description() {
            let fixture = Fixture::new(Some(ORIGINAL));
            let target = fixture.target();
            let _descriptor_scope = test_support::descriptor_scope();
            let first = acquire_lock(&target, true).expect("acquire first guard");
            let identity = first.identity;
            let inherited = first
                ._file
                .try_clone()
                .expect("duplicate locked description");

            assert!(
                matches!(acquire_lock(&target, false), Err(Error::LockBusy)),
                "another open must remain excluded while the first guard lives"
            );
            drop(first);
            let second = acquire_lock(&target, false)
                .expect("normal release must unlock even with a duplicate still open");
            assert_eq!(second.identity, identity);
            drop(inherited);
            assert!(
                matches!(acquire_lock(&target, false), Err(Error::LockBusy)),
                "closing the old duplicate must not release the new owner"
            );
            assert!(check_lock_names(&target, identity).expect("persistent aliases"));
            drop(second);

            fixture.connect();
            disconnect(&fixture.config).expect("disconnect after ordinary lock handoff");
            assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
            for name in [LOCK_NAME, LEGACY_LOCK_NAME] {
                let meta = fs::metadata(target.state.join(name)).expect("persistent lock name");
                assert_eq!((meta.dev(), meta.ino()), identity);
                assert_eq!(meta.nlink(), 2);
            }
            fixture.assert_private();
        }

        #[test]
        fn ordinary_lock_initialization_errors_release_the_descriptor() {
            use test_support::{Action, Point};
            for point in [
                Point::LockCreated,
                Point::LockAcquired,
                Point::LegacyLockLinked,
            ] {
                let fixture = Fixture::new(Some(ORIGINAL));
                let target = fixture.target();
                test_support::arm(point, &target.state, 1, Action::Fail);
                {
                    let _descriptor_scope = test_support::descriptor_scope();
                    assert!(matches!(acquire_lock(&target, true), Err(Error::Write)));
                }
                assert_eq!(status(&fixture.config), Ok(ConnectionStatus::Unmanaged));
                fixture.connect();
                disconnect(&fixture.config).expect("disconnect after initialization failure");
                assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
            }
        }

        #[test]
        fn unknown_legacy_locks_remain_untouched_with_or_without_v2_initialization() {
            for initialized in [false, true] {
                for contents in [b"".as_slice(), b"unknown prior owner".as_slice()] {
                    let fixture = Fixture::new(Some(ORIGINAL));
                    if initialized {
                        Worker::paused_at(&fixture, "connect", "created", 1).kill();
                    } else {
                        DirBuilder::new()
                            .mode(0o700)
                            .create(fixture.state())
                            .expect("state");
                    }
                    let legacy = fixture.state().join(LEGACY_LOCK_NAME);
                    write_private(&legacy, contents);
                    let before = fs::symlink_metadata(&legacy).expect("legacy metadata");
                    assert_eq!(status(&fixture.config), Err(Error::LockBusy));
                    assert!(matches!(
                        prepare(&fixture.config, MODEL, endpoint()),
                        Err(Error::LockBusy)
                    ));
                    assert_eq!(disconnect(&fixture.config), Err(Error::LockBusy));
                    let after = fs::symlink_metadata(&legacy).expect("legacy metadata");
                    assert_eq!(fs::read(&legacy).expect("legacy bytes"), contents);
                    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
                    assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
                    assert_eq!(fixture.state().join(LOCK_NAME).exists(), initialized);
                }
            }
        }

        #[test]
        fn interrupted_gate_publication_does_not_adopt_a_racing_legacy_owner() {
            let fixture = Fixture::new(Some(ORIGINAL));
            let plan = prepare(&fixture.config, MODEL, endpoint()).expect("prepare");
            let worker = Worker::paused_at(&fixture, "connect", "acquired", 1);
            let legacy = fixture.state().join(LEGACY_LOCK_NAME);
            write_private(&legacy, b"legacy owner acquired before the v2 gate");
            worker.kill();
            assert_eq!(commit(plan), Err(Error::LockBusy));
            assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
            assert_eq!(
                fs::read(legacy).expect("legacy bytes"),
                b"legacy owner acquired before the v2 gate"
            );
        }

        #[test]
        fn unsafe_lock_files_and_unrecognized_hardlinks_are_rejected() {
            for fault in ["symlink", "extra-link", "public", "read-only", "nonempty"] {
                let fixture = Fixture::new(Some(ORIGINAL));
                fixture.connect();
                let lock = fixture.state().join(LOCK_NAME);
                match fault {
                    "symlink" => {
                        fs::remove_file(&lock).expect("test-only alias replacement");
                        symlink(fixture.state().join(LEGACY_LOCK_NAME), &lock).expect("symlink");
                    }
                    "extra-link" => {
                        fs::hard_link(&lock, fixture.root.join("third-link")).expect("third link");
                    }
                    "public" => {
                        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644))
                            .expect("public lock");
                    }
                    "read-only" => {
                        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400))
                            .expect("read-only lock");
                    }
                    "nonempty" => fs::write(&lock, b"unknown").expect("unknown lock bytes"),
                    _ => unreachable!("fixture fault"),
                }
                let before = fixture.bytes();
                assert!(status(&fixture.config).is_err(), "{fault}");
                assert!(disconnect(&fixture.config).is_err(), "{fault}");
                assert_eq!(fixture.bytes(), before, "{fault}");
            }
        }

        #[test]
        fn killed_disconnect_preserves_external_unknown_fields_and_drift() {
            let fixture = Fixture::new(Some(ORIGINAL));
            fixture.connect();
            Worker::paused_at(&fixture, "disconnect", "image", 1).kill();
            let mut current = parse(fixture.bytes().as_deref().expect("config")).expect("TOML");
            current["model"] = value("user/later");
            current["unknown_after_connect"] = value("retain");
            current["model_providers"]["magpie"]["http_headers"]["X-Fixture"] = value("retain");
            fs::write(&fixture.config, current.to_string()).expect("external edit");
            let outcome = disconnect(&fixture.config).expect("selective recovery");
            assert_eq!(outcome.restored_fields, 6);
            assert_eq!(outcome.preserved_fields, 1);
            let restored = parse(fixture.bytes().as_deref().expect("config")).expect("TOML");
            assert_eq!(restored["model"].as_str(), Some("user/later"));
            assert_eq!(restored["unknown_after_connect"].as_str(), Some("retain"));
            let extension: Vec<_> = restored["unknown_extension"]
                .as_array()
                .expect("unknown array retained")
                .iter()
                .map(Value::as_str)
                .collect();
            assert_eq!(extension, [Some("a"), Some("b")]);
            assert_eq!(
                restored["model_providers"]["magpie"]["http_headers"]["X-Fixture"].as_str(),
                Some("retain")
            );
            fixture.assert_private();
        }

        #[test]
        fn ambiguous_recovery_after_sigkill_keeps_target_and_pending_journal() {
            let fixture = Fixture::new(Some(ORIGINAL));
            Worker::paused_at(&fixture, "connect", "parent-sync", 2).kill();
            let external = [
                b"# external edit after crash\n".as_slice(),
                &fixture.bytes().expect("config"),
            ]
            .concat();
            fs::write(&fixture.config, &external).expect("external change");
            let journal = fs::read(fixture.state().join("journal.toml")).expect("journal");
            assert_eq!(
                status(&fixture.config),
                Ok(ConnectionStatus::RecoveryRequired)
            );
            assert_eq!(disconnect(&fixture.config), Err(Error::RecoveryRequired));
            assert_eq!(fixture.bytes(), Some(external));
            assert_eq!(
                fs::read(fixture.state().join("journal.toml")).expect("journal"),
                journal
            );
        }

        #[test]
        fn permanent_locks_and_killed_fragments_remain_inside_storage_budgets() {
            for byte_limit in [true, false] {
                let fixture = Fixture::new(Some(ORIGINAL));
                fixture.connect();
                Worker::paused_at(&fixture, "disconnect", "parent-sync", 1).kill();
                let padding = fixture.state().join("unknown-operator-data");
                if byte_limit {
                    write_private(&padding, b"");
                    let used = fixture.usage().0;
                    OpenOptions::new()
                        .write(true)
                        .open(&padding)
                        .expect("sparse padding")
                        .set_len(MAX_STATE_BYTES - used)
                        .expect("128 MiB logical state, without allocating its contents");
                } else {
                    for index in fixture.usage().1..MAX_STATE_ENTRIES {
                        write_private(&fixture.state().join(format!("unknown-{index}")), b"retain");
                    }
                }
                let before = fixture.usage();
                assert_eq!(
                    status(&fixture.config),
                    Ok(ConnectionStatus::RecoveryRequired)
                );
                assert_eq!(disconnect(&fixture.config), Err(Error::StorageLimit));
                assert_eq!(fixture.usage(), before, "refusal creates no fragments");
                assert_eq!(fixture.bytes().as_deref(), Some(ORIGINAL));
                let target = fixture.target();
                let journal = read_journal(&target)
                    .expect("journal")
                    .expect("pending journal");
                assert!(journal.phase == Phase::DisconnectPrepared);
                if byte_limit {
                    fs::remove_file(&padding).expect("remove test padding");
                } else {
                    // Only delete fixture files this test created.
                    for entry in fs::read_dir(fixture.state()).expect("entries") {
                        let entry = entry.expect("entry");
                        if entry.file_name().to_string_lossy().starts_with("unknown-") {
                            assert_eq!(fs::read(entry.path()).expect("unknown bytes"), b"retain");
                            fs::remove_file(entry.path()).expect("remove fixture padding");
                        }
                    }
                }
                disconnect(&fixture.config).expect("recover with room for journal");
                fixture.assert_private();
            }
        }

        #[test]
        fn initializing_the_permanent_pair_reserves_both_directory_entries() {
            let fixture = Fixture::new(Some(ORIGINAL));
            DirBuilder::new()
                .mode(0o700)
                .create(fixture.state())
                .expect("state");
            for index in 0..MAX_STATE_ENTRIES - 1 {
                write_private(&fixture.state().join(format!("unknown-{index}")), b"retain");
            }
            let before = fixture.usage();
            let target = fixture.target();
            {
                let _descriptor_scope = test_support::descriptor_scope();
                assert!(matches!(
                    acquire_lock(&target, true),
                    Err(Error::StorageLimit)
                ));
            }
            assert_eq!(fixture.usage(), before);
            assert!(!fixture.state().join(LOCK_NAME).exists());
            assert!(!fixture.state().join(LEGACY_LOCK_NAME).exists());
        }
    }
}
