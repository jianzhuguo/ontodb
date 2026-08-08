# Changelog

All notable changes to OntoDB will be documented in this file.

Format based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

## [0.1.0-alpha] — 2026-08-08

Initial alpha release. 133 commits covering core database engine, query layer, ontology reasoning, and distributed infrastructure.

### Features

#### Storage Engine
- LSM-Tree storage engine with WAL, SSTable, MemTable
- MVCC transaction system with snapshot isolation
- Leveled compaction with size-based scoring and tombstone cleanup
- SSTable bloom filter for fast negative lookups
- Block cache with LRU eviction
- BufferPool with O(1) touch operations
- WAL batch sync for write throughput
- B+Tree disk index with insert, lookup, delete, and rebalance
- BinaryRow compact binary format (u16 field count + type tags + offset index)
- CSV/JSON batch import (`IMPORT INTO` syntax with auto type detection)
- Backup and restore (snapshot-based backup with full recovery)
- WAL disk index integration (fsync + corruption detection + auto-rebuild)

#### Query Layer
- SQL parser with full DDL/DML support
- Query optimizer with planner and cost model
- Plan-driven execution engine
- Plan cache integration with index condition pushdown
- Hash join, sort-merge join, nested-loop join
- Join reordering and index selection
- GROUP BY, ORDER BY (multi-column), LIMIT OFFSET
- Aggregate functions (COUNT, SUM, AVG, MIN, MAX)
- CASE WHEN, CTE (recursive), window functions
- Materialized views with incremental refresh
- EXPLAIN and EXPLAIN ANALYZE
- DISTINCT, LIKE, BETWEEN, IN operators
- UNION, subqueries, EXISTS subquery
- Batch INSERT, INSERT SELECT, UPSERT
- Built-in functions
- Query timeout and memory budget
- Multi-statement transactions (BEGIN/COMMIT/ROLLBACK)
- SPARQL endpoint with graph pattern support (DISTINCT/OPTIONAL/UNION/EXISTS)
- Semantic query optimization using ontology constraints

#### Ontology & Reasoning
- OWL-lite model with class/property/restriction support
- Reasoning engine with 7 inference rules (subclass propagation, property inference)
- Reasoner integration into executor scan and filter stages
- Inference result materialization cache
- OWL Restriction write validation
- RDF import/export
- Schema introspection

#### Graph
- Property graph model with CRUD operations
- BFS/DFS traversal with path reconstruction
- Graph-enhanced vector recall (6.67% improvement)
- Graph query language in SQL parser
- Graph HTTP API endpoints with persistence

#### Vector Search
- HNSW vector index implementation (100% recall rate)
- HNSW integration with storage engine and query layer
- Vector search with filter support

#### HTTP API & Server
- HTTP API with authentication (API key based)
- Rate limiting (configurable, 300 req/min default)
- Prometheus metrics endpoint (storage metrics real-time sync)
- Health check endpoints
- TCP server (async with connection tracking)

#### CLI & SDK
- Interactive CLI with REPL, multi-line input, formatted table output
- Python SDK with HTTP API bindings
- Python SDK graph operations (traverse, BFS, path reconstruction)

#### Deployment
- Docker and docker-compose configuration
- Kubernetes deployment manifests
- Nginx TLS proxy configuration
- Production deployment guide

### Performance

- COUNT fast path (skip deserialization) + MemTable range query
- BinaryRow parse+to_map 1.30x speedup, field lookup 4.59x, filter eval 5.45x
- WAL serialize buffer reuse + MemTable composite key zero-alloc
- Batch WAL flush (every 64 writes instead of per-write)
- BinaryRow-aware index scan filtering
- GROUP BY / ORDER BY direct Value extraction (avoid string conversion)
- SST iterator zero-copy + flush_memtable zero-clone
- SST cache precise eviction, get() lock merge, find_field O(1)
- WAL batch sync + HNSW batch insert (274K writes/sec, 1.4M reads/sec)
- flush_memtable snapshot under brief lock
- commit_txn 3-phase lock separation (8R+1W 4.5s → 4.0s)
- scan_prefix/get SST iteration outside write_state lock (2.1x speedup)
- LsmEngine interior mutability (unblock read from write)
- MemTable get() O(n) → O(log n)
- BufferPool touch() O(n) → O(1)
- SSTable handle caching (avoid reopen on every read)
- SSTable bloom filter + level parsing fix

### Refactoring

- B+Tree nodes Vec → HashMap O(1) lookup + parent pointers
- Compaction scheduling: size-based scoring + smart L0 + tombstone cleanup
- Deduplicate scan_prefix, remove dead code
- commit_txn 3-phase lock separation
- LsmEngine interior mutability
- Query Phase 25: unified plan-driven execution, remove legacy path

### Documentation

- OntoDB feasibility analysis reports (v1.6 → v1.23)
- Performance benchmark report (before/after optimization, multi-scale data)
- HTTP API documentation (SPARQL, Schema Introspection, Ontology Reasoning)
- Deployment guide (Docker, Kubernetes, production)

### Security

- `.gitignore` updated: `.env`, `config/api_keys.json`, TLS keys excluded
- `SECURITY.md` vulnerability reporting policy
- API key authentication with rate limiting
- Security defaults: bind to 127.0.0.1, TLS proxy ready
