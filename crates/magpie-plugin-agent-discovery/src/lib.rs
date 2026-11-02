//! Optional legacy requirement catalog and bounded filename discovery.
//! Catalog entries are discovery evidence, not 68 working adapters.
pub mod agent_catalog;
pub mod agents;

pub const PLUGIN_ID: &str = "feature.agent-discovery";
pub const ENGINE: &str = "agent-discovery/1";
pub const MANIFEST: &str = include_str!("../../../plugins/features/agent-discovery/plugin.toml");
