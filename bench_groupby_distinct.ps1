Add-Type -AssemblyName System.Net.Http

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(5)
$uri = "http://127.0.0.1:7912/api/query"

function Q([string]$sql, [string]$label) {
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
    $ms = New-Object System.IO.MemoryStream
    $stream.CopyTo($ms)
    $text = [System.Text.Encoding]::UTF8.GetString($ms.ToArray())
    $rows = "?"; $ok = "?"; $sample = ""
    try {
        $j = $text | ConvertFrom-Json
        $ok = $j.success
        if ($j.data) { $rows = @($j.data).Count }
        if ($j.data -and @($j.data).Count -gt 0) {
            $sample = (@($j.data) | Select-Object -First 3 | ConvertTo-Json -Compress)
            if ($sample.Length -gt 100) { $sample = $sample.Substring(0,100) }
        }
    } catch {
        $sample = "PARSE_ERR: " + $text.Substring(0, [Math]::Min(120, $text.Length))
    }
    Write-Output ("{0,-46} | TTFT={1,7:N0}ms | rows={2,-8} | ok={3} | {4}" -f $label, $ttft, $rows, $ok, $sample)
}

Write-Output "=== Index-driven GROUP BY / DISTINCT Benchmark ==="
Write-Output ("Started: {0}" -f (Get-Date -Format "HH:mm:ss"))
Write-Output ""
Q "SELECT id FROM sembio.BioAssay LIMIT 1" "warmup"
Write-Output ""

Write-Output "--- GROUP BY (index on assay_type) ---"
Q "SELECT assay_type, COUNT(*) AS c FROM sembio.BioAssay GROUP BY assay_type" "GROUP BY assay_type, COUNT(*) (607k)"
Q "SELECT assay_type, COUNT(*) AS c FROM sembio.BioAssay WHERE assay_type = 'B' GROUP BY assay_type" "GROUP BY + WHERE eq (single group)"
Q "SELECT drug_name, COUNT(*) AS c FROM sembio.Drug GROUP BY drug_name" "GROUP BY drug_name (23k, high card)"

Write-Output ""
Write-Output "--- DISTINCT (index on assay_type) ---"
Q "SELECT DISTINCT assay_type FROM sembio.BioAssay" "DISTINCT assay_type (607k)"
Q "SELECT DISTINCT drug_name FROM sembio.Drug" "DISTINCT drug_name (23k)"

Write-Output ""
Write-Output "--- Fallback (no index / complex agg) ---"
Q "SELECT assay_type, SUM(id) AS s FROM sembio.BioAssay GROUP BY assay_type" "GROUP BY + SUM (row scan)"
Q "SELECT COUNT(*) AS c FROM sembio.BioAssay WHERE assay_type = 'B'" "COUNT(*) WHERE 'B' (index)"
Q "SELECT DISTINCT assay_type, id FROM sembio.BioAssay" "DISTINCT 2 cols (fallback)"

Write-Output ""
Write-Output ("Finished: {0}" -f (Get-Date -Format "HH:mm:ss"))
