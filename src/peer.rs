use crate::metalbond::MetalBondCommand;
use crate::pb;
use crate::types::{
    Config, ConnectionDirection, ConnectionState, Destination, MessageType, NextHop, UpdateAction,
    Vni,
};
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
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
                    let result = Self::handle_send_hello(&state, is_server).await;
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
                PeerMessage::Shutdown => {
                    tracing::debug!(peer = %state.remote_addr, "Peer actor shutting down");
                    break;
                }
            }
        }
        tracing::info!(peer = %state.remote_addr, "Peer actor terminated");
    }

    // Helper methods for the actor
    async fn handle_send_hello(state: &PeerState, is_server: bool) -> Result<()> {
        let hello = pb::Hello {
            keepalive_interval: state.config.keepalive_interval.as_secs() as u32,
            is_server,
        };
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
}

// Rewrite the handle_peer function to work with our actor model
pub async fn handle_peer(
    _peer: Arc<Peer>,
    _stream: TcpStream,
    _wire_rx: mpsc::Receiver<GeneratedMessage>,
) {
    // Function implementation
    // ...
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
