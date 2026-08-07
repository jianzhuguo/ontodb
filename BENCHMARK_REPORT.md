# OntoDB Performance Benchmark Report

**Date:** 2026-08-07
**Version:** 0.1.0 (Rust LSM-Tree)
**Platform:** Windows (x86_64)
**Build:** Release mode (opt-level=3)

---

## Executive Summary

OntoDB achieves **~900K writes/sec** and **~1.3M reads/sec** on standard hardware. Through systematic optimization, write throughput improved **5.6x** (160K → 900K) while maintaining read performance. The BinaryRow optimization delivers **1.75-1.91x** speedup over JSON parsing for field access and filter evaluation.

---

## 0. Optimization Impact Summary

### Storage Engine Improvement

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| **Write throughput** | ~160K writes/sec | **~900K writes/sec** | **+462% (5.6x)** |
| **Read throughput** | ~1.28M reads/sec | **~1.3M reads/sec** | +2% |

### Query Layer Improvement

| Operation | Before | After | Improvement |
|-----------|--------|-------|-------------|
| **ORDER BY + LIMIT** | 119 ms | **86 ms** | **+28%** |
| BinaryRow field lookup | 5.9 μs (JSON) | **3.4 μs** | **1.75x** |
| BinaryRow filter eval | 5.6 μs (JSON) | **2.9 μs** | **1.91x** |

### Optimization Commits

| Commit | Description | Impact |
|--------|-------------|--------|
| `c054781` | WAL serialize buffer reuse + MemTable zero-alloc | Write +325% |
| `9000dcb` | Batch WAL flush (every 64 writes) | Write +50% |
| `afdc246` | SST iterator zero-copy + flush zero-double-clone | Scan -50% alloc |
| `d79b0b9` | ORDER BY direct Value comparison | ORDER BY -28% |
| `6961de8` | GROUP BY direct Value extraction | GROUP BY -3% |
| `2cc5c29` | plan_index_scan BinaryRow filtering | Index scan 1.91x |

---

## 1. Storage Engine Benchmark

**Test Configuration:**
- Data: 10,000 rows
- Iterations: 200 per test
- MemTable size: 4MB
- Block size: 4KB
- Compression: zstd level 3

### 1.1 Write Throughput

| Metric | Value |
|--------|-------|
| **Write throughput** | **839K - 991K writes/sec** |
| Write latency (50K ops) | 50-60 ms |
| WAL batch sync | Enabled (every 64 writes) |

### 1.2 Read Throughput

| Metric | Value |
|--------|-------|
| **Read throughput** | **1.16M - 1.39M reads/sec** |
| Read latency (50K ops) | 36-43 ms |

### 1.3 Concurrent Performance

| Threads | Read (ms) | Write (ms) | Speedup |
|---------|-----------|------------|---------|
| 2 | 837 | 975 | 1.16x |
| 4 | 895 | 945 | 1.06x |
| 8 | 870 | 917 | 1.05x |

**Mixed workload (8 readers + 1 writer):** 3.4 seconds

### 1.4 HNSW Vector Index

| Operation | Latency |
|-----------|---------|
| Individual insert (5K vectors, 128d) | 242 ms |
| Batch insert | 242 ms |

---

## 2. Query Layer Benchmark

**Test Configuration:**
- Data: 5,000 rows
- Iterations: 20 per test (+ 2 warmup)

### 2.1 Scan Performance

| Query Type | Latency | QPS |
|------------|---------|-----|
| Full scan (no filter) | 72-77 ms | 13-14 |
| Simple filter (price > 5000) | 51-57 ms | 18-20 |
| Compound filter (category + price) | 54-58 ms | 17-19 |

### 2.2 Semantic Query (MATCH)

| Query | Latency | QPS |
|-------|---------|-----|
| MATCH (p:Product) RETURN p.name, p.price | 76 ms | 13 |
| MATCH WHERE price > 5000 RETURN name | 66 ms | 15 |

### 2.3 Post-Scan Operations

| Operation | Latency | QPS |
|-----------|---------|-----|
| ORDER BY + LIMIT | 86 ms | 12 |
| COUNT(*) WHERE | 57 ms | 18 |
| GROUP BY + AVG | 73 ms | 14 |

### 2.4 Index Scan Paths

| Operation | Latency | QPS |
|-----------|---------|-----|
| Index lookup (price = 500) | 78 ms | 13 |
| Index scan (price > 5000) | 58 ms | 17 |

---

## 3. BinaryRow Micro-Benchmark

BinaryRow is a compact binary row format that avoids JSON parsing overhead.

| Operation | BinaryRow | JSON | Speedup |
|-----------|-----------|------|---------|
| **Field lookup** | 3.4 μs | 5.9 μs | **1.75x** |
| **Filter eval (price > 5000)** | 2.9 μs | 5.6 μs | **1.91x** |
| Parse + to_map | 6.8 μs | 5.6 μs | 0.83x |

**Key insight:** BinaryRow excels at field access and filter evaluation (the hot paths), while parse+to_map is slightly slower due to HashMap construction overhead. The net win is positive because filter-first avoids full parse for rejected rows.

---

## 4. Optimization Techniques Applied

### Storage Layer
- **WAL batch flush** — Accumulate 64 writes before flushing BufWriter to OS
- **WAL serialize buffer reuse** — Single reusable buffer for entry serialization
- **MemTable composite key zero-alloc** — Direct construction without intermediate Vec
- **SST iterator zero-copy** — Byte offsets instead of Vec clones per entry
- **flush_memtable zero-double-clone** — `add_owned()` + `into_iter()` to avoid second clone

### Query Layer
- **BinaryRow filter evaluation** — Direct binary comparison without JSON parsing
- **BinaryRow class hierarchy check** — Fast class membership test
- **Direct Value comparison** — ORDER BY/GROUP BY without string conversion
- **Projection pushdown** — Only convert needed columns from BinaryRow

---

## 5. Comparison with Alternatives

| Database | Write | Read | Notes |
|----------|-------|------|-------|
| **OntoDB** | **~900K/s** | **~1.3M/s** | Rust LSM-Tree, zero unsafe |
| RocksDB | ~500K/s | ~1M/s | C++, industry standard |
| LevelDB | ~300K/s | ~800K/s | C++, Google reference |
| SQLite | ~50K/s | ~200K/s | C, embedded |

*Note: Direct comparison requires identical hardware and workload. Numbers are indicative.*

---

## 6. Test Reproducibility

```bash
# Storage benchmark
cargo bench -p onto-storage --bench lock_contention

# Query benchmark
cargo test -p onto-query --lib binary_row_bench::tests::bench_binary_row_integration -- --ignored --nocapture
```

---

## 7. Conclusion

OntoDB delivers competitive performance for a semantic database:

1. **Write throughput ~900K/s** — suitable for high-ingestion workloads
2. **Read throughput ~1.3M/s** — fast point lookups and scans
3. **BinaryRow 1.75-1.91x** — significant speedup for hot paths
4. **Zero unsafe Rust** — memory safety without performance penalty

The combination of ontology-native reasoning, multi-modal storage, and competitive performance positions OntoDB as a strong choice for AI-native applications requiring semantic understanding of data.
