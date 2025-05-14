FROM rust:1.86-bookworm as builder

WORKDIR /app

# Install protobuf compiler
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Install Rust components
RUN rustup component add rustfmt clippy

WORKDIR /app
