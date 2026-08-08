# OntoDB Performance Benchmark Automation
# Usage: .\scripts\run_benchmarks.ps1 [-Baseline] [-Report] [-All]

param(
    [switch]$Baseline,    # Save results as new baseline
    [switch]$Report,      # Generate comparison report
    [switch]$All,         # Run all benchmarks (storage + raft + ontology + graph)
    [switch]$Storage,     # Run storage benchmarks only
    [switch]$Raft,        # Run raft benchmarks only
    [switch]$Quick        # Quick mode: reduced iterations
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$ResultsDir = Join-Path $ProjectRoot "benchmark_results"
$BaselineFile = Join-Path $ResultsDir "baseline.json"
$Timestamp = Get-Date -Format "yyyyMMdd_HHmmss"

# Create results directory if not exists
if (-not (Test-Path $ResultsDir)) {
    New-Item -ItemType Directory -Path $ResultsDir -Force | Out-Null
}

# Default: run all if no specific flag
if (-not $Storage -and -not $Raft -and -not $All) {
    $All = $true
}

Write-Host "═══════════════════════════════════════════════════════════════" -ForegroundColor Cyan
Write-Host "           OntoDB Performance Benchmark Automation" -ForegroundColor Cyan
Write-Host "═══════════════════════════════════════════════════════════════" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Timestamp: $Timestamp" -ForegroundColor Gray
Write-Host "  Results:   $ResultsDir" -ForegroundColor Gray
Write-Host ""

# ─── Helper Functions ────────────────────────────────────────────────────────

function Run-Benchmark {
    param(
        [string]$Name,
        [string]$Crate,
        [string]$BenchFile
    )
    
    Write-Host "─── Running: $Name ───" -ForegroundColor Yellow
    
    $outputFile = Join-Path $ResultsDir "${Name}_${Timestamp}.txt"
    $jsonFile = Join-Path $ResultsDir "${Name}_${Timestamp}.json"
    
    try {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        
        # Run the benchmark
        $output = & cargo bench --bench $BenchFile -p $Crate -- --output-format=json 2>&1 | Out-String
        
        $sw.Stop()
        
        # Save raw output
        $output | Out-File -FilePath $outputFile -Encoding utf8
        
        # Parse and save structured results
        $results = @{
            name = $Name
            timestamp = $Timestamp
            duration_ms = $sw.ElapsedMilliseconds
            raw_output = $output
        }
        
        $results | ConvertTo-Json -Depth 10 | Out-File -FilePath $jsonFile -Encoding utf8
        
        Write-Host "  Completed in $($sw.ElapsedMilliseconds)ms" -ForegroundColor Green
        Write-Host "  Results: $jsonFile" -ForegroundColor Gray
        
        return $results
    }
    catch {
        Write-Host "  FAILED: $_" -ForegroundColor Red
        return $null
    }
}

function Parse-BenchmarkOutput {
    param([string]$Output)
    
    $metrics = @{}
    $lines = $Output -split "`n"
    
    foreach ($line in $lines) {
        # Parse ops/sec metrics
        if ($line -match '(\d[\d,]+)\s+ops/sec') {
            $ops = [double]($matches[1] -replace ',', '')
            $metrics['ops_per_sec'] = $ops
        }
        
        # Parse duration
        if ($line -match '([\d.]+)\s*(ms|µs|s)') {
            $value = [double]$matches[1]
            $unit = $matches[2]
            switch ($unit) {
                'ms' { $metrics['duration_ms'] = $value }
                'µs' { $metrics['duration_ms'] = $value / 1000 }
                's'  { $metrics['duration_ms'] = $value * 1000 }
            }
        }
    }
    
    return $metrics
}

function Compare-WithBaseline {
    param(
        [string]$BenchName,
        [hashtable]$Current,
        [hashtable]$Baseline
    )
    
    $results = @()
    
    foreach ($key in $Current.Keys) {
        if ($Baseline.ContainsKey($key)) {
            $currentVal = $Current[$key]
            $baselineVal = $Baseline[$key]
            
            if ($baselineVal -gt 0) {
                $change = (($currentVal - $baselineVal) / $baselineVal) * 100
                $status = if ($change -gt 5) { "IMPROVED" } 
                         elseif ($change -lt -5) { "REGRESSION" }
                         else { "STABLE" }
                
                $results += @{
                    metric = $key
                    baseline = $baselineVal
                    current = $currentVal
                    change_pct = [math]::Round($change, 2)
                    status = $status
                }
            }
        }
    }
    
    return $results
}

# ─── Main Benchmark Execution ────────────────────────────────────────────────

$allResults = @()

# Storage Benchmarks
if ($Storage -or $All) {
    Write-Host ""
    Write-Host "╔═══════════════════════════════════════════════════════════════╗" -ForegroundColor Magenta
    Write-Host "║                    STORAGE BENCHMARKS                        ║" -ForegroundColor Magenta
    Write-Host "╚═══════════════════════════════════════════════════════════════╝" -ForegroundColor Magenta
    Write-Host ""
    
    $result = Run-Benchmark -Name "storage_lock_contention" -Crate "onto-storage" -BenchFile "lock_contention"
    if ($result) { $allResults += $result }
    
    $result = Run-Benchmark -Name "storage_vector_recall" -Crate "onto-storage" -BenchFile "vector_recall"
    if ($result) { $allResults += $result }
}

# Raft Benchmarks
if ($Raft -or $All) {
    Write-Host ""
    Write-Host "╔═══════════════════════════════════════════════════════════════╗" -ForegroundColor Magenta
    Write-Host "║                      RAFT BENCHMARKS                         ║" -ForegroundColor Magenta
    Write-Host "╚═══════════════════════════════════════════════════════════════╝" -ForegroundColor Magenta
    Write-Host ""
    
    $result = Run-Benchmark -Name "raft_bench" -Crate "onto-raft" -BenchFile "raft_bench"
    if ($result) { $allResults += $result }
}

# Graph Benchmarks
if ($All) {
    Write-Host ""
    Write-Host "╔═══════════════════════════════════════════════════════════════╗" -ForegroundColor Magenta
    Write-Host "║                     GRAPH BENCHMARKS                         ║" -ForegroundColor Magenta
    Write-Host "╚═══════════════════════════════════════════════════════════════╝" -ForegroundColor Magenta
    Write-Host ""
    
    $result = Run-Benchmark -Name "graph_bench" -Crate "onto-graph" -BenchFile "graph_bench"
    if ($result) { $allResults += $result }
    
    $result = Run-Benchmark -Name "graph_vector_recall" -Crate "onto-graph" -BenchFile "graph_vector_recall"
    if ($result) { $allResults += $result }
}

# Ontology Benchmarks
if ($All) {
    Write-Host ""
    Write-Host "╔═══════════════════════════════════════════════════════════════╗" -ForegroundColor Magenta
    Write-Host "║                   ONTOLOGY BENCHMARKS                        ║" -ForegroundColor Magenta
    Write-Host "╚═══════════════════════════════════════════════════════════════╝" -ForegroundColor Magenta
    Write-Host ""
    
    $result = Run-Benchmark -Name "ontology_bench" -Crate "onto-ontology" -BenchFile "ontology_bench"
    if ($result) { $allResults += $result }
}

# ─── Save Results ────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "═══════════════════════════════════════════════════════════════" -ForegroundColor Cyan

# Save combined results
$combinedFile = Join-Path $ResultsDir "combined_${Timestamp}.json"
$allResults | ConvertTo-Json -Depth 10 | Out-File -FilePath $combinedFile -Encoding utf8
Write-Host "  Combined results: $combinedFile" -ForegroundColor Gray

# ─── Baseline Management ─────────────────────────────────────────────────────

if ($Baseline) {
    Write-Host ""
    Write-Host "  Saving as new baseline..." -ForegroundColor Yellow
    $allResults | ConvertTo-Json -Depth 10 | Out-File -FilePath $BaselineFile -Encoding utf8
    Write-Host "  Baseline saved: $BaselineFile" -ForegroundColor Green
}

# ─── Generate Report ─────────────────────────────────────────────────────────

if ($Report) {
    Write-Host ""
    Write-Host "╔═══════════════════════════════════════════════════════════════╗" -ForegroundColor Magenta
    Write-Host "║                    BENCHMARK REPORT                          ║" -ForegroundColor Magenta
    Write-Host "╚═══════════════════════════════════════════════════════════════╝" -ForegroundColor Magenta
    Write-Host ""
    
    $reportFile = Join-Path $ResultsDir "report_${Timestamp}.md"
    $report = @"
# OntoDB Performance Benchmark Report

**Generated:** $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
**Results Directory:** $ResultsDir

## Summary

| Benchmark | Status | Duration |
|-----------|--------|----------|
"@
    
    foreach ($result in $allResults) {
        $status = if ($result.raw_output -match 'FAILED') { "❌ FAILED" } else { "✅ PASSED" }
        $report += "`n| $($result.name) | $status | $($result.duration_ms)ms |"
    }
    
    $report += @"

## Detailed Results

"@
    
    foreach ($result in $allResults) {
        $report += @"

### $($result.name)

``````
$($result.raw_output)
``````

"@
    }
    
    if (Test-Path $BaselineFile) {
        $report += @"

## Baseline Comparison

Comparing against baseline from $(Get-Item $BaselineFile).LastWriteTime.ToString("yyyy-MM-dd HH:mm:ss")

"@
    }
    
    $report | Out-File -FilePath $reportFile -Encoding utf8
    Write-Host "  Report generated: $reportFile" -ForegroundColor Green
}

Write-Host ""
Write-Host "═══════════════════════════════════════════════════════════════" -ForegroundColor Cyan
Write-Host "  Benchmark automation complete." -ForegroundColor Green
Write-Host "═══════════════════════════════════════════════════════════════" -ForegroundColor Cyan
