//! Public, bounded projections of the application plugin registry.
use magpie_core::{agent_connection::AgentDefinition, provider_presets::ProviderPreset};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PluginRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub enabled: bool,
    pub status: String,
    pub source: PluginSource,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PluginSource {
    pub kind: String,
    pub path: Option<String>,
    pub label: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Diagnostic {
    pub source: PluginSource,
    pub error: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct OwnedProvider {
    pub owner: String,
    #[serde(flatten)]
    pub preset: ProviderPresetWire,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ProviderPresetWire {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(deserialize_with = "protocol")]
    pub protocol: magpie_core::provider_store::ProviderProtocol,
    pub requires_key: bool,
    pub source: String,
}
fn protocol<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<magpie_core::provider_store::ProviderProtocol, D::Error> {
    let value = String::deserialize(deserializer)?;
    magpie_core::provider_store::ProviderProtocol::parse(&value)
        .map_err(|_| serde::de::Error::custom("unsupported protocol"))
}
impl ProviderPresetWire {
    pub fn preset(&self) -> ProviderPreset {
        ProviderPreset {
            id: self.id.clone(),
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            protocol: self.protocol,
            requires_key: self.requires_key,
            source: self.source.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct OwnedAgent {
    pub owner: String,
    pub id: String,
    pub name: String,
    pub engine: Option<String>,
    pub definition: Option<AgentDefinition>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PageContribution {
    pub owner: String,
    pub id: String,
    pub name: String,
    pub command: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Contributions {
    pub providers: Vec<OwnedProvider>,
    pub agents: Vec<OwnedAgent>,
    pub pages: Vec<PageContribution>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Catalog {
    pub version: String,
    pub revision: u64,
    pub plugins: Vec<PluginRecord>,
    pub contributions: Contributions,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}
impl Catalog {
    pub fn parse(value: serde_json::Value) -> Result<Self, ()> {
        let catalog: Self = serde_json::from_value(value).map_err(|_| ())?;
        let text = |s: &str, n| !s.is_empty() && s.len() <= n && !s.chars().any(char::is_control);
        let id = |s: &str| {
            text(s, 128)
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        let source = |source: &PluginSource| {
            matches!(source.kind.as_str(), "bundled" | "user")
                && source.path.as_ref().is_none_or(|path| text(path, 4096))
                && source.label.as_ref().is_none_or(|label| text(label, 256))
        };
        if catalog.version != "0.2.0"
            || catalog.plugins.len() > 1024
            || catalog.contributions.providers.len() > 4096
            || catalog.contributions.agents.len() > 1024
            || catalog.contributions.pages.len() > 128
            || catalog.diagnostics.len() > 1024
        {
            return Err(());
        }
        let mut owners = std::collections::BTreeMap::new();
        for record in &catalog.plugins {
            if !id(&record.id)
                || !text(&record.name, 256)
                || !text(&record.version, 64)
                || !id(&record.kind)
                || !id(&record.status)
                || !source(&record.source)
                || owners
                    .insert(&record.id, record.enabled && record.status == "active")
                    .is_some()
            {
                return Err(());
            }
        }
        if catalog
            .diagnostics
            .iter()
            .any(|diagnostic| !source(&diagnostic.source) || !text(&diagnostic.error, 512))
        {
            return Err(());
        }
        let active = |owner: &String| owners.get(owner) == Some(&true);
        let mut preset_ids = std::collections::BTreeSet::new();
        for entry in &catalog.contributions.providers {
            if !active(&entry.owner)
                || entry.preset.preset().validate().is_err()
                || !preset_ids.insert(&entry.preset.id)
            {
                return Err(());
            }
        }
        let mut agent_ids = std::collections::BTreeSet::new();
        for entry in &catalog.contributions.agents {
            if !active(&entry.owner)
                || !id(&entry.id)
                || !text(&entry.name, 256)
                || !agent_ids.insert(&entry.id)
            {
                return Err(());
            }
            if let Some(definition) = &entry.definition {
                if definition.id != entry.id || definition.validate().is_err() {
                    return Err(());
                }
            } else if !entry
                .engine
                .as_ref()
                .is_some_and(|engine| text(engine, 128))
            {
                return Err(());
            }
        }
        let mut page_ids = std::collections::BTreeSet::new();
        for page in &catalog.contributions.pages {
            if !active(&page.owner)
                || !id(&page.id)
                || !text(&page.name, 256)
                || !id(&page.command)
                || !page_ids.insert((&page.owner, &page.id))
            {
                return Err(());
            }
        }
        Ok(catalog)
    }
}
