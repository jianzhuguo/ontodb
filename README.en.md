# OntoDB — Ontology-Driven Six-Modal Semantic Database

<p align="center">
  <b>The world's first database embedding an OWL reasoning engine in the storage kernel</b>
</p>

---

## Core Features

| Feature | Description |
|---------|-------------|
| **Six-Modal Unified Storage** | Relational + Graph + Vector + Time-Series + Spatial + Ontology, unified query API |
| **Semantic-on-Insert** | 7-step pipeline: document → graph vertex → rdf:type triple → property triple → OWL reasoning → vector index → B+Tree index |
| **Real-Time Ontology Reasoning** | 7 OWL 2 RL rules, incremental fixpoint algorithm, traceable inference chains |
| **Semantic-Vector Hybrid Query** | HNSW vector index + SQL/SPARQL joint queries |
| **Adaptive Memory Management** | MemTable 4-256MB dynamic sizing, Block Cache 16MB-1GB adaptive |
| **Group Commit** | put/commit_txn/put_batch full-path batched WAL sync |
| **Memory Safety** | Pure Rust, zero `unsafe` blocks, zero bare `unwrap()` in production |
| **PG/MySQL Compatible** | PostgreSQL Wire Protocol + MySQL Wire Protocol, connect with existing clients |

## Performance

| Metric | Value |
|--------|-------|
| Write throughput | 980,359 ops/s |
| Read throughput | 1,275,608 ops/s |
| Batch write | 1,353,140 rows/s |
| HNSW vector recall | 100% (ef_search=200, 341µs latency) |
| 8-thread speedup | 2.09x |
| GIS spatial predicate | ≤8µs |

## Quick Start

### Option 1: Download Pre-built Binary

```bash
# Linux x86_64
wget https://release.ontodb.io/ontodb-v0.6.0-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.0-linux-x86_64.tar.gz
cd ontodb-v0.6.0

# Start server
./ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

```powershell
# Windows x86_64
Invoke-WebRequest -Uri "https://release.ontodb.io/ontodb-v0.6.0-windows-x86_64.zip" -OutFile ontodb.zip
Expand-Archive ontodb.zip -DestinationPath .
cd ontodb-v0.6.0

# Start server
.\ontodb-server.exe --data-dir .\data --http 0.0.0.0:7912
```

### Option 2: Build from Source

```bash
# Prerequisites: Rust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# Binaries at target/release/
ls target/release/ontodb-server*
```

### Option 3: One-Line Install

```bash
# Linux/macOS
curl -fsSL https://get.ontodb.io | bash

# Windows (PowerShell)
irm https://get.ontodb.io/install.ps1 | iex
```

## Starting the Server

```bash
# Basic
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# With authentication
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --auth --api-key your-secret-key

# With TLS
./ontodb-server --data-dir ./data --http 0.0.0.0:7912 --tls-cert cert.pem --tls-key key.pem

# Raft cluster
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --raft --raft-id 1 --raft-peers "2@10.0.0.2:7913,3@10.0.0.3:7913"
```

### CLI Arguments

| Argument | Description | Default |
|----------|-------------|---------|
| `--data-dir` | Data directory | `./data` |
| `--http` | HTTP listen address | `127.0.0.1:7912` |
| `--auth` | Enable API Key auth | `false` |
| `--api-key` | API key | auto-generated |
| `--no-rate-limit` | Disable rate limiting | `false` |
| `--tls-cert` | TLS certificate file | none |
| `--tls-key` | TLS private key file | none |
| `--raft` | Enable Raft cluster | `false` |
| `--raft-id` | Node ID | `1` |
| `--raft-peers` | Cluster peer list | none |
| `--encryption-enabled` | Enable storage encryption | `false` |

## Access Methods

### HTTP REST API

```bash
# Health check
curl http://127.0.0.1:7912/api/health

# Execute SQL
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users LIMIT 10"}'

# Vector search
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{"class": "documents", "column": "embedding", "query_vector": [0.1, 0.2], "top_k": 5}'

# SPARQL query
curl -X POST http://127.0.0.1:7912/api/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT ?x WHERE { ?x rdf:type :Person }"}'
```

### Web Console

Open browser: `http://127.0.0.1:7912/console`

### PostgreSQL Client

```bash
psql -h 127.0.0.1 -p 7913 -U ontodb
```

### MySQL Client

```bash
mysql -h 127.0.0.1 -P 7914 -u ontodb
```

### CLI Tool

```bash
# Interactive REPL
./ontodb-cli 127.0.0.1:7912

# Single query
./ontodb-cli 127.0.0.1:7912 -q "SELECT * FROM users"

# Execute script
./ontodb-cli 127.0.0.1:7912 -f init.sql
```

## SQL Examples

### Basic CRUD

```sql
CREATE VERTEX TABLE users (name STRING, age INT, email STRING);
INSERT INTO users (name, age, email) VALUES ('Alice', 30, 'alice@example.com');
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;
UPDATE users SET age = 31 WHERE name = 'Alice';
DELETE FROM users WHERE name = 'Alice';
```

### Vector Search

```sql
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2, 0.3] TOP 10;
```

### Graph Query

```sql
CREATE VERTEX TABLE Person (name STRING);
CREATE EDGE TABLE knows (from_id STRING, to_id STRING, since INT);
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

### Ontology Reasoning

```sql
CREATE ONTOLOGY MyOntology (
  CLASS Animal,
  CLASS Dog SUBCLASS OF Animal,
  PROPERTY hasName DOMAIN Animal RANGE STRING
);
INSERT INTO Dog (hasName) VALUES ('Rex');
-- Dog automatically included in Animal queries via OWL reasoning
SELECT * FROM Animal;
```

## Enterprise Edition

| Feature | Community | Enterprise Standard | Gov/Finance |
|---------|-----------|--------------------| ------------|
| Core storage engine | Yes | Yes | Yes |
| SQL/SPARQL query | Yes | Yes | Yes |
| Vector index | Yes | Yes | Yes |
| Graph traversal | Yes | Yes | Yes |
| Ontology reasoning | Yes | Yes | Yes |
| HTTP API | Yes | Yes | Yes |
| PG/MySQL protocol | Yes | Yes | Yes |
| Full backup | Yes | Yes | Yes |
| Incremental backup | No | Yes | Yes |
| Raft cluster | No | Yes | Yes |
| Data sharding | No | Yes | Yes |
| SM4/AES encryption | No | No | Yes |
| RBAC separation | No | No | Yes |
| Audit logging | No | No | Yes |
| Data masking | No | No | Yes |

## Project Structure

```
ontodb/
├── crates/
│   ├── onto-core/          # Core types and error definitions
│   ├── onto-storage/       # Storage engine (LSM-Tree + B+Tree + HNSW + TSM)
│   ├── onto-query/         # Query engine (SQL parser + SPARQL + optimizer)
│   ├── onto-graph/         # Graph engine (adjacency list + BFS/DFS + shortest path)
│   ├── onto-ontology/      # Ontology engine (OWL reasoning + triple store + RDF)
│   ├── onto-server/        # HTTP/TCP/PG/MySQL server
│   ├── onto-enterprise/    # Enterprise features (encryption/RBAC/audit/backup)
│   ├── onto-raft/          # Raft consensus layer
│   ├── onto-cli/           # Command-line tool
│   └── onto-sharding/      # Sharding (in development)
├── sdk/                    # SDKs (Python, JavaScript, TypeScript, Go, Java)
├── frontend/               # React web console
├── docs/                   # Documentation
├── benches/                # Benchmarks
└── examples/               # Example applications (RAG, knowledge graph, multi-modal)
```

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/health` | Health check |
| GET | `/api/health/ready` | Readiness probe (K8s) |
| GET | `/api/health/live` | Liveness probe (K8s) |
| GET | `/api/metrics` | JSON metrics |
| GET | `/metrics` | Prometheus metrics |
| POST | `/api/query` | Execute SQL |
| POST | `/api/vector/search` | Vector similarity search |
| POST | `/api/hybrid/query` | Hybrid SQL + vector query |
| POST | `/api/sparql` | SPARQL query |
| GET | `/api/schema` | Schema introspection |
| POST | `/api/backup` | Full backup |
| POST | `/api/backup/incremental` | Incremental backup |
| POST | `/api/restore` | Restore backup |
| GET | `/api/cluster` | Cluster status |
| GET | `/api/docs` | Swagger UI |
| GET | `/console` | Web console |

## License

- **Community Edition**: Apache License 2.0
- **Enterprise Edition**: Commercial license (see `crates/onto-enterprise/COMMERCIAL_LICENSE.md`)

## Contact

- Website: https://ontodb.io
- Docs: https://docs.ontodb.io
- GitHub: https://github.com/ontodb/ontodb
- Email: contact@ontodb.io
- Security: security@ontodb.io
