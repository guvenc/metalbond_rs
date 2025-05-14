# MetalBond_rs

A Rust implementation of MetalBond networking system with lock-less design.

## Project Structure

The project is structured into three main components:

1. **Library**: The core MetalBond functionality
2. **Server**: A server binary that runs on any platform (Linux, macOS, Windows)
3. **Client**: A client binary that requires Linux for netlink support

## Building

### Building on Linux

On Linux, you can build and run all components:

```bash
# Build everything
cargo build

# Build server only
cargo build --bin metalbond-server

# Build client only
cargo build --bin metalbond-client

# Run server
cargo run --bin metalbond-server -- --listen "[::]:4711"

# Run client
cargo run --bin metalbond-client -- --server "192.168.1.1:4711" --subscribe 23
```

### Building on macOS/Other Platforms

On non-Linux platforms, only the server and library can be built:

```bash
# Build server only (client won't be built)
cargo build 

# Run server
cargo run -- --listen "[::]:4711"
```

If you want to explicitly build the client, you'll need to enable the netlink feature:

```bash
# This will fail on non-Linux platforms
cargo build --features netlink-support --bin metalbond-client
```

## Conditional Compilation

The project uses conditional compilation to ensure:

1. The server works on all platforms
2. The client is only built on Linux
3. Netlink functionality is isolated from cross-platform code

## Custom Features

- `netlink-support`: Enables netlink functionality (only compiles on Linux)

## Usage

### Server Mode

```
metalbond-server --listen "[::]:4711" [options]
```

Options:
- `--verbose`: Enable debug logging
- `--keepalive <SECONDS>`: Set keepalive interval
- `--http <ADDRESS>`: Enable HTTP dashboard (e.g., ":4712")

### Client Mode (Linux only)

```
metalbond-client --server <ADDRESS> [options]
```

Options:
- `--subscribe <VNI>`: Subscribe to VNI
- `--announce <SPEC>`: Announce routes (format: vni#prefix#destHop[#type][#from#to])
- `--verbose`: Enable debug logging
- `--install-routes <SPEC>`: Install routes via netlink (format: vni#table)
- `--tun <DEVICE>`: Tunnel device name (required with --install-routes)
- `--ipv4-only`: Receive only IPv4 routes
- `--http <ADDRESS>`: Enable HTTP dashboard 