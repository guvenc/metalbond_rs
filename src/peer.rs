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
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{Decoder, Encoder};

const PEER_TX_BUFFER_SIZE: usize = 256; // How many outbound messages can queue up
#[allow(dead_code)]
const METALBOND_VERSION: u8 = 1;
#[allow(dead_code)]
const MAX_MESSAGE_LEN: usize = 1188;

// Messages that can be sent to the Peer actor
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
    Shutdown,
}

#[derive(Debug)]
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

#[allow(dead_code)]
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

// Internal state for the Peer actor
#[allow(dead_code)]
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

// Public interface for the Peer
pub struct Peer {
    remote_addr: SocketAddr,
    message_tx: mpsc::Sender<PeerMessage>,
}

impl Peer {
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

        // Spawn the actor
        tokio::spawn(Self::actor_loop(state, message_rx));

        let peer = Arc::new(Self {
            remote_addr,
            message_tx,
        });

        (peer, wire_rx)
    }

    async fn actor_loop(mut state: PeerState, mut rx: mpsc::Receiver<PeerMessage>) {
        while let Some(msg) = rx.recv().await {
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
                        continue;
                    }

                    state.state = new_state;
                    tracing::info!(
                        peer = %state.remote_addr,
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
                            tracing::info!(
                                peer = %state.remote_addr,
                                "Connection lost, cleaning up peer resources"
                            );
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
                PeerMessage::Shutdown => {
                    tracing::debug!(peer = %state.remote_addr, "Peer actor shutting down");
                    break;
                }
            }
        }
        tracing::info!(peer = %state.remote_addr, "Peer actor terminated");
    }

    // Helper methods for the actor
    async fn handle_send_hello(state: &mut PeerState, is_server: bool) -> Result<()> {
        let hello = pb::Hello {
            keepalive_interval: state.config.keepalive_interval.as_secs() as u32,
            is_server,
        };
        state.hello_sent = true;
        state.is_server = is_server;
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
        // Send initial subscriptions
        // For now we're not implementing the detailed logic, just showing the approach
        tracing::debug!(peer = %state.remote_addr, "Connection established");
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
            Err(mpsc::error::TrySendError::Full(_)) => Err(anyhow!("Peer TX channel full")),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(anyhow!("Peer TX channel closed")),
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
            .map_err(|_| anyhow!("Failed to send GetState message"))?;
        rx.await.map_err(|_| anyhow!("Failed to receive state"))
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
        // Fire and forget, not waiting for result
        let _ = self.message_tx.try_send(PeerMessage::SetState(new_state));
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
        // Fire and forget, not waiting for result
        let _ = self.message_tx.try_send(PeerMessage::SetHelloReceived(received));
    }
}

// Rewrite the handle_peer function to work with our actor model
pub async fn handle_peer(
    peer: Arc<Peer>,
    stream: TcpStream,
    mut wire_rx: mpsc::Receiver<GeneratedMessage>,
) {
    let addr = peer.get_remote_addr();
    tracing::info!(peer = %addr, "Starting peer connection handler");
    
    let result = match stream.peer_addr() {
        Ok(peer_addr) => {
            tracing::info!(peer = %addr, remote_addr = %peer_addr, "Connected to peer");
            
            // Get local address info for better debugging
            if let Ok(local_addr) = stream.local_addr() {
                tracing::info!(peer = %addr, local_addr = %local_addr, "Local endpoint for connection");
            }
            
            // Set peer as connected
            peer.set_state(ConnectionState::Established);
            
            // Set up framed connection with codec
            let codec = MetalBondCodec;
            let framed = tokio_util::codec::Framed::new(stream, codec);
            let (mut sink, mut stream) = framed.split();
            
            tracing::info!(peer = %addr, "Protocol connection established, now handling messages");
            
            // Process incoming and outgoing messages in a loop
            let mut result = Ok(());
            let mut shutdown_requested = false;
            
            while !shutdown_requested {
                tokio::select! {
                    Some(outgoing_msg) = wire_rx.recv() => {
                        match &outgoing_msg {
                            GeneratedMessage::Hello(_) => tracing::info!(peer = %addr, "Sending Hello message"),
                            GeneratedMessage::Keepalive => tracing::debug!(peer = %addr, "Sending Keepalive"),
                            GeneratedMessage::Subscribe(sub) => tracing::info!(peer = %addr, vni = sub.vni, "Sending Subscribe"),
                            GeneratedMessage::Unsubscribe(unsub) => tracing::info!(peer = %addr, vni = unsub.vni, "Sending Unsubscribe"),
                            GeneratedMessage::Update(update) => {
                                let action = if update.action == pb::Action::Add as i32 { "add" } else { "remove" };
                                tracing::info!(
                                    peer = %addr, 
                                    vni = update.vni, 
                                    action = %action,
                                    "Sending Update message"
                                );
                            }
                        }
                        
                        if let Err(e) = sink.send(outgoing_msg).await {
                            tracing::error!(peer = %addr, "Failed to send message: {}", e);
                            result = Err(anyhow!("Failed to send message: {}", e));
                            break;
                        }
                    }
                    
                    incoming = stream.next() => {
                        match incoming {
                            Some(Ok(msg)) => {
                                match msg {
                                    GeneratedMessage::Hello(hello) => {
                                        tracing::info!(
                                            peer = %addr, 
                                            is_server = hello.is_server, 
                                            "Received Hello message"
                                        );
                                        
                                        // Mark that we've received a hello message
                                        peer.set_hello_received(true);
                                        
                                        // Store the remote's server status
                                        let is_remote_server = hello.is_server;
                                        
                                        // Check if we've already sent a hello
                                        let exchange_status = peer.get_hello_exchange_status().await;
                                        match exchange_status {
                                            Ok((hello_sent, _)) => {
                                                if !hello_sent {
                                                    // If we haven't sent a hello yet, send one
                                                    tracing::info!(peer = %addr, "Responding with Hello message");
                                                    
                                                    // Here we'd ideally get our own is_server status, but since we can't
                                                    // access that directly in this function, we'll use the negation of
                                                    // the remote's is_server as a heuristic
                                                    let our_is_server = !is_remote_server;
                                                    
                                                    if let Err(e) = peer.send_hello(our_is_server).await {
                                                        tracing::error!(peer = %addr, "Failed to respond to Hello: {}", e);
                                                    }
                                                } else {
                                                    tracing::debug!(peer = %addr, "Already sent Hello, not responding with another");
                                                }
                                            },
                                            Err(e) => {
                                                tracing::error!(peer = %addr, "Failed to get hello exchange status: {}", e);
                                                // As a fallback, respond with hello
                                                if let Err(e) = peer.send_hello(false).await {
                                                    tracing::error!(peer = %addr, "Failed to respond to Hello: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    GeneratedMessage::Keepalive => {
                                        tracing::debug!(peer = %addr, "Received Keepalive");
                                        
                                        // Respond with our own Keepalive
                                        if let Err(e) = peer.send_keepalive().await {
                                            tracing::warn!(peer = %addr, "Failed to respond to Keepalive: {}", e);
                                        }
                                    }
                                    GeneratedMessage::Subscribe(sub) => {
                                        tracing::info!(peer = %addr, vni = sub.vni, "Received Subscribe for VNI");
                                        // Handle subscription (logging only for now)
                                    }
                                    GeneratedMessage::Unsubscribe(unsub) => {
                                        tracing::info!(peer = %addr, vni = unsub.vni, "Received Unsubscribe for VNI");
                                        // Handle unsubscription (logging only for now)
                                    }
                                    GeneratedMessage::Update(update) => {
                                        let action = if update.action == pb::Action::Add as i32 { "add" } else { "remove" };
                                        
                                        // Extract destination details for better logging
                                        let dest_info = if let Some(dest) = &update.destination {
                                            format!("Prefix: {}/{}", 
                                                if dest.ip_version == pb::IpVersion::IPv4 as i32 {"IPv4"} else {"IPv6"},
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
                                        
                                        tracing::info!(
                                            peer = %addr, 
                                            vni = update.vni, 
                                            action = %action,
                                            destination = %dest_info,
                                            next_hop = %nh_info,
                                            "Received Update message"
                                        );
                                        // Process the update (logging only for now)
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                tracing::error!(peer = %addr, "Error receiving message: {}", e);
                                result = Err(anyhow!("Error receiving message: {}", e));
                                break;
                            }
                            None => {
                                tracing::info!(peer = %addr, "Peer connection closed by remote");
                                result = Err(anyhow!("Connection closed by remote"));
                                break;
                            }
                        }
                    }
                    
                    else => {
                        tracing::info!(peer = %addr, "All channels closed, ending peer handler");
                        shutdown_requested = true;
                    }
                }
            }
            
            result
        }
        Err(e) => {
            tracing::error!(peer = %addr, "Failed to get peer address: {}", e);
            Err(anyhow!("Failed to get peer address: {}", e))
        }
    };
    
    // Handle connection teardown
    peer.set_state(ConnectionState::Closed);
    
    if let Err(e) = result {
        tracing::error!(peer = %addr, "Peer handler encountered error: {}", e);
    }
    
    tracing::info!(peer = %addr, "Peer connection handler completed");
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
        let hop_type = pb::NextHopType::try_from(pb_nh.r#type).unwrap_or(pb::NextHopType::Standard);
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

trait IpAddrBytes {
    fn addr_bytes_vec(&self) -> Vec<u8>;
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
            _ => Err(anyhow!("Invalid IP address byte length: {}", slice.len())),
        }
    }
}
