# MetalBond_rs API Reference

This document provides reference information for developers who want to use MetalBond_rs as a library in their Rust applications.

## Core Types

### MetalBond

The main entry point for the MetalBond system.

```rust
pub struct MetalBond {
    // Implementation details omitted
}
```

#### Constructor

```rust
pub fn new(config: Config, client: Arc<dyn NetworkClient>, is_server: bool) -> Self
```

Creates a new MetalBond instance.
- `config`: Configuration for the MetalBond instance
- `client`: Network client implementation
- `is_server`: Whether this instance should act as a server

#### Methods

```rust
pub fn start(&mut self)
```
Starts the MetalBond services.

```rust
pub async fn shutdown(&mut self)
```
Shuts down the MetalBond instance gracefully.

```rust
pub async fn start_server(&mut self, addr: String) -> Result<()>
```
Starts a server on the specified address.
- `addr`: The address and port to listen on (e.g., "[::]:4711")

```rust
pub async fn add_peer(&mut self, addr: &str) -> Result<()>
```
Adds a peer connection to a remote server.
- `addr`: The address and port of the remote server (e.g., "192.168.1.1:4711")

```rust
pub async fn remove_peer(&mut self, addr: &str) -> Result<()>
```
Removes a peer connection.
- `addr`: The address of the peer to remove

```rust
pub async fn get_peer_state(&self, addr: &str) -> Result<ConnectionState>
```
Gets the connection state of a peer.
- `addr`: The address of the peer

```rust
pub async fn subscribe(&mut self, vni: Vni) -> Result<()>
```
Subscribes to routes for a specific VNI.
- `vni`: The Virtual Network Identifier to subscribe to

```rust
pub async fn unsubscribe(&mut self, vni: Vni) -> Result<()>
```
Unsubscribes from routes for a specific VNI.
- `vni`: The VNI to unsubscribe from

```rust
pub async fn announce_route(&mut self, vni: Vni, dest: Destination, next_hop: NextHop) -> Result<()>
```
Announces a route to all peers who have subscribed to the VNI.
- `vni`: The Virtual Network Identifier
- `dest`: The destination network (prefix)
- `next_hop`: The next hop information

```rust
pub async fn withdraw_route(&mut self, vni: Vni, dest: Destination, next_hop: NextHop) -> Result<()>
```
Withdraws a previously announced route.
- `vni`: The Virtual Network Identifier
- `dest`: The destination network (prefix)
- `next_hop`: The next hop information

### Config

Configuration for a MetalBond instance.

```rust
pub struct Config {
    pub keepalive_interval: Duration,
    pub retry_interval_min: Duration,
    pub retry_interval_max: Duration,
}
```

#### Fields
- `keepalive_interval`: How often to send keepalive messages
- `retry_interval_min`: Minimum backoff time for connection retry
- `retry_interval_max`: Maximum backoff time for connection retry

#### Methods

```rust
pub fn default() -> Self
```
Creates a configuration with default values.

### NetworkClient Trait

Interface for platform-specific network operations.

```rust
pub trait NetworkClient: Send + Sync + 'static {
    async fn new() -> Result<Self> where Self: Sized;
    async fn install_route(&self, vni: Vni, dest: Destination, next_hop: NextHop) -> Result<()>;
    async fn remove_route(&self, vni: Vni, dest: Destination, next_hop: NextHop) -> Result<()>;
    // Other methods omitted
}
```

#### Implementations
- `DummyClient`: A no-op implementation for platforms without netlink support
- `NetlinkClient`: A Linux implementation that uses netlink to install routes

## Type Definitions

### Vni

```rust
pub type Vni = u32;
```
A Virtual Network Identifier.

### Destination

```rust
pub struct Destination {
    pub prefix: IpNet,
}
```

Represents a network destination with a prefix.

### NextHop

```rust
pub struct NextHop {
    pub target_address: IpAddr,
    pub target_vni: Vni,
    pub hop_type: pb::NextHopType,
    pub nat_port_range_from: u16,
    pub nat_port_range_to: u16,
}
```

Represents a next hop for routing purposes.

### ConnectionState

```rust
pub enum ConnectionState {
    Connecting,
    HelloSent,
    HelloReceived,
    Established,
    Retry,
    Closed,
}
```

Represents the state of a peer connection within the MetalBond state machine.

#### States
- `Connecting`: Initial state when a TCP connection is being established
- `HelloSent`: The local peer has sent a Hello message but hasn't received one
- `HelloReceived`: The local peer has received a Hello message but hasn't sent one yet
- `Established`: Both peers have exchanged Hello messages and the connection is fully established
- `Retry`: A previously established connection has been lost and will be retried
- `Closed`: The connection has been terminated and resources are being cleaned up

The state machine follows a specific transition flow as documented in the Architecture document. The current state can be queried using `get_peer_state()` method, which is useful for monitoring the health and status of connections.

## Example Usage

```rust
use metalbond::{Config, DefaultNetworkClient, MetalBond};
use metalbond::types::{Destination, NextHop, Vni};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use ipnet::IpNet;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create custom configuration
    let config = Config {
        keepalive_interval: Duration::from_secs(10),
        retry_interval_min: Duration::from_secs(1),
        retry_interval_max: Duration::from_secs(30),
    };
    
    // Create network client
    let client = Arc::new(DefaultNetworkClient::new().await?);
    
    // Create MetalBond instance in client mode
    let mut metalbond = MetalBond::new(config, client, false);
    
    // Start the component
    metalbond.start();
    
    // Connect to a server
    metalbond.add_peer("192.168.1.1:4711").await?;
    
    // Subscribe to a VNI
    let vni: Vni = 100;
    metalbond.subscribe(vni).await?;
    
    // Announce a route
    let dest = Destination { 
        prefix: "192.168.2.0/24".parse::<IpNet>()? 
    };
    let next_hop = NextHop {
        target_address: "10.0.0.1".parse::<IpAddr>()?,
        target_vni: 200,
        hop_type: metalbond::pb::NextHopType::Standard,
        nat_port_range_from: 0,
        nat_port_range_to: 0,
    };
    
    metalbond.announce_route(vni, dest, next_hop).await?;
    
    // Keep running for a while
    tokio::time::sleep(Duration::from_secs(60)).await;
    
    // Withdraw the route
    metalbond.withdraw_route(vni, dest, next_hop).await?;
    
    // Unsubscribe
    metalbond.unsubscribe(vni).await?;
    
    // Shut down
    metalbond.shutdown().await;
    
    Ok(())
}
```

## Error Handling

The MetalBond API uses `anyhow::Result` for most operations, which allows for flexible error handling and propagation. Specific error types include:
- Connection errors
- Protocol errors
- Configuration errors
- Route validation errors

It's recommended to handle errors appropriately based on your application's needs. 