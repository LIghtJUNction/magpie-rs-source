pub mod catalog;
pub mod credentials;
pub mod gateway;
pub mod paths;
pub mod provider_store;
pub mod routing;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent_connection;
pub mod plugins;
pub mod provider_presets;
pub mod request_metadata;

pub mod config_probe;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
