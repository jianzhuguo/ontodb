# REST API Reference

Full interactive documentation is available at **`/api/docs`** (Swagger UI) when the server is running.

## Base URL

```
http://localhost:7912
```

## Authentication

Protected endpoints require an API key in the `X-API-Key` header:

```bash
curl -H "X-API-Key: your-key" http://localhost:7912/api/query
```

Health check and metrics endpoints do not require authentication.

## Endpoints

### Health

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/api/health` | No | Comprehensive health check |
| GET | `/api/health/ready` | No | Kubernetes readiness probe |
| GET | `/api/health/live` | No | Kubernetes liveness probe |

### Query

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/query` | Yes | Execute SQL query |
| POST | `/sparql` | Yes | Execute SPARQL query |
| GET | `/api/schema` | Yes | Schema introspection |

### Vector Search

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/vector/search` | Yes | Vector similarity search |
| POST | `/api/hybrid/query` | Yes | Hybrid SQL + vector search |

### Graph

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/graph/vertex` | Yes | Add vertex |
| GET | `/api/graph/vertex/:id` | Yes | Get vertex |
| DELETE | `/api/graph/vertex/:id` | Yes | Delete vertex |
| POST | `/api/graph/edge` | Yes | Add edge |
| GET | `/api/graph/neighbors/:id` | Yes | Get neighbors |
| POST | `/api/graph/traverse` | Yes | BFS/DFS traversal |
| POST | `/api/graph/shortest-path` | Yes | Shortest path |

### Admin

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/backup` | Admin | Full backup |
| POST | `/api/backup/incremental` | Admin | Incremental backup |
| POST | `/api/backup/verify` | Admin | Verify backup integrity |
| POST | `/api/flush` | Admin | Flush MemTable |

### Monitoring

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/metrics` | No | Prometheus metrics |
| GET | `/api/metrics` | No | JSON metrics |

### Documentation

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/api/docs` | No | Swagger UI |
| GET | `/api/openapi.json` | No | OpenAPI 3.0 spec |

## Error responses

All errors follow the same format:

```json
{
  "success": false,
  "error": "description of what went wrong"
}
```

HTTP status codes:
- `200` — Success
- `400` — Bad request (parse error, validation error)
- `401` — Unauthorized (missing or invalid API key)
- `404` — Not found
- `409` — Conflict (duplicate key)
- `429` — Rate limited
- `500` — Internal server error
- `503` — Service unavailable (health check failed)
