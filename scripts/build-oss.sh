#!/bin/bash
# Build OntoDB Open Source edition
# No enterprise features enabled

set -e

echo "=== Building OntoDB Open Source ==="
echo "Features: none (core only)"

cargo build --release --bin ontodb-server

echo ""
echo "=== Build complete ==="
echo "Binary: target/release/ontodb-server"
echo "Edition: Open Source"
