"""
M3 stress test: write >=10,000 ExecEvent records/minute through the HTTP API.
Compares ExecEvent writes vs simple-record writes against PERFORMANCE_REPORT baseline.
"""
import json
import time
import urllib.request
import statistics

BASE = "http://127.0.0.1:7912"
SQL_URL = f"{BASE}/api/query"


def sql(query: str) -> dict:
    body = json.dumps({"query": query}).encode("utf-8")
    req = urllib.request.Request(SQL_URL, data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return {"success": False, "error": e.read().decode("utf-8", errors="replace")}


def bench(label: str, stmts: list[str], n: int) -> dict:
    """Execute statements n times, return throughput stats."""
    latencies = []
    errors = 0
    t0 = time.perf_counter()
    for i in range(n):
        stmt = stmts[i % len(stmts)]
        ts = time.perf_counter()
        try:
            r = sql(stmt)
            if not r.get("success", False):
                errors += 1
        except Exception:
            errors += 1
        latencies.append(time.perf_counter() - ts)
    elapsed = time.perf_counter() - t0

    total_rows = n * getattr(bench, "_rows_per_stmt", 1)
    events_per_min = (total_rows - errors * getattr(bench, "_rows_per_stmt", 1)) / elapsed * 60
    avg_ms = statistics.mean(latencies) * 1000
    p50 = statistics.median(latencies) * 1000
    p99 = sorted(latencies)[int(len(latencies) * 0.99)] * 1000

    print(f"\n[{label}]")
    print(f"  stmts={n}  rows={total_rows}  errors={errors}")
    print(f"  elapsed={elapsed:.2f}s  throughput={events_per_min:,.0f} events/min")
    print(f"  latency avg={avg_ms:.2f}ms  p50={p50:.2f}ms  p99={p99:.2f}ms")

    return {
        "label": label,
        "n": n,
        "rows": total_rows,
        "errors": errors,
        "events_per_min": events_per_min,
        "avg_ms": avg_ms,
        "p50_ms": p50,
        "p99_ms": p99,
    }


def main():
    # 1. Setup: create ExecEvent-like table
    print("Setting up ExecEvent schema ...")
    sql("DROP TABLE BenchExecEvent")
    sql("""
        CREATE ONTOLOGY bench (
            CLASS BenchExecEvent,
            PROPERTY event_agent   DOMAIN BenchExecEvent RANGE STRING,
            PROPERTY event_type    DOMAIN BenchExecEvent RANGE STRING,
            PROPERTY event_ts      DOMAIN BenchExecEvent RANGE INT64,
            PROPERTY entity_ref    DOMAIN BenchExecEvent RANGE STRING,
            PROPERTY event_detail  DOMAIN BenchExecEvent RANGE STRING
        )
    """)

    # 2. Benchmark A: ExecEvent batch inserts (5 fields per record, 100 rows per stmt)
    print("\n--- Phase 1: ExecEvent batch write throughput (100 rows/stmt) ---")
    BATCH = 100
    bench._rows_per_stmt = BATCH
    exec_stmts = []
    for batch in range(20):
        rows = []
        for i in range(BATCH):
            idx = batch * BATCH + i
            rows.append(
                f"('ev-{idx}', 'ExecAgent::robot_01', 'ExecEvent::TaskComplete', "
                f"{1700000000 + idx}, 'Resource::box-{idx%100}', 'cycle={idx} station={idx%5}')"
            )
        exec_stmts.append(
            "INSERT INTO BenchExecEvent (id, event_agent, event_type, event_ts, entity_ref, event_detail) "
            + "VALUES " + ",".join(rows)
        )
    result_a = bench("ExecEvent batch writes (100 rows/batch)", exec_stmts, 20)

    # 3. Benchmark B: simple batch writes (for degradation comparison)
    print("\n--- Phase 2: Simple batch write baseline (100 rows/stmt) ---")
    bench._rows_per_stmt = BATCH
    sql("DROP TABLE BenchSimple")
    sql("""
        CREATE ONTOLOGY bench2 (
            CLASS BenchSimple,
            PROPERTY val DOMAIN BenchSimple RANGE STRING
        )
    """)
    simple_stmts = []
    for batch in range(20):
        rows = []
        for i in range(BATCH):
            idx = batch * BATCH + i
            rows.append(f"('s-{idx}', 'payload-{idx}')")
        simple_stmts.append(
            "INSERT INTO BenchSimple (id, val) VALUES " + ",".join(rows)
        )
    result_b = bench("Simple batch writes (100 rows/batch)", simple_stmts, 20)

    # 4. Verdict
    print("\n" + "=" * 50)
    print("M3 STRESS TEST VERDICT")
    print("=" * 50)
    threshold = 10_000
    ok = result_a["events_per_min"] >= threshold and result_a["errors"] == 0
    print(f"  Threshold:  >= {threshold:,} events/min")
    print(f"  ExecEvent:  {result_a['events_per_min']:,.0f} events/min  {'PASS' if ok else 'FAIL'}")

    # Degradation check: ExecEvent (5 fields) should not be >5x slower than simple (1 field)
    ratio = result_b["events_per_min"] / max(result_a["events_per_min"], 1)
    print(f"  Degradation vs simple: {ratio:.1f}x  {'OK' if ratio < 5 else 'WARN'}")

    if ok:
        print("\n>>> M3 HIGH-FREQ WRITES: PASS <<<")
    else:
        print("\n>>> M3 HIGH-FREQ WRITES: FAIL <<<")

    return ok


if __name__ == "__main__":
    main()
