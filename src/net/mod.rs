pub mod client;
pub mod credentials;
pub mod log;
pub mod protocol;
pub mod server;
mod tls;

pub use client::{
    DiscoveredServer, RemoteEvent, RemoteHandle, browse_mdns, cache_is_complete, cache_path,
    connect, connect_once,
};
pub use protocol::DEFAULT_PORT;
pub use server::{ServeOptions, run as run_server};

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

/// Shared QUIC transport. Idle timeout is disabled so a library session can
/// sit quiet between tracks. Keep-alives stay off.
pub fn quic_transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    Arc::new(transport)
}

/// `0.0.0.0` / `::` is a listen address, not a destination. Dial localhost instead.
pub fn normalize_server_addr(address: SocketAddr) -> SocketAddr {
    if !address.ip().is_unspecified() {
        return address;
    }
    if address.is_ipv6() {
        SocketAddr::from((Ipv6Addr::LOCALHOST, address.port()))
    } else {
        SocketAddr::from((Ipv4Addr::LOCALHOST, address.port()))
    }
}

pub fn client_bind_addr(destination: SocketAddr) -> SocketAddr {
    if destination.is_ipv6() {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    }
}
