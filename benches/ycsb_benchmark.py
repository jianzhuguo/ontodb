"""
YCSB-style benchmark for OntoDB.

Measures throughput and latency for standard database operations:
- Read, Update, Insert, Scan, Read-Modify-Write
- Mixed workloads (A-F)

Usage:
    python ycsb_benchmark.py [--rows 100000] [--ops 50000] [--threads 1]
"""

import requests
import time
import random
import string
import json
import argparse
import statistics

BASE_URL = "http://localhost:7912"

def api_post(endpoint, data):
    resp = requests.post(f"{BASE_URL}{endpoint}", json=data, timeout=30)
    return resp.json()

def random_string(length=10):
    return ''.join(random.choices(string.ascii_lowercase, k=length))

def random_value():
    return json.dumps({
        "field0": random_string(20),
        "field1": random.randint(0, 100000),
        "field2": round(random.uniform(0, 10000), 2),
        "field3": random_string(50),
        "field4": random.randint(0, 1),
    })

def setup_schema():
    """Create the YCSB table."""
    api_post("/api/query", {"query": "DROP CLASS usertable"})
    time.sleep(0.1)
    api_post("/api/query", {
        "query": "CREATE CLASS usertable (id STRING, field0 STRING, field1 INT, field2 FLOAT, field3 STRING, field4 INT)"
    })

def load_data(num_rows):
    """Load initial data (YCSB Load phase)."""
    print(f"Loading {num_rows} rows...")
    start = time.time()

    for i in range(num_rows):
        key = f"user{i}"
        val = random_value()
        data = json.loads(val)
        api_post("/api/query", {
            "query": f'INSERT INTO usertable SET id = "{key}", field0 = "{data["field0"]}", field1 = {data["field1"]}, field2 = {data["field2"]}, field3 = "{data["field3"]}", field4 = {data["field4"]}'
        })

        if (i + 1) % 1000 == 0:
            elapsed = time.time() - start
            rate = (i + 1) / elapsed
            print(f"  {i+1}/{num_rows} rows loaded ({rate:.0f} ops/sec)")

    elapsed = time.time() - start
    print(f"Load complete: {num_rows} rows in {elapsed:.1f}s ({num_rows/elapsed:.0f} ops/sec)")
    return elapsed

def run_operation(op_type, num_rows):
    """Run a single YCSB operation and return latency in seconds."""
    key = f"user{random.randint(0, num_rows - 1)}"

    start = time.time()

    if op_type == "read":
        api_post("/api/query", {"query": f'SELECT * FROM usertable WHERE id = "{key}"'})

    elif op_type == "update":
        new_val = random_string(20)
        api_post("/api/query", {"query": f'UPDATE usertable SET field0 = "{new_val}" WHERE id = "{key}"'})

    elif op_type == "insert":
        new_key = f"user_new_{random_string(8)}"
        val = random_value()
        data = json.loads(val)
        api_post("/api/query", {
            "query": f'INSERT INTO usertable SET id = "{new_key}", field0 = "{data["field0"]}", field1 = {data["field1"]}, field2 = {data["field2"]}, field3 = "{data["field3"]}", field4 = {data["field4"]}'
        })

    elif op_type == "scan":
        count = random.randint(1, 100)
        api_post("/api/query", {"query": f'SELECT * FROM usertable LIMIT {count}'})

    elif op_type == "read_modify_write":
        api_post("/api/query", {"query": f'SELECT * FROM usertable WHERE id = "{key}"'})
        new_val = random_string(20)
        api_post("/api/query", {"query": f'UPDATE usertable SET field0 = "{new_val}" WHERE id = "{key}"'})

    return time.time() - start

def run_workload(name, workload, num_ops, num_rows):
    """Run a YCSB workload and return metrics."""
    print(f"\nRunning Workload {name}: {num_ops} operations...")

    latencies = []
    start = time.time()

    for i in range(num_ops):
        # Select operation type based on workload distribution
        r = random.random()
        cumulative = 0
        op_type = "read"  # default
        for op, prob in workload.items():
            cumulative += prob
            if r < cumulative:
                op_type = op
                break

        latency = run_operation(op_type, num_rows)
        latencies.append(latency)

        if (i + 1) % 1000 == 0:
            elapsed = time.time() - start
            rate = (i + 1) / elapsed
            print(f"  {i+1}/{num_ops} ops ({rate:.0f} ops/sec)")

    total_time = time.time() - start

    # Calculate metrics
    throughput = num_ops / total_time
    avg_latency = statistics.mean(latencies) * 1000  # ms
    p50 = statistics.median(latencies) * 1000
    p95 = sorted(latencies)[int(len(latencies) * 0.95)] * 1000
    p99 = sorted(latencies)[int(len(latencies) * 0.99)] * 1000

    return {
        "workload": name,
        "operations": num_ops,
        "throughput_ops_sec": round(throughput, 1),
        "total_time_sec": round(total_time, 2),
        "avg_latency_ms": round(avg_latency, 3),
        "p50_latency_ms": round(p50, 3),
        "p95_latency_ms": round(p95, 3),
        "p99_latency_ms": round(p99, 3),
    }

# YCSB Standard Workloads
WORKLOADS = {
    "A": {  # 50% read, 50% update
        "read": 0.5,
        "update": 0.5,
    },
    "B": {  # 95% read, 5% update
        "read": 0.95,
        "update": 0.05,
    },
    "C": {  # 100% read
        "read": 1.0,
    },
    "D": {  # 95% read, 5% insert (latest)
        "read": 0.95,
        "insert": 0.05,
    },
    "E": {  # 95% scan, 5% insert
        "scan": 0.95,
        "insert": 0.05,
    },
    "F": {  # 50% read, 25% update, 25% read-modify-write
        "read": 0.5,
        "update": 0.25,
        "read_modify_write": 0.25,
    },
}

def main():
    parser = argparse.ArgumentParser(description="YCSB-style benchmark for OntoDB")
    parser.add_argument("--rows", type=int, default=10000, help="Number of rows to load")
    parser.add_argument("--ops", type=int, default=10000, help="Operations per workload")
    parser.add_argument("--workloads", nargs="+", default=["A", "B", "C", "F"],
                        help="Workloads to run (A-F)")
    parser.add_argument("--skip-load", action="store_true", help="Skip data loading")
    args = parser.parse_args()

    print("=" * 60)
    print("OntoDB YCSB-Style Benchmark")
    print("=" * 60)
    print(f"Rows: {args.rows}")
    print(f"Operations per workload: {args.ops}")
    print(f"Workloads: {', '.join(args.workloads)}")
    print(f"Server: {BASE_URL}")

    # Setup
    if not args.skip_load:
        setup_schema()
        load_data(args.rows)

    # Run workloads
    results = []
    for wl_name in args.workloads:
        if wl_name not in WORKLOADS:
            print(f"Unknown workload: {wl_name}")
            continue
        result = run_workload(wl_name, WORKLOADS[wl_name], args.ops, args.rows)
        results.append(result)

    # Print results
    print("\n" + "=" * 60)
    print("Results Summary")
    print("=" * 60)
    print(f"{'Workload':<10} {'Throughput':>12} {'Avg Lat':>10} {'P50':>10} {'P95':>10} {'P99':>10}")
    print(f"{'':10} {'(ops/sec)':>12} {'(ms)':>10} {'(ms)':>10} {'(ms)':>10} {'(ms)':>10}")
    print("-" * 62)

    for r in results:
        print(f"{r['workload']:<10} {r['throughput_ops_sec']:>12.1f} {r['avg_latency_ms']:>10.3f} "
              f"{r['p50_latency_ms']:>10.3f} {r['p95_latency_ms']:>10.3f} {r['p99_latency_ms']:>10.3f}")

    # Save JSON report
    report = {
        "benchmark": "YCSB",
        "database": "OntoDB",
        "version": "0.1.0",
        "rows": args.rows,
        "ops_per_workload": args.ops,
        "results": results,
    }

    report_path = "ycsb_results.json"
    with open(report_path, "w") as f:
        json.dump(report, f, indent=2)
    print(f"\nDetailed results saved to {report_path}")

if __name__ == "__main__":
    main()
