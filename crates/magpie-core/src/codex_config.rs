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

/// The explicit top-level Codex model settings, without credentials or other fields.
///
/// Missing or nonstring fields remain `None`; values are never inferred from
/// defaults, profiles, or provider definitions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexSelection {
    /// The configured model ID, exactly as stored.
    pub model: Option<String>,
    /// The configured provider ID, exactly as stored.
    pub model_provider: Option<String>,
    /// The configured reasoning effort, exactly as stored.
    pub model_reasoning_effort: Option<String>,
}

/// Reads only the selected model, provider, and effort from a bounded Codex config.
///
/// Returns `None` when the file is missing. A valid file with no explicit string
/// selections returns `Some(CodexSelection::default())`. Uses the same regular
/// file, symlink, UTF-8, TOML, and 4 MiB checks as config inspection and previews.
///
/// # Errors
///
/// Returns a config error for unsafe, unreadable, oversized, or malformed files.
/// Parse diagnostics contain a line number without source excerpts.
pub fn read_selection(path: &Path) -> Result<Option<CodexSelection>, CodexConfigError> {
    load(path).map(|loaded| {
        loaded.map(|loaded| {
            let selected = |name| {
                loaded
                    .document
                    .get(name)
                    .and_then(Item::as_str)
                    .map(str::to_owned)
            };
            CodexSelection {
                model: selected("model"),
                model_provider: selected("model_provider"),
                model_reasoning_effort: selected("model_reasoning_effort"),
            }
        })
    })
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
        config: PathBuf,
    }

    impl Fixture {
        fn new(contents: &[u8]) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "magpie-selection-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create isolated config directory");
            let config = root.join("config.toml");
            fs::write(&config, contents).expect("write isolated Codex config");
            Self { root, config }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn selection_preserves_configured_strings_and_excludes_secret_fields() {
        let original = br#"model = "vendor/selected-model"
model_provider = "my-provider"
model_reasoning_effort = "high"
api_key = "fixture-secret-root"
[model_providers.my-provider]
name = "Fixture provider"
api_key = "fixture-secret-provider"
[model_providers.my-provider.http_headers]
Authorization = "Bearer fixture-secret-header"
[profiles.other]
model = "other-model"
model_reasoning_effort = "low"
"#;
        let fixture = Fixture::new(original);
        let selection = read_selection(&fixture.config)
            .expect("read valid config")
            .expect("config exists");
        assert_eq!(
            selection,
            CodexSelection {
                model: Some("vendor/selected-model".to_owned()),
                model_provider: Some("my-provider".to_owned()),
                model_reasoning_effort: Some("high".to_owned()),
            }
        );
        assert!(!format!("{selection:?}").contains("fixture-secret"));
        assert_eq!(
            fs::read(&fixture.config).expect("read unchanged fixture"),
            original
        );
    }

    #[test]
    fn selections_are_optional_and_never_inferred_or_coerced() {
        let fixture = Fixture::new(
            br#"model = 123
model_reasoning_effort = ["high"]
model_provider = "native"
[model_providers.native]
model = "not-a-top-level-selection"
api_key = "fixture-secret"
"#,
        );
        assert_eq!(
            read_selection(&fixture.config).expect("read valid TOML"),
            Some(CodexSelection {
                model_provider: Some("native".to_owned()),
                ..CodexSelection::default()
            })
        );
        let empty = Fixture::new(b"# native defaults\n");
        assert_eq!(
            read_selection(&empty.config).expect("read config without selections"),
            Some(CodexSelection::default())
        );
        assert_eq!(
            read_selection(&empty.root.join("missing.toml")).expect("missing config is allowed"),
            None
        );
    }

    #[test]
    fn malformed_config_diagnostics_never_include_secrets_or_source() {
        let fixture = Fixture::new(
            b"model='selected'\napi_key='fixture-secret-root'\nmodel_provider=\"fixture-secret-unclosed\n",
        );
        let error = read_selection(&fixture.config).expect_err("malformed TOML must fail");
        assert_eq!(error, CodexConfigError::InvalidToml { line: Some(3) });
        let diagnostics = format!("{error} {error:?}");
        assert!(!diagnostics.contains("fixture-secret"));
        assert!(!diagnostics.contains("api_key"));
        assert!(diagnostics.contains("line 3"));
    }

    #[test]
    fn selection_uses_the_existing_file_and_encoding_limits() {
        let fixture = Fixture::new(b"\xfffixture-secret");
        assert_eq!(
            read_selection(&fixture.config),
            Err(CodexConfigError::InvalidUtf8)
        );
        assert_eq!(
            read_selection(&fixture.root),
            Err(CodexConfigError::NotAFile)
        );
        File::create(&fixture.config)
            .expect("replace owned fixture")
            .set_len(MAX_CONFIG_BYTES + 1)
            .expect("create oversized sparse fixture");
        assert_eq!(
            read_selection(&fixture.config),
            Err(CodexConfigError::TooLarge)
        );
    }

    #[cfg(unix)]
    #[test]
    fn selection_refuses_symlinks_instead_of_following_them() {
        let fixture = Fixture::new(b"model='selected'\napi_key='fixture-secret'");
        let link = fixture.root.join("linked.toml");
        std::os::unix::fs::symlink(&fixture.config, &link).expect("link owned fixture");
        assert_eq!(read_selection(&link), Err(CodexConfigError::Symlink));
    }
}
