# Quick benchmark runner - wrapper for benchmark_automation.py
# Usage:
#   .\scripts\bench.ps1              # Run all benchmarks
#   .\scripts\bench.ps1 storage      # Storage only
#   .\scripts\bench.ps1 raft         # Raft only
#   .\scripts\bench.ps1 baseline     # Save as baseline
#   .\scripts\bench.ps1 compare      # Compare with baseline
#   .\scripts\bench.ps1 report       # Generate report

param(
    [string]$Action = "all"
)

$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Script = Join-Path $ProjectRoot "scripts\benchmark_automation.py"

# Build arguments
$Args = @()
switch ($Action.ToLower()) {
    "storage"  { $Args += "--storage" }
    "raft"     { $Args += "--raft" }
    "ontology" { $Args += "--ontology" }
    "graph"    { $Args += "--graph" }
    "baseline" { $Args += "--all"; $Args += "--baseline" }
    "compare"  { $Args += "--all"; $Args += "--compare" }
    "report"   { $Args += "--all"; $Args += "--report" }
    "ci"       { $Args += "--all"; $Args += "--compare"; $Args += "--report"; $Args += "--ci" }
    default    { $Args += "--all" }
}

# Run the automation script
python $Script @Args
