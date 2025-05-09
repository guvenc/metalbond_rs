use crate::types::{Destination, NextHop, Vni};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

// Key for the inner map: the peer that announced the route.
// None signifies a locally announced route.
type PeerKey = Option<SocketAddr>;

#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    // VNI -> Destination -> NextHop -> Set<PeerKey>
    routes: Arc<RwLock<HashMap<Vni, HashMap<Destination, HashMap<NextHop, HashSet<PeerKey>>>>>>,
}

impl RouteTable {
    pub fn new() -> Self {
        Default::default()
    }

    pub async fn get_vnis(&self) -> Vec<Vni> {
        let routes = self.routes.read().await;
        routes.keys().cloned().collect()
    }

    pub async fn get_destinations_by_vni(&self, vni: Vni) -> HashMap<Destination, Vec<NextHop>> {
        let routes = self.routes.read().await;
        let mut result = HashMap::new();
        if let Some(dest_map) = routes.get(&vni) {
            for (dest, nh_map) in dest_map {
                result.insert(*dest, nh_map.keys().cloned().collect());
            }
        }
        result
    }

    pub async fn get_destinations_by_vni_with_peer(
        &self,
        vni: Vni,
    ) -> HashMap<Destination, HashMap<NextHop, Vec<PeerKey>>> {
        let routes = self.routes.read().await;
        let mut result = HashMap::new();
        if let Some(dest_map) = routes.get(&vni) {
            for (dest, nh_map) in dest_map.iter() {
                let mut hop_peer_map = HashMap::new();
                for (nh, peer_set) in nh_map.iter() {
                    hop_peer_map.insert(*nh, peer_set.iter().cloned().collect());
                }
                result.insert(*dest, hop_peer_map);
            }
        }
        result
    }


    pub async fn get_next_hops_by_destination(
        &self,
        vni: Vni,
        dest: Destination,
    ) -> Vec<NextHop> {
        let routes = self.routes.read().await;
        routes
            .get(&vni)
            .and_then(|dest_map| dest_map.get(&dest))
            .map(|nh_map| nh_map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Adds a nexthop and returns true if it was newly added.
    pub async fn add_next_hop(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> bool {
        let mut routes = self.routes.write().await;
        routes
            .entry(vni)
            .or_default()
            .entry(dest)
            .or_default()
            .entry(nh)
            .or_default()
            .insert(received_from)
    }

    /// Removes a nexthop and returns the number of remaining peers for this exact VNI/Dest/NH tuple.
    /// Returns an error if the path doesn't exist.
    pub async fn remove_next_hop(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> Result<usize> {
        let mut routes = self.routes.write().await;

        let vni_map = routes
            .get_mut(&vni)
            .ok_or_else(|| anyhow::anyhow!("VNI {} not found", vni))?;
        let dest_map = vni_map
            .get_mut(&dest)
            .ok_or_else(|| anyhow::anyhow!("Destination {} not found in VNI {}", dest, vni))?;
        let nh_map = dest_map
            .get_mut(&nh)
            .ok_or_else(|| anyhow::anyhow!("NextHop {} not found for {}", nh, dest))?;

        if !nh_map.remove(&received_from) {
            return Err(anyhow::anyhow!(
                "Peer {:?} not found for NextHop {}",
                received_from,
                nh
            ));
        }

        let remaining_count = nh_map.len();

        // Cleanup empty maps/sets
        if nh_map.is_empty() {
            dest_map.remove(&nh);
        }
        if dest_map.is_empty() {
            vni_map.remove(&dest);
        }
        if vni_map.is_empty() {
            routes.remove(&vni);
        }

        Ok(remaining_count)
    }

    pub async fn next_hop_exists(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> bool {
        let routes = self.routes.read().await;
        routes
            .get(&vni)
            .and_then(|dest_map| dest_map.get(&dest))
            .and_then(|nh_map| nh_map.get(&nh))
            .map_or(false, |peer_set| peer_set.contains(&received_from))
    }

    /// Removes all routes learned from a specific peer.
    pub async fn remove_routes_from_peer(&self, peer: SocketAddr) -> Vec<(Vni, Destination, NextHop)> {
        let mut removed = Vec::new();
        let mut routes = self.routes.write().await;
        let mut empty_vnis = Vec::new();

        for (vni, dest_map) in routes.iter_mut() {
            let mut empty_dests = Vec::new();
            for (dest, nh_map) in dest_map.iter_mut() {
                let mut empty_nhs = Vec::new();
                for (nh, peer_set) in nh_map.iter_mut() {
                    if peer_set.remove(&Some(peer)) {
                        removed.push((*vni, *dest, *nh));
                    }
                    if peer_set.is_empty() {
                        empty_nhs.push(*nh);
                    }
                }
                // Cleanup empty NH maps
                for nh in empty_nhs {
                    nh_map.remove(&nh);
                }
                if nh_map.is_empty() {
                    empty_dests.push(*dest);
                }
            }
            // Cleanup empty Dest maps
            for dest in empty_dests {
                dest_map.remove(&dest);
            }
            if dest_map.is_empty() {
                empty_vnis.push(*vni);
            }
        }
        // Cleanup empty VNI maps
        for vni in empty_vnis {
            routes.remove(&vni);
        }
        removed
    }

    /// Get all routes announced locally (received_from == None)
    pub async fn get_local_announcements(&self) -> HashMap<Vni, HashMap<Destination, Vec<NextHop>>> {
        let routes = self.routes.read().await;
        let mut local_routes = HashMap::new();

        for (vni, dest_map) in routes.iter() {
            let mut vni_local_routes = HashMap::new();
            for (dest, nh_map) in dest_map.iter() {
                let mut nhs = Vec::new();
                for (nh, peer_set) in nh_map.iter() {
                    if peer_set.contains(&None) {
                        nhs.push(*nh);
                    }
                }
                if !nhs.is_empty() {
                    vni_local_routes.insert(*dest, nhs);
                }
            }
            if !vni_local_routes.is_empty() {
                local_routes.insert(*vni, vni_local_routes);
            }
        }
        local_routes
    }
}