# Changelog

All notable changes to OntoDB will be documented in this file.

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
