//! Shared, bounded Provider metadata with crash-recoverable compare-and-swap writes.
//!
//! Version 1 stores environment credential references only. It never persists a
//! plaintext secret or silently falls back from an unavailable secure store.
//! Native keychain/account support remains a separate, unimplemented layer.

use std::collections::BTreeSet;
use std::fmt;
#[cfg(any(test, target_os = "linux", target_os = "android", target_os = "macos"))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use std::io::Read;
#[cfg(any(test, target_os = "linux", target_os = "android", target_os = "macos"))]
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

pub const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PROVIDERS: usize = 128;
const MAX_KEYS: usize = 32;
const MAX_MODELS: usize = 512;
const MAX_ARTIFACT_BYTES: u64 = 4096;

// No production environment variable enables fault injection. The checkpoint
// implementation exists only in the test binary and uses a child-only handshake.
macro_rules! provider_checkpoint {
    ($stage:expr) => {{
        #[cfg(all(
            test,
            any(target_os = "linux", target_os = "android", target_os = "macos")
        ))]
        tests::os_lock_tests::checkpoint($stage);
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Chat,
    Responses,
    Messages,
    Gemini,
}
impl Protocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Messages => "messages",
            Self::Gemini => "gemini",
        }
    }
    pub fn parse(s: &str) -> Result<Self, ProviderError> {
        match s {
            "chat" => Ok(Self::Chat),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            "gemini" => Ok(Self::Gemini),
            _ => Err(error("provider_invalid_protocol")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderKey {
    pub id: String,
    pub name: String,
    pub env: String,
    pub enabled: bool,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: Protocol,
    pub enabled: bool,
    pub deleted: bool,
    pub allow_insecure_http: bool,
    pub keys: Vec<ProviderKey>,
    pub models: Vec<String>,
    pub selected_models: Vec<String>,
    pub auto_include: bool,
    pub catalog_checked_at: Option<u64>,
    pub catalog_error: Option<String>,
}
impl Provider {
    #[must_use]
    pub fn enabled_models(&self) -> Vec<&str> {
        if !self.enabled || self.deleted {
            return Vec::new();
        }
        if self.auto_include {
            self.models.iter().map(String::as_str).collect()
        } else {
            self.selected_models.iter().map(String::as_str).collect()
        }
    }
    pub fn active_keys(&self) -> impl Iterator<Item = &ProviderKey> {
        self.keys
            .iter()
            .filter(|key| self.enabled && !self.deleted && key.enabled && !key.deleted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub revision: u64,
    pub token: String,
    pub providers: Vec<Provider>,
}
impl ProviderSnapshot {
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers
            .iter()
            .find(|provider| provider.id == id && !provider.deleted)
    }
    /// Public view only: does not resolve or expose environment variable values.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = format!(
            "{{\"schema_version\":{SCHEMA_VERSION},\"revision\":{},\"token\":{},\"providers\":[",
            self.revision,
            quote(&self.token)
        );
        for (index, provider) in self.providers.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"id\":{},\"name\":{},\"base_url\":{},\"protocol\":{},\"enabled\":{},\"deleted\":{},\"allow_insecure_http\":{},\"auto_include\":{},\"models\":{},\"selected_models\":{},\"catalog_checked_at\":{},\"catalog_error\":{},\"keys\":[",
                quote(&provider.id), quote(&provider.name), quote(&provider.base_url), quote(provider.protocol.as_str()), provider.enabled, provider.deleted, provider.allow_insecure_http, provider.auto_include, string_array_json(&provider.models), string_array_json(&provider.selected_models), provider.catalog_checked_at.map_or_else(|| "null".to_owned(), |t| t.to_string()), provider.catalog_error.as_ref().map_or_else(|| "null".to_owned(), |s| quote(s))));
            for (key_index, key) in provider.keys.iter().enumerate() {
                if key_index != 0 {
                    out.push(',');
                }
                out.push_str(&format!("{{\"id\":{},\"name\":{},\"env\":{},\"enabled\":{},\"deleted\":{},\"masked\":\"••••\"}}",quote(&key.id),quote(&key.name),quote(&key.env),key.enabled,key.deleted));
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

#[derive(Clone, Debug)]
pub enum ProviderMutation {
    Add(Provider),
    Edit {
        id: String,
        name: Option<String>,
        base_url: Option<String>,
        protocol: Option<Protocol>,
        enabled: Option<bool>,
        allow_insecure_http: Option<bool>,
    },
    Remove {
        id: String,
    },
    PutKey {
        provider_id: String,
        key: ProviderKey,
    },
    RemoveKey {
        provider_id: String,
        key_id: String,
    },
    ReorderKeys {
        provider_id: String,
        key_ids: Vec<String>,
    },
    Models {
        provider_id: String,
        models: Vec<String>,
        selected_models: Vec<String>,
        auto_include: bool,
    },
    CatalogSuccess {
        provider_id: String,
        models: Vec<String>,
        checked_at: u64,
    },
    CatalogFailure {
        provider_id: String,
        code: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
    code: &'static str,
    io_kind: Option<io::ErrorKind>,
}
impl ProviderError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}
impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for ProviderError {}
fn error(code: &'static str) -> ProviderError {
    ProviderError {
        code,
        io_kind: None,
    }
}
fn io_error(code: &'static str, cause: io::Error) -> ProviderError {
    ProviderError {
        code,
        io_kind: Some(cause.kind()),
    }
}

/// Explicitly exposed only at the outbound authentication boundary.
pub struct Secret(String);
impl Secret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
pub fn resolve_credential(key: &ProviderKey) -> Result<Secret, ProviderError> {
    if key.deleted || !key.enabled {
        return Err(error("provider_key_paused"));
    }
    if !valid_env(&key.env) {
        return Err(error("provider_invalid_credential_reference"));
    }
    let value = std::env::var(&key.env).map_err(|_| error("provider_credential_unavailable"))?;
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(error("provider_credential_invalid"));
    }
    Ok(Secret(value))
}

#[derive(Clone, Debug)]
pub struct ProviderStore {
    path: PathBuf,
}
impl ProviderStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn default_path() -> Result<PathBuf, ProviderError> {
        crate::paths::config_dir()
            .map(|p| p.join("providers.toml"))
            .map_err(|_| error("provider_config_directory_unavailable"))
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// A local, path-bound identifier for comparing configured data sources.
    ///
    /// This performs no writes and does not require the store or its parent to
    /// exist. It uses lexical normalization, not main-file inode identity, so
    /// atomic saves do not change it. It is not authentication, proof that a
    /// file exists, or equivalence of case-insensitive/symlink aliases.
    pub fn identity(&self) -> Result<String, ProviderError> {
        self.check_path()?;
        let mut normalized = PathBuf::new();
        for component in self.path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                component => normalized.push(component.as_os_str()),
            }
        }
        let mut domain = b"magpie-provider-store-path-v1\n".to_vec();
        domain.extend_from_slice(normalized.as_os_str().as_encoded_bytes());
        Ok(format!("provider-store-v1:{}", sha256(&domain)))
    }
    pub fn load(&self) -> Result<ProviderSnapshot, ProviderError> {
        self.check_path()?;
        // Existing-only shared locks keep reads out of a cooperating write or
        // recovery. An absent store stays a side-effect-free read. If first-use
        // lock publication races that read, retry once through the locked path.
        for _ in 0..2 {
            let lock = self.acquire_read_lock()?;
            if path_exists(&self.sidecar("recovery-guard"))? {
                return Err(error("provider_recovery_guard_present"));
            }
            if self.has_pending()? {
                return Err(error("provider_recovery_required"));
            }
            let bytes = read_optional(&self.path, MAX_BYTES, true)?;
            if lock.is_none()
                && (path_exists(&self.sidecar("lock"))? || path_exists(&self.sidecar("lock.v2"))?)
            {
                continue;
            }
            return snapshot_from_bytes(bytes.as_deref());
        }
        Err(error("provider_busy"))
    }
    pub fn apply(
        &self,
        expected_token: &str,
        mutation: ProviderMutation,
    ) -> Result<ProviderSnapshot, ProviderError> {
        self.prepare_parent()?;
        let _lock = self.acquire_lock(false)?;
        provider_checkpoint!("lock.acquired");
        if self.has_pending()? {
            return Err(error("provider_recovery_required"));
        }
        let original = read_optional(&self.path, MAX_BYTES, true)?;
        let mut snapshot = snapshot_from_bytes(original.as_deref())?;
        if snapshot.token != expected_token {
            return Err(error("provider_revision_conflict"));
        }
        apply_mutation(&mut snapshot.providers, mutation)?;
        validate_providers(&snapshot.providers)?;
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or_else(|| error("provider_revision_exhausted"))?;
        let new_bytes = encode(&snapshot)?.into_bytes();
        if new_bytes.len() as u64 > MAX_BYTES {
            return Err(error("provider_store_too_large"));
        }
        let after = snapshot_from_bytes(Some(&new_bytes))?;
        let transaction = Transaction {
            before: expected_token.to_owned(),
            after: after.token.clone(),
            existed: original.is_some(),
        };
        let journal = transaction.encode();
        publish_complete(
            &self.sidecar("transaction"),
            journal.as_bytes(),
            "transaction",
        )?;
        // From this point onward any failure leaves a verifiable journal. Explicit
        // recover rolls back an uncommitted write or completes committed cleanup.
        (|| {
            publish_complete(&self.sidecar("next"), &new_bytes, "next")?;
            if let Some(bytes) = &original {
                publish_complete(&self.sidecar("backup"), bytes, "backup")?;
            }
            sync_parent(&self.path)?;
            let current = read_optional(&self.path, MAX_BYTES, true)?;
            if current != original {
                return Err(error("provider_revision_conflict"));
            }
            check_artifact_token(&self.sidecar("next"), &transaction.after)?;
            fs::rename(self.sidecar("next"), &self.path)
                .map_err(|e| io_error("provider_commit_failed", e))?;
            provider_checkpoint!("main.renamed");
            sync_parent(&self.path)?;
            provider_checkpoint!("main.synced");
            self.clean_transaction(&transaction)?;
            Ok(after)
        })()
    }
    /// Explicit crash recovery. Unknown or inconsistent materials are preserved.
    pub fn recover(&self) -> Result<ProviderSnapshot, ProviderError> {
        self.prepare_parent()?;
        let _lock = self.acquire_lock(true)?;
        provider_checkpoint!("recovery.acquired");
        let journal = read_optional(&self.sidecar("transaction"), MAX_ARTIFACT_BYTES, true)?;
        let Some(journal) = journal else {
            if self.has_pending()? {
                return Err(error("provider_unknown_recovery_material"));
            }
            return snapshot_from_bytes(read_optional(&self.path, MAX_BYTES, true)?.as_deref());
        };
        let transaction = Transaction::parse(&journal)?;
        let main = read_optional(&self.path, MAX_BYTES, true)?;
        let main_snapshot = snapshot_from_bytes(main.as_deref())?;
        if main_snapshot.token != transaction.before && main_snapshot.token != transaction.after {
            // Missing old main can be restored only from its exact known backup.
            if main.is_none() && transaction.existed {
                let backup = read_optional(&self.sidecar("backup"), MAX_BYTES, true)?
                    .ok_or_else(|| error("provider_unknown_recovery_material"))?;
                if snapshot_from_bytes(Some(&backup))?.token != transaction.before {
                    return Err(error("provider_unknown_recovery_material"));
                }
                check_artifact_token(&self.sidecar("next"), &transaction.after)?;
                fs::rename(self.sidecar("backup"), &self.path)
                    .map_err(|e| io_error("provider_recovery_failed", e))?;
                sync_parent(&self.path)?;
            } else {
                return Err(error("provider_recovery_conflict"));
            }
        }
        self.clean_transaction(&transaction)?;
        snapshot_from_bytes(read_optional(&self.path, MAX_BYTES, true)?.as_deref())
    }
    fn clean_transaction(&self, transaction: &Transaction) -> Result<(), ProviderError> {
        check_artifact_token(&self.sidecar("next"), &transaction.after)?;
        if transaction.existed {
            check_artifact_token(&self.sidecar("backup"), &transaction.before)?;
        } else if path_exists(&self.sidecar("backup"))? {
            return Err(error("provider_unknown_recovery_material"));
        }
        // Delete only artifacts whose validated bytes match this transaction.
        remove_if_present(&self.sidecar("next"))?;
        provider_checkpoint!("cleanup.next_removed");
        remove_if_present(&self.sidecar("backup"))?;
        provider_checkpoint!("cleanup.backup_removed");
        let journal = read_optional(&self.sidecar("transaction"), MAX_ARTIFACT_BYTES, true)?
            .ok_or_else(|| error("provider_recovery_conflict"))?;
        if Transaction::parse(&journal)? != *transaction {
            return Err(error("provider_recovery_conflict"));
        }
        fs::remove_file(self.sidecar("transaction"))
            .map_err(|e| io_error("provider_cleanup_failed", e))?;
        provider_checkpoint!("cleanup.transaction_removed");
        sync_parent(&self.path)
    }
    fn sidecar(&self, suffix: &str) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(format!(".{suffix}"));
        PathBuf::from(name)
    }
    fn has_pending(&self) -> Result<bool, ProviderError> {
        for suffix in ["transaction", "next", "backup"] {
            if path_exists(&self.sidecar(suffix))? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn check_path(&self) -> Result<(), ProviderError> {
        if !self.path.is_absolute() || self.path.file_name().is_none() {
            return Err(error("provider_store_absolute_path_required"));
        }
        for ancestor in self.path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(error("provider_symlink_rejected"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error("provider_path_unreadable", e)),
            }
        }
        Ok(())
    }
    fn prepare_parent(&self) -> Result<(), ProviderError> {
        self.check_path()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("provider_store_absolute_path_required"))?;
        let existed = parent.exists();
        fs::create_dir_all(parent).map_err(|e| io_error("provider_directory_create_failed", e))?;
        #[cfg(unix)]
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|e| io_error("provider_permissions_failed", e))?;
        }
        #[cfg(not(unix))]
        let _ = existed;
        self.check_path()
    }
    fn acquire_lock(&self, _recovery: bool) -> Result<StoreLock, ProviderError> {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        {
            v2_lock::acquire(self, true)?.ok_or_else(|| error("provider_lock_failed"))
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            Err(error("provider_lock_unsupported"))
        }
    }
    fn acquire_read_lock(&self) -> Result<Option<StoreLock>, ProviderError> {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        {
            v2_lock::acquire(self, false)
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            if path_exists(&self.sidecar("lock"))? || path_exists(&self.sidecar("lock.v2"))? {
                Err(error("provider_lock_unsupported"))
            } else {
                Ok(None)
            }
        }
    }
}

// SIGKILL closes the fd in the kernel. Normal release explicitly unlocks before
// closing: another thread's fork can briefly inherit this open-file-description
// until CLOEXEC takes effect. Neither path ever unlinks a published lock inode.
struct StoreLock {
    _file: File,
}
impl Drop for StoreLock {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::NonBlockingUnlock);
    }
}

fn publish_complete(path: &Path, bytes: &[u8], stage: &str) -> Result<(), ProviderError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        v2_lock::publish_complete(path, bytes, stage)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (path, bytes, stage);
        Err(error("provider_lock_unsupported"))
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod v2_lock {
    use super::*;
    use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
    use rustix::io::Errno;
    use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(super) const MARKER: &[u8] = b"magpie-provider-lock-v2\n";
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

    pub(super) fn acquire(
        store: &ProviderStore,
        exclusive: bool,
    ) -> Result<Option<StoreLock>, ProviderError> {
        let legacy = store.sidecar("lock");
        let current = store.sidecar("lock.v2");
        for _ in 0..3 {
            check_guard(store)?;
            let file = match open_existing(&legacy, exclusive)? {
                Some(file) => Some(file),
                None => open_existing(&current, exclusive)?,
            };
            if let Some(file) = file {
                lock(&file, exclusive)?;
                let guard = StoreLock { _file: file };
                #[cfg(test)]
                super::tests::os_lock_tests::capture_inherited_if_armed(&guard._file);
                verify_marker(&guard._file)?;
                let present = verify_entries(&guard._file, &legacy, &current)?;
                if exclusive {
                    // A SIGKILL during first publication can leave one valid
                    // entry. Repair only under this inode's exclusive OS lock,
                    // with hard_link's no-overwrite semantics.
                    if !present.0 {
                        link_entry(&current, &legacy)?;
                    }
                    if !present.1 {
                        link_entry(&legacy, &current)?;
                    }
                    verify_entries(&guard._file, &legacy, &current)?;
                    if !present.0 || !present.1 {
                        sync_parent(&legacy)?;
                    }
                }
                check_guard(store)?;
                return Ok(Some(guard));
            }
            if !exclusive {
                return Ok(None);
            }
            // Never expose an empty or partially written canonical marker.
            let mut stage = PrivateStage::create(&legacy, MARKER, "lock.marker")?;
            lock(stage.file(), true)?;
            stage.locked = true;
            check_guard(store)?;
            match fs::hard_link(&stage.path, &legacy) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error("provider_lock_failed", e)),
            }
            provider_checkpoint!("lock.legacy_published");
            link_entry(&legacy, &current)?;
            provider_checkpoint!("lock.v2_published");
            verify_entries(stage.file(), &legacy, &current)?;
            check_guard(store)?;
            sync_parent(&legacy)?;
            let file = stage.finish()?;
            return Ok(Some(StoreLock { _file: file }));
        }
        Err(error("provider_busy"))
    }

    fn check_guard(store: &ProviderStore) -> Result<(), ProviderError> {
        if path_exists(&store.sidecar("recovery-guard"))? {
            Err(error("provider_recovery_guard_present"))
        } else {
            Ok(())
        }
    }
    fn open_existing(path: &Path, exclusive: bool) -> Result<Option<File>, ProviderError> {
        let access = if exclusive {
            OFlags::RDWR
        } else {
            OFlags::RDONLY
        };
        let fd = match open(
            path,
            access | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP) => return Err(error("provider_lock_mismatch")),
            Err(Errno::ISDIR) => return Err(error("provider_unknown_lock")),
            Err(e) => return Err(io_error("provider_lock_failed", e.into())),
        };
        let file = File::from(fd);
        verify_marker(&file)?;
        Ok(Some(file))
    }
    fn lock(file: &File, exclusive: bool) -> Result<(), ProviderError> {
        let operation = if exclusive {
            FlockOperation::NonBlockingLockExclusive
        } else {
            FlockOperation::NonBlockingLockShared
        };
        flock(file, operation).map_err(|e| {
            if e == Errno::WOULDBLOCK || e == Errno::AGAIN {
                error("provider_busy")
            } else if e == Errno::OPNOTSUPP || e == Errno::NOSYS {
                error("provider_lock_unsupported")
            } else {
                io_error("provider_lock_failed", e.into())
            }
        })
    }
    fn verify_marker(file: &File) -> Result<(), ProviderError> {
        let metadata = file
            .metadata()
            .map_err(|e| io_error("provider_lock_failed", e))?;
        if !metadata.is_file() {
            return Err(error("provider_unknown_lock"));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(error("provider_insecure_permissions"));
        }
        if metadata.len() != MARKER.len() as u64 {
            return Err(error("provider_unknown_lock"));
        }
        let mut bytes = [0u8; MARKER.len()];
        file.read_exact_at(&mut bytes, 0)
            .map_err(|e| io_error("provider_lock_failed", e))?;
        if bytes != MARKER {
            return Err(error("provider_unknown_lock"));
        }
        Ok(())
    }
    fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    fn verify_entries(
        file: &File,
        legacy: &Path,
        current: &Path,
    ) -> Result<(bool, bool), ProviderError> {
        let opened = file
            .metadata()
            .map_err(|e| io_error("provider_lock_failed", e))?;
        let mut present = [false; 2];
        for (index, path) in [legacy, current].iter().enumerate() {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_error("provider_lock_failed", e)),
            };
            if !metadata.is_file() || !same_inode(&opened, &metadata) {
                return Err(error("provider_lock_mismatch"));
            }
            present[index] = true;
        }
        if !present[0] && !present[1] {
            return Err(error("provider_lock_mismatch"));
        }
        verify_marker(file)?;
        Ok((present[0], present[1]))
    }
    fn link_entry(source: &Path, destination: &Path) -> Result<(), ProviderError> {
        match fs::hard_link(source, destination) {
            Ok(()) => Ok(()),
            // The caller always verifies both entries against the held fd.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(io_error("provider_lock_failed", e)),
        }
    }

    pub(super) fn publish_complete(
        path: &Path,
        bytes: &[u8],
        stage: &str,
    ) -> Result<(), ProviderError> {
        let staged = PrivateStage::create(path, bytes, stage)?;
        fs::hard_link(&staged.path, path).map_err(|e| io_error("provider_write_failed", e))?;
        provider_checkpoint!(&format!("{stage}.published"));
        staged.verify_published(path)?;
        sync_parent(path)?;
        staged.finish()?;
        Ok(())
    }

    // Unpublished stage names never participate in has_pending(). A SIGKILL can
    // leave one behind; without a journal ownership proof we preserve it. It is
    // bounded to one artifact's bytes and cannot prevent the next transaction.
    struct PrivateStage {
        path: PathBuf,
        file: Option<File>,
        length: usize,
        digest: String,
        locked: bool,
    }
    impl PrivateStage {
        fn create(path: &Path, bytes: &[u8], stage: &str) -> Result<Self, ProviderError> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| error("provider_write_failed"))?
                .as_nanos();
            for _ in 0..32 {
                let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
                let mut name = path.as_os_str().to_os_string();
                name.push(format!(".stage-v2-{}-{now}-{sequence}", std::process::id()));
                let path = PathBuf::from(name);
                let file = match OpenOptions::new()
                    .write(true)
                    .read(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(io_error("provider_write_failed", e)),
                };
                let mut staged = Self {
                    path,
                    file: Some(file),
                    length: bytes.len(),
                    digest: sha256(bytes),
                    locked: false,
                };
                provider_checkpoint!(&format!("{stage}.created"));
                #[cfg(test)]
                {
                    let midpoint = bytes.len() / 2;
                    staged
                        .file_mut()
                        .write_all(&bytes[..midpoint])
                        .map_err(|e| io_error("provider_write_failed", e))?;
                    provider_checkpoint!(&format!("{stage}.partial"));
                    staged
                        .file_mut()
                        .write_all(&bytes[midpoint..])
                        .map_err(|e| io_error("provider_write_failed", e))?;
                }
                #[cfg(not(test))]
                {
                    let _ = stage;
                    staged
                        .file_mut()
                        .write_all(bytes)
                        .map_err(|e| io_error("provider_write_failed", e))?;
                }
                staged
                    .file()
                    .sync_all()
                    .map_err(|e| io_error("provider_write_failed", e))?;
                provider_checkpoint!(&format!("{stage}.synced"));
                return Ok(staged);
            }
            Err(error("provider_write_failed"))
        }
        fn file(&self) -> &File {
            self.file.as_ref().expect("stage owns its file")
        }
        fn file_mut(&mut self) -> &mut File {
            self.file.as_mut().expect("stage owns its file")
        }
        fn verify_published(&self, destination: &Path) -> Result<(), ProviderError> {
            let metadata = fs::symlink_metadata(destination)
                .map_err(|e| io_error("provider_write_failed", e))?;
            let opened = self
                .file()
                .metadata()
                .map_err(|e| io_error("provider_write_failed", e))?;
            if !metadata.is_file()
                || !same_inode(&metadata, &opened)
                || opened.len() != self.length as u64
            {
                return Err(error("provider_unknown_recovery_material"));
            }
            let mut bytes = vec![0; self.length];
            self.file()
                .read_exact_at(&mut bytes, 0)
                .map_err(|e| io_error("provider_write_failed", e))?;
            if sha256(&bytes) != self.digest {
                return Err(error("provider_unknown_recovery_material"));
            }
            Ok(())
        }
        fn finish(mut self) -> Result<File, ProviderError> {
            self.unlink_owned()?;
            Ok(self.file.take().expect("stage owns its file"))
        }
        fn unlink_owned(&self) -> Result<(), ProviderError> {
            let Some(file) = &self.file else {
                return Ok(());
            };
            let metadata = match fs::symlink_metadata(&self.path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(io_error("provider_cleanup_failed", e)),
            };
            let opened = file
                .metadata()
                .map_err(|e| io_error("provider_cleanup_failed", e))?;
            if !metadata.is_file()
                || !same_inode(&metadata, &opened)
                || opened.len() != self.length as u64
            {
                return Ok(());
            }
            let mut bytes = vec![0; self.length];
            file.read_exact_at(&mut bytes, 0)
                .map_err(|e| io_error("provider_cleanup_failed", e))?;
            if sha256(&bytes) != self.digest {
                return Ok(());
            }
            fs::remove_file(&self.path).map_err(|e| io_error("provider_cleanup_failed", e))
        }
    }
    impl Drop for PrivateStage {
        fn drop(&mut self) {
            let _ = self.unlink_owned();
            if self.locked {
                if let Some(file) = &self.file {
                    // Also release a partially published marker immediately on
                    // ordinary error paths, despite unrelated fork/exec copies.
                    let _ = flock(file, FlockOperation::NonBlockingUnlock);
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Transaction {
    before: String,
    after: String,
    existed: bool,
}
impl Transaction {
    fn encode(&self) -> String {
        let mut doc = DocumentMut::new();
        doc["format"] = value("magpie-provider-transaction-v1");
        doc["before"] = value(self.before.clone());
        doc["after"] = value(self.after.clone());
        doc["existed"] = value(self.existed);
        doc.to_string()
    }
    fn parse(bytes: &[u8]) -> Result<Self, ProviderError> {
        let source =
            std::str::from_utf8(bytes).map_err(|_| error("provider_unknown_recovery_material"))?;
        let doc = source
            .parse::<DocumentMut>()
            .map_err(|_| error("provider_unknown_recovery_material"))?;
        let table = doc.as_table();
        known_fields(table, &["format", "before", "after", "existed"])?;
        if text(table, "format")? != "magpie-provider-transaction-v1" {
            return Err(error("provider_unknown_recovery_material"));
        }
        let before = text(table, "before")?.to_owned();
        let after = text(table, "after")?.to_owned();
        if !valid_token(&before) || !valid_token(&after) {
            return Err(error("provider_unknown_recovery_material"));
        }
        Ok(Self {
            before,
            after,
            existed: boolean(table, "existed")?,
        })
    }
}
fn valid_token(token: &str) -> bool {
    token.split_once(':').is_some_and(|(rev, hash)| {
        rev.parse::<u64>().is_ok()
            && hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
    })
}
fn check_artifact_token(path: &Path, expected: &str) -> Result<(), ProviderError> {
    if let Some(bytes) = read_optional(path, MAX_BYTES, true)? {
        if snapshot_from_bytes(Some(&bytes))?.token != expected {
            return Err(error("provider_unknown_recovery_material"));
        }
    }
    Ok(())
}
fn path_exists(path: &Path) -> Result<bool, ProviderError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error("provider_path_unreadable", e)),
    }
}
fn read_optional(path: &Path, limit: u64, private: bool) -> Result<Option<Vec<u8>>, ProviderError> {
    let observed = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            provider_checkpoint!("read.absent");
            // Missing is a point-in-time observation, never a fallback for a
            // file that vanished after being observed. Refuse a concurrently
            // published entry if the second check can observe it.
            return match fs::symlink_metadata(path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(io_error("provider_store_unreadable", e)),
                Ok(_) => Err(error("provider_read_conflict")),
            };
        }
        Err(e) => return Err(io_error("provider_store_unreadable", e)),
    };
    validate_read_metadata(&observed, limit, private)?;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        provider_checkpoint!("read.checked_path");
        let mut file = open_read_file(path)?;
        let opened = file
            .metadata()
            .map_err(|e| io_error("provider_store_unreadable", e))?;
        validate_read_metadata(&opened, limit, private)?;
        ensure_same_read_file(&observed, &opened)?;
        provider_checkpoint!("read.opened");
        let current = read_path_metadata(path)?;
        validate_read_metadata(&current, limit, private)?;
        ensure_same_read_file(&opened, &current)?;
        provider_checkpoint!("read.before_bytes");
        let mut bytes = Vec::new();
        (&mut file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("provider_store_unreadable", e))?;
        if bytes.len() as u64 > limit {
            return Err(error("provider_store_too_large"));
        }
        provider_checkpoint!("read.after_bytes");
        // Keep the descriptor open through both post-read checks. No bytes are
        // returned/parsed if an observed identity, permission or content stamp
        // changed. This is defensive detection, not atomicity against arbitrary
        // noncooperating mutation after the final check.
        let finished = file
            .metadata()
            .map_err(|e| io_error("provider_store_unreadable", e))?;
        validate_read_metadata(&finished, limit, private)?;
        ensure_same_read_file(&opened, &finished)?;
        let current = read_path_metadata(path)?;
        validate_read_metadata(&current, limit, private)?;
        ensure_same_read_file(&finished, &current)?;
        if bytes.len() as u64 != opened.len() {
            return Err(error("provider_read_conflict"));
        }
        Ok(Some(bytes))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        // No unsafe File::open fallback on platforms whose private/no-follow
        // descriptor identity semantics have not been implemented.
        Err(error("provider_read_unsupported"))
    }
}
fn validate_read_metadata(
    metadata: &fs::Metadata,
    limit: u64,
    private: bool,
) -> Result<(), ProviderError> {
    if !metadata.is_file() {
        return Err(error("provider_regular_file_required"));
    }
    if metadata.len() > limit {
        return Err(error("provider_store_too_large"));
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(error("provider_insecure_permissions"));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn open_read_file(path: &Path) -> Result<File, ProviderError> {
    use rustix::fs::{Mode, OFlags, open};
    use rustix::io::Errno;
    open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|cause| {
        if cause == Errno::NOENT || cause == Errno::NOTDIR {
            error("provider_read_conflict")
        } else if cause == Errno::LOOP || cause == Errno::ISDIR {
            error("provider_regular_file_required")
        } else if cause == Errno::OPNOTSUPP || cause == Errno::NOSYS {
            error("provider_read_unsupported")
        } else {
            io_error("provider_store_unreadable", cause.into())
        }
    })
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn read_path_metadata(path: &Path) -> Result<fs::Metadata, ProviderError> {
    fs::symlink_metadata(path).map_err(|cause| {
        if cause.kind() == io::ErrorKind::NotFound {
            error("provider_read_conflict")
        } else {
            io_error("provider_store_unreadable", cause)
        }
    })
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn ensure_same_read_file(before: &fs::Metadata, after: &fs::Metadata) -> Result<(), ProviderError> {
    use std::os::unix::fs::MetadataExt;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mode() != after.mode()
        || before.uid() != after.uid()
        || before.gid() != after.gid()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        Err(error("provider_read_conflict"))
    } else {
        Ok(())
    }
}
#[cfg(test)]
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| io_error("provider_write_failed", e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| io_error("provider_write_failed", e))
}
fn sync_parent(path: &Path) -> Result<(), ProviderError> {
    #[cfg(unix)]
    {
        File::open(
            path.parent()
                .ok_or_else(|| error("provider_store_absolute_path_required"))?,
        )
        .and_then(|f| f.sync_all())
        .map_err(|e| io_error("provider_directory_sync_failed", e))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
fn remove_if_present(path: &Path) -> Result<(), ProviderError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error("provider_cleanup_failed", e)),
    }
}

fn snapshot_from_bytes(bytes: Option<&[u8]>) -> Result<ProviderSnapshot, ProviderError> {
    let Some(bytes) = bytes else {
        return Ok(ProviderSnapshot {
            revision: 0,
            token: format!("0:{}", sha256(b"magpie-provider-store-missing-v1")),
            providers: Vec::new(),
        });
    };
    if bytes.len() as u64 > MAX_BYTES {
        return Err(error("provider_store_too_large"));
    }
    let source = std::str::from_utf8(bytes).map_err(|_| error("provider_invalid_utf8"))?;
    let doc = source
        .parse::<DocumentMut>()
        .map_err(|_| error("provider_invalid_toml"))?;
    let table = doc.as_table();
    known_fields(table, &["schema_version", "revision", "providers"])?;
    if integer(table, "schema_version")? != u64::from(SCHEMA_VERSION) {
        return Err(error("provider_unsupported_schema"));
    }
    let revision = integer(table, "revision")?;
    if revision == 0 {
        return Err(error("provider_invalid_revision"));
    }
    let mut providers = Vec::new();
    for table in tables(table, "providers")? {
        known_fields(
            table,
            &[
                "id",
                "name",
                "base_url",
                "protocol",
                "enabled",
                "deleted",
                "allow_insecure_http",
                "keys",
                "models",
                "selected_models",
                "auto_include",
                "catalog_checked_at",
                "catalog_error",
            ],
        )?;
        let mut keys = Vec::new();
        for key in tables(table, "keys")? {
            known_fields(key, &["id", "name", "env", "enabled", "deleted"])?;
            keys.push(ProviderKey {
                id: text(key, "id")?.to_owned(),
                name: text(key, "name")?.to_owned(),
                env: text(key, "env")?.to_owned(),
                enabled: boolean(key, "enabled")?,
                deleted: boolean(key, "deleted")?,
            });
        }
        providers.push(Provider {
            id: text(table, "id")?.to_owned(),
            name: text(table, "name")?.to_owned(),
            base_url: text(table, "base_url")?.to_owned(),
            protocol: Protocol::parse(text(table, "protocol")?)?,
            enabled: boolean(table, "enabled")?,
            deleted: boolean(table, "deleted")?,
            allow_insecure_http: boolean(table, "allow_insecure_http")?,
            keys,
            models: strings(table, "models")?,
            selected_models: strings(table, "selected_models")?,
            auto_include: boolean(table, "auto_include")?,
            catalog_checked_at: optional_integer(table, "catalog_checked_at")?,
            catalog_error: optional_text(table, "catalog_error")?.map(str::to_owned),
        });
    }
    validate_providers(&providers)?;
    Ok(ProviderSnapshot {
        revision,
        token: format!("{revision}:{}", sha256(bytes)),
        providers,
    })
}
fn known_fields(table: &Table, allowed: &[&str]) -> Result<(), ProviderError> {
    if table.iter().any(|(key, _)| !allowed.contains(&key)) {
        Err(error("provider_unknown_field"))
    } else {
        Ok(())
    }
}
fn text<'a>(table: &'a Table, field: &str) -> Result<&'a str, ProviderError> {
    table
        .get(field)
        .and_then(Item::as_str)
        .ok_or_else(|| error("provider_invalid_string_field"))
}
fn optional_text<'a>(table: &'a Table, field: &str) -> Result<Option<&'a str>, ProviderError> {
    table
        .get(field)
        .map(|v| {
            v.as_str()
                .ok_or_else(|| error("provider_invalid_string_field"))
        })
        .transpose()
}
fn boolean(table: &Table, field: &str) -> Result<bool, ProviderError> {
    table
        .get(field)
        .and_then(Item::as_bool)
        .ok_or_else(|| error("provider_invalid_bool_field"))
}
fn integer(table: &Table, field: &str) -> Result<u64, ProviderError> {
    table
        .get(field)
        .and_then(Item::as_integer)
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| error("provider_invalid_integer_field"))
}
fn optional_integer(table: &Table, field: &str) -> Result<Option<u64>, ProviderError> {
    if table.contains_key(field) {
        integer(table, field).map(Some)
    } else {
        Ok(None)
    }
}
fn strings(table: &Table, field: &str) -> Result<Vec<String>, ProviderError> {
    let array = table
        .get(field)
        .and_then(Item::as_array)
        .ok_or_else(|| error("provider_invalid_models_field"))?;
    if array.len() > MAX_MODELS {
        return Err(error("provider_models_limit"));
    }
    array
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| error("provider_invalid_model"))
        })
        .collect()
}
fn tables<'a>(table: &'a Table, field: &str) -> Result<Vec<&'a Table>, ProviderError> {
    let Some(item) = table.get(field) else {
        return Err(error("provider_missing_field"));
    };
    if item.as_array().is_some_and(Array::is_empty) {
        return Ok(Vec::new());
    }
    let array = item
        .as_array_of_tables()
        .ok_or_else(|| error("provider_invalid_table_array"))?;
    let limit = if field == "providers" {
        MAX_PROVIDERS
    } else {
        MAX_KEYS
    };
    if array.len() > limit {
        return Err(error("provider_collection_limit"));
    }
    Ok(array.iter().collect())
}
fn array(strings: &[String]) -> Array {
    let mut a = Array::new();
    for s in strings {
        a.push(s.as_str());
    }
    a
}
fn encode(snapshot: &ProviderSnapshot) -> Result<String, ProviderError> {
    let mut doc = DocumentMut::new();
    doc["schema_version"] = value(i64::from(SCHEMA_VERSION));
    doc["revision"] =
        value(i64::try_from(snapshot.revision).map_err(|_| error("provider_revision_exhausted"))?);
    let mut providers = ArrayOfTables::new();
    for p in &snapshot.providers {
        let mut t = Table::new();
        t["id"] = value(p.id.clone());
        t["name"] = value(p.name.clone());
        t["base_url"] = value(p.base_url.clone());
        t["protocol"] = value(p.protocol.as_str());
        t["enabled"] = value(p.enabled);
        t["deleted"] = value(p.deleted);
        t["allow_insecure_http"] = value(p.allow_insecure_http);
        t["auto_include"] = value(p.auto_include);
        t["models"] = value(array(&p.models));
        t["selected_models"] = value(array(&p.selected_models));
        if let Some(time) = p.catalog_checked_at {
            t["catalog_checked_at"] =
                value(i64::try_from(time).map_err(|_| error("provider_invalid_timestamp"))?);
        }
        if let Some(code) = &p.catalog_error {
            t["catalog_error"] = value(code.clone());
        }
        if p.keys.is_empty() {
            t["keys"] = value(Array::new());
        } else {
            let mut keys = ArrayOfTables::new();
            for k in &p.keys {
                let mut kt = Table::new();
                kt["id"] = value(k.id.clone());
                kt["name"] = value(k.name.clone());
                kt["env"] = value(k.env.clone());
                kt["enabled"] = value(k.enabled);
                kt["deleted"] = value(k.deleted);
                keys.push(kt);
            }
            t["keys"] = Item::ArrayOfTables(keys);
        }
        providers.push(t);
    }
    if providers.is_empty() {
        doc["providers"] = value(Array::new());
    } else {
        doc["providers"] = Item::ArrayOfTables(providers);
    }
    Ok(doc.to_string())
}

fn apply_mutation(
    providers: &mut Vec<Provider>,
    mutation: ProviderMutation,
) -> Result<(), ProviderError> {
    match mutation {
        ProviderMutation::Add(p) => {
            if p.deleted || providers.iter().any(|v| v.id == p.id) {
                return Err(error("provider_id_exists"));
            }
            if p.keys.iter().any(|k| k.deleted) {
                return Err(error("provider_invalid_new_key"));
            }
            providers.push(p);
        }
        ProviderMutation::Edit {
            id,
            name,
            base_url,
            protocol,
            enabled,
            allow_insecure_http,
        } => {
            let p = find_mut(providers, &id)?;
            if let Some(v) = name {
                p.name = v;
            }
            if let Some(v) = base_url {
                p.base_url = v;
            }
            if let Some(v) = protocol {
                p.protocol = v;
            }
            if let Some(v) = enabled {
                p.enabled = v;
            }
            if let Some(v) = allow_insecure_http {
                p.allow_insecure_http = v;
            }
        }
        ProviderMutation::Remove { id } => {
            let p = find_mut(providers, &id)?;
            p.enabled = false;
            p.deleted = true;
            for key in &mut p.keys {
                key.enabled = false;
                key.deleted = true;
                key.env.clear();
            }
        }
        ProviderMutation::PutKey { provider_id, key } => {
            if key.deleted {
                return Err(error("provider_invalid_new_key"));
            }
            let p = find_mut(providers, &provider_id)?;
            if let Some(existing) = p.keys.iter_mut().find(|k| k.id == key.id) {
                if existing.deleted {
                    return Err(error("provider_key_retired"));
                }
                *existing = key;
            } else {
                p.keys.push(key);
            }
        }
        ProviderMutation::RemoveKey {
            provider_id,
            key_id,
        } => {
            let p = find_mut(providers, &provider_id)?;
            let key = p
                .keys
                .iter_mut()
                .find(|k| k.id == key_id && !k.deleted)
                .ok_or_else(|| error("provider_key_not_found"))?;
            key.deleted = true;
            key.enabled = false;
            key.env.clear();
        }
        ProviderMutation::ReorderKeys {
            provider_id,
            key_ids,
        } => {
            let p = find_mut(providers, &provider_id)?;
            let existing: BTreeSet<_> = p
                .keys
                .iter()
                .filter(|k| !k.deleted)
                .map(|k| k.id.as_str())
                .collect();
            if key_ids.len() != existing.len()
                || key_ids.iter().map(String::as_str).collect::<BTreeSet<_>>() != existing
            {
                return Err(error("provider_key_order_mismatch"));
            }
            let mut ordered = Vec::with_capacity(p.keys.len());
            for id in key_ids {
                ordered.push(
                    p.keys
                        .iter()
                        .find(|k| k.id == id)
                        .expect("validated key ID")
                        .clone(),
                );
            }
            ordered.extend(p.keys.iter().filter(|k| k.deleted).cloned());
            p.keys = ordered;
        }
        ProviderMutation::Models {
            provider_id,
            models,
            selected_models,
            auto_include,
        } => {
            let p = find_mut(providers, &provider_id)?;
            p.models = models;
            p.selected_models = selected_models;
            p.auto_include = auto_include;
        }
        ProviderMutation::CatalogSuccess {
            provider_id,
            models,
            checked_at,
        } => {
            let p = find_mut(providers, &provider_id)?;
            p.selected_models.retain(|m| models.contains(m));
            p.models = models;
            p.catalog_checked_at = Some(checked_at);
            p.catalog_error = None;
        }
        ProviderMutation::CatalogFailure { provider_id, code } => {
            let p = find_mut(providers, &provider_id)?;
            if !valid_catalog_error(&code) {
                return Err(error("provider_invalid_catalog_error"));
            }
            p.catalog_error = Some(code);
        }
    }
    Ok(())
}
fn find_mut<'a>(
    providers: &'a mut [Provider],
    id: &str,
) -> Result<&'a mut Provider, ProviderError> {
    providers
        .iter_mut()
        .find(|p| p.id == id && !p.deleted)
        .ok_or_else(|| error("provider_not_found"))
}
fn validate_providers(providers: &[Provider]) -> Result<(), ProviderError> {
    if providers.len() > MAX_PROVIDERS {
        return Err(error("provider_collection_limit"));
    }
    let mut ids = BTreeSet::new();
    for p in providers {
        if !valid_id(&p.id) || !ids.insert(&p.id) {
            return Err(error("provider_invalid_or_duplicate_id"));
        }
        if !valid_name(&p.name) {
            return Err(error("provider_invalid_name"));
        }
        validate_base_url(&p.base_url, p.allow_insecure_http)?;
        if p.deleted && p.enabled {
            return Err(error("provider_deleted_enabled"));
        }
        if p.keys.len() > MAX_KEYS {
            return Err(error("provider_keys_limit"));
        }
        let mut keys = BTreeSet::new();
        for k in &p.keys {
            if !valid_id(&k.id) || !keys.insert(&k.id) {
                return Err(error("provider_invalid_or_duplicate_key_id"));
            }
            if !valid_name(&k.name) {
                return Err(error("provider_invalid_key_name"));
            }
            if k.deleted {
                if k.enabled || !k.env.is_empty() {
                    return Err(error("provider_invalid_retired_key"));
                }
            } else if !valid_env(&k.env) {
                return Err(error("provider_invalid_credential_reference"));
            }
        }
        validate_models(&p.models)?;
        validate_models(&p.selected_models)?;
        if p.selected_models.iter().any(|m| !p.models.contains(m)) {
            return Err(error("provider_selected_model_unknown"));
        }
        if p.catalog_checked_at.is_some_and(|n| n > i64::MAX as u64) {
            return Err(error("provider_invalid_timestamp"));
        }
        if p.catalog_error
            .as_deref()
            .is_some_and(|c| !valid_catalog_error(c))
        {
            return Err(error("provider_invalid_catalog_error"));
        }
    }
    Ok(())
}
fn validate_models(models: &[String]) -> Result<(), ProviderError> {
    if models.len() > MAX_MODELS {
        return Err(error("provider_models_limit"));
    }
    let mut seen = BTreeSet::new();
    for model in models {
        if !is_valid_model_id(model) || !seen.insert(model) {
            return Err(error("provider_invalid_or_duplicate_model"));
        }
    }
    Ok(())
}
/// Provider-managed namespace IDs use the existing 1..128-byte ASCII grammar.
/// The first byte is alphanumeric; remaining bytes are alphanumeric or . _ -.
/// `group` is reserved. This does not change the separate legacy gateway grammar.
#[must_use]
pub fn is_valid_provider_id(s: &str) -> bool {
    valid_id(s)
}

fn valid_id(s: &str) -> bool {
    s.len() <= 128
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && s != "group"
}
fn valid_name(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
fn valid_env(s: &str) -> bool {
    s.len() <= 128
        && s.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
/// Provider-managed raw model IDs retain the existing 1..256-byte ASCII grammar:
/// an alphanumeric first byte, then alphanumeric or . _ - : @ /, with no empty,
/// dot or parent slash segments. This is shared with managed discovery/inspect;
/// it does not broaden the independent legacy ForwardConfig grammar.
#[must_use]
pub fn is_valid_model_id(s: &str) -> bool {
    s.len() <= 256
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/".contains(&b))
        && s.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
fn valid_catalog_error(s: &str) -> bool {
    matches!(
        s,
        "network_error"
            | "timeout"
            | "unauthorized"
            | "forbidden"
            | "rate_limited"
            | "upstream_error"
            | "invalid_response"
            | "empty_catalog"
            | "model_not_found"
            | "not_supported"
            | "cancelled"
            | "credential_unavailable"
            | "provider_catalog_failed"
    )
}

/// Syntax validation only; does not resolve DNS, follow redirects, or send keys.
pub fn validate_base_url(url: &str, allow_insecure_http: bool) -> Result<(), ProviderError> {
    if url.len() > 2048
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b == b'\\')
        || url.contains(['?', '#'])
    {
        return Err(error("provider_invalid_base_url"));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| error("provider_invalid_base_url"))?;
    if !matches!(scheme, "http" | "https") {
        return Err(error("provider_invalid_base_url"));
    }
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if authority.is_empty() || authority.contains(['@', '%']) || !authority.is_ascii() {
        return Err(error("provider_invalid_base_url"));
    }
    let (host, port) = if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| error("provider_invalid_base_url"))?;
        let host = &authority[1..close];
        let tail = &authority[close + 1..];
        if !tail.is_empty() && !tail.starts_with(':') {
            return Err(error("provider_invalid_base_url"));
        }
        let ip: IpAddr = host
            .parse()
            .map_err(|_| error("provider_invalid_base_url"))?;
        if !ip.is_ipv6() {
            return Err(error("provider_invalid_base_url"));
        }
        (host, tail.strip_prefix(':'))
    } else {
        if authority.matches(':').count() > 1 {
            return Err(error("provider_invalid_base_url"));
        }
        authority
            .split_once(':')
            .map_or((authority, None), |(h, p)| (h, Some(p)))
    };
    if let Some(port) = port {
        if port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return Err(error("provider_invalid_base_url"));
        }
    }
    let ip = host.parse::<IpAddr>().ok();
    if ip.is_none()
        && (host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || !label
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    || !label
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return Err(error("provider_invalid_base_url"));
    }
    if path
        .split('/')
        .any(|p| matches!(p, "." | "..") || p.to_ascii_lowercase().contains("%2e"))
        || path.contains('%')
    {
        return Err(error("provider_invalid_base_url"));
    }
    if scheme == "http" && !allow_insecure_http && !ip.is_some_and(|ip| ip.is_loopback()) {
        return Err(error("provider_insecure_http_requires_consent"));
    }
    Ok(())
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(",")
    )
}

// SHA-256 is used as a content-change token, not encryption or credential storage.
// A local implementation avoids introducing any new dependency or version pin.
fn sha256(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut data = input.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((input.len() as u64) * 8).to_be_bytes());
    for block in data.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, bytes) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(bytes.try_into().expect("four-byte SHA word"));
        }
        for i in 16..64 {
            let a = w[i - 15];
            let b = w[i - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[i] = h[i].wrapping_add(v);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        directory: PathBuf,
        store: ProviderStore,
    }
    impl Fixture {
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "magpie-provider-test-{}-{now}-{n}",
                std::process::id()
            ));
            fs::create_dir(&directory).unwrap();
            Self {
                store: ProviderStore::new(directory.join("providers.toml")),
                directory,
            }
        }
        fn add(&self) -> ProviderSnapshot {
            let empty = self.store.load().unwrap();
            self.store
                .apply(&empty.token, ProviderMutation::Add(provider()))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    fn key(id: &str, env: &str) -> ProviderKey {
        ProviderKey {
            id: id.to_owned(),
            name: format!("Key {id}"),
            env: env.to_owned(),
            enabled: true,
            deleted: false,
        }
    }
    fn provider() -> Provider {
        Provider {
            id: "fixture".into(),
            name: "合成 Provider".into(),
            base_url: "http://127.0.0.1:32123/v1".into(),
            protocol: Protocol::Chat,
            enabled: true,
            deleted: false,
            allow_insecure_http: false,
            keys: vec![
                key("primary", "MAGPIE_SYNTHETIC_KEY_A"),
                key("secondary", "MAGPIE_SYNTHETIC_KEY_B"),
            ],
            models: vec!["model-a".into(), "model-b".into()],
            selected_models: vec!["model-a".into()],
            auto_include: false,
            catalog_checked_at: None,
            catalog_error: None,
        }
    }
    fn pending(f: &Fixture, before: &ProviderSnapshot) -> (Transaction, Vec<u8>) {
        let mut after = before.clone();
        after.revision += 1;
        after.providers[0].name = "Edited".into();
        let bytes = encode(&after).unwrap().into_bytes();
        let transaction = Transaction {
            before: before.token.clone(),
            after: snapshot_from_bytes(Some(&bytes)).unwrap().token,
            existed: true,
        };
        write_new(
            &f.store.sidecar("transaction"),
            transaction.encode().as_bytes(),
        )
        .unwrap();
        (transaction, bytes)
    }
    #[test]
    fn missing_is_distinct_and_does_not_create_a_file() {
        let f = Fixture::new();
        let s = f.store.load().unwrap();
        assert_eq!(s.revision, 0);
        assert!(s.providers.is_empty());
        assert!(!f.store.path().exists());
        assert_eq!(s.token.len(), 66);
        assert_eq!(fs::read_dir(&f.directory).unwrap().count(), 0);
    }
    #[test]
    fn add_roundtrip_is_durable_and_public_json_contains_only_references() {
        let f = Fixture::new();
        let s = f.add();
        assert_eq!(s.revision, 1);
        assert_eq!(s, f.store.load().unwrap());
        let p = s.provider("fixture").unwrap();
        assert_eq!(p.enabled_models(), vec!["model-a"]);
        assert_eq!(p.active_keys().count(), 2);
        let view = s.to_json();
        assert!(view.contains("合成 Provider"));
        assert!(view.contains("MAGPIE_SYNTHETIC_KEY_A"));
        assert!(view.contains("••••"));
        assert!(!view.contains("api_key"));
        for suffix in ["transaction", "next", "backup"] {
            assert!(!f.store.sidecar(suffix).exists());
        }
    }
    #[test]
    fn stale_token_and_same_revision_external_edit_are_rejected() {
        let f = Fixture::new();
        let s = f.add();
        let original = fs::read(f.store.path()).unwrap();
        let mut altered = original.clone();
        altered.extend_from_slice(b"\n# third party comment\n");
        fs::write(f.store.path(), &altered).unwrap();
        let e = f
            .store
            .apply(
                &s.token,
                ProviderMutation::Remove {
                    id: "fixture".into(),
                },
            )
            .unwrap_err();
        assert_eq!(e.code(), "provider_revision_conflict");
        assert_eq!(fs::read(f.store.path()).unwrap(), altered);
        let newer = f.store.load().unwrap();
        assert_eq!(newer.revision, s.revision);
        assert_ne!(newer.token, s.token);
    }
    #[test]
    fn malformed_unknown_fields_and_future_schema_preserve_original_bytes() {
        for source in [
            "schema_version=99\nrevision=1\nproviders=[]\n",
            "schema_version=1\nrevision=1\nproviders=[]\nsecret_field='synthetic-secret'\n",
            "schema_version='synthetic-secret",
        ] {
            let f = Fixture::new();
            write_new(f.store.path(), source.as_bytes()).unwrap();
            let e = f.store.load().unwrap_err();
            assert!(!format!("{e} {e:?}").contains("synthetic-secret"));
            assert_eq!(fs::read(f.store.path()).unwrap(), source.as_bytes());
        }
    }
    #[test]
    fn key_rotation_pause_order_and_tombstones_keep_stable_identity() {
        let f = Fixture::new();
        let s = f.add();
        let mut rotated = key("primary", "MAGPIE_SYNTHETIC_KEY_NEW");
        rotated.name = "Renamed".into();
        rotated.enabled = false;
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::PutKey {
                    provider_id: "fixture".into(),
                    key: rotated,
                },
            )
            .unwrap();
        assert_eq!(
            s.provider("fixture")
                .unwrap()
                .active_keys()
                .map(|k| k.id.as_str())
                .collect::<Vec<_>>(),
            vec!["secondary"]
        );
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::ReorderKeys {
                    provider_id: "fixture".into(),
                    key_ids: vec!["secondary".into(), "primary".into()],
                },
            )
            .unwrap();
        assert_eq!(s.provider("fixture").unwrap().keys[0].id, "secondary");
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::RemoveKey {
                    provider_id: "fixture".into(),
                    key_id: "primary".into(),
                },
            )
            .unwrap();
        let tombstone = &s.provider("fixture").unwrap().keys[1];
        assert_eq!(tombstone.id, "primary");
        assert_eq!(tombstone.name, "Renamed");
        assert!(tombstone.env.is_empty());
        assert!(tombstone.deleted);
        assert_eq!(
            f.store
                .apply(
                    &s.token,
                    ProviderMutation::PutKey {
                        provider_id: "fixture".into(),
                        key: key("primary", "MAGPIE_SYNTHETIC_REUSE")
                    }
                )
                .unwrap_err()
                .code(),
            "provider_key_retired"
        );
    }
    #[test]
    fn catalog_error_retains_models_timestamp_and_selection() {
        let f = Fixture::new();
        let s = f.add();
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::CatalogSuccess {
                    provider_id: "fixture".into(),
                    models: vec!["model-a".into(), "new-model".into()],
                    checked_at: 42,
                },
            )
            .unwrap();
        let expected = s.providers[0].clone();
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::CatalogFailure {
                    provider_id: "fixture".into(),
                    code: "unauthorized".into(),
                },
            )
            .unwrap();
        let p = &s.providers[0];
        assert_eq!(p.models, expected.models);
        assert_eq!(p.selected_models, expected.selected_models);
        assert_eq!(p.catalog_checked_at, Some(42));
        assert_eq!(p.catalog_error.as_deref(), Some("unauthorized"));
        let before = fs::read(f.store.path()).unwrap();
        assert!(
            f.store
                .apply(
                    &s.token,
                    ProviderMutation::CatalogFailure {
                        provider_id: "fixture".into(),
                        code: "synthetic-secret".into()
                    }
                )
                .is_err()
        );
        assert_eq!(before, fs::read(f.store.path()).unwrap());
    }
    #[test]
    fn refresh_prunes_unavailable_selection_and_auto_includes_new_models() {
        let f = Fixture::new();
        let s = f.add();
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::CatalogSuccess {
                    provider_id: "fixture".into(),
                    models: vec!["model-b".into(), "new-model".into()],
                    checked_at: 50,
                },
            )
            .unwrap();
        assert!(s.providers[0].selected_models.is_empty());
        assert!(s.providers[0].enabled_models().is_empty());
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::Models {
                    provider_id: "fixture".into(),
                    models: s.providers[0].models.clone(),
                    selected_models: vec![],
                    auto_include: true,
                },
            )
            .unwrap();
        assert_eq!(
            s.providers[0].enabled_models(),
            vec!["model-b", "new-model"]
        );
    }
    #[test]
    fn unknown_selected_model_duplicate_and_unsafe_ids_do_not_write() {
        let f = Fixture::new();
        let s = f.add();
        let before = fs::read(f.store.path()).unwrap();
        for models in [
            vec!["bad id".into()],
            vec!["a".into(), "a".into()],
            vec!["a/../b".into()],
        ] {
            assert!(
                f.store
                    .apply(
                        &s.token,
                        ProviderMutation::Models {
                            provider_id: "fixture".into(),
                            models,
                            selected_models: vec![],
                            auto_include: true
                        }
                    )
                    .is_err()
            );
            assert_eq!(before, fs::read(f.store.path()).unwrap());
        }
        assert!(
            f.store
                .apply(
                    &s.token,
                    ProviderMutation::Models {
                        provider_id: "fixture".into(),
                        models: vec!["a".into()],
                        selected_models: vec!["b".into()],
                        auto_include: false
                    }
                )
                .is_err()
        );
        assert_eq!(before, fs::read(f.store.path()).unwrap());
    }
    #[test]
    fn paused_and_removed_providers_do_not_serve_or_lose_history_identity() {
        let f = Fixture::new();
        let s = f.add();
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::Edit {
                    id: "fixture".into(),
                    name: None,
                    base_url: None,
                    protocol: None,
                    enabled: Some(false),
                    allow_insecure_http: None,
                },
            )
            .unwrap();
        assert!(s.providers[0].enabled_models().is_empty());
        assert_eq!(s.providers[0].active_keys().count(), 0);
        let s = f
            .store
            .apply(
                &s.token,
                ProviderMutation::Remove {
                    id: "fixture".into(),
                },
            )
            .unwrap();
        assert!(s.provider("fixture").is_none());
        assert_eq!(s.providers[0].id, "fixture");
        assert!(
            s.providers[0]
                .keys
                .iter()
                .all(|k| k.deleted && k.env.is_empty())
        );
        assert_eq!(
            f.store
                .apply(&s.token, ProviderMutation::Add(provider()))
                .unwrap_err()
                .code(),
            "provider_id_exists"
        );
    }
    #[test]
    fn managed_model_and_provider_id_helpers_preserve_existing_rules() {
        for model in [
            "model@2026",
            "vendor/model@2026",
            "model:latest",
            "a-b_c.d",
            "a",
        ] {
            assert!(is_valid_model_id(model), "{model}");
        }
        assert!(is_valid_model_id(&"a".repeat(256)));
        for model in [
            "",
            "@model",
            "a//b",
            "a/../b",
            "a/./b",
            "a/",
            "model?key=x",
            "模型",
        ] {
            assert!(!is_valid_model_id(model), "{model}");
        }
        assert!(!is_valid_model_id(&"a".repeat(257)));
        for provider in ["fixture.alpha", "a-b_c", "a"] {
            assert!(is_valid_provider_id(provider));
        }
        for provider in ["", "group", "a/b", "a@b", ".a"] {
            assert!(!is_valid_provider_id(provider));
        }
        let f = Fixture::new();
        let mut p = provider();
        p.models = vec!["model@2026".into(), "vendor/model:latest".into()];
        p.selected_models = p.models.clone();
        let empty = f.store.load().unwrap();
        let stored = f
            .store
            .apply(&empty.token, ProviderMutation::Add(p))
            .unwrap();
        assert_eq!(f.store.load().unwrap(), stored);
        assert_eq!(
            stored.providers[0].enabled_models(),
            vec!["model@2026", "vendor/model:latest"]
        );
    }
    #[test]
    fn store_identity_is_lexical_local_and_stable_across_atomic_saves() {
        let f = Fixture::new();
        let identity = f.store.identity().unwrap();
        assert_eq!(fs::read_dir(&f.directory).unwrap().count(), 0);
        assert!(identity.starts_with("provider-store-v1:"));
        assert_eq!(identity.len(), "provider-store-v1:".len() + 64);
        assert!(!identity.contains(&f.directory.to_string_lossy().to_string()));
        let same = ProviderStore::new(f.directory.join(".").join("providers.toml"));
        assert_eq!(identity, same.identity().unwrap());
        fs::create_dir(f.directory.join("alias-step")).unwrap();
        let same = ProviderStore::new(f.directory.join("alias-step/../providers.toml"));
        assert_eq!(identity, same.identity().unwrap());
        let other = ProviderStore::new(f.directory.join("other.toml"));
        assert_ne!(identity, other.identity().unwrap());
        let first = f.add();
        assert_eq!(identity, f.store.identity().unwrap());
        f.store
            .apply(
                &first.token,
                ProviderMutation::Remove {
                    id: "fixture".into(),
                },
            )
            .unwrap();
        assert_eq!(identity, f.store.identity().unwrap());
        let absent = f.directory.join("parent-not-created");
        ProviderStore::new(absent.join("providers.toml"))
            .identity()
            .unwrap();
        assert!(!absent.exists());
        assert_eq!(
            ProviderStore::new("relative/providers.toml")
                .identity()
                .unwrap_err()
                .code(),
            "provider_store_absolute_path_required"
        );
    }
    #[test]
    fn only_safe_explicit_urls_pass_syntax_gate() {
        for valid in [
            "https://api.example.invalid/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:1234/v1",
            "https://api.example.invalid/a/b/",
        ] {
            assert!(validate_base_url(valid, false).is_ok(), "{valid}");
        }
        for invalid in [
            "file:///tmp/a",
            "https://user:secret@example.invalid",
            "https://example.invalid?key=secret",
            "https://example.invalid/#a",
            "http://localhost:1234/v1",
            "https://example.invalid/a/../b",
            "https://example.invalid/%2e%2e/",
            "https://example.invalid:0/v1",
            "https://example.invalid\n/v1",
            "https://example.invalid\\evil",
            "https://127.0.0.1:99999/v1",
            "https://[::1]evil/v1",
        ] {
            assert!(validate_base_url(invalid, false).is_err(), "{invalid}");
        }
        assert!(validate_base_url("http://my-explicit-private-server:8080/v1", true).is_ok());
    }
    #[test]
    fn recovery_rolls_back_uncommitted_transaction_without_losing_state() {
        let f = Fixture::new();
        let s = f.add();
        let original = fs::read(f.store.path()).unwrap();
        let (_, new) = pending(&f, &s);
        write_new(&f.store.sidecar("next"), &new).unwrap();
        write_new(&f.store.sidecar("backup"), &original).unwrap();
        assert_eq!(
            f.store.load().unwrap_err().code(),
            "provider_recovery_required"
        );
        assert_eq!(f.store.recover().unwrap(), s);
        assert_eq!(fs::read(f.store.path()).unwrap(), original);
        assert!(!f.store.has_pending().unwrap());
    }
    #[test]
    fn recovery_completes_cleanup_after_committed_rename() {
        let f = Fixture::new();
        let s = f.add();
        let original = fs::read(f.store.path()).unwrap();
        let (t, new) = pending(&f, &s);
        write_new(&f.store.sidecar("next"), &new).unwrap();
        write_new(&f.store.sidecar("backup"), &original).unwrap();
        fs::rename(f.store.sidecar("next"), f.store.path()).unwrap();
        let recovered = f.store.recover().unwrap();
        assert_eq!(recovered.token, t.after);
        assert_eq!(recovered.providers[0].name, "Edited");
        assert!(!f.store.has_pending().unwrap());
    }
    #[test]
    fn journal_only_crash_rolls_back_and_unknown_artifact_is_preserved() {
        let f = Fixture::new();
        let s = f.add();
        pending(&f, &s);
        assert_eq!(f.store.recover().unwrap(), s);
        write_new(&f.store.sidecar("next"), b"unknown recovery material").unwrap();
        assert_eq!(
            f.store.recover().unwrap_err().code(),
            "provider_unknown_recovery_material"
        );
        assert_eq!(
            fs::read(f.store.sidecar("next")).unwrap(),
            b"unknown recovery material"
        );
    }
    #[test]
    fn conflicting_main_and_backup_are_never_guessed_or_overwritten() {
        let f = Fixture::new();
        let s = f.add();
        let (_, new) = pending(&f, &s);
        write_new(&f.store.sidecar("next"), &new).unwrap();
        let mut altered = fs::read(f.store.path()).unwrap();
        altered.extend_from_slice(b"\n# external change\n");
        fs::write(f.store.path(), &altered).unwrap();
        assert_eq!(
            f.store.recover().unwrap_err().code(),
            "provider_recovery_conflict"
        );
        assert_eq!(fs::read(f.store.path()).unwrap(), altered);
        assert!(f.store.sidecar("transaction").exists());
    }
    #[test]
    fn live_or_unknown_lock_is_not_deleted_by_recovery() {
        let f = Fixture::new();
        let s = f.add();
        let lock = f.store.acquire_lock(false).unwrap();
        assert_eq!(f.store.recover().unwrap_err().code(), "provider_busy");
        assert_eq!(f.store.load().unwrap_err().code(), "provider_busy");
        drop(lock);
        assert_eq!(f.store.load().unwrap(), s);
        assert!(f.store.sidecar("lock").exists());
        assert!(f.store.sidecar("lock.v2").exists());
        let unknown = Fixture::new();
        write_new(&unknown.store.sidecar("lock"), b"unknown owner material").unwrap();
        assert_eq!(
            unknown.store.recover().unwrap_err().code(),
            "provider_unknown_lock"
        );
        assert_eq!(
            fs::read(unknown.store.sidecar("lock")).unwrap(),
            b"unknown owner material"
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_dead_lock_requires_offline_review_and_is_never_removed() {
        let f = Fixture::new();
        let s = f.add();
        // Construct the material an older v1 client could have left. Only the
        // fixture setup removes its own idle v2 entries; product code never does.
        fs::remove_file(f.store.sidecar("lock")).unwrap();
        fs::remove_file(f.store.sidecar("lock.v2")).unwrap();
        let legacy = b"magpie-provider-lock-v1\n4294967295\n1\n";
        write_new(&f.store.sidecar("lock"), legacy).unwrap();
        let before = fs::read(f.store.path()).unwrap();
        assert_eq!(
            f.store
                .apply(
                    &s.token,
                    ProviderMutation::Remove {
                        id: "fixture".into()
                    }
                )
                .unwrap_err()
                .code(),
            "provider_unknown_lock"
        );
        assert_eq!(
            f.store.recover().unwrap_err().code(),
            "provider_unknown_lock"
        );
        assert_eq!(fs::read(f.store.sidecar("lock")).unwrap(), legacy);
        assert_eq!(fs::read(f.store.path()).unwrap(), before);
    }
    #[test]
    fn concurrent_writers_cannot_both_commit_same_snapshot() {
        let f = Fixture::new();
        let s = f.add();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for name in ["A", "B"] {
            let store = f.store.clone();
            let token = s.token.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store.apply(
                    &token,
                    ProviderMutation::Edit {
                        id: "fixture".into(),
                        name: Some(name.into()),
                        base_url: None,
                        protocol: None,
                        enabled: None,
                        allow_insecure_http: None,
                    },
                )
            }));
        }
        barrier.wait();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(f.store.load().unwrap().revision, 2);
    }
    #[cfg(unix)]
    #[test]
    fn private_file_permissions_and_symlink_rejection_are_enforced() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let f = Fixture::new();
        f.add();
        assert_eq!(
            fs::metadata(f.store.path()).unwrap().permissions().mode() & 0o077,
            0
        );
        fs::set_permissions(f.store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            f.store.load().unwrap_err().code(),
            "provider_insecure_permissions"
        );
        let link = f.directory.join("alias");
        symlink(f.store.path(), &link).unwrap();
        assert_eq!(
            ProviderStore::new(link).load().unwrap_err().code(),
            "provider_symlink_rejected"
        );
    }
    #[test]
    fn bounded_store_and_collection_limits_reject_without_mutation() {
        let f = Fixture::new();
        let s = f.add();
        let mut p = provider();
        p.id = "second".into();
        p.models = (0..513).map(|i| format!("m{i}")).collect();
        assert_eq!(
            f.store
                .apply(&s.token, ProviderMutation::Add(p))
                .unwrap_err()
                .code(),
            "provider_models_limit"
        );
        assert_eq!(f.store.load().unwrap(), s);
        let large = f.directory.join("large");
        write_new(&large, &vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(
            ProviderStore::new(large).load().unwrap_err().code(),
            "provider_store_too_large"
        );
    }
    #[test]
    fn secret_debug_and_errors_never_echo_sensitive_value() {
        let secret = Secret("synthetic-credential-value".into());
        assert_eq!(secret.expose(), "synthetic-credential-value");
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        let key = key(
            "unavailable",
            "MAGPIE_TEST_KEY_THAT_IS_NEVER_CONFIGURED_7891",
        );
        let e = resolve_credential(&key).unwrap_err();
        assert_eq!(e.code(), "provider_credential_unavailable");
        assert!(!format!("{e:?}").contains(&key.env));
    }
    #[test]
    fn sha256_content_tokens_match_known_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    pub(super) mod os_lock_tests {
        use super::*;
        use std::cell::RefCell;
        use std::io::{BufRead, BufReader};
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Child, Command, Output, Stdio};
        use std::sync::mpsc;
        use std::thread::JoinHandle;
        use std::time::{Duration, Instant};

        thread_local! {
            static FAULT: RefCell<Option<String>> = const { RefCell::new(None) };
            static RETAINED_DESCRIPTION: RefCell<Option<Option<File>>> = const { RefCell::new(None) };
        }
        pub(crate) fn capture_inherited_if_armed(file: &File) {
            RETAINED_DESCRIPTION.with(|retained| {
                let mut retained = retained.borrow_mut();
                if let Some(slot @ None) = retained.as_mut() {
                    *slot = Some(file.try_clone().expect("duplicate locked description"));
                }
            });
        }
        // Only the explicitly invoked ignored helper initializes this state.
        // Concurrent parent tests never modify the process environment.
        pub(crate) fn checkpoint(stage: &str) {
            let pause = FAULT.with(|fault| {
                let mut fault = fault.borrow_mut();
                if fault.as_ref().is_some_and(|wanted| wanted == stage) {
                    fault.take();
                    true
                } else {
                    false
                }
            });
            if pause {
                println!("\nPROVIDER_CHILD_READY {stage}");
                std::io::stdout().flush().unwrap();
                let mut proceed = [0];
                std::io::stdin().read_exact(&mut proceed).unwrap();
                assert_eq!(proceed[0], b'c');
            }
        }
        fn edit(name: &str) -> ProviderMutation {
            ProviderMutation::Edit {
                id: "fixture".into(),
                name: Some(name.into()),
                base_url: None,
                protocol: None,
                enabled: None,
                allow_insecure_http: None,
            }
        }
        #[test]
        #[ignore = "subprocess-only checkpoint helper"]
        fn child_helper() {
            let Ok(mode) = std::env::var("MAGPIE_PROVIDER_TEST_MODE") else {
                return;
            };
            let path = PathBuf::from(std::env::var_os("MAGPIE_PROVIDER_TEST_STORE").unwrap());
            let expected = std::env::var("MAGPIE_PROVIDER_TEST_TOKEN").unwrap_or_default();
            let name =
                std::env::var("MAGPIE_PROVIDER_TEST_NAME").unwrap_or_else(|_| "Edited".into());
            let target = std::env::var("MAGPIE_PROVIDER_TEST_STAGE").unwrap();
            FAULT.with(|fault| *fault.borrow_mut() = Some(target));
            let store = ProviderStore::new(path);
            checkpoint("child.start");
            let result = match mode.as_str() {
                "write" => store.apply(&expected, edit(&name)).map(|state| state.token),
                "recover" => store.recover().map(|state| state.token),
                "read" => store.load().map(|state| state.token),
                "hold" | "initialize" => {
                    store.prepare_parent().unwrap();
                    let lock = store.acquire_lock(false);
                    match lock {
                        Ok(_lock) => {
                            checkpoint("child.held");
                            Ok("held".into())
                        }
                        Err(error) => Err(error),
                    }
                }
                _ => panic!("unknown test child mode"),
            };
            match result {
                Ok(token) => println!("PROVIDER_CHILD_RESULT ok {token}"),
                Err(error) => println!("PROVIDER_CHILD_RESULT {}", error.code()),
            }
        }

        struct PausedChild {
            child: Option<Child>,
            output_reader: Option<JoinHandle<Vec<u8>>>,
            checkpoint: String,
        }
        impl PausedChild {
            fn spawn(f: &Fixture, mode: &str, token: &str, stage: &str, name: &str) -> Self {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "providers::tests::os_lock_tests::child_helper",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("MAGPIE_PROVIDER_TEST_MODE", mode)
                    .env("MAGPIE_PROVIDER_TEST_STORE", f.store.path())
                    .env("MAGPIE_PROVIDER_TEST_TOKEN", token)
                    .env("MAGPIE_PROVIDER_TEST_STAGE", stage)
                    .env("MAGPIE_PROVIDER_TEST_NAME", name)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let stdout = child.stdout.take().unwrap();
                let (ready_tx, ready_rx) = mpsc::channel();
                let reader = std::thread::spawn(move || {
                    let mut captured = Vec::new();
                    for line in BufReader::new(stdout).lines() {
                        let line = line.expect("child stdout");
                        if let Some(stage) = line.strip_prefix("PROVIDER_CHILD_READY ") {
                            let _ = ready_tx.send(stage.to_owned());
                        }
                        assert!(
                            captured.len() + line.len() < 65536,
                            "bounded test child output"
                        );
                        captured.extend_from_slice(line.as_bytes());
                        captured.push(b'\n');
                    }
                    captured
                });
                match ready_rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(seen) => assert_eq!(seen, stage),
                    Err(error) => {
                        let _ = child.kill();
                        let output = child.wait_with_output().unwrap();
                        let stdout = reader.join().unwrap();
                        panic!(
                            "checkpoint {stage}: {error}; {} {}",
                            String::from_utf8_lossy(&stdout),
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                }
                Self {
                    child: Some(child),
                    output_reader: Some(reader),
                    checkpoint: format!("{mode}:{stage}"),
                }
            }
            fn resume(&mut self) {
                self.child
                    .as_mut()
                    .unwrap()
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"c")
                    .unwrap();
            }
            fn output(mut self) -> Output {
                let mut child = self.child.take().unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if child.try_wait().unwrap().is_some() {
                        let mut output = child.wait_with_output().unwrap();
                        output.stdout = self.output_reader.take().unwrap().join().unwrap();
                        return output;
                    }
                    if Instant::now() >= deadline {
                        child.kill().unwrap();
                        let output = child.wait_with_output().unwrap();
                        panic!(
                            "child completion timeout: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            fn kill(mut self) {
                let mut child = self.child.take().unwrap();
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                assert_eq!(output.status.signal(), Some(9), "actual SIGKILL required");
                self.output_reader.take().unwrap().join().unwrap();
                println!("PROVIDER_SIGKILL {} signal=9", self.checkpoint);
            }
        }
        impl Drop for PausedChild {
            fn drop(&mut self) {
                if let Some(mut child) = self.child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if let Some(reader) = self.output_reader.take() {
                    let _ = reader.join();
                }
            }
        }
        fn identity(path: &Path) -> (u64, u64) {
            let metadata = fs::symlink_metadata(path).unwrap();
            (metadata.dev(), metadata.ino())
        }
        fn stable_entries(f: &Fixture, expected: (u64, u64)) {
            for suffix in ["lock", "lock.v2"] {
                assert_eq!(identity(&f.store.sidecar(suffix)), expected);
                assert_eq!(fs::read(f.store.sidecar(suffix)).unwrap(), v2_lock::MARKER);
            }
        }
        fn stages(f: &Fixture) -> Vec<(PathBuf, (u64, u64), Vec<u8>)> {
            let mut result: Vec<_> = fs::read_dir(&f.directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(".stage-v2-")
                })
                .map(|path| (path.clone(), identity(&path), fs::read(path).unwrap()))
                .collect();
            result.sort_by(|a, b| a.0.cmp(&b.0));
            result
        }
        fn assert_stages_preserved(before: &[(PathBuf, (u64, u64), Vec<u8>)]) {
            for (path, inode, bytes) in before {
                assert_eq!(&identity(path), inode);
                assert_eq!(&fs::read(path).unwrap(), bytes);
            }
        }
        fn assert_result(output: &Output) -> &str {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::str::from_utf8(&output.stdout)
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("PROVIDER_CHILD_RESULT "))
                .expect("child result")
        }

        #[test]
        fn safe_reader_rejects_observed_entry_permission_and_content_races() {
            let cases = [
                ("read.checked_path", "replace", "provider_read_conflict"),
                (
                    "read.checked_path",
                    "symlink",
                    "provider_regular_file_required",
                ),
                (
                    "read.checked_path",
                    "permissions",
                    "provider_insecure_permissions",
                ),
                ("read.checked_path", "missing", "provider_read_conflict"),
                (
                    "read.checked_path",
                    "fifo",
                    "provider_regular_file_required",
                ),
                ("read.opened", "replace", "provider_read_conflict"),
                ("read.opened", "symlink", "provider_regular_file_required"),
                (
                    "read.opened",
                    "permissions",
                    "provider_insecure_permissions",
                ),
                ("read.opened", "missing", "provider_read_conflict"),
                ("read.before_bytes", "in-place", "provider_read_conflict"),
                ("read.after_bytes", "replace", "provider_read_conflict"),
                (
                    "read.after_bytes",
                    "permissions",
                    "provider_insecure_permissions",
                ),
                ("read.after_bytes", "missing", "provider_read_conflict"),
                ("read.after_bytes", "in-place", "provider_read_conflict"),
            ];
            for (stage, change, expected) in cases {
                let f = Fixture::new();
                f.add();
                let original = fs::read(f.store.path()).unwrap();
                let original_inode = identity(f.store.path());
                let mut child = PausedChild::spawn(&f, "read", "", stage, "");
                let retained = f.directory.join("retained-original");
                let victim = f.directory.join("victim");
                match change {
                    "replace" | "symlink" | "missing" | "fifo" => {
                        fs::rename(f.store.path(), &retained).unwrap();
                        match change {
                            "replace" => {
                                write_new(f.store.path(), b"outside-owned-replacement").unwrap()
                            }
                            "symlink" => {
                                write_new(&victim, b"never-return-victim-content").unwrap();
                                symlink(&victim, f.store.path()).unwrap();
                            }
                            "fifo" => rustix::fs::mkfifoat(
                                rustix::fs::CWD,
                                f.store.path(),
                                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                            )
                            .unwrap(),
                            _ => {}
                        }
                    }
                    "permissions" => {
                        fs::set_permissions(f.store.path(), fs::Permissions::from_mode(0o644))
                            .unwrap()
                    }
                    "in-place" => fs::write(f.store.path(), b"external-in-place-content").unwrap(),
                    _ => unreachable!(),
                }
                let current_inode = fs::symlink_metadata(f.store.path())
                    .ok()
                    .map(|m| (m.dev(), m.ino()));
                child.resume();
                let output = child.output();
                assert_eq!(assert_result(&output), expected, "{stage}:{change}");
                assert!(
                    !String::from_utf8_lossy(&output.stdout)
                        .contains("never-return-victim-content")
                );
                assert_eq!(
                    fs::symlink_metadata(f.store.path())
                        .ok()
                        .map(|m| (m.dev(), m.ino())),
                    current_inode,
                    "reader never replaces external materials"
                );
                if retained.exists() {
                    assert_eq!(fs::read(&retained).unwrap(), original);
                    assert_eq!(identity(&retained), original_inode);
                }
                match change {
                    "replace" => assert_eq!(
                        fs::read(f.store.path()).unwrap(),
                        b"outside-owned-replacement"
                    ),
                    "in-place" => assert_eq!(
                        fs::read(f.store.path()).unwrap(),
                        b"external-in-place-content"
                    ),
                    "permissions" => {
                        assert_eq!(fs::read(f.store.path()).unwrap(), original);
                        assert_eq!(
                            fs::metadata(f.store.path()).unwrap().permissions().mode() & 0o777,
                            0o644
                        );
                    }
                    "symlink" => {
                        assert!(
                            fs::symlink_metadata(f.store.path())
                                .unwrap()
                                .file_type()
                                .is_symlink()
                        );
                        assert_eq!(fs::read(&victim).unwrap(), b"never-return-victim-content");
                    }
                    "fifo" => assert!(!fs::symlink_metadata(f.store.path()).unwrap().is_file()),
                    "missing" => assert!(!f.store.path().exists()),
                    _ => unreachable!(),
                }
                println!("PROVIDER_READ_RACE {stage}:{change} rejected={expected}");
            }
        }
        #[test]
        fn stable_absence_is_side_effect_free_but_observed_appearance_is_a_conflict() {
            let f = Fixture::new();
            let empty = f.store.load().unwrap();
            assert_eq!(fs::read_dir(&f.directory).unwrap().count(), 0);
            let mut child = PausedChild::spawn(&f, "read", "", "read.absent", "");
            write_new(f.store.path(), b"outside-new-entry").unwrap();
            let inode = identity(f.store.path());
            child.resume();
            let output = child.output();
            assert_eq!(assert_result(&output), "provider_read_conflict");
            assert_eq!(fs::read(f.store.path()).unwrap(), b"outside-new-entry");
            assert_eq!(identity(f.store.path()), inode);
            assert!(!f.store.sidecar("lock").exists());
            assert_eq!(empty.revision, 0);
            println!("PROVIDER_READ_RACE read.absent:appeared rejected=provider_read_conflict");
        }
        #[test]
        fn private_read_descriptor_flags_and_identity_guard_are_effective() {
            let f = Fixture::new();
            let saved = f.add();
            let file = open_read_file(f.store.path()).unwrap();
            assert!(
                rustix::io::fcntl_getfd(&file)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            assert!(
                rustix::fs::fcntl_getfl(&file)
                    .unwrap()
                    .contains(rustix::fs::OFlags::NONBLOCK)
            );
            assert_eq!(
                read_optional(f.store.path(), MAX_BYTES, true)
                    .unwrap()
                    .unwrap(),
                fs::read(f.store.path()).unwrap()
            );
            assert_eq!(f.store.load().unwrap(), saved);
            let link = f.directory.join("link");
            symlink(f.store.path(), &link).unwrap();
            assert_eq!(
                open_read_file(&link).unwrap_err().code(),
                "provider_regular_file_required"
            );
            assert_eq!(
                ProviderStore::new(link).identity().unwrap_err().code(),
                "provider_symlink_rejected"
            );
        }
        #[test]
        fn real_process_lock_is_released_by_sigkill_without_unlink() {
            let f = Fixture::new();
            let before = f.add();
            let inode = identity(&f.store.sidecar("lock"));
            let child = PausedChild::spawn(&f, "hold", "", "child.held", "");
            assert_eq!(f.store.load().unwrap_err().code(), "provider_busy");
            assert_eq!(
                f.store
                    .apply(&before.token, edit("blocked"))
                    .unwrap_err()
                    .code(),
                "provider_busy"
            );
            assert_eq!(f.store.recover().unwrap_err().code(), "provider_busy");
            stable_entries(&f, inode);
            child.kill();
            assert_eq!(f.store.load().unwrap(), before);
            let after = f.store.apply(&before.token, edit("after-kill")).unwrap();
            assert_eq!(after.revision, before.revision + 1);
            stable_entries(&f, inode);
        }
        #[test]
        fn normal_release_unlocks_even_if_an_inherited_description_is_still_open() {
            let f = Fixture::new();
            let before = f.add();
            let holder = f.store.acquire_lock(false).unwrap();
            // dup and fork share an open-file-description. Keeping a duplicate
            // models the pre-exec window deterministically; production never
            // clones or passes this fd to a worker.
            let inherited_description = holder._file.try_clone().unwrap();
            assert_eq!(
                f.store
                    .apply(&before.token, edit("blocked"))
                    .unwrap_err()
                    .code(),
                "provider_busy"
            );
            drop(holder);
            let after = f.store.apply(&before.token, edit("released")).unwrap();
            assert_eq!(after.revision, before.revision + 1);
            drop(inherited_description);
        }
        #[test]
        fn invalid_lock_entries_release_the_description_on_error() {
            let f = Fixture::new();
            let legacy = f.store.sidecar("lock");
            let current = f.store.sidecar("lock.v2");
            write_new(&legacy, v2_lock::MARKER).unwrap();
            write_new(&current, v2_lock::MARKER).unwrap();
            let before = [identity(&legacy), identity(&current)];
            RETAINED_DESCRIPTION.with(|retained| *retained.borrow_mut() = Some(None));
            assert_eq!(f.store.load().unwrap_err().code(), "provider_lock_mismatch");
            assert_eq!(
                f.store.recover().unwrap_err().code(),
                "provider_lock_mismatch",
                "an inherited descriptor must not leave the error path locked"
            );
            RETAINED_DESCRIPTION.with(|retained| {
                retained.borrow_mut().take();
            });
            assert_eq!([identity(&legacy), identity(&current)], before);
            assert_eq!(fs::read(&legacy).unwrap(), v2_lock::MARKER);
            assert_eq!(fs::read(&current).unwrap(), v2_lock::MARKER);
            assert!(!f.store.path().exists());
        }
        #[test]
        fn same_token_two_real_writers_commit_exactly_once() {
            let f = Fixture::new();
            let before = f.add();
            let inode = identity(&f.store.sidecar("lock"));
            let mut a = PausedChild::spawn(&f, "write", &before.token, "child.start", "A");
            let mut b = PausedChild::spawn(&f, "write", &before.token, "child.start", "B");
            a.resume();
            b.resume();
            let a = a.output();
            let b = b.output();
            let results = [assert_result(&a), assert_result(&b)];
            assert_eq!(
                results
                    .iter()
                    .filter(|result| result.starts_with("ok "))
                    .count(),
                1
            );
            assert!(results.iter().all(|result| result.starts_with("ok ")
                || *result == "provider_busy"
                || *result == "provider_revision_conflict"));
            let after = f.store.load().unwrap();
            assert_eq!(after.revision, before.revision + 1);
            assert!(matches!(after.providers[0].name.as_str(), "A" | "B"));
            stable_entries(&f, inode);
        }
        #[test]
        fn initialization_sigkill_at_every_publication_boundary_can_continue() {
            for stage in [
                "lock.marker.created",
                "lock.marker.partial",
                "lock.marker.synced",
                "lock.legacy_published",
                "lock.v2_published",
            ] {
                let f = Fixture::new();
                let empty = f.store.load().unwrap();
                let child = PausedChild::spawn(&f, "initialize", "", stage, "");
                let leftovers = stages(&f);
                child.kill();
                assert_eq!(f.store.load().unwrap(), empty, "{stage}");
                let added = f
                    .store
                    .apply(&empty.token, ProviderMutation::Add(provider()))
                    .unwrap();
                assert_eq!(added.revision, 1, "{stage}");
                stable_entries(&f, identity(&f.store.sidecar("lock")));
                assert_stages_preserved(&leftovers);
                assert!(!f.store.has_pending().unwrap());
            }
        }
        #[test]
        fn transaction_sigkill_at_every_stage_recovers_before_or_after_and_accepts_next_cas() {
            for stage in [
                "lock.acquired",
                "transaction.created",
                "transaction.partial",
                "transaction.synced",
                "transaction.published",
                "next.created",
                "next.partial",
                "next.synced",
                "next.published",
                "backup.created",
                "backup.partial",
                "backup.synced",
                "backup.published",
                "main.renamed",
                "main.synced",
                "cleanup.next_removed",
                "cleanup.backup_removed",
                "cleanup.transaction_removed",
            ] {
                let f = Fixture::new();
                let before = f.add();
                let inode = identity(&f.store.sidecar("lock"));
                let child = PausedChild::spawn(&f, "write", &before.token, stage, "Edited");
                let leftovers = stages(&f);
                child.kill();
                let recovered = f
                    .store
                    .recover()
                    .unwrap_or_else(|error| panic!("{stage}: {error}"));
                let committed = stage.starts_with("main.") || stage.starts_with("cleanup.");
                if committed {
                    assert_eq!(recovered.revision, before.revision + 1, "{stage}");
                    assert_eq!(recovered.providers[0].name, "Edited", "{stage}");
                } else {
                    assert_eq!(recovered, before, "{stage}");
                }
                assert!(!f.store.has_pending().unwrap(), "{stage}");
                assert_stages_preserved(&leftovers);
                let next = f.store.apply(&recovered.token, edit("next-cas")).unwrap();
                assert_eq!(next.revision, recovered.revision + 1, "{stage}");
                stable_entries(&f, inode);
            }
        }
        #[test]
        fn recovery_sigkill_never_leaves_a_guard_or_unlinks_the_lock() {
            for stage in [
                "recovery.acquired",
                "cleanup.next_removed",
                "cleanup.backup_removed",
                "cleanup.transaction_removed",
            ] {
                let f = Fixture::new();
                let before = f.add();
                let original = fs::read(f.store.path()).unwrap();
                let (_, next) = pending(&f, &before);
                write_new(&f.store.sidecar("next"), &next).unwrap();
                write_new(&f.store.sidecar("backup"), &original).unwrap();
                let inode = identity(&f.store.sidecar("lock"));
                let child = PausedChild::spawn(&f, "recover", "", stage, "");
                assert_eq!(f.store.recover().unwrap_err().code(), "provider_busy");
                assert_eq!(
                    f.store
                        .apply(&before.token, edit("blocked"))
                        .unwrap_err()
                        .code(),
                    "provider_busy"
                );
                child.kill();
                assert_eq!(f.store.recover().unwrap(), before, "{stage}");
                assert!(!f.store.sidecar("recovery-guard").exists());
                stable_entries(&f, inode);
                f.store
                    .apply(&before.token, edit("after-recovery-kill"))
                    .unwrap();
            }
        }
        #[test]
        fn a_single_exact_v2_entry_is_read_without_repair_then_repaired_by_writer() {
            for suffix in ["lock", "lock.v2"] {
                let f = Fixture::new();
                write_new(&f.store.sidecar(suffix), v2_lock::MARKER).unwrap();
                let inode = identity(&f.store.sidecar(suffix));
                let missing = if suffix == "lock" { "lock.v2" } else { "lock" };
                let empty = f.store.load().unwrap();
                assert!(!f.store.sidecar(missing).exists());
                f.store
                    .apply(&empty.token, ProviderMutation::Add(provider()))
                    .unwrap();
                stable_entries(&f, inode);
            }
        }
        #[test]
        fn shared_readers_coexist_and_block_writers_and_fds_are_cloexec() {
            let f = Fixture::new();
            let before = f.add();
            let a = f.store.acquire_read_lock().unwrap().unwrap();
            let b = f.store.acquire_read_lock().unwrap().unwrap();
            assert_eq!(f.store.load().unwrap(), before);
            assert!(
                rustix::io::fcntl_getfd(&a._file)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            assert_eq!(
                f.store
                    .apply(&before.token, edit("blocked"))
                    .unwrap_err()
                    .code(),
                "provider_busy"
            );
            drop(a);
            drop(b);
            f.store.apply(&before.token, edit("unblocked")).unwrap();
            let lock = f.store.acquire_lock(false).unwrap();
            assert!(
                rustix::io::fcntl_getfd(&lock._file)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
        }
        #[test]
        fn observed_stage_replacement_is_preserved_and_never_committed() {
            for boundary in ["next.synced", "next.published"] {
                let f = Fixture::new();
                let before = f.add();
                let original = fs::read(f.store.path()).unwrap();
                let mut child = PausedChild::spawn(&f, "write", &before.token, boundary, "Edited");
                let unknown = b"outside-owned-stage-material";
                let changed = if boundary == "next.synced" {
                    let stage = stages(&f)
                        .into_iter()
                        .find(|(path, _, _)| {
                            path.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .contains(".next.stage-v2-")
                        })
                        .unwrap()
                        .0;
                    fs::rename(&stage, f.directory.join("retained-stage-original")).unwrap();
                    write_new(&stage, unknown).unwrap();
                    stage
                } else {
                    let next = f.store.sidecar("next");
                    fs::write(&next, unknown).unwrap();
                    next
                };
                let inode = identity(&changed);
                child.resume();
                let output = child.output();
                assert_eq!(assert_result(&output), "provider_unknown_recovery_material");
                assert_eq!(fs::read(f.store.path()).unwrap(), original);
                assert_eq!(fs::read(&changed).unwrap(), unknown);
                assert_eq!(identity(&changed), inode);
                assert!(f.store.recover().is_err());
                assert_eq!(fs::read(&changed).unwrap(), unknown);
                assert!(f.store.sidecar("transaction").exists());
            }
        }
        #[test]
        fn unknown_v1_mismatched_and_symlink_lock_material_is_unchanged() {
            for kind in [
                "unknown",
                "legacy-dead",
                "independent-v2",
                "symlink",
                "guard",
                "directory",
            ] {
                let f = Fixture::new();
                let empty = snapshot_from_bytes(None).unwrap();
                let a = f.store.sidecar("lock");
                let b = f.store.sidecar("lock.v2");
                match kind {
                    "unknown" => write_new(&a, b"outside-owned-lock").unwrap(),
                    "legacy-dead" => {
                        write_new(&a, b"magpie-provider-lock-v1\n4294967295\n1\n").unwrap()
                    }
                    "independent-v2" => {
                        write_new(&a, v2_lock::MARKER).unwrap();
                        write_new(&b, v2_lock::MARKER).unwrap();
                    }
                    "symlink" => {
                        write_new(&b, v2_lock::MARKER).unwrap();
                        symlink(&b, &a).unwrap();
                    }
                    "guard" => {
                        write_new(&a, v2_lock::MARKER).unwrap();
                        fs::hard_link(&a, &b).unwrap();
                        write_new(&f.store.sidecar("recovery-guard"), b"retained-v1-guard")
                            .unwrap();
                    }
                    "directory" => fs::create_dir(&a).unwrap(),
                    _ => unreachable!(),
                }
                let paths: Vec<_> = fs::read_dir(&f.directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect();
                let preserved: Vec<_> = paths
                    .iter()
                    .map(|path| {
                        let metadata = fs::symlink_metadata(path).unwrap();
                        (
                            path,
                            identity(path),
                            if metadata.is_file() {
                                Some(fs::read(path).unwrap())
                            } else {
                                None
                            },
                        )
                    })
                    .collect();
                let expected = match kind {
                    "unknown" | "legacy-dead" => "provider_unknown_lock",
                    "guard" => "provider_recovery_guard_present",
                    "directory" => "provider_unknown_lock",
                    _ => "provider_lock_mismatch",
                };
                assert_eq!(f.store.load().unwrap_err().code(), expected, "{kind}");
                assert_eq!(f.store.recover().unwrap_err().code(), expected, "{kind}");
                assert_eq!(
                    f.store
                        .apply(&empty.token, ProviderMutation::Add(provider()))
                        .unwrap_err()
                        .code(),
                    expected,
                    "{kind}"
                );
                for (path, inode, bytes) in preserved {
                    assert_eq!(identity(path), inode, "{kind}");
                    if let Some(bytes) = bytes {
                        assert_eq!(fs::read(path).unwrap(), bytes, "{kind}");
                    }
                }
                assert!(!f.store.path().exists());
            }
        }
    }
}

#[cfg(test)]
mod recovery_guard_regression {
    use super::*;
    #[test]
    fn existing_recovery_guard_preserves_other_writers_lock_and_state() {
        let root = std::env::temp_dir().join(format!(
            "magpie-provider-recovery-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let store = ProviderStore::new(root.join("providers.toml"));
        write_new(
            &store.sidecar("lock"),
            b"magpie-provider-lock-v1\n4294967295\n1\n",
        )
        .unwrap();
        write_new(
            &store.sidecar("recovery-guard"),
            b"magpie-provider-lock-v1\n4294967294\n1\n",
        )
        .unwrap();
        let lock = std::fs::read(store.sidecar("lock")).unwrap();
        let guard = std::fs::read(store.sidecar("recovery-guard")).unwrap();
        assert_eq!(
            store.load().unwrap_err().code(),
            "provider_recovery_guard_present"
        );
        assert_eq!(
            store.recover().unwrap_err().code(),
            "provider_recovery_guard_present"
        );
        assert_eq!(std::fs::read(store.sidecar("lock")).unwrap(), lock);
        assert_eq!(
            std::fs::read(store.sidecar("recovery-guard")).unwrap(),
            guard
        );
        assert!(!store.path().exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
