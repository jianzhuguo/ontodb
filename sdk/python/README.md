# OntoDB Python SDK

Python client library for [OntoDB](https://ontodb.io) — the ontology-driven semantic multi-modal database.

## Installation

```bash
pip install ontodb
```

## Quick Start

```python
from ontodb import OntoDB

# Connect to OntoDB
db = OntoDB("http://localhost:7912")

# Execute SQL
rows = db.query("SELECT * FROM users LIMIT 10")
for row in rows:
    print(row["name"], row["age"])

# Insert data
db.execute("INSERT INTO users (name, age) VALUES ('Alice', 30)")

# Batch insert
db.insert_many("users", [
    {"name": "Bob", "age": 25},
    {"name": "Charlie", "age": 35},
])
```

## Authentication

```python
db = OntoDB("http://localhost:7912", api_key="your-secret-key")
```

## SQL Queries

```python
# Simple query
rows = db.query("SELECT * FROM products WHERE price > 100")

# With timeout
rows = db.query("SELECT * FROM large_table", timeout=60.0)

# Multiple queries
results = db.query_many([
    "SELECT COUNT(*) FROM users",
    "SELECT COUNT(*) FROM orders",
])
```

## Vector Search

```python
# Create vector index
db.execute("CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine")

# Search similar vectors
results = db.vector_search(
    table="documents",
    column="embedding",
    vector=[0.1, 0.2, 0.3, ...],  # 128-dimensional vector
    top_k=5,
)

for r in results:
    print(r["title"], r.get("_score"))
```

## Hybrid Search (SQL + Vector)

```python
# Combine SQL filtering with vector similarity
results = db.hybrid_search(
    table="documents",
    vector_column="embedding",
    vector=[0.1, 0.2, 0.3, ...],
    sql_filter="category = '技术' AND year > 2020",
    top_k=10,
)
```

## SPARQL Queries

```python
results = db.sparql("""
    PREFIX ex: <http://example.org/>
    SELECT ?name ?age
    WHERE {
        ?person rdf:type ex:Employee .
        ?person ex:name ?name .
        ?person ex:age ?age .
        FILTER(?age > 30)
    }
    ORDER BY ?name
""")
```

## Graph Operations

```python
# Traverse graph
result = db.graph_traverse(
    start_id="Person::1",
    direction="out",
    edge_label="knows",
    depth=3,
    algorithm="bfs",
)

for vertex in result.get("vertices", []):
    print(vertex)

# Shortest path
path = db.graph_shortest_path("Person::1", "Person::5")
print(" -> ".join(path))
```

## Schema Introspection

```python
schema = db.schema()
for table in schema.get("tables", []):
    print(f"Table: {table['name']}")
    for col in table.get("columns", []):
        print(f"  {col['name']}: {col['type']}")
```

## Health & Metrics

```python
# Check health
health = db.health()
print(health["status"])  # "ok"

# Get metrics
metrics = db.metrics()
print(f"QPS: {metrics.get('qps')}")
print(f"Storage: {metrics.get('storage_bytes')} bytes")

# Quick readiness check
if db.is_ready():
    print("Server is ready")
```

## Backup & Restore

```python
# Create backup
result = db.backup("/backups/2026-08-10.ontodb")
print(f"Backup saved to {result['path']}")

# Restore from backup
db.restore("/backups/2026-08-10.ontodb")
```

## Context Manager

```python
with OntoDB("http://localhost:7912") as db:
    rows = db.query("SELECT * FROM users")
    # Session is automatically closed when exiting
```

## Error Handling

```python
from ontodb import OntoDB, QueryError, ConnectionError, AuthenticationError

db = OntoDB("http://localhost:7912", api_key="your-key")

try:
    rows = db.query("SELECT * FROM nonexistent_table")
except QueryError as e:
    print(f"Query failed: {e}")
except ConnectionError as e:
    print(f"Connection failed: {e}")
except AuthenticationError as e:
    print(f"Auth failed: {e}")
```

## Configuration

```python
db = OntoDB(
    base_url="http://localhost:7912",  # Server URL
    api_key="your-key",                # API key (optional)
    timeout=30.0,                      # Default timeout (seconds)
    max_retries=3,                     # Max retries on failure
)
```

## API Reference

### `OntoDB(base_url, api_key=None, timeout=30.0, max_retries=3)`

#### SQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `query(sql, timeout=None)` | Execute SQL query | `List[Dict]` |
| `execute(sql, timeout=None)` | Execute SQL statement | `Dict` |
| `query_many(sqls, timeout=None)` | Execute multiple queries | `List[List[Dict]]` |
| `insert_many(table, rows, timeout=None)` | Batch insert rows | `Dict` |

#### Vector Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `vector_search(table, column, vector, top_k=10, filter_expr=None)` | Vector similarity search | `List[Dict]` |
| `hybrid_search(table, vector_column, vector, sql_filter, top_k=10)` | SQL + vector search | `List[Dict]` |

#### SPARQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `sparql(query, timeout=None)` | Execute SPARQL query | `List[Dict]` |

#### Graph Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `graph_traverse(start_id, direction, edge_label, depth, algorithm)` | Graph traversal | `Dict` |
| `graph_shortest_path(from_id, to_id)` | Shortest path | `List[str]` |

#### System Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `schema()` | Get database schema | `Dict` |
| `health()` | Check server health | `Dict` |
| `metrics()` | Get server metrics | `Dict` |
| `is_ready()` | Quick readiness check | `bool` |
| `backup(path)` | Create backup | `Dict` |
| `restore(path)` | Restore from backup | `Dict` |

## License

Apache License 2.0
