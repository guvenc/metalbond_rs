# MetalBond_rs Architecture

This document describes the architecture of the MetalBond_rs system, explaining its components, how they interact, and the design decisions that were made.

## System Overview

MetalBond_rs is a distributed route exchange system that enables seamless integration between different virtual networks. It consists of several key components working together to provide efficient route management and distribution.

## Component Architecture

### Core Components

1. **MetalBond Core**
   - Central orchestrator that manages all components
   - Handles lifecycle of peers, routes, and subscriptions
   - Provides API for higher-level applications
   - Implemented as a frontend that sends commands to a backend actor

2. **RouteTable**
   - Lock-less table for storing and querying routes
   - Efficiently maps VNIs to destinations to next hops
   - Tracks route sources for proper clean-up on peer disconnect
   - Implemented as an actor that processes commands sequentially

3. **Peer Management**
   - Manages connections to remote MetalBond instances
   - Implements the MetalBond wire protocol
   - Handles connection state, keepalives, and reconnection
   - Each peer operates as an independent actor

4. **Network Client**
   - Platform-specific abstraction for network operations
   - On Linux: Uses netlink for route installation
   - On other platforms: Provides a dummy implementation

### Protocol Design

The MetalBond protocol uses a simple binary format for efficiency:

```
+--------+----------+-----------+---------------+
| Version| Length   | Msg Type  | Payload       |
| (1b)   | (2b)     | (1b)      | (variable)    |
+--------+----------+-----------+---------------+
```

Message types include:
- Hello (establishes connection parameters)
- Subscribe/Unsubscribe (declares interest in VNIs)
- Update (announces or withdraws routes)
- Keepalive (maintains connection health)

All messages are encoded using Protocol Buffers for cross-platform compatibility.

## Lock-less Design

One of the key design features of MetalBond_rs is its lock-less approach to route table management and overall system architecture. This design is guided by the following principles:

### Design Principles

1. **No shared mutable state**: Replace mutexes with message passing
2. **Actor model**: Core components operate as independent actors
3. **Single ownership**: Each piece of data has exactly one owner
4. **Immutability by default**: Use immutable data where possible
5. **Message-based communication**: Components communicate via message channels

### Implementation Details

#### MetalBond Structure

The MetalBond struct is designed as a frontend that sends commands to a backend actor:

```rust
pub struct MetalBond {
    // Command sender to the MetalBond actor
    command_tx: mpsc::Sender<MetalBondCommand>,
    // For public access, only contains getters
    pub route_table_view: RouteTable,
    // Task handles
    actor_task: JoinHandle<()>,
    distributor_task: Option<JoinHandle<()>>,
    connection_manager_task: Option<JoinHandle<()>>,
}
```

#### RouteTable as an Actor

The RouteTable is implemented as an actor that processes commands:

```rust
#[derive(Debug, Clone)]
pub struct RouteTable {
    // Command sender to the actor
    cmd_tx: Option<mpsc::Sender<RouteTableCommand>>,
    // For subscription to route changes
    _subscribers: Vec<mpsc::Sender<RouteTableMsg>>,
}
```

#### Peer as an Actor

Each peer connection is managed by its own actor:

```rust
pub struct Peer {
    remote_addr: SocketAddr,
    message_tx: mpsc::Sender<PeerMessage>,
}
```

### Communication Patterns

The system uses two primary communication patterns:

1. **Command-Response Pattern**

For operations that need a response, a oneshot channel is used:

```rust
pub async fn subscribe(&self, vni: Vni) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    self.command_tx.send(MetalBondCommand::Subscribe(vni, tx)).await?;
    rx.await?
}
```

2. **Fire-and-Forget Pattern**

For notifications that don't need a response:

```rust
pub fn set_state(&self, new_state: ConnectionState) {
    // Fire and forget, not waiting for result
    let _ = self.message_tx.try_send(PeerMessage::SetState(new_state));
}
```

### Benefits of Lock-less Design

This lock-less approach provides several advantages:

1. **No deadlocks**: Since there are no mutexes, deadlocks are impossible
2. **Improved performance**: Less contention and waiting for locks
3. **Simplified reasoning**: Each component has a single task processing messages sequentially
4. **Better testability**: Components can be tested in isolation
5. **More idiomatic Rust**: Leverages Rust's ownership model
6. **Clearer data flow**: Message passing makes the flow of data explicit

## Concurrency Model

MetalBond_rs uses the actor-based concurrency model:

1. **Actor-based processing**: Each core component operates as an actor that processes messages sequentially
2. **Message passing**: Communication between components occurs via message channels
3. **State encapsulation**: Each actor encapsulates its own state, eliminating shared mutable state
4. **Tokio for async I/O**: Uses Tokio for asynchronous I/O and task management

This design allows MetalBond_rs to efficiently utilize CPU resources while handling many concurrent connections and route updates.

## Design Decisions

### Split Client/Server Implementation

The client and server implementations are split to allow:
- Server operation on any platform
- Client netlink functionality on Linux only
- Reuse of common components

### Protocol Buffer Usage

Protocol Buffers were chosen for message serialization because they:
- Provide efficient binary encoding
- Support schema evolution
- Work across multiple languages if needed

### Actor Model

The actor model was chosen because:
- It eliminates the need for locks
- It simplifies reasoning about concurrent code
- It provides clear boundaries between components
- It makes the system more resilient to failures

## Performance Considerations

MetalBond_rs is designed for high-performance route exchange with these optimizations:

1. **Lock-less design**: Eliminates contention and waiting for locks
2. **Minimal copying of route data**: Efficient data handling
3. **Efficient memory usage**: Appropriate data structures
4. **Batching of route updates**: Where possible
5. **Asynchronous processing**: Maximize throughput

## Extensibility

The system is designed to be extensible in several ways:

1. **New message types**: Can be added to the protocol
2. **Additional route types and attributes**: Can be supported
3. **Custom network clients**: Can be implemented
4. **API usable as a library**: For integration into other applications

## Future Directions

Planned future enhancements include:

1. **Enhanced security features**: For secure route exchange
2. **Support for route filtering and policies**: For more control
3. **More comprehensive monitoring and metrics**: For better observability
4. **Additional platform-specific optimizations**: For greater performance 