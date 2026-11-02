//! Application-composed declarative plugins, without Usage runtime or service dependencies.

use magpie_core::{
    agent_connection::AgentDefinition,
    plugins::{PluginInput, PluginKind, PluginRegistry, PluginSource, PluginStatus},
    provider_presets::ProviderPreset,
    provider_store::ProviderProtocol,
};
use serde_json::{Value, json};
use std::path::Path;

mod resources;

struct BundleEntry {
    label: &'static str,
    text: &'static str,
    resources: &'static [(&'static str, &'static str)],
}
include!(concat!(env!("OUT_DIR"), "/bundle.rs"));

/// A host's already implemented handlers and factories, not a promise to execute unknown code.
#[derive(Clone, Debug)]
pub struct FeatureRegistration {
    pub input: PluginInput,
    pub commands: Vec<(String, String)>,
    pub pages: Vec<(String, String)>,
    pub metadata_subscribers: Vec<(String, String)>,
}
/// A contributed preset with immutable registration ownership.
#[derive(Clone, Debug)]
pub struct OwnedProvider {
    pub owner: String,
    pub preset: ProviderPreset,
}
/// A validated Agent definition installed in the generic transaction engine.
#[derive(Clone, Debug)]
pub struct OwnedAgent {
    pub owner: String,
    pub definition: AgentDefinition,
}
/// An explicitly registered native adapter engine supplied by the host.
#[derive(Clone, Debug)]
pub struct OwnedNativeAgent {
    pub owner: String,
    pub id: String,
    pub name: String,
    pub engine: String,
}
/// A host-owned discovery catalog; entries are candidates, not working adapters.
#[derive(Clone, Debug)]
pub struct OwnedDiscovery {
    pub owner: String,
    pub id: String,
    pub engine: String,
    pub entries: u64,
    pub claim: String,
}
/// Installed command metadata; the host owns the handler implementation.
#[derive(Clone, Debug)]
pub struct OwnedCommand {
    pub owner: String,
    pub name: String,
    pub handler: String,
}
/// Installed page metadata; the host owns the renderer.
#[derive(Clone, Debug)]
pub struct OwnedPage {
    pub owner: String,
    pub id: String,
    pub name: String,
    pub command: String,
}
/// Installed event subscription metadata; the host owns its factory and disposal.
#[derive(Clone, Debug)]
pub struct OwnedSubscriber {
    pub owner: String,
    pub id: String,
    pub event: String,
}

/// A fresh registration snapshot. Loading never opens business logs or Agent configurations.
#[derive(Debug)]
pub struct AppPlugins {
    pub registry: PluginRegistry,
    pub providers: Vec<OwnedProvider>,
    pub agents: Vec<OwnedAgent>,
    pub native_agents: Vec<OwnedNativeAgent>,
    pub discovery: Vec<OwnedDiscovery>,
    pub pages: Vec<OwnedPage>,
    pub commands: Vec<OwnedCommand>,
    pub metadata_subscribers: Vec<OwnedSubscriber>,
}
impl AppPlugins {
    /// Loads providers and Agent adapters, with no optional feature backends.
    pub fn load() -> Result<Self, String> {
        Self::load_with_features(&[])
    }
    /// Adds explicitly implemented host features to the pure declarative bundle.
    pub fn load_with_features(features: &[FeatureRegistration]) -> Result<Self, String> {
        let directory =
            PluginRegistry::default_plugin_directory().map_err(|_| "plugin_config_unavailable")?;
        let state =
            PluginRegistry::default_state_path().map_err(|_| "plugin_config_unavailable")?;
        Self::load_at(&directory, &state, features)
    }
    /// Registers engines already implemented and linked by this host, without linking them here.
    pub fn load_with_engines(
        features: &[FeatureRegistration],
        engines: &[String],
    ) -> Result<Self, String> {
        let directory =
            PluginRegistry::default_plugin_directory().map_err(|_| "plugin_config_unavailable")?;
        let state =
            PluginRegistry::default_state_path().map_err(|_| "plugin_config_unavailable")?;
        Self::load_at_with_engines(&directory, &state, features, engines)
    }
    /// Uses explicit absolute paths for isolated hosts and synthetic fixtures.
    pub fn load_at(
        directory: &Path,
        state: &Path,
        features: &[FeatureRegistration],
    ) -> Result<Self, String> {
        Self::load_at_with_engines(directory, state, features, &[])
    }
    /// Explicit paths plus optional native engines, for hosts and synthetic fixtures.
    pub fn load_at_with_engines(
        directory: &Path,
        state: &Path,
        features: &[FeatureRegistration],
        engines: &[String],
    ) -> Result<Self, String> {
        let mut inputs = bundled_manifests();
        inputs.extend(features.iter().map(|f| f.input.clone()));
        let registry = PluginRegistry::load(inputs, directory, state)
            .map_err(|_| "plugin_state_unavailable")?;
        let mut result = Self {
            registry,
            providers: Vec::new(),
            agents: Vec::new(),
            native_agents: Vec::new(),
            discovery: Vec::new(),
            pages: Vec::new(),
            commands: Vec::new(),
            metadata_subscribers: Vec::new(),
        };
        loop {
            let ready: Vec<_> = result
                .registry
                .pending_contributions()
                .iter()
                .map(|c| c.owner.to_owned())
                .collect();
            if ready.is_empty() {
                break;
            }
            let mut progress = false;
            for owner in ready {
                let record = result
                    .registry
                    .record(&owner)
                    .ok_or("plugin_registration_failed")?
                    .clone();
                let native_engine = record
                    .manifest
                    .document()
                    .get("contributions")
                    .and_then(|v| v.get("agents"))
                    .and_then(|v| v.as_array_of_tables())
                    .and_then(|a| a.get(0))
                    .and_then(|t| t.get("engine"))
                    .and_then(|v| v.as_str());
                if record.manifest.kind == PluginKind::AgentAdapter
                    && native_engine.is_some_and(|e| {
                        e != "declarative-config/1"
                            && (!matches!(&record.source, PluginSource::Bundled(_))
                                || !engines.iter().any(|known| known == e))
                    })
                {
                    continue;
                }
                let discovery_engine = record
                    .manifest
                    .document()
                    .get("contributions")
                    .and_then(|v| v.get("discovery"))
                    .and_then(|v| v.as_array_of_tables())
                    .and_then(|a| a.get(0))
                    .and_then(|t| t.get("engine"))
                    .and_then(|v| v.as_str());
                if record.manifest.kind == PluginKind::Feature
                    && !discovery_engine.is_some_and(|e| {
                        matches!(&record.source, PluginSource::Bundled(_))
                            && engines.iter().any(|known| known == e)
                    })
                    && !features.iter().any(|f| {
                        magpie_core::plugins::PluginManifest::parse(&f.input.text)
                            .is_ok_and(|m| m.id == owner)
                    })
                {
                    // An unknown feature has no implemented factories and remains pending.
                    continue;
                }
                match result.register(&owner, &record.manifest, &record.source, features, engines) {
                    Ok(()) => {
                        result
                            .registry
                            .acknowledge_activation(&owner)
                            .map_err(|_| "plugin_registration_failed")?;
                    }
                    Err(()) => {
                        result.withdraw(&owner);
                        result
                            .registry
                            .fail_activation(&owner)
                            .map_err(|_| "plugin_registration_failed")?;
                    }
                }
                progress = true;
            }
            if !progress {
                break;
            }
        }
        Ok(result)
    }
    fn register(
        &mut self,
        owner: &str,
        manifest: &magpie_core::plugins::PluginManifest,
        source: &PluginSource,
        features: &[FeatureRegistration],
        engines: &[String],
    ) -> Result<(), ()> {
        let table = manifest
            .document()
            .get("contributions")
            .and_then(|v| v.as_table())
            .ok_or(())?;
        let allowed: &[&str] = match manifest.kind {
            PluginKind::ProviderPreset => &["providers"],
            PluginKind::AgentAdapter => &["agents"],
            PluginKind::Feature => &["commands", "pages", "metadata_subscribers", "discovery"],
            _ => return Err(()),
        };
        if table.is_empty() || table.iter().any(|(key, _)| !allowed.contains(&key)) {
            return Err(());
        }
        let owner = owner.to_owned();
        if let Some(entries) = table.get("providers") {
            let entries = entries.as_array_of_tables().ok_or(())?;
            if entries.is_empty() || entries.len() > 64 {
                return Err(());
            }
            for entry in entries {
                let preset = ProviderPreset {
                    id: field(entry, "id")?,
                    name: field(entry, "name")?,
                    base_url: field(entry, "base_url")?,
                    source: field(entry, "source")?,
                    protocol: ProviderProtocol::parse(&field(entry, "protocol")?)
                        .map_err(|_| ())?,
                    requires_key: entry
                        .get("requires_key")
                        .and_then(|v| v.as_bool())
                        .ok_or(())?,
                };
                preset.validate().map_err(|_| ())?;
                if self.providers.iter().any(|p| p.preset.id == preset.id) {
                    return Err(());
                }
                self.providers.push(OwnedProvider {
                    owner: owner.clone(),
                    preset,
                });
            }
        }
        if let Some(entries) = table.get("agents") {
            let entries = entries.as_array_of_tables().ok_or(())?;
            if entries.is_empty() || entries.len() > 64 {
                return Err(());
            }
            for entry in entries {
                let engine = field(entry, "engine")?;
                if engine != "declarative-config/1" {
                    if !engines.contains(&engine) {
                        return Err(());
                    }
                    let id = field(entry, "id")?;
                    if self.native_agents.iter().any(|a| a.id == id)
                        || self.agents.iter().any(|a| a.definition.id == id)
                    {
                        return Err(());
                    }
                    self.native_agents.push(OwnedNativeAgent {
                        owner: owner.clone(),
                        id,
                        name: field(entry, "name")?,
                        engine,
                    });
                    continue;
                }
                let id = field(entry, "id")?;
                let path = field(entry, "definition")?;
                let definition = load_definition(source, &path)?;
                if definition.id != id
                    || self.native_agents.iter().any(|a| a.id == id)
                    || self.agents.iter().any(|a| a.definition.id == id)
                {
                    return Err(());
                }
                self.agents.push(OwnedAgent {
                    owner: owner.clone(),
                    definition,
                });
            }
        }
        if let Some(entries) = table.get("discovery") {
            for entry in bounded_tables(entries)? {
                let engine = field(entry, "engine")?;
                let id = field(entry, "id")?;
                if !engines.contains(&engine) || self.discovery.iter().any(|d| d.id == id) {
                    return Err(());
                }
                let entries = entry
                    .get("entries")
                    .and_then(|v| v.as_integer())
                    .filter(|n| (0..=1024).contains(n))
                    .ok_or(())? as u64;
                self.discovery.push(OwnedDiscovery {
                    owner: owner.clone(),
                    id,
                    engine,
                    entries,
                    claim: field(entry, "claim")?,
                });
            }
        }
        if manifest.kind == PluginKind::Feature && table.iter().any(|(key, _)| key != "discovery") {
            let feature = features
                .iter()
                .find(|f| {
                    magpie_core::plugins::PluginManifest::parse(&f.input.text)
                        .is_ok_and(|m| m.id == owner)
                })
                .ok_or(())?;
            if let Some(entries) = table.get("commands") {
                for entry in bounded_tables(entries)? {
                    let name = field(entry, "name")?;
                    let handler = field(entry, "handler")?;
                    if !feature.commands.contains(&(name.clone(), handler.clone()))
                        || self.commands.iter().any(|c| c.name == name)
                    {
                        return Err(());
                    }
                    self.commands.push(OwnedCommand {
                        owner: owner.clone(),
                        name,
                        handler,
                    });
                }
            }
            if let Some(entries) = table.get("pages") {
                for entry in bounded_tables(entries)? {
                    let id = field(entry, "id")?;
                    let command = field(entry, "command")?;
                    let name = field(entry, "name")?;
                    if !feature.pages.contains(&(id.clone(), command.clone()))
                        || self.pages.iter().any(|p| p.id == id)
                    {
                        return Err(());
                    }
                    self.pages.push(OwnedPage {
                        owner: owner.clone(),
                        id,
                        name,
                        command,
                    });
                }
            }
            if let Some(entries) = table.get("metadata_subscribers") {
                for entry in bounded_tables(entries)? {
                    let id = field(entry, "id")?;
                    let event = field(entry, "event")?;
                    if !feature
                        .metadata_subscribers
                        .contains(&(id.clone(), event.clone()))
                        || self.metadata_subscribers.iter().any(|s| s.id == id)
                    {
                        return Err(());
                    }
                    self.metadata_subscribers.push(OwnedSubscriber {
                        owner: owner.clone(),
                        id,
                        event,
                    });
                }
            }
        }
        Ok(())
    }
    fn withdraw(&mut self, owner: &str) {
        self.native_agents.retain(|a| a.owner != owner);
        self.discovery.retain(|d| d.owner != owner);
        self.providers.retain(|p| p.owner != owner);
        self.agents.retain(|a| a.owner != owner);
        self.commands.retain(|c| c.owner != owner);
        self.pages.retain(|p| p.owner != owner);
        self.metadata_subscribers.retain(|s| s.owner != owner);
    }
    /// True only when this fresh snapshot has installed the requested owner's contributions.
    pub fn active(&self, owner: &str) -> bool {
        self.registry
            .record(owner)
            .is_some_and(|r| r.status == PluginStatus::Active)
    }
    /// Serializes the public owner-aware catalog, with no raw manifests or business log reads.
    pub fn to_json(&self) -> Value {
        let plugins: Vec<_> = self.registry.records().iter().map(|r| {
            let waiting = match &r.status { PluginStatus::Pending { waiting_for } => waiting_for.clone(), _ => Vec::new() };
            let error = match &r.status { PluginStatus::Failed(e) => Some(e.to_string()), _ => None };
            json!({"id":r.manifest.id,"owner":r.owner(),"name":r.manifest.name,"version":r.manifest.version,"api_version":r.manifest.api_version,"kind":r.manifest.kind.as_str(),"source":source_json(&r.source),"enabled":r.enabled,"status":r.status.as_str(),"waiting_for":waiting,"error":error})
        }).collect();
        let providers: Vec<_> = self.providers.iter().map(|p| json!({"owner":p.owner,"id":p.preset.id,"name":p.preset.name,"base_url":p.preset.base_url,"protocol":p.preset.protocol.as_str(),"requires_key":p.preset.requires_key,"source":p.preset.source})).collect();
        let mut agents: Vec<_> = self.agents.iter().map(|a| json!({"owner":a.owner,"id":a.definition.id,"name":a.definition.name,"engine":"declarative-config/1","definition":a.definition})).collect();
        agents.extend(self.native_agents.iter().map(|a| json!({"owner":a.owner,"id":a.id,"name":a.name,"engine":a.engine,"definition":null})));
        let discovery: Vec<_> = self.discovery.iter().map(|d| json!({"owner":d.owner,"id":d.id,"engine":d.engine,"entries":d.entries,"claim":d.claim})).collect();
        let pages: Vec<_> = self
            .pages
            .iter()
            .map(|p| json!({"owner":p.owner,"id":p.id,"name":p.name,"command":p.command}))
            .collect();
        let commands: Vec<_> = self
            .commands
            .iter()
            .map(|c| json!({"owner":c.owner,"name":c.name,"handler":c.handler}))
            .collect();
        let subscribers: Vec<_> = self
            .metadata_subscribers
            .iter()
            .map(|s| json!({"owner":s.owner,"id":s.id,"event":s.event}))
            .collect();
        let diagnostics: Vec<_> = self
            .registry
            .diagnostics()
            .iter()
            .map(|d| json!({"source":source_json(&d.source),"error":d.error.to_string()}))
            .collect();
        json!({"version":magpie_core::VERSION,"revision":self.registry.revision(),"plugins":plugins,"contributions":{"providers":providers,"agents":agents,"pages":pages,"commands":commands,"metadata_subscribers":subscribers,"discovery":discovery},"diagnostics":diagnostics})
    }
    /// Validates a declaration even while off, for explicit recovery only, never new work.
    pub fn recovery_agent(&self, owner: &str) -> Result<AgentDefinition, String> {
        let record = self.registry.record(owner).ok_or("plugin_not_found")?;
        if record.manifest.kind != PluginKind::AgentAdapter
            || matches!(record.status, PluginStatus::Failed(_))
        {
            return Err("plugin_not_available".into());
        }
        let entries = record
            .manifest
            .document()
            .get("contributions")
            .and_then(|v| v.get("agents"))
            .and_then(|v| v.as_array_of_tables())
            .ok_or("plugin_invalid_definition")?;
        if entries.len() != 1 {
            return Err("plugin_ambiguous_definition".into());
        }
        let entry = entries.get(0).ok_or("plugin_invalid_definition")?;
        let definition = load_definition(
            &record.source,
            &field(entry, "definition").map_err(|_| "plugin_invalid_definition")?,
        )
        .map_err(|_| "plugin_invalid_definition")?;
        Ok(definition)
    }
}
/// Injected bundled envelopes, deterministically ordered by their source-package path.
pub fn bundled_manifests() -> Vec<PluginInput> {
    BUNDLE
        .iter()
        .map(|b| PluginInput::bundled(b.label, b.text))
        .collect()
}
fn field(table: &toml_edit::Table, key: &str) -> Result<String, ()> {
    let value = table.get(key).and_then(|v| v.as_str()).ok_or(())?;
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(value.to_owned())
}
fn bounded_tables(item: &toml_edit::Item) -> Result<&toml_edit::ArrayOfTables, ()> {
    item.as_array_of_tables()
        .filter(|a| !a.is_empty() && a.len() <= 64)
        .ok_or(())
}
fn load_definition(source: &PluginSource, name: &str) -> Result<AgentDefinition, ()> {
    if name.len() > 128
        || name.is_empty()
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(());
    }
    let text = match source {
        PluginSource::Bundled(label) => BUNDLE
            .iter()
            .find(|b| b.label == label)
            .and_then(|b| b.resources.iter().find(|(n, _)| *n == name))
            .map(|(_, body)| (*body).to_owned())
            .ok_or(())?,
        PluginSource::User(path) => {
            resources::read_text(&path.parent().ok_or(())?.join(name), 64 * 1024, false)?
                .ok_or(())?
        }
    };
    AgentDefinition::from_manifest_json(&text).map_err(|_| ())
}
fn source_json(source: &PluginSource) -> Value {
    match source {
        PluginSource::Bundled(label) => json!({"kind":"bundled","label":label}),
        PluginSource::User(path) => json!({"kind":"user","path":path}),
    }
}

/// Exact definition retained before applying an Agent connection; never includes config bytes.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryBinding {
    pub schema: u32,
    pub owner: String,
    pub definition: AgentDefinition,
    pub targets: Vec<std::path::PathBuf>,
}
fn binding_path(owner: &str) -> Result<std::path::PathBuf, String> {
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err("plugin_invalid_id".into());
    }
    magpie_core::paths::config_dir()
        .map(|p| p.join("plugin-bindings").join(format!("{owner}.json")))
        .map_err(|_| "plugin_config_unavailable".into())
}
fn decode_binding(text: &str, owner: &str) -> Result<RecoveryBinding, ()> {
    let binding: RecoveryBinding = serde_json::from_str(text).map_err(|_| ())?;
    if binding.schema != 1
        || binding.owner != owner
        || binding.targets.len() > 32
        || binding
            .targets
            .iter()
            .any(|p| !p.is_absolute() || p.as_os_str().len() > 4096)
    {
        return Err(());
    }
    binding.definition.validate().map_err(|_| ())?;
    Ok(binding)
}
/// Reads only host-owned metadata, never Agent credentials or configuration contents.
pub fn recovery_binding(owner: &str) -> Result<Option<RecoveryBinding>, String> {
    let text = resources::read_text(&binding_path(owner)?, 256 * 1024, true)
        .map_err(|_| "plugin_recovery_unavailable")?;
    text.map(|s| decode_binding(&s, owner).map_err(|_| "plugin_recovery_unavailable".to_owned()))
        .transpose()
}
/// Pins the original validated definition and target before a potentially mutating operation.
pub fn pin_recovery(
    owner: &str,
    definition: &AgentDefinition,
    target: &Path,
) -> Result<(), String> {
    definition
        .validate()
        .map_err(|_| "plugin_invalid_definition")?;
    if !target.is_absolute() || target.as_os_str().len() > 4096 {
        return Err("plugin_invalid_config_path".into());
    }
    resources::edit_private(&binding_path(owner)?, |previous| {
        let mut binding = match previous { Some(text) => decode_binding(text, owner)?, None => RecoveryBinding { schema: 1, owner: owner.to_owned(), definition: definition.clone(), targets: Vec::new() } };
        if binding.definition != *definition { return Err(()); }
        if !binding.targets.iter().any(|p| p == target) { if binding.targets.len() == 32 { return Err(()); } binding.targets.push(target.to_owned()); }
        serde_json::to_string(&binding).map_err(|_| ())
    }).map_err(|_| "plugin_recovery_conflict: retained Agent definition differs or recovery metadata is unavailable".into())
}
/// Removes a target association only after successful explicit disconnect, retaining the definition.
pub fn release_recovery(owner: &str, target: &Path) -> Result<(), String> {
    resources::edit_private(&binding_path(owner)?, |previous| {
        let Some(previous) = previous else {
            return Err(());
        };
        let mut binding = decode_binding(previous, owner)?;
        binding.targets.retain(|p| p != target);
        serde_json::to_string(&binding).map_err(|_| ())
    })
    .map_err(|_| "plugin_recovery_unavailable".into())
}

/// Explicit targets associated with an installed native engine; no configuration contents.
pub fn native_targets(owner: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let path = binding_path(owner)?.with_extension("native.json");
    let text =
        resources::read_text(&path, 256 * 1024, true).map_err(|_| "plugin_recovery_unavailable")?;
    match text {
        None => Ok(Vec::new()),
        Some(text) => {
            decode_targets(&text, owner).map_err(|_| "plugin_recovery_unavailable".into())
        }
    }
}
fn decode_targets(text: &str, owner: &str) -> Result<Vec<std::path::PathBuf>, ()> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Targets {
        schema: u32,
        owner: String,
        targets: Vec<std::path::PathBuf>,
    }
    let data: Targets = serde_json::from_str(text).map_err(|_| ())?;
    if data.schema != 1
        || data.owner != owner
        || data.targets.len() > 32
        || data
            .targets
            .iter()
            .any(|p| !p.is_absolute() || p.as_os_str().len() > 4096)
    {
        return Err(());
    }
    Ok(data.targets)
}
/// Records an explicit native target before mutation, preserving older target associations.
pub fn pin_native_target(owner: &str, target: &Path) -> Result<(), String> {
    if !target.is_absolute() || target.as_os_str().len() > 4096 {
        return Err("plugin_invalid_config_path".into());
    }
    resources::edit_private(
        &binding_path(owner)?.with_extension("native.json"),
        |previous| {
            let mut targets = previous
                .map(|s| decode_targets(s, owner))
                .transpose()?
                .unwrap_or_default();
            if !targets.iter().any(|p| p == target) {
                if targets.len() == 32 {
                    return Err(());
                }
                targets.push(target.to_owned());
            }
            serde_json::to_string(&json!({"schema":1,"owner":owner,"targets":targets}))
                .map_err(|_| ())
        },
    )
    .map_err(|_| "plugin_recovery_unavailable".into())
}

#[cfg(test)]
mod tests;
