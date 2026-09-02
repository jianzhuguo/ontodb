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

Vectors are automatically indexed when documents are written. The embedding field should be stored as a JSON array.

```python
# Using Python SDK
db = OntoDB("http://localhost:7912")
db.execute('INSERT INTO Product (name, category, embedding) VALUES ("Widget", "electronics", [0.1, 0.2, 0.3])')
```

```bash
# Using HTTP API directly
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Product (name, embedding) VALUES (\"Widget\", [0.1, 0.2, 0.3])"}'
```

> **Note**: Vectors must be inserted as JSON arrays `[0.1, 0.2, ...]`, not as strings. The engine automatically detects vector fields and indexes them.

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

## Advanced Features

### Cosine Pre-normalization

When using cosine distance, vectors are automatically L2-normalized on insert. This provides ~2x speedup for cosine search because the distance calculation reduces to a simple dot product.

### Multi-vector Search

Search multiple vector columns simultaneously and combine results using Reciprocal Rank Fusion (RRF):

```python
results = db.vector_search_multi("Product", [
    {"column": "title_embedding", "query_vector": [...], "weight": 0.7},
    {"column": "image_embedding", "query_vector": [...], "weight": 0.3},
], top_k=10)
```

```bash
curl -X POST http://localhost:7912/api/vector/search-multi \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "searches": [
      {"column": "title_embedding", "query_vector": [0.1, 0.2], "weight": 0.7},
      {"column": "image_embedding", "query_vector": [0.3, 0.4], "weight": 0.3}
    ],
    "top_k": 10
  }'
```

### K-Means Clustering

Cluster vectors using K-Means with k-means++ initialization:

```python
result = db.vector_cluster("Product", "embedding", k=5)
for cluster in result["clusters"]:
    print(f"Cluster {cluster['id']}: {cluster['member_count']} members")
```

```bash
curl -X POST http://localhost:7912/api/vector/cluster \
  -H "Content-Type: application/json" \
  -d '{"class": "Product", "column": "embedding", "k": 5}'
```

### Incremental Persistence

HNSW indexes support incremental persistence - only modified nodes are saved on flush, reducing I/O from O(graph_size) to O(changes).

### Index Compaction

Over time, deleted vectors accumulate as tombstones. Use compaction to rebuild the index and remove tombstones:

```sql
COMPACT VECTOR INDEX ON Product (embedding);
```

### Index Warmup

After server restart, vector indexes can be pre-warmed to reduce cold-start latency. Warmup happens automatically on startup (configurable).
