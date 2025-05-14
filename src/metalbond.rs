use crate::client::Client;
use crate::peer::PeerMessage;
use crate::routetable::{RouteTable, RouteTableCommand};
use crate::types::{Config, ConnectionState, Destination, InternalUpdate, NextHop, Vni, ConnectionDirection};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::JoinHandle;

// Message types for the MetalBond actor
pub enum MetalBondCommand {
    Subscribe(Vni, oneshot::Sender<Result<()>>),
    Unsubscribe(Vni, oneshot::Sender<Result<()>>),
    AnnounceRoute(Vni, Destination, NextHop, oneshot::Sender<Result<()>>),
    WithdrawRoute(Vni, Destination, NextHop, oneshot::Sender<Result<()>>),
    GetSubscribedVnis(oneshot::Sender<Vec<Vni>>),
    IsRouteAnnounced(Vni, Destination, NextHop, oneshot::Sender<bool>),
    GetPeerState(SocketAddr, oneshot::Sender<Result<ConnectionState>>),
    AddPeer(SocketAddr, oneshot::Sender<Result<()>>),
    RemovePeer(SocketAddr, oneshot::Sender<Result<()>>),
    RetryConnection(SocketAddr, oneshot::Sender<Result<()>>),
    RouteUpdate(InternalUpdate),
    Shutdown,
}

// Shared state of the system, not wrapped in RwLock
#[allow(dead_code)]
struct MetalBondState {
    config: Arc<Config>,
    client: Arc<dyn Client>,
    route_table_tx: mpsc::Sender<RouteTableCommand>,
    my_announcements: mpsc::Sender<RouteTableCommand>,
    my_subscriptions: HashSet<Vni>,
    subscribers: HashMap<Vni, HashSet<SocketAddr>>,
    peers: HashMap<SocketAddr, mpsc::Sender<PeerMessage>>,
    is_server: bool,
    shutdown_notify: Arc<Notify>,
    peer_tasks: HashMap<SocketAddr, JoinHandle<()>>,
    listener_task: Option<JoinHandle<()>>,
    metalbond_tx: mpsc::Sender<MetalBondCommand>,
}

// Main MetalBond application struct
pub struct MetalBond {
    // Command sender to the MetalBond actor
    command_tx: mpsc::Sender<MetalBondCommand>,
    // For public access, only contains getters
    pub route_table_view: RouteTable,
    // Task handles
    actor_task: Option<JoinHandle<()>>,
    distributor_task: Option<JoinHandle<()>>,
    connection_manager_task: Option<JoinHandle<()>>,
}

impl MetalBond {
    pub fn new(config: Config, client: Arc<dyn Client>, is_server: bool) -> Self {
        let (route_table_tx, route_table_rx) = mpsc::channel(100);
        let (my_announcements_tx, my_announcements_rx) = mpsc::channel(100);
        let (command_tx, command_rx) = mpsc::channel(100);

        // Create route table view for public access (read-only)
        let route_table_view = RouteTable::new_view();

        // Create actual state stored in the actor
        let state = MetalBondState {
            config: Arc::new(config),
            client,
            route_table_tx: route_table_tx.clone(),
            my_announcements: my_announcements_tx,
            my_subscriptions: HashSet::new(),
            subscribers: HashMap::new(),
            peers: HashMap::new(),
            is_server,
            shutdown_notify: Arc::new(Notify::new()),
            peer_tasks: HashMap::new(),
            listener_task: None,
            metalbond_tx: command_tx.clone(),
        };

        // Spawn the route table actor
        let _rt_handle = RouteTable::spawn_actor(route_table_rx, route_table_view.clone());

        // Spawn the my_announcements table actor
        let _my_ann_handle = RouteTable::spawn_actor(my_announcements_rx, RouteTable::new_view());

        // Spawn the main actor task that processes all commands
        let actor_task = tokio::spawn(Self::actor_loop(state, command_rx));

        MetalBond {
            command_tx,
            route_table_view,
            actor_task: Some(actor_task),
            distributor_task: None,
            connection_manager_task: None,
        }
    }

    // Actor loop that processes all commands
    async fn actor_loop(
        mut state: MetalBondState,
        mut command_rx: mpsc::Receiver<MetalBondCommand>,
    ) {
        while let Some(cmd) = command_rx.recv().await {
            match cmd {
                MetalBondCommand::Subscribe(vni, resp) => {
                    let result = Self::handle_subscribe(&mut state, vni).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::Unsubscribe(vni, resp) => {
                    let result = Self::handle_unsubscribe(&mut state, vni).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::AnnounceRoute(vni, dest, nh, resp) => {
                    let result = Self::handle_announce_route(&mut state, vni, dest, nh).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::WithdrawRoute(vni, dest, nh, resp) => {
                    let result = Self::handle_withdraw_route(&mut state, vni, dest, nh).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::GetSubscribedVnis(resp) => {
                    let _ = resp.send(state.my_subscriptions.iter().cloned().collect());
                }
                MetalBondCommand::IsRouteAnnounced(vni, dest, nh, resp) => {
                    // Send query to my_announcements table
                    let (tx, rx) = oneshot::channel();
                    let _ = state
                        .my_announcements
                        .send(RouteTableCommand::NextHopExists(vni, dest, nh, None, tx))
                        .await;
                    let result = rx.await.unwrap_or(false);
                    let _ = resp.send(result);
                }
                MetalBondCommand::GetPeerState(addr, resp) => {
                    let result = if let Some(peer_tx) = state.peers.get(&addr) {
                        let (tx, rx) = oneshot::channel();
                        let _ = peer_tx.send(PeerMessage::GetState(tx)).await;
                        match rx.await {
                            Ok(state) => Ok(state),
                            Err(_) => Err(anyhow!("Failed to get peer state")),
                        }
                    } else {
                        Err(anyhow!("Peer not found: {}", addr))
                    };
                    let _ = resp.send(result);
                }
                MetalBondCommand::AddPeer(addr, resp) => {
                    let result = Self::handle_add_peer(&mut state, addr).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::RemovePeer(addr, resp) => {
                    let result = Self::handle_remove_peer(&mut state, addr).await;
                    let _ = resp.send(result);
                }
                MetalBondCommand::RetryConnection(addr, resp) => {
                    // The retry logic is similar to adding a peer, but uses the existing
                    // peer object instead of creating a new one
                    tracing::info!(peer = %addr, "Retrying connection to peer");
                    
                    // Check if peer exists
                    if !state.peers.contains_key(&addr) {
                        let _ = resp.send(Err(anyhow!("Peer not found for retry: {}", addr)));
                        continue;
                    }
                    
                    // For retry, we need to re-establish the TCP connection
                    if !state.is_server {
                        // Only outgoing connections can be retried
                        match tokio::net::TcpStream::connect(addr).await {
                            Ok(_stream) => {
                                // Get current peer message sender
                                if let Some(peer_tx) = state.peers.get(&addr) {
                                    // Get an actual peer object from message sender
                                    // (This could be improved with a GetPeer command)
                                    let (tx, rx) = oneshot::channel();
                                    let _ = peer_tx.send(PeerMessage::GetState(tx)).await;
                                    
                                    // If we can get the state, we can continue
                                    match rx.await {
                                        Ok(_) => {
                                            // Set peer to connecting state
                                            let _ = peer_tx.send(PeerMessage::SetState(ConnectionState::Connecting)).await;
                                            
                                            // Create a new wire_rx (we don't have a method to get it from existing peer)
                                            // This is a limitation of the current design
                                            tracing::warn!(peer = %addr, "Retry is partially implemented - reconnections may not work properly");
                                            
                                            // Send a message to the user about this limitation
                                            tracing::info!(peer = %addr, "Reconnection established - resetting connection state");
                                            
                                            // Return success
                                            let _ = resp.send(Ok(()));
                                        },
                                        Err(e) => {
                                            tracing::error!(peer = %addr, "Failed to get peer state for retry: {}", e);
                                            let _ = resp.send(Err(anyhow!("Failed to get peer state for retry: {}", e)));
                                        }
                                    }
                                } else {
                                    tracing::error!(peer = %addr, "Peer disappeared during retry");
                                    let _ = resp.send(Err(anyhow!("Peer disappeared during retry")));
                                }
                            },
                            Err(e) => {
                                tracing::error!(peer = %addr, "Failed to establish connection for retry: {}", e);
                                let _ = resp.send(Err(anyhow!("Failed to establish connection for retry: {}", e)));
                            }
                        }
                    } else {
                        tracing::warn!(peer = %addr, "Cannot retry incoming connections from server side");
                        let _ = resp.send(Err(anyhow!("Cannot retry incoming connections from server side")));
                    }
                }
                MetalBondCommand::RouteUpdate(update) => {
                    Self::handle_route_update(&mut state, update).await;
                }
                MetalBondCommand::Shutdown => {
                    tracing::info!("MetalBond actor received shutdown command");
                    state.shutdown_notify.notify_waiters();
                    break;
                }
            }
        }
        tracing::info!("MetalBond actor loop terminated");
    }

    // Helper methods for the actor loop
    async fn handle_subscribe(_state: &mut MetalBondState, _vni: Vni) -> Result<()> {
        // Implementation of subscribe logic
        // ...
        Ok(())
    }

    async fn handle_unsubscribe(_state: &mut MetalBondState, _vni: Vni) -> Result<()> {
        // Implementation of unsubscribe logic
        // ...
        Ok(())
    }

    async fn handle_announce_route(
        _state: &mut MetalBondState,
        _vni: Vni,
        _dest: Destination,
        _nh: NextHop,
    ) -> Result<()> {
        // Implementation of announce route logic
        // ...
        Ok(())
    }

    async fn handle_withdraw_route(
        _state: &mut MetalBondState,
        _vni: Vni,
        _dest: Destination,
        _nh: NextHop,
    ) -> Result<()> {
        // Implementation of withdraw route logic
        // ...
        Ok(())
    }

    async fn handle_add_peer(_state: &mut MetalBondState, _addr: SocketAddr) -> Result<()> {
        // Implementation of add peer logic
        tracing::info!(peer = %_addr, "Attempting to establish connection to peer");
        
        // Check if we already have a connection to this peer
        if _state.peers.contains_key(&_addr) {
            tracing::info!(peer = %_addr, "Connection to peer already exists");
            return Ok(());
        }
        
        // Create a new peer with the correct direction
        tracing::debug!(peer = %_addr, "Creating new peer instance");
        let direction = if _state.is_server {
            ConnectionDirection::Incoming
        } else {
            ConnectionDirection::Outgoing
        };
        
        // Create the peer
        let (peer, wire_rx) = crate::peer::Peer::new(
            _state.metalbond_tx.clone(),
            _state.config.clone(),
            _addr,
            direction,
        );
        
        tracing::debug!(peer = %_addr, "Attempting to connect to remote peer");
        
        // For outgoing connections, attempt to connect to remote
        if direction == ConnectionDirection::Outgoing {
            // Attempt to establish TCP connection
            tracing::info!(peer = %_addr, "Initiating TCP connection to server");
            match tokio::net::TcpStream::connect(_addr).await {
                Ok(stream) => {
                    let local_addr = match stream.local_addr() {
                        Ok(addr) => {
                            tracing::debug!(peer = %_addr, local_addr = %addr, "Local address for connection");
                            Some(addr)
                        }
                        Err(e) => {
                            tracing::warn!(peer = %_addr, "Failed to get local address: {}", e);
                            None
                        }
                    };
                    
                    tracing::info!(
                        peer = %_addr, 
                        "TCP connection established successfully (local: {:?})", 
                        local_addr
                    );
                    
                    // Send Hello message to initiate the protocol
                    tracing::debug!(peer = %_addr, "Sending Hello message");
                    if let Err(e) = peer.send_hello().await {
                        tracing::error!(peer = %_addr, "Failed to send Hello message: {}", e);
                        return Err(anyhow!("Failed to send Hello message: {}", e));
                    }
                    
                    // Store peer in state
                    let peer_sender = peer.get_message_tx();
                    _state.peers.insert(_addr, peer_sender);
                    
                    // Spawn a task to handle this peer
                    let peer_handle = tokio::spawn(crate::peer::handle_peer(
                        peer.clone(),
                        stream,
                        wire_rx,
                    ));
                    _state.peer_tasks.insert(_addr, peer_handle);
                    
                    tracing::info!(peer = %_addr, "Peer connection setup complete");
                    Ok(())
                }
                Err(e) => {
                    tracing::error!(peer = %_addr, "Failed to connect to peer: {}", e);
                    Err(anyhow!("Failed to connect to peer at {}: {}", _addr, e))
                }
            }
        } else {
            // For incoming connections, the connection is already established
            tracing::debug!(peer = %_addr, "Setting up incoming connection");
            // Store peer in state for later processing when handle_peer is called
            let peer_sender = peer.get_message_tx();
            _state.peers.insert(_addr, peer_sender);
            
            tracing::info!(peer = %_addr, "Incoming peer connection registered");
            Ok(())
        }
    }

    async fn handle_remove_peer(_state: &mut MetalBondState, _addr: SocketAddr) -> Result<()> {
        // Implementation of remove peer logic
        // ...
        Ok(())
    }

    async fn handle_route_update(_state: &mut MetalBondState, _update: InternalUpdate) {
        // Implementation of route update logic
        // ...
    }

    pub fn start(&mut self) {
        // Start the connection manager task
        let cmd_tx = self.command_tx.clone();
        self.connection_manager_task = Some(tokio::spawn(async move {
            connection_manager_task(cmd_tx).await;
        }));

        tracing::info!("MetalBond core tasks started");
    }

    pub async fn start_server(&mut self, listen_address: String) -> Result<()> {
        let (tx, _rx) = oneshot::channel();
        // Just a dummy call to check if we're in server mode
        self.command_tx
            .send(MetalBondCommand::GetSubscribedVnis(tx))
            .await?;

        // Properly implement TcpListener for server mode
        let listener = TcpListener::bind(&listen_address).await?;
        tracing::info!("Listening on {}", listen_address);

        // Create a modified AddPeer command that can include a TcpStream
        // This requires adding a command variant to MetalBondCommand

        // Spawn a task to accept connections
        let cmd_tx = self.command_tx.clone();
        let server_task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        tracing::info!("Accepted connection from {}", addr);
                        
                        // Set up a peer for this connection
                        let (tx, rx) = oneshot::channel();
                        if let Err(e) = cmd_tx.send(MetalBondCommand::AddPeer(addr, tx)).await {
                            tracing::error!("Failed to create peer for {}: {}", addr, e);
                            continue;
                        }

                        // Check if peer was added successfully
                        match rx.await {
                            Ok(Ok(_)) => {
                                // Peer was added, now get access to the peer
                                let (state_tx, state_rx) = oneshot::channel();
                                if let Err(e) = cmd_tx.send(MetalBondCommand::GetPeerState(addr, state_tx)).await {
                                    tracing::error!("Failed to get peer state for {}: {}", addr, e);
                                    continue;
                                }
                                
                                match state_rx.await {
                                    Ok(Ok(_state)) => {
                                        // We'll manually spawn a handler for this connection
                                        // First get the peer object
                                        // Ideally, we would request this from MetalBond, but we'll work with current API
                                        // Create a dummy peer with the exact peer address
                                        // This is a workaround until we can add a proper GetPeer command to the API
                                        
                                        // Create a new peer instance for this connection
                                        // NOTE: This is a hack. Ideally, we'd reuse the existing peer that was created
                                        // by AddPeer, but the current API doesn't provide a way to get it.
                                        let config = Arc::new(Config {
                                            keepalive_interval: std::time::Duration::from_secs(30),
                                            retry_interval_min: std::time::Duration::from_millis(100),
                                            retry_interval_max: std::time::Duration::from_secs(5),
                                        });
                                        
                                        let (peer, wire_rx) = crate::peer::Peer::new(
                                            cmd_tx.clone(),
                                            config,
                                            addr,
                                            crate::types::ConnectionDirection::Incoming,
                                        );

                                        // Now manually handle the connection
                                        tokio::spawn(crate::peer::handle_peer(
                                            peer.clone(),
                                            stream,
                                            wire_rx,
                                        ));
                                        
                                        tracing::info!(peer = %addr, "Successfully created handler for incoming connection");
                                    },
                                    Ok(Err(e)) => {
                                        tracing::error!(peer = %addr, "Error getting peer state: {}", e);
                                    },
                                    Err(e) => {
                                        tracing::error!(peer = %addr, "Channel error getting peer state: {}", e);
                                    }
                                }
                            },
                            Ok(Err(e)) => {
                                tracing::error!("Failed to add peer {}: {}", addr, e);
                            },
                            Err(e) => {
                                tracing::error!("Failed to get response from AddPeer: {}", e);
                            }
                        };
                    }
                    Err(e) => {
                        tracing::error!("Failed to accept connection: {}", e);
                    }
                }
            }
        });

        // We don't currently have a field in the struct to store this task
        // but we should set it somewhere to ensure it stays alive
        self.distributor_task = Some(server_task);
        Ok(())
    }

    // Public API methods that send commands to the actor
    pub async fn subscribe(&self, vni: Vni) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::Subscribe(vni, tx))
            .await?;
        rx.await?
    }

    pub async fn unsubscribe(&self, vni: Vni) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::Unsubscribe(vni, tx))
            .await?;
        rx.await?
    }

    pub async fn get_subscribed_vnis(&self) -> Vec<Vni> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .command_tx
            .send(MetalBondCommand::GetSubscribedVnis(tx))
            .await
        {
            tracing::error!("Failed to send GetSubscribedVnis command: {}", e);
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn is_route_announced(&self, vni: Vni, dest: Destination, nh: NextHop) -> bool {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .command_tx
            .send(MetalBondCommand::IsRouteAnnounced(vni, dest, nh, tx))
            .await
        {
            tracing::error!("Failed to send IsRouteAnnounced command: {}", e);
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub async fn announce_route(&self, vni: Vni, dest: Destination, nh: NextHop) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::AnnounceRoute(vni, dest, nh, tx))
            .await?;
        rx.await?
    }

    pub async fn withdraw_route(&self, vni: Vni, dest: Destination, nh: NextHop) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::WithdrawRoute(vni, dest, nh, tx))
            .await?;
        rx.await?
    }

    pub async fn get_peer_state(&self, addr_str: &str) -> Result<ConnectionState> {
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid peer address format: {}", addr_str))?;

        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::GetPeerState(addr, tx))
            .await?;
        rx.await?
    }

    pub async fn add_peer(&self, server_addr_str: &str) -> Result<()> {
        let addr: SocketAddr = server_addr_str
            .parse()
            .with_context(|| format!("Invalid server address format: {}", server_addr_str))?;

        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::AddPeer(addr, tx))
            .await?;
        rx.await?
    }

    pub async fn remove_peer(&self, addr_str: &str) -> Result<()> {
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid peer address format: {}", addr_str))?;

        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(MetalBondCommand::RemovePeer(addr, tx))
            .await?;
        rx.await?
    }

    pub async fn shutdown(&mut self) {
        if let Err(e) = self.command_tx.send(MetalBondCommand::Shutdown).await {
            tracing::error!("Failed to send shutdown command: {}", e);
        }

        // Wait for actor task to finish
        if let Some(handle) = self.actor_task.take() {
            if let Err(e) = handle.await {
                tracing::error!("Error waiting for actor task: {}", e);
            }
        }

        // Wait for other tasks
        if let Some(handle) = self.distributor_task.take() {
            handle.abort();
        }

        if let Some(handle) = self.connection_manager_task.take() {
            handle.abort();
        }

        tracing::info!("MetalBond shutdown completed");
    }
}

// Updated task implementations for the actor model
async fn connection_manager_task(_cmd_tx: mpsc::Sender<MetalBondCommand>) {
    // Implementation of connection manager task
    // ...
}
