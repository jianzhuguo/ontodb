$body = @{ query = "SELECT DISTINCT key_prefix FROM storage_scan LIMIT 200" } | ConvertTo-Json
try {
    $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
    Write-Output $resp.Content
} catch {
    Write-Output $_.Exception.Message
}
