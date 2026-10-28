//! Hosts the local Gateway and its bounded request workers.

pub mod forward;
mod gateway;
pub mod protocol;

pub use gateway::{GatewayLimits, GatewayServer};
pub use magpie_core::gateway::{DEFAULT_GATEWAY_PORT, default_gateway_addr};
