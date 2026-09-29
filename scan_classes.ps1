# Scan for all distinct key prefixes in storage
# The keys are stored as "{class}::{id}", so we need to find all distinct class prefixes

# First, let's try to get a sample of keys from different classes to see the pattern
$classes = @("Drug", "Gene", "Protein", "Disease", "interacts_with", "causes", "targets")

foreach ($c in $classes) {
    $body = @{ query = "SELECT __pk__ FROM 'sembio.$c' LIMIT 1" } | ConvertTo-Json
    try {
        $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
        $json = $resp.Content | ConvertFrom-Json
        if ($json.success -and $json.data.Count -gt 0) {
            $pk = $json.data[0].__pk__
            Write-Output "sembio.$c : PK = $pk"
        }
    } catch {
        Write-Output "sembio.$c : ERROR"
    }
}

# Now let's try to find all distinct prefixes by scanning with a very broad filter
Write-Output "`n=== Scanning for all distinct prefixes ==="

# Try to get all unique __class__ values
$body = @{ query = "SELECT DISTINCT __class__ FROM '__all__' LIMIT 100" } | ConvertTo-Json
try {
    $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
    $json = $resp.Content | ConvertFrom-Json
    if ($json.success) {
        Write-Output "Distinct classes found: $($json.data.Count)"
        foreach ($row in $json.data) {
            Write-Output "  $($row.__class__)"
        }
    } else {
        Write-Output "Error: $($json.error)"
    }
} catch {
    Write-Output "Error: $($_.Exception.Message)"
}

# Try another approach - scan with empty prefix
$body = @{ query = "SELECT __class__, COUNT(*) as cnt FROM '__all__' GROUP BY __class__ ORDER BY cnt DESC LIMIT 50" } | ConvertTo-Json
try {
    $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
    $json = $resp.Content | ConvertFrom-Json
    if ($json.success) {
        Write-Output "`n=== All classes with counts ==="
        foreach ($row in $json.data) {
            Write-Output "$($row.__class__) : $($row.cnt)"
        }
    } else {
        Write-Output "Error: $($json.error)"
    }
} catch {
    Write-Output "Error: $($_.Exception.Message)"
}
