# Build OntoDB Open Source edition (Windows)
# No enterprise features enabled

Write-Host "=== Building OntoDB Open Source ===" -ForegroundColor Cyan
Write-Host "Features: none (core only)"

cargo build --release --bin ontodb-server

Write-Host ""
Write-Host "=== Build complete ===" -ForegroundColor Green
Write-Host "Binary: target/release/ontodb-server.exe"
Write-Host "Edition: Open Source"
