//! Hosts the local Gateway and its bounded request workers.

pub mod forward;
mod gateway;
mod local_access;
pub mod protocol;
pub mod provider_catalog;

pub use gateway::{GatewayLimits, GatewayServer};
pub use magpie_core::gateway::{DEFAULT_GATEWAY_PORT, default_gateway_addr};
