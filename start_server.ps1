$logDir = "E:\ontodb"
$exe = "E:\ontodb\target\release\ontodb-server.exe"

# Rotate logs before start so a crash window is never overwritten.
# Keeps the last 10 rotations as server_out.YYYYMMDD_HHMMSS.log (same for err).
function Rotate-Log([string]$path) {
    if (-not (Test-Path $path)) { return }
    if ((Get-Item $path).Length -eq 0) { return }
    $stamp = Get-Date -Format "yyyyMMdd_HHmmss"
    $dest = $path -replace '\.log$', ".$stamp.log"
    Move-Item -Force $path $dest
    # prune: keep newest 10 per prefix
    $prefix = [System.IO.Path]::GetFileNameWithoutExtension($path)
    Get-ChildItem $logDir -Filter "$prefix.*.log" |
        Sort-Object LastWriteTime -Descending |
        Select-Object -Skip 10 |
        Remove-Item -Force
}

Rotate-Log "$logDir\server_out.log"
Rotate-Log "$logDir\server_err.log"

Start-Process -FilePath $exe `
  -ArgumentList `
    "--data-dir", "E:\sembio\data\ontodb_clean", `
    "--http", "127.0.0.1:7912", `
    "--memtable-size", "134217728", `
    "--compression-level", "1", `
    "--graph-max-cache-mb", "512", `
    "--index-storage-mode", "hybrid", `
    "--index-hybrid-threshold", "100000", `
    "--no-rate-limit" `
  -WindowStyle Hidden `
  -RedirectStandardOutput "$logDir\server_out.log" `
  -RedirectStandardError "$logDir\server_err.log" `
  -WorkingDirectory $logDir
