# Build OntoDB Enterprise Standard edition (Windows)
# Features: cluster, sharding, basic backup

Write-Host "=== Building OntoDB Enterprise Standard ===" -ForegroundColor Cyan
Write-Host "Features: cluster, sharding, backup"

cargo build --release --bin ontodb-server --features enterprise-standard

Write-Host ""
Write-Host "=== Build complete ===" -ForegroundColor Green
Write-Host "Binary: target/release/ontodb-server.exe"
Write-Host "Edition: Enterprise Standard"
Write-Host "Enabled features: cluster, sharding, backup"
