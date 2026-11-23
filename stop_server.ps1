# Graceful stop for ontodb-server: hit POST /api/shutdown so the server can
# drain in-flight work and persist row counts (row_counts.json) before exit.
# Replaces taskkill/Stop-Process, which skip all cleanup.
$uri = "http://127.0.0.1:7912/api/shutdown"

if (-not (Get-Process ontodb-server -ErrorAction SilentlyContinue)) {
    Write-Output "ontodb-server is not running."
    exit 0
}

try {
    Invoke-RestMethod -Method Post -Uri $uri -TimeoutSec 10 | Out-Null
    Write-Output "Shutdown requested."
} catch {
    Write-Warning "shutdown endpoint not reachable: $_"
    exit 1
}

$deadline = (Get-Date).AddSeconds(60)
while ((Get-Date) -lt $deadline) {
    if (-not (Get-Process ontodb-server -ErrorAction SilentlyContinue)) {
        Write-Output "ontodb-server stopped cleanly."
        exit 0
    }
    Start-Sleep -Milliseconds 500
}

Write-Warning "ontodb-server did not exit within 60s."
exit 1
