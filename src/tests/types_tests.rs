use crate::pb;
use crate::types::{ConnectionState, Destination, IpVersion, NextHop, UpdateAction};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/**
 * Tests the string representation of Destination objects.
 * Verifies that:
 * 1. IPv4 Destination objects display correctly with CIDR notation
 * 2. IPv6 Destination objects display correctly with CIDR notation
 */
#[test]
fn test_destination_display() {
    let dest = Destination {
        prefix: IpNet::V4("192.168.1.0/24".parse().unwrap()),
    };
    assert_eq!(dest.to_string(), "192.168.1.0/24");

    let dest_v6 = Destination {
        prefix: IpNet::V6("2001:db8::/64".parse().unwrap()),
    };
    assert_eq!(dest_v6.to_string(), "2001:db8::/64");
}

/**
 * Tests parsing Destination objects from strings.
 * Verifies that:
 * 1. IPv4 CIDR strings can be parsed into Destination objects
 * 2. IPv6 CIDR strings can be parsed into Destination objects
 * 3. Invalid strings result in appropriate errors
 */
#[test]
fn test_destination_from_str() {
    let dest = Destination::from_str("192.168.1.0/24").unwrap();
    assert_eq!(dest.prefix, IpNet::V4("192.168.1.0/24".parse().unwrap()));

    let dest_v6 = Destination::from_str("2001:db8::/64").unwrap();
    assert_eq!(dest_v6.prefix, IpNet::V6("2001:db8::/64".parse().unwrap()));

    // Test invalid format
    let result = Destination::from_str("invalid");
    assert!(result.is_err());
}

/**
 * Tests the IP version detection of Destination objects.
 * Verifies that:
 * 1. IPv4 Destinations correctly report their IP version as V4
 * 2. IPv6 Destinations correctly report their IP version as V6
 */
#[test]
fn test_destination_ip_version() {
    let dest_v4 = Destination {
        prefix: IpNet::V4("192.168.1.0/24".parse().unwrap()),
    };
    assert_eq!(dest_v4.ip_version(), IpVersion::V4);

    let dest_v6 = Destination {
        prefix: IpNet::V6("2001:db8::/64".parse().unwrap()),
    };
    assert_eq!(dest_v6.ip_version(), IpVersion::V6);
}

/**
 * Tests the string representation of NextHop objects.
 * Verifies that:
 * 1. IPv4 NextHop objects display correctly with VNI
 * 2. NextHop objects without a VNI display correctly
 * 3. IPv6 NextHop objects display correctly with VNI
 */
#[test]
fn test_next_hop_display() {
    let nh = NextHop {
        target_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        target_vni: 200,
        hop_type: pb::NextHopType::Standard,
        nat_port_range_from: 0,
        nat_port_range_to: 0,
    };
    assert_eq!(nh.to_string(), "10.0.0.1 (VNI: 200)");

    let nh_no_vni = NextHop {
        target_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        target_vni: 0,
        hop_type: pb::NextHopType::Standard,
        nat_port_range_from: 0,
        nat_port_range_to: 0,
    };
    assert_eq!(nh_no_vni.to_string(), "10.0.0.1");

    let nh_v6 = NextHop {
        target_address: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        target_vni: 200,
        hop_type: pb::NextHopType::Standard,
        nat_port_range_from: 0,
        nat_port_range_to: 0,
    };
    assert_eq!(nh_v6.to_string(), "2001:db8::1 (VNI: 200)");
}

/**
 * Tests the string representation of ConnectionState variants.
 * Verifies that each connection state is displayed with its correct string representation:
 * 1. CONNECTING
 * 2. HELLO_SENT
 * 3. HELLO_RECEIVED
 * 4. ESTABLISHED
 * 5. RETRY
 * 6. CLOSED
 */
#[test]
fn test_connection_state_display() {
    assert_eq!(ConnectionState::Connecting.to_string(), "CONNECTING");
    assert_eq!(ConnectionState::HelloSent.to_string(), "HELLO_SENT");
    assert_eq!(ConnectionState::HelloReceived.to_string(), "HELLO_RECEIVED");
    assert_eq!(ConnectionState::Established.to_string(), "ESTABLISHED");
    assert_eq!(ConnectionState::Retry.to_string(), "RETRY");
    assert_eq!(ConnectionState::Closed.to_string(), "CLOSED");
}

/**
 * Tests conversion between IpVersion and protobuf IpVersion enums.
 * Verifies that:
 * 1. pb::IpVersion::IPv4 correctly converts to IpVersion::V4
 * 2. pb::IpVersion::IPv6 correctly converts to IpVersion::V6
 * 3. IpVersion::V4 correctly converts to pb::IpVersion::IPv4
 * 4. IpVersion::V6 correctly converts to pb::IpVersion::IPv6
 */
#[test]
fn test_ip_version_conversion() {
    // Test conversion from protobuf enum to our enum
    assert_eq!(IpVersion::from(pb::IpVersion::IPv4), IpVersion::V4);
    assert_eq!(IpVersion::from(pb::IpVersion::IPv6), IpVersion::V6);

    // Test conversion from our enum to protobuf enum
    assert_eq!(pb::IpVersion::from(IpVersion::V4), pb::IpVersion::IPv4);
    assert_eq!(pb::IpVersion::from(IpVersion::V6), pb::IpVersion::IPv6);
}

/**
 * Tests conversion between UpdateAction and protobuf Action enums.
 * Verifies that:
 * 1. pb::Action::Add correctly converts to UpdateAction::Add
 * 2. pb::Action::Remove correctly converts to UpdateAction::Remove
 * 3. UpdateAction::Add correctly converts to pb::Action::Add
 * 4. UpdateAction::Remove correctly converts to pb::Action::Remove
 */
#[test]
fn test_update_action_conversion() {
    // Test conversion from protobuf enum to our enum
    assert_eq!(UpdateAction::from(pb::Action::Add), UpdateAction::Add);
    assert_eq!(UpdateAction::from(pb::Action::Remove), UpdateAction::Remove);

    // Test conversion from our enum to protobuf enum
    assert_eq!(pb::Action::from(UpdateAction::Add), pb::Action::Add);
    assert_eq!(pb::Action::from(UpdateAction::Remove), pb::Action::Remove);
}

/**
 * Stub for testing Destination conversion between application and protocol buffer types.
 * Note: The actual implementation references functions in peer.rs.
 */
#[test]
fn test_destination_conversion() {
    // We test the TryFrom/Into implementations between Destination and pb::Destination
    // using the functions in peer.rs

    // Test in a separate test module when needed
}

/**
 * Stub for testing NextHop conversion between application and protocol buffer types.
 * Note: The actual implementation references functions in peer.rs.
 */
#[test]
fn test_next_hop_conversion() {
    // We test the TryFrom/Into implementations between NextHop and pb::NextHop
    // using the functions in peer.rs

    // Test in a separate test module when needed
}
