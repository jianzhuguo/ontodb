# Quick Start

Get OntoDB running in 5 minutes.

## Option 1: Docker (recommended)

```bash
docker run -d \
  --name ontodb \
  -p 7912:7912 \
  -p 7913:7913 \
  -v ontodb_data:/data \
  ontodb/ontodb:latest
```

## Option 2: Build from source

```bash
git clone https://gitee.com/ontovalue/ontodb.git
cd ontodb
cargo build --release
./target/release/ontodb-server --http 127.0.0.1:7912 --data-dir ./data
```

## Verify it's running

```bash
curl http://localhost:7912/api/health
```

Expected response:
```json
{
  "status": "ok",
  "version": "0.6.2",
  "engine": "OntoDB",
  "uptime_seconds": 5,
  "checks": {
    "storage": "ok",
    "query_engine": "ok"
  }
}
```

## First query

```bash
# Create a class (table)
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS Product (id STRING, name STRING, price FLOAT)"}'

# Insert data
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Product SET id = \"1\", name = \"Widget\", price = 9.99"}'

# Query
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product"}'
```

## API Documentation

Once the server is running, visit **http://localhost:7912/api/docs** for interactive Swagger UI documentation.

## Next steps

- [Concepts](./concepts.md) — Understand OntoDB's multi-modal model
- [SQL Reference](./api/sql.md) — Full SQL syntax
- [Vector Search](./concepts/vector.md) — Semantic similarity search
- [Graph Queries](./concepts/graph.md) — Property graph operations
- [SDKs](./sdks.md) — Python and TypeScript client libraries
