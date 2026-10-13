# Batch 回表 (multi_get) benchmark — uses .NET HttpClient for large responses
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Net.Http

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(5)
$uri = "http://127.0.0.1:7912/api/query"

function Invoke-Query([string]$sql, [string]$label) {
    $body = [System.Net.Http.StringContent]::new(
        ('{"query":"' + $sql.Replace('\','\\').Replace('"','\"') + '"}'),
        [System.Text.Encoding]::UTF8, "application/json")
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $resp = $client.PostAsync($uri, $body).Result
    $respBody = $resp.Content.ReadAsStringAsync().Result
    $sw.Stop()
    $clientMs = $sw.Elapsed.TotalMilliseconds

    # Parse timing from response
    $serverMs = "?"
    $rows = "?"
    $ok = "?"
    try {
        $j = $respBody | ConvertFrom-Json
        $ok = $j.success
        if ($j.data) { $rows = @($j.data).Count }
        if ($j.elapsed_ms) { $serverMs = [math]::Round($j.elapsed_ms, 1) }
    } catch {
        Write-Output "  PARSE ERROR: $($respBody.Substring(0, [Math]::Min(200, $respBody.Length)))"
    }

    # Fallback: count from server log
    Write-Output ("{0,-50} | client={1,8:N0}ms | server={2,8}ms | rows={3,-8} | ok={4}" -f $label, $clientMs, $serverMs, $rows, $ok)
}

Write-Output "=== Batch 回表 (multi_get) Benchmark ==="
Write-Output ("Started: {0}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"))
Write-Output ""

# Warm up cache with a small query first
Invoke-Query "SELECT id FROM sembio.BioAssay LIMIT 1" "warmup (LIMIT 1)"
Write-Output ""

# --- Core 回表 tests ---
# 1. Full row fetch via index + 回表 (the multi_get path)
Invoke-Query "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B'" "SELECT * WHERE assay_type='B' (492k rows)"

# 2. Column pruned (less data parsed after 回表)
Invoke-Query "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B'" "SELECT id WHERE assay_type='B' (492k rows)"

# 3. LIMIT variants (回表 for N rows only)
Invoke-Query "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 100" "SELECT * ... LIMIT 100"
Invoke-Query "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 1000" "SELECT * ... LIMIT 1000"
Invoke-Query "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 1000" "SELECT id ... LIMIT 1000"
Invoke-Query "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 10000" "SELECT * ... LIMIT 10000"

# 4. Smaller index selectivity
Invoke-Query "SELECT * FROM sembio.BioAssay WHERE assay_type = 'F'" "SELECT * WHERE assay_type='F' (72k rows)"

# 5. COUNT (no 回表, baseline)
Invoke-Query "SELECT COUNT(*) FROM sembio.BioAssay WHERE assay_type = 'B'" "COUNT(*) WHERE assay_type='B'"

Write-Output ""
Write-Output ("Finished: {0}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"))
