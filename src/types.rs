use crate::pb;
use anyhow::{anyhow, Result};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

pub type Vni = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpVersion {
    V4,
    V6,
}

impl From<pb::IpVersion> for IpVersion {
    fn from(value: pb::IpVersion) -> Self {
        match value {
            pb::IpVersion::IPv4 => IpVersion::V4,
            pb::IpVersion::IPv6 => IpVersion::V6,
        }
    }
}

impl From<IpVersion> for pb::IpVersion {
    fn from(value: IpVersion) -> Self {
        match value {
            IpVersion::V4 => pb::IpVersion::IPv4,
            IpVersion::V6 => pb::IpVersion::IPv6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Destination {
    pub prefix: IpNet,
}

impl Destination {
    pub fn ip_version(&self) -> IpVersion {
        match self.prefix {
            IpNet::V4(_) => IpVersion::V4,
            IpNet::V6(_) => IpVersion::V6,
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.prefix)
    }
}

impl FromStr for Destination {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let prefix = IpNet::from_str(s).map_err(|e| anyhow!("Invalid prefix format: {}", e))?;
        Ok(Destination { prefix })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NextHop {
    pub target_address: IpAddr,
    pub target_vni: u32,
    pub hop_type: pb::NextHopType,
    pub nat_port_range_from: u16,
    pub nat_port_range_to: u16,
}

impl fmt::Display for NextHop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.target_vni != 0 {
            write!(f, "{} (VNI: {})", self.target_address, self.target_vni)
        } else {
            write!(f, "{}", self.target_address)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    HelloSent,
    HelloReceived,
    Established,
    Retry,
    Closed,
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionState::Connecting => write!(f, "CONNECTING"),
            ConnectionState::HelloSent => write!(f, "HELLO_SENT"),
            ConnectionState::HelloReceived => write!(f, "HELLO_RECEIVED"),
            ConnectionState::Established => write!(f, "ESTABLISHED"),
            ConnectionState::Retry => write!(f, "RETRY"),
            ConnectionState::Closed => write!(f, "CLOSED"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 1,
    Keepalive = 2,
    Subscribe = 3,
    Unsubscribe = 4,
    Update = 5,
}

impl TryFrom<u8> for MessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(MessageType::Hello),
            2 => Ok(MessageType::Keepalive),
            3 => Ok(MessageType::Subscribe),
            4 => Ok(MessageType::Unsubscribe),
            5 => Ok(MessageType::Update),
            _ => Err(anyhow!("Invalid message type: {}", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    Add,
    Remove,
}

impl From<pb::Action> for UpdateAction {
    fn from(action: pb::Action) -> Self {
        match action {
            pb::Action::Add => UpdateAction::Add,
            pb::Action::Remove => UpdateAction::Remove,
        }
    }
}
impl From<UpdateAction> for pb::Action {
    fn from(action: UpdateAction) -> Self {
        match action {
            UpdateAction::Add => pb::Action::Add,
            UpdateAction::Remove => pb::Action::Remove,
        }
    }
}

// Configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub keepalive_interval: Duration,
    pub retry_interval_min: Duration,
    pub retry_interval_max: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keepalive_interval: Duration::from_secs(5),
            retry_interval_min: Duration::from_secs(5),
            retry_interval_max: Duration::from_secs(10), // Allow some jitter
        }
    }
}

// Represents an update passed between components
#[derive(Debug, Clone)]
pub struct InternalUpdate {
    pub action: UpdateAction,
    pub vni: Vni,
    pub destination: Destination,
    pub next_hop: NextHop,
    // Optional: Track the source peer for loop prevention etc.
    pub source_peer: Option<SocketAddr>,
}