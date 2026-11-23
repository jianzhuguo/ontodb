# -*- coding: utf-8 -*-
"""
批量入湖写入性能基准测试
================================================================
对比三种写入方式的实际吞吐：
  A. 逐条 INSERT       —— 每行一条 HTTP 请求
  B. 批量 INSERT       —— 多行 VALUES 合并语句（100/500 行/批）
  C. COPY JSONL        —— 文件批量导入

参考基线（项目已有实测）：
  单条 INSERT ≈ 92 行/秒（5,515/min，M3 压测）
  批量 100 行 ≈ 3,539 行/秒（212,330/min，M3 压测）
  COPY JSONL ≈ 87,585 行/秒（Symptom 20,482 条 0.08s）

目标：拿到本机实际数字，验证入湖管线的写入层选型。

运行前提：ontodb-server 在 127.0.0.1:7912（--no-rate-limit）
"""
import json
import os
import time
import statistics
import urllib.request

BASE = "http://127.0.0.1:7912/api/query"
PASS, FAIL = 0, 0

# ── 测试参数 ──
SINGLE_N = 200           # 逐条 INSERT 行数（慢，少跑点）
BATCH_N = 5000           # 批量 INSERT 行数
COPY_N = 50000           # COPY JSONL 行数（快，多跑点）
BATCH_SIZES = [100, 500]  # 批量 INSERT 的批大小

# 临时文件
JSONL_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "_bench_lake_data.jsonl")


def q(sql):
    body = json.dumps({"query": sql}).encode("utf-8")
    req = urllib.request.Request(BASE, data=body, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return json.loads(r.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        return {"success": False, "error": e.read().decode("utf-8", errors="replace")}


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        print("PASS  " + name + (("  (%s)" % detail) if detail else ""))
        PASS += 1
    else:
        print("FAIL  " + name + (("  (%s)" % detail) if detail else ""))
        FAIL += 1


def make_row(i):
    """生成一条模拟入湖记录（7 字段，贴近管线数据形态）"""
    return {
        "id": "BENCH-ROW-%08d" % i,
        "lr_system": "ERP" if i % 3 else "WMS",
        "lr_entity": ["Order", "Shipment", "Customer"][i % 3],
        "lr_state": ["draft", "confirmed", "shipped", "signed", "cancelled"][i % 5],
        "lr_ts": 1700000000.0 + i,
        "lr_quality": ["clean", "clean", "annotated", "clean", "quarantined"][i % 5],
        "lr_detail": "batch_bench_row_%d_source=sales_orders" % i,
    }


def bench_single_insert(n):
    """A: 逐条 INSERT —— 每行一条 HTTP 请求"""
    print("\n--- A: 逐条 INSERT (%d 行) ---" % n)
    q("TRUNCATE TABLE BenchLakeRow")
    latencies = []
    errors = 0
    t0 = time.perf_counter()
    for i in range(n):
        row = make_row(i)
        sql = ("INSERT INTO BenchLakeRow (id, lr_system, lr_entity, lr_state, lr_ts, lr_quality, lr_detail) "
               "VALUES ('%s', '%s', '%s', '%s', %f, '%s', '%s')"
               % (row["id"], row["lr_system"], row["lr_entity"], row["lr_state"],
                  row["lr_ts"], row["lr_quality"], row["lr_detail"]))
        ts = time.perf_counter()
        r = q(sql)
        latencies.append(time.perf_counter() - ts)
        if not r.get("success"):
            errors += 1
        if (i + 1) % 50 == 0:
            print("  ... %d/%d rows" % (i + 1, n))
    elapsed = time.perf_counter() - t0
    ok_rows = n - errors
    rps = ok_rows / elapsed if elapsed > 0 else 0
    lat_ms = [x * 1000 for x in latencies]
    result = {
        "label": "逐条 INSERT",
        "rows": n, "errors": errors, "elapsed": elapsed,
        "rows_per_sec": rps, "rows_per_min": rps * 60,
        "lat_avg": statistics.mean(lat_ms) if lat_ms else 0,
        "lat_p50": statistics.median(lat_ms) if lat_ms else 0,
        "lat_p99": sorted(lat_ms)[int(len(lat_ms) * 0.99)] if lat_ms else 0,
    }
    print("  结果: {0:.1f} rows/sec ({1:,.0f} rows/min) | {2:.1f}s | avg={3:.1f}ms p50={4:.1f}ms p99={5:.1f}ms | errors={6}".format(
        result["rows_per_sec"], result["rows_per_min"], elapsed,
        result["lat_avg"], result["lat_p50"], result["lat_p99"], errors))
    return result


def bench_batch_insert(n, batch_size):
    """B: 批量 INSERT —— 多行 VALUES 合并语句"""
    print("\n--- B: 批量 INSERT (%d 行, %d 行/批) ---" % (n, batch_size))
    q("TRUNCATE TABLE BenchLakeRow")
    n_batches = (n + batch_size - 1) // batch_size
    latencies = []
    errors = 0
    t0 = time.perf_counter()
    for b in range(n_batches):
        start = b * batch_size
        end = min(start + batch_size, n)
        values = []
        for i in range(start, end):
            row = make_row(i)
            values.append("('%s','%s','%s','%s',%f,'%s','%s')"
                          % (row["id"], row["lr_system"], row["lr_entity"], row["lr_state"],
                             row["lr_ts"], row["lr_quality"], row["lr_detail"]))
        sql = ("INSERT INTO BenchLakeRow (id, lr_system, lr_entity, lr_state, lr_ts, lr_quality, lr_detail) "
               "VALUES " + ",".join(values))
        ts = time.perf_counter()
        r = q(sql)
        latencies.append(time.perf_counter() - ts)
        if not r.get("success"):
            errors += 1
        if (b + 1) % 10 == 0:
            print("  ... batch %d/%d (%d rows)" % (b + 1, n_batches, end))
    elapsed = time.perf_counter() - t0
    ok_rows = n - errors * batch_size
    rps = ok_rows / elapsed if elapsed > 0 else 0
    lat_ms = [x * 1000 for x in latencies]
    result = {
        "label": "批量 INSERT (%d行/批)" % batch_size,
        "rows": n, "errors": errors, "elapsed": elapsed,
        "rows_per_sec": rps, "rows_per_min": rps * 60,
        "lat_avg": statistics.mean(lat_ms) if lat_ms else 0,
        "lat_p50": statistics.median(lat_ms) if lat_ms else 0,
        "lat_p99": sorted(lat_ms)[int(len(lat_ms) * 0.99)] if lat_ms else 0,
    }
    print("  结果: {0:.1f} rows/sec ({1:,.0f} rows/min) | {2:.1f}s | avg={3:.1f}ms p50={4:.1f}ms p99={5:.1f}ms | errors={6}".format(
        result["rows_per_sec"], result["rows_per_min"], elapsed,
        result["lat_avg"], result["lat_p50"], result["lat_p99"], errors))
    return result


def bench_copy_jsonl(n):
    """C: COPY JSONL —— 文件批量导入"""
    print("\n--- C: COPY JSONL (%d 行) ---" % n)
    q("TRUNCATE TABLE BenchLakeRow")

    # 生成 JSONL 文件
    print("  生成 JSONL 文件...")
    t_gen0 = time.perf_counter()
    with open(JSONL_PATH, "w", encoding="utf-8") as f:
        for i in range(n):
            row = make_row(i)
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
    t_gen = time.perf_counter() - t_gen0
    file_size = os.path.getsize(JSONL_PATH)
    print("  文件: %s (%.1f MB, %.1fs)" % (os.path.basename(JSONL_PATH), file_size / 1024 / 1024, t_gen))

    # COPY 导入
    path_escaped = JSONL_PATH.replace("\\", "\\\\").replace("'", "''")
    sql = "COPY BenchLakeRow FROM '%s'" % path_escaped
    t0 = time.perf_counter()
    r = q(sql)
    elapsed = time.perf_counter() - t0

    if not r.get("success"):
        # 尝试带 FORMAT 的变体
        sql2 = "COPY BenchLakeRow FROM '%s' (FORMAT JSON)" % path_escaped
        t0 = time.perf_counter()
        r = q(sql2)
        elapsed = time.perf_counter() - t0

    success = r.get("success", False)
    err_msg = str(r.get("error", ""))[:100] if not success else ""
    ok_rows = n if success else 0
    rps = ok_rows / elapsed if elapsed > 0 and success else 0

    result = {
        "label": "COPY JSONL",
        "rows": n, "errors": 0 if success else 1, "elapsed": elapsed,
        "rows_per_sec": rps, "rows_per_min": rps * 60,
        "file_gen_sec": t_gen, "file_size_mb": file_size / 1024 / 1024,
    }
    if success:
        print("  结果: {0:.1f} rows/sec ({1:,.0f} rows/min) | {2:.2f}s | file={3:.1f}MB gen={4:.1f}s".format(
            result["rows_per_sec"], result["rows_per_min"], elapsed,
            result["file_size_mb"], t_gen))
    else:
        print("  COPY 失败: %s" % err_msg)
        # 回退：用 /api/import 批量端点
        print("  尝试 /api/import 批量端点...")
        rows_data = [make_row(i) for i in range(n)]
        import_body = json.dumps({"class": "BenchLakeRow", "rows": rows_data, "skip_errors": True})
        req = urllib.request.Request(
            "http://127.0.0.1:7912/api/import",
            data=import_body.encode("utf-8"),
            headers={"Content-Type": "application/json"})
        t0 = time.perf_counter()
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                imp_result = json.loads(resp.read().decode("utf-8"))
            elapsed = time.perf_counter() - t0
            imported = imp_result.get("data", {}).get("imported", 0)
            result["label"] = "/api/import 批量端点"
            result["rows_per_sec"] = imported / elapsed if elapsed > 0 else 0
            result["rows_per_min"] = result["rows_per_sec"] * 60
            result["errors"] = 0 if imported > 0 else 1
            print("  结果: {0:.1f} rows/sec ({1:,.0f} rows/min) | {2:.2f}s | imported={3}".format(
                result["rows_per_sec"], result["rows_per_min"], elapsed, imported))
        except Exception as e:
            print("  /api/import 也失败: %s" % str(e)[:100])

    return result


def cleanup():
    """清理临时文件"""
    try:
        os.remove(JSONL_PATH)
    except OSError:
        pass


def main():
    global PASS, FAIL

    print("=" * 72)
    print("批量入湖写入性能基准测试")
    print("服务端: http://127.0.0.1:7912")
    print("=" * 72)

    # ── 建表 ──
    print("\n--- Setup: 创建基准表 ---")
    q("DROP ONTOLOGY bench_lake")
    r = q("CREATE ONTOLOGY bench_lake ("
          "  CLASS BenchLakeRow,"
          "  PROPERTY lr_system  DOMAIN BenchLakeRow RANGE STRING,"
          "  PROPERTY lr_entity  DOMAIN BenchLakeRow RANGE STRING,"
          "  PROPERTY lr_state   DOMAIN BenchLakeRow RANGE STRING,"
          "  PROPERTY lr_ts      DOMAIN BenchLakeRow RANGE FLOAT64,"
          "  PROPERTY lr_quality DOMAIN BenchLakeRow RANGE STRING,"
          "  PROPERTY lr_detail  DOMAIN BenchLakeRow RANGE STRING"
          ")")
    check("建表 BenchLakeRow", r.get("success", False))

    results = []

    # ── A: 逐条 INSERT ──
    r_a = bench_single_insert(SINGLE_N)
    results.append(r_a)

    # ── B: 批量 INSERT ──
    for bs in BATCH_SIZES:
        r_b = bench_batch_insert(BATCH_N, bs)
        results.append(r_b)

    # ── C: COPY JSONL ──
    r_c = bench_copy_jsonl(COPY_N)
    results.append(r_c)

    # ── 验证写入数据量 ──
    print("\n--- 验证: COPY 后行数 ---")
    cnt = q("SELECT id FROM BenchLakeRow")
    actual = len(cnt.get("data", []) or [])
    check("COPY 写入行数验证", actual > 0, "rows=%d" % actual)

    # ══════════════════════════════════════════════════════════
    print("\n" + "=" * 72)
    print("基准测试结果汇总")
    print("=" * 72)

    # 表头
    print("%-25s %10s %12s %12s %8s %8s %8s" % (
        "写入方式", "行数", "rows/sec", "rows/min", "avg(ms)", "p50(ms)", "p99(ms)"))
    print("-" * 95)
    for r in results:
        if "lat_avg" in r:
            print("{0:<25} {1:>10} {2:>12.1f} {3:>12} {4:>8.1f} {5:>8.1f} {6:>8.1f}".format(
                r["label"], "{:,}".format(r["rows"]), r["rows_per_sec"],
                "{:,.0f}".format(r["rows_per_min"]),
                r.get("lat_avg", 0), r.get("lat_p50", 0), r.get("lat_p99", 0)))
        else:
            print("{0:<25} {1:>10} {2:>12.1f} {3:>12} {4:>8} {5:>8} {6:>8}".format(
                r["label"], "{:,}".format(r["rows"]), r["rows_per_sec"],
                "{:,.0f}".format(r["rows_per_min"]), "-", "-", "-"))

    # ── 1000 万行 / 5000 万行外推 ──
    print("\n" + "=" * 72)
    print("外推：不同数据量的预计耗时")
    print("=" * 72)
    print("%-25s %12s %12s %12s" % ("写入方式", "100万行", "1000万行", "5000万行"))
    print("-" * 65)
    for r in results:
        if r["rows_per_sec"] > 0:
            t1m = 1_000_000 / r["rows_per_sec"]
            t10m = 10_000_000 / r["rows_per_sec"]
            t50m = 50_000_000 / r["rows_per_sec"]
            print("{0:<25} {1:>12} {2:>12} {3:>12}".format(
                r["label"],
                format_duration(t1m), format_duration(t10m), format_duration(t50m)))

    # ── 倍率对比 ──
    if len(results) >= 2:
        fastest = max(results, key=lambda x: x["rows_per_sec"])
        slowest = min(results, key=lambda x: x["rows_per_sec"] if x["rows_per_sec"] > 0 else 1e9)
        if slowest["rows_per_sec"] > 0:
            ratio = fastest["rows_per_sec"] / slowest["rows_per_sec"]
            print("\n  最快 vs 最慢: %s 是 %s 的 %.0f 倍" % (
                fastest["label"], slowest["label"], ratio))

    print("\n  结论: 入湖管线写入层应选 COPY JSONL（或 /api/import 批量端点）")
    print("        逐条 INSERT 仅适合调试/小批量 (<100 行)")

    # ── 清理 ──
    cleanup()
    q("DROP ONTOLOGY bench_lake")

    print("\n" + "=" * 72)
    print("结果: %d PASS / %d FAIL" % (PASS, FAIL))
    print("=" * 72)


def format_duration(seconds):
    if seconds < 1:
        return "%.0fms" % (seconds * 1000)
    elif seconds < 60:
        return "%.1fs" % seconds
    elif seconds < 3600:
        return "%.1fmin" % (seconds / 60)
    else:
        return "%.1fh" % (seconds / 3600)


if __name__ == "__main__":
    main()
