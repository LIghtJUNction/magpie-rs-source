//! Declarative, reversible configuration transactions for independently packaged Agent plugins.
//!
//! Reads only the caller-selected file. Recovery images are local plaintext in a
//! private adjacent directory, never a credential export. No gateway health is
//! inferred. Native writes currently support Linux, Android and macOS only.
//! Noncooperating writers must be quiescent: rename is not filesystem CAS.
use serde_json::Value;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use sha2::{Digest, Sha256};
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use std::path::PathBuf;

mod document;
mod jsonc;
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
mod native;
mod toml;

/// Target syntax; JSONC and TOML preserve comments and unowned settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Jsonc,
    /// TOML tables and scalar ownership; arrays and datetimes remain unowned.
    Toml,
}
/// How a plugin's stored selection maps back to the unified model ID.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionSpec {
    pub path: Vec<String>,
    #[serde(default)]
    pub strip_prefix: Option<String>,
}
/// One owned scalar field, with literal path segments and optional template variables.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedFieldSpec {
    pub path: Vec<String>,
    pub value: Value,
}
/// Declarative plugin definition. The transaction core contains no vendor adapters.
///
/// `config_paths` are discovery hints, resolved by the host; the engine opens only
/// the absolute path explicitly passed to an operation. `${model}` and
/// `${gateway}` templates are expanded in string field values; `${model}` is also
/// supported in path segments. The gateway is the loopback HTTP origin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub config_paths: Vec<String>,
    pub format: ConfigFormat,
    pub selection: SelectionSpec,
    #[serde(default)]
    pub reject_existing: Vec<Vec<String>>,
    pub fields: Vec<OwnedFieldSpec>,
}
impl AgentDefinition {
    /// Decode a bounded manifest; unsupported schemas or ambiguous ownership fail closed.
    pub fn from_manifest_json(manifest: &str) -> Result<Self, ConnectionError> {
        if manifest.len() > 64 * 1024 {
            return Err(Error::TooLarge);
        }
        // The same duplicate-key detection as configuration parsing applies here.
        jsonc::parse(manifest.as_bytes(), ConfigFormat::Json)?;
        let definition: Self =
            serde_json::from_str(manifest).map_err(|_| Error::InvalidDefinition)?;
        definition.validate()?;
        Ok(definition)
    }
    /// Validate public-field definitions constructed by a plugin registry.
    pub fn validate(&self) -> Result<(), ConnectionError> {
        let valid_path = |path: &[String]| {
            !path.is_empty()
                && path.len() <= 16
                && path
                    .iter()
                    .all(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        };
        if self.schema_version != 1
            || self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || self.name.len() > 256
            || self.config_paths.len() > 16
            || self
                .config_paths
                .iter()
                .any(|p| p.len() > 4096 || p.chars().any(char::is_control))
            || self.reject_existing.len() > 128
            || self.fields.is_empty()
            || self.fields.len() > 128
            || !valid_path(&self.selection.path)
            || self.selection.path.iter().any(|s| s.contains("${"))
            || self
                .reject_existing
                .iter()
                .any(|p| !valid_path(p) || p.iter().any(|s| s.contains("${")))
        {
            return Err(Error::InvalidDefinition);
        }
        let mut seen = std::collections::BTreeSet::new();
        for field in &self.fields {
            if !valid_path(&field.path)
                || field.value.is_object()
                || field.value.is_array()
                || (self.format == ConfigFormat::Toml
                    && (field.value.is_null()
                        || field.value.as_u64().is_some_and(|n| n > i64::MAX as u64)))
                || field.path.iter().any(|s| {
                    s.replace("${model}", "").contains("${") || s.matches("${model}").count() > 1
                })
                || field.value.as_str().is_some_and(|s| {
                    s.len() > 4096
                        || s.replace("${model}", "")
                            .replace("${gateway}", "")
                            .contains("${")
                })
                || !seen.insert(field.path.clone())
            {
                return Err(Error::InvalidDefinition);
            }
        }
        if seen
            .iter()
            .any(|a| seen.iter().any(|b| a != b && b.starts_with(a)))
        {
            return Err(Error::InvalidDefinition);
        }
        if !self.fields.iter().any(|f| {
            f.path == self.selection.path
                && f.value.as_str().is_some_and(|s| s.contains("${model}"))
        }) {
            return Err(Error::InvalidDefinition);
        }
        Ok(())
    }
}
/// Data-free failures: no parser snippets, keys, or original configuration values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionError {
    UnsupportedPlatform,
    UnsafePath,
    MissingParent,
    MissingConfig,
    NotAFile,
    Symlink,
    TooLarge,
    InvalidUtf8,
    InvalidJson,
    ReadOnly,
    Read,
    Write,
    LockBusy,
    InvalidDefinition,
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
impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent configuration: {self:?}")
    }
}
impl std::error::Error for ConnectionError {}
use ConnectionError as Error;
/// Ownership only, never an Agent-process or gateway health claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Unmanaged,
    Connected,
    Drifted,
    RecoveryRequired,
}
impl ConnectionStatus {
    /// Stable CLI status string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmanaged => "unmanaged",
            Self::Connected => "connected",
            Self::Drifted => "drifted",
            Self::RecoveryRequired => "recovery-required",
        }
    }
}
/// Read-only explicit selection from this file, with no synthesized default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub model: Option<String>,
    pub status: ConnectionStatus,
}
/// Counts of owned fields restored or retained after external edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisconnectOutcome {
    pub restored_fields: usize,
    pub preserved_fields: usize,
    pub config_removed: bool,
}
/// Single-use preview bound to the exact file identity and ownership revision.
/// Debug exposes field names only; original bytes remain private.
pub struct PreparedConnect {
    revision: String,
    changed_fields: Vec<String>,
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    inner: native::Plan,
}
impl PreparedConnect {
    /// Opaque revision for preview/apply clients.
    pub fn revision(&self) -> &str {
        &self.revision
    }
    /// JSON pointer field names; values are deliberately omitted.
    pub fn changed_fields(&self) -> &[String] {
        &self.changed_fields
    }
}
impl fmt::Debug for PreparedConnect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedConnect")
            .field("changed_fields", &self.changed_fields)
            .finish_non_exhaustive()
    }
}
/// A model change retains the first connection's recovery image.
#[derive(Debug)]
pub struct PreparedModelUpdate(PreparedConnect);
impl PreparedModelUpdate {
    /// Opaque revision bound to both file and ownership state.
    pub fn revision(&self) -> &str {
        self.0.revision()
    }
    /// Names only, never previous private values.
    pub fn changed_fields(&self) -> &[String] {
        self.0.changed_fields()
    }
}
/// Read only the selected configuration and its ownership record.
pub fn read_selection(definition: &AgentDefinition, path: &Path) -> Result<Selection, Error> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::selection(definition, path);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (definition, path);
        Err(Error::UnsupportedPlatform)
    }
}
/// Preview a new connection without writing configuration, locks, or recovery files.
pub fn prepare_connect(
    definition: &AgentDefinition,
    path: &Path,
    model: &str,
    gateway: SocketAddr,
) -> Result<PreparedConnect, Error> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::prepare(definition, path, model, Some(gateway));
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (definition, path, model, gateway);
        Err(Error::UnsupportedPlatform)
    }
}
/// Apply the preview only while its file and ownership revision still match.
pub fn commit_connect(plan: PreparedConnect) -> Result<ConnectionStatus, Error> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::commit(plan.inner);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = plan;
        Err(Error::UnsupportedPlatform)
    }
}
/// Preview a new model on an existing undrifted connection; endpoint stays fixed.
pub fn prepare_model_update(
    definition: &AgentDefinition,
    path: &Path,
    model: &str,
) -> Result<PreparedModelUpdate, Error> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::prepare(definition, path, model, None).map(PreparedModelUpdate);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (definition, path, model);
        Err(Error::UnsupportedPlatform)
    }
}
/// Apply the bound model preview, retaining the original recovery point.
pub fn commit_model_update(plan: PreparedModelUpdate) -> Result<ConnectionStatus, Error> {
    commit_connect(plan.0)
}
/// Restore still-owned fields; interrupted unambiguous transactions recover first.
pub fn disconnect(definition: &AgentDefinition, path: &Path) -> Result<DisconnectOutcome, Error> {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    return native::disconnect(definition, path);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        let _ = (definition, path);
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn fields(
    definition: &AgentDefinition,
    model: &str,
    gateway: SocketAddr,
) -> Vec<(Vec<String>, Value)> {
    definition
        .fields
        .iter()
        .map(|field| {
            let path = field
                .path
                .iter()
                .map(|s| s.replace("${model}", model))
                .collect();
            let value = match &field.value {
                Value::String(s) => Value::String(
                    s.replace("${model}", model)
                        .replace("${gateway}", &format!("http://{gateway}")),
                ),
                v => v.clone(),
            };
            (path, value)
        })
        .collect()
}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn pointer(keys: &[String]) -> String {
    keys.iter()
        .map(|s| format!("/{}", s.replace('~', "~0").replace('/', "~1")))
        .collect()
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod tests;

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod toml_tests;
