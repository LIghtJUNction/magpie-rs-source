//! Hosts the local Gateway and its bounded request workers.

mod gateway;

pub use gateway::{GatewayLimits, GatewayServer};
pub use magpie_core::gateway::{DEFAULT_GATEWAY_PORT, default_gateway_addr};
