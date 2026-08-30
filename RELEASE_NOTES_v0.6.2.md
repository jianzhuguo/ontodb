# OntoDB v0.6.2 Release Notes

## What's New

### Open Source Release
- Full open source preparation with Apache 2.0 license
- 28-page documentation site (mdBook) with ~95% feature coverage
- CI/CD pipeline with multi-platform builds (Linux, Windows, macOS)
- Community edition build support (`cargo build --no-default-features`)
- Edge crates (onto-edge, onto-edge-bin, onto-edge-esp32) added to workspace

### New Features
- **Batch Query API** — `POST /api/batch` for executing multiple queries in a single request
- **Transaction API** — `POST /api/transaction/{begin,execute,commit,rollback}` for HTTP transactions
- **Cursor Pagination** — `POST /api/cursor` for efficient pagination of large result sets
- **Export/Import API** — `POST /api/export` and `POST /api/import` for JSONL/CSV data transfer
- **SQLAlchemy Dialect** — Python SQLAlchemy support with `ontodb://` protocol
- **WAL Archiving** — Automatic WAL archiving for incremental backups
- **UNIQUE Constraints** — `UNIQUE(col1, col2)` syntax in CREATE ONTOLOGY
- **CLI Backup Tool** — `ontodb-cli dump/restore` for logical backup

### OntoQL
- OntoQL parser with class inheritance, ontology operations, triple operations
- 14 parser bug fixes
- Subclass propagation optimization (3.5x improvement on 500 facts)

### Bug Fixes
- Fix `drop_ontology` tombstone bug (LSM get vs scan_prefix inconsistency)
- Fix non-deterministic graph edges in Digital Twin visualization
- Fix scan_prefix HashMap iteration ordering
- WAL fsync on engine drop for crash recovery
- Background WAL fsync for single put() durability
- 17 security fixes (DoS guards, error sanitization, overflow protection)

## Performance

| Metric | Value |
|--------|-------|
| Write throughput | 863,618 ops/s |
| Read throughput | 1,256,518 ops/s |
| Batch write | 1,082,230 ops/s |
| HNSW vector recall | 100% (ef_search=200, 341µs) |
| Subclass propagation | 3.5x faster |

## Breaking Changes

None.

## Installation

### Build from Source
```bash
git clone https://github.com/ontodb/ontodb.git
cd ontodb
git checkout v0.6.2
cargo build --release
```

### Docker
```bash
docker build -t ontodb:v0.6.2 .
docker run -p 7912:7912 ontodb:v0.6.2
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for full details.
