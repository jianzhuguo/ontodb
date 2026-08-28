# Changelog

All notable changes to OntoDB will be documented in this file.

## [v0.7.0] - 2026-08-29

### GRAPH MATCH Engine (New)

- Implement GRAPH MATCH executor with multi-hop pattern matching
- Support `(a:Label) -[e:edge]-> (b:Label)` syntax with WHERE/RETURN
- Integrate ontology reasoning: subclass expansion, inverse property bidirectional traversal
- Add GRAPH SHORTEST PATH executor with MAX DEPTH support
- Add GRAPH TRAVERSE executor with direction/label/filter support

### Ontology Validation (New)

- Add `Ontology::validate()` with cycle detection for inheritance and equivalence
- Auto-validate on CREATE ONTOLOGY, reject invalid definitions
- Detect undefined superclass/equivalent/property references

### Reasoning Performance Optimization

- **BFS fast path** for transitive closure: 10x improvement (chain=50: 10.3ms → 1.0ms)
- **Superclass cache** for Cax-sco: 3.6x improvement (500 facts: 12.7ms → 3.5ms)
- **Parallel rule application** via `std::thread::scope` for independent rules
- **Merged ontology cache** to avoid repeated `scan_prefix("__ontology__")`
- **Selective cache invalidation** by ontology name instead of full clear

### R-tree KNN Optimization (3300x)

- Replace brute-force DFS with **priority queue best-first search**
- R-tree KNN: 81 ops/s → 298K ops/s

### SQL Compatibility

- **Recursive CTE** support with fixed-point iteration (max 100 iterations)
- Correct **RANK/DENSE_RANK** with ORDER BY value comparison (ties handled properly)

### EXPLAIN REASONING (New)

- New SQL syntax: `EXPLAIN REASONING <query>`
- Returns derivation chain: rule name + premises → conclusion
- Summary: original facts, inferred facts, iterations, per-rule counts
- Latency: ~22µs

### Live Data — 活数据 (New)

- **ValueMetadata**: decay formula `value_score × e^(-λ × Δt)`, computed at read time
- **Lambda presets**: `LAMBDA_7H` (7h), `LAMBDA_70D` (70d), `LAMBDA_2Y` (2y)
- **ValueScorer**: 4-rule engine (text content, field completeness, data size, class identifier)
- **LsmEngine API**: `get_value_meta`, `put_value_meta`, `activate`, `get_value_score`
- **SQL syntax**: `SYSTEM ACTIVATE 'Class::pk' 'reason'`
- **DBA views**: `system.data_temperature`, `system.value_events`, `system.value_decay_prediction`
- **Activate semantics**: uses `current_score() + delta` (not raw value_score)
- **Zero overhead**: `value_scorer_enabled=false` (default) → no meta keys written, no scorer called
- **Independent key scheme**: `__val_meta__::{class}::{pk}`, no changes to existing Entry structure

### Documentation

- OntoQL syntax reference manual (21 chapters): `docs/OntoQL语法参考手册.md`
- OntoQL core capability checklist updated: `docs/OntoQL核心能力Checklist.md`
- Live data implementation checklist updated: `docs/活数据最小闭环实施Checklist.md`

### Benchmarks

- Full system benchmark covering storage, core, graph, raft, vector, reasoning
- Reasoning benchmark examples: `bench_reasoning.rs`, `bench_system.rs`
- All **673 tests pass**, 0 regression

---

## [v0.6.1] - 2026-08-13

### Bug Fixes

- **B7.20**: Explicit saturating cast for SUM aggregation results to prevent overflow
- Resolve 5 release blocking issues for v0.6.1

### Release Readiness

- Version alignment across Cargo.toml, README, SDK, and docs
- Repository hygiene and config consistency
- Tests, English docs, and SDK version alignment
- Bump version to v0.6.1

---

## [v0.6.0] - 2026-08-12

### Security Hardening

- **Phase 7 Security Audit**: Resolve 23 security issues total
  - 6 critical/high issues (B7.4-B7.6, B7.12, B7.17, B7.35)
  - 17 medium/low issues (DoS guards, error sanitization, overflow protection)

### Code Quality

- Eliminate last 3 bare `unwrap()` from production code — now zero production unwrap
- All 430 `unwrap()` calls replaced with `expect()` or proper error handling

### Tests

- 584/584 passing, zero regressions

---

## [v0.5.9] - 2026-08-10

### Features

#### Adaptive Memory Management
- **MemoryManager** integrated into LsmEngine storage engine
- Dynamic MemTable sizing based on write rate (4MB - 256MB)
- Adaptive Block Cache sizing based on hit rate (16MB - 1GB)
- Memory pressure detection and automatic shrinking
- Write rate tracking with 10-interval sliding window
- Cache hit/miss tracking for adaptive sizing
- `with_initial_sizes()` constructor for backward compatibility

#### Performance
- Write: 863,618 ops/s
- Read: 1,256,518 ops/s
- Batch: 1,082,230 ops/s
- Vector recall: 100% at ef_search=200 (341µs latency)
- 8-thread speedup: 1.82x

#### Industry Analysis
- 9 industry scenarios analyzed (Algorithm/Storage/Computing/AI/Embodied/Automotive/Aerospace/Computing-Integrated/Industrial AI)
- Comprehensive competitive analysis vs AIDBS

### Tests
- 584/584 passing, zero regressions

## [v0.5.7] - 2026-08-10

### Security Audit & Hardening

#### Parser Hardening (Critical)
- **Replace all 80 direct string slicing operations** in `parser.rs` with safe `safe_slice()`/`safe_slice_from()` functions
- Add `safe_slice(s, start, end)` and `safe_slice_from(s, start)` helper functions that return empty string on out-of-bounds
- Fix `unwrap()` in CREATE INDEX parser (empty column list panic)
- Fix IMPORT/COPY parser bounds checking (malformed input panic)
- **Fuzz tests: 10/10 runs stable, zero panics**

#### Storage Hardening
- `disk.rs`: Add bounds check in `write_slot()` to prevent page overflow
- `geo.rs`: Add safety comments for bounded slicing operations
- `tsm.rs`: Verified existing bounds checks are correct

#### SPARQL Parser Fixes
- Fix CONSTRUCT parser byte range panic (empty/malformed template)
- Fix WHERE clause parser bounds checking (empty/malformed braces)

### Bug Fixes (18 total)
- **CRITICAL**: SQL BACKUP/RESTORE path traversal validation
- **CRITICAL**: SQL COPY/IMPORT arbitrary file read prevention
- **HIGH**: SPARQL SQL injection prevention (variable/class name sanitization)
- **HIGH**: Lock poisoning recovery (unwrap_or_else pattern)
- **HIGH**: Graph store lock ordering fixes (write→read for read-only ops)
- **HIGH**: Integer overflow protection (TSM/edge index saturating cast)
- **HIGH**: Compaction underflow guard
- **MEDIUM**: Input validation limits (max_depth, top_k, vector dimensions)
- **MEDIUM**: Error message sanitization (remove internal paths)
- **MEDIUM**: Sequence counter AcqRel ordering for snapshot isolation

### Performance
- Write: 808-908K ops/s (varies by run)
- Read: 1.17-1.37M ops/s
- Batch: 973K ops/s
- 8-thread speedup: 2.14x

### Tests
- 577+ tests passing, zero regressions
- 5 fuzz tests stable (SQL parser x3, SPARQL parser x2)

---

## [v0.5.6] - 2026-08-10

### Features
- **Observability module**: Slow query logging, alert rules, log aggregation
- **Cluster query router**: Read/write splitting, 4 routing strategies, replica lag monitoring
- **Cross-shard coordinator**: 7 aggregation types, partial failure handling

### Documentation
- Deployment guide (systemd, Docker, cluster, backup, monitoring)
- Best practices (data modeling, query optimization, vector/graph)
- FAQ (20+ common questions)
- API reference (all endpoints documented)
- SDK comparison table

### Frontend
- SQL query console with Ctrl+Enter execution
- Data browser with pagination
- Vector search UI with random vector generation
- Graph explorer with canvas visualization
- Schema browser with type info
- Metrics dashboard with charts and gauges
- Settings modal with theme switching
- Connection manager with version/uptime display

---

## [v0.5.5] - 2026-08-10

### Features
- **SDK**: Python, JavaScript/TypeScript, Go, Java (4 languages)
- **Grafana dashboard**: 11 monitoring panels
- **Prometheus config**: Ready-to-use scrape configuration

---

## [v0.5.0] - 2026-08-10

### Features
- **KMS integration**: HashiCorp Vault, AWS KMS, Azure Key Vault
- **Regex data masking**: Real regex crate + SHA-256 hash
- **LDAP authentication**: Bind auth, user search, group membership
- **Cluster management**: Node management, health monitoring, auto-failover
- **Data sharding**: Modulo, consistent hash, range-based

---

## [v0.4.0] - 2026-08-10

### Features
- **Enterprise security**: RBAC, encryption, audit retention
- **Patent disclosures**: 6 CNIPA-format patents
- **Digital twin**: 3D topology monitoring dashboard
- **Digital advisor**: Decision intelligence system

---

## [v0.3.0-stable] - 2026-08-10

### Features
- **Core engine**: LSM-Tree, B+Tree, HNSW vector index
- **Query engine**: SQL, SPARQL, graph traversal
- **Ontology**: OWL-Lite reasoning, 7 rules
- **HTTP API**: REST, PG Wire, MySQL Wire
- **TLS/HTTPS**: rustls, mTLS support

### Performance
- Write: 870K ops/s
- Read: 1.37M ops/s
- Batch: 1.1M ops/s
