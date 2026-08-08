# Vector Search

OntoDB provides native HNSW (Hierarchical Navigable Small World) vector indexing with 100% recall rate.

## Creating a vector index

```sql
CREATE VECTOR INDEX ON Product (embedding) DIM 128 METRIC cosine
```

Supported distance metrics:
- `cosine` — Cosine similarity (best for text embeddings)
- `l2` — Euclidean distance
- `inner_product` — Inner product (for normalized vectors)

## Inserting vectors

```sql
-- Insert a document with its vector
INSERT INTO Product SET id = "1", name = "Widget"

-- Insert the vector separately
VECTOR INSERT ON Product ("1") embedding [0.1, 0.2, 0.3, ...]
```

## Searching

```sql
-- Basic vector search
VECTOR SEARCH ON Product (embedding) QUERY [0.1, 0.2, 0.3, ...] TOP 10

-- With filter (hybrid search)
VECTOR SEARCH ON Product (embedding) QUERY [0.1, 0.2, 0.3, ...] TOP 10 WHERE category = "electronics"
```

## HTTP API

```bash
curl -X POST http://localhost:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "top_k": 10,
    "filter": "price > 50"
  }'
```

## Performance

Benchmark results (5000 rows, debug build):

| Operation | Latency |
|-----------|---------|
| Vector search (top 10) | ~5ms |
| Vector search + SQL filter | ~8ms |
| HNSW insert | ~0.1ms/vector |

## How it works

1. **HNSW index** builds a multi-layer graph for fast approximate nearest neighbor search
2. **100% recall** achieved through optimized graph construction and search parameters
3. **Hybrid queries** apply SQL filters before or after vector search for maximum flexibility
4. **Integration** with storage engine means vectors are persisted alongside regular data
