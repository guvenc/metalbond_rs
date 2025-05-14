use crate::types::{Destination, NextHop, Vni};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

// Key for the inner map: the peer that announced the route.
// None signifies a locally announced route.
pub type PeerKey = Option<SocketAddr>;

// Commands that can be sent to the RouteTable actor
pub enum RouteTableCommand {
    AddNextHop(Vni, Destination, NextHop, PeerKey, oneshot::Sender<bool>),
    RemoveNextHop(
        Vni,
        Destination,
        NextHop,
        PeerKey,
        oneshot::Sender<Result<usize>>,
    ),
    NextHopExists(Vni, Destination, NextHop, PeerKey, oneshot::Sender<bool>),
    GetVnis(oneshot::Sender<Vec<Vni>>),
    GetDestinationsByVni(Vni, oneshot::Sender<HashMap<Destination, Vec<NextHop>>>),
    GetDestinationsByVniWithPeer(
        Vni,
        oneshot::Sender<HashMap<Destination, HashMap<NextHop, Vec<PeerKey>>>>,
    ),
    GetNextHopsByDestination(Vni, Destination, oneshot::Sender<Vec<NextHop>>),
    RemoveRoutesFromPeer(
        SocketAddr,
        oneshot::Sender<Vec<(Vni, Destination, NextHop)>>,
    ),
    GetLocalAnnouncements(oneshot::Sender<HashMap<Vni, HashMap<Destination, Vec<NextHop>>>>),
}

// Messages that can be broadcast to subscribers
pub enum RouteTableMsg {
    RouteAdded(Vni, Destination, NextHop, PeerKey),
    RouteRemoved(Vni, Destination, NextHop, PeerKey),
}

// The new RouteTable structure
#[derive(Debug, Clone)]
pub struct RouteTable {
    // Command sender to the actor
    cmd_tx: Option<mpsc::Sender<RouteTableCommand>>,
    // For subscription to route changes
    _subscribers: Vec<mpsc::Sender<RouteTableMsg>>,
}

impl Default for RouteTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RouteTable {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(100);
        let route_table = Self {
            cmd_tx: Some(tx),
            _subscribers: Vec::new(),
        };

        // Spawn the actor
        Self::spawn_actor(rx, route_table.clone());

        route_table
    }

    // Create a view-only clone for read access
    pub fn new_view() -> Self {
        Self {
            cmd_tx: None,
            _subscribers: Vec::new(),
        }
    }

    // Spawn the actor that manages the route table state
    pub fn spawn_actor(mut rx: mpsc::Receiver<RouteTableCommand>, view: Self) -> JoinHandle<()> {
        // The actual state, kept private in the actor
        let mut routes: HashMap<Vni, HashMap<Destination, HashMap<NextHop, HashSet<PeerKey>>>> =
            HashMap::new();

        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    RouteTableCommand::AddNextHop(vni, dest, nh, peer, resp) => {
                        let result = routes
                            .entry(vni)
                            .or_default()
                            .entry(dest)
                            .or_default()
                            .entry(nh)
                            .or_default()
                            .insert(peer);
                        let _ = resp.send(result);

                        // Notify subscribers
                        for sub in &view._subscribers {
                            let _ = sub
                                .send(RouteTableMsg::RouteAdded(vni, dest, nh, peer))
                                .await;
                        }
                    }
                    RouteTableCommand::RemoveNextHop(vni, dest, nh, peer, resp) => {
                        let result = match routes.get_mut(&vni) {
                            Some(vni_map) => match vni_map.get_mut(&dest) {
                                Some(dest_map) => match dest_map.get_mut(&nh) {
                                    Some(peer_set) => {
                                        if peer_set.remove(&peer) {
                                            let remaining_peers = peer_set.len();
                                            
                                            // Clean up empty maps but store whether we need to remove
                                            let removing_next_hop = peer_set.is_empty();
                                            if removing_next_hop {
                                                dest_map.remove(&nh);
                                                
                                                // Check if we need to remove the destination too
                                                if dest_map.is_empty() {
                                                    vni_map.remove(&dest);
                                                    
                                                    // Check if we need to remove the VNI too
                                                    if vni_map.is_empty() {
                                                        routes.remove(&vni);
                                                    }
                                                }
                                            }

                                            // Notify subscribers
                                            for sub in &view._subscribers {
                                                let _ = sub
                                                    .send(RouteTableMsg::RouteRemoved(
                                                        vni, dest, nh, peer,
                                                    ))
                                                    .await;
                                            }

                                            Ok(remaining_peers)
                                        } else {
                                            Err(anyhow::anyhow!(
                                                "Peer {:?} not found for NextHop {}",
                                                peer,
                                                nh
                                            ))
                                        }
                                    }
                                    None => Err(anyhow::anyhow!(
                                        "NextHop {} not found for {}",
                                        nh,
                                        dest
                                    )),
                                },
                                None => Err(anyhow::anyhow!(
                                    "Destination {} not found in VNI {}",
                                    dest,
                                    vni
                                )),
                            },
                            None => Err(anyhow::anyhow!("VNI {} not found", vni)),
                        };
                        let _ = resp.send(result);
                    }
                    RouteTableCommand::NextHopExists(vni, dest, nh, peer, resp) => {
                        let exists = routes
                            .get(&vni)
                            .and_then(|vni_map| vni_map.get(&dest))
                            .and_then(|dest_map| dest_map.get(&nh))
                            .is_some_and(|peer_set| peer_set.contains(&peer));
                        let _ = resp.send(exists);
                    }
                    RouteTableCommand::GetVnis(resp) => {
                        let vnis = routes.keys().cloned().collect();
                        let _ = resp.send(vnis);
                    }
                    RouteTableCommand::GetDestinationsByVni(vni, resp) => {
                        let mut result = HashMap::new();
                        if let Some(dest_map) = routes.get(&vni) {
                            for (dest, nh_map) in dest_map {
                                result.insert(*dest, nh_map.keys().cloned().collect());
                            }
                        }
                        let _ = resp.send(result);
                    }
                    RouteTableCommand::GetDestinationsByVniWithPeer(vni, resp) => {
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
                        let _ = resp.send(result);
                    }
                    RouteTableCommand::GetNextHopsByDestination(vni, dest, resp) => {
                        let hops = routes
                            .get(&vni)
                            .and_then(|dest_map| dest_map.get(&dest))
                            .map(|nh_map| nh_map.keys().cloned().collect())
                            .unwrap_or_default();
                        let _ = resp.send(hops);
                    }
                    RouteTableCommand::RemoveRoutesFromPeer(peer, resp) => {
                        let mut removed = Vec::new();
                        let mut empty_vnis = Vec::new();

                        for (vni, dest_map) in routes.iter_mut() {
                            let mut empty_dests = Vec::new();
                            for (dest, nh_map) in dest_map.iter_mut() {
                                let mut empty_nhs = Vec::new();
                                for (nh, peer_set) in nh_map.iter_mut() {
                                    if peer_set.remove(&Some(peer)) {
                                        removed.push((*vni, *dest, *nh));

                                        // Notify subscribers
                                        for sub in &view._subscribers {
                                            let _ = sub
                                                .send(RouteTableMsg::RouteRemoved(
                                                    *vni,
                                                    *dest,
                                                    *nh,
                                                    Some(peer),
                                                ))
                                                .await;
                                        }
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
                        let _ = resp.send(removed);
                    }
                    RouteTableCommand::GetLocalAnnouncements(resp) => {
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
                        let _ = resp.send(local_routes);
                    }
                }
            }
            tracing::info!("RouteTable actor terminated");
        })
    }

    // Public API methods that send commands to the actor
    pub async fn get_vnis(&self) -> Vec<Vni> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx.send(RouteTableCommand::GetVnis(resp_tx)).await.is_ok() {
                return resp_rx.await.unwrap_or_default();
            }
        }
        Vec::new()
    }

    pub async fn get_destinations_by_vni(&self, vni: Vni) -> HashMap<Destination, Vec<NextHop>> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::GetDestinationsByVni(vni, resp_tx))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or_default();
            }
        }
        HashMap::new()
    }

    pub async fn get_destinations_by_vni_with_peer(
        &self,
        vni: Vni,
    ) -> HashMap<Destination, HashMap<NextHop, Vec<PeerKey>>> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::GetDestinationsByVniWithPeer(
                    vni, resp_tx,
                ))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or_default();
            }
        }
        HashMap::new()
    }

    pub async fn get_next_hops_by_destination(&self, vni: Vni, dest: Destination) -> Vec<NextHop> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::GetNextHopsByDestination(
                    vni, dest, resp_tx,
                ))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or_default();
            }
        }
        Vec::new()
    }

    pub async fn add_next_hop(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> bool {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::AddNextHop(
                    vni,
                    dest,
                    nh,
                    received_from,
                    resp_tx,
                ))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or(false);
            }
        }
        false
    }

    pub async fn remove_next_hop(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> Result<usize> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::RemoveNextHop(
                    vni,
                    dest,
                    nh,
                    received_from,
                    resp_tx,
                ))
                .await
                .is_ok()
            {
                return resp_rx
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Response channel closed")));
            }
        }
        Err(anyhow::anyhow!("RouteTable actor not available"))
    }

    pub async fn next_hop_exists(
        &self,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
        received_from: PeerKey,
    ) -> bool {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::NextHopExists(
                    vni,
                    dest,
                    nh,
                    received_from,
                    resp_tx,
                ))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or(false);
            }
        }
        false
    }

    pub async fn remove_routes_from_peer(
        &self,
        peer: SocketAddr,
    ) -> Vec<(Vni, Destination, NextHop)> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::RemoveRoutesFromPeer(peer, resp_tx))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or_default();
            }
        }
        Vec::new()
    }

    pub async fn get_local_announcements(
        &self,
    ) -> HashMap<Vni, HashMap<Destination, Vec<NextHop>>> {
        if let Some(tx) = &self.cmd_tx {
            let (resp_tx, resp_rx) = oneshot::channel();
            if tx
                .send(RouteTableCommand::GetLocalAnnouncements(resp_tx))
                .await
                .is_ok()
            {
                return resp_rx.await.unwrap_or_default();
            }
        }
        HashMap::new()
    }
}
