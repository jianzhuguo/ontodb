# Try to find all tables by querying with various patterns
$queries = @(
    "SELECT COUNT(*) as cnt FROM 'Drug'",
    "SELECT COUNT(*) as cnt FROM 'Gene'",
    "SELECT COUNT(*) as cnt FROM 'Protein'",
    "SELECT COUNT(*) as cnt FROM 'graph_nodes'",
    "SELECT COUNT(*) as cnt FROM 'graph_edges'",
    "SELECT COUNT(*) as cnt FROM 'vectors'",
    "SELECT COUNT(*) as cnt FROM 'timeseries'",
    "SELECT COUNT(*) as cnt FROM 'spatial'",
    "SELECT COUNT(*) as cnt FROM 'kv'",
    "SELECT COUNT(*) as cnt FROM 'documents'",
    "SELECT COUNT(*) as cnt FROM 'triples'",
    "SELECT COUNT(*) as cnt FROM 'entities'",
    "SELECT COUNT(*) as cnt FROM 'relations'",
    "SELECT COUNT(*) as cnt FROM 'all_data'",
    "SELECT COUNT(*) as cnt FROM 'storage'",
    "SELECT COUNT(*) as cnt FROM 'sst'",
    "SELECT COUNT(*) as cnt FROM 'memtable'",
    "SELECT COUNT(*) as cnt FROM 'wal'",
    "SELECT COUNT(*) as cnt FROM 'index'",
    "SELECT COUNT(*) as cnt FROM 'bloom'",
    "SELECT COUNT(*) as cnt FROM 'cache'",
    "SELECT COUNT(*) as cnt FROM 'ontology'",
    "SELECT COUNT(*) as cnt FROM 'schema'",
    "SELECT COUNT(*) as cnt FROM 'metadata'"
)

foreach ($sql in $queries) {
    $body = @{ query = $sql } | ConvertTo-Json
    try {
        $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
        $json = $resp.Content | ConvertFrom-Json
        if ($json.success) {
            $cnt = $json.data[0].cnt
            if ($cnt -gt 0) {
                Write-Output "$sql => $cnt"
            }
        }
    } catch {
        # Ignore errors
    }
}

# Also try to export with specific class
Write-Output "`n=== Exporting Drug class ==="
$body = @{ class = "Drug"; format = "json"; limit = 2 } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/export -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
if ($json.success) {
    Write-Output "Export message: $($json.data.message)"
    if ($json.data.data) {
        Write-Output "First record: $($json.data.data[0] | ConvertTo-Json -Depth 3)"
    }
}
