# OntoDB Performance Benchmark Report V2

**Date:** 2026-08-07
**Version:** 0.1.0 (Rust LSM-Tree)
**Platform:** Windows (x86_64)
**Build:** Release mode (opt-level=3, LTO)
**Data Scale:** 100,000 rows (10万行)
**Test Iterations:** 200 rounds per test

---

## Executive Summary

OntoDB achieves **989,343 writes/sec** and **1,375,785 reads/sec** under 100K row dataset with 200 iterations. Read-write lock separation delivers **1.49x-1.59x** speedup for concurrent scan operations.

---

## Core Results

| Operation | Count | Time | Throughput |
|-----------|-------|------|-----------|
| **Sequential Write** | 50,000 | 50.5ms | **989,343 writes/sec** |
| **Sequential Read** | 50,000 | 36.3ms | **1,375,785 reads/sec** |

### Write Performance Detail

- **WAL strategy:** Batch sync (`sync_wal_on_commit: false`)
- **MemTable size:** 64MB
- **Key format:** `K::000000000001` (12-digit zero-padded)
- **Value format:** JSON `{"v":N}` (~10-20 bytes per value)
- **Single-threaded, sequential**

### Read Performance Detail

- **Read pattern:** Point lookups (`engine.get(&key)`)
- **100% hit rate**, single-threaded

---

## Lock Contention (100K rows, 200 iterations)

| Test | Time |
|------|------|
| Sequential scan (1 thread) | 24.43s |

### Concurrent scan_prefix: Read Lock vs Write Lock

| Threads | Read Lock | Write Lock | Speedup |
|---------|-----------|------------|---------|
| 2 | 16.16s | 25.69s | **1.59x** |
| 4 | 15.79s | 23.52s | **1.49x** |
| 8 | 14.73s | 22.78s | **1.55x** |

Read-lock concurrency consistently delivers ~1.5x speedup over write-lock serialization.

### Mixed Workload (8 readers + 1 writer with flush)

| Configuration | Time |
|--------------|------|
| 8 readers + 1 writer (50 flushes) | 45.14s |

---

## HNSW Vector Index (5,000 vectors, 128 dimensions)

| Method | Time | Notes |
|--------|------|-------|
| Individual insert | 261ms | One-by-one insertion |
| Batch insert | 242ms | `insert_batch()` |
| Speedup | 1.08x | Batch slightly faster |

---

## Comparison with V1 Report

| Metric | V1 (10K rows, 50K ops) | V2 (100K rows, 50K ops) | Delta |
|--------|------------------------|------------------------|-------|
| Write throughput | ~962K writes/sec | **989,343 writes/sec** | +2.8% |
| Read throughput | ~1.3M reads/sec | **1,375,785 reads/sec** | +5.8% |
| Lock read speedup (8T) | 1.12x | **1.55x** | +38% |
| Dataset size | 10K rows | 100K rows | 10x larger |

V2 uses 10x more data (100K vs 10K rows), making lock contention results more representative of real workloads.

---

## Key Optimizations Verified

1. **WAL Batch Sync** (`sync_wal_on_commit: false`) — Single biggest throughput win.
2. **Read-Write Lock Separation** — 1.49-1.59x concurrent scan speedup at 100K scale.
3. **LSM-Tree Architecture** — Sequential write pattern, near-1M writes/sec.
4. **HNSW Batch Insert** — Slight advantage over individual inserts.

---

## Reproduction

```bash
cd E:\ontodb
cargo bench --bench lock_contention
```

Benchmark source: `crates/onto-storage/benches/lock_contention.rs`

---

*Report generated from actual benchmark output on 2026-08-07.*
