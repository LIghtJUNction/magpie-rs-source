//! Syntax-independent semantic views and preserving edits for transaction ownership.
use super::{ConfigFormat, Error, Value, jsonc, toml};

pub(super) struct Document {
    pub(super) value: Value,
}

pub(super) fn parse(bytes: &[u8], format: ConfigFormat) -> Result<Document, Error> {
    let value = match format {
        ConfigFormat::Toml => toml::parse(bytes)?,
        ConfigFormat::Json | ConfigFormat::Jsonc => jsonc::parse(bytes, format)?.value,
    };
    Ok(Document { value })
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub(super) fn set(
    bytes: &[u8],
    format: ConfigFormat,
    keys: &[String],
    value: Option<&Value>,
) -> Result<Vec<u8>, Error> {
    match format {
        ConfigFormat::Toml => toml::set(bytes, keys, value),
        ConfigFormat::Json | ConfigFormat::Jsonc => jsonc::set(bytes, format, keys, value),
    }
}
