# Internal Architecture

This document covers OntoDB's internal implementation details for developers, contributors, and performance tuning.

## Binary Row Format

OntoDB uses a compact binary format (Tag-Length-Value) for fast document storage and scanning.

### Layout

```
┌─────────────────────────────────────────────┐
│ Header (4 bytes)                             │
│  - version (1 byte)                          │
│  - flags (1 byte)                            │
│  - field_count (2 bytes)                     │
├─────────────────────────────────────────────┤
│ Field 1: tag(1) + len(2) + value(var)       │
│ Field 2: tag(1) + len(2) + value(var)       │
│ ...                                          │
│ Field N: tag(1) + len(2) + value(var)       │
└─────────────────────────────────────────────┘
```

### Benefits

| Benefit | Description |
|---------|-------------|
| Zero-copy | Direct memory access without deserialization |
| Fast scan | Skip fields by length without parsing |
| SIMD-friendly | Aligned memory layout for vectorized operations |
| Compact | No JSON key overhead, minimal framing |

### Type Tags

| Tag | Type | Size |
|-----|------|------|
| 0x01 | Null | 0 bytes |
| 0x02 | Bool | 1 byte |
| 0x03 | Int64 | 8 bytes |
| 0x04 | Float64 | 8 bytes |
| 0x05 | String | variable |
| 0x06 | Bytes | variable |
| 0x07 | Array | variable |
| 0x08 | Object | variable |

## Bloom Filter

Every SSTable has a Bloom filter for O(1) negative lookups.

### Implementation

- **Algorithm**: Double hashing (FNV-1a + Murmur3)
- **False positive rate**: ~1% (configurable)
- **Memory**: ~10 bits per key

### How It Works

```
Query: GET key "user_123"
  1. Check MemTable → miss
  2. Check immutable MemTable → miss
  3. For each SSTable:
     a. Check Bloom filter → "definitely not here" → skip (O(1))
     b. Bloom says "maybe" → binary search in SSTable
```

Without Bloom filter, every SSTable would need to be searched. With Bloom filter, only relevant SSTables are touched.

## Block Cache

LRU cache for decompressed SSTable data blocks.

### Features

| Feature | Description |
|---------|-------------|
| LRU eviction | Least recently used blocks evicted first |
| Prefetch | Adjacent blocks prefetched on sequential scan |
| Adaptive sizing | Cache size adjusts based on hit rate |
| Monotonic counter | O(1) LRU update via counter |

### Configuration

Block cache size is adaptive (16MB - 1GB):
- Starts at 16MB
- Grows when hit rate > 80%
- Shrinks when system memory pressure detected

## Disk-Based B+Tree

Secondary indexes use a disk-based B+Tree with 4KB pages.

### Structure

```
┌─────────────────────────────────────────┐
│ Root Page (4KB)                          │
│  - Keys: [10, 20, 30]                   │
│  - Children: [page_2, page_3, page_4]   │
├─────────────────────────────────────────┤
│ Leaf Pages (4KB each)                    │
│  - Keys + values + sibling pointer      │
│  - Linked list for range scan           │
└─────────────────────────────────────────┘
```

### Features

- **Slotted page layout**: Variable-size records in fixed-size pages
- **LRU buffer pool**: Hot pages cached in memory
- **Leaf sibling pointers**: Efficient range scan via leaf chain
- **Automatic split**: Node splits at 128 keys
- **Redistribute/merge**: Handle underflow on deletion

### Operations

| Operation | Complexity | Description |
|-----------|------------|-------------|
| Point lookup | O(log n) | Root-to-leaf traversal |
| Range scan | O(log n + k) | Find start, follow leaf chain |
| Insert | O(log n) | Find leaf, split if full |
| Delete | O(log n) | Find leaf, redistribute/merge if underflow |

## Index Corruption Recovery

On startup, OntoDB validates secondary indexes:

1. Load `.idx` files from disk
2. Verify checksums
3. If corruption detected:
   - Delete corrupted `.idx` file
   - Rebuild index from LSM entries (`__idx__` prefix)
   - Log warning

This ensures indexes are always consistent with the underlying data.

## Group Commit

Multiple transactions batch their `fsync` into one.

### How It Works

```
Thread 1: BEGIN → write → COMMIT (waits for fsync)
Thread 2: BEGIN → write → COMMIT (waits for fsync)
Thread 3: BEGIN → write → COMMIT (waits for fsync)

Group commit: batch all 3 fsyncs into one syscall
```

### Hybrid Batching

Wait for either:
- N microseconds (default 1000µs = 1ms), OR
- M transactions ready

Whichever comes first. This balances latency vs throughput.

## Adaptive Memory Manager

Dynamically adjusts MemTable and Block Cache sizes.

### MemTable Adaptation

```
if write_rate > threshold:
    memtable_size = min(memtable_size * 2, 256MB)
else if write_rate < threshold / 4:
    memtable_size = max(memtable_size / 2, 4MB)
```

### Block Cache Adaptation

```
if cache_hit_rate > 0.8:
    cache_size = min(cache_size * 2, 1GB)
else if cache_hit_rate < 0.5:
    cache_size = max(cache_size / 2, 16MB)
```

### Memory Pressure

When system memory is low:
1. Shrink MemTable to minimum (4MB)
2. Shrink Block Cache to minimum (16MB)
3. Trigger aggressive compaction
4. Log warning

## TSM Compression

Time-Series Merge format for column-oriented time-series storage.

### Encoding Types

| Encoding | Data Type | Compression | Description |
|----------|-----------|-------------|-------------|
| Delta | Timestamps | ~90% | Store differences between consecutive values |
| Gorilla | Float64 | ~80% | XOR-based encoding for floating point |
| Integer Delta | Int64 | ~85% | Delta encoding for counters |
| zstd block | All | ~60% | Block-level compression |

### Delta Timestamp

```
Timestamps: [1000, 1005, 1012, 1018]
Deltas:     [1000, 5, 7, 6]
```

Storing deltas requires fewer bits than absolute values.

### Gorilla Float

XOR consecutive values, store only non-zero bits:

```
Value 1: 0.5 → 0x3FE0000000000000
Value 2: 0.6 → 0x3FE3333333333333
XOR:         → 0x0013333333333333 (fewer bits to store)
```

## Query Optimization

### Plan Cache

LRU cache (500 entries) for execution plans:

```
Query: "SELECT * FROM users WHERE age > 25"
  1. Hash query AST
  2. Check plan cache → hit → use cached plan
  3. Cache miss → generate plan → cache it
```

### Query Result Cache

LRU cache (1000 entries, 60s TTL) for query results:

```
Query: "SELECT COUNT(*) FROM users"
  1. Hash query string
  2. Check result cache → hit → return cached result
  3. Cache miss → execute query → cache result
```

### Inference Cache

Caches ontology reasoning results:

```
Query: "SELECT * FROM Vehicle"
  1. Check inference cache → miss
  2. Run OWL reasoning: Vehicle → Car, Truck, Motorcycle
  3. Cache result
  4. Next query hits cache
```

### COUNT(*) Fast Path

`COUNT(*)` is computed during scan without materializing rows:

```
Normal: scan → create row objects → count them
Fast:   scan → increment counter (no row objects created)
```

### Read-Only Fast Path

Read-only queries use read locks for concurrency:

```
Read query:  read_lock → execute → release (concurrent)
Write query: write_lock → execute → release (exclusive)
```

### DML Short Lock

Write operations hold the write lock for minimal scope:

```
1. Acquire write lock
2. Take writes from transaction buffer
3. Assign sequence numbers
4. Release write lock
5. Apply to WAL + MemTable (no lock needed)
```

## Query Timeout & Memory Budget

### Per-Query Timeout

Default: 30 seconds. Configurable per query.

```
Check timeout every 1024 rows (cheap: one atomic load)
```

### Per-Query Memory Budget

Default: 256MB. Prevents OOM from large result sets.

```
if memory_used > budget:
    return error("Query exceeded memory budget")
```

## MVCC (Multi-Version Concurrency Control)

### Snapshot Isolation

Each transaction sees a consistent snapshot:

```
Transaction A (started at seq=100):
  - Sees all writes with seq <= 100
  - Does not see writes with seq > 100

Transaction B (started at seq=105):
  - Sees all writes with seq <= 105
  - Sees A's writes (committed at seq=102)
```

### Sequence Numbers

Every write gets a monotonically increasing sequence number:

```
put(key, value) → seq=1001
put(key, value) → seq=1002
delete(key)     → seq=1003 (tombstone)
```

### Visibility Rules

```
isVisible(entry_seq, txn_start_seq):
    if entry_seq > txn_start_seq → not visible
    if entry is tombstone → not visible
    else → visible
```
