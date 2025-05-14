# Lock-less Design for MetalBond

## Overview

This document describes the redesigned architecture for MetalBond, moving away from heavy mutex usage to a more Rust-idiomatic, lock-less approach using the actor model with message passing.

## Design Principles

1. **No shared mutable state**: Replace mutexes with message passing
2. **Actor model**: Core components operate as independent actors
3. **Single ownership**: Each piece of data has exactly one owner
4. **Immutability by default**: Use immutable data where possible
5. **Message-based communication**: Components communicate via message channels

## Core Components

### MetalBond

The MetalBond struct has been redesigned as a frontend that sends commands to a backend actor. The actor maintains all state and processes commands sequentially, eliminating the need for locks.

```rust
// Main MetalBond application struct
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

### RouteTable

The RouteTable is reimplemented as an actor that processes commands:

```rust
// The new RouteTable structure
#[derive(Debug, Clone)]
pub struct RouteTable {
    // Command sender to the actor
    cmd_tx: Option<mpsc::Sender<RouteTableCommand>>,
    // For subscription to route changes
    _subscribers: Vec<mpsc::Sender<RouteTableMsg>>,
}
```

### Peer

Each peer connection is managed by its own actor:

```rust
// Public interface for the Peer
pub struct Peer {
    remote_addr: SocketAddr,
    message_tx: mpsc::Sender<PeerMessage>,
}
```

## Communication Patterns

### Command-Response Pattern

For operations that need a response, we use a oneshot channel:

```rust
pub async fn subscribe(&self, vni: Vni) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    self.command_tx.send(MetalBondCommand::Subscribe(vni, tx)).await?;
    rx.await?
}
```

### Fire-and-Forget Pattern

For notifications that don't need a response:

```rust
pub fn set_state(&self, new_state: ConnectionState) {
    // Fire and forget, not waiting for result
    let _ = self.message_tx.try_send(PeerMessage::SetState(new_state));
}
```

## Benefits

1. **No deadlocks**: Since there are no mutexes, deadlocks are impossible
2. **Improved performance**: Less contention and waiting for locks
3. **Simplified reasoning**: Each component has a single task processing messages sequentially
4. **Better testability**: Components can be tested in isolation
5. **More idiomatic Rust**: Leverages Rust's ownership model
6. **Clearer data flow**: Message passing makes the flow of data explicit

## Implementation Notes

### State Transitions

State transitions are handled as messages to the actor, which can update its internal state without needing locks:

```rust
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
        // ...
    }
}
```

### Actor Lifecycle

Actors manage their own lifecycle:

1. **Initialization**: State is created and owned by the actor
2. **Message Processing**: Loop processes messages one at a time
3. **Cleanup**: When the actor receives a Shutdown message or the channel closes

## Comparison with Original Design

### Original Design Issues

1. **Mutex contention**: Multiple readers/writers waiting for access
2. **Complex locking patterns**: Need to manage lock acquisition order
3. **Deadlock potential**: If locks are acquired in the wrong order
4. **Difficult reasoning**: Hard to track who owns what data when
5. **Performance overhead**: Lock acquisition/release costs

### New Design Advantages

1. **Sequential processing**: Each actor processes messages in sequence
2. **No lock contention**: No waiting for locks
3. **Clear ownership**: Each piece of state has exactly one owner
4. **Simpler mental model**: Easier to reason about data flow
5. **Better scalability**: Actors can run concurrently 