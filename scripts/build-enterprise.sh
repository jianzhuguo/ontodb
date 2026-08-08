#!/bin/bash
# Build OntoDB Enterprise Standard edition
# Features: cluster, sharding, basic backup

set -e

echo "=== Building OntoDB Enterprise Standard ==="
echo "Features: cluster, sharding, backup"

cargo build --release --bin ontodb-server --features enterprise-standard

echo ""
echo "=== Build complete ==="
echo "Binary: target/release/ontodb-server"
echo "Edition: Enterprise Standard"
echo "Enabled features: cluster, sharding, backup"
