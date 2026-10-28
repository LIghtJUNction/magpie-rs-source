use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

pub const DEFAULT_GATEWAY_PORT: u16 = 3425;

pub const fn default_gateway_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, DEFAULT_GATEWAY_PORT))
}
