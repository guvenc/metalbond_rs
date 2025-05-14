#!/bin/bash
set -e

# Default container name (can be overridden with METALBOND_CONTAINER_NAME env var)
CONTAINER_NAME=${METALBOND_CONTAINER_NAME:-metalbond-builder}

# Name for the cargo cache volume
CACHE_VOLUME=${METALBOND_CACHE_VOLUME:-metalbond-cargo-cache}

# Check if Docker or Podman is available
if command -v podman &> /dev/null; then
    CONTAINER_CMD="podman"
elif command -v docker &> /dev/null; then
    CONTAINER_CMD="docker"
else
    echo "Error: Neither Docker nor Podman is installed. Please install one of them."
    exit 1
fi

# Check if the container exists, build it if it doesn't
if ! $CONTAINER_CMD image exists "$CONTAINER_NAME" 2>/dev/null; then
    echo "Container image $CONTAINER_NAME not found, building it now..."
    $CONTAINER_CMD build -t "$CONTAINER_NAME" -f hack/buildcontainer/Dockerfile .
fi

# Create the cache volume if it doesn't exist yet
if ! $CONTAINER_CMD volume inspect "$CACHE_VOLUME" &>/dev/null; then
    echo "Creating cargo cache volume: $CACHE_VOLUME"
    $CONTAINER_CMD volume create "$CACHE_VOLUME"
fi

# Run cargo inside the container with all arguments passed to this script
# Mount both the current directory and the cargo cache volume
$CONTAINER_CMD run --rm \
    -v "$(pwd)":/app \
    -v "$CACHE_VOLUME:/usr/local/cargo/registry" \
    -w /app \
    "$CONTAINER_NAME" cargo "$@" 