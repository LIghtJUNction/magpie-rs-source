use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::gateway::default_gateway_addr;

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const FIXTURE_MODELS: [&str; 2] = ["local/fixture-a", "local/fixture-b"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexConfigError {
    NotAFile,
    Symlink,
    TooLarge,
    InvalidUtf8,
    Read,
    InvalidToml { line: Option<usize> },
    Conflict(&'static str),
    UnsupportedModel,
}

impl CodexConfigError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotAFile => "config_not_a_file",
            Self::Symlink => "config_symlink",
            Self::TooLarge => "config_too_large",
            Self::InvalidUtf8 => "config_invalid_utf8",
            Self::Read => "config_read_error",
            Self::InvalidToml { .. } => "config_parse_error",
            Self::Conflict(_) => "config_conflict",
            Self::UnsupportedModel => "unsupported_model",
        }
    }
}

impl std::fmt::Display for CodexConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAFile => f.write_str("Codex config path is not a regular file"),
            Self::Symlink => f.write_str("Codex config is a symlink; refusing to follow it"),
            Self::TooLarge => f.write_str("Codex config exceeds the 4 MiB preview limit"),
            Self::InvalidUtf8 => f.write_str("Codex config is not valid UTF-8"),
            Self::Read => f.write_str("cannot read Codex config"),
            Self::InvalidToml { line: Some(line) } => {
                write!(f, "Codex config has invalid TOML near line {line}")
            }
            Self::InvalidToml { line: None } => f.write_str("Codex config has invalid TOML"),
            Self::Conflict(field) => write!(
                f,
                "Codex config field '{field}' has an unsupported shape or is already user-owned"
            ),
            Self::UnsupportedModel => {
                f.write_str("preview supports local/fixture-a or local/fixture-b")
            }
        }
    }
}

struct LoadedConfig {
    document: DocumentMut,
    exists: bool,
}

pub fn inspect_file(path: &Path) -> Result<bool, CodexConfigError> {
    load(path).map(|loaded| loaded.is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub field: &'static str,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConnectPreview {
    pub config_exists: bool,
    pub model: String,
    pub changes: Vec<FieldChange>,
}

pub fn preview_connect(path: &Path, model: &str) -> Result<CodexConnectPreview, CodexConfigError> {
    if !FIXTURE_MODELS.contains(&model) {
        return Err(CodexConfigError::UnsupportedModel);
    }

    let loaded = load(path)?;
    let config_exists = loaded.as_ref().is_some_and(|config| config.exists);
    let document = loaded.map_or_else(DocumentMut::new, |config| config.document);

    let features = table(&document, "features")?;
    let providers = table(&document, "model_providers")?;
    if providers.is_some_and(|table| table.contains_key("magpie")) {
        return Err(CodexConfigError::Conflict("model_providers.magpie"));
    }

    let mut changes = Vec::new();
    push_change(
        &mut changes,
        "model",
        string_field(&document, "model")?,
        model,
    );
    push_change(
        &mut changes,
        "model_provider",
        string_field(&document, "model_provider")?,
        "magpie",
    );

    let discovery_enabled = match features.and_then(|table| table.get("api_key_model_discovery")) {
        None => None,
        Some(item) => Some(item.as_value().and_then(Value::as_bool).ok_or(
            CodexConfigError::Conflict("features.api_key_model_discovery"),
        )?),
    };
    if discovery_enabled != Some(true) {
        changes.push(FieldChange {
            field: "features.api_key_model_discovery",
            before: discovery_enabled.map(|value| value.to_string()),
            after: "true".to_owned(),
        });
    }

    let endpoint = default_gateway_addr();
    let base_url = format!("http://{endpoint}/v1");
    let catalog_url = format!("http://{endpoint}/v1/codex/models");
    for (field, value) in [
        ("model_providers.magpie.name", "magpie".to_owned()),
        ("model_providers.magpie.base_url", base_url),
        ("model_providers.magpie.wire_api", "responses".to_owned()),
        (
            "model_providers.magpie.requires_openai_auth",
            "false".to_owned(),
        ),
        (
            "model_providers.magpie.env_key",
            "MAGPIE_LOCAL_TOKEN".to_owned(),
        ),
        (
            "model_providers.magpie.supports_websockets",
            "false".to_owned(),
        ),
        ("model_providers.magpie.model_catalog_url", catalog_url),
    ] {
        changes.push(FieldChange {
            field,
            before: None,
            after: value,
        });
    }

    Ok(CodexConnectPreview {
        config_exists,
        model: model.to_owned(),
        changes,
    })
}

fn load(path: &Path) -> Result<Option<LoadedConfig>, CodexConfigError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CodexConfigError::Read),
    };
    if metadata.file_type().is_symlink() {
        return Err(CodexConfigError::Symlink);
    }
    if !metadata.is_file() {
        return Err(CodexConfigError::NotAFile);
    }

    let file = File::open(path).map_err(|_| CodexConfigError::Read)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CodexConfigError::Read)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(CodexConfigError::TooLarge);
    }

    let source = String::from_utf8(bytes).map_err(|_| CodexConfigError::InvalidUtf8)?;
    let document =
        source
            .parse::<DocumentMut>()
            .map_err(|error| CodexConfigError::InvalidToml {
                line: error.span().map(|span| {
                    source.as_bytes()[..span.start.min(source.len())]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count()
                        + 1
                }),
            })?;

    Ok(Some(LoadedConfig {
        document,
        exists: true,
    }))
}

fn table<'a>(
    document: &'a DocumentMut,
    name: &'static str,
) -> Result<Option<&'a Table>, CodexConfigError> {
    match document.get(name) {
        None => Ok(None),
        Some(Item::Table(table)) => Ok(Some(table)),
        Some(_) => Err(CodexConfigError::Conflict(name)),
    }
}

fn string_field(
    document: &DocumentMut,
    name: &'static str,
) -> Result<Option<String>, CodexConfigError> {
    match document.get(name) {
        None => Ok(None),
        Some(Item::Value(value)) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(CodexConfigError::Conflict(name)),
        Some(_) => Err(CodexConfigError::Conflict(name)),
    }
}

fn push_change(
    changes: &mut Vec<FieldChange>,
    field: &'static str,
    before: Option<String>,
    after: &str,
) {
    if before.as_deref() != Some(after) {
        changes.push(FieldChange {
            field,
            before,
            after: after.to_owned(),
        });
    }
}
