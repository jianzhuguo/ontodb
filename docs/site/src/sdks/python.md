# Python SDK

Official Python client for the OntoDB HTTP API.

## Installation

```bash
pip install ontodb
```

Or from source:

```bash
cd sdk/python
pip install -e .
```

## Quick start

```python
from ontodb import OntoDBClient

db = OntoDBClient("http://localhost:7912")

# SQL query
result = db.query("SELECT * FROM Product WHERE price > 100")
for row in result:
    print(row)
```

## API

### `OntoDBClient(base_url, api_key=None)`

Create a client instance.

### `db.query(sql)` → list[dict]

Execute a SQL query.

### `db.sparql(query)` → dict

Execute a SPARQL query.

### `db.vector_search(class_name, column, query_vector, top_k, filter=None)` → list[dict]

Perform vector similarity search.

### `db.add_vertex(vertex_dict)`

Add a graph vertex.

### `db.add_edge(edge_dict)`

Add a graph edge.

### `db.get_vertex(id)` → dict

Get a vertex by ID.

### `db.traverse(start, direction, max_depth, edge_label=None)` → dict

Traverse the graph.

### `db.shortest_path(from_id, to_id, max_depth=10)` → dict

Find shortest path between two vertices.

### `db.backup(path)`

Create a full backup.

### `db.flush()`

Flush MemTable to disk.

See `sdk/python/README.md` for the full API reference.
