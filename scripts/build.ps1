# OntoDB Build Script (Windows)
# Supports building community (AGPL) and enterprise editions

param(
    [ValidateSet("community", "enterprise-standard", "enterprise-gov")]
    [string]$Edition = "community"
)

$ErrorActionPreference = "Stop"

$Version = (Select-String -Path "Cargo.toml" -Pattern '^version' | Select-Object -First 1).Line -replace '.*"(.+)".*','$1'

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  OntoDB Build Script" -ForegroundColor Cyan
Write-Host "  Edition: $Edition" -ForegroundColor Cyan
Write-Host "  Version: $Version" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

switch ($Edition) {
    "community" {
        Write-Host "Building Community Edition (AGPL-3.0)..." -ForegroundColor Green
        cargo build --release
        Write-Host ""
        Write-Host "Build complete: target/release/ontodb-server.exe" -ForegroundColor Green
        Write-Host "License: AGPL-3.0" -ForegroundColor Yellow
    }
    "enterprise-standard" {
        Write-Host "Building Enterprise Standard Edition..." -ForegroundColor Green
        cargo build --release --features enterprise-standard
        Write-Host ""
        Write-Host "Build complete: target/release/ontodb-server.exe" -ForegroundColor Green
        Write-Host "License: Commercial" -ForegroundColor Yellow
    }
    "enterprise-gov" {
        Write-Host "Building Enterprise Gov/Finance Edition..." -ForegroundColor Green
        cargo build --release --features enterprise-gov
        Write-Host ""
        Write-Host "Build complete: target/release/ontodb-server.exe" -ForegroundColor Green
        Write-Host "License: Commercial (Gov/Finance)" -ForegroundColor Yellow
    }
}

Write-Host ""
Write-Host "Build successful!" -ForegroundColor Green
