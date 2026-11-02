//! Payload-free request lifecycle events shared by the Gateway and optional plugins.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

/// Normalized optional counts. Unknown values remain absent; snapshots are cumulative.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

/// A final protocol outcome, independent of whether the client received every byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Completed,
    Failed,
    Cancelled,
}

/// One final, schema-versioned observation. No bodies, headers or credentials are allowed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMetadata {
    pub schema: u8,
    pub id: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub provider_id: String,
    pub model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub upstream_protocol: String,
    pub stream: bool,
    pub outcome: RequestOutcome,
    pub usage: Option<TokenUsage>,
}

/// The application supplies a host that dispatches only to active plugin owners.
/// Implementations must enqueue bounded work and must never block request completion.
pub trait MetadataSubscriber: Send + Sync {
    /// Receives exactly one final event per admitted upstream request.
    fn on_request_finished(&self, event: &RequestMetadata);
    /// Returns payload-free operational diagnostics, not the plugin's stored data.
    fn health(&self) -> Value {
        Value::Null
    }
    /// Waits for accepted events after all request tasks have been dropped.
    ///
    /// # Errors
    /// Returns a static operational error if draining cannot be confirmed.
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}
