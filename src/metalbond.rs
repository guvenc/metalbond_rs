use crate::client::Client;
use crate::peer::{handle_peer, Peer};
// Fix: Import PeerKey from routetable (ensure PeerKey is pub in routetable.rs)
use crate::routetable::RouteTable;
use crate::types::{
    Config, ConnectionDirection, ConnectionState, Destination, InternalUpdate, NextHop, Vni,
    UpdateAction, // Import UpdateAction
};
use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use rand::Rng;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration, Instant}; // Import Instant

pub type SharedMetalBondState = Arc<RwLock<MetalBondState>>;

// Shared state for the main application logic
pub struct MetalBondState {
    pub config: Arc<Config>,
    pub client: Arc<dyn Client>, // Underlying OS client (Netlink/Dummy)
    pub route_table: RouteTable, // Routes learned from all peers + local
    pub my_announcements: RouteTable, // Routes announced *by this instance*
    pub my_subscriptions: HashSet<Vni>, // VNIs this instance subscribes to
    pub subscribers: HashMap<Vni, HashSet<SocketAddr>>, // Peers subscribing to specific VNIs from us
    pub peers: HashMap<SocketAddr, Arc<Peer>>, // Active peer connections
    pub is_server: bool,
    // Channel for peers to send updates for distribution
    pub route_update_tx: mpsc::Sender<InternalUpdate>,
    // Signal for graceful shutdown
    pub shutdown_notify: Arc<Notify>,
    // Handles for managed peer tasks (e.g. outgoing connections)
    peer_tasks: HashMap<SocketAddr, JoinHandle<()>>,
    // For server listener task
    listener_task: Option<JoinHandle<()>>,
}

// Main MetalBond application struct
pub struct MetalBond {
    pub state: SharedMetalBondState, // Make state public for http_server
    // Receiver for internal route updates
    route_update_rx: Option<mpsc::Receiver<InternalUpdate>>, // Use Option to take ownership
    // Task handles
    distributor_task: Option<JoinHandle<()>>,
    connection_manager_task: Option<JoinHandle<()>>,
}

impl MetalBond {
    pub fn new(config: Config, client: Arc<dyn Client>, is_server: bool) -> Self {
        let (route_update_tx, route_update_rx) = mpsc::channel(1024); // Buffer size for updates
        let shared_state = Arc::new(RwLock::new(MetalBondState {
            config: Arc::new(config),
            client,
            route_table: RouteTable::new(),
            my_announcements: RouteTable::new(),
            my_subscriptions: HashSet::new(),
            subscribers: HashMap::new(),
            peers: HashMap::new(),
            is_server,
            route_update_tx,
            shutdown_notify: Arc::new(Notify::new()),
            peer_tasks: HashMap::new(),
            listener_task: None,
        }));

        MetalBond {
            state: shared_state,
            route_update_rx: Some(route_update_rx), // Store receiver in Option
            distributor_task: None,
            connection_manager_task: None,
        }
    }

    pub fn start(&mut self) {
        // Start the route distribution task
        let state_clone = Arc::clone(&self.state);
        // Take the receiver from Option
        if let Some(owned_rx) = self.route_update_rx.take() {
            self.distributor_task = Some(tokio::spawn(async move {
                route_distributor_task(state_clone, owned_rx).await;
            }));
        } else {
            // This case should ideally not happen if start is called only once.
            tracing::error!("Route update receiver already taken, cannot start distributor task.");
        }

        // Start connection manager task
        let state_clone_mgr = Arc::clone(&self.state);
        self.connection_manager_task = Some(tokio::spawn(async move {
            connection_manager_task(state_clone_mgr).await;
        }));

        tracing::info!("MetalBond core tasks started");
    }

    pub async fn start_server(&mut self, listen_address: String) -> Result<()> {
        if !self.state.read().await.is_server {
            return Err(anyhow!("Cannot start server in client mode"));
        }

        let listener = TcpListener::bind(&listen_address)
            .await
            .with_context(|| format!("Failed to bind server to {}", listen_address))?;
        tracing::info!("Server listening on {}", listen_address);

        let state_clone = Arc::clone(&self.state);

        let listener_handle = tokio::spawn(async move {
            // FIX for Error 1: Clone the Arc<Notify> to ensure its lifetime for the select macro.
            // The read lock on state_clone is held only for the duration of this clone operation.
            let shutdown_signal_for_select = state_clone.read().await.shutdown_notify.clone();

            loop {
                tokio::select! {
                    // Use the cloned Arc<Notify>. It lives as long as `shutdown_signal_for_select`.
                    _ = shutdown_signal_for_select.notified() => {
                        tracing::info!("Server listener shutting down.");
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, remote_addr)) => {
                                tracing::info!(peer = %remote_addr, "Accepted incoming connection");
                                let state_c = Arc::clone(&state_clone);
                                let config_c = state_c.read().await.config.clone(); // Clone Arc<Config>
                                // Spawn a task to handle this peer
                                tokio::spawn(async move {
                                    // Create Peer instance (Arc)
                                    let (peer_arc, tx_receiver) = Peer::new(
                                        state_c.clone(),
                                        config_c, // Pass cloned Arc<Config>
                                        remote_addr,
                                        ConnectionDirection::Incoming,
                                    );
                                    // Add peer to central state
                                    {
                                        let mut state_w = state_c.write().await;
                                        state_w.peers.insert(remote_addr, Arc::clone(&peer_arc));
                                    }
                                    // Start handling the peer
                                    handle_peer(peer_arc, stream, tx_receiver).await;
                                    // Remove peer from central state after handle_peer finishes/closes
                                    {
                                        tracing::debug!(peer = %remote_addr, "Removing incoming peer from state after task completion");
                                        let mut state_w = state_c.write().await;
                                        state_w.peers.remove(&remote_addr);
                                    }
                                });
                            }
                            Err(e) => {
                                // Fix: Don't use non-existent `is_notified`. Check after notified() fails select.
                                // A simple error log is usually sufficient here, errors during shutdown are expected.
                                tracing::error!("Failed to accept connection: {}", e);
                                // Consider a small delay before retrying accept, maybe only if not shutting down?
                                // For simplicity, just log and continue the loop. If shutdown is triggered,
                                // the .notified() branch will eventually be taken.
                                time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        });
        self.state.write().await.listener_task = Some(listener_handle);

        Ok(())
    }

    // Client function: Add a server to connect to
    pub async fn add_peer(&self, server_addr_str: &str) -> Result<()> {
        let server_addr: SocketAddr = server_addr_str
            .parse()
            .with_context(|| format!("Invalid server address format: {}", server_addr_str))?;

        if self.state.read().await.is_server {
            return Err(anyhow!("Cannot add outgoing peer in server mode"));
        }

        tracing::info!("Adding peer target: {}", server_addr);
        // Connection manager task will pick this up and initiate connection.
        // We just need to store the target address somewhere the manager can see.
        // Let's add a target list to MetalBondState.
        {
            let mut state = self.state.write().await;
            if state.peers.contains_key(&server_addr) || state.peer_tasks.contains_key(&server_addr)
            {
                return Err(anyhow!(
                    "Peer {} already added or connection attempt in progress",
                    server_addr
                ));
            }
            // Add to a list of targets the connection manager should ensure are connected
            // This needs a new field e.g., `target_peers: HashSet<SocketAddr>`
            // state.target_peers.insert(server_addr); // Assuming field exists
            tracing::warn!("add_peer: Need to implement target peer tracking for connection manager (using direct spawn for now)");
            // For now, let's directly spawn a connection attempt here - simpler but less robust retry logic
            let state_clone = Arc::clone(&self.state);
            let handle = tokio::spawn(async move {
                manage_single_outgoing_connection(state_clone, server_addr).await;
            });
            state.peer_tasks.insert(server_addr, handle);
        }

        Ok(())
    }

    pub async fn remove_peer(&self, addr_str: &str) -> Result<()> {
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid peer address format: {}", addr_str))?;
        tracing::info!("Removing peer target: {}", addr);

        let mut state = self.state.write().await;

        // Remove from target list if client mode
        // state.target_peers.remove(&addr); // Assuming field exists

        // Signal the managing task to shutdown
        if let Some(handle) = state.peer_tasks.remove(&addr) {
            handle.abort(); // Forcefully stop the management task
            tracing::debug!("Aborted management task for peer {}", addr);
        }

        // If a connection exists, trigger its shutdown
        if let Some(peer) = state.peers.get(&addr) {
            peer.trigger_shutdown();
            // The handle_peer task should clean up and remove itself from state.peers
            tracing::debug!("Triggered shutdown for active peer connection {}", addr);
        } else {
            tracing::debug!(
                "No active connection found for peer {} to trigger shutdown",
                addr
            );
        }

        // Ensure routes from this peer are removed even if connection wasn't active
        // The cleanup in `handle_peer` relies on the connection having been established.
        // We might need an explicit cleanup here if the task is aborted before connection.
        let removed_routes = state.route_table.remove_routes_from_peer(addr).await;
        if !removed_routes.is_empty() {
            tracing::warn!(peer = %addr, count = removed_routes.len(), "Forcefully removed routes learned from peer on remove_peer call");
            // Need to propagate these removals via the distribution channel if they weren't handled by peer shutdown
            for (vni, dest, nh) in removed_routes {
                let update = InternalUpdate {
                    action: UpdateAction::Remove, // Use imported UpdateAction
                    vni,
                    destination: dest,
                    next_hop: nh,
                    source_peer: Some(addr),
                };
                if let Err(e) = state.route_update_tx.send(update).await {
                    tracing::error!(peer=%addr, "Failed to send route removal for distribution during remove_peer: {}", e);
                }
            }
        }

        Ok(())
    }

    pub async fn get_peer_state(&self, addr_str: &str) -> Result<ConnectionState> {
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid peer address format: {}", addr_str))?;
        let state = self.state.read().await;
        if let Some(peer) = state.peers.get(&addr) {
            Ok(peer.get_state().await)
        } else {
            // Check if we are trying to connect (task exists but peer not yet in map)
            if state.peer_tasks.contains_key(&addr) {
                Ok(ConnectionState::Connecting) // Or Retry? Need better state tracking in manager task
            } else {
                Err(anyhow!("Peer {} not found", addr))
            }
        }
    }

    // Client function: Subscribe to updates for a VNI
    pub async fn subscribe(&self, vni: Vni) -> Result<()> {
        let mut state = self.state.write().await;
        if state.is_server {
            return Err(anyhow!("Cannot subscribe in server mode"));
        }

        if state.my_subscriptions.insert(vni) {
            tracing::info!("Subscribing to VNI {}", vni);
            // Send SUBSCRIBE to all connected peers
            let peers_to_notify: Vec<Arc<Peer>> = state.peers.values().cloned().collect();
            drop(state); // Release lock before network calls

            for peer in peers_to_notify {
                if peer.get_state().await == ConnectionState::Established {
                    if let Err(e) = peer.send_subscribe(vni).await {
                        tracing::error!(peer = %peer.get_remote_addr(), vni = vni, "Failed to send SUBSCRIBE: {}", e);
                        // Optionally trigger peer reset
                    }
                } else {
                    tracing::debug!(peer = %peer.get_remote_addr(), vni = vni, "Peer not established, skipping initial SUBSCRIBE send");
                }
            }

            // Request initial routes for this VNI from the OS client
            self.state
                .read()
                .await
                .client
                .clear_routes_for_vni(vni)
                .await?;
        } else {
            tracing::warn!("Already subscribed to VNI {}", vni);
        }
        Ok(())
    }

    // Client function: Unsubscribe from updates for a VNI
    pub async fn unsubscribe(&self, vni: Vni) -> Result<()> {
        let mut state = self.state.write().await;
        if state.is_server {
            return Err(anyhow!("Cannot unsubscribe in server mode"));
        }

        if state.my_subscriptions.remove(&vni) {
            tracing::info!("Unsubscribing from VNI {}", vni);
            // Send UNSUBSCRIBE to all connected peers
            let peers_to_notify: Vec<Arc<Peer>> = state.peers.values().cloned().collect();
            drop(state); // Release lock before network calls

            for peer in peers_to_notify {
                if peer.get_state().await == ConnectionState::Established {
                    // Check state? Go code doesn't check.
                    if let Err(e) = peer.send_unsubscribe(vni).await {
                        tracing::error!(peer = %peer.get_remote_addr(), vni = vni, "Failed to send UNSUBSCRIBE: {}", e);
                        // Optionally trigger peer reset
                    }
                }
            }
            // Remove routes for this VNI from local system? Go code removes them from peer's receivedRoutes table.
            // Let's clear them via the client interface.
            self.state
                .read()
                .await
                .client
                .clear_routes_for_vni(vni)
                .await?;
        } else {
            tracing::warn!("Not subscribed to VNI {}", vni);
        }
        Ok(())
    }

    pub async fn get_subscribed_vnis(&self) -> Vec<Vni> {
        self.state
            .read()
            .await
            .my_subscriptions
            .iter()
            .cloned()
            .collect()
    }

    pub async fn is_route_announced(&self, vni: Vni, dest: Destination, nh: NextHop) -> bool {
        self.state
            .read()
            .await
            .my_announcements
            .next_hop_exists(vni, dest, nh, None)
            .await
    }

    // Announce a route locally
    pub async fn announce_route(&self, vni: Vni, dest: Destination, nh: NextHop) -> Result<()> {
        tracing::info!("Announcing route: VNI {} Dest {} NH {}", vni, dest, nh);
        let state = self.state.write().await;

        // Add to local announcements table (source peer is None)
        if !state.my_announcements.add_next_hop(vni, dest, nh, None).await {
            tracing::warn!(
                "Route VNI {} Dest {} NH {} already announced locally",
                vni,
                dest,
                nh
            );
            // return Err(anyhow!("Route already announced locally")); // Go code doesn't return error here
        }

        // Send update to central distributor task
        let update = InternalUpdate {
            action: UpdateAction::Add, // Use imported UpdateAction
            vni,
            destination: dest,
            next_hop: nh,
            source_peer: None, // Local announcement
        };
        state
            .route_update_tx
            .send(update)
            .await
            .map_err(|e| anyhow!("Failed to send local announcement for distribution: {}", e))?;

        Ok(())
    }

    // Withdraw a locally announced route
    pub async fn withdraw_route(&self, vni: Vni, dest: Destination, nh: NextHop) -> Result<()> {
        tracing::info!("Withdrawing route: VNI {} Dest {} NH {}", vni, dest, nh);
        let state = self.state.write().await;

        // Remove from local announcements table
        match state.my_announcements.remove_next_hop(vni, dest, nh, None).await {
            Ok(remaining) => {
                // Send update to central distributor task only if this was the last instance of the local announcement (should always be 0 if it existed)
                if remaining == 0 {
                    let update = InternalUpdate {
                        action: UpdateAction::Remove, // Use imported UpdateAction
                        vni,
                        destination: dest,
                        next_hop: nh,
                        source_peer: None, // Local withdrawal
                    };
                    state
                        .route_update_tx
                        .send(update)
                        .await
                        .map_err(|e| {
                            anyhow!("Failed to send local withdrawal for distribution: {}", e)
                        })?;
                } else {
                    tracing::warn!("Route VNI {} Dest {} NH {} had unexpected remaining count {} after local withdrawal", vni, dest, nh, remaining);
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to remove local announcement (maybe not announced?): {}",
                    e
                );
                // Don't propagate removal if it wasn't found locally
                // Err(anyhow!("Cannot withdraw route: {}", e)) // Go returns error here
                Ok(()) // Match Go's behaviour more closely - don't error if not found
            }
        }
    }

    // Changed to &mut self
    pub async fn shutdown(&mut self) {
        tracing::info!("Shutting down MetalBond...");

        // Notify all tasks
        self.state.read().await.shutdown_notify.notify_waiters();

        // Shutdown server listener explicitly if running
        if let Some(handle) = self.state.write().await.listener_task.take() {
            tracing::debug!("Waiting for server listener task to complete...");
            let _ = handle.await;
            tracing::debug!("Server listener task finished.");
        }

        // Shutdown connection manager task
        // Access task handle via self, not self.state
        if let Some(handle) = self.connection_manager_task.take() {
            tracing::debug!("Waiting for connection manager task to complete...");
            let _ = handle.await;
            tracing::debug!("Connection manager task finished.");
        }

        // Abort and wait for managed peer tasks (created by add_peer/connection_manager)
        let peer_handles: Vec<JoinHandle<()>> = self
            .state
            .write()
            .await
            .peer_tasks
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        tracing::debug!("Aborting {} managed peer tasks...", peer_handles.len());
        for handle in &peer_handles {
            handle.abort();
        }
        join_all(peer_handles).await;
        tracing::debug!("Managed peer tasks finished.");

        // Shutdown active peer connections (managed by handle_peer) - rely on shutdown_notify
        let active_peers: Vec<Arc<Peer>> = self.state.read().await.peers.values().cloned().collect();
        tracing::debug!(
            "Signalling {} active peer connections to shutdown...",
            active_peers.len()
        );
        for peer in active_peers {
            peer.trigger_shutdown();
        }
        // Give some time for handle_peer tasks to exit gracefully? Or wait on their handles?
        // handle_peer should remove itself from state.peers when done. Wait until empty?
        let shutdown_deadline = Instant::now() + Duration::from_secs(5); // Use imported Instant
        loop {
            if self.state.read().await.peers.is_empty() {
                tracing::debug!("All active peer connections closed.");
                break;
            }
            if Instant::now() > shutdown_deadline { // Use imported Instant
                tracing::warn!("Timeout waiting for active peer connections to close.");
                break;
            }
            time::sleep(Duration::from_millis(100)).await;
        }

        // Wait for distributor task
        // Access task handle via self, requires &mut self
        if let Some(handle) = self.distributor_task.take() {
            tracing::debug!("Waiting for distributor task...");
            let _ = handle.await;
            tracing::debug!("Distributor task finished.");
        }

        tracing::info!("MetalBond shutdown complete.");
    }
}

// Task responsible for distributing route updates to subscribed peers
async fn route_distributor_task(
    state: SharedMetalBondState,
    mut rx: mpsc::Receiver<InternalUpdate>,
) {
    tracing::info!("Route distributor task started.");
    let shutdown = state.read().await.shutdown_notify.clone();

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("Route distributor task shutting down.");
                break;
            }
            Some(update) = rx.recv() => {
                tracing::debug!(?update, "Distributor received update");
                let mb_state = state.read().await; // Read lock

                // Apply update to OS client first
                match update.action {
                    UpdateAction::Add => { // Use imported UpdateAction
                        if let Err(e) = mb_state.client.add_route(update.vni, update.destination, update.next_hop).await {
                            tracing::error!("Client.add_route failed: {}", e);
                            // Continue distribution? Or stop? Go logs error and continues.
                        }
                    }
                    UpdateAction::Remove => { // Use imported UpdateAction
                        if let Err(e) = mb_state.client.remove_route(update.vni, update.destination, update.next_hop).await {
                            tracing::error!("Client.remove_route failed: {}", e);
                            // Continue distribution? Go logs error and continues.
                        }
                    }
                }

                // Distribute to subscribed peers
                if let Some(subscribers) = mb_state.subscribers.get(&update.vni) {
                    let peers_to_notify: Vec<Arc<Peer>> = subscribers
                        .iter()
                        .filter_map(|addr| mb_state.peers.get(addr)) // Get Arc<Peer> if connected
                        .cloned()
                        .collect();

                    tracing::debug!(vni = update.vni, dest = %update.destination, nh = %update.next_hop, count = peers_to_notify.len(), "Distributing update to subscribers");

                    for peer_arc in peers_to_notify { // Renamed peer to peer_arc for clarity
                        // Don't send back to the source peer (loop prevention)
                        if Some(peer_arc.get_remote_addr()) == update.source_peer {
                            // Special case from Go: only skip if NAT type?
                            if update.next_hop.hop_type == crate::pb::NextHopType::Nat {
                                tracing::trace!(peer = %peer_arc.get_remote_addr(), "Skipping send (source peer loop, NAT type)");
                                continue;
                            }
                        }

                        // Server-to-server distribution check (Go: fromPeer.isServer && p.isServer)
                        // FIX for Error 2 (and assuming Peer.is_remote_server: RwLock<bool>)
                        let source_is_server = match update.source_peer {
                            Some(src_addr) => {
                                if let Some(source_peer_ref) = mb_state.peers.get(&src_addr) {
                                    // source_peer_ref is &Arc<Peer>
                                    // Assuming Peer has `pub is_remote_server: RwLock<bool>`
                                    *source_peer_ref.is_remote_server.read().await
                                } else {
                                    false // Peer not found in map, default to false
                                }
                            }
                            None => mb_state.is_server, // Use local server status
                        };

                        // FIX for Error 3 (and assuming Peer.is_remote_server: RwLock<bool>)
                        // peer_arc is Arc<Peer>
                        let target_is_server = *peer_arc.is_remote_server.read().await;


                        if source_is_server && target_is_server {
                            tracing::trace!(peer = %peer_arc.get_remote_addr(), "Skipping send (server-to-server)");
                            continue;
                        }

                        // Note: E0382 (use after move) might appear here depending on IDE analysis,
                        // but shouldn't occur at compile time as UpdateAction/Vni/Destination/NextHop are Copy.
                        if let Err(e) = peer_arc.send_update(update.action, update.vni, update.destination, update.next_hop).await {
                            tracing::warn!(peer = %peer_arc.get_remote_addr(), "Failed to send update during distribution: {}", e);
                            // Optionally trigger peer reset here if sending fails
                        }
                    }
                } else {
                    tracing::trace!(vni = update.vni, "No subscribers for VNI, skipping distribution");
                }
            }
            else => {
                tracing::info!("Route distributor channel closed.");
                break; // Channel closed
            }
        }
    }
    tracing::info!("Route distributor task finished.");
}

// Task to manage outgoing connections (retries etc.)
async fn connection_manager_task(state: SharedMetalBondState) {
    tracing::info!("Connection manager task started.");
    let shutdown = state.read().await.shutdown_notify.clone();
    // TODO: This task needs access to the list of target peers added via `add_peer`.
    // It should periodically check `state.peers` and `state.peer_tasks` against `state.target_peers`.
    // If a target peer is not connected and no task is running, spawn `manage_single_outgoing_connection`.
    // If a task handle exists in `peer_tasks` but the task finished (e.g. connection failed), remove handle and retry.

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("Connection manager task shutting down.");
                break;
            }
            _ = time::sleep(Duration::from_secs(15)) => { // Check periodically
                // TODO: Implement connection management logic here
                tracing::trace!("Connection manager tick (TODO: Implement logic)");
                // Pseudocode:
                // let mut state_w = state.write().await;
                // let targets = state_w.target_peers.clone(); // Need target_peers field
                // for target_addr in targets {
                //     if !state_w.peers.contains_key(&target_addr) && !state_w.peer_tasks.contains_key(&target_addr) {
                //         tracing::info!(peer = %target_addr, "Target peer not connected, initiating connection task.");
                //         let state_clone = Arc::clone(&state);
                //         let handle = tokio::spawn(async move {
                //             manage_single_outgoing_connection(state_clone, target_addr).await;
                //         });
                //         state_w.peer_tasks.insert(target_addr, handle);
                //     } else if let Some(handle) = state_w.peer_tasks.get(&target_addr) {
                //         if handle.is_finished() {
                //             tracing::warn!(peer = %target_addr, "Connection task finished unexpectedly, removing handle for retry.");
                //             state_w.peer_tasks.remove(&target_addr);
                //         }
                //     }
                // }
            }
        }
    }
    tracing::info!("Connection manager task finished.");
}

// Manages connection lifecycle for a single outgoing peer target
async fn manage_single_outgoing_connection(state: SharedMetalBondState, target_addr: SocketAddr) {
    let config = state.read().await.config.clone();
    let shutdown = state.read().await.shutdown_notify.clone();

    loop {
        tracing::info!(peer = %target_addr, "Attempting to connect...");
        let connect_future = TcpStream::connect(target_addr);

        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!(peer = %target_addr, "Shutdown signal received during connection attempt.");
                break; // Exit management task
            }
            result = connect_future => {
                match result {
                    Ok(stream) => {
                        tracing::info!(peer = %target_addr, local = %stream.local_addr().map_or("?".to_string(), |a| a.to_string()), "Successfully connected");
                        let (peer_arc, tx_receiver) = Peer::new(
                            state.clone(),
                            config.clone(),
                            target_addr,
                            ConnectionDirection::Outgoing,
                        );

                        // Add to active peers
                        {
                            state.write().await.peers.insert(target_addr, Arc::clone(&peer_arc));
                        }

                        // Start handling the peer connection
                        // We need to know when handle_peer exits to trigger retry
                        handle_peer(peer_arc, stream, tx_receiver).await;

                        // `handle_peer` exited, meaning connection closed or error occurred
                        tracing::info!(peer = %target_addr, "Peer connection handler finished.");

                        // Remove from active peers if it wasn't already removed by cleanup
                        {
                            state.write().await.peers.remove(&target_addr);
                        }

                        // Decide whether to retry
                        // Refactor await outside closure
                        let maybe_peer = state.read().await.peers.get(&target_addr).cloned();
                        let current_state = if let Some(p) = maybe_peer {
                            Some(p.get_state().await)
                        } else {
                            None
                        };

                        // Go code sets state to RETRY. We simulate this by looping.
                        if current_state != Some(ConnectionState::Closed) { // Don't retry if explicitly closed
                            let retry_delay = Duration::from_secs(
                                rand::thread_rng().gen_range(config.retry_interval_min.as_secs()..=config.retry_interval_max.as_secs())
                            );
                            tracing::info!(peer = %target_addr, ?retry_delay, "Connection closed/failed, retrying after delay...");

                            // Wait for retry delay, but also listen for shutdown
                            tokio::select! {
                                _ = shutdown.notified() => {
                                    tracing::info!(peer = %target_addr, "Shutdown signal received during retry delay.");
                                    break; // Exit management task
                                }
                                _ = time::sleep(retry_delay) => {
                                    // Continue loop to retry connection
                                }
                            }
                        } else {
                            tracing::info!(peer = %target_addr, "Peer state is Closed, not retrying.");
                            break; // Explicitly closed, stop managing
                        }
                    }
                    Err(e) => {
                        tracing::warn!(peer = %target_addr, "Connection failed: {}", e);
                        // Wait and retry, similar to above
                        let retry_delay = Duration::from_secs(
                            rand::thread_rng().gen_range(config.retry_interval_min.as_secs()..=config.retry_interval_max.as_secs())
                        );
                        tracing::info!(peer = %target_addr, ?retry_delay, "Retrying connection after delay...");
                        tokio::select! {
                            _ = shutdown.notified() => {
                                tracing::info!(peer = %target_addr, "Shutdown signal received during retry delay.");
                                break; // Exit management task
                            }
                            _ = time::sleep(retry_delay) => {
                                // Continue loop to retry connection
                            }
                        }
                    }
                }
            }
        }
    }
    // Task finished (shutdown or explicit stop)
    tracing::info!(peer = %target_addr, "Stopping connection management task.");
    // Remove own handle from state.peer_tasks
    state.write().await.peer_tasks.remove(&target_addr);
}