//! Vendor-independent connection defaults contributed by runtime provider plugins.
//!
//! Discovery, registration and enablement belong to the plugin Registry. This module
//! contains no built-in vendors, network operations or credential access.

use crate::provider_store::{Provider, ProviderProtocol, StoreError};

/// An editable connection seed, owned by a runtime plugin rather than static core data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Stable contribution ID, distinct from an independently saved Provider instance ID.
    pub id: String,
    /// Display name, including a region where the endpoint is region-specific.
    pub name: String,
    /// API root, excluding the operation path appended by the Gateway.
    pub base_url: String,
    /// Default wire protocol supported by this API root and authentication scheme.
    pub protocol: ProviderProtocol,
    /// Whether the default server requires a key; false still permits local server tokens.
    pub requires_key: bool,
    /// Official documentation supporting the API connection defaults.
    pub source: String,
}

impl ProviderPreset {
    /// Checks connection fields using the same validation as saved Providers.
    ///
    /// This does not assert model availability, credentials or successful registration.
    ///
    /// # Errors
    /// Returns a field error for invalid Provider defaults or a missing HTTPS source.
    pub fn validate(&self) -> Result<(), StoreError> {
        Provider {
            id: self.id.clone(),
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            protocol: self.protocol,
            api_key_env: None,
            credential_ref: None,
            models: Vec::new(),
            enabled: true,
        }
        .validate()?;
        // Sources are citations, not request targets; permit UTF-8 documentation paths.
        let rest = self
            .source
            .strip_prefix("https://")
            .ok_or(StoreError::InvalidField("source"))?;
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty()
            || authority.contains(['@', '?', '#', '\\'])
            || self.source.len() > 2048
            || self
                .source
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(StoreError::InvalidField("source"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_preset() -> ProviderPreset {
        ProviderPreset {
            id: "synthetic".into(),
            name: "Synthetic API".into(),
            base_url: "https://api.example.test/v1".into(),
            protocol: ProviderProtocol::OpenAiChat,
            requires_key: true,
            source: "https://docs.example.test/api".into(),
        }
    }

    #[test]
    fn generic_defaults_accept_all_supported_protocols_without_models_or_credentials() {
        for protocol in ProviderProtocol::ALL {
            let mut preset = synthetic_preset();
            preset.protocol = protocol;
            assert_eq!(preset.validate(), Ok(()), "{protocol:?}");
            assert_eq!(ProviderProtocol::parse(protocol.as_str()), Ok(protocol));
        }
    }

    #[test]
    fn connection_defaults_follow_provider_validation() {
        for url in [
            "http://remote.example.test/v1",
            "https://api.example.test/v1/chat/completions",
            "https://user:secret@api.example.test/v1",
            "https://api.example.test/v1?key=synthetic",
        ] {
            let mut preset = synthetic_preset();
            preset.base_url = url.into();
            assert_eq!(preset.validate(), Err(StoreError::InvalidField("base_url")));
        }
        let mut preset = synthetic_preset();
        preset.base_url = "http://127.0.0.1:12345/v1".into();
        preset.requires_key = false;
        assert_eq!(preset.validate(), Ok(()));
        preset.id = "invalid id".into();
        assert_eq!(preset.validate(), Err(StoreError::InvalidField("id")));
    }

    #[test]
    fn documentation_sources_must_be_present_and_credential_free() {
        for source in [
            "",
            "http://docs.example.test",
            "https://",
            "https://user:secret@docs.example.test/api",
            "https://docs.example.test/\napi",
        ] {
            let mut preset = synthetic_preset();
            preset.source = source.into();
            assert_eq!(preset.validate(), Err(StoreError::InvalidField("source")));
        }
    }

    #[test]
    fn shipped_manifests_contribute_valid_unique_provider_defaults() {
        use std::collections::BTreeSet;
        use std::fs;
        use std::path::Path;

        use toml_edit::DocumentMut;

        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/providers");
        let mut plugin_ids = BTreeSet::new();
        let mut preset_ids = BTreeSet::new();
        for entry in fs::read_dir(directory).expect("bundled provider plugin directory") {
            let path = entry
                .expect("plugin directory entry")
                .path()
                .join("plugin.toml");
            if !path.is_file() {
                continue;
            }
            let input = fs::read_to_string(&path).expect("UTF-8 provider plugin manifest");
            let manifest = input.parse::<DocumentMut>().expect("valid TOML manifest");
            assert_eq!(
                manifest["format"].as_str(),
                Some("magpie-plugin/1"),
                "{}",
                path.display()
            );
            assert_eq!(manifest["kind"].as_str(), Some("provider-preset"));
            assert_eq!(manifest["version"].as_str(), Some("0.1.0"));
            assert_eq!(manifest["api_version"].as_integer(), Some(1));
            assert!(
                manifest["requires"]
                    .as_array()
                    .expect("plugin dependencies")
                    .is_empty()
            );
            let provides = manifest["provides"]
                .as_array()
                .expect("plugin capabilities");
            assert!(
                provides
                    .iter()
                    .any(|item| item.as_str() == Some("provider-preset"))
            );
            assert_eq!(manifest["enabled_by_default"].as_bool(), Some(true));
            let plugin_id = manifest["id"].as_str().expect("plugin ID");
            assert!(plugin_id.starts_with("provider."));
            assert!(
                plugin_ids.insert(plugin_id.to_owned()),
                "duplicate plugin: {plugin_id}"
            );
            let providers = manifest["contributions"]["providers"]
                .as_array_of_tables()
                .expect("provider contributions");
            assert!(!providers.is_empty(), "empty plugin: {plugin_id}");
            for fields in providers {
                let string = |key: &str| {
                    fields[key]
                        .as_str()
                        .expect("string preset field")
                        .to_owned()
                };
                let preset = ProviderPreset {
                    id: string("id"),
                    name: string("name"),
                    base_url: string("base_url"),
                    protocol: ProviderProtocol::parse(&string("protocol")).expect("known protocol"),
                    requires_key: fields["requires_key"].as_bool().expect("key requirement"),
                    source: string("source"),
                };
                assert_eq!(preset.validate(), Ok(()), "{}: {}", plugin_id, preset.id);
                assert!(
                    preset_ids.insert(preset.id.clone()),
                    "duplicate preset: {}",
                    preset.id
                );
                if !preset.requires_key {
                    assert!(preset.base_url.starts_with("http://127.0.0.1:"));
                }
            }
        }
        assert!(
            plugin_ids.len() >= 20,
            "expected twenty distinct service packages"
        );
    }
}
