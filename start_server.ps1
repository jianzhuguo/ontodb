$logDir = "E:\ontodb"
$exe = "E:\ontodb\target\release\ontodb-server.exe"

Start-Process -FilePath $exe `
  -ArgumentList `
    "--data-dir", "E:\sembio\data\ontodb_clean", `
    "--http", "127.0.0.1:7912", `
    "--memtable-size", "134217728", `
    "--compression-level", "2", `
    "--graph-max-cache-mb", "512", `
    "--index-storage-mode", "hybrid", `
    "--index-hybrid-threshold", "100000", `
    "--no-rate-limit" `
  -WindowStyle Hidden `
  -RedirectStandardOutput "$logDir\server_out.log" `
  -RedirectStandardError "$logDir\server_err.log" `
  -WorkingDirectory $logDir