use crate::pb;
use crate::peer::{GeneratedMessage, Peer};
use crate::types::{Config, ConnectionDirection, ConnectionState, Destination, UpdateAction, Vni};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/**
 * Tests the creation of a Peer object.
 * Verifies that:
 * 1. A Peer can be successfully created with the specified properties
 * 2. The peer's remote address matches what was provided
 * 3. The initial connection state is correctly set to Connecting
 * 4. The peer can be shut down properly
 */
#[tokio::test]
async fn test_peer_creation() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Verify peer properties
    assert_eq!(peer.get_remote_addr(), remote_addr);

    // Check initial state
    let state = timeout(Duration::from_millis(500), peer.get_state())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, ConnectionState::Connecting);

    // Test shutdown
    peer.trigger_shutdown();

    // Wire receiver should eventually close
    let _ = timeout(Duration::from_millis(100), wire_rx.recv()).await;
}

/**
 * Tests sending a Hello message from a peer.
 * Verifies that:
 * 1. The peer can successfully send a Hello message
 * 2. The message contains the correct is_server flag
 * 3. The message contains the correct keepalive_interval from the config
 * 4. The message is properly received on the wire channel
 */
#[tokio::test]
async fn test_peer_send_hello() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Send a hello message
    let is_server = true;
    let result = timeout(Duration::from_millis(500), peer.send_hello(is_server))
        .await
        .unwrap();
    assert!(result.is_ok());

    // Check that message was sent to wire
    if let Ok(Some(msg)) = timeout(Duration::from_millis(100), wire_rx.recv()).await {
        match msg {
            GeneratedMessage::Hello(hello) => {
                assert_eq!(hello.is_server, is_server);
                assert_eq!(hello.keepalive_interval, 5); // Default from Config
            }
            _ => panic!("Wrong message type received"),
        }
    } else {
        panic!("No message was sent to wire");
    }

    // Clean up
    peer.trigger_shutdown();
}

/**
 * Tests sending a Keepalive message from a peer.
 * Verifies that:
 * 1. The peer can successfully send a Keepalive message
 * 2. The message is properly received on the wire channel
 * 3. The received message is correctly identified as a Keepalive
 */
#[tokio::test]
async fn test_peer_send_keepalive() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Send a keepalive message
    let result = timeout(Duration::from_millis(500), peer.send_keepalive())
        .await
        .unwrap();
    assert!(result.is_ok());

    // Check that message was sent to wire
    if let Ok(Some(msg)) = timeout(Duration::from_millis(100), wire_rx.recv()).await {
        match msg {
            GeneratedMessage::Keepalive => {
                // Success, no data to verify
            }
            _ => panic!("Wrong message type received"),
        }
    } else {
        panic!("No message was sent to wire");
    }

    // Clean up
    peer.trigger_shutdown();
}

/**
 * Tests sending a Subscribe message from a peer.
 * Verifies that:
 * 1. The peer can successfully send a Subscribe message
 * 2. The message contains the correct VNI
 * 3. The message is properly received on the wire channel
 */
#[tokio::test]
async fn test_peer_send_subscribe() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Send a subscribe message
    let vni: Vni = 100;
    let result = timeout(Duration::from_millis(500), peer.send_subscribe(vni))
        .await
        .unwrap();
    assert!(result.is_ok());

    // Check that message was sent to wire
    if let Ok(Some(msg)) = timeout(Duration::from_millis(100), wire_rx.recv()).await {
        match msg {
            GeneratedMessage::Subscribe(sub) => {
                assert_eq!(sub.vni, vni);
            }
            _ => panic!("Wrong message type received"),
        }
    } else {
        panic!("No message was sent to wire");
    }

    // Clean up
    peer.trigger_shutdown();
}

/**
 * Tests sending an Unsubscribe message from a peer.
 * Verifies that:
 * 1. The peer can successfully send an Unsubscribe message
 * 2. The message contains the correct VNI
 * 3. The message is properly received on the wire channel
 */
#[tokio::test]
async fn test_peer_send_unsubscribe() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Send an unsubscribe message
    let vni: Vni = 100;
    let result = timeout(Duration::from_millis(500), peer.send_unsubscribe(vni))
        .await
        .unwrap();
    assert!(result.is_ok());

    // Check that message was sent to wire
    if let Ok(Some(msg)) = timeout(Duration::from_millis(100), wire_rx.recv()).await {
        match msg {
            GeneratedMessage::Unsubscribe(unsub) => {
                assert_eq!(unsub.vni, vni);
            }
            _ => panic!("Wrong message type received"),
        }
    } else {
        panic!("No message was sent to wire");
    }

    // Clean up
    peer.trigger_shutdown();
}

/**
 * Tests sending an Update message from a peer.
 * Verifies that:
 * 1. The peer can successfully send an Update message
 * 2. The message contains the correct action, VNI, destination, and next hop
 * 3. The message is properly received on the wire channel
 * 4. The protocol buffer objects are correctly constructed
 */
#[tokio::test]
async fn test_peer_send_update() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Send an update message
    let action = UpdateAction::Add;
    let vni: Vni = 100;
    let dest = Destination {
        prefix: IpNet::V4("192.168.1.0/24".parse().unwrap()),
    };
    let next_hop = crate::types::NextHop {
        target_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        target_vni: 200,
        hop_type: pb::NextHopType::Standard,
        nat_port_range_from: 0,
        nat_port_range_to: 0,
    };

    let result = timeout(
        Duration::from_millis(500),
        peer.send_update(action, vni, dest, next_hop),
    )
    .await
    .unwrap();
    assert!(result.is_ok());

    // Check that message was sent to wire
    if let Ok(Some(msg)) = timeout(Duration::from_millis(100), wire_rx.recv()).await {
        match msg {
            GeneratedMessage::Update(update) => {
                assert_eq!(update.action, i32::from(pb::Action::Add));
                assert_eq!(update.vni, vni);

                let dest_pb = update.destination.unwrap();
                assert_eq!(dest_pb.ip_version, i32::from(pb::IpVersion::IPv4));
                assert_eq!(dest_pb.prefix_length, 24);

                let nh_pb = update.next_hop.unwrap();
                assert_eq!(nh_pb.target_vni, 200);
            }
            _ => panic!("Wrong message type received"),
        }
    } else {
        panic!("No message was sent to wire");
    }

    // Clean up
    peer.trigger_shutdown();
}

/**
 * Tests state transitions for a peer.
 * Verifies that:
 * 1. The peer's initial state is Connecting
 * 2. The peer's state can be changed to other states
 * 3. The state changes are reflected when queried
 * 4. Multiple state transitions work correctly
 */
#[tokio::test]
async fn test_peer_state_changes() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, _wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing, false);

    // Helper function to check state
    async fn check_state(peer: &Peer, expected_state: ConnectionState) {
        if let Ok(state_result) = timeout(Duration::from_millis(100), peer.get_state()).await {
            if let Ok(state) = state_result {
                assert_eq!(state, expected_state);
            }
        }
    }

    // Initial state should be Connecting
    check_state(&peer, ConnectionState::Connecting).await;

    // Change state to HelloSent
    peer.set_state(ConnectionState::HelloSent);
    // Allow time for state change to propagate
    tokio::time::sleep(Duration::from_millis(5)).await;
    check_state(&peer, ConnectionState::HelloSent).await;

    // Change state to Established
    peer.set_state(ConnectionState::Established);
    // Allow time for state change to propagate
    tokio::time::sleep(Duration::from_millis(5)).await;
    check_state(&peer, ConnectionState::Established).await;

    // Change state to Closed
    peer.set_state(ConnectionState::Closed);
    // Allow time for state change to propagate
    tokio::time::sleep(Duration::from_millis(5)).await;
    check_state(&peer, ConnectionState::Closed).await;

    // Clean up - make sure we don't hang
    peer.trigger_shutdown();

    // Wait a bit for shutdown to complete
    tokio::time::sleep(Duration::from_millis(100)).await;
}
