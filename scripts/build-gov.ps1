# Build OntoDB Enterprise Gov/Finance edition (Windows)
# Features: all enterprise features including security and compliance

Write-Host "=== Building OntoDB Enterprise Gov/Finance ===" -ForegroundColor Cyan
Write-Host "Features: all enterprise features"

cargo build --release --bin ontodb-server --features enterprise-gov

Write-Host ""
Write-Host "=== Build complete ===" -ForegroundColor Green
Write-Host "Binary: target/release/ontodb-server.exe"
Write-Host "Edition: Enterprise Gov/Finance"
Write-Host "Enabled features: cluster, sharding, security, encryption, backup, incremental-backup, pitr, observability, audit-retention, crc-validation, rolling-upgrade"
