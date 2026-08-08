# Contributing to OntoDB

Thank you for your interest in contributing!

## Development Environment

### Prerequisites

- Rust 1.75+ (stable)
- Cargo

### Setup

```bash
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build
cargo test
```

### Running the Server

```bash
cargo run -p onto-server
```

The server listens on `127.0.0.1:8088` (HTTP) and `127.0.0.1:8089` (TCP) by default.

## Code Structure

```
crates/
  onto-core/       Core types, errors, BinaryRow
  onto-storage/    LSM-Tree engine, WAL, SSTable, MVCC
  onto-ontology/   OWL-lite model, reasoner, RDF import/export
  onto-query/      SQL parser, executor, SPARQL, optimizer
  onto-graph/      Property graph model, BFS/DFS traversal
  onto-raft/       Raft consensus (WIP)
  onto-sharding/   Data sharding (WIP)
  onto-server/     HTTP + TCP server
  onto-cli/        CLI client
sdk/python/        Python SDK
```

## Commit Convention

Format: `type: description`

Types:
- `feat` — new feature
- `fix` — bug fix
- `perf` — performance improvement
- `refactor` — code restructuring without behavior change
- `docs` — documentation
- `test` — test additions or fixes
- `chore` — build/CI/tooling changes

Examples:
```
feat: add graph traversal API
fix: WAL atomicity on process kill
perf: MemTable get() O(n) → O(log n)
```

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `main`
3. Write tests for new functionality
4. Ensure `cargo test` passes with no warnings
5. Ensure `cargo clippy` passes
6. Open a PR with a clear description of changes

## Testing

```bash
# Run all tests
cargo test

# Run specific crate tests
cargo test -p onto-query

# Run with output
cargo test -- --nocapture

# Benchmarks
cargo bench
```

## License

By contributing, you agree that your contributions will be licensed under the [Apache-2.0 License](LICENSE).
