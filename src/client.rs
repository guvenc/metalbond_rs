use crate::types::{Destination, NextHop, Vni};
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Client: Send + Sync {
    async fn add_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()>;
    async fn remove_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()>;
    // Maybe add a function to clear all routes for a VNI on startup?
    async fn clear_routes_for_vni(&self, vni: Vni) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct DummyClient {}

impl DummyClient {
    pub async fn new() -> Result<Self> {
        tracing::info!("Creating DummyClient (no netlink support)");
        Ok(DummyClient {})
    }
}

#[async_trait]
impl Client for DummyClient {
    async fn add_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()> {
        tracing::debug!(
            "DummyClient: Add route VNI {}, Dest {}, NextHop {}",
            vni,
            dest,
            nexthop
        );
        Ok(())
    }

    async fn remove_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()> {
        tracing::debug!(
            "DummyClient: Remove route VNI {}, Dest {}, NextHop {}",
            vni,
            dest,
            nexthop
        );
        Ok(())
    }

    async fn clear_routes_for_vni(&self, vni: Vni) -> Result<()> {
        tracing::debug!("DummyClient: Clear routes for VNI {}", vni);
        Ok(())
    }
}
