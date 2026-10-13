Add-Type -AssemblyName System.Net.Http

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(5)
$uri = "http://127.0.0.1:7912/api/query"

function InvokeQ([string]$sql) {
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
    $j = $text | ConvertFrom-Json
    return @{ ttft=$ttft; total=$sw.Elapsed.TotalMilliseconds; json=$j; text=$text }
}

function CheckSort($rows, $col, $desc) {
    if (-not $rows -or @($rows).Count -lt 2) { return "n/a" }
    $vals = @($rows | ForEach-Object { $_.$col })
    $sorted = @($vals)
    if ($desc) { $sorted = @($vals | Sort-Object -Descending) } else { $sorted = @($vals | Sort-Object) }
    for ($i=0; $i -lt $vals.Count; $i++) {
        if ("$($vals[$i])" -ne "$($sorted[$i])") { return "UNSORTED at $i" }
    }
    return "sorted"
}

Write-Output "=== Final Validation (top-N correctness + TTFT) ==="
Write-Output ("Started: {0}" -f (Get-Date -Format "HH:mm:ss"))
$r = InvokeQ "SELECT id FROM sembio.BioAssay LIMIT 1"

# ORDER BY + LIMIT top-N (non-indexed col)
$r = InvokeQ "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B' ORDER BY id LIMIT 5"
Write-Output ("ORDER BY id LIMIT 5          | TTFT={0,6:N0}ms | rows={1} | {2} | first={3} last={4}" -f $r.ttft, @($r.json.data).Count, (CheckSort $r.json.data "id" $false), @($r.json.data)[0].id, @($r.json.data)[-1].id)

$r = InvokeQ "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B' ORDER BY id DESC LIMIT 5"
Write-Output ("ORDER BY id DESC LIMIT 5     | TTFT={0,6:N0}ms | rows={1} | {2} | first={3} last={4}" -f $r.ttft, @($r.json.data).Count, (CheckSort $r.json.data "id" $true), @($r.json.data)[0].id, @($r.json.data)[-1].id)

# ORDER BY + LIMIT top-N with larger N (need real top-N over 492k)
$r = InvokeQ "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B' ORDER BY id LIMIT 100"
$ids = @($r.json.data | ForEach-Object { $_.id })
Write-Output ("ORDER BY id LIMIT 100        | TTFT={0,6:N0}ms | rows={1} | {2}" -f $r.ttft, $ids.Count, (CheckSort $r.json.data "id" $false))

# ORDER BY indexed col
$r = InvokeQ "SELECT assay_type, id FROM sembio.BioAssay WHERE assay_type = 'B' ORDER BY assay_type LIMIT 5"
Write-Output ("ORDER BY assay_type LIMIT 5  | TTFT={0,6:N0}ms | rows={1} | {2}" -f $r.ttft, @($r.json.data).Count, (CheckSort $r.json.data "assay_type" $false))

# Index-ordered large (no limit) - TTFT should be small
$r = InvokeQ "SELECT assay_type, id FROM sembio.BioAssay WHERE assay_type = 'F' ORDER BY assay_type"
Write-Output ("ORDER BY assay_type 72k      | TTFT={0,6:N0}ms | total={1,6:N0}ms | rows={2} | {3}" -f $r.ttft, $r.total, @($r.json.data).Count, (CheckSort $r.json.data "assay_type" $false))

# Aggregates
$r = InvokeQ "SELECT COUNT(*) AS c FROM sembio.BioAssay WHERE assay_type = 'B'"
Write-Output ("COUNT(*) WHERE 'B'           | TTFT={0,6:N0}ms | c={1}" -f $r.ttft, $r.json.data[0].c)

$r = InvokeQ "SELECT assay_type, COUNT(*) AS c FROM sembio.BioAssay GROUP BY assay_type"
Write-Output ("GROUP BY assay_type          | TTFT={0,6:N0}ms | groups={1}" -f $r.ttft, @($r.json.data).Count)

# DISTINCT
$r = InvokeQ "SELECT DISTINCT assay_type FROM sembio.BioAssay"
Write-Output ("DISTINCT assay_type          | TTFT={0,6:N0}ms | rows={1}" -f $r.ttft, @($r.json.data).Count)

# Regression plain streaming
$r = InvokeQ "SELECT * FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 100"
Write-Output ("SELECT * LIMIT 100           | TTFT={0,6:N0}ms | rows={1}" -f $r.ttft, @($r.json.data).Count)

$r = InvokeQ "SELECT id FROM sembio.BioAssay WHERE assay_type = 'B' LIMIT 1000"
Write-Output ("SELECT id LIMIT 1000         | TTFT={0,6:N0}ms | rows={1}" -f $r.ttft, @($r.json.data).Count)

Write-Output ""
Write-Output ("Finished: {0}" -f (Get-Date -Format "HH:mm:ss"))
