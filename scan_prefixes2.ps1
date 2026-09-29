# Scan for all distinct key prefixes in storage
# First, let's try to get a sample of keys to understand the key structure
$body = @{ query = "SELECT __pk__, __class__ FROM 'sembio.Drug' LIMIT 3" } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
Write-Output "=== Sample Drug PKs ==="
foreach ($row in $json.data) {
    Write-Output "PK: $($row.__pk__)"
    Write-Output "Class: $($row.__class__)"
}

# Now let's try to find all distinct prefixes by scanning with a very broad filter
Write-Output "`n=== Scanning for all distinct prefixes ==="

# Try to get all unique __class__ values from all data
$body = @{ query = "SELECT DISTINCT __class__ FROM 'sembio.Drug' LIMIT 100" } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
if ($json.success) {
    Write-Output "Distinct classes in Drug: $($json.data.Count)"
    foreach ($row in $json.data) {
        Write-Output "  $($row.__class__)"
    }
}

# Try to scan with different prefixes to find all data
$prefixes = @(
    "sembio",
    "compound",
    "compoundproperty",
    "protein",
    "gene",
    "drug",
    "disease",
    "pathway",
    "interaction",
    "relation",
    "property",
    "structure",
    "sequence",
    "annotation",
    "metadata",
    "index",
    "cache",
    "temp",
    "test",
    "sample",
    "example"
)

Write-Output "`n=== Scanning for data with different prefixes ==="
foreach ($prefix in $prefixes) {
    $body = @{ query = "SELECT COUNT(*) as cnt FROM '$prefix' LIMIT 1" } | ConvertTo-Json
    try {
        $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
        $json = $resp.Content | ConvertFrom-Json
        if ($json.success -and $json.data[0].cnt -gt 0) {
            Write-Output "Prefix '$prefix': $($json.data[0].cnt) records"
        }
    } catch {
        # Ignore errors
    }
}

# Try to get all data from the storage by scanning with empty prefix
Write-Output "`n=== Scanning for all data with empty prefix ==="
$body = @{ query = "SELECT __class__, COUNT(*) as cnt FROM 'all' GROUP BY __class__ ORDER BY cnt DESC LIMIT 50" } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
if ($json.success) {
    Write-Output "Total classes found: $($json.data.Count)"
    foreach ($row in $json.data) {
        Write-Output "$($row.__class__) : $($row.cnt)"
    }
} else {
    Write-Output "Error: $($json.error)"
}

# Try to get all data from the storage by scanning with wildcard
Write-Output "`n=== Scanning for all data with wildcard ==="
$body = @{ query = "SELECT __class__, COUNT(*) as cnt FROM '*' GROUP BY __class__ ORDER BY cnt DESC LIMIT 50" } | ConvertTo-Json
$resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
$json = $resp.Content | ConvertFrom-Json
if ($json.success) {
    Write-Output "Total classes found: $($json.data.Count)"
    foreach ($row in $json.data) {
        Write-Output "$($row.__class__) : $($row.cnt)"
    }
} else {
    Write-Output "Error: $($json.error)"
}
