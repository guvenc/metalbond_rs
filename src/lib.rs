pub mod client;
pub mod http_server;
pub mod metalbond;
pub mod pb;
pub mod peer;
pub mod routetable;
pub mod types;

#[cfg(test)]
mod tests;

// On Linux, implicitly consider netlink-support to be enabled, regardless of whether
// the feature was explicitly enabled.
#[cfg(target_os = "linux")]
pub mod netlink;

// On non-Linux systems, only include netlink module if the feature is explicitly enabled
#[cfg(all(not(target_os = "linux"), feature = "netlink-support"))]
pub mod netlink;

// Re-export main components for easier usage
pub use crate::metalbond::MetalBond;
pub use crate::types::{Config, ConnectionState, Destination, NextHop, Vni};

// Re-export DummyClient only when netlink-support is not active
#[cfg(not(any(feature = "netlink-support", target_os = "linux")))]
pub use crate::client::DummyClient;

// On Linux, always export the NetlinkClient
#[cfg(target_os = "linux")]
pub use crate::netlink::{NetlinkClient, NetlinkClientConfig};

// On non-Linux systems, only export NetlinkClient if the feature is explicitly enabled
#[cfg(all(not(target_os = "linux"), feature = "netlink-support"))]
pub use crate::netlink::{NetlinkClient, NetlinkClientConfig};
