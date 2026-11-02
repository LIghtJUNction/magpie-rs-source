//! Optional native Codex configuration/discovery adapter and recovery implementation.
pub mod agents;
pub mod codex_config;
pub mod codex_transaction;
pub mod command;

pub const PLUGIN_ID: &str = "agent.codex";
pub const ENGINE: &str = "native-codex/1";
pub const MANIFEST: &str = include_str!("../../../plugins/agents/codex/plugin.toml");
