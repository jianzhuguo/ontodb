# OntoDB Python SDK

Python client library for [OntoDB](https://github.com/ontodb/ontodb) — an ontology-driven, semantic multi-modal database.

## Installation

```bash
pip install ontodb
```

Or from source:

```bash
cd sdk/python
pip install -e .
```

## Quick Start

```python
from ontodb import OntoDBClient

# Connect to OntoDB server
client = OntoDBClient("http://localhost:7912")

# Check server health
health = client.health()
print(health)

# Execute SQL query
rows = client.query("SELECT * FROM Product WHERE price > 100")
for row in rows:
    print(row["name"], row["price"])

# Execute SPARQL query
result = client.sparql("""
    PREFIX ex: <http://example.org/>
    SELECT ?name WHERE {
        ?p a ex:Product .
        ?p ex:name ?name
    }
""")

# Vector search
results = client.vector_search(
    "Product",
    "embedding",
    [0.1, 0.2, 0.3, 0.4],
    top_k=5,
    filter_expr="price > 100"
)
for result in results:
    print(result["name"], result["_distance"])

# Hybrid SQL + vector search
results = client.hybrid_query(
    sql_filter="SELECT * FROM Product WHERE category = 'Electronics'",
    vector_column="embedding",
    query_vector=[0.1, 0.2, 0.3],
    top_k=10
)
```

## Graph Operations

OntoDB supports property graph operations with vertices, edges, and traversals.

```python
from ontodb import OntoDBClient

client = OntoDBClient("http://localhost:7912")

# Add vertices
client.add_vertex("alice", ["Person"], {"name": "Alice", "age": 30})
client.add_vertex("bob", ["Person"], {"name": "Bob", "age": 25})
client.add_vertex("acme", ["Company"], {"name": "Acme Corp"})

# Add edges
client.add_edge("e1", "alice", "bob", "KNOWS", {"since": 2020})
client.add_edge("e2", "alice", "acme", "WORKS_AT", {"role": "Engineer"})

# Get vertex
alice = client.get_vertex("alice")
print(alice)

# Get neighbors
friends = client.get_neighbors("alice", direction="out", edge_label="KNOWS")
for friend in friends:
    print(friend["id"])

# Graph traversal
result = client.graph_traverse(
    "alice",
    direction="out",
    max_depth=2,
    edge_label="KNOWS"
)
for vertex in result["vertices"]:
    print(vertex["id"])

# Shortest path
path = client.shortest_path("alice", "bob")
if path.get("path"):
    print(f"Path length: {path['path']['length']}")
```

## Authentication

```python
client = OntoDBClient(
    "http://localhost:7912",
    api_key="your-secret-key"
)
```

## API Reference

### OntoDBClient

#### `__init__(base_url, api_key=None, timeout=30)`

Initialize the client.

- `base_url`: Base URL of the OntoDB server
- `api_key`: Optional API key for authentication
- `timeout`: Request timeout in seconds

#### `health() -> dict`

Check server health status.

#### `ready() -> bool`

Check if server is ready to accept traffic.

#### `live() -> bool`

Check if server is alive.

#### `metrics() -> dict`

Get server metrics in JSON format.

#### `schema() -> dict`

Get database schema information.

#### `query(sql, pretty=False) -> list | dict`

Execute a SQL query.

#### `sparql(query) -> dict`

Execute a SPARQL query.

#### `vector_search(class_name, column, query_vector, top_k=10, filter_expr=None) -> list`

Perform vector similarity search.

#### `hybrid_query(sql_filter, vector_column, query_vector, top_k=10, class_name=None) -> list`

Execute a hybrid SQL + vector search query.

### Graph Operations

#### `add_vertex(vertex_id, labels, properties=None) -> dict`

Add a vertex to the graph.

#### `add_edge(edge_id, from_id, to_id, label, properties=None) -> dict`

Add an edge to the graph.

#### `get_vertex(vertex_id) -> dict`

Get a vertex by ID.

#### `delete_vertex(vertex_id) -> dict`

Delete a vertex and all connected edges.

#### `get_neighbors(vertex_id, direction="out", edge_label=None) -> list`

Get neighbors of a vertex.

#### `graph_traverse(start_id, direction="out", max_depth=3, edge_label=None) -> dict`

Traverse the graph from a starting vertex.

#### `shortest_path(from_id, to_id, max_depth=10) -> dict`

Find shortest path between two vertices.

## Error Handling

```python
from ontodb import OntoDBClient, OntoDBError, ConnectionError, QueryError

try:
    client = OntoDBClient("http://localhost:7912")
    result = client.query("SELECT * FROM Product")
except ConnectionError:
    print("Failed to connect to server")
except QueryError as e:
    print(f"Query failed: {e}")
except OntoDBError as e:
    print(f"Error: {e}")
```

## License

Apache License 2.0
