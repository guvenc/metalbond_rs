# MetalBond_rs Installation Guide

This guide covers how to install and build MetalBond_rs on different platforms.

## Prerequisites

Before installing MetalBond_rs, you need the following:

- Rust toolchain (1.60.0+)
- Cargo package manager
- Git
- On Linux: Development libraries for netlink (for client support)

### Installing Rust

If you don't have Rust installed, the recommended way is to use [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the on-screen instructions to complete the installation.

## Building from Source

### Clone the Repository

```bash
git clone https://github.com/your-organization/metalbond_rs.git
cd metalbond_rs
```

### Building on Linux

On Linux, you can build and run all components:

```bash
# Build everything with default features
cargo build

# Build in release mode
cargo build --release

# Build server only
cargo build --bin metalbond-server

# Build client only (Linux only)
cargo build --bin metalbond-client
```

#### Required Libraries for Linux Client

For the client functionality, you'll need the following libraries:

```bash
# Ubuntu/Debian
sudo apt-get install libnl-3-dev libnl-route-3-dev

# Fedora/RHEL
sudo dnf install libnl3-devel
```

### Building on macOS/Other Platforms

On non-Linux platforms, only the server and library can be built:

```bash
# Build server only (client won't be built)
cargo build 

# Build in release mode
cargo build --release
```

## Testing

To ensure everything is working correctly, run the test suite:

```bash
cargo test
```

Or, if you want to run the integration tests that are typically ignored:

```bash
cargo test -- --ignored
```

## Installation Options

### Installing as a Binary

If you want to install the binaries to your system:

```bash
cargo install --path .
```

This will install the binaries to your Cargo bin directory (usually `~/.cargo/bin/`).

### Running without Installation

You can also run the binaries directly without installing:

```bash
# Run server
cargo run --bin metalbond-server -- --listen "[::]:4711"

# Run client (Linux only)
cargo run --bin metalbond-client -- --server "192.168.1.1:4711" --subscribe 23
```

## Troubleshooting

If you encounter any issues during installation:

1. Make sure your Rust toolchain is up to date: `rustup update`
2. Check that you have the required dependencies installed
3. On Linux, verify netlink libraries are properly installed
4. Check the project issues page for known problems

If problems persist, please [create an issue](https://github.com/your-organization/metalbond_rs/issues) with details about your environment and the error you're encountering. 