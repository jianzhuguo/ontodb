# OntoDB

An ontology-driven, semantic multi-modal database built in Rust.

OntoDB embeds OWL-lite ontology reasoning directly into the database kernel — data is not just stored, it is **understood**. It combines relational storage, vector search, and semantic querying in a single engine, eliminating the need to glue together separate databases for each workload.

## Key Features

- **Ontology-Native Storage** — Define classes, properties, inheritance, and constraints with `CREATE ONTOLOGY`. The reasoner automatically propagates inferences (subclass, equivalent class, subproperty, inverse, transitive, symmetric) via 7 OWL 2 RL rules.
- **Full SQL Engine** — Hand-written recursive-descent parser supporting SELECT, INSERT, UPDATE, DELETE, JOINs (INNER/LEFT/CROSS), subqueries, CTEs, window functions, GROUP BY, HAVING, ORDER BY, LIMIT/OFFSET, UNION, UPSERT, and more.
- **SPARQL Endpoint** — W3C-compliant SPARQL 1.1 with SELECT, CONSTRUCT, ASK, FILTER, OPTIONAL, UNION. Translates to the internal SQL engine for execution.
- **Vector Search** — Built-in HNSW index with L2, Cosine, and InnerProduct metrics. Hybrid SQL + vector queries with ontology-aware filtered search.
- **LSM-Tree Storage Engine** — WAL + MemTable + SSTable with leveled compaction, zstd compression, bloom filters, block cache, and MVCC with snapshot isolation.
- **B+Tree Indexes** — Both in-memory and disk-based (4KB pages, buffer pool with LRU eviction) secondary indexes.
- **BinaryRow Format** — Compact binary row encoding for 4.5x faster field lookup and 5.5x faster filter evaluation vs JSON parsing.
- **Production-Ready Server** — HTTP API (axum) + TCP server with API key authentication, token bucket rate limiting, Prometheus metrics, and Kubernetes health probes.
- **Zero `unsafe`** — The entire codebase contains no unsafe Rust blocks.

## Quick Start

### Build from Source

```bash
# Prerequisites: Rust 1.70+ (install via https://rustup.rs)
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release
```

### Run the Server

```bash
# Start with TCP server (default: localhost:7913)
./target/release/ontodb-server --data-dir ./mydata

# Start with HTTP API
./target/release/ontodb-server --data-dir ./mydata --http 0.0.0.0:7912

# Enable authentication
./target/release/ontodb-server --data-dir ./mydata --http 0.0.0.0:7912 \
  --auth --api-keys-file config/api_keys.example.json
```

### Connect with CLI

```bash
# Interactive REPL
./target/release/ontodb-cli

# Single query
./target/release/ontodb-cli -q "SELECT * FROM Product WHERE price > 100"

# Execute SQL file
./target/release/ontodb-cli -f init.sql
```

### Docker

```bash
docker build -t ontodb .
docker run -p 7912:7912 -v ontodb-data:/data ontodb
```

## Usage Examples

### Create an Ontology

```sql
CREATE ONTOLOGY shop (
  CLASS Product,
  CLASS ElectronicProduct SUBCLASSOF Product,
  PROPERTY name DOMAIN Product RANGE STRING,
  PROPERTY price DOMAIN Product RANGE FLOAT,
  PROPERTY category DOMAIN Product RANGE STRING
);
```

### Insert and Query Data

```sql
INSERT INTO Product (name, price, category) VALUES ('Laptop', 999.99, 'electronics');
INSERT INTO Product (name, price, category) VALUES ('Book', 29.99, 'education');

-- Standard SQL query
SELECT name, price FROM Product WHERE price > 50 ORDER BY price DESC;

-- Semantic query (leverages ontology inheritance)
MATCH (p: Product) WHERE p.price > 50 RETURN p.name, p.price;
```

### Vector Search

```sql
CREATE VECTOR INDEX product_embeddings ON Product (embedding)
  WITH dimensions=768, metric=cosine;

VECTOR SEARCH ON product_embeddings
  USING QUERY_VECTOR([0.1, 0.2, ...])
  TOP 10;
```

### HTTP API

```bash
# Execute a query
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product WHERE price > 50"}'

# SPARQL query
curl -X POST http://localhost:7912/sparql \
  -H "Content-Type: application/sparql-query" \
  -d 'SELECT ?name WHERE { ?product <http://shop/name> ?name }'

# Health check
curl http://localhost:7912/api/health

# Prometheus metrics
curl http://localhost:7912/metrics
```

## Architecture

```
┌──────────────┐   ┌──────────────┐
│   HTTP API   │   │   TCP CLI    │
│   (axum)     │   │   Protocol   │
└──────┬───────┘   └──────┬───────┘
       │                  │
       └────────┬─────────┘
                │
       ┌────────▼─────────┐
       │  Query Executor   │  SQL / SPARQL / MATCH / VECTOR SEARCH
       │  (Cost Optimizer) │
       └────────┬─────────┘
                │
       ┌────────▼─────────┐
       │  Ontology Engine  │  OWL-lite reasoning (7 rules)
       │  (Reasoner)       │  RDF import/export
       └────────┬─────────┘
                │
       ┌────────▼─────────┐
       │  Storage Engine   │  LSM-Tree + WAL + MVCC
       │                   │  B+Tree indexes, HNSW vectors
       │                   │  zstd compression, bloom filters
       └──────────────────┘
```

### Crate Layout

| Crate | Purpose |
|-------|---------|
| `onto-core` | Core types, values, errors, BinaryRow format |
| `onto-storage` | LSM-Tree engine, WAL, SSTables, MVCC, B+Tree, HNSW |
| `onto-ontology` | OWL-lite model, reasoner, RDF import/export |
| `onto-query` | SQL/SPARQL parser, executor, cost optimizer, cache |
| `onto-raft` | Raft consensus layer (work in progress) |
| `onto-sharding` | Data sharding strategies (work in progress) |
| `onto-server` | HTTP + TCP server, auth, rate limiting, metrics |
| `onto-cli` | Interactive command-line client |

## Configuration

### Server Options

| Flag | Default | Description |
|------|---------|-------------|
| `--data-dir` | `./ontodb_data` | Data directory path |
| `--memtable-size` | `4194304` (4MB) | MemTable size limit in bytes |
| `--listen` | `127.0.0.1:7913` | TCP listen address |
| `--http` | _(disabled)_ | HTTP listen address (enables HTTP API) |
| `--auth` | `false` | Enable API key authentication |
| `--api-keys-file` | _(none)_ | Path to API keys JSON file |
| `--rate-limit` | `60` | Default requests per minute |
| `--burst-size` | `10` | Rate limit burst size |
| `--no-rate-limit` | `false` | Disable rate limiting |
| `-i` / `--interactive` | `false` | Run in REPL mode |

### API Key Configuration

Create a JSON file (see `config/api_keys.example.json`):

```json
{
  "enabled": true,
  "keys": [
    {
      "key": "your-secret-key",
      "description": "Admin access",
      "permission": "Admin",
      "rate_limit": 120
    }
  ],
  "default_permission": "ReadOnly"
}
```

Permissions: `ReadOnly`, `ReadWrite`, `Admin`.

## API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/health` | Health check |
| GET | `/api/health/ready` | Kubernetes readiness probe |
| GET | `/api/health/live` | Kubernetes liveness probe |
| GET | `/metrics` | Prometheus metrics |
| GET | `/api/metrics` | JSON metrics |
| POST | `/api/query` | Execute SQL/OntoDB query |
| POST | `/api/vector/search` | Vector similarity search |
| POST | `/api/hybrid/query` | Hybrid SQL + vector search |
| GET | `/api/schema` | Schema introspection |
| POST | `/sparql` | SPARQL query endpoint |

## Testing

```bash
# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p onto-storage
cargo test -p onto-query

# Run with output
cargo test -- --nocapture
```

## Project Status

**Version:** 0.1.0 — Active development

| Component | Status |
|-----------|--------|
| Storage engine (LSM-Tree, MVCC, indexes) | Stable |
| SQL parser and executor | Stable |
| Ontology engine and reasoner | Stable |
| Vector search (HNSW) | Stable |
| HTTP API server | Stable |
| CLI client | Stable |
| Raft consensus (openraft, TCP networking, config sync) | Stable |
| Cluster whitelist management | Stable |
| Data sharding | Scaffold (not integrated) |

## License

Apache License 2.0

## Contributing

Contributions are welcome. Please open an issue first to discuss what you would like to change.
