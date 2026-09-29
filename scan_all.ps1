# Try to scan all keys with empty prefix to see what's in storage
$body = @{ query = "SELECT key FROM __all__ LIMIT 50" } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
if ($json.success -and $json.data.Count -gt 0) {
    Write-Output "Found $($json.data.Count) keys:"
    foreach ($row in $json.data) {
        Write-Output "  $($row.key)"
    }
} else {
    Write-Output "No data or error: $($resp.Content)"
}

# Also try scanning with different prefixes
$prefixes = @("__ontology__", "__val_meta__", "__val_event__", "entity", "relation", "graph", "vector", "ts", "spatial")
foreach ($p in $prefixes) {
    $body = @{ query = "SELECT COUNT(*) as cnt FROM '__scan__' WHERE prefix = '$p'" } | ConvertTo-Json
    try {
        $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
        $json = $resp.Content | ConvertFrom-Json
        Write-Output "Prefix '$p': $($json.data[0].cnt)"
    } catch {
        Write-Output "Prefix '$p': ERROR"
    }
}
