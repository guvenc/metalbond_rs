use crate::metalbond::MetalBondCommand;
use crate::pb;
use crate::types::{
    Config, ConnectionDirection, ConnectionState, Destination, MessageType, NextHop, UpdateAction,
    Vni,
};
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use futures::stream::StreamExt;
use futures::sink::SinkExt;
use ipnet::IpNet;
use prost::Message;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, error, info, info_span, warn, Instrument};

const PEER_TX_BUFFER_SIZE: usize = 256; // How many outbound messages can queue up
const METALBOND_VERSION: u8 = 1;
const MAX_MESSAGE_LEN: usize = 1188;
const RETRY_DELAY_MS: u64 = 3000;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("Channel send error: {0}")]
    ChannelSend(String),
    
    #[error("Channel receive error: {0}")]
    ChannelReceive(String),
    
    #[error("Protocol error: {0}")]
    Protocol(String),
    
    #[error("Connection error: {0}")]
    Connection(String),
    
    #[error("Peer TX channel full")]
    ChannelFull,
    
    #[error("Peer TX channel closed")]
    ChannelClosed,
    
    #[error("Invalid message: {0}")]
    InvalidMessage(String),
}

// Messages that can be sent to the Peer actor
#[derive(Debug)]
pub enum PeerMessage {
    SendHello(bool, oneshot::Sender<Result<()>>),
    SendKeepalive(oneshot::Sender<Result<()>>),
    SendSubscribe(Vni, oneshot::Sender<Result<()>>),
    SendUnsubscribe(Vni, oneshot::Sender<Result<()>>),
    SendUpdate(
        UpdateAction,
        Vni,
        Destination,
        NextHop,
        oneshot::Sender<Result<()>>,
    ),
    GetState(oneshot::Sender<ConnectionState>),
    SetState(ConnectionState),
    SetHelloReceived(bool),
    GetHelloExchangeStatus(oneshot::Sender<(bool, bool)>), // (hello_sent, hello_received)
    Retry,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum GeneratedMessage {
    Hello(pb::Hello),
    Keepalive,
    Subscribe(pb::Subscription),
    Unsubscribe(pb::Subscription),
    Update(pb::Update),
}

impl GeneratedMessage {
    pub fn from_bytes(msg_type: MessageType, bytes: &[u8]) -> Result<Self> {
        match msg_type {
            MessageType::Hello => Ok(Self::Hello(pb::Hello::decode(bytes)?)),
            MessageType::Keepalive => Ok(Self::Keepalive),
            MessageType::Subscribe => Ok(Self::Subscribe(pb::Subscription::decode(bytes)?)),
            MessageType::Unsubscribe => Ok(Self::Unsubscribe(pb::Subscription::decode(bytes)?)),
            MessageType::Update => Ok(Self::Update(pb::Update::decode(bytes)?)),
        }
    }

    pub fn into_typed_bytes(self) -> Result<(MessageType, Vec<u8>)> {
        match self {
            GeneratedMessage::Hello(msg) => Ok((MessageType::Hello, msg.encode_to_vec())),
            GeneratedMessage::Keepalive => Ok((MessageType::Keepalive, Vec::new())),
            GeneratedMessage::Subscribe(msg) => Ok((MessageType::Subscribe, msg.encode_to_vec())),
            GeneratedMessage::Unsubscribe(msg) => {
                Ok((MessageType::Unsubscribe, msg.encode_to_vec()))
            }
            GeneratedMessage::Update(msg) => Ok((MessageType::Update, msg.encode_to_vec())),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct MetalBondCodec;

impl Encoder<GeneratedMessage> for MetalBondCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: GeneratedMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let (msg_type, msg_bytes) = item.into_typed_bytes()?;
        let msg_len = msg_bytes.len();
        if msg_len > MAX_MESSAGE_LEN {
            bail!(
                "Message too long: {} bytes > maximum {}",
                msg_len,
                MAX_MESSAGE_LEN
            );
        }
        dst.reserve(4 + msg_len);
        dst.extend_from_slice(&[
            METALBOND_VERSION,
            (msg_len >> 8) as u8,
            (msg_len & 0xFF) as u8,
            msg_type as u8,
        ]);
        dst.extend_from_slice(&msg_bytes);
        Ok(())
    }
}

impl Decoder for MetalBondCodec {
    type Item = GeneratedMessage;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let version = src[0];
        if version != METALBOND_VERSION {
            bail!("Incompatible protocol version: {}", version);
        }
        let msg_len = ((src[1] as usize) << 8) | (src[2] as usize);
        let msg_type_byte = src[3];
        if src.len() < 4 + msg_len {
            src.reserve(4 + msg_len - src.len());
            return Ok(None);
        }
        let _header = src.split_to(4);
        let msg_bytes = src.split_to(msg_len);
        let msg_type = MessageType::try_from(msg_type_byte)
            .map_err(|_| anyhow!("Invalid message type byte: {}", msg_type_byte))?;
        let decoded_msg = GeneratedMessage::from_bytes(msg_type, &msg_bytes)?;
        Ok(Some(decoded_msg))
    }
}

/// Internal state for the Peer actor.
/// 
/// This struct holds all the data needed by the peer actor to maintain
/// its connection, track state, and communicate with other components.
#[derive(Debug)]
struct PeerState {
    metalbond_tx: mpsc::Sender<MetalBondCommand>,
    config: Arc<Config>,
    remote_addr: SocketAddr,
    local_addr: Option<SocketAddr>,
    direction: ConnectionDirection,
    is_remote_server: bool,
    state: ConnectionState,
    subscribed_vnis_remote: HashSet<Vni>,
    wire_tx: mpsc::Sender<GeneratedMessage>,
    hello_sent: bool,
    hello_received: bool,
    is_server: bool, // Store whether this peer is a server
}

/// Public interface for a peer connection.
///
/// Each peer represents a connection to a remote MetalBond instance.
/// The peer uses the actor model for internal concurrency, exposing
/// a public API to interact with it.
pub struct Peer {
    remote_addr: SocketAddr,
    message_tx: mpsc::Sender<PeerMessage>,
}

impl Peer {
    /// Creates a new peer instance and returns its handle along with a channel
    /// for sending wire protocol messages.
    ///
    /// # Arguments
    /// * `metalbond_tx` - Channel to send commands to the MetalBond instance
    /// * `config` - System configuration
    /// * `remote_addr` - Address of the remote peer
    /// * `direction` - Direction of the connection (incoming or outgoing)
    /// * `is_server` - Whether this instance is acting as a server
    pub fn new(
        metalbond_tx: mpsc::Sender<MetalBondCommand>,
        config: Arc<Config>,
        remote_addr: SocketAddr,
        direction: ConnectionDirection,
        is_server: bool,
    ) -> (Arc<Self>, mpsc::Receiver<GeneratedMessage>) {
        let (wire_tx, wire_rx) = mpsc::channel(PEER_TX_BUFFER_SIZE);
        let (message_tx, message_rx) = mpsc::channel(32);

        // Create the internal state
        let state = PeerState {
            metalbond_tx,
            config,
            remote_addr,
            local_addr: None,
            direction,
            is_remote_server: false,
            state: ConnectionState::Connecting,
            subscribed_vnis_remote: HashSet::new(),
            wire_tx,
            hello_sent: false,
            hello_received: false,
            is_server,
        };

        // Create a span for this peer actor
        let span = info_span!("peer_actor", peer = %remote_addr, direction = ?direction);

        // Spawn the actor
        tokio::spawn(
            Self::actor_loop(state, message_rx)
                .instrument(span)
        );

        let peer = Arc::new(Self {
            remote_addr,
            message_tx,
        });

        (peer, wire_rx)
    }

    async fn actor_loop(mut state: PeerState, mut rx: mpsc::Receiver<PeerMessage>) {
        debug!(peer = %state.remote_addr, "Peer actor starting");
        
        while let Some(msg) = rx.recv().await {
            let msg_span = info_span!("actor_message", peer = %state.remote_addr, message_type = ?msg);
            
            async {
                match msg {
                    PeerMessage::SendHello(is_server, resp) => {
                        let result = Self::handle_send_hello(&mut state, is_server).await;
                        let _ = resp.send(result);
                    }
                    PeerMessage::SendKeepalive(resp) => {
                        let result = Self::handle_send_keepalive(&state).await;
                        let _ = resp.send(result);
                    }
                    PeerMessage::SendSubscribe(vni, resp) => {
                        let result = Self::handle_send_subscribe(&state, vni).await;
                        let _ = resp.send(result);
                    }
                    PeerMessage::SendUnsubscribe(vni, resp) => {
                        let result = Self::handle_send_unsubscribe(&state, vni).await;
                        let _ = resp.send(result);
                    }
                    PeerMessage::SendUpdate(action, vni, dest, nh, resp) => {
                        let result = Self::handle_send_update(&state, action, vni, dest, nh).await;
                        let _ = resp.send(result);
                    }
                    PeerMessage::GetState(resp) => {
                        let _ = resp.send(state.state);
                    }
                    PeerMessage::SetState(new_state) => {
                        let old_state = state.state;
                        if old_state == new_state {
                            return; // Exit the async block instead of continue
                        }

                        state.state = new_state;
                        info!(
                            old = %old_state,
                            new = %new_state,
                            "Peer state changed"
                        );

                        // Handle state transitions
                        match (old_state, new_state) {
                            (_, ConnectionState::Established)
                                if old_state != ConnectionState::Established =>
                            {
                                // Newly established connection
                                Self::handle_connection_established(&state).await;
                            }
                            (ConnectionState::Established, _) => {
                                // Connection lost
                                info!("Connection lost, cleaning up peer resources");
                                Self::handle_connection_lost(&state).await;
                            }
                            _ => {}
                        }
                    }
                    PeerMessage::SetHelloReceived(received) => {
                        state.hello_received = received;
                    }
                    PeerMessage::GetHelloExchangeStatus(resp) => {
                        let _ = resp.send((state.hello_sent, state.hello_received));
                    }
                    PeerMessage::Retry => {
                        // Handle retry logic
                        info!("Retrying connection");
                        
                        // Only handle retry if we're in Retry state
                        if state.state == ConnectionState::Retry {
                            // If this is an outgoing connection, attempt to reconnect
                            if state.direction == ConnectionDirection::Outgoing {
                                // Reset hello flags
                                state.hello_sent = false;
                                state.hello_received = false;
                                
                                // Notify MetalBond about the retry attempt
                                let (tx, rx) = oneshot::channel();
                                if let Err(e) = state.metalbond_tx.send(
                                    MetalBondCommand::RetryConnection(state.remote_addr, tx)
                                ).await {
                                    error!("Failed to send retry notification: {}", e);
                                } else {
                                    if let Err(e) = rx.await {
                                        error!("Failed to receive retry response: {}", e);
                                    }
                                }
                                
                                // Move to Connecting state
                                state.state = ConnectionState::Connecting;
                            } else {
                                // For incoming connections, we can't retry - remote must reconnect
                                warn!("Cannot retry incoming connection");
                            }
                        } else {
                            warn!("Retry requested but state is {}", state.state);
                        }
                    }
                    PeerMessage::Shutdown => {
                        debug!("Peer actor shutting down");
                        rx.close(); // Close the channel instead of using break
                        return; // Exit the async block
                    }
                }
            }
            .instrument(msg_span)
            .await;
        }
        
        info!(peer = %state.remote_addr, "Peer actor terminated");
    }

    // Helper methods for the actor
    async fn handle_send_hello(state: &mut PeerState, is_server: bool) -> Result<()> {
        let hello = pb::Hello {
            keepalive_interval: state.config.keepalive_interval.as_secs() as u32,
            is_server,
        };
        state.hello_sent = true;
        state.is_server = is_server;
        
        // Update the state based on current state and hello_received flag
        match (state.state, state.hello_received) {
            (ConnectionState::Connecting, false) => {
                // No Hello received yet, transition to HelloSent
                state.state = ConnectionState::HelloSent;
            },
            (ConnectionState::Connecting, true) | (ConnectionState::HelloReceived, true) => {
                // We've received Hello and now sending one, transition to Established
                state.state = ConnectionState::Established;
            },
            (ConnectionState::HelloReceived, false) => {
                // This is a rare case - we received a Hello but the flag wasn't set
                // Update state to Established since we're now sending hello
                state.state = ConnectionState::Established;
                tracing::warn!(
                    peer = %state.remote_addr,
                    "Unexpected state: HelloReceived with hello_received=false, transitioning to Established"
                );
            },
            (ConnectionState::HelloSent, _) | (ConnectionState::Established, _) => {
                // Already sent Hello before, maintain current state
            },
            (ConnectionState::Retry, _) => {
                // If in retry state, sending Hello means we're now in HelloSent
                state.state = ConnectionState::HelloSent;
            },
            (ConnectionState::Closed, _) => {
                // Sending Hello while closed is unusual but possible during reconnection
                // Move to HelloSent
                state.state = ConnectionState::HelloSent;
            },
        }
        
        tracing::debug!(
            peer = %state.remote_addr,
            old_state = %state.state,
            "Sending Hello message as {}",
            if is_server { "server" } else { "client" }
        );
        
        Self::send_message(&state.wire_tx, GeneratedMessage::Hello(hello)).await
    }

    async fn handle_send_keepalive(state: &PeerState) -> Result<()> {
        Self::send_message(&state.wire_tx, GeneratedMessage::Keepalive).await
    }

    async fn handle_send_subscribe(state: &PeerState, vni: Vni) -> Result<()> {
        let sub = pb::Subscription { vni };
        Self::send_message(&state.wire_tx, GeneratedMessage::Subscribe(sub)).await
    }

    async fn handle_send_unsubscribe(state: &PeerState, vni: Vni) -> Result<()> {
        let unsub = pb::Subscription { vni };
        Self::send_message(&state.wire_tx, GeneratedMessage::Unsubscribe(unsub)).await
    }

    async fn handle_send_update(
        state: &PeerState,
        action: UpdateAction,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
    ) -> Result<()> {
        let pb_dest = dest
            .try_into()
            .context("Failed to convert Destination to protobuf")?;
        let pb_nh = nh
            .try_into()
            .context("Failed to convert NextHop to protobuf")?;
        let update = pb::Update {
            action: pb::Action::from(action) as i32,
            vni,
            destination: Some(pb_dest),
            next_hop: Some(pb_nh),
        };
        Self::send_message(&state.wire_tx, GeneratedMessage::Update(update)).await
    }

    async fn handle_connection_established(state: &PeerState) {
        let peer_addr = state.remote_addr;
        let span = info_span!("connection_established", peer = %peer_addr);
        
        async {
            info!("Connection established, sending subscriptions");
            
            // Only send subscription requests if we're the client or direction is outgoing
            if !state.is_server || state.direction == ConnectionDirection::Outgoing {
                // Get the list of subscribed VNIs from MetalBond
                let (tx, rx) = oneshot::channel();
                if let Err(e) = state.metalbond_tx.send(MetalBondCommand::GetSubscribedVnis(tx)).await {
                    error!("Failed to get subscribed VNIs: {}", e);
                    return;
                }
                
                // Get the list of VNIs
                let vnis = match rx.await {
                    Ok(vnis) => vnis,
                    Err(e) => {
                        error!("Failed to receive subscribed VNIs: {}", e);
                        return;
                    }
                };
                
                // Log the VNIs we're going to subscribe to
                if vnis.is_empty() {
                    info!("No VNIs to subscribe to");
                } else {
                    info!(vni_count = vnis.len(), "Sending subscription requests");
                    
                    // Send subscription for each VNI
                    for vni in vnis {
                        let sub = pb::Subscription { vni };
                        if let Err(e) = Self::send_message(&state.wire_tx, GeneratedMessage::Subscribe(sub)).await {
                            warn!(vni = vni, "Failed to send subscription: {}", e);
                        } else {
                            debug!(vni = vni, "Sent subscription request");
                        }
                    }
                    
                    info!("All subscription requests sent");
                }
            } else {
                info!("Not sending subscriptions as we're the server for this connection");
            }
        }
        .instrument(span)
        .await;
    }

    async fn handle_connection_lost(state: &PeerState) {
        // Notify MetalBond to remove routes from this peer
        let (tx, rx) = oneshot::channel();
        let _ = state
            .metalbond_tx
            .send(MetalBondCommand::RemovePeer(state.remote_addr, tx))
            .await;
        if let Err(e) = rx.await {
            tracing::error!(peer = %state.remote_addr, "Failed to clean up peer: {}", e);
        }
    }

    async fn send_message(
        tx: &mpsc::Sender<GeneratedMessage>,
        msg: GeneratedMessage,
    ) -> Result<()> {
        match tx.try_send(msg) {
            Ok(_) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(PeerError::ChannelFull.into()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(PeerError::ChannelClosed.into()),
        }
    }

    // Public interface methods
    pub fn get_remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    pub async fn get_state(&self) -> Result<ConnectionState> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::GetState(tx))
            .await
            .map_err(|e| PeerError::ChannelSend(e.to_string()))?;
        rx.await.map_err(|e| PeerError::ChannelReceive(e.to_string()).into())
    }

    pub async fn send_hello(&self, is_server: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::SendHello(is_server, tx))
            .await
            .map_err(|_| anyhow!("Failed to send Hello message"))?;
        rx.await
            .map_err(|_| anyhow!("Failed to get Hello result"))?
    }

    pub async fn send_keepalive(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::SendKeepalive(tx))
            .await
            .map_err(|_| anyhow!("Failed to send Keepalive message"))?;
        rx.await
            .map_err(|_| anyhow!("Failed to get Keepalive result"))?
    }

    pub async fn send_subscribe(&self, vni: Vni) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::SendSubscribe(vni, tx))
            .await
            .map_err(|_| anyhow!("Failed to send Subscribe message"))?;
        rx.await
            .map_err(|_| anyhow!("Failed to get Subscribe result"))?
    }

    pub async fn send_unsubscribe(&self, vni: Vni) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::SendUnsubscribe(vni, tx))
            .await
            .map_err(|_| anyhow!("Failed to send Unsubscribe message"))?;
        rx.await
            .map_err(|_| anyhow!("Failed to get Unsubscribe result"))?
    }

    pub async fn send_update(
        &self,
        action: UpdateAction,
        vni: Vni,
        dest: Destination,
        nh: NextHop,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::SendUpdate(action, vni, dest, nh, tx))
            .await
            .map_err(|_| anyhow!("Failed to send Update message"))?;
        rx.await
            .map_err(|_| anyhow!("Failed to get Update result"))?
    }

    pub fn trigger_shutdown(&self) {
        // Fire and forget, not waiting for result
        let _ = self.message_tx.try_send(PeerMessage::Shutdown);
    }

    pub fn set_state(&self, new_state: ConnectionState) {
        if let Err(e) = self.message_tx.try_send(PeerMessage::SetState(new_state)) {
            tracing::warn!(
                peer = %self.remote_addr,
                "Failed to set state to {}: {}",
                new_state,
                e
            );
        }
    }

    pub fn get_message_tx(&self) -> mpsc::Sender<PeerMessage> {
        self.message_tx.clone()
    }

    pub async fn get_hello_exchange_status(&self) -> Result<(bool, bool)> {
        let (tx, rx) = oneshot::channel();
        self.message_tx
            .send(PeerMessage::GetHelloExchangeStatus(tx))
            .await
            .map_err(|_| anyhow!("Failed to send GetHelloExchangeStatus message"))?;
        rx.await.map_err(|_| anyhow!("Failed to receive hello exchange status"))
    }

    pub fn set_hello_received(&self, received: bool) {
        if let Err(e) = self.message_tx.try_send(PeerMessage::SetHelloReceived(received)) {
            tracing::warn!(
                peer = %self.remote_addr,
                "Failed to set hello_received to {}: {}",
                received,
                e
            );
        }
    }

    pub fn trigger_retry(&self) {
        if let Err(e) = self.message_tx.try_send(PeerMessage::Retry) {
            tracing::warn!(
                peer = %self.remote_addr,
                "Failed to trigger retry: {}",
                e
            );
        }
    }
    
    // Handle connection failure by moving to retry state
    pub fn handle_connection_failure(&self) {
        // Clone all necessary values to avoid borrowing self in the async block
        let addr = self.remote_addr;
        let message_tx = self.message_tx.clone();
        
        // Create a future that can be spawned
        let future = async move {
            // Set state to Retry
            info!("Handling connection failure");
            
            // Use the cloned message_tx to send the SetState message
            if let Err(e) = message_tx.send(PeerMessage::SetState(ConnectionState::Retry)).await {
                error!("Failed to set state to Retry: {}", e);
                return;
            }
            
            // Schedule a retry attempt after a delay
            let retry_sender = message_tx.clone();
            tokio::spawn(async move {
                debug!("Scheduling retry in {}ms", RETRY_DELAY_MS);
                tokio::time::sleep(tokio::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                
                // After delay, send the retry message
                debug!("Retry delay elapsed, sending retry message");
                if let Err(e) = retry_sender.send(PeerMessage::Retry).await {
                    error!("Failed to send retry message: {}", e);
                }
            }.instrument(info_span!("retry_scheduler", peer = %addr)));
            
            info!("Connection failure handling complete");
        };
        
        // Create a span for this task
        let span = info_span!("connection_failure", peer = %addr);
        
        // Spawn the instrumented future
        tokio::spawn(future.instrument(span));
    }
}

/// Handles the peer connection communication loop.
///
/// This function is the core of the peer protocol handling. It manages the
/// TCP stream, processes incoming messages, sends outgoing messages, and
/// implements the state machine for the peer connection.
///
/// # Arguments
/// * `peer` - The peer instance to handle
/// * `stream` - The TCP stream for communication
/// * `wire_rx` - Channel for receiving messages to send to the remote peer
pub async fn handle_peer(
    peer: Arc<Peer>,
    stream: TcpStream,
    wire_rx: mpsc::Receiver<GeneratedMessage>,
) {
    let _addr = peer.get_remote_addr();
    // Create a span for this peer that will track the entire connection lifecycle
    let span = info_span!("peer_connection", peer = %_addr);
    
    async {
        info!("Starting peer connection handler");
        
        let result = match setup_connection(&peer, &stream).await {
            Ok(()) => {
                let framed = tokio_util::codec::Framed::new(stream, MetalBondCodec::default());
                let (sink, stream) = framed.split();
                
                info!("Protocol connection established, now handling messages");
                handle_message_loop(&peer, sink, stream, wire_rx).await
            }
            Err(e) => Err(e),
        };
        
        // Handle connection teardown
        peer.set_state(ConnectionState::Closed);
        
        if let Err(e) = result {
            error!("Peer handler encountered error: {}", e);
        }
        
        info!("Peer connection handler completed");
    }
    .instrument(span)
    .await;
}

/// Sets up the initial connection state and logging.
///
/// # Arguments
/// * `peer` - The peer instance to handle
/// * `stream` - The TCP stream for the connection
///
/// # Returns
/// * `Ok(())` if setup is successful
/// * `Err` with the error message if setup fails
async fn setup_connection(peer: &Arc<Peer>, stream: &TcpStream) -> Result<()> {
    let _addr = peer.get_remote_addr();
    
    let peer_addr = stream.peer_addr()
        .map_err(|e| PeerError::Connection(format!("Failed to get peer address: {}", e)))?;
    
    info!(remote_addr = %peer_addr, "Connected to peer");
    
    // Get local address info for better debugging
    if let Ok(local_addr) = stream.local_addr() {
        info!(local_addr = %local_addr, "Local endpoint for connection");
    }
    
    // Set peer as connecting, not directly to established
    peer.set_state(ConnectionState::Connecting);
    
    Ok(())
}

/// Handles the main message processing loop for a peer connection.
///
/// # Arguments
/// * `peer` - The peer instance
/// * `mut sink` - Message sink for sending protocol messages
/// * `mut stream` - Message stream for receiving protocol messages
/// * `mut wire_rx` - Channel for receiving messages to send to the remote peer
///
/// # Returns
/// * `Ok(())` if the loop terminates normally
/// * `Err` with the error message if an error occurs
async fn handle_message_loop<S, T>(
    peer: &Arc<Peer>,
    mut sink: S,
    mut stream: T,
    mut wire_rx: mpsc::Receiver<GeneratedMessage>,
) -> Result<()>
where
    S: SinkExt<GeneratedMessage> + Unpin,
    T: StreamExt<Item = Result<GeneratedMessage>> + Unpin,
{
    // We don't need to store the address here since we're using structured spans
    let mut shutdown_requested = false;
    
    // Create a span for the message loop
    let span = info_span!("message_loop");
    
    async {
        debug!("Starting message processing loop");
        
        while !shutdown_requested {
            tokio::select! {
                Some(outgoing_msg) = wire_rx.recv() => {
                    if let Err(e) = handle_outgoing_message(&mut sink, &peer, outgoing_msg).await {
                        return Err(e);
                    }
                }
                
                incoming = stream.next() => {
                    match incoming {
                        Some(Ok(msg)) => {
                            handle_incoming_message(&peer, msg).await;
                        }
                        Some(Err(e)) => {
                            error!("Error receiving message: {}", e);
                            peer.handle_connection_failure();
                            return Err(PeerError::Protocol(format!("Error receiving message: {}", e)).into());
                        }
                        None => {
                            info!("Peer connection closed by remote");
                            peer.handle_connection_failure();
                            return Err(PeerError::Connection("Connection closed by remote".into()).into());
                        }
                    }
                }
                
                else => {
                    info!("All channels closed, ending peer handler");
                    shutdown_requested = true;
                }
            }
        }
        
        debug!("Message loop completed normally");
        Ok(())
    }
    .instrument(span)
    .await
}

/// Handles outgoing protocol messages.
///
/// # Arguments
/// * `sink` - The message sink to send messages to
/// * `peer` - The peer instance
/// * `msg` - The message to send
///
/// # Returns
/// * `Ok(())` if the message is sent successfully
/// * `Err` with error details if sending fails
async fn handle_outgoing_message<S>(
    sink: &mut S,
    peer: &Arc<Peer>,
    msg: GeneratedMessage,
) -> Result<()>
where
    S: SinkExt<GeneratedMessage> + Unpin,
{
    // No need to store the address - it's included in the span
    let span = info_span!("outgoing_message", message_type = ?msg);
    
    async {
        // Log outgoing message details
        match &msg {
            GeneratedMessage::Hello(_) => {
                info!("Sending Hello message")
            }
            GeneratedMessage::Keepalive => {
                debug!("Sending Keepalive")
            }
            GeneratedMessage::Subscribe(sub) => {
                info!(vni = sub.vni, "Sending Subscribe")
            }
            GeneratedMessage::Unsubscribe(unsub) => {
                info!(vni = unsub.vni, "Sending Unsubscribe")
            }
            GeneratedMessage::Update(update) => {
                let action = action_to_string(update.action);
                info!(
                    vni = update.vni, 
                    action = %action,
                    "Sending Update message"
                );
            }
        }
        
        // Send the message
        if let Err(_) = sink.send(msg).await {
            error!("Failed to send message");
            peer.handle_connection_failure();
            return Err(PeerError::Connection("Failed to send message to peer".into()).into());
        }
        
        Ok(())
    }
    .instrument(span)
    .await
}

/// Handles incoming protocol messages.
///
/// # Arguments
/// * `peer` - The peer instance
/// * `msg` - The received message to process
async fn handle_incoming_message(peer: &Arc<Peer>, msg: GeneratedMessage) {
    // No need to store the address - it's included in the span
    let span = info_span!("incoming_message", message_type = ?msg);
    
    async {
        match msg {
            GeneratedMessage::Hello(hello) => {
                handle_hello_message(peer, hello).await;
            }
            GeneratedMessage::Keepalive => {
                debug!("Received Keepalive");
                
                // Respond with our own Keepalive
                if let Err(e) = peer.send_keepalive().await {
                    warn!("Failed to respond to Keepalive: {}", e);
                }
            }
            GeneratedMessage::Subscribe(sub) => {
                info!(vni = sub.vni, "Received Subscribe for VNI");
                // Handle subscription (logging only for now)
            }
            GeneratedMessage::Unsubscribe(unsub) => {
                info!(vni = unsub.vni, "Received Unsubscribe for VNI");
                // Handle unsubscription (logging only for now)
            }
            GeneratedMessage::Update(update) => {
                handle_update_message(peer, update).await;
            }
        }
    }
    .instrument(span)
    .await;
}

/// Handles Hello protocol messages.
///
/// # Arguments
/// * `peer` - The peer instance
/// * `hello` - The Hello message
async fn handle_hello_message(peer: &Arc<Peer>, hello: pb::Hello) {
    // No need to store the address - it's included in the span
    let span = info_span!("hello_handler", is_server = hello.is_server);
    
    async {
        info!("Received Hello message");
        
        // Mark that we've received a hello message
        peer.set_hello_received(true);
        
        // Store the remote's server status
        let is_remote_server = hello.is_server;
        
        // Get current state
        match peer.get_state().await {
            Ok(current_state) => {
                match current_state {
                    ConnectionState::Connecting => {
                        // We received a Hello before sending one
                        // Update state to HelloReceived
                        peer.set_state(ConnectionState::HelloReceived);
                        
                        // Send our Hello response
                        // Determine our is_server status (opposite of remote)
                        let our_is_server = !is_remote_server;
                        if let Err(e) = peer.send_hello(our_is_server).await {
                            error!("Failed to respond to Hello: {}", e);
                        }
                    },
                    ConnectionState::HelloSent => {
                        // We already sent a Hello and now received one
                        // Transition to Established
                        peer.set_state(ConnectionState::Established);
                        
                        // No need to send another Hello
                        info!("Connection established after Hello exchange");
                    },
                    ConnectionState::HelloReceived => {
                        // We've already received a Hello, this is a duplicate
                        warn!("Received duplicate Hello message");
                    },
                    ConnectionState::Established => {
                        // Already established, might be a re-Hello
                        info!("Received Hello while already established");
                    },
                    ConnectionState::Retry => {
                        // We were in retry mode, now connected
                        // Send our Hello and go to HelloReceived
                        peer.set_state(ConnectionState::HelloReceived);
                        let our_is_server = !is_remote_server;
                        if let Err(e) = peer.send_hello(our_is_server).await {
                            error!("Failed to respond to Hello in Retry state: {}", e);
                        }
                    },
                    ConnectionState::Closed => {
                        // Strange, received Hello while closed
                        warn!("Received Hello while in Closed state");
                    }
                }
            },
            Err(e) => {
                error!("Failed to get peer state: {}", e);
                // Fallback: assume we're connecting and respond with Hello
                let our_is_server = !is_remote_server;
                if let Err(e) = peer.send_hello(our_is_server).await {
                    error!("Failed to respond to Hello after state error: {}", e);
                }
            }
        }
    }
    .instrument(span)
    .await;
}

/// Handles Update protocol messages.
///
/// # Arguments
/// * `peer` - The peer instance
/// * `update` - The Update message
async fn handle_update_message(_peer: &Arc<Peer>, update: pb::Update) {
    // No need to store the address - it's included in the span
    let action = action_to_string(update.action);
    
    // Extract destination details for better logging
    let dest_info = if let Some(dest) = &update.destination {
        format!("Prefix: {}/{}", 
            ip_version_to_string(dest.ip_version),
            dest.prefix_length
        )
    } else {
        "Invalid destination".to_string()
    };
    
    // Extract next hop details for better logging
    let nh_info = if let Some(nh) = &update.next_hop {
        format!("NextHop type: {}, VNI: {}", 
            nh.r#type, 
            nh.target_vni
        )
    } else {
        "Invalid next hop".to_string()
    };
    
    let span = info_span!(
        "update_handler", 
        vni = update.vni, 
        action = %action,
        destination = %dest_info,
        next_hop = %nh_info
    );
    
    async {
        info!("Received Update message");
        // Process the update (logging only for now)
    }
    .instrument(span)
    .await;
}

impl TryFrom<pb::Destination> for Destination {
    type Error = anyhow::Error;
    fn try_from(pb_dest: pb::Destination) -> Result<Self, Self::Error> {
        let addr = IpAddr::from_slice(&pb_dest.prefix)?;
        let len = pb_dest.prefix_length as u8;
        let prefix = IpNet::new(addr, len)?;
        Ok(Destination { prefix })
    }
}

impl TryFrom<Destination> for pb::Destination {
    type Error = anyhow::Error;
    fn try_from(dest: Destination) -> Result<Self, Self::Error> {
        Ok(pb::Destination {
            ip_version: pb::IpVersion::from(dest.ip_version()) as i32,
            prefix: dest.prefix.addr().addr_bytes_vec(),
            prefix_length: dest.prefix.prefix_len() as u32,
        })
    }
}

impl TryFrom<pb::NextHop> for NextHop {
    type Error = anyhow::Error;
    fn try_from(pb_nh: pb::NextHop) -> Result<Self, Self::Error> {
        let target_address = IpAddr::from_slice(&pb_nh.target_address)?;
        let hop_type = pb::NextHopType::try_from(pb_nh.r#type)
            .unwrap_or(pb::NextHopType::Standard);
            
        Ok(NextHop {
            target_address,
            target_vni: pb_nh.target_vni,
            hop_type,
            nat_port_range_from: pb_nh.nat_port_range_from as u16,
            nat_port_range_to: pb_nh.nat_port_range_to as u16,
        })
    }
}

impl TryFrom<NextHop> for pb::NextHop {
    type Error = anyhow::Error;
    fn try_from(nh: NextHop) -> Result<Self, Self::Error> {
        Ok(pb::NextHop {
            target_address: nh.target_address.addr_bytes_vec(),
            target_vni: nh.target_vni,
            r#type: nh.hop_type as i32,
            nat_port_range_from: nh.nat_port_range_from as u32,
            nat_port_range_to: nh.nat_port_range_to as u32,
        })
    }
}

/// Extension trait for IpAddr to handle conversion to and from byte arrays.
///
/// This trait provides utilities for working with IP addresses in their raw byte form,
/// which is useful for protocol serialization and deserialization.
trait IpAddrBytes {
    /// Converts an IP address to its byte representation as a Vec<u8>.
    fn addr_bytes_vec(&self) -> Vec<u8>;
    
    /// Creates an IP address from a slice of bytes.
    ///
    /// # Errors
    /// Returns an error if the slice length is neither 4 (IPv4) nor 16 (IPv6).
    fn from_slice(slice: &[u8]) -> Result<IpAddr>;
}

impl IpAddrBytes for IpAddr {
    fn addr_bytes_vec(&self) -> Vec<u8> {
        match self {
            IpAddr::V4(ip4) => ip4.octets().to_vec(),
            IpAddr::V6(ip6) => ip6.octets().to_vec(),
        }
    }
    
    fn from_slice(slice: &[u8]) -> Result<IpAddr> {
        match slice.len() {
            4 => {
                let mut octets = [0u8; 4];
                octets.copy_from_slice(slice);
                Ok(IpAddr::V4(octets.into()))
            }
            16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(slice);
                Ok(IpAddr::V6(octets.into()))
            }
            len => Err(PeerError::InvalidMessage(format!("Invalid IP address byte length: {}", len)).into()),
        }
    }
}

fn action_to_string(action: i32) -> &'static str {
    if action == pb::Action::Add as i32 {
        "add"
    } else {
        "remove"
    }
}

fn ip_version_to_string(version: i32) -> &'static str {
    if version == pb::IpVersion::IPv4 as i32 {
        "IPv4"
    } else {
        "IPv6"
    }
}
