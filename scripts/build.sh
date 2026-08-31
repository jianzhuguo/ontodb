#!/bin/bash
# OntoDB Build Script
# Supports building community (AGPL) and enterprise editions

set -e

EDITION=${1:-community}
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)

echo "========================================"
echo "  OntoDB Build Script"
echo "  Edition: $EDITION"
echo "  Version: $VERSION"
echo "========================================"

case "$EDITION" in
    community)
        echo "Building Community Edition (AGPL-3.0)..."
        cargo build --release
        echo ""
        echo "Build complete: target/release/ontodb-server"
        echo "License: AGPL-3.0"
        ;;
    enterprise-standard)
        echo "Building Enterprise Standard Edition..."
        cargo build --release --features enterprise-standard
        echo ""
        echo "Build complete: target/release/ontodb-server"
        echo "License: Commercial"
        ;;
    enterprise-gov)
        echo "Building Enterprise Gov/Finance Edition..."
        cargo build --release --features enterprise-gov
        echo ""
        echo "Build complete: target/release/ontodb-server"
        echo "License: Commercial (Gov/Finance)"
        ;;
    *)
        echo "Usage: $0 {community|enterprise-standard|enterprise-gov}"
        exit 1
        ;;
esac

echo ""
echo "Build successful!"
