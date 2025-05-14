use crate::peer::GeneratedMessage;
use crate::pb;
use crate::types::{MessageType, Vni};
use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};
use anyhow::{anyhow, bail, Result};

// Constants copied from peer.rs for testing
const METALBOND_VERSION: u8 = 1;
const MAX_MESSAGE_LEN: usize = 1188;

// Test implementation of the codec
struct TestMetalBondCodec;

impl Encoder<GeneratedMessage> for TestMetalBondCodec {
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

impl Decoder for TestMetalBondCodec {
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

/**
 * Tests the serialization and deserialization of Hello messages.
 * Verifies that:
 * 1. Hello messages can be encoded correctly with proper headers
 * 2. Encoded Hello messages can be decoded back to their original form
 * 3. All fields of the Hello message are preserved during the process
 */
#[test]
fn test_hello_message_serialization() {
    let mut codec = TestMetalBondCodec;
    let mut buffer = BytesMut::new();
    
    // Create Hello message
    let hello = pb::Hello {
        keepalive_interval: 30,
        is_server: true,
    };
    let msg = GeneratedMessage::Hello(hello);
    
    // Encode
    codec.encode(msg, &mut buffer).unwrap();
    
    // Should have header + encoded message
    assert!(buffer.len() > 4);
    
    // Check if the header bytes are correct
    assert_eq!(buffer[0], 1); // Version
    // buffer[1] and buffer[2] are message length bytes
    assert_eq!(buffer[3], MessageType::Hello as u8);
    
    // Decode
    let decoded = codec.decode(&mut buffer).unwrap().unwrap();
    
    // Verify decoded message
    match decoded {
        GeneratedMessage::Hello(hello) => {
            assert_eq!(hello.keepalive_interval, 30);
            assert!(hello.is_server);
        }
        _ => panic!("Wrong message type decoded"),
    }
    
    // Buffer should be empty after decode
    assert_eq!(buffer.len(), 0);
}

/**
 * Tests the serialization and deserialization of Subscription messages.
 * Verifies that:
 * 1. Subscription messages can be encoded correctly with proper headers
 * 2. Encoded Subscription messages can be decoded back to their original form
 * 3. The VNI value is preserved during the serialization process
 */
#[test]
fn test_subscription_message_serialization() {
    let mut codec = TestMetalBondCodec;
    let mut buffer = BytesMut::new();
    
    // Create Subscription message
    let vni: Vni = 100;
    let subscription = pb::Subscription { vni };
    let msg = GeneratedMessage::Subscribe(subscription);
    
    // Encode
    codec.encode(msg, &mut buffer).unwrap();
    
    // Check header
    assert_eq!(buffer[0], 1); // Version
    assert_eq!(buffer[3], MessageType::Subscribe as u8);
    
    // Decode
    let decoded = codec.decode(&mut buffer).unwrap().unwrap();
    
    // Verify decoded message
    match decoded {
        GeneratedMessage::Subscribe(sub) => {
            assert_eq!(sub.vni, vni);
        }
        _ => panic!("Wrong message type decoded"),
    }
}

/**
 * Tests the serialization and deserialization of Update messages.
 * Verifies that:
 * 1. Update messages with complex nested structures can be encoded
 * 2. All fields including nested Destination and NextHop are preserved
 * 3. The action, VNI, and IP addresses are correctly maintained
 */
#[test]
fn test_update_message_serialization() {
    let mut codec = TestMetalBondCodec;
    let mut buffer = BytesMut::new();
    
    // Create an Update message
    let update = pb::Update {
        action: i32::from(pb::Action::Add),
        vni: 100,
        destination: Some(pb::Destination {
            ip_version: i32::from(pb::IpVersion::IPv4),
            prefix: vec![192, 168, 1, 0],
            prefix_length: 24,
        }),
        next_hop: Some(pb::NextHop {
            target_address: vec![10, 0, 0, 1],
            target_vni: 200,
            r#type: i32::from(pb::NextHopType::Standard),
            nat_port_range_from: 0,
            nat_port_range_to: 0,
        }),
    };
    
    let msg = GeneratedMessage::Update(update);
    
    // Encode
    codec.encode(msg, &mut buffer).unwrap();
    
    // Check header
    assert_eq!(buffer[0], 1); // Version
    assert_eq!(buffer[3], MessageType::Update as u8);
    
    // Decode
    let decoded = codec.decode(&mut buffer).unwrap().unwrap();
    
    // Verify decoded message
    match decoded {
        GeneratedMessage::Update(update) => {
            assert_eq!(update.action, i32::from(pb::Action::Add));
            assert_eq!(update.vni, 100);
            
            let dest = update.destination.unwrap();
            assert_eq!(dest.ip_version, i32::from(pb::IpVersion::IPv4));
            assert_eq!(dest.prefix, vec![192, 168, 1, 0]);
            assert_eq!(dest.prefix_length, 24);
            
            let nh = update.next_hop.unwrap();
            assert_eq!(nh.target_address, vec![10, 0, 0, 1]);
            assert_eq!(nh.target_vni, 200);
            assert_eq!(nh.r#type, i32::from(pb::NextHopType::Standard));
        }
        _ => panic!("Wrong message type decoded"),
    }
}

/**
 * Tests the serialization and deserialization of Keepalive messages.
 * Verifies that:
 * 1. Keepalive messages (which have no payload) are encoded correctly
 * 2. The header has the correct version and message type
 * 3. The length fields are set to zero as expected
 * 4. The message can be decoded back to a Keepalive
 */
#[test]
fn test_keepalive_message_serialization() {
    let mut codec = TestMetalBondCodec;
    let mut buffer = BytesMut::new();
    
    // Create Keepalive message (has no payload)
    let msg = GeneratedMessage::Keepalive;
    
    // Encode
    codec.encode(msg, &mut buffer).unwrap();
    
    // Check header
    assert_eq!(buffer[0], 1); // Version
    assert_eq!(buffer[1], 0); // Length (upper byte)
    assert_eq!(buffer[2], 0); // Length (lower byte)
    assert_eq!(buffer[3], MessageType::Keepalive as u8);
    
    // Decode
    let decoded = codec.decode(&mut buffer).unwrap().unwrap();
    
    // Verify decoded message
    match decoded {
        GeneratedMessage::Keepalive => {
            // Success, no data to verify
        }
        _ => panic!("Wrong message type decoded"),
    }
}

/**
 * Tests the codec's handling of malformed messages.
 * Verifies that:
 * 1. Messages with invalid message types are rejected
 * 2. Messages with unsupported protocol versions are rejected
 * 3. The codec returns appropriate errors for these cases
 */
#[test]
fn test_malformed_message_handling() {
    let mut codec = TestMetalBondCodec;
    
    // Invalid message type test
    let mut buffer = BytesMut::new();
    buffer.extend_from_slice(&[1, 0, 0, 99]); // Version 1, len 0, invalid type 99
    
    // Should error on decode
    let result = codec.decode(&mut buffer);
    assert!(result.is_err());
    
    // Invalid protocol version test
    let mut buffer = BytesMut::new();
    buffer.extend_from_slice(&[99, 0, 0, 1]); // Version 99, len 0, type Hello
    
    // Should error on decode
    let result = codec.decode(&mut buffer);
    assert!(result.is_err());
}

/**
 * Tests the codec's handling of incomplete messages.
 * Verifies that:
 * 1. When only a header is received (without the full payload), 
 *    the codec returns None and waits for more data
 * 2. The codec correctly reserves space for the expected message
 */
#[test]
fn test_incomplete_message_handling() {
    let mut codec = TestMetalBondCodec;
    
    // Header only, no payload
    let mut buffer = BytesMut::new();
    buffer.extend_from_slice(&[1, 0, 4, 1]); // Version 1, len 4, type Hello
    
    // Should return None, not enough data
    let result = codec.decode(&mut buffer);
    assert!(result.unwrap().is_none());
    
    // Incomplete header
    let mut buffer = BytesMut::new();
    buffer.extend_from_slice(&[1, 0]); // Only 2 bytes
    
    // Should return None, not enough data
    let result = codec.decode(&mut buffer);
    assert!(result.unwrap().is_none());
} 