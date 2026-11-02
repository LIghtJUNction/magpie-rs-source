//! System credentials and coordinated Provider updates.
//!
//! A replacement key receives a fresh reference before the public configuration
//! is committed. A failed compare-and-save cannot overwrite the key used by a
//! concurrent request. Old keys are removed only after the new configuration is
//! durable. An uncertain filesystem commit retains both entries for recovery.

use std::{fmt, io, path::Path};

use zeroize::Zeroizing;

use crate::provider_store::{Provider, ProviderStore, StoreError};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const SERVICE: &str = "magpie-rs.providers";
const MAX_SECRET_BYTES: usize = 8192;

/// Intentionally has no Debug implementation: the replacement is sensitive.
pub enum SecretChange {
    Keep,
    Replace(String),
    Clear,
}

#[derive(Clone, Copy)]
pub enum SaveMode {
    Create,
    Update,
}

pub struct SaveOutcome {
    pub store: ProviderStore,
    /// The configuration was saved, but an unused key could not be removed.
    pub cleanup_pending: bool,
}

#[derive(Debug)]
pub enum CredentialError {
    InvalidSecret,
    InvalidReference,
    InvalidChange,
    Unavailable,
    Missing,
    RandomUnavailable,
    RollbackPending,
    Store(StoreError),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            _ => f.write_str(match self {
                Self::InvalidSecret => {
                    "API key must be 1–8192 visible ASCII characters without spaces"
                }
                Self::InvalidReference => "the stored credential reference is invalid",
                Self::InvalidChange => {
                    "clear the saved API key before selecting an environment reference"
                }
                Self::Unavailable => {
                    "system credential storage is unavailable or locked; unlock it and retry"
                }
                Self::Missing => "the configured API key is missing from system credential storage",
                Self::RandomUnavailable => "a new credential reference could not be created",
                Self::RollbackPending => {
                    "Provider was not saved; system credential cleanup could not be confirmed"
                }
                Self::Store(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for CredentialError {}

impl From<StoreError> for CredentialError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

trait Vault {
    fn put(&self, reference: &str, secret: &str) -> Result<(), CredentialError>;
    fn remove(&self, reference: &str) -> Result<(), CredentialError>;
}

struct SystemVault;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn entry(reference: &str) -> Result<keyring::Entry, CredentialError> {
    validate_reference(reference)?;
    keyring::Entry::new(SERVICE, reference).map_err(|_| CredentialError::Unavailable)
}

impl Vault for SystemVault {
    fn put(&self, reference: &str, secret: &str) -> Result<(), CredentialError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            entry(reference)?
                .set_password(secret)
                .map_err(|_| CredentialError::Unavailable)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = (reference, secret);
            Err(CredentialError::Unavailable)
        }
    }

    fn remove(&self, reference: &str) -> Result<(), CredentialError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            match entry(reference)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(CredentialError::Unavailable),
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = reference;
            Err(CredentialError::Unavailable)
        }
    }
}

fn validate_reference(reference: &str) -> Result<(), CredentialError> {
    let Some((provider, nonce)) = reference
        .strip_prefix("provider/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return Err(CredentialError::InvalidReference);
    };
    if !crate::catalog::valid_provider_id(provider)
        || nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CredentialError::InvalidReference);
    }
    Ok(())
}

fn fresh_reference(id: &str) -> Result<String, CredentialError> {
    use fmt::Write as _;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| CredentialError::RandomUnavailable)?;
    let mut reference = format!("provider/{id}/");
    for byte in random {
        let _ = write!(reference, "{byte:02x}");
    }
    Ok(reference)
}

fn valid_secret(secret: &str) -> bool {
    !secret.is_empty()
        && secret.len() <= MAX_SECRET_BYTES
        && secret.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Reads only the selected Provider's credential. Call off the UI/reactor thread.
/// Missing or locked credentials are errors, never an implicit keyless request.
pub fn read(reference: &str) -> io::Result<String> {
    fn read_inner(reference: &str) -> Result<String, CredentialError> {
        validate_reference(reference)?;
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let mut secret =
                Zeroizing::new(
                    entry(reference)?
                        .get_password()
                        .map_err(|error| match error {
                            keyring::Error::NoEntry => CredentialError::Missing,
                            _ => CredentialError::Unavailable,
                        })?,
                );
            if !valid_secret(&secret) {
                return Err(CredentialError::InvalidSecret);
            }
            Ok(std::mem::take(&mut *secret))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(CredentialError::Unavailable)
    }
    read_inner(reference).map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
}

pub fn save_provider(
    path: &Path,
    provider: Provider,
    expected_revision: u64,
    change: SecretChange,
    mode: SaveMode,
) -> Result<SaveOutcome, CredentialError> {
    save_with(
        &SystemVault,
        path,
        provider,
        expected_revision,
        change,
        mode,
    )
}

fn save_with(
    vault: &impl Vault,
    path: &Path,
    mut provider: Provider,
    expected_revision: u64,
    change: SecretChange,
    mode: SaveMode,
) -> Result<SaveOutcome, CredentialError> {
    // Wrap the secret before any fallible read/validation so all early returns
    // erase our owned replacement buffer as well.
    let (replacement, clear) = match change {
        SecretChange::Keep => (None, false),
        SecretChange::Clear => (None, true),
        SecretChange::Replace(secret) => (Some(Zeroizing::new(secret)), false),
    };
    let previous = ProviderStore::load(path)?;
    if previous.revision() != expected_revision {
        return Err(StoreError::Conflict.into());
    }
    let old = previous.provider(&provider.id);
    match (mode, old.is_some()) {
        (SaveMode::Create, true) => return Err(StoreError::DuplicateProvider.into()),
        (SaveMode::Update, false) => return Err(StoreError::ProviderNotFound.into()),
        _ => {}
    }
    let old_reference = old.and_then(|provider| provider.credential_ref.clone());
    provider.credential_ref = if clear || replacement.is_some() {
        None
    } else {
        old_reference.clone()
    };
    if replacement.is_some() {
        provider.api_key_env = None;
    }
    if provider.credential_ref.is_some() && provider.api_key_env.is_some() {
        return Err(CredentialError::InvalidChange);
    }
    provider.validate()?;
    if replacement
        .as_ref()
        .is_some_and(|secret| !valid_secret(secret))
    {
        return Err(CredentialError::InvalidSecret);
    }
    let new_reference = if let Some(secret) = replacement {
        let reference = fresh_reference(&provider.id)?;
        if let Err(error) = vault.put(&reference, &secret) {
            // A keyring write can succeed before its response is lost. This
            // fresh reference is not yet published, so try to remove it even
            // when the write reports an error; never touch the previous key.
            if vault.remove(&reference).is_err() {
                return Err(CredentialError::RollbackPending);
            }
            return Err(error);
        }
        provider.credential_ref = Some(reference.clone());
        Some(reference)
    } else {
        None
    };
    let saved = match mode {
        SaveMode::Create => ProviderStore::create(path, provider, expected_revision),
        SaveMode::Update => ProviderStore::update(path, provider, expected_revision),
    };
    let store = match saved {
        Ok(store) => store,
        Err(error) => {
            // After rename a directory fsync can fail: deleting the new key in
            // that case could make a successfully committed Provider unusable.
            if error != StoreError::CommitUncertain {
                if let Some(reference) = new_reference {
                    if vault.remove(&reference).is_err() {
                        return Err(CredentialError::RollbackPending);
                    }
                }
            }
            return Err(error.into());
        }
    };
    let cleanup_pending = cleanup_unused(vault, &store, old_reference.as_deref());
    Ok(SaveOutcome {
        store,
        cleanup_pending,
    })
}

pub fn remove_provider(
    path: &Path,
    id: &str,
    expected_revision: u64,
) -> Result<SaveOutcome, CredentialError> {
    remove_with(&SystemVault, path, id, expected_revision)
}

fn remove_with(
    vault: &impl Vault,
    path: &Path,
    id: &str,
    expected_revision: u64,
) -> Result<SaveOutcome, CredentialError> {
    let previous = ProviderStore::load(path)?;
    if previous.revision() != expected_revision {
        return Err(StoreError::Conflict.into());
    }
    let old = previous.provider(id).ok_or(StoreError::ProviderNotFound)?;
    let old_reference = old.credential_ref.clone();
    let store = ProviderStore::remove(path, id, expected_revision)?;
    let cleanup_pending = cleanup_unused(vault, &store, old_reference.as_deref());
    Ok(SaveOutcome {
        store,
        cleanup_pending,
    })
}

fn cleanup_unused(vault: &impl Vault, store: &ProviderStore, reference: Option<&str>) -> bool {
    reference.is_some_and(|reference| {
        !store
            .providers()
            .iter()
            .any(|provider| provider.credential_ref.as_deref() == Some(reference))
            && vault.remove(reference).is_err()
    })
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "android")
))]
mod tests {
    use super::*;
    use crate::provider_store::ProviderProtocol;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "magpie-credentials-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("providers.toml")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Default)]
    struct MemoryVault {
        entries: RefCell<BTreeMap<String, String>>,
        fail_put: Cell<bool>,
        fail_after_put: Cell<bool>,
        fail_remove: Cell<bool>,
        race: RefCell<Option<PathBuf>>,
    }
    impl Vault for MemoryVault {
        fn put(&self, reference: &str, secret: &str) -> Result<(), CredentialError> {
            if self.fail_put.get() {
                return Err(CredentialError::Unavailable);
            }
            self.entries
                .borrow_mut()
                .insert(reference.into(), secret.into());
            if self.fail_after_put.get() {
                return Err(CredentialError::Unavailable);
            }
            if let Some(path) = self.race.borrow_mut().take() {
                let store = ProviderStore::load(&path).unwrap();
                ProviderStore::create(&path, provider("concurrent"), store.revision()).unwrap();
            }
            Ok(())
        }
        fn remove(&self, reference: &str) -> Result<(), CredentialError> {
            if self.fail_remove.get() {
                return Err(CredentialError::Unavailable);
            }
            self.entries.borrow_mut().remove(reference);
            Ok(())
        }
    }
    fn provider(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: "Fixture".into(),
            base_url: "http://127.0.0.1:48001/v1".into(),
            protocol: ProviderProtocol::OpenAiChat,
            api_key_env: None,
            credential_ref: None,
            models: vec!["tiny".into()],
            enabled: true,
        }
    }
    #[test]
    fn key_rotation_and_clear_keep_plaintext_out_of_public_configuration() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        let first = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            0,
            SecretChange::Replace("synthetic-first-key".into()),
            SaveMode::Create,
        )
        .unwrap();
        let old = first
            .store
            .provider("local")
            .unwrap()
            .credential_ref
            .clone()
            .unwrap();
        let second = save_with(
            &vault,
            &fixture.path(),
            first.store.provider("local").unwrap().clone(),
            first.store.revision(),
            SecretChange::Replace("synthetic-next-key".into()),
            SaveMode::Update,
        )
        .unwrap();
        assert!(!vault.entries.borrow().contains_key(&old));
        assert_eq!(vault.entries.borrow().len(), 1);
        let bytes = std::fs::read_to_string(fixture.path()).unwrap();
        assert!(!bytes.contains("synthetic-first-key") && !bytes.contains("synthetic-next-key"));
        let cleared = save_with(
            &vault,
            &fixture.path(),
            second.store.provider("local").unwrap().clone(),
            second.store.revision(),
            SecretChange::Clear,
            SaveMode::Update,
        )
        .unwrap();
        assert!(
            cleared
                .store
                .provider("local")
                .unwrap()
                .credential_ref
                .is_none()
        );
        assert!(vault.entries.borrow().is_empty());
    }
    #[test]
    fn unavailable_vault_never_creates_a_keyless_provider() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        vault.fail_put.set(true);
        assert!(matches!(
            save_with(
                &vault,
                &fixture.path(),
                provider("local"),
                0,
                SecretChange::Replace("fixture-secret".into()),
                SaveMode::Create
            ),
            Err(CredentialError::Unavailable)
        ));
        assert!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .providers()
                .is_empty()
        );
    }
    #[test]
    fn uncertain_keyring_write_rolls_back_only_the_unpublished_key() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        let first = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            0,
            SecretChange::Replace("synthetic-existing-key".into()),
            SaveMode::Create,
        )
        .unwrap();
        let original_bytes = std::fs::read(fixture.path()).unwrap();
        let old_reference = first
            .store
            .provider("local")
            .unwrap()
            .credential_ref
            .clone()
            .unwrap();
        vault.fail_after_put.set(true);

        let result = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            first.store.revision(),
            SecretChange::Replace("synthetic-replacement-key".into()),
            SaveMode::Update,
        );

        assert!(matches!(result, Err(CredentialError::Unavailable)));
        assert_eq!(std::fs::read(fixture.path()).unwrap(), original_bytes);
        assert_eq!(ProviderStore::load(&fixture.path()).unwrap(), first.store);
        assert_eq!(vault.entries.borrow().len(), 1);
        assert_eq!(
            vault
                .entries
                .borrow()
                .get(&old_reference)
                .map(String::as_str),
            Some("synthetic-existing-key")
        );
    }

    #[test]
    fn uncertain_keyring_write_with_failed_cleanup_preserves_the_active_provider() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        let first = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            0,
            SecretChange::Replace("synthetic-existing-key".into()),
            SaveMode::Create,
        )
        .unwrap();
        let original_bytes = std::fs::read(fixture.path()).unwrap();
        let old_reference = first
            .store
            .provider("local")
            .unwrap()
            .credential_ref
            .clone()
            .unwrap();
        vault.fail_after_put.set(true);
        vault.fail_remove.set(true);

        let result = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            first.store.revision(),
            SecretChange::Replace("synthetic-replacement-key".into()),
            SaveMode::Update,
        );

        assert!(matches!(result, Err(CredentialError::RollbackPending)));
        assert_eq!(std::fs::read(fixture.path()).unwrap(), original_bytes);
        assert_eq!(ProviderStore::load(&fixture.path()).unwrap(), first.store);
        assert_eq!(vault.entries.borrow().len(), 2);
        assert_eq!(
            vault
                .entries
                .borrow()
                .get(&old_reference)
                .map(String::as_str),
            Some("synthetic-existing-key")
        );
    }

    #[test]
    fn concurrent_save_rolls_back_only_the_new_secret() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        vault.race.replace(Some(fixture.path()));
        assert!(matches!(
            save_with(
                &vault,
                &fixture.path(),
                provider("local"),
                0,
                SecretChange::Replace("fixture-secret".into()),
                SaveMode::Create
            ),
            Err(CredentialError::Store(StoreError::Conflict))
        ));
        assert!(vault.entries.borrow().is_empty());
        assert!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .provider("concurrent")
                .is_some()
        );
    }
    #[test]
    fn cleanup_failure_reports_committed_state() {
        let fixture = Fixture::new();
        let vault = MemoryVault::default();
        let initial = save_with(
            &vault,
            &fixture.path(),
            provider("local"),
            0,
            SecretChange::Replace("fixture-secret".into()),
            SaveMode::Create,
        )
        .unwrap();
        vault.fail_remove.set(true);
        let outcome =
            remove_with(&vault, &fixture.path(), "local", initial.store.revision()).unwrap();
        assert!(outcome.cleanup_pending);
        assert!(outcome.store.providers().is_empty());
        assert!(
            ProviderStore::load(&fixture.path())
                .unwrap()
                .providers()
                .is_empty()
        );
    }
    #[test]
    fn references_and_secrets_are_bounded_and_reject_header_injection() {
        assert!(!valid_secret("fixture\r\nAuthorization: injected"));
        assert!(!valid_secret(""));
        assert!(!valid_secret(&"a".repeat(MAX_SECRET_BYTES + 1)));
        assert!(validate_reference("unrelated/secret").is_err());
        assert!(validate_reference(&fresh_reference("local").unwrap()).is_ok());
        assert!(
            !CredentialError::Unavailable
                .to_string()
                .contains("fixture-secret")
        );
    }
}
