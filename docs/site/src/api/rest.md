# REST API Reference

Full interactive documentation: **`/api/docs`** (Swagger UI)

## Base URL

```
http://localhost:7912
```

## Authentication

Protected endpoints require `X-API-Key` header:

```bash
curl -H "X-API-Key: your-key" http://localhost:7912/api/query
```

---

## Query

### Execute Query

```bash
POST /api/query
```

Execute SQL, OntoQL, or SPARQL queries.

```bash
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{"query": "SELECT * FROM users WHERE age > 25 LIMIT 10"}'
```

Response:
```json
{
  "success": true,
  "data": [{"name": "Alice", "age": 30}],
  "elapsed_ms": 1.23
}
```

### Batch Query

```bash
POST /api/batch
```

Execute multiple queries in a single request (up to 1000). Reduces network round trips.

```bash
curl -X POST http://localhost:7912/api/batch \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "queries": [
      {"query": "SELECT COUNT(*) FROM users"},
      {"query": "SELECT * FROM products LIMIT 5"},
      {"query": "SELECT * FROM orders WHERE status = \"pending\""}
    ]
  }'
```

Response:
```json
{
  "results": [
    {"success": true, "data": [{"count(*)": 1000}]},
    {"success": true, "data": [...]},
    {"success": true, "data": [...]}
  ],
  "count": 3
}
```

### Schema Introspection

```bash
GET /api/schema
```

Returns all ontologies, classes, properties, and indexes.

---

## Transactions

Multi-statement transactions with snapshot isolation.

### Begin Transaction

```bash
POST /api/transaction/begin
```

```bash
curl -X POST http://localhost:7912/api/transaction/begin \
  -H "X-API-Key: your-key"
```

Response:
```json
{"success": true, "data": {"txn_id": "txn_abc123"}}
```

### Execute in Transaction

```bash
POST /api/transaction/execute
```

```bash
curl -X POST http://localhost:7912/api/transaction/execute \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "txn_id": "txn_abc123",
    "query": "INSERT INTO accounts (id, balance) VALUES (\"acc1\", 1000)"
  }'
```

### Commit Transaction

```bash
POST /api/transaction/commit
```

```bash
curl -X POST http://localhost:7912/api/transaction/commit \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{"txn_id": "txn_abc123"}'
```

### Rollback Transaction

```bash
POST /api/transaction/rollback
```

```bash
curl -X POST http://localhost:7912/api/transaction/rollback \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{"txn_id": "txn_abc123"}'
```

---

## Vector Search

### Vector Similarity Search

```bash
POST /api/vector/search
```

```bash
curl -X POST http://localhost:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "class": "documents",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, ...],
    "top_k": 10,
    "filter": "category = \"tech\""
  }'
```

### Hybrid SQL + Vector Query

```bash
POST /api/hybrid/query
```

Combine SQL filters with vector similarity ranking.

```bash
curl -X POST http://localhost:7912/api/hybrid/query \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "sql": "SELECT * FROM products WHERE category = \"electronics\"",
    "vector_class": "products",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, ...],
    "top_k": 5
  }'
```

---

## Graph

### Add Vertex

```bash
POST /api/graph/vertex
```

```bash
curl -X POST http://localhost:7912/api/graph/vertex \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "id": "person_1",
    "labels": ["Person"],
    "properties": {"name": "Alice", "age": 30}
  }'
```

### Get Vertex

```bash
GET /api/graph/vertex/:id
```

### Delete Vertex

```bash
DELETE /api/graph/vertex/:id
```

### Add Edge

```bash
POST /api/graph/edge
```

```bash
curl -X POST http://localhost:7912/api/graph/edge \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "from": "person_1",
    "to": "person_2",
    "label": "knows",
    "properties": {"since": 2020}
  }'
```

### Get Neighbors

```bash
GET /api/graph/neighbors/:id?direction=out&label=knows&depth=2
```

Parameters:
- `direction` — `out`, `in`, or `both` (default: `both`)
- `label` — filter by edge label
- `depth` — max hops (default: 1)

### Graph Traversal

```bash
POST /api/graph/traverse
```

```bash
curl -X POST http://localhost:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "start_id": "person_1",
    "direction": "out",
    "edge_label": "knows",
    "max_depth": 3,
    "algorithm": "bfs",
    "filter": {"property": "age", "op": "gt", "value": 25}
  }'
```

Parameters:
- `algorithm` — `bfs` or `dfs`
- `direction` — `out`, `in`, `both`
- `filter` — optional property filter (`eq`, `neq`, `gt`, `lt`, `gte`, `lte`, `contains`)

### Shortest Path

```bash
POST /api/graph/shortest-path
```

```bash
curl -X POST http://localhost:7912/api/graph/shortest-path \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "from": "person_1",
    "to": "person_5",
    "edge_label": "knows"
  }'
```

---

## Pagination

### Cursor-Based Pagination

```bash
POST /api/cursor
```

Efficient pagination for large result sets.

```bash
# First page
curl -X POST http://localhost:7912/api/cursor \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "query": "SELECT * FROM logs ORDER BY timestamp DESC",
    "page_size": 100
  }'
```

Response:
```json
{
  "data": [...],
  "cursor": "eyJ0IjoxNjkw...",
  "has_more": true
}
```

```bash
# Next page
curl -X POST http://localhost:7912/api/cursor \
  -H "Content-Type: application/json" \
  -d '{
    "query": "SELECT * FROM logs ORDER BY timestamp DESC",
    "page_size": 100,
    "cursor": "eyJ0IjoxNjkw..."
  }'
```

---

## Data Import/Export

### Export Data

```bash
POST /api/export
```

Export table data as JSONL or CSV.

```bash
curl -X POST http://localhost:7912/api/export \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "query": "SELECT * FROM users WHERE active = true",
    "format": "jsonl"
  }'
```

Formats: `jsonl`, `csv`

### Import Data

```bash
POST /api/import
```

Import data from JSONL.

```bash
curl -X POST http://localhost:7912/api/import \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{
    "class": "users",
    "data": [
      {"id": "u1", "name": "Alice", "age": 30},
      {"id": "u2", "name": "Bob", "age": 25}
    ]
  }'
```

---

## Backup & Operations

### Full Backup

```bash
POST /api/backup
```

```bash
curl -X POST http://localhost:7912/api/backup \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-key" \
  -d '{"path": "/backups/2026-08-30"}'
```

### Incremental Backup

```bash
POST /api/backup/incremental
```

Only backs up files modified since the last backup.

### Verify Backup

```bash
POST /api/backup/verify
```

Checks backup integrity (file existence + checksums).

### Flush MemTable

```bash
POST /api/flush
```

Force flush MemTable to disk.

---

## Monitoring

### Prometheus Metrics

```bash
GET /metrics
```

Prometheus exposition format. Includes:
- `ontodb_queries_total` — Total queries
- `ontodb_query_duration_seconds` — Query latency histogram
- `ontodb_storage_entries` — Total stored entries
- `ontodb_cache_hit_rate` — Cache hit rate

### JSON Metrics

```bash
GET /api/metrics
```

JSON format with detailed stats:
- Query counts (total, vector, graph)
- Connection stats
- Auth stats
- Storage stats (MemTable, SSTables, cache)
- Slow query count

---

## Health

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/health` | Comprehensive health check |
| GET | `/api/health/ready` | Kubernetes readiness probe |
| GET | `/api/health/live` | Kubernetes liveness probe |

```json
{
  "status": "ok",
  "engine": "OntoDB",
  "version": "0.6.2",
  "uptime_seconds": 3600,
  "checks": {
    "storage": "ok",
    "query_engine": "ok"
  }
}
```

---

## Documentation

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/docs` | Swagger UI |
| GET | `/api/openapi.json` | OpenAPI 3.0 spec |
| GET | `/console` | Web console |
| GET | `/digital-twin` | Digital Twin visualization |

---

## Error Responses

```json
{
  "success": false,
  "error": "description of what went wrong"
}
```

| Status | Meaning |
|--------|---------|
| 200 | Success |
| 400 | Bad request (parse error, validation error) |
| 401 | Unauthorized (missing or invalid API key) |
| 404 | Not found |
| 409 | Conflict (duplicate key) |
| 429 | Rate limited |
| 500 | Internal server error |
| 503 | Service unavailable |

---

## Rate Limiting

Default: 60 requests/minute per API key, burst size 10.

Disable with `--no-rate-limit` flag.

Configure:
```bash
./ontodb-server --rate-limit-rpm 1000 --rate-limit-burst 50
```

---

## All Endpoints Summary

| Category | Method | Path | Auth |
|----------|--------|------|------|
| **Query** | POST | `/api/query` | Yes |
| | POST | `/api/batch` | Yes |
| | GET | `/api/schema` | Yes |
| **Transaction** | POST | `/api/transaction/begin` | Yes |
| | POST | `/api/transaction/execute` | Yes |
| | POST | `/api/transaction/commit` | Yes |
| | POST | `/api/transaction/rollback` | Yes |
| **Vector** | POST | `/api/vector/search` | Yes |
| | POST | `/api/hybrid/query` | Yes |
| **Graph** | POST | `/api/graph/vertex` | Yes |
| | GET | `/api/graph/vertex/:id` | Yes |
| | DELETE | `/api/graph/vertex/:id` | Yes |
| | POST | `/api/graph/edge` | Yes |
| | GET | `/api/graph/neighbors/:id` | Yes |
| | POST | `/api/graph/traverse` | Yes |
| | POST | `/api/graph/shortest-path` | Yes |
| **SPARQL** | POST | `/sparql` | Yes |
| **Pagination** | POST | `/api/cursor` | Yes |
| **Import/Export** | POST | `/api/export` | Yes |
| | POST | `/api/import` | Yes |
| **Backup** | POST | `/api/backup` | Admin |
| | POST | `/api/backup/incremental` | Admin |
| | POST | `/api/backup/verify` | Admin |
| | POST | `/api/flush` | Admin |
| **Monitoring** | GET | `/metrics` | No |
| | GET | `/api/metrics` | No |
| **Health** | GET | `/api/health` | No |
| | GET | `/api/health/ready` | No |
| | GET | `/api/health/live` | No |
| **Docs** | GET | `/api/docs` | No |
| | GET | `/api/openapi.json` | No |
| | GET | `/console` | No |
| | GET | `/digital-twin` | No |
| | GET | `/digital-advisor` | No |
