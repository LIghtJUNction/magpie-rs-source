//! Vendor-neutral discovery, persisted enablement and acknowledged plugin registration.
//!
//! Parsing never activates a plugin. The application registers contributions in
//! its typed engines, then acknowledges activation. Any returned withdrawal list
//! must be disposed in order before dispatch resumes. No manifest executes code.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub use toml_edit::DocumentMut;
use toml_edit::{Item, Table, value};

mod native;

/// Per-file input and private state limit; total discovery is bounded separately.
const MAX_BYTES: usize = 256 * 1024;
const MAX_PLUGINS: usize = 256;
const MAX_SCAN_ENTRIES: usize = 1024;
const MAX_SERVICES: usize = 64;

/// Stable, sanitized failures. Parser excerpts, file contents and paths are omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginError {
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
    DuplicateId,
    Limit,
    NotFound,
    NotReady,
    Apply,
    RevisionOverflow,
    UnsupportedPlatform,
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::InvalidField(field) = self {
            return write!(f, "invalid plugin field: {field}");
        }
        f.write_str(match self {
            Self::ConfigDirectory => "cannot locate plugin configuration directory",
            Self::UnsafePath => "plugin state path is unsafe or not private",
            Self::NotAFile => "plugin input must be a regular single-link file",
            Self::TooLarge => "plugin input exceeds the size limit",
            Self::Read => "cannot read plugin input; existing data was preserved",
            Self::Write => "cannot write plugin state",
            Self::CommitUncertain => "plugin state may have been saved; reload before retrying",
            Self::Busy => "another plugin update holds the lock; reload and retry",
            Self::Conflict => "plugin state changed; reload before saving",
            Self::InvalidFormat => "invalid plugin document; existing data was preserved",
            Self::UnsupportedVersion => "plugin format or API version is unsupported",
            Self::DuplicateId => {
                "ambiguous duplicate plugin ID; all matching plugins were rejected"
            }
            Self::Limit => "plugin discovery or declaration limit exceeded",
            Self::NotFound => "plugin ID was not found",
            Self::NotReady => "plugin is disabled, failed, unsupported or awaiting dependencies",
            Self::Apply => "plugin registration failed; withdraw this owner's registrations",
            Self::RevisionOverflow => "plugin state revision limit reached",
            Self::UnsupportedPlatform => {
                "secure plugin state updates are unsupported on this platform"
            }
            Self::InvalidField(_) => unreachable!(),
        })
    }
}
impl std::error::Error for PluginError {}

/// Runtime families remain distinct even when IDs or display names resemble each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    ProviderPreset,
    AgentAdapter,
    Feature,
    CommunityProvider,
    McpServer,
    GatewayMiddleware,
}
impl PluginKind {
    /// Stable spelling for CLI and UI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderPreset => "provider-preset",
            Self::AgentAdapter => "agent-adapter",
            Self::Feature => "feature",
            Self::CommunityProvider => "community-provider",
            Self::McpServer => "mcp-server",
            Self::GatewayMiddleware => "gateway-middleware",
        }
    }
    fn parse(s: &str) -> Result<Self, PluginError> {
        match s {
            "provider-preset" => Ok(Self::ProviderPreset),
            "agent-adapter" => Ok(Self::AgentAdapter),
            "feature" => Ok(Self::Feature),
            "community-provider" => Ok(Self::CommunityProvider),
            "mcp-server" => Ok(Self::McpServer),
            "gateway-middleware" => Ok(Self::GatewayMiddleware),
            _ => Err(PluginError::InvalidField("kind")),
        }
    }
    fn declarative(self) -> bool {
        matches!(
            self,
            Self::ProviderPreset | Self::AgentAdapter | Self::Feature
        )
    }
}

/// Where this exact manifest was discovered, independent of its self-reported metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginSource {
    Bundled(String),
    User(PathBuf),
}

/// An injected application bundle or user declaration. Debug never prints its payload.
#[derive(Clone)]
pub struct PluginInput {
    pub source: PluginSource,
    pub text: String,
}
impl PluginInput {
    /// Injects bundled text without coupling core to any vendor or feature package.
    pub fn bundled(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            source: PluginSource::Bundled(label.into()),
            text: text.into(),
        }
    }
}
impl fmt::Debug for PluginInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginInput")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Validated envelope; typed contribution schemas belong to their respective engines.
#[derive(Clone)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub kind: PluginKind,
    pub enabled_by_default: bool,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    document: DocumentMut,
}
impl fmt::Debug for PluginManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginManifest")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
impl PluginManifest {
    /// Parses a bounded API-1 envelope; errors never include the supplied text.
    pub fn parse(text: &str) -> Result<Self, PluginError> {
        if text.len() > MAX_BYTES {
            return Err(PluginError::TooLarge);
        }
        let document = text
            .parse::<DocumentMut>()
            .map_err(|_| PluginError::InvalidFormat)?;
        if string(&document, "format")? != "magpie-plugin/1"
            || document.get("api_version").and_then(Item::as_integer) != Some(1)
        {
            return Err(PluginError::UnsupportedVersion);
        }
        let id = string(&document, "id")?;
        if !valid_id(&id) {
            return Err(PluginError::InvalidField("id"));
        }
        let name = string(&document, "name")?;
        if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(PluginError::InvalidField("name"));
        }
        let version = string(&document, "version")?;
        if !valid_version(&version) {
            return Err(PluginError::InvalidField("version"));
        }
        let kind = PluginKind::parse(&string(&document, "kind")?)?;
        let enabled_by_default = document
            .get("enabled_by_default")
            .and_then(Item::as_bool)
            .ok_or(PluginError::InvalidField("enabled_by_default"))?;
        let requires = services(&document, "requires")?;
        let provides = services(&document, "provides")?;
        if document.get("contributions").is_some_and(|v| !v.is_table()) {
            return Err(PluginError::InvalidField("contributions"));
        }
        Ok(Self {
            id,
            name,
            version,
            api_version: 1,
            kind,
            enabled_by_default,
            requires,
            provides,
            document,
        })
    }
    /// Original document for typed engines; never put this data in diagnostics or logs.
    pub fn document(&self) -> &DocumentMut {
        &self.document
    }
}

/// A dependency cannot become available just because its manifest was parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Off,
    Pending { waiting_for: Vec<String> },
    Active,
    Failed(PluginError),
    Unsupported,
}
impl PluginStatus {
    /// Stable lifecycle spelling for presentation layers.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Pending { .. } => "pending",
            Self::Active => "active",
            Self::Failed(_) => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Read-only registration record; owner is always the validated manifest ID.
#[derive(Clone, Debug)]
pub struct PluginRecord {
    pub manifest: PluginManifest,
    pub source: PluginSource,
    pub enabled: bool,
    pub status: PluginStatus,
}
impl PluginRecord {
    /// Stable owner used when withdrawing pages, adapters, observers and services.
    pub fn owner(&self) -> &str {
        &self.manifest.id
    }
}

/// Discovery errors are isolated so another valid plugin can still be registered.
#[derive(Clone, Debug)]
pub struct PluginDiagnostic {
    pub source: PluginSource,
    pub error: PluginError,
}

/// Engine input carrying explicit source and owner metadata.
#[derive(Clone, Copy, Debug)]
pub struct PluginContribution<'a> {
    pub owner: &'a str,
    pub source: &'a PluginSource,
    pub kind: PluginKind,
    pub manifest: &'a PluginManifest,
}

/// Engine registration authority and persisted user preferences, without executable loading.
pub struct PluginRegistry {
    records: Vec<PluginRecord>,
    diagnostics: Vec<PluginDiagnostic>,
    revision: u64,
    overrides: BTreeMap<String, bool>,
    snapshot: Option<Vec<u8>>,
    state_path: Option<PathBuf>,
    bundled: Vec<PluginInput>,
    user_directory: Option<PathBuf>,
    host_services: BTreeSet<String>,
    activation_order: Vec<String>,
}
impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("records", &self.records)
            .field("diagnostics", &self.diagnostics)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
impl PluginRegistry {
    /// Builds an in-memory registry from application-injected declarations.
    pub fn from_manifests(
        manifests: impl IntoIterator<Item = PluginInput>,
    ) -> Result<Self, PluginError> {
        let mut bundled = Vec::new();
        for manifest in manifests {
            if bundled.len() == MAX_PLUGINS {
                return Err(PluginError::Limit);
            }
            bundled.push(manifest);
        }
        let mut result = Self {
            records: Vec::new(),
            diagnostics: Vec::new(),
            revision: 0,
            overrides: BTreeMap::new(),
            snapshot: None,
            state_path: None,
            bundled,
            user_directory: None,
            host_services: BTreeSet::new(),
            activation_order: Vec::new(),
        };
        result.discover()?;
        Ok(result)
    }
    /// Loads injected bundles, bounded user packages and a private state file.
    ///
    /// Missing state is empty; malformed/unreadable state returns an error and is preserved.
    pub fn load(
        bundled: impl IntoIterator<Item = PluginInput>,
        user_directory: &Path,
        state_path: &Path,
    ) -> Result<Self, PluginError> {
        if !user_directory.is_absolute() || !state_path.is_absolute() {
            return Err(PluginError::UnsafePath);
        }
        let mut result = Self::from_manifests(bundled)?;
        result.state_path = Some(state_path.to_owned());
        result.user_directory = Some(user_directory.to_owned());
        result.snapshot = native::read(state_path)?;
        (result.revision, result.overrides) = parse_state(result.snapshot.as_deref())?;
        result.discover()?;
        Ok(result)
    }
    /// Uses the same configuration root as ProviderStore.
    pub fn default_state_path() -> Result<PathBuf, PluginError> {
        crate::paths::config_dir()
            .map(|p| p.join("plugins.toml"))
            .map_err(|_| PluginError::ConfigDirectory)
    }
    /// Default unpacked declarative package directory; no packages are installed here automatically.
    pub fn default_plugin_directory() -> Result<PathBuf, PluginError> {
        crate::paths::config_dir()
            .map(|p| p.join("plugins"))
            .map_err(|_| PluginError::ConfigDirectory)
    }
    /// Current persisted preference revision, unaffected by activation acknowledgements.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// All validated envelopes, including off, failed and unsupported records.
    pub fn records(&self) -> &[PluginRecord] {
        &self.records
    }
    /// Invalid files which could not produce a validated envelope.
    pub fn diagnostics(&self) -> &[PluginDiagnostic] {
        &self.diagnostics
    }
    /// Finds one unambiguous envelope; duplicate IDs remain visibly failed.
    pub fn record(&self, id: &str) -> Option<&PluginRecord> {
        self.records.iter().find(|r| r.manifest.id == id)
    }
    /// Contributions available to consumers after successful engine registration.
    pub fn contributions(&self) -> Vec<PluginContribution<'_>> {
        self.select(|r| r.status == PluginStatus::Active)
    }
    /// Ready declarations for registration; waiting/unsupported/failed declarations are excluded.
    pub fn pending_contributions(&self) -> Vec<PluginContribution<'_>> {
        self.select(|r| matches!(&r.status, PluginStatus::Pending { waiting_for } if waiting_for.is_empty()))
    }
    fn select(&self, predicate: impl Fn(&PluginRecord) -> bool) -> Vec<PluginContribution<'_>> {
        self.records
            .iter()
            .filter(|r| predicate(r))
            .map(|r| PluginContribution {
                owner: r.owner(),
                source: &r.source,
                kind: r.manifest.kind,
                manifest: &r.manifest,
            })
            .collect()
    }
    /// Declares services actually installed by the host; returns owners to dispose first.
    pub fn set_host_services(
        &mut self,
        services: impl IntoIterator<Item = String>,
    ) -> Result<Vec<String>, PluginError> {
        let mut next = BTreeSet::new();
        for service in services {
            if next.len() == MAX_SERVICES || !valid_id(&service) || !next.insert(service) {
                return Err(PluginError::InvalidField("services"));
            }
        }
        self.host_services = next;
        Ok(self.reconcile())
    }
    /// Acknowledges complete real registration by the application, never mere parsing.
    ///
    /// Call only after every typed contribution has been installed transactionally.
    pub fn acknowledge_activation(&mut self, id: &str) -> Result<(), PluginError> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.manifest.id == id)
            .ok_or(PluginError::NotFound)?;
        if !matches!(&record.status, PluginStatus::Pending { waiting_for } if waiting_for.is_empty())
        {
            return Err(PluginError::NotReady);
        }
        record.status = PluginStatus::Active;
        self.activation_order.push(id.to_owned());
        self.reconcile();
        Ok(())
    }
    /// Marks a registration failure and invalidates dependents; dispose returned owners in order.
    ///
    /// The host must also roll back partially registered contributions owned by `id`.
    pub fn fail_activation(&mut self, id: &str) -> Result<Vec<String>, PluginError> {
        let record = self
            .records
            .iter_mut()
            .find(|r| r.manifest.id == id)
            .ok_or(PluginError::NotFound)?;
        if !record.enabled
            || matches!(
                record.status,
                PluginStatus::Failed(PluginError::DuplicateId) | PluginStatus::Unsupported
            )
        {
            return Err(PluginError::NotReady);
        }
        record.status = PluginStatus::Failed(PluginError::Apply);
        let mut withdrawals = self.reconcile();
        if !withdrawals.iter().any(|owner| owner == id) {
            withdrawals.push(id.to_owned());
        }
        Ok(withdrawals)
    }
    /// Persists enablement with optimistic revision and exact-byte conflict checking.
    ///
    /// Dispose the returned owners in order before serving further actions/requests.
    /// Data files and credentials are never removed. Failed entries retry after off/on.
    pub fn set_enabled(
        &mut self,
        id: &str,
        enabled: bool,
        expected_revision: u64,
    ) -> Result<Vec<String>, PluginError> {
        if expected_revision != self.revision {
            return Err(PluginError::Conflict);
        }
        let record = self.record(id).ok_or(PluginError::NotFound)?;
        if record.status == PluginStatus::Failed(PluginError::DuplicateId) {
            return Err(PluginError::DuplicateId);
        }
        let revision = self
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(PluginError::RevisionOverflow)?;
        let mut overrides = self.overrides.clone();
        overrides.insert(id.to_owned(), enabled);
        if overrides.len() > MAX_PLUGINS {
            return Err(PluginError::Limit);
        }
        let bytes = encode_state(revision, &overrides);
        if let Some(path) = &self.state_path {
            native::update(path, |original| {
                if original != self.snapshot.as_deref() {
                    return Err(PluginError::Conflict);
                }
                let (disk_revision, _) = parse_state(original)?;
                if disk_revision != expected_revision {
                    return Err(PluginError::Conflict);
                }
                Ok(((), bytes.clone()))
            })?;
        }
        self.snapshot = Some(bytes);
        self.overrides = overrides;
        self.revision = revision;
        let record = self
            .records
            .iter_mut()
            .find(|r| r.manifest.id == id)
            .ok_or(PluginError::NotFound)?;
        let was_enabled = record.enabled;
        record.enabled = enabled;
        if enabled && !was_enabled {
            record.status = PluginStatus::Pending {
                waiting_for: Vec::new(),
            };
        }
        Ok(self.reconcile())
    }
    /// Rereads disk, returning every formerly active owner for reverse-order disposal.
    ///
    /// A failed reload leaves this registry intact. Successful reload requires fresh registration.
    pub fn reload(&mut self) -> Result<Vec<String>, PluginError> {
        let mut next = Self::from_manifests(self.bundled.clone())?;
        next.user_directory = self.user_directory.clone();
        next.state_path = self.state_path.clone();
        next.host_services = self.host_services.clone();
        if let Some(path) = &next.state_path {
            next.snapshot = native::read(path)?;
            (next.revision, next.overrides) = parse_state(next.snapshot.as_deref())?;
        } else {
            next.revision = self.revision;
            next.overrides = self.overrides.clone();
            next.snapshot = self.snapshot.clone();
        }
        next.discover()?;
        let withdrawals = self.activation_order.iter().rev().cloned().collect();
        *self = next;
        Ok(withdrawals)
    }
    fn discover(&mut self) -> Result<(), PluginError> {
        let mut inputs = self.bundled.clone();
        self.records.clear();
        self.diagnostics.clear();
        if let Some(directory) = &self.user_directory {
            let (user_inputs, diagnostics) = discover_directory(directory)?;
            inputs.extend(user_inputs);
            self.diagnostics = diagnostics;
        }
        if inputs.len() > MAX_PLUGINS {
            return Err(PluginError::Limit);
        }
        for input in inputs {
            match PluginManifest::parse(&input.text) {
                Ok(manifest) => {
                    let enabled = self
                        .overrides
                        .get(&manifest.id)
                        .copied()
                        .unwrap_or(manifest.enabled_by_default);
                    self.records.push(PluginRecord {
                        manifest,
                        source: input.source,
                        enabled,
                        status: PluginStatus::Pending {
                            waiting_for: Vec::new(),
                        },
                    });
                }
                Err(error) => self.diagnostics.push(PluginDiagnostic {
                    source: input.source,
                    error,
                }),
            }
        }
        let mut counts = BTreeMap::new();
        for record in &self.records {
            *counts.entry(record.manifest.id.clone()).or_insert(0) += 1;
        }
        for record in &mut self.records {
            if counts[&record.manifest.id] > 1 {
                record.status = PluginStatus::Failed(PluginError::DuplicateId);
            }
        }
        self.reconcile();
        Ok(())
    }
    fn reconcile(&mut self) -> Vec<String> {
        // Withdraw transitively until no active owner depends on a withdrawn service.
        loop {
            let mut services = self.host_services.clone();
            for record in &self.records {
                if record.enabled && record.status == PluginStatus::Active {
                    services.extend(record.manifest.provides.iter().cloned());
                }
            }
            let mut changed = false;
            for record in &mut self.records {
                if record.status == PluginStatus::Failed(PluginError::DuplicateId) {
                    continue;
                }
                let next = if !record.enabled {
                    PluginStatus::Off
                } else if !record.manifest.kind.declarative() {
                    PluginStatus::Unsupported
                } else if matches!(record.status, PluginStatus::Failed(_)) {
                    continue;
                } else {
                    let waiting_for: Vec<_> = record
                        .manifest
                        .requires
                        .iter()
                        .filter(|s| !services.contains(*s))
                        .cloned()
                        .collect();
                    if waiting_for.is_empty() && record.status == PluginStatus::Active {
                        PluginStatus::Active
                    } else {
                        PluginStatus::Pending { waiting_for }
                    }
                };
                if record.status != next {
                    record.status = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut withdrawn = Vec::new();
        self.activation_order.retain(|id| {
            if self
                .records
                .iter()
                .any(|r| r.manifest.id == *id && r.status == PluginStatus::Active)
            {
                true
            } else {
                withdrawn.push(id.clone());
                false
            }
        });
        withdrawn.reverse();
        withdrawn
    }
}

fn string(document: &DocumentMut, key: &'static str) -> Result<String, PluginError> {
    document
        .get(key)
        .and_then(Item::as_str)
        .map(str::to_owned)
        .ok_or(PluginError::InvalidField(key))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.split('.').all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_lowercase()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        })
}
fn valid_version(version: &str) -> bool {
    if version.len() > 128 {
        return false;
    }
    let (release, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    let (base, prerelease) = release
        .split_once('-')
        .map_or((release, None), |(a, b)| (a, Some(b)));
    let numeric = |part: &str| {
        !part.is_empty()
            && (part == "0" || !part.starts_with('0'))
            && part.bytes().all(|b| b.is_ascii_digit())
            && part.parse::<u64>().is_ok()
    };
    let identifiers = |text: &str, reject_leading_zero: bool| {
        text.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!reject_leading_zero
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || numeric(part))
        })
    };
    base.split('.').count() == 3
        && base.split('.').all(numeric)
        && prerelease.is_none_or(|s| identifiers(s, true))
        && build.is_none_or(|s| identifiers(s, false))
}

fn services(document: &DocumentMut, key: &'static str) -> Result<Vec<String>, PluginError> {
    let array = document
        .get(key)
        .and_then(Item::as_array)
        .ok_or(PluginError::InvalidField(key))?;
    if array.len() > MAX_SERVICES {
        return Err(PluginError::Limit);
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for entry in array {
        let service = entry.as_str().ok_or(PluginError::InvalidField(key))?;
        if !valid_id(service) || !seen.insert(service) {
            return Err(PluginError::InvalidField(key));
        }
        result.push(service.to_owned());
    }
    Ok(result)
}
fn parse_state(bytes: Option<&[u8]>) -> Result<(u64, BTreeMap<String, bool>), PluginError> {
    let Some(bytes) = bytes else {
        return Ok((0, BTreeMap::new()));
    };
    if bytes.len() > MAX_BYTES {
        return Err(PluginError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PluginError::InvalidFormat)?;
    let document = text
        .parse::<DocumentMut>()
        .map_err(|_| PluginError::InvalidFormat)?;
    if document.get("format").and_then(Item::as_str) != Some("magpie-plugin-state/1") {
        return Err(PluginError::UnsupportedVersion);
    }
    if document
        .iter()
        .any(|(key, _)| !["format", "revision", "enabled"].contains(&key))
    {
        return Err(PluginError::InvalidFormat);
    }
    let revision = document
        .get("revision")
        .and_then(Item::as_integer)
        .filter(|r| *r >= 0)
        .ok_or(PluginError::InvalidField("revision"))? as u64;
    let table = document
        .get("enabled")
        .and_then(Item::as_table)
        .ok_or(PluginError::InvalidField("enabled"))?;
    if table.len() > MAX_PLUGINS {
        return Err(PluginError::Limit);
    }
    let mut overrides = BTreeMap::new();
    for (id, enabled) in table {
        if !valid_id(id) {
            return Err(PluginError::InvalidField("enabled"));
        }
        overrides.insert(
            id.to_owned(),
            enabled
                .as_bool()
                .ok_or(PluginError::InvalidField("enabled"))?,
        );
    }
    Ok((revision, overrides))
}
fn encode_state(revision: u64, overrides: &BTreeMap<String, bool>) -> Vec<u8> {
    let mut document = DocumentMut::new();
    document["format"] = value("magpie-plugin-state/1");
    document["revision"] = value(revision as i64);
    let mut enabled = Table::new();
    for (id, setting) in overrides {
        enabled[id] = value(*setting);
    }
    document["enabled"] = Item::Table(enabled);
    document.to_string().into_bytes()
}

fn discover_directory(
    directory: &Path,
) -> Result<(Vec<PluginInput>, Vec<PluginDiagnostic>), PluginError> {
    // One level of packages only; no recursive walk, symlink following or npm execution.
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(_) => return Err(PluginError::Read),
    };
    let mut paths = Vec::new();
    for entry in entries {
        if paths.len() == MAX_SCAN_ENTRIES {
            return Err(PluginError::Limit);
        }
        paths.push(entry.map_err(|_| PluginError::Read)?.path());
    }
    paths.sort();
    let mut inputs = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        let source = PluginSource::User(path.clone());
        let read = (|| {
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| PluginError::Read)?;
            if metadata.is_dir() {
                read_manifest(&path.join("plugin.toml"))
            } else if path.extension().is_some_and(|ext| ext == "toml") || metadata.is_symlink() {
                read_manifest(&path)
            } else {
                Ok(None)
            }
        })();
        match read {
            Ok(Some((path, text))) => {
                if inputs.len() == MAX_PLUGINS {
                    return Err(PluginError::Limit);
                }
                inputs.push(PluginInput {
                    source: PluginSource::User(path),
                    text,
                });
            }
            Ok(None) => {}
            Err(error) => diagnostics.push(PluginDiagnostic { source, error }),
        }
    }
    Ok((inputs, diagnostics))
}
fn read_manifest(path: &Path) -> Result<Option<(PathBuf, String)>, PluginError> {
    // The descriptor-relative reader also rejects parent symlinks, FIFOs and hardlinks.
    let bytes = native::read_manifest(path)?;
    bytes
        .map(|bytes| {
            String::from_utf8(bytes)
                .map(|text| (path.to_owned(), text))
                .map_err(|_| PluginError::InvalidFormat)
        })
        .transpose()
}

#[cfg(test)]
mod tests;
