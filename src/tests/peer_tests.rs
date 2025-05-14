use crate::pb;
use crate::peer::{GeneratedMessage, Peer};
use crate::types::{Config, ConnectionDirection, ConnectionState, Destination, UpdateAction, Vni};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use crate::metalbond::MetalBondCommand;

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
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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
 * Tests the peer's ability to send a hello message.
 *
 * Expected behavior:
 * * 1. The peer sends the hello message
 * * 2. The message contains the correct keepalive_interval from the config
 * * 3. The message is sent immediately
 */
#[tokio::test]
async fn test_peer_send_hello() {
    // Create peer
    let config = Arc::new(Config::default());
    let (mb_tx, _) = mpsc::channel(10);
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, mut wire_rx) = Peer::new(mb_tx, config.clone(), remote_addr, ConnectionDirection::Outgoing);

    // Send a hello message
    let result = timeout(Duration::from_millis(500), peer.send_hello())
        .await
        .expect("send_hello timed out");
    // Check that no errors were returned
    assert!(result.is_ok());

    // Check that the message was sent on the wire
    if let Some(msg) = timeout(Duration::from_millis(100), wire_rx.recv()).await.unwrap() {
        match msg {
            GeneratedMessage::Hello(hello) => {
                // Check server flag was set based on direction (outgoing = client)
                assert_eq!(hello.is_server, false);
                // Check keepalive interval
                assert_eq!(hello.keepalive_interval, config.keepalive_interval.as_secs() as u32);
            }
            _ => panic!("Expected Hello message"),
        }
    } else {
        panic!("No message received from send_hello");
    }
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
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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
    let (peer, mut wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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
    let (peer, _wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

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

#[tokio::test]
async fn test_auto_subscription_on_established() {
    // Setup mock metalbond channel to capture commands
    let (mb_tx, mut mb_rx) = mpsc::channel(10);
    let config = Arc::new(Config::default());
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    
    // Create peer as client (is_server = false)
    let (peer, mut wire_rx) = Peer::new(
        mb_tx, 
        config, 
        remote_addr, 
        ConnectionDirection::Outgoing
    );

    // Set up fake VNIs that will be returned when the peer asks for subscriptions
    let test_vnis = vec![10, 20, 30];
    let test_vnis_clone = test_vnis.clone();
    
    // Spawn a task to respond to GetSubscribedVnis command
    tokio::spawn(async move {
        while let Some(cmd) = mb_rx.recv().await {
            if let MetalBondCommand::GetSubscribedVnis(tx) = cmd {
                let _ = tx.send(test_vnis_clone.clone());
            }
        }
    });
    
    // Simulate connection becoming established:
    // 1. First set to HelloSent
    peer.set_state(ConnectionState::HelloSent);
    
    // 2. Then set hello_received to true
    peer.set_hello_received(true);
    
    // 3. Finally transition to Established
    peer.set_state(ConnectionState::Established);
    
    // Verify the state change
    let state = peer.get_state().await.unwrap();
    assert_eq!(state, ConnectionState::Established);
    
    // Now we should see subscription messages being sent for our test VNIs
    let mut subscription_count = 0;
    
    // Wait for subscription messages, with a timeout
    for _ in 0..test_vnis.len() {
        match timeout(Duration::from_millis(500), wire_rx.recv()).await {
            Ok(Some(msg)) => {
                match msg {
                    GeneratedMessage::Subscribe(sub) => {
                        assert!(test_vnis.contains(&sub.vni), "Unexpected VNI: {}", sub.vni);
                        subscription_count += 1;
                    }
                    _ => panic!("Expected Subscribe message, got {:?}", msg),
                }
            }
            Ok(None) => break,
            Err(_) => {
                // Timeout waiting for message
                break;
            }
        }
    }
    
    assert_eq!(subscription_count, test_vnis.len(), 
               "Expected {} subscription messages, got {}", 
               test_vnis.len(), subscription_count);
}

/**
 * Tests the basic creation of a peer.
 *
 * Expected behavior:
 * * 1. Peer is created with the provided parameters
 * * 2. Initial connection state is Connecting
 * * 3. Remote address matches the provided address
 */
#[tokio::test]
async fn test_peer_basic_creation() {
    // Create channels
    let (mb_tx, _mb_rx) = mpsc::channel(10);

    // Create config
    let config = Arc::new(Config::default());

    // Create peer
    let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 4711);
    let (peer, _wire_rx) = Peer::new(mb_tx, config, remote_addr, ConnectionDirection::Outgoing);

    // Verify peer properties
    assert_eq!(peer.get_remote_addr(), remote_addr);

    // Check initial state
    let state = timeout(Duration::from_millis(500), peer.get_state())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, ConnectionState::Connecting);
}
