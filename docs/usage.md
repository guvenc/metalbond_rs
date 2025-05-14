# MetalBond_rs Usage Guide

This guide explains how to use the MetalBond_rs server and client components.

## Basic Concepts

Before diving into usage, it's helpful to understand some core concepts:

- **VNI (Virtual Network Identifier)**: A numeric identifier used to segregate virtual networks. Only routes with matching VNIs are exchanged between peers.
- **Peer**: A connection to another MetalBond instance. Peers exchange route information.
- **Route**: A network path definition consisting of a destination (prefix) and a next hop.
- **Subscription**: A declaration of interest in routes for a specific VNI.

## Server Mode

The server acts as a central point for route exchange between multiple clients.

### Starting the Server

```bash
metalbond-server --listen "[::]:4711" [options]
```

Or using cargo:

```bash
cargo run --bin metalbond-server -- --listen "[::]:4711" [options]
```

### Server Options

| Option | Description |
|--------|-------------|
| `--listen <ADDRESS>` | The address and port to listen on (required) |
| `--verbose` | Enable verbose logging for debugging |
| `--keepalive <SECONDS>` | Set the keepalive interval (default: 5 seconds) |
| `--http <ADDRESS>` | Enable HTTP dashboard (e.g., ":4712") |

### Example

```bash
# Start a server on all interfaces, port 4711, with HTTP dashboard
metalbond-server --listen "[::]:4711" --http ":4712" --verbose
```

## Client Mode (Linux only)

The client connects to a MetalBond server, subscribes to VNIs, and exchanges route information.

### Starting the Client

```bash
metalbond-client --server <ADDRESS> [options]
```

Or using cargo:

```bash
cargo run --bin metalbond-client -- --server "192.168.1.1:4711" [options]
```

### Client Options

| Option | Description |
|--------|-------------|
| `--server <ADDRESS>` | The address and port of the server to connect to (required) |
| `--subscribe <VNI>` | Subscribe to routes for a specific VNI (can be used multiple times) |
| `--announce <SPEC>` | Announce routes (format: vni#prefix#destHop[#type][#from#to]) |
| `--verbose` | Enable verbose logging for debugging |
| `--install-routes <SPEC>` | Install routes via netlink (format: vni#table) |
| `--tun <DEVICE>` | Tunnel device name (required with --install-routes) |
| `--ipv4-only` | Receive only IPv4 routes |
| `--http <ADDRESS>` | Enable HTTP dashboard |

### Examples

```bash
# Connect to a server and subscribe to VNI 100
metalbond-client --server "192.168.1.1:4711" --subscribe 100

# Connect, subscribe, and announce a route
metalbond-client --server "192.168.1.1:4711" --subscribe 100 --announce "100#192.168.1.0/24#10.0.0.1"

# Install routes from VNI 100 to table 10, using tun0
metalbond-client --server "192.168.1.1:4711" --subscribe 100 --install-routes "100#10" --tun tun0
```

## Route Announcement Format

When announcing routes, use the following format:

```
vni#prefix#destHop[#type][#from#to]
```

Where:
- `vni`: Virtual Network Identifier (numeric)
- `prefix`: Network prefix in CIDR notation (e.g., 192.168.1.0/24)
- `destHop`: Next hop IP address
- `type` (optional): Next hop type (standard, nat)
- `from` and `to` (optional): Port range for NAT (only used with type=nat)

Examples:
```
100#192.168.1.0/24#10.0.0.1
100#10.0.0.0/8#192.168.0.1#nat#1024#2048
```

## HTTP Dashboard

Both server and client can expose an HTTP dashboard for monitoring. Access it by navigating to the configured address in a web browser.

The dashboard provides:
- Peer connection status
- VNI subscriptions
- Route tables
- System statistics

## Programmatic Usage

MetalBond_rs can also be used as a library in your Rust applications:

```rust
use metalbond::{Config, DefaultNetworkClient, MetalBond};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create configuration
    let config = Config::default();
    
    // Create network client
    let client = Arc::new(DefaultNetworkClient::new().await?);
    
    // Create MetalBond instance (server mode)
    let mut metalbond = MetalBond::new(config, client, true);
    
    // Start the component
    metalbond.start();
    
    // Start server
    metalbond.start_server("[::]:4711").await?;
    
    // Keep running until terminated
    tokio::signal::ctrl_c().await?;
    
    // Shut down
    metalbond.shutdown().await;
    
    Ok(())
}
```

For more details on the API, see the [API Reference](api.md). 