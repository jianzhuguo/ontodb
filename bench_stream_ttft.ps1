Add-Type -AssemblyName System.Net.Http

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(5)
$uri = "http://127.0.0.1:7912/api/query"

function Measure-TTFT([string]$sql, [string]$label) {
    $body = [System.Net.Http.StringContent]::new(
        ('{"query":"' + $sql.Replace('\','\\').Replace('"','\"') + '"}'),
        [System.Text.Encoding]::UTF8, "application/json")
    $req = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Post, $uri)
    $req.Content = $body
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $resp = $client.SendAsync($req, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).Result
    $sw.Stop()
    $ttft = $sw.Elapsed.TotalMilliseconds
    $stream = $resp.Content.ReadAsStreamAsync().Result
    $buf = New-Object byte[] 65536
    $total = 0
    $first = $true
    $readSw = [System.Diagnostics.Stopwatch]::new()
    while (($n = $stream.Read($buf, 0, $buf.Length)) -gt 0) {
        if ($first) {
            $readSw.Start()
            $first = $false
        }
        $total += $n
    }
    $readSw.Stop()
    Write-Output ("{0,-42} | TTFT={1,7:N0}ms | body_read={2,7:N0}ms | bytes={3}" -f $label, $ttft, $readSw.Elapsed.TotalMilliseconds, $total)
}

Write-Output "=== Streaming TTFT Benchmark ==="
Write-Output ("Started: {0}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"))
Measure-TTFT "SELECT id FROM sembio.BioAssay LIMIT 1" "warmup"
Measure-TTFT "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B'" "SELECT * 492k (streaming TTFT)"
Measure-TTFT "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B'" "SELECT id 492k (streaming TTFT)"
Measure-TTFT "SELECT * FROM sembio.BioAssay WHERE assay_type = 'F'" "SELECT * 72k (streaming TTFT)"
Measure-TTFT "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 1000" "SELECT * LIMIT 1000 (TTFT)"
