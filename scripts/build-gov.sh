#!/bin/bash
# Build OntoDB Enterprise Gov/Finance edition
# Features: all enterprise features including security and compliance

set -e

echo "=== Building OntoDB Enterprise Gov/Finance ==="
echo "Features: all enterprise features"

cargo build --release --bin ontodb-server --features enterprise-gov

echo ""
echo "=== Build complete ==="
echo "Binary: target/release/ontodb-server"
echo "Edition: Enterprise Gov/Finance"
echo "Enabled features: cluster, sharding, security, encryption, backup, incremental-backup, pitr, observability, audit-retention, crc-validation, rolling-upgrade"
