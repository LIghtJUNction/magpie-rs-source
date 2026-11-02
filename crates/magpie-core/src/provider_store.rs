//! Shared, versioned Provider configuration for CLI, desktop and the Gateway.
//!
//! Only credential references are stored here. Reading configuration never
//! looks up credentials or contacts a Provider. Native writes use an advisory
//! lock, an expected revision and a same-directory atomic replacement. As with
//! other filesystem transactions, an external writer which ignores the lock
//! can still race the final check and rename; conflicts observed before that
//! point are rejected instead of silently replacing newer configuration.

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::catalog::{valid_model_id, valid_provider_id};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_PROVIDERS: usize = 128;
/// Bounds one Provider while accommodating large relay model catalogs.
pub const MAX_PROVIDER_MODELS: usize = 4096;
/// Bounds the combined catalog independently of the 1 MiB on-disk size cap.
pub const MAX_TOTAL_MODELS: usize = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderProtocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    Gemini,
}

impl ProviderProtocol {
    pub const ALL: [Self; 4] = [
        Self::OpenAiChat,
        Self::OpenAiResponses,
        Self::AnthropicMessages,
        Self::Gemini,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::Gemini => "gemini",
        }
    }

    pub fn parse(input: &str) -> Result<Self, StoreError> {
        match input {
            "openai_chat" => Ok(Self::OpenAiChat),
            "openai_responses" => Ok(Self::OpenAiResponses),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            "gemini" => Ok(Self::Gemini),
            _ => Err(StoreError::InvalidField("protocol")),
        }
    }
}

/// Public configuration only: neither field contains the API key itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    pub api_key_env: Option<String>,
    pub credential_ref: Option<String>,
    pub models: Vec<String>,
    pub enabled: bool,
}

impl Provider {
    pub fn validate(&self) -> Result<(), StoreError> {
        if !valid_provider_id(&self.id) {
            return Err(StoreError::InvalidField("id"));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err(StoreError::InvalidField("name"));
        }
        if !valid_base_url(&self.base_url) {
            return Err(StoreError::InvalidField("base_url"));
        }
        if let Some(name) = &self.api_key_env {
            if name.len() > 128
                || !name
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(StoreError::InvalidField("api_key_env"));
            }
        }
        if let Some(reference) = &self.credential_ref {
            if reference.is_empty()
                || reference.len() > 256
                || !reference
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-/:".contains(&byte))
            {
                return Err(StoreError::InvalidField("credential_ref"));
            }
        }
        if self.api_key_env.is_some() && self.credential_ref.is_some() {
            return Err(StoreError::InvalidField("credential_source"));
        }
        if self.models.len() > MAX_PROVIDER_MODELS {
            return Err(StoreError::InvalidField("models"));
        }
        let mut models = BTreeSet::new();
        for model in &self.models {
            if !valid_model_id(model) || !models.insert(model) {
                return Err(StoreError::InvalidField("models"));
            }
        }
        Ok(())
    }
}

/// A successfully parsed configuration snapshot. Missing and unreadable files
/// have different results: only a missing file produces an empty snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderStore {
    revision: u64,
    providers: Vec<Provider>,
}

/// Errors deliberately omit raw configuration, parser excerpts and IO paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    ConfigDirectory,
    UnsafePath,
    NotAFile,
    TooLarge,
    Read,
    Write,
    CommitUncertain,
    Busy,
    Conflict,
    InvalidFormat,
    UnsupportedVersion,
    InvalidField(&'static str),
    DuplicateProvider,
    ProviderNotFound,
    ProviderLimit,
    TotalModelLimit,
    RevisionOverflow,
    UnsupportedPlatform,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "invalid Provider field: {field}"),
            _ => formatter.write_str(match self {
                Self::ConfigDirectory => "cannot determine the Magpie configuration directory",
                Self::UnsafePath => "Provider configuration path is unsafe or not private",
                Self::NotAFile => "Provider configuration must be a regular single-link file",
                Self::TooLarge => "Provider configuration exceeds the 1 MiB size limit",
                Self::Read => {
                    "Provider configuration could not be read; existing data was preserved"
                }
                Self::Write => "Provider configuration could not be saved",
                Self::CommitUncertain => {
                    "Provider replacement may have completed; reload before retrying"
                }
                Self::Busy => "another Provider update is in progress; retry after reloading",
                Self::Conflict => "Provider configuration changed; reload before saving again",
                Self::InvalidFormat => {
                    "Provider configuration is invalid; existing data was preserved"
                }
                Self::UnsupportedVersion => "Provider configuration version is not supported",
                Self::DuplicateProvider => "a Provider with this ID already exists",
                Self::ProviderNotFound => "the selected Provider no longer exists",
                Self::ProviderLimit => "Provider configuration is limited to 128 Providers",
                Self::TotalModelLimit => {
                    "Provider configuration is limited to 16384 models in total"
                }
                Self::RevisionOverflow => "Provider configuration revision limit reached",
                Self::UnsupportedPlatform => {
                    "secure Provider updates are not implemented on this platform"
                }
                Self::InvalidField(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for StoreError {}

pub fn default_path() -> Result<PathBuf, StoreError> {
    crate::paths::config_dir()
        .map(|directory| directory.join("providers.toml"))
        .map_err(|_| StoreError::ConfigDirectory)
}

impl ProviderStore {
    pub fn default_path() -> Result<PathBuf, StoreError> {
        default_path()
    }

    pub fn load(path: &Path) -> Result<Self, StoreError> {
        match native::read(path)? {
            Some(bytes) => Self::parse(&bytes),
            None => Ok(Self::default()),
        }
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    pub fn create(
        path: &Path,
        provider: Provider,
        expected_revision: u64,
    ) -> Result<Self, StoreError> {
        provider.validate()?;
        Self::mutate(path, expected_revision, |store| {
            if store.provider(&provider.id).is_some() {
                return Err(StoreError::DuplicateProvider);
            }
            if store.providers.len() >= MAX_PROVIDERS {
                return Err(StoreError::ProviderLimit);
            }
            store.providers.push(provider);
            Ok(())
        })
    }

    pub fn update(
        path: &Path,
        provider: Provider,
        expected_revision: u64,
    ) -> Result<Self, StoreError> {
        provider.validate()?;
        Self::mutate(path, expected_revision, |store| {
            let old = store
                .providers
                .iter_mut()
                .find(|old| old.id == provider.id)
                .ok_or(StoreError::ProviderNotFound)?;
            *old = provider;
            Ok(())
        })
    }

    pub fn set_enabled(
        path: &Path,
        id: &str,
        enabled: bool,
        expected_revision: u64,
    ) -> Result<Self, StoreError> {
        Self::mutate(path, expected_revision, |store| {
            store
                .providers
                .iter_mut()
                .find(|provider| provider.id == id)
                .ok_or(StoreError::ProviderNotFound)?
                .enabled = enabled;
            Ok(())
        })
    }

    pub fn remove(path: &Path, id: &str, expected_revision: u64) -> Result<Self, StoreError> {
        Self::mutate(path, expected_revision, |store| {
            let index = store
                .providers
                .iter()
                .position(|provider| provider.id == id)
                .ok_or(StoreError::ProviderNotFound)?;
            store.providers.remove(index);
            Ok(())
        })
    }

    fn mutate(
        path: &Path,
        expected_revision: u64,
        update: impl FnOnce(&mut Self) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        native::update(path, |original| {
            let mut store = match original {
                Some(bytes) => Self::parse(bytes)?,
                None => Self::default(),
            };
            if store.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            update(&mut store)?;
            if store
                .providers
                .iter()
                .map(|provider| provider.models.len())
                .sum::<usize>()
                > MAX_TOTAL_MODELS
            {
                return Err(StoreError::TotalModelLimit);
            }
            store.revision = store
                .revision
                .checked_add(1)
                .filter(|revision| *revision <= i64::MAX as u64)
                .ok_or(StoreError::RevisionOverflow)?;
            let bytes = store.encode().into_bytes();
            if bytes.len() > MAX_BYTES {
                return Err(StoreError::TooLarge);
            }
            Ok((store, bytes))
        })
    }

    fn parse(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() > MAX_BYTES {
            return Err(StoreError::TooLarge);
        }
        let source = std::str::from_utf8(bytes).map_err(|_| StoreError::InvalidFormat)?;
        let document = source
            .parse::<DocumentMut>()
            .map_err(|_| StoreError::InvalidFormat)?;
        if document
            .iter()
            .any(|(key, _)| !matches!(key, "version" | "revision" | "providers"))
        {
            return Err(StoreError::InvalidFormat);
        }
        if document.get("version").and_then(Item::as_integer) != Some(1) {
            return Err(StoreError::UnsupportedVersion);
        }
        let revision = document
            .get("revision")
            .and_then(Item::as_integer)
            .filter(|revision| *revision > 0)
            .ok_or(StoreError::InvalidFormat)? as u64;
        let item = document.get("providers").ok_or(StoreError::InvalidFormat)?;
        if item.as_array().is_some_and(|array| array.is_empty()) {
            return Ok(Self {
                revision,
                providers: Vec::new(),
            });
        }
        let tables = item.as_array_of_tables().ok_or(StoreError::InvalidFormat)?;
        if tables.len() > MAX_PROVIDERS {
            return Err(StoreError::ProviderLimit);
        }
        let mut providers = Vec::with_capacity(tables.len());
        let mut ids = BTreeSet::new();
        let mut total_models = 0;
        for table in tables {
            if table.iter().any(|(key, _)| {
                !matches!(
                    key,
                    "id" | "name"
                        | "base_url"
                        | "protocol"
                        | "api_key_env"
                        | "credential_ref"
                        | "models"
                        | "enabled"
                )
            }) {
                return Err(StoreError::InvalidFormat);
            }
            let models = table
                .get("models")
                .and_then(Item::as_array)
                .ok_or(StoreError::InvalidField("models"))?;
            if models.len() > MAX_PROVIDER_MODELS {
                return Err(StoreError::InvalidField("models"));
            }
            total_models += models.len();
            if total_models > MAX_TOTAL_MODELS {
                return Err(StoreError::TotalModelLimit);
            }
            let provider = Provider {
                id: string(table, "id")?.to_owned(),
                name: string(table, "name")?.to_owned(),
                base_url: string(table, "base_url")?.to_owned(),
                protocol: ProviderProtocol::parse(string(table, "protocol")?)?,
                api_key_env: optional_string(table, "api_key_env")?,
                credential_ref: optional_string(table, "credential_ref")?,
                models: models
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_owned)
                            .ok_or(StoreError::InvalidField("models"))
                    })
                    .collect::<Result<_, _>>()?,
                enabled: table
                    .get("enabled")
                    .and_then(Item::as_bool)
                    .ok_or(StoreError::InvalidField("enabled"))?,
            };
            provider.validate()?;
            if !ids.insert(provider.id.clone()) {
                return Err(StoreError::DuplicateProvider);
            }
            providers.push(provider);
        }
        Ok(Self {
            revision,
            providers,
        })
    }

    fn encode(&self) -> String {
        let mut document = DocumentMut::new();
        document["version"] = value(1);
        document["revision"] = value(self.revision as i64);
        if self.providers.is_empty() {
            document["providers"] = value(Array::new());
        } else {
            let mut providers = ArrayOfTables::new();
            for provider in &self.providers {
                let mut table = Table::new();
                table["id"] = value(&provider.id);
                table["name"] = value(&provider.name);
                table["base_url"] = value(&provider.base_url);
                table["protocol"] = value(provider.protocol.as_str());
                table["enabled"] = value(provider.enabled);
                if let Some(name) = &provider.api_key_env {
                    table["api_key_env"] = value(name);
                }
                if let Some(reference) = &provider.credential_ref {
                    table["credential_ref"] = value(reference);
                }
                table["models"] = value(
                    provider
                        .models
                        .iter()
                        .map(String::as_str)
                        .collect::<Array>(),
                );
                providers.push(table);
            }
            document["providers"] = Item::ArrayOfTables(providers);
        }
        document.to_string()
    }
}

fn string<'a>(table: &'a Table, key: &'static str) -> Result<&'a str, StoreError> {
    table
        .get(key)
        .and_then(Item::as_str)
        .ok_or(StoreError::InvalidField(key))
}

fn optional_string(table: &Table, key: &'static str) -> Result<Option<String>, StoreError> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or(StoreError::InvalidField(key))
        })
        .transpose()
}

fn valid_base_url(url: &str) -> bool {
    if url.len() > 2048
        || !url.is_ascii()
        || url
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || url.contains(['?', '#', '\\'])
    {
        return false;
    }
    let (https, rest) = match url.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => match url.strip_prefix("http://") {
            Some(rest) => (false, rest),
            None => return false,
        },
    };
    let path = rest.find('/').map_or("", |index| &rest[index..]);
    if path
        .bytes()
        .any(|byte| !matches!(byte, b'!'..=b'~') || b"<>\"{}|^`".contains(&byte))
        || path.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b'%'
                && !path
                    .as_bytes()
                    .get(index + 1..index + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        })
        || [
            "/chat/completions",
            "/responses",
            "/messages",
            ":generateContent",
            ":streamGenerateContent",
        ]
        .iter()
        .any(|operation| path.trim_end_matches('/').ends_with(operation))
    {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        match suffix {
            "" => (host, None),
            _ => match suffix.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return false,
            },
        }
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if host.len() > 253
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
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return false;
        }
        (host, port)
    };
    if port.is_some_and(|port| {
        port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || !port.parse::<u16>().is_ok_and(|port| port != 0)
    }) {
        return false;
    }
    // Leave DNS resolution to the network layer; HTTP credentials must never
    // travel to a remote host merely because it resolves to an unexpected IP.
    https
        || host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod native {
    use super::{MAX_BYTES, StoreError};
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
        fn open(path: &Path, create: bool) -> Result<Option<Self>, StoreError> {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|_| StoreError::UnsafePath)?
                    .join(path)
            };
            let filename = absolute
                .file_name()
                .ok_or(StoreError::UnsafePath)?
                .to_owned();
            let parent = absolute.parent().ok_or(StoreError::UnsafePath)?;
            if parent.components().any(|component| {
                !matches!(
                    component,
                    Component::RootDir | Component::CurDir | Component::Normal(_)
                )
            }) {
                return Err(StoreError::UnsafePath);
            }
            let mut directory =
                open("/", DIRECTORY_FLAGS, Mode::empty()).map_err(|_| StoreError::UnsafePath)?;
            for component in parent.components() {
                let name = match component {
                    Component::RootDir | Component::CurDir => continue,
                    Component::Normal(name) => name,
                    _ => return Err(StoreError::UnsafePath),
                };
                match openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()) {
                    Ok(next) => directory = next,
                    Err(Errno::NOENT) if create => {
                        match mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
                            Ok(()) => fsync(&directory).map_err(|_| StoreError::Write)?,
                            Err(Errno::EXIST) => {}
                            Err(_) => return Err(StoreError::Write),
                        }
                        directory = openat(&directory, name, DIRECTORY_FLAGS, Mode::empty())
                            .map_err(|_| StoreError::UnsafePath)?;
                    }
                    Err(Errno::NOENT) => return Ok(None),
                    Err(_) => return Err(StoreError::UnsafePath),
                }
            }
            let metadata = fstat(&directory).map_err(|_| StoreError::Read)?;
            if create && (metadata.st_mode & 0o022 != 0 || metadata.st_mode & 0o200 == 0) {
                return Err(StoreError::UnsafePath);
            }
            Ok(Some(Self {
                descriptor: directory,
                filename,
            }))
        }

        fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
            let descriptor = match openat(
                &self.descriptor,
                &self.filename,
                OFlags::RDONLY | FILE_FLAGS,
                Mode::empty(),
            ) {
                Ok(descriptor) => descriptor,
                Err(Errno::NOENT) => return Ok(None),
                Err(Errno::LOOP) => return Err(StoreError::UnsafePath),
                Err(_) => return Err(StoreError::Read),
            };
            regular_file(&descriptor)?;
            let mut bytes = Vec::new();
            File::from(descriptor)
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| StoreError::Read)?;
            if bytes.len() > MAX_BYTES {
                return Err(StoreError::TooLarge);
            }
            Ok(Some(bytes))
        }
    }

    fn regular_file(descriptor: &OwnedFd) -> Result<(), StoreError> {
        let metadata = fstat(descriptor).map_err(|_| StoreError::Read)?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
            || metadata.st_nlink != 1
        {
            return Err(StoreError::NotAFile);
        }
        if metadata.st_size < 0 || metadata.st_size as u64 > MAX_BYTES as u64 {
            return Err(StoreError::TooLarge);
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

    pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
        match Directory::open(path, false)? {
            Some(directory) => directory.read(),
            None => Ok(None),
        }
    }

    pub(super) fn update<T>(
        path: &Path,
        operation: impl FnOnce(Option<&[u8]>) -> Result<(T, Vec<u8>), StoreError>,
    ) -> Result<T, StoreError> {
        let directory = Directory::open(path, true)?.ok_or(StoreError::UnsafePath)?;
        let mut lock_name = OsString::from(".");
        lock_name.push(&directory.filename);
        lock_name.push(".lock");
        let descriptor = openat(
            &directory.descriptor,
            &lock_name,
            OFlags::RDWR | OFlags::CREATE | FILE_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| StoreError::UnsafePath)?;
        regular_file(&descriptor)?;
        flock(&descriptor, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == Errno::WOULDBLOCK {
                StoreError::Busy
            } else {
                StoreError::Write
            }
        })?;
        let _lock = Lock(descriptor);
        let original = directory.read()?;
        let (result, bytes) = operation(original.as_deref())?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StoreError::Write)?
            .as_nanos();
        let temporary = format!(
            ".magpie-provider-{}-{timestamp}-{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        );
        let staged = openat(
            &directory.descriptor,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | FILE_FLAGS,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| StoreError::Write)?;
        let write = (|| {
            let mut staged = File::from(staged);
            staged.write_all(&bytes).map_err(|_| StoreError::Write)?;
            staged.sync_all().map_err(|_| StoreError::Write)?;
            if directory.read()? != original {
                return Err(StoreError::Conflict);
            }
            renameat(
                &directory.descriptor,
                temporary.as_str(),
                &directory.descriptor,
                &directory.filename,
            )
            .map_err(|_| StoreError::Write)?;
            fsync(&directory.descriptor).map_err(|_| StoreError::CommitUncertain)?;
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
mod native {
    use super::{MAX_BYTES, StoreError};
    use std::io::Read;
    use std::path::Path;

    pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(StoreError::Read),
        };
        if !metadata.is_file() {
            return Err(StoreError::NotAFile);
        }
        if metadata.len() > MAX_BYTES as u64 {
            return Err(StoreError::TooLarge);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| StoreError::Read)?
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError::Read)?;
        if bytes.len() > MAX_BYTES {
            return Err(StoreError::TooLarge);
        }
        Ok(Some(bytes))
    }

    pub(super) fn update<T>(
        _: &Path,
        _: impl FnOnce(Option<&[u8]>) -> Result<(T, Vec<u8>), StoreError>,
    ) -> Result<T, StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }
}
