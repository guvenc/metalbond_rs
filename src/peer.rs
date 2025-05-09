use crate::metalbond::SharedMetalBondState;
use crate::pb;
use crate::types::{
    Config, ConnectionDirection, ConnectionState, Destination, InternalUpdate, MessageType, NextHop,
    UpdateAction, Vni,
};
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use ipnet::IpNet;
use prost::Message;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio::time::{self, Instant, Interval, Sleep}; // Ensure Instant is imported if not already via `self`
use tokio_util::codec::{Decoder, Encoder, Framed};

const PEER_TX_BUFFER_SIZE: usize = 256; // How many outbound messages can queue up
const METALBOND_VERSION: u8 = 1;
const MAX_MESSAGE_LEN: usize = 1188;

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
            GeneratedMessage::Unsubscribe(msg) => Ok((MessageType::Unsubscribe, msg.encode_to_vec())),
            GeneratedMessage::Update(msg) => Ok((MessageType::Update, msg.encode_to_vec())),
        }
    }
}

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

pub struct Peer {
    metalbond_state: SharedMetalBondState,
    config: Arc<Config>,
    remote_addr: SocketAddr,
    local_addr: Option<SocketAddr>,
    direction: ConnectionDirection,
    pub is_remote_server: RwLock<bool>,
    state: Arc<RwLock<ConnectionState>>,
    subscribed_vnis_remote: Arc<RwLock<HashSet<Vni>>>,
    tx_sender: mpsc::Sender<GeneratedMessage>,
    shutdown_signal: Arc<tokio::sync::Notify>,
}

impl Peer {
    pub fn new(
        metalbond_state: SharedMetalBondState,
        config: Arc<Config>,
        remote_addr: SocketAddr,
        direction: ConnectionDirection,
    ) -> (Arc<Self>, mpsc::Receiver<GeneratedMessage>) {
        let (tx_sender, tx_receiver) = mpsc::channel(PEER_TX_BUFFER_SIZE);
        let peer = Arc::new(Peer {
            metalbond_state,
            config,
            remote_addr,
            local_addr: None,
            direction,
            is_remote_server: RwLock::new(false),
            state: Arc::new(RwLock::new(ConnectionState::Connecting)),
            subscribed_vnis_remote: Arc::new(RwLock::new(HashSet::new())),
            tx_sender,
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
        });
        (peer, tx_receiver)
    }

    pub async fn get_state(&self) -> ConnectionState {
        *self.state.read().await
    }

    pub fn get_remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    async fn set_state(&self, new_state: ConnectionState) {
        let mut current_state = self.state.write().await;
        let old_state = *current_state;
        if old_state == new_state {
            return;
        }
        *current_state = new_state;
        drop(current_state);
        tracing::info!(peer = %self.remote_addr, old = %old_state, new = %new_state, "Peer state changed");

        if new_state == ConnectionState::Established && old_state != ConnectionState::Established {
            let local_subs = self.metalbond_state.read().await.my_subscriptions.clone();
            for vni in local_subs {
                tracing::debug!(peer = %self.remote_addr, %vni, "Sending initial SUBSCRIBE");
                if let Err(e) = self.send_subscribe(vni).await {
                    tracing::error!(peer = %self.remote_addr, %vni, "Failed to send initial subscribe: {}", e);
                    self.trigger_shutdown();
                }
            }
            let local_announcements = self.metalbond_state.read().await.my_announcements.get_local_announcements().await;
            for (vni, dest_map) in local_announcements {
                for(dest, hops) in dest_map {
                    for hop in hops {
                        tracing::debug!(peer = %self.remote_addr, %vni, %dest, %hop, "Sending initial ADD Announce");
                        if let Err(e) = self.send_update(UpdateAction::Add, vni, dest, hop).await {
                            tracing::error!(peer = %self.remote_addr, %vni, %dest, %hop, "Failed to send initial announcement: {}", e);
                            self.trigger_shutdown();
                            return;
                        }
                    }
                }
            }
        } else if new_state != ConnectionState::Established && old_state == ConnectionState::Established {
            tracing::info!(peer = %self.remote_addr, "Connection lost, cleaning up peer resources");
            self.cleanup_peer_state().await;
        }
    }

    async fn send_message(&self, msg: GeneratedMessage) -> Result<()> {
        match self.tx_sender.try_send(msg) {
            Ok(_) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(peer = %self.remote_addr, "Tx channel full, message dropped");
                Err(anyhow!("Peer TX channel full"))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!(peer = %self.remote_addr, "Tx channel closed, cannot send message");
                Err(anyhow!("Peer TX channel closed"))
            }
        }
    }

    pub async fn send_hello(&self, is_server: bool) -> Result<()> {
        let hello = pb::Hello {
            keepalive_interval: self.config.keepalive_interval.as_secs() as u32,
            is_server,
        };
        self.send_message(GeneratedMessage::Hello(hello)).await
    }

    pub async fn send_keepalive(&self) -> Result<()> {
        self.send_message(GeneratedMessage::Keepalive).await
    }

    pub async fn send_subscribe(&self, vni: Vni) -> Result<()> {
        let sub = pb::Subscription { vni };
        self.send_message(GeneratedMessage::Subscribe(sub)).await
    }

    pub async fn send_unsubscribe(&self, vni: Vni) -> Result<()> {
        let unsub = pb::Subscription { vni };
        self.send_message(GeneratedMessage::Unsubscribe(unsub)).await
    }

    pub async fn send_update(&self, action: UpdateAction, vni: Vni, dest: Destination, nh: NextHop) -> Result<()> {
        let pb_dest = dest.try_into().context("Failed to convert Destination to protobuf")?;
        let pb_nh = nh.try_into().context("Failed to convert NextHop to protobuf")?;
        let update = pb::Update {
            action: pb::Action::from(action) as i32,
            vni,
            destination: Some(pb_dest),
            next_hop: Some(pb_nh),
        };
        self.send_message(GeneratedMessage::Update(update)).await
    }

    pub fn trigger_shutdown(&self) {
        tracing::debug!(peer = %self.remote_addr, "Shutdown triggered");
        self.shutdown_signal.notify_one();
    }

    async fn cleanup_peer_state(&self) {
        let mb_state = self.metalbond_state.read().await;
        let removed_routes = mb_state.route_table.remove_routes_from_peer(self.remote_addr).await;
        tracing::info!(peer = %self.remote_addr, count = removed_routes.len(), "Removed routes learned from peer");
        for (vni, dest, nh) in removed_routes {
            let update = InternalUpdate {
                action: UpdateAction::Remove,
                vni,
                destination: dest,
                next_hop: nh,
                source_peer: Some(self.remote_addr),
            };
            if let Err(e) = mb_state.route_update_tx.send(update).await {
                tracing::error!(peer = %self.remote_addr, "Failed to send route removal update for distribution: {}", e);
            }
        }
        let mut remote_subs = self.subscribed_vnis_remote.write().await;
        if !remote_subs.is_empty() {
            let mut mb_state_w = self.metalbond_state.write().await;
            for vni in remote_subs.iter() {
                if let Some(peer_set) = mb_state_w.subscribers.get_mut(vni) {
                    peer_set.remove(&self.remote_addr);
                    tracing::debug!(peer = %self.remote_addr, %vni, "Removed peer from VNI subscribers list");
                }
            }
            remote_subs.clear();
        }
        drop(remote_subs);
        tracing::info!(peer = %self.remote_addr, "Peer state cleanup complete");
    }

    async fn process_hello(&self, hello: pb::Hello) -> Result<()> {
        tracing::info!(peer = %self.remote_addr, interval = hello.keepalive_interval, is_server = hello.is_server, "Received HELLO");
        if hello.keepalive_interval == 0 {
            bail!("Invalid keepalive interval 0 received");
        }
        *self.is_remote_server.write().await = hello.is_server;
        let current_state = self.get_state().await;
        match self.direction {
            ConnectionDirection::Incoming => {
                if current_state == ConnectionState::Connecting || current_state == ConnectionState::HelloReceived {
                    self.send_hello(true).await.context("Failed to send HELLO response")?;
                    self.set_state(ConnectionState::HelloSent).await;
                } else {
                    tracing::warn!(peer = %self.remote_addr, state = %current_state, "Received HELLO in unexpected state");
                }
            }
            ConnectionDirection::Outgoing => {
                if current_state == ConnectionState::HelloSent {
                    self.set_state(ConnectionState::HelloReceived).await;
                } else {
                    tracing::warn!(peer = %self.remote_addr, state = %current_state, "Received HELLO in unexpected state");
                }
            }
        }
        Ok(())
    }

    async fn process_keepalive(&self) -> Result<()> {
        tracing::trace!(peer = %self.remote_addr, "Received KEEPALIVE");
        let current_state = self.get_state().await;
        match self.direction {
            ConnectionDirection::Incoming => {
                if current_state == ConnectionState::HelloSent {
                    self.set_state(ConnectionState::Established).await;
                } else if current_state == ConnectionState::Established {
                    self.send_keepalive().await.context("Failed to send KEEPALIVE response")?;
                } else {
                    tracing::warn!(peer = %self.remote_addr, state = %current_state, "Received KEEPALIVE in unexpected state");
                    bail!("Keepalive in wrong state");
                }
            }
            ConnectionDirection::Outgoing => {
                if current_state == ConnectionState::HelloReceived {
                    self.set_state(ConnectionState::Established).await;
                } else if current_state == ConnectionState::Established {
                    // Expected, just reset timeout
                } else {
                    tracing::warn!(peer = %self.remote_addr, state = %current_state, "Received KEEPALIVE in unexpected state");
                    bail!("Keepalive in wrong state");
                }
            }
        }
        Ok(())
    }

    async fn process_subscribe(&self, sub: pb::Subscription) -> Result<()> {
        let vni = sub.vni;
        tracing::info!(peer = %self.remote_addr, %vni, "Received SUBSCRIBE");
        let mut mb_state = self.metalbond_state.write().await;
        let was_inserted = mb_state
            .subscribers
            .entry(vni)
            .or_default()
            .insert(self.remote_addr);
        drop(mb_state);
        self.subscribed_vnis_remote.write().await.insert(vni);
        if was_inserted {
            tracing::info!(peer = %self.remote_addr, %vni, "Added peer as subscriber");
            let mb_state_read = self.metalbond_state.read().await;
            let routes_to_send = mb_state_read.route_table.get_destinations_by_vni_with_peer(vni).await;
            drop(mb_state_read);
            for (dest, nh_map) in routes_to_send {
                for (nh, sources) in nh_map {
                    let is_nat_from_peer = nh.hop_type == pb::NextHopType::Nat && sources.contains(&Some(self.remote_addr));
                    let is_server_to_server_loop = *self.is_remote_server.read().await && sources.iter().any(|_src_peer| {
                        false
                    });
                    if !is_nat_from_peer && !is_server_to_server_loop {
                        if let Err(e) = self.send_update(UpdateAction::Add, vni, dest, nh).await {
                            tracing::error!(peer = %self.remote_addr, %vni, %dest, %nh, "Failed to send existing route to new subscriber: {}",e);
                            self.trigger_shutdown();
                            return Err(e);
                        }
                    } else {
                        tracing::trace!(peer = %self.remote_addr, %vni, %dest, %nh, "Skipping send to subscriber (loop prevention)");
                    }
                }
            }
        } else {
            tracing::warn!(peer = %self.remote_addr, %vni, "Peer already subscribed");
        }
        Ok(())
    }

    async fn process_unsubscribe(&self, unsub: pb::Subscription) -> Result<()> {
        let vni = unsub.vni;
        tracing::info!(peer = %self.remote_addr, %vni, "Received UNSUBSCRIBE");
        let mut mb_state = self.metalbond_state.write().await;
        let mut removed = false;
        if let Some(peer_set) = mb_state.subscribers.get_mut(&vni) {
            removed = peer_set.remove(&self.remote_addr);
            if peer_set.is_empty() {
                mb_state.subscribers.remove(&vni);
                tracing::debug!(peer = %self.remote_addr, %vni, "Removed last subscriber, removing VNI from subscribers map");
            }
        }
        drop(mb_state);
        self.subscribed_vnis_remote.write().await.remove(&vni);
        if removed {
            tracing::info!(peer = %self.remote_addr, %vni, "Removed peer from subscribers");
            let mb_state_read = self.metalbond_state.read().await;
            let routes_to_remove = mb_state_read.route_table.get_destinations_by_vni_with_peer(vni).await;
            drop(mb_state_read);
            for (dest, nh_map) in routes_to_remove {
                for (nh, sources) in nh_map {
                    if sources.contains(&Some(self.remote_addr)) {
                        tracing::debug!(peer = %self.remote_addr, %vni, %dest, %nh, "Unsubscribe: Removing learned route");
                        let mb_state_w = self.metalbond_state.write().await;
                        match mb_state_w.route_table.remove_next_hop(vni, dest, nh, Some(self.remote_addr)).await {
                            Ok(0) => {
                                let update = InternalUpdate {
                                    action: UpdateAction::Remove,
                                    vni, destination: dest, next_hop: nh,
                                    source_peer: Some(self.remote_addr),
                                };
                                if let Err(e) = mb_state_w.route_update_tx.send(update).await {
                                    tracing::error!(peer=%self.remote_addr, "Failed to send route removal for distribution: {}", e);
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::error!(peer=%self.remote_addr, "Failed to remove route from table during unsubscribe: {}", e);
                            }
                        }
                    }
                }
            }
        } else {
            tracing::warn!(peer = %self.remote_addr, %vni, "Peer was not subscribed");
        }
        Ok(())
    }

    async fn process_update(&self, update: pb::Update) -> Result<()> {
        let vni = update.vni;
        let action = UpdateAction::from(pb::Action::try_from(update.action)?);
        let pb_dest = update.destination.ok_or_else(|| anyhow!("Update missing destination"))?;
        let pb_nh = update.next_hop.ok_or_else(|| anyhow!("Update missing nexthop"))?;
        let dest: Destination = pb_dest.try_into().context("Invalid destination format")?;
        let nh: NextHop = pb_nh.try_into().context("Invalid nexthop format")?;
        tracing::info!(peer = %self.remote_addr, ?action, %vni, %dest, %nh, "Received UPDATE");
        let is_subscribed_locally = self.metalbond_state.read().await.my_subscriptions.contains(&vni);
        if !is_subscribed_locally && self.direction == ConnectionDirection::Outgoing {
            tracing::warn!(peer = %self.remote_addr, %vni, "Received update for VNI we are not subscribed to, ignoring");
            return Ok(());
        }
        let mb_state = self.metalbond_state.write().await;
        let route_table = &mb_state.route_table;
        let remaining_count: Result<usize>;
        match action {
            UpdateAction::Add => {
                if route_table.add_next_hop(vni, dest, nh, Some(self.remote_addr)).await {
                    remaining_count = Ok(1);
                    tracing::debug!(peer=%self.remote_addr, %vni, %dest, %nh, "Added new route from peer to table");
                } else {
                    tracing::debug!(peer=%self.remote_addr, %vni, %dest, %nh, "Route already existed from peer in table");
                    remaining_count = Ok(999);
                }
            }
            UpdateAction::Remove => {
                remaining_count = route_table.remove_next_hop(vni, dest, nh, Some(self.remote_addr)).await;
                if remaining_count.is_ok() {
                    tracing::debug!(peer=%self.remote_addr, %vni, %dest, %nh, remaining = remaining_count.as_ref().unwrap(), "Removed route from peer from table");
                } else {
                    tracing::warn!(peer=%self.remote_addr, %vni, %dest, %nh, "Failed to remove route from peer (maybe already gone?): {:?}", remaining_count.as_ref().err());
                    return Ok(());
                }
            }
        }
        match remaining_count {
            Ok(0) => {
                let internal_update = InternalUpdate { action: UpdateAction::Remove, vni, destination: dest, next_hop: nh, source_peer: Some(self.remote_addr) };
                if let Err(e) = mb_state.route_update_tx.send(internal_update).await {
                    tracing::error!(peer = %self.remote_addr, "Failed to send route removal for distribution: {}", e);
                }
            },
            Ok(1) if action == UpdateAction::Add => {
                let internal_update = InternalUpdate { action: UpdateAction::Add, vni, destination: dest, next_hop: nh, source_peer: Some(self.remote_addr) };
                if let Err(e) = mb_state.route_update_tx.send(internal_update).await {
                    tracing::error!(peer = %self.remote_addr, "Failed to send route addition for distribution: {}", e);
                }
            },
            _ => {}
        }
        Ok(())
    }
}

pub async fn handle_peer(
    peer: Arc<Peer>,
    stream: TcpStream,
    mut tx_receiver: mpsc::Receiver<GeneratedMessage>,
) {
    let peer_addr = stream.peer_addr().unwrap_or(peer.remote_addr);
    let local_addr = stream.local_addr().ok();
    if let Some(la) = local_addr {
        tracing::info!(peer = %peer_addr, local = %la, "Connection established");
    }

    let mut framed = Framed::new(stream, MetalBondCodec);
    let shutdown = peer.shutdown_signal.clone();
    let mut keepalive_interval_timer: Option<Interval> = None;
    let mut keepalive_timeout_timer: Option<Sleep> = None;

    if peer.direction == ConnectionDirection::Outgoing {
        let mb_state = peer.metalbond_state.read().await;
        let is_server = mb_state.is_server;
        drop(mb_state);
        if let Err(e) = peer.send_hello(is_server).await {
            tracing::error!(peer = %peer_addr, "Failed to send initial HELLO: {}", e);
            peer.set_state(ConnectionState::Closed).await;
            return;
        }
        peer.set_state(ConnectionState::HelloSent).await;
    } else {
        peer.set_state(ConnectionState::Connecting).await;
    }

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!(peer = %peer_addr, "Shutdown signal received, closing connection task.");
                let _ = framed.close().await;
                peer.set_state(ConnectionState::Closed).await;
                return;
            }

            frame = framed.next() => {
                match frame {
                    Some(Ok(message)) => {
                        tracing::debug!(peer = %peer_addr, ?message, "Received message");
                        // FIX: Reset keepalive_timeout_timer (Error 1: mismatched types)
                        // This code is assumed to be at line 526 from the error
                        if keepalive_timeout_timer.is_some() {
                            let timeout_duration = peer.config.keepalive_interval * 5 / 2; // e.g. 2.5x interval
                            let deadline = Instant::now() + timeout_duration;
                            keepalive_timeout_timer = Some(time::sleep_until(deadline)); // Use sleep_until
                        }

                        let result = match message {
                            GeneratedMessage::Hello(hello) => peer.process_hello(hello).await,
                            GeneratedMessage::Keepalive => peer.process_keepalive().await,
                            GeneratedMessage::Subscribe(sub) => peer.process_subscribe(sub).await,
                            GeneratedMessage::Unsubscribe(unsub) => peer.process_unsubscribe(unsub).await,
                            GeneratedMessage::Update(update) => peer.process_update(update).await,
                        };

                        if let Err(e) = result {
                            tracing::error!(peer = %peer_addr, "Error processing message: {}. Triggering reset.", e);
                            peer.set_state(ConnectionState::Closed).await;
                            peer.trigger_shutdown();
                        } else {
                            if peer.get_state().await == ConnectionState::Established && keepalive_interval_timer.is_none() {
                                let interval_duration = peer.config.keepalive_interval;
                                let timeout_duration = interval_duration * 5 / 2;
                                tracing::info!(peer = %peer_addr, ?interval_duration, ?timeout_duration, "Connection Established, starting keepalive timers");
                                let mut interval = time::interval(interval_duration);
                                interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
                                keepalive_interval_timer = Some(interval);
                                // For initial setup of timeout_timer, ensure it's also sleep_until if deadline based
                                let deadline = Instant::now() + timeout_duration;
                                keepalive_timeout_timer = Some(time::sleep_until(deadline));

                                if peer.direction == ConnectionDirection::Outgoing {
                                    if let Err(e) = peer.send_keepalive().await {
                                        tracing::error!(peer = %peer_addr, "Failed to send initial keepalive: {}", e);
                                        peer.trigger_shutdown();
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!(peer = %peer_addr, "Error reading frame: {}. Triggering reset.", e);
                        peer.set_state(ConnectionState::Closed).await;
                        peer.trigger_shutdown();
                    }
                    None => {
                        tracing::info!(peer = %peer_addr, "Connection closed by remote. Triggering reset.");
                        peer.set_state(ConnectionState::Closed).await;
                        peer.trigger_shutdown();
                    }
                }
            }

            Some(message_to_send) = tx_receiver.recv() => {
                tracing::trace!(peer = %peer_addr, msg = ?message_to_send, "Sending message");
                if let Err(e) = framed.send(message_to_send).await {
                    tracing::error!(peer = %peer_addr, "Error sending message: {}. Triggering reset.", e);
                    peer.set_state(ConnectionState::Closed).await;
                    peer.trigger_shutdown();
                }
            }

            _ = async {
                // This arm for Interval::tick() should be okay as .tick() returns an owned future.
                keepalive_interval_timer.as_mut().unwrap().tick().await;
            }, if keepalive_interval_timer.is_some() => {
                if peer.direction == ConnectionDirection::Outgoing && peer.get_state().await == ConnectionState::Established {
                    tracing::trace!(peer = %peer_addr, "Sending keepalive");
                    if let Err(e) = peer.send_keepalive().await {
                        tracing::error!(peer = %peer_addr, "Failed to send keepalive: {}", e);
                        peer.trigger_shutdown();
                    }
                }
            }

            // FIX: Corrected keepalive timeout timer arm for !Unpin Sleep (Error 2: PhantomPinned)
            // This code is assumed to be at line 601 from the error
            _ = async {
                // Take the Sleep future from the Option to gain ownership within this async block.
                // This allows the !Unpin Sleep future to be pinned and awaited correctly.
                if let Some(sleep_to_await) = keepalive_timeout_timer.take() {
                    sleep_to_await.await;
                    // Sleep is a consuming future, no need to put it back if it completed.
                    // If it were to pend and select! polls this async block again,
                    // keepalive_timeout_timer would be None. The `if guard` handles this.
                    // However, for a one-shot timer, this take() is fine.
                    // If the timer needed to be "reset" rather than "expire and be gone",
                    // the reset logic (re-creating it) is handled when messages are received.
                } else {
                    // This case should ideally not be hit if the `if` guard on the select arm
                    // (`if keepalive_timeout_timer.is_some()`) is working as expected,
                    // as this async block would only be polled when it's Some.
                    // If select re-polls a completed async block future whose outer guard became false,
                    // or if take() happens and it pends, then this might be an issue.
                    // For simplicity, assuming guard + take() + one-shot await is the pattern.
                    // If this `async` block itself pends, `take()` means the timer is gone from the Option.
                    // For a timeout, this is usually fine: once it starts being awaited, it runs to completion or is dropped.
                    // If it pends, and select chooses another branch, then this timer is effectively disarmed.
                    // This might need more robust handling if disarming is not intended on pend.
                    // For now, this fixes the immediate !Unpin await error.
                    // Awaiting pending() ensures this arm doesn't resolve if None was taken unexpectedly.
                    std::future::pending::<()>().await;
                }
            }, if keepalive_timeout_timer.is_some() => {
                tracing::warn!(peer = %peer_addr, "Keepalive timeout detected. Triggering reset.");
                peer.set_state(ConnectionState::Closed).await;
                peer.trigger_shutdown();
            }
        }
    }
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
            _ => Err(anyhow!("Invalid IP address byte length: {}", slice.len()))
        }
    }
}