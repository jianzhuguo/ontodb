# OntoDB 鈥?Ontology-Driven Six-Modal Semantic Database

<p align="center">
  <b>The world's first database embedding an OWL reasoning engine in the storage kernel</b>
</p>

---

## Core Features

| Feature | Description |
|---------|-------------|
| **Six-Modal Unified Storage** | Relational + Graph + Vector + Time-Series + Spatial + Ontology, unified query API |
| **Semantic-on-Insert** | 7-step pipeline: document 鈫?graph vertex 鈫?rdf:type triple 鈫?property triple 鈫?OWL reasoning 鈫?vector index 鈫?B+Tree index |
| **Real-Time Ontology Reasoning** | 7 OWL 2 RL rules, incremental fixpoint algorithm, traceable inference chains |
| **Semantic-Vector Hybrid Query** | HNSW vector index + SQL/SPARQL joint queries |
| **Adaptive Memory Management** | MemTable 4-256MB dynamic sizing, Block Cache 16MB-1GB adaptive |
| **Group Commit** | put/commit_txn/put_batch full-path batched WAL sync |
| **Memory Safety** | Pure Rust, zero `unsafe` blocks |
| **PG/MySQL Compatible** | PostgreSQL Wire Protocol + MySQL Wire Protocol |

## Performance

| Metric | Value |
|--------|-------|
| Write throughput | 863,618 ops/s |
| Read throughput | 1,256,518 ops/s |
| Batch write | 1,082,230 ops/s |
| HNSW vector recall | 100% (ef_search=200, 341碌s latency) |
| 8-thread speedup | 1.82x |
| GIS spatial predicate | 鈮?碌s |

## Quick Start

### Option 1: Build from Source

```bash
# Prerequisites: Rust 1.70+
git clone https://gitee.com/ontovalue/ontodb.git
cd ontodb
cargo build --release

# Start server
./target/release/ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

### Option 2: Docker

```bash
docker run -p 7912:7912 ontodb/ontodb-server --data-dir /data --http 0.0.0.0:7912
```

## Usage

### HTTP REST API

```bash
# Health check
curl http://127.0.0.1:7912/api/health

# Execute OntoQL
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users LIMIT 10"}'

# Vector search
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{"class": "documents", "column": "embedding", "query_vector": [0.1, 0.2], "top_k": 5}'
```

### Web Console

Open browser: `http://127.0.0.1:7912`

### SDKs

| Language | Directory |
|----------|-----------|
| Python | `sdk/python/` |
| JavaScript/TypeScript | `sdk/javascript/` / `sdk/typescript/` |
| Go | `sdk/go/` |
| Java | `sdk/java/` |

## SQL Examples

```sql
-- Create table
CREATE VERTEX TABLE users (name STRING, age INT, email STRING);

-- Insert
INSERT INTO users (name, age, email) VALUES ('Alice', 30, 'alice@example.com');

-- Query
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;
```

### Vector Search

```sql
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2, 0.3] TOP 10;
```

### Graph Query

```sql
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

OntoDB uses a dual licensing model:

| Edition | License | Description |
|---------|---------|-------------|
| **Community** | AGPL-3.0 | Free to use, modifications must be open source |
| **Enterprise** | Commercial | Production use, no open source requirement |

**Community Edition (AGPL-3.0)**:
- Free to use, modify, and distribute
- If providing network services, modified code must be open source
- See [LICENSE](LICENSE)

**Enterprise Edition (Commercial)**:
- Production environment use
- No requirement to open source modifications
- Includes technical support and SLA
- See [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- Contact: license@ontovalue.com
