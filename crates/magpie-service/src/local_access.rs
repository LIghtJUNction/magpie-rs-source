//! The loopback exemption applies only to a direct request to this listener.

use std::net::SocketAddr;

pub(crate) fn valid_local_host(host: &str, listener: SocketAddr) -> bool {
    host.parse::<SocketAddr>().ok() == Some(listener)
}

pub(crate) fn untrusted_browser_or_proxy_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("origin")
        || name.eq_ignore_ascii_case("forwarded")
        || name
            .get(..12)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("x-forwarded-"))
}
