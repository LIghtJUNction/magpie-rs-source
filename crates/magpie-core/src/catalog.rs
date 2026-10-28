//! Loads a bounded, public model catalog without accepting credential fields.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table};

/// Maximum accepted catalog file size, including comments and whitespace.
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;
/// Limits expansion when a provider's display name is repeated for many models.
const MAX_MODELS_JSON_BYTES: usize = 8 * 1024 * 1024;
const EMPTY_MODELS_JSON: &str = "{\"object\":\"list\",\"data\":[]}";

/// An immutable snapshot of configured public models in configuration order.
///
/// Each model has an OpenAI-compatible `id`, `object`, `created`, and `owned_by`,
/// plus `name` (the original model ID) and `provider_name` (the display name).
/// `created` is zero because the configuration supplies no creation timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    models_json: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            models_json: EMPTY_MODELS_JSON.to_owned(),
        }
    }
}

/// A catalog read or validation failure that never includes configuration contents.
#[derive(Debug)]
pub struct LoadCatalogError {
    kind: ErrorKind,
}

#[derive(Debug)]
enum ErrorKind {
    Read(io::Error),
    NotAFile,
    TooLarge,
    InvalidUtf8,
    InvalidToml {
        line: Option<usize>,
    },
    InvalidField {
        provider: Option<usize>,
        field: &'static str,
        reason: &'static str,
    },
    DuplicateProvider {
        provider: usize,
    },
    DuplicateModel {
        provider: usize,
        model: usize,
    },
    JsonTooLarge,
}

impl fmt::Display for LoadCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Read(error) => write!(f, "cannot read catalog: {error}"),
            ErrorKind::NotAFile => f.write_str("catalog path is not a regular file"),
            ErrorKind::TooLarge => f.write_str("catalog exceeds the 1 MiB file limit"),
            ErrorKind::InvalidUtf8 => f.write_str("catalog is not valid UTF-8"),
            ErrorKind::InvalidToml { line: Some(line) } => {
                write!(f, "catalog has invalid TOML near line {line}")
            }
            ErrorKind::InvalidToml { line: None } => f.write_str("catalog has invalid TOML"),
            ErrorKind::InvalidField {
                provider: Some(provider),
                field,
                reason,
            } => write!(f, "catalog provider #{provider} field '{field}': {reason}"),
            ErrorKind::InvalidField {
                provider: None,
                field,
                reason,
            } => write!(f, "catalog field '{field}': {reason}"),
            ErrorKind::DuplicateProvider { provider } => {
                write!(f, "catalog provider #{provider} has a duplicate id")
            }
            ErrorKind::DuplicateModel { provider, model } => write!(
                f,
                "catalog provider #{provider} model #{model} has a duplicate id"
            ),
            ErrorKind::JsonTooLarge => {
                f.write_str("catalog model list exceeds the 8 MiB response limit")
            }
        }
    }
}

impl std::error::Error for LoadCatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Read(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LoadCatalogError {
    fn from(error: io::Error) -> Self {
        Self {
            kind: ErrorKind::Read(error),
        }
    }
}

impl Catalog {
    /// Loads `[[providers]]` tables containing only `id`, `name`, and `models`.
    ///
    /// Empty files and `providers = []` produce an empty catalog. Each provider
    /// must supply all three fields; its models array may be empty. IDs are case
    /// sensitive. Provider IDs are 1..=128 ASCII bytes, start with a letter or
    /// digit, and otherwise contain letters, digits, `.`, `_`, or `-`; `group` is
    /// reserved. Model IDs are 1..=256 ASCII bytes, start with a letter or digit,
    /// and additionally permit `:`, `@`, and `/` for upstream namespaces.
    /// Empty, `.` and `..` model path segments are rejected. Display names must
    /// contain 1..=256 UTF-8 bytes, be nonblank, and contain no control characters.
    ///
    /// # Errors
    ///
    /// Rejects unreadable or nonregular files, files larger than 1 MiB, invalid
    /// UTF-8/TOML, unknown fields, invalid identifiers or field types, duplicate
    /// provider IDs, duplicate model IDs within one provider, and model lists
    /// larger than 8 MiB after JSON encoding. Diagnostics omit input values,
    /// unknown field names, and parser excerpts, including from `Debug`.
    pub fn load(path: &Path) -> Result<Self, LoadCatalogError> {
        check_file(&fs::metadata(path)?)?;
        let file = File::open(path)?;
        check_file(&file.metadata()?)?;
        let mut bytes = Vec::new();
        file.take(MAX_CATALOG_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CATALOG_BYTES {
            return Err(LoadCatalogError {
                kind: ErrorKind::TooLarge,
            });
        }
        let source = String::from_utf8(bytes).map_err(|_| LoadCatalogError {
            kind: ErrorKind::InvalidUtf8,
        })?;
        Self::parse(&source)
    }

    /// Returns the OpenAI model list whose IDs are `provider/model`.
    ///
    /// Borrows the validated JSON snapshot without allocating.
    #[must_use]
    pub fn models_json(&self) -> &str {
        &self.models_json
    }

    fn parse(source: &str) -> Result<Self, LoadCatalogError> {
        let document = source
            .parse::<DocumentMut>()
            .map_err(|error| LoadCatalogError {
                kind: ErrorKind::InvalidToml {
                    line: error.span().map(|span| {
                        source.as_bytes()[..span.start.min(source.len())]
                            .iter()
                            .filter(|byte| **byte == b'\n')
                            .count()
                            + 1
                    }),
                },
            })?;
        if document.iter().any(|(key, _)| key != "providers") {
            return Err(invalid_field(
                None,
                "fields",
                "only 'providers' is allowed; credentials do not belong in catalogs",
            ));
        }
        let Some(providers) = document.get("providers") else {
            return Ok(Self::default());
        };
        if providers.as_array().is_some_and(|array| array.is_empty()) {
            return Ok(Self::default());
        }
        let providers = providers
            .as_array_of_tables()
            .ok_or_else(|| invalid_field(None, "providers", "expected [[providers]] tables"))?;
        let mut provider_ids = BTreeSet::new();
        let mut json = String::from("{\"object\":\"list\",\"data\":[");
        let mut first_model = true;
        for (index, provider) in providers.iter().enumerate() {
            let position = index + 1;
            if provider
                .iter()
                .any(|(key, _)| !matches!(key, "id" | "name" | "models"))
            {
                return Err(invalid_field(
                    Some(position),
                    "fields",
                    "only 'id', 'name', and 'models' are allowed; credentials do not belong in catalogs",
                ));
            }
            let id = string_field(provider, position, "id")?;
            if !valid_provider_id(id) {
                return Err(invalid_field(
                    Some(position),
                    "id",
                    "expected 1..=128 ASCII identifier bytes starting with a letter or digit; 'group' is reserved",
                ));
            }
            if !provider_ids.insert(id) {
                return Err(LoadCatalogError {
                    kind: ErrorKind::DuplicateProvider { provider: position },
                });
            }
            let name = string_field(provider, position, "name")?;
            if name.len() > 256 || name.trim().is_empty() || name.chars().any(char::is_control) {
                return Err(invalid_field(
                    Some(position),
                    "name",
                    "expected 1..=256 UTF-8 bytes, nonblank and without control characters",
                ));
            }
            let models = provider
                .get("models")
                .and_then(Item::as_array)
                .ok_or_else(|| {
                    invalid_field(Some(position), "models", "expected an array of model IDs")
                })?;
            let provider_name = quote_json(name);
            let mut model_ids = BTreeSet::new();
            for (model_index, model) in models.iter().enumerate() {
                let model = model.as_str().filter(|id| valid_model_id(id)).ok_or_else(|| {
                    invalid_field(
                        Some(position),
                        "models",
                        "expected 1..=256 ASCII model identifier bytes starting with a letter or digit",
                    )
                })?;
                if !model_ids.insert(model) {
                    return Err(LoadCatalogError {
                        kind: ErrorKind::DuplicateModel {
                            provider: position,
                            model: model_index + 1,
                        },
                    });
                }
                let entry = format!(
                    r#"{{"id":"{id}/{model}","object":"model","created":0,"owned_by":"{id}","name":"{model}","provider_name":{provider_name}}}"#
                );
                if json.len() + usize::from(!first_model) + entry.len() + 2 > MAX_MODELS_JSON_BYTES
                {
                    return Err(LoadCatalogError {
                        kind: ErrorKind::JsonTooLarge,
                    });
                }
                if !first_model {
                    json.push(',');
                }
                json.push_str(&entry);
                first_model = false;
            }
        }
        json.push_str("]}");
        Ok(Self { models_json: json })
    }
}

fn check_file(metadata: &fs::Metadata) -> Result<(), LoadCatalogError> {
    if !metadata.is_file() {
        return Err(LoadCatalogError {
            kind: ErrorKind::NotAFile,
        });
    }
    if metadata.len() > MAX_CATALOG_BYTES {
        return Err(LoadCatalogError {
            kind: ErrorKind::TooLarge,
        });
    }
    Ok(())
}

fn invalid_field(
    provider: Option<usize>,
    field: &'static str,
    reason: &'static str,
) -> LoadCatalogError {
    LoadCatalogError {
        kind: ErrorKind::InvalidField {
            provider,
            field,
            reason,
        },
    }
}

fn string_field<'a>(
    table: &'a Table,
    provider: usize,
    field: &'static str,
) -> Result<&'a str, LoadCatalogError> {
    table
        .get(field)
        .and_then(Item::as_str)
        .ok_or_else(|| invalid_field(Some(provider), field, "expected a string"))
}

fn valid_provider_id(id: &str) -> bool {
    id.len() <= 128
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && id != "group"
}

fn valid_model_id(id: &str) -> bool {
    id.len() <= 256
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@/".contains(&byte))
        && id.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn quote_json(value: &str) -> String {
    let mut json = String::with_capacity(value.len() + 2);
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            // Validated display names contain no controls; all remaining UTF-8 is valid JSON.
            _ => json.push(character),
        }
    }
    json.push('"');
    json
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);

    struct TempCatalog(PathBuf);

    impl TempCatalog {
        fn new(contents: &[u8]) -> Self {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "magpie-catalog-{}-{timestamp}-{sequence}.toml",
                std::process::id()
            ));
            let mut file = File::create_new(&path).expect("create isolated catalog fixture");
            file.write_all(contents).expect("write catalog fixture");
            Self(path)
        }

        fn load(&self) -> Result<Catalog, LoadCatalogError> {
            Catalog::load(&self.0)
        }
    }

    impl Drop for TempCatalog {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn default_and_explicit_empty_catalogs_are_openai_lists() {
        assert_eq!(Catalog::default().models_json(), EMPTY_MODELS_JSON);
        for source in ["", "# no providers yet\n", "providers = []\n"] {
            let fixture = TempCatalog::new(source.as_bytes());
            assert_eq!(
                fixture.load().expect("load empty catalog").models_json(),
                EMPTY_MODELS_JSON
            );
        }
        let fixture = TempCatalog::new(b"[[providers]]\nid='local'\nname='Local'\nmodels=[]");
        assert_eq!(
            fixture
                .load()
                .expect("load provider without models")
                .models_json(),
            EMPTY_MODELS_JSON
        );
    }

    #[test]
    fn configured_models_preserve_order_and_provider_identity() {
        let fixture = TempCatalog::new(
            br#"[[providers]]
id = "local"
name = "Local"
models = ["a", "namespace/b:latest@v1"]
[[providers]]
id = "remote"
name = "Remote"
models = ["a"]
"#,
        );
        assert_eq!(
            fixture
                .load()
                .expect("load configured catalog")
                .models_json(),
            concat!(
                r#"{"object":"list","data":["#,
                r#"{"id":"local/a","object":"model","created":0,"owned_by":"local","name":"a","provider_name":"Local"},"#,
                r#"{"id":"local/namespace/b:latest@v1","object":"model","created":0,"owned_by":"local","name":"namespace/b:latest@v1","provider_name":"Local"},"#,
                r#"{"id":"remote/a","object":"model","created":0,"owned_by":"remote","name":"a","provider_name":"Remote"}]}"#
            )
        );
    }

    #[test]
    fn display_names_are_json_escaped_and_keep_unicode() {
        let fixture = TempCatalog::new(
            br#"[[providers]]
id = "local"
name = "Local \"Lab\" \\ \u672C"
models = ["a"]
"#,
        );
        let expected = concat!(
            r#"{"object":"list","data":[{"id":"local/a","object":"model","created":0,"owned_by":"local","name":"a","provider_name":"Local \"Lab\" \\ "#,
            "\u{672c}",
            r#""}]}"#
        );
        assert_eq!(
            fixture
                .load()
                .expect("load escaped display name")
                .models_json(),
            expected
        );
    }

    #[test]
    fn schema_rejects_unknown_fields_and_credentials_without_echoing_them() {
        for source in [
            "fixture_secret_field = 'fixture_secret_value'",
            "[[providers]]\nid='local'\nname='Local'\nmodels=['a']\nfixture_secret_field='fixture_secret_value'",
            "[[providers]]\nid='local'\nname='Local'\nmodels=['a']\napi_key='fixture_secret_value'",
            "[[providers]]\nid='local'\nname='Local'\nmodels=['a']\n[providers.credentials]\ntoken='fixture_secret_value'",
        ] {
            let fixture = TempCatalog::new(source.as_bytes());
            let error = fixture.load().expect_err("unknown catalog field must fail");
            assert!(error.to_string().contains("only"), "{error}");
            let diagnostics = format!("{error} {error:?}");
            assert!(!diagnostics.contains("fixture_secret"));
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[test]
    fn malformed_toml_reports_location_without_parser_excerpt() {
        let fixture = TempCatalog::new(
            b"[[providers]]\nid='local'\nname=\"fixture_secret_value\nmodels=['a']\n",
        );
        let error = fixture.load().expect_err("malformed TOML must fail");
        assert!(
            error.to_string().contains("invalid TOML near line 3"),
            "{error}"
        );
        assert!(!format!("{error} {error:?}").contains("fixture_secret_value"));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn schema_rejects_missing_fields_and_wrong_types() {
        for source in [
            "providers = 'local'",
            "[providers]\nid='local'",
            "[[providers]]\nname='Local'\nmodels=['a']",
            "[[providers]]\nid='local'\nmodels=['a']",
            "[[providers]]\nid='local'\nname='Local'",
            "[[providers]]\nid=1\nname='Local'\nmodels=['a']",
            "[[providers]]\nid='local'\nname=1\nmodels=['a']",
            "[[providers]]\nid='local'\nname='Local'\nmodels='a'",
            "[[providers]]\nid='local'\nname='Local'\nmodels=['a', 1]",
            "[[providers]]\nid='local'\nname='Local'\nmodels=[{id='a',key='fixture_secret_value'}]",
        ] {
            let fixture = TempCatalog::new(source.as_bytes());
            let error = fixture.load().expect_err("invalid schema must fail");
            assert!(error.to_string().contains("expected"), "{error}");
            assert!(!format!("{error:?}").contains("fixture_secret_value"));
        }
    }

    #[test]
    fn provider_identifiers_reject_ambiguous_or_unsafe_names() {
        for id in ["", "a/b", "../a", "group", "a b", "a:port", "-local"] {
            let source = format!("[[providers]]\nid='{id}'\nname='Local'\nmodels=['a']");
            let fixture = TempCatalog::new(source.as_bytes());
            let error = fixture.load().expect_err("invalid provider ID must fail");
            assert!(error.to_string().contains("field 'id'"), "{error}");
        }
        let source = format!(
            "[[providers]]\nid='{}'\nname='Local'\nmodels=['a']",
            "a".repeat(129)
        );
        assert!(TempCatalog::new(source.as_bytes()).load().is_err());
    }

    #[test]
    fn model_identifiers_reject_ambiguous_or_unsafe_names() {
        for model in [
            "",
            "/a",
            "a/",
            "a//b",
            "a/../b",
            "a/./b",
            "a b",
            "a?key=secret",
            "a\\b",
            "a\"b",
        ] {
            let source = format!("[[providers]]\nid='local'\nname='Local'\nmodels=['{model}']");
            let fixture = TempCatalog::new(source.as_bytes());
            let error = fixture.load().expect_err("invalid model ID must fail");
            assert!(error.to_string().contains("field 'models'"), "{error}");
        }
        let source = format!(
            "[[providers]]\nid='local'\nname='Local'\nmodels=['{}']",
            "a".repeat(257)
        );
        assert!(TempCatalog::new(source.as_bytes()).load().is_err());
    }

    #[test]
    fn display_names_reject_blank_controls_and_excessive_length() {
        for name in [
            "\"\"".to_owned(),
            "'   '".to_owned(),
            "\"line\\nfeed\"".to_owned(),
            "\"tab\\tname\"".to_owned(),
            "\"\\u007f\"".to_owned(),
            format!("'{}'", "a".repeat(257)),
        ] {
            let source = format!("[[providers]]\nid='local'\nname={name}\nmodels=['a']");
            let fixture = TempCatalog::new(source.as_bytes());
            let error = fixture.load().expect_err("invalid display name must fail");
            assert!(error.to_string().contains("field 'name'"), "{error}");
        }
    }

    #[test]
    fn duplicate_provider_and_model_ids_fail_instead_of_deduplicating() {
        let fixture = TempCatalog::new(
            b"[[providers]]\nid='local'\nname='First'\nmodels=['a']\n[[providers]]\nid='local'\nname='Second'\nmodels=['b']",
        );
        assert_eq!(
            fixture
                .load()
                .expect_err("duplicate provider must fail")
                .to_string(),
            "catalog provider #2 has a duplicate id"
        );
        let fixture =
            TempCatalog::new(b"[[providers]]\nid='local'\nname='Local'\nmodels=['a','a']");
        assert_eq!(
            fixture
                .load()
                .expect_err("duplicate model must fail")
                .to_string(),
            "catalog provider #1 model #2 has a duplicate id"
        );
    }

    #[test]
    fn file_loading_distinguishes_missing_nonregular_and_invalid_utf8() {
        let fixture = TempCatalog::new(b"\xff");
        assert_eq!(
            fixture
                .load()
                .expect_err("invalid UTF-8 must fail")
                .to_string(),
            "catalog is not valid UTF-8"
        );
        let missing = fixture.0.with_extension("missing");
        let error = Catalog::load(&missing).expect_err("missing file must fail");
        assert!(error.to_string().starts_with("cannot read catalog:"));
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(
            Catalog::load(&std::env::temp_dir())
                .expect_err("directory must fail")
                .to_string(),
            "catalog path is not a regular file"
        );
    }

    #[test]
    fn file_size_limit_includes_exactly_one_mib() {
        let mut contents = b"[[providers]]\nid='local'\nname='Local'\nmodels=['a']\n#".to_vec();
        contents.resize(MAX_CATALOG_BYTES as usize, b' ');
        let fixture = TempCatalog::new(&contents);
        assert!(fixture.load().is_ok(), "exactly 1 MiB is supported");
        contents.push(b' ');
        let fixture = TempCatalog::new(&contents);
        assert_eq!(
            fixture
                .load()
                .expect_err("oversized file must fail")
                .to_string(),
            "catalog exceeds the 1 MiB file limit"
        );
    }

    #[test]
    fn repeated_display_metadata_cannot_expand_json_without_a_bound() {
        let models = (0..25_000)
            .map(|index| format!("'m{index}'"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            "[[providers]]\nid='local'\nname='{}'\nmodels=[{models}]",
            "n".repeat(256)
        );
        assert!(source.len() < MAX_CATALOG_BYTES as usize);
        let fixture = TempCatalog::new(source.as_bytes());
        assert_eq!(
            fixture
                .load()
                .expect_err("expanded response must be bounded")
                .to_string(),
            "catalog model list exceeds the 8 MiB response limit"
        );
    }
}
