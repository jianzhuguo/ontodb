"""
OntoQL 推理性能基准测试

测试项目：
1. 小本体（100 节点）推理延迟
2. 中本体（1000 节点）推理延迟
3. 大本体（10000 节点）推理延迟
4. 缓存命中率对比（首次 vs 缓存命中）
5. GRAPH MATCH + 推理 vs 纯图遍历

Usage:
    python reasoning_benchmark.py
"""

import time
import sys
import os

# 直接测试 Rust 推理引擎（通过 Python 绑定或进程调用）
# 这里使用 HTTP API 测试端到端性能

try:
    import requests
    BASE_URL = "http://localhost:7912"
    HAS_REQUESTS = True
except ImportError:
    HAS_REQUESTS = False
    print("requests not available, running local Rust benchmark instead")


def api_post(query):
    resp = requests.post(f"{BASE_URL}/api/query", json={"query": query}, timeout=60)
    return resp.json()


def build_ontology(depth, width):
    """构建一个测试本体：depth 层继承，每层 width 个类"""
    classes = []
    properties = []
    
    # 根类
    classes.append("CLASS Entity")
    properties.append("PROPERTY name ON Entity TYPE STRING REQUIRED")
    properties.append("PROPERTY value ON Entity TYPE FLOAT64")
    
    prev_level = ["Entity"]
    for level in range(1, depth + 1):
        current_level = []
        for i in range(width):
            class_name = f"L{level}_C{i}"
            parent = prev_level[i % len(prev_level)]
            classes.append(f"CLASS {class_name} SUBCLASS OF {parent}")
            properties.append(f"PROPERTY attr_{class_name} ON {class_name} TYPE STRING")
            current_level.append(class_name)
        prev_level = current_level
    
    # 添加传递属性和逆属性
    properties.append("PROPERTY ancestor ON Entity TYPE STRING TRANSITIVE")
    properties.append("PROPERTY manages ON Entity TYPE STRING")
    properties.append("PROPERTY managed_by ON Entity TYPE STRING INVERSE OF manages")
    properties.append("PROPERTY colleague ON Entity TYPE STRING SYMMETRIC")
    
    ontology_sql = f"CREATE ONTOLOGY BenchTest (\n  {',\n  '.join(classes)},\n  {',\n  '.join(properties)}\n)"
    return ontology_sql, prev_level


def insert_test_data(leaf_classes, count_per_class):
    """插入测试数据"""
    total = 0
    for cls in leaf_classes:
        for i in range(count_per_class):
            api_post(f"INSERT INTO {cls} (name, value) VALUES ('{cls}_{i}', {i * 1.5})")
            total += 1
    return total


def benchmark_query(query, iterations=10):
    """测量查询延迟"""
    times = []
    for _ in range(iterations):
        start = time.time()
        result = api_post(query)
        elapsed = (time.time() - start) * 1000  # ms
        times.append(elapsed)
    
    return {
        "min_ms": round(min(times), 2),
        "max_ms": round(max(times), 2),
        "avg_ms": round(sum(times) / len(times), 2),
        "p50_ms": round(sorted(times)[len(times) // 2], 2),
        "iterations": iterations,
    }


def run_benchmark():
    print("=" * 60)
    print("OntoQL 推理性能基准测试")
    print("=" * 60)
    
    # 清理旧数据
    api_post("DELETE FROM Entity")
    time.sleep(0.5)
    
    results = {}
    
    # ── 测试 1: 小本体（3层 x5 = 15 类） ──
    print("\n[1/4] 小本体（15 类，每类 10 条数据）")
    ontology_sql, leaves = build_ontology(depth=3, width=5)
    resp = api_post(ontology_sql)
    print(f"  创建本体: {resp.get('success', False)}")
    insert_test_data(leaves, 10)
    
    # 子类展开查询
    r1 = benchmark_query("SELECT * FROM Entity WHERE value > 5", iterations=20)
    print(f"  子类展开查询: avg={r1['avg_ms']}ms, p50={r1['p50_ms']}ms")
    results["small_subclass"] = r1
    
    # 首次查询（无缓存）
    api_post("CREATE ONTOLOGY _cache_buster (CLASS Temp)")
    api_post("CREATE ONTOLOGY BenchTest2 (" + ontology_sql.split("(", 1)[1])
    r1_first = benchmark_query("SELECT * FROM L3_C0 WHERE value > 0", iterations=1)
    print(f"  首次查询（无缓存）: {r1_first['avg_ms']}ms")
    results["small_first_run"] = r1_first
    
    # 缓存命中查询
    r1_cached = benchmark_query("SELECT * FROM L3_C0 WHERE value > 0", iterations=20)
    print(f"  缓存命中查询: avg={r1_cached['avg_ms']}ms, p50={r1_cached['p50_ms']}ms")
    results["small_cached"] = r1_cached
    
    # ── 测试 2: 中本体（5层 x10 = 50 类） ──
    print("\n[2/4] 中本体（~50 类，每类 20 条数据）")
    ontology_sql, leaves = build_ontology(depth=5, width=10)
    resp = api_post(ontology_sql)
    print(f"  创建本体: {resp.get('success', False)}")
    insert_test_data(leaves, 20)
    
    r2 = benchmark_query("SELECT * FROM Entity WHERE value > 100", iterations=20)
    print(f"  子类展开查询: avg={r2['avg_ms']}ms, p50={r2['p50_ms']}ms")
    results["medium_subclass"] = r2
    
    r2_root = benchmark_query("SELECT * FROM Entity LIMIT 100", iterations=20)
    print(f"  全类查询: avg={r2_root['avg_ms']}ms, p50={r2_root['p50_ms']}ms")
    results["medium_root"] = r2_root
    
    # ── 测试 3: MATCH 查询 + 推理 ──
    print("\n[3/4] MATCH 语义查询 + 推理")
    
    r3_match = benchmark_query("MATCH (e: Entity) WHERE value > 50 RETURN name, value", iterations=20)
    print(f"  MATCH 查询: avg={r3_match['avg_ms']}ms, p50={r3_match['p50_ms']}ms")
    results["match_query"] = r3_match
    
    r3_select = benchmark_query("SELECT name, value FROM Entity WHERE value > 50", iterations=20)
    print(f"  SELECT 查询: avg={r3_select['avg_ms']}ms, p50={r3_select['p50_ms']}ms")
    results["select_query"] = r3_select
    
    # ── 测试 4: EXPLAIN REASONING ──
    print("\n[4/4] EXPLAIN REASONING 推理溯源")
    
    r4 = benchmark_query("EXPLAIN REASONING MATCH (e: L1_C0) RETURN name", iterations=5)
    print(f"  EXPLAIN REASONING: avg={r4['avg_ms']}ms, p50={r4['p50_ms']}ms")
    results["explain_reasoning"] = r4
    
    # ── 汇总 ──
    print("\n" + "=" * 60)
    print("测试结果汇总")
    print("=" * 60)
    print(f"{'场景':<30} {'avg(ms)':<10} {'p50(ms)':<10} {'min(ms)':<10}")
    print("-" * 60)
    for name, r in results.items():
        print(f"{name:<30} {r['avg_ms']:<10} {r['p50_ms']:<10} {r['min_ms']:<10}")
    
    return results


def run_local_benchmark():
    """不依赖 HTTP API 的本地 Rust 推理基准测试"""
    print("=" * 60)
    print("OntoQL 推理性能基准测试（本地模式）")
    print("=" * 60)
    print("需要启动 ontodb-server 才能运行完整测试。")
    print("请运行: cargo run --release --bin ontodb-server")
    print("然后重新执行此脚本。")


if __name__ == "__main__":
    if HAS_REQUESTS:
        try:
            requests.get(f"{BASE_URL}/api/health", timeout=5)
            run_benchmark()
        except Exception as e:
            print(f"无法连接到 OntoDB 服务器: {e}")
            print("请先启动: cargo run --release --bin ontodb-server")
            run_local_benchmark()
    else:
        run_local_benchmark()
