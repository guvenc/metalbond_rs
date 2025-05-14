pub mod client;
pub mod http_server;
pub mod metalbond;
pub mod pb;
pub mod peer;
pub mod routetable;
pub mod types;

#[cfg(test)]
mod tests;

// Conditional export of netlink module based on feature flag
#[cfg(any(feature = "netlink-support", target_os = "linux"))]
pub mod netlink;

// Re-export main components for easier usage
pub use crate::metalbond::MetalBond;
pub use crate::types::{Config, ConnectionState, Destination, NextHop, Vni};

// A dummy client for platforms without netlink support
#[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
pub use crate::client::DummyClient as DefaultNetworkClient;

// Export NetlinkClient as the default network client on Linux
#[cfg(any(feature = "netlink-support", target_os = "linux"))]
pub use crate::netlink::NetlinkClient as DefaultNetworkClient;
#[cfg(any(feature = "netlink-support", target_os = "linux"))]
pub use crate::netlink::{NetlinkClient, NetlinkClientConfig};
