#!/usr/bin/env python3
"""
OntoDB Performance Benchmark Automation System

Features:
- Run all benchmarks (storage, raft, ontology, graph, query)
- Parse structured output from custom benchmark harnesses
- Compare against baseline and detect regressions
- Generate markdown/HTML reports
- CI-friendly exit codes

Usage:
    python scripts/benchmark_automation.py                    # Run all benchmarks
    python scripts/benchmark_automation.py --storage          # Storage only
    python scripts/benchmark_automation.py --baseline         # Save as baseline
    python scripts/benchmark_automation.py --compare          # Compare with baseline
    python scripts/benchmark_automation.py --report           # Generate report
    python scripts/benchmark_automation.py --ci               # CI mode (fail on regression)
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, asdict
from datetime import datetime
from pathlib import Path
from typing import Optional

# ─── Configuration ───────────────────────────────────────────────────────────

PROJECT_ROOT = Path(__file__).parent.parent
RESULTS_DIR = PROJECT_ROOT / "benchmark_results"
BASELINE_FILE = RESULTS_DIR / "baseline.json"
REGRESSION_THRESHOLD = 5.0  # 5% regression triggers warning
REGRESSION_FAIL_THRESHOLD = 10.0  # 10% regression triggers failure in CI

# ─── Data Models ─────────────────────────────────────────────────────────────

@dataclass
class BenchmarkMetric:
    """Single benchmark metric."""
    name: str
    value: float
    unit: str  # "ops/sec", "ms", "µs", "bytes"
    raw_line: str = ""

@dataclass
class BenchmarkResult:
    """Result of a single benchmark run."""
    name: str
    crate: str
    bench_file: str
    timestamp: str
    duration_ms: int
    metrics: list
    raw_output: str
    success: bool
    error: Optional[str] = None

@dataclass
class RegressionReport:
    """Report of performance regression."""
    metric_name: str
    baseline_value: float
    current_value: float
    change_pct: float
    severity: str  # "warning", "critical"
    unit: str

# ─── Benchmark Parsers ───────────────────────────────────────────────────────

class BenchmarkParser:
    """Parse output from custom benchmark harnesses."""
    
    # Pattern: "  10000 entries:    1.23s (  1,234,567 ops/sec)"
    OPS_PATTERN = re.compile(
        r'([\d,]+)\s+entries?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(\s*([\d,]+(?:\.\d+)?)\s+ops/sec\)'
    )
    
    # Pattern: "  batch_size=100, 1000 batches:    1.23s (  1,234,567 ops/sec)"
    BATCH_PATTERN = re.compile(
        r'batch_size=(\d+),\s+(\d+)\s+batches?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(\s*([\d,]+(?:\.\d+)?)\s+ops/sec\)'
    )
    
    # Pattern: "  10000 entries:    1.23s (snapshot size: 123 KB)"
    SNAPSHOT_PATTERN = re.compile(
        r'([\d,]+)\s+entries?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(snapshot size:\s+([\d,]+)\s+KB\)'
    )
    
    # Pattern: "  10000 entries: write=   1.23s, recover=   0.45s"
    RECOVERY_PATTERN = re.compile(
        r'([\d,]+)\s+entries?:\s+'
        r'write=([\d.]+)\s*(ms|µs|s),\s+'
        r'recover=([\d.]+)\s*(ms|µs|s)'
    )
    
    # Pattern: "  4 threads,  10000 entries:    1.23s (  1,234,567 ops/sec)"
    CONCURRENT_PATTERN = re.compile(
        r'(\d+)\s+threads?,\s+([\d,]+)\s+entries?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(\s*([\d,]+(?:\.\d+)?)\s+ops/sec\)'
    )
    
    # Pattern: "  Sequential scan (1 thread):    1.23s"
    SIMPLE_DURATION_PATTERN = re.compile(
        r'(.*?):\s+([\d.]+)\s*(ms|µs|s)'
    )
    
    # Pattern: "  {} writes:    1.23s  (1,234,567 writes/sec)"
    WRITE_PATTERN = re.compile(
        r'([\d,]+)\s+writes?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(\s*([\d,]+(?:\.\d+)?)\s+writes/sec\)'
    )
    
    # Pattern: "  {} reads:    1.23s  (1,234,567 reads/sec)"
    READ_PATTERN = re.compile(
        r'([\d,]+)\s+reads?:\s+'
        r'([\d.]+)\s*(ms|µs|s)\s+'
        r'\(\s*([\d,]+(?:\.\d+)?)\s+reads/sec\)'
    )
    
    @staticmethod
    def parse_duration(value: float, unit: str) -> float:
        """Convert duration to milliseconds."""
        if unit == 'µs':
            return value / 1000
        elif unit == 's':
            return value * 1000
        return value
    
    @classmethod
    def parse_storage_output(cls, output: str) -> list:
        """Parse storage benchmark output."""
        metrics = []
        
        # Parse write throughput
        for match in cls.WRITE_PATTERN.finditer(output):
            count, duration, unit, ops = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"write_throughput_{count}",
                value=float(ops.replace(',', '')),
                unit="ops/sec",
                raw_line=match.group(0)
            ))
        
        # Parse read throughput
        for match in cls.READ_PATTERN.finditer(output):
            count, duration, unit, ops = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"read_throughput_{count}",
                value=float(ops.replace(',', '')),
                unit="ops/sec",
                raw_line=match.group(0)
            ))
        
        # Parse concurrent scan
        for match in cls.OPS_PATTERN.finditer(output):
            if 'scan' in match.group(0).lower() or 'concurrent' in match.group(0).lower():
                entries, duration, unit, ops = match.groups()
                metrics.append(BenchmarkMetric(
                    name=f"concurrent_scan_{entries}",
                    value=float(ops.replace(',', '')),
                    unit="ops/sec",
                    raw_line=match.group(0)
                ))
        
        return metrics
    
    @classmethod
    def parse_raft_output(cls, output: str) -> list:
        """Parse raft benchmark output."""
        metrics = []
        
        # Parse standard ops/sec metrics
        for match in cls.OPS_PATTERN.finditer(output):
            entries, duration, unit, ops = match.groups()
            # Determine metric name from context
            line = match.group(0)
            if 'append' in output[max(0, match.start()-200):match.start()].lower():
                name = f"log_append_{entries}"
            elif 'read' in output[max(0, match.start()-200):match.start()].lower():
                name = f"log_read_{entries}"
            elif 'apply' in output[max(0, match.start()-200):match.start()].lower():
                name = f"state_machine_apply_{entries}"
            elif 'concurrent' in output[max(0, match.start()-200):match.start()].lower():
                name = f"concurrent_write_{entries}"
            else:
                name = f"ops_{entries}"
            
            metrics.append(BenchmarkMetric(
                name=name,
                value=float(ops.replace(',', '')),
                unit="ops/sec",
                raw_line=match.group(0)
            ))
        
        # Parse batch apply
        for match in cls.BATCH_PATTERN.finditer(output):
            batch_size, num_batches, duration, unit, ops = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"batch_apply_{batch_size}",
                value=float(ops.replace(',', '')),
                unit="ops/sec",
                raw_line=match.group(0)
            ))
        
        # Parse snapshot build
        for match in cls.SNAPSHOT_PATTERN.finditer(output):
            entries, duration, unit, size_kb = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"snapshot_build_{entries}",
                value=cls.parse_duration(float(duration), unit),
                unit="ms",
                raw_line=match.group(0)
            ))
        
        # Parse restart recovery
        for match in cls.RECOVERY_PATTERN.finditer(output):
            entries, write_dur, write_unit, recover_dur, recover_unit = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"recovery_write_{entries}",
                value=cls.parse_duration(float(write_dur), write_unit),
                unit="ms",
                raw_line=match.group(0)
            ))
            metrics.append(BenchmarkMetric(
                name=f"recovery_read_{entries}",
                value=cls.parse_duration(float(recover_dur), recover_unit),
                unit="ms",
                raw_line=match.group(0)
            ))
        
        return metrics
    
    @classmethod
    def parse_ontology_output(cls, output: str) -> list:
        """Parse ontology benchmark output."""
        metrics = []
        
        # Parse timing patterns
        for match in cls.SIMPLE_DURATION_PATTERN.finditer(output):
            name_raw, duration, unit = match.groups()
            name = name_raw.strip().lower().replace(' ', '_')
            metrics.append(BenchmarkMetric(
                name=name,
                value=cls.parse_duration(float(duration), unit),
                unit="ms",
                raw_line=match.group(0)
            ))
        
        return metrics
    
    @classmethod
    def parse_graph_output(cls, output: str) -> list:
        """Parse graph benchmark output."""
        metrics = []
        
        for match in cls.OPS_PATTERN.finditer(output):
            entries, duration, unit, ops = match.groups()
            metrics.append(BenchmarkMetric(
                name=f"graph_ops_{entries}",
                value=float(ops.replace(',', '')),
                unit="ops/sec",
                raw_line=match.group(0)
            ))
        
        return metrics

# ─── Benchmark Runner ────────────────────────────────────────────────────────

class BenchmarkRunner:
    """Run and manage benchmarks."""
    
    BENCHMARKS = {
        'storage': {
            'crate': 'onto-storage',
            'bench_files': ['lock_contention', 'vector_recall'],
            'parser': BenchmarkParser.parse_storage_output,
        },
        'raft': {
            'crate': 'onto-raft',
            'bench_files': ['raft_bench'],
            'parser': BenchmarkParser.parse_raft_output,
        },
        'ontology': {
            'crate': 'onto-ontology',
            'bench_files': ['ontology_bench'],
            'parser': BenchmarkParser.parse_ontology_output,
        },
        'graph': {
            'crate': 'onto-graph',
            'bench_files': ['graph_bench', 'graph_vector_recall'],
            'parser': BenchmarkParser.parse_graph_output,
        },
    }
    
    def __init__(self, project_root: Path):
        self.project_root = project_root
        self.results_dir = project_root / "benchmark_results"
        self.results_dir.mkdir(exist_ok=True)
    
    def run_benchmark(self, category: str, crate: str, bench_file: str) -> BenchmarkResult:
        """Run a single benchmark and capture output."""
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        name = f"{crate}_{bench_file}"
        
        print(f"  Running {name}...", end=" ", flush=True)
        
        start_time = time.time()
        try:
            # Use bytes mode to avoid encoding issues on Windows
            result = subprocess.run(
                ["cargo", "bench", "--bench", bench_file, "-p", crate],
                cwd=self.project_root,
                capture_output=True,
                timeout=600,  # 10 minute timeout
            )
            duration_ms = int((time.time() - start_time) * 1000)
            
            # Decode with fallback encoding for Windows
            try:
                stdout = result.stdout.decode('utf-8')
            except UnicodeDecodeError:
                stdout = result.stdout.decode('gbk', errors='replace')
            try:
                stderr = result.stderr.decode('utf-8')
            except UnicodeDecodeError:
                stderr = result.stderr.decode('gbk', errors='replace')
            
            success = result.returncode == 0
            output = stdout + stderr
            
            if not success:
                print(f"FAILED (exit code {result.returncode})")
                return BenchmarkResult(
                    name=name,
                    crate=crate,
                    bench_file=bench_file,
                    timestamp=timestamp,
                    duration_ms=duration_ms,
                    metrics=[],
                    raw_output=output,
                    success=False,
                    error=f"Exit code {result.returncode}"
                )
            
            print(f"OK ({duration_ms}ms)")
            
            # Parse metrics
            parser = self.BENCHMARKS.get(category, {}).get('parser', lambda x: [])
            metrics = parser(output)
            
            return BenchmarkResult(
                name=name,
                crate=crate,
                bench_file=bench_file,
                timestamp=timestamp,
                duration_ms=duration_ms,
                metrics=[asdict(m) for m in metrics],
                raw_output=output,
                success=True
            )
            
        except subprocess.TimeoutExpired:
            print("TIMEOUT")
            return BenchmarkResult(
                name=name,
                crate=crate,
                bench_file=bench_file,
                timestamp=timestamp,
                duration_ms=600000,
                metrics=[],
                raw_output="",
                success=False,
                error="Benchmark timed out after 10 minutes"
            )
        except Exception as e:
            print(f"ERROR: {e}")
            return BenchmarkResult(
                name=name,
                crate=crate,
                bench_file=bench_file,
                timestamp=timestamp,
                duration_ms=0,
                metrics=[],
                raw_output="",
                success=False,
                error=str(e)
            )
    
    def run_all(self, categories: list = None) -> list:
        """Run all benchmarks or specified categories."""
        if categories is None:
            categories = list(self.BENCHMARKS.keys())
        
        results = []
        
        print("\n" + "=" * 70)
        print("           OntoDB Performance Benchmark Automation")
        print("=" * 70)
        print(f"\n  Categories: {', '.join(categories)}")
        print(f"  Results:    {self.results_dir}")
        print()
        
        for category in categories:
            if category not in self.BENCHMARKS:
                print(f"  Unknown category: {category}")
                continue
            
            bench_config = self.BENCHMARKS[category]
            crate = bench_config['crate']
            
            print(f"\n┌─ {category.upper()} BENCHMARKS ─────────────────────────────────")
            
            for bench_file in bench_config['bench_files']:
                result = self.run_benchmark(category, crate, bench_file)
                results.append(result)
                
                # Save individual result
                result_file = self.results_dir / f"{result.name}_{result.timestamp}.json"
                with open(result_file, 'w', encoding='utf-8') as f:
                    json.dump(asdict(result), f, indent=2)
            
            print(f"└──────────────────────────────────────────────────────────────\n")
        
        return results

# ─── Baseline Management ─────────────────────────────────────────────────────

class BaselineManager:
    """Manage benchmark baselines."""
    
    def __init__(self, results_dir: Path):
        self.results_dir = results_dir
        self.baseline_file = results_dir / "baseline.json"
    
    def save_baseline(self, results: list):
        """Save results as new baseline."""
        baseline = {
            'timestamp': datetime.now().isoformat(),
            'results': {}
        }
        
        for result in results:
            if result.success:
                baseline['results'][result.name] = {
                    'metrics': result.metrics,
                    'duration_ms': result.duration_ms
                }
        
        with open(self.baseline_file, 'w', encoding='utf-8') as f:
            json.dump(baseline, f, indent=2)
        
        print(f"\n  Baseline saved: {self.baseline_file}")
    
    def load_baseline(self) -> Optional[dict]:
        """Load existing baseline."""
        if not self.baseline_file.exists():
            return None
        
        with open(self.baseline_file, encoding='utf-8') as f:
            return json.load(f)
    
    def compare_with_baseline(self, current_results: list) -> list:
        """Compare current results with baseline."""
        baseline = self.load_baseline()
        if baseline is None:
            print("\n  No baseline found. Run with --baseline first.")
            return []
        
        regressions = []
        
        print("\n" + "=" * 70)
        print("           Baseline Comparison")
        print("=" * 70)
        
        for result in current_results:
            if not result.success:
                continue
            
            bench_baseline = baseline['results'].get(result.name)
            if bench_baseline is None:
                print(f"\n  {result.name}: NEW (no baseline)")
                continue
            
            print(f"\n  {result.name}:")
            
            # Compare metrics
            baseline_metrics = {m['name']: m for m in bench_baseline['metrics']}
            
            for metric in result.metrics:
                metric_name = metric['name']
                if metric_name in baseline_metrics:
                    baseline_val = baseline_metrics[metric_name]['value']
                    current_val = metric['value']
                    
                    if baseline_val > 0:
                        # For ops/sec, higher is better
                        if metric['unit'] == 'ops/sec':
                            change_pct = ((current_val - baseline_val) / baseline_val) * 100
                        # For ms/µs, lower is better
                        else:
                            change_pct = ((baseline_val - current_val) / baseline_val) * 100
                        
                        status = ""
                        severity = None
                        
                        if change_pct < -REGRESSION_FAIL_THRESHOLD:
                            status = "CRITICAL REGRESSION"
                            severity = "critical"
                        elif change_pct < -REGRESSION_THRESHOLD:
                            status = "REGRESSION"
                            severity = "warning"
                        elif change_pct > REGRESSION_THRESHOLD:
                            status = "IMPROVED"
                        else:
                            status = "STABLE"
                        
                        print(f"    {metric_name}: {baseline_val:.1f} → {current_val:.1f} "
                              f"({change_pct:+.1f}%) [{status}]")
                        
                        if severity:
                            regressions.append(RegressionReport(
                                metric_name=f"{result.name}/{metric_name}",
                                baseline_value=baseline_val,
                                current_value=current_val,
                                change_pct=change_pct,
                                severity=severity,
                                unit=metric['unit']
                            ))
        
        return regressions

# ─── Report Generator ────────────────────────────────────────────────────────

class ReportGenerator:
    """Generate benchmark reports."""
    
    def __init__(self, results_dir: Path):
        self.results_dir = results_dir
    
    def generate_markdown(self, results: list, regressions: list = None) -> str:
        """Generate markdown report."""
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        
        report = f"""# OntoDB Performance Benchmark Report

**Generated:** {timestamp}
**Results Directory:** {self.results_dir}

## Summary

| Benchmark | Status | Duration | Key Metrics |
|-----------|--------|----------|-------------|
"""
        
        for result in results:
            status = "✅ PASS" if result.success else "❌ FAIL"
            duration = f"{result.duration_ms}ms"
            
            # Get key metrics
            key_metrics = []
            for m in result.metrics[:3]:  # Top 3 metrics
                if m['unit'] == 'ops/sec':
                    key_metrics.append(f"{m['name']}: {m['value']:,.0f} ops/sec")
                else:
                    key_metrics.append(f"{m['name']}: {m['value']:.1f} {m['unit']}")
            
            metrics_str = "; ".join(key_metrics) if key_metrics else "N/A"
            report += f"| {result.name} | {status} | {duration} | {metrics_str} |\n"
        
        # Regressions section
        if regressions:
            report += "\n## Regressions Detected\n\n"
            report += "| Metric | Baseline | Current | Change | Severity |\n"
            report += "|--------|----------|---------|--------|----------|\n"
            
            for reg in regressions:
                severity_emoji = "🔴" if reg.severity == "critical" else "🟡"
                report += (f"| {reg.metric_name} | {reg.baseline_value:.1f} | "
                          f"{reg.current_value:.1f} | {reg.change_pct:+.1f}% | "
                          f"{severity_emoji} {reg.severity} |\n")
        
        # Detailed results
        report += "\n## Detailed Results\n\n"
        
        for result in results:
            report += f"### {result.name}\n\n"
            
            if result.success:
                report += "**Metrics:**\n\n"
                for m in result.metrics:
                    if m['unit'] == 'ops/sec':
                        report += f"- {m['name']}: {m['value']:,.0f} ops/sec\n"
                    else:
                        report += f"- {m['name']}: {m['value']:.2f} {m['unit']}\n"
                
                report += f"\n**Duration:** {result.duration_ms}ms\n"
            else:
                report += f"**Error:** {result.error}\n"
            
            report += "\n---\n\n"
        
        return report
    
    def save_report(self, content: str, format: str = "md") -> Path:
        """Save report to file."""
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        filename = f"benchmark_report_{timestamp}.{format}"
        filepath = self.results_dir / filename
        
        with open(filepath, 'w', encoding='utf-8') as f:
            f.write(content)
        
        return filepath

# ─── Main Entry Point ────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="OntoDB Performance Benchmark Automation")
    parser.add_argument("--storage", action="store_true", help="Run storage benchmarks only")
    parser.add_argument("--raft", action="store_true", help="Run raft benchmarks only")
    parser.add_argument("--ontology", action="store_true", help="Run ontology benchmarks only")
    parser.add_argument("--graph", action="store_true", help="Run graph benchmarks only")
    parser.add_argument("--all", action="store_true", help="Run all benchmarks (default)")
    parser.add_argument("--baseline", action="store_true", help="Save results as baseline")
    parser.add_argument("--compare", action="store_true", help="Compare with baseline")
    parser.add_argument("--report", action="store_true", help="Generate report")
    parser.add_argument("--ci", action="store_true", help="CI mode (fail on critical regression)")
    
    args = parser.parse_args()
    
    # Determine categories
    categories = []
    if args.storage:
        categories.append('storage')
    if args.raft:
        categories.append('raft')
    if args.ontology:
        categories.append('ontology')
    if args.graph:
        categories.append('graph')
    
    if not categories:
        categories = ['storage', 'raft', 'ontology', 'graph']
    
    # Initialize components
    runner = BenchmarkRunner(PROJECT_ROOT)
    baseline_mgr = BaselineManager(RESULTS_DIR)
    report_gen = ReportGenerator(RESULTS_DIR)
    
    # Run benchmarks
    results = runner.run_all(categories)
    
    # Save as baseline if requested
    if args.baseline:
        baseline_mgr.save_baseline(results)
    
    # Compare with baseline if requested
    regressions = []
    if args.compare or args.ci:
        regressions = baseline_mgr.compare_with_baseline(results)
    
    # Generate report if requested
    if args.report:
        report_content = report_gen.generate_markdown(results, regressions)
        report_path = report_gen.save_report(report_content)
        print(f"\n  Report saved: {report_path}")
    
    # Print summary
    print("\n" + "=" * 70)
    print("           Summary")
    print("=" * 70)
    
    total = len(results)
    passed = sum(1 for r in results if r.success)
    failed = total - passed
    
    print(f"\n  Total:  {total}")
    print(f"  Passed: {passed}")
    print(f"  Failed: {failed}")
    
    if regressions:
        critical = sum(1 for r in regressions if r.severity == "critical")
        warnings = sum(1 for r in regressions if r.severity == "warning")
        print(f"\n  Regressions: {critical} critical, {warnings} warnings")
    
    # CI mode: exit with failure if critical regressions
    if args.ci and any(r.severity == "critical" for r in regressions):
        print("\n  ❌ CI FAILED: Critical performance regressions detected!")
        sys.exit(1)
    
    if failed > 0:
        print("\n  ⚠️  Some benchmarks failed. Check logs for details.")
        sys.exit(1)
    
    print("\n  ✅ All benchmarks passed.")
    sys.exit(0)

if __name__ == "__main__":
    main()
