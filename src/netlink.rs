use crate::client::Client;
use crate::types::{Destination, NextHop, Vni};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::TryStreamExt;
use ipnet::IpNet;
use netlink_packet_route::{
    route::{RouteAttribute, RouteMessage, RouteProtocol, RouteScope, RouteType},
    AddressFamily,
};
use rtnetlink::{Handle, IpVersion};
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::Mutex;

const METALBOND_RT_PROTO: u8 = 254;

// --- FIX: Moved helper trait before its use ---
// Helper trait to get OIF from RouteMessage
trait RouteMessageExt {
    fn output_interface(&self) -> Option<u32>;
}

impl RouteMessageExt for RouteMessage {
    fn output_interface(&self) -> Option<u32> {
        self.attributes.iter().find_map(|attr| {
            if let RouteAttribute::Oif(index) = attr {
                Some(*index)
            } else {
                None
            }
        })
    }
}
// --------------------------------------------

#[derive(Debug)]
pub struct NetlinkClient {
    handle: Handle,
    config: NetlinkClientConfig,
    tun_index: u32,
    _mutex: Mutex<()>,
}

#[derive(Debug, Clone)]
pub struct NetlinkClientConfig {
    pub vni_table_map: HashMap<Vni, i32>,
    pub link_name: String,
    pub ipv4_only: bool,
}

impl NetlinkClient {
    pub async fn new(config: NetlinkClientConfig) -> Result<Self> {
        let (connection, handle, _) = rtnetlink::new_connection()?;
        tokio::spawn(connection);

        let tun_index = handle
            .link()
            .get()
            .match_name(config.link_name.clone())
            .execute()
            .try_next()
            .await
            .context("Failed to query link")?
            .ok_or_else(|| anyhow!("Cannot find tun device '{}'", config.link_name))?
            .header
            .index;

        tracing::info!("Found tun device '{}' with index {}", config.link_name, tun_index);

        for (&vni, &table_id) in &config.vni_table_map {
            if table_id <= 0 {
                tracing::warn!("Skipping route clearing for VNI {} with non-positive table ID {}", vni, table_id);
                continue;
            }
            tracing::info!("Clearing existing Metalbond routes for VNI {} in table {}", vni, table_id);
            Self::clear_metalbond_routes(&handle, table_id as u32, tun_index).await
                .with_context(|| format!("Failed to clear routes for VNI {} in table {}", vni, table_id))?;
        }

        Ok(NetlinkClient {
            handle,
            config,
            tun_index,
            _mutex: Mutex::new(()),
        })
    }

    async fn clear_metalbond_routes(handle: &Handle, table_id: u32, link_index: u32) -> Result<()> {
        let mut routes_v4 = handle.route().get(IpVersion::V4).execute();
        let mut routes_v6 = handle.route().get(IpVersion::V6).execute();

        while let Some(route) = routes_v4.try_next().await? {
            // FIX: Compare protocol enum correctly
            if route.header.table == (table_id as u8)
                && route.header.protocol == RouteProtocol::from(METALBOND_RT_PROTO) // FIX: Correct comparison
                && route.output_interface() == Some(link_index) // FIX: Ensure trait method available
            {
                let route_to_delete = route.clone(); // Clone route before moving
                handle.route().del(route_to_delete).execute().await.with_context(||"Failed to delete IPv4 route")?;
                tracing::debug!("Cleared IPv4 route: {:?}", route); // Use original route for logging
            }
        }
        while let Some(route) = routes_v6.try_next().await? {
            if route.header.table == (table_id as u8)
                && route.header.protocol == RouteProtocol::from(METALBOND_RT_PROTO) // FIX: Correct comparison
                && route.output_interface() == Some(link_index) // FIX: Ensure trait method available
            {
                let route_to_delete = route.clone(); // Clone route before moving
                handle.route().del(route_to_delete).execute().await.with_context(||"Failed to delete IPv6 route")?;
                tracing::debug!("Cleared IPv6 route: {:?}", route); // Use original route for logging
            }
        }

        Ok(())
    }

    fn build_route_message(
        &self,
        table_id: i32,
        dest: Destination,
        nexthop_ip: IpAddr,
    ) -> Result<RouteMessage> {
        let mut route_message = RouteMessage::default();
        let header = &mut route_message.header;

        match dest.prefix {
            IpNet::V4(pfx) => {
                header.address_family = AddressFamily::Inet;
                header.destination_prefix_length = pfx.prefix_len();
                // FIX: Use .into() for RouteAddress conversion
                route_message
                    .attributes
                    .push(RouteAttribute::Destination(pfx.network().into()));
            }
            IpNet::V6(pfx) => {
                header.address_family = AddressFamily::Inet6;
                header.destination_prefix_length = pfx.prefix_len();
                // FIX: Use .into() for RouteAddress conversion
                route_message
                    .attributes
                    .push(RouteAttribute::Destination(pfx.network().into()));
            }
        }

        header.table = table_id as u8;
        header.protocol = RouteProtocol::from(METALBOND_RT_PROTO); // FIX: Assign enum value
        header.scope = RouteScope::Universe; // FIX: Use imported type
        header.kind = RouteType::Unicast; // FIX: Use RouteType::Unicast for v0.19

        route_message.attributes.push(RouteAttribute::Oif(self.tun_index));

        match nexthop_ip {
            IpAddr::V4(_) => {
                tracing::warn!("Nexthop is IPv4 ({}), ip6tnl usually requires IPv6. Route might not work as expected.", nexthop_ip);
            }
            IpAddr::V6(addr) => {
                // FIX: Use .into() for RouteAddress conversion
                route_message.attributes.push(RouteAttribute::Gateway(addr.into()));
            }
        }

        tracing::debug!("Constructed route message: {:?}", route_message);
        Ok(route_message)
    }
}

#[async_trait]
impl Client for NetlinkClient {
    async fn add_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()> {
        if self.config.ipv4_only && dest.ip_version() != crate::types::IpVersion::V4 {
            tracing::info!(
                "Received non-IPv4 route {}, skipping install (IPv4-only mode)",
                dest
            );
            return Ok(());
        }

        let table_id = *self
            .config
            .vni_table_map
            .get(&vni)
            .ok_or_else(|| anyhow!("No route table ID known for VNI {}", vni))?;

        if table_id <= 0 {
            return Err(anyhow!("Invalid table ID {} configured for VNI {}", table_id, vni));
        }

        if nexthop.hop_type != crate::pb::NextHopType::Standard {
            tracing::warn!("Netlink client currently only supports Standard nexthop type, received {:?}, skipping install for {}", nexthop.hop_type, dest);
            return Ok(());
        }

        let route_message = self.build_route_message(table_id, dest, nexthop.target_address)?;

        tracing::info!("Adding route: VNI {} Dest {} NH {} via dev {} to table {}", vni, dest, nexthop.target_address, self.config.link_name, table_id);

        // FIX: Use context for better error message & correct add() usage for rtnetlink v0.14
        // Get mutable access to the message within the request
        let mut request = self.handle.route().add();
        *request.message_mut() = route_message;
        request.execute()
            .await
            .with_context(|| format!("Failed to add route {} to table {}", dest, table_id))?;

        Ok(())
    }

    async fn remove_route(&self, vni: Vni, dest: Destination, nexthop: NextHop) -> Result<()> {
        if self.config.ipv4_only && dest.ip_version() != crate::types::IpVersion::V4 {
            tracing::debug!(
                "Skipping removal of non-IPv4 route {} (IPv4-only mode)",
                dest
            );
            return Ok(());
        }

        let table_id = *self
            .config
            .vni_table_map
            .get(&vni)
            .ok_or_else(|| anyhow!("No route table ID known for VNI {}", vni))?;

        if table_id <= 0 {
            return Err(anyhow!("Invalid table ID {} configured for VNI {}", table_id, vni));
        }

        if nexthop.hop_type != crate::pb::NextHopType::Standard {
            tracing::warn!("Netlink client currently only supports Standard nexthop type, received {:?}, skipping removal for {}", nexthop.hop_type, dest);
            return Ok(());
        }

        let route_message = self.build_route_message(table_id, dest, nexthop.target_address)?;

        tracing::info!("Removing route: VNI {} Dest {} NH {} via dev {} from table {}", vni, dest, nexthop.target_address, self.config.link_name, table_id);

        // FIX: Use match for clearer error handling, corrected pattern match
        match self.handle.route().del(route_message).execute().await {
            Ok(_) => {
                tracing::debug!("Successfully removed route {}", dest);
                Ok(())
            }
            // FIX: Correct pattern for NetlinkError(ErrorMessage) and code check
            Err(rtnetlink::Error::NetlinkError(err_msg)) => {
                // Compare error code correctly (err_msg.code is Option<NonZero<i32>>)
                if err_msg.code.map_or(false, |c| c.get() == nix::libc::ENOENT) {
                    tracing::debug!("Route {} already removed or never existed (ENOENT).", dest);
                    Ok(()) // Not an error if it's already gone
                } else {
                    // Clone err_msg before moving it into the error context
                    let err_msg_clone = err_msg.clone();
                    // Re-wrap other netlink errors
                    Err(rtnetlink::Error::NetlinkError(err_msg))
                        .with_context(|| format!("Netlink error removing route {} from table {}: {:?}", dest, table_id, err_msg_clone))
                }
            }
            Err(rtnetlink::Error::UnexpectedMessage(msg)) => {
                Err(anyhow!("Unexpected message: {:?}", msg))
            }
            Err(rtnetlink::Error::RequestFailed) => {
                Err(anyhow!("Request failed while removing route {}", dest))
            }
            Err(rtnetlink::Error::NamespaceError(err)) => {
                Err(anyhow!("Namespace error: {:?}", err))
            }
            Err(e) => {
                Err(anyhow!("Unhandled error while removing route {}: {:?}", dest, e))
            }
        }
    }

    async fn clear_routes_for_vni(&self, vni: Vni) -> Result<()> {
        let table_id = *self
            .config
            .vni_table_map
            .get(&vni)
            .ok_or_else(|| anyhow!("No route table ID known for VNI {}", vni))?;

        if table_id <= 0 {
            return Err(anyhow!("Invalid table ID {} configured for VNI {}", table_id, vni));
        }

        tracing::info!("Explicitly clearing Metalbond routes for VNI {} in table {}", vni, table_id);
        Self::clear_metalbond_routes(&self.handle, table_id as u32, self.tun_index).await
            .with_context(|| format!("Failed to clear routes for VNI {} in table {}", vni, table_id))
    }
}

