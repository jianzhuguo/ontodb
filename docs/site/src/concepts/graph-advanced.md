# Graph Advanced Features

## EntityId Integration (Relational-to-Graph Bridge)

OntoDB automatically bridges relational data to the graph model. Every row in a class becomes a vertex.

```sql
-- Create relational tables
CREATE CLASS Person (name STRING, age INT64)
CREATE CLASS Company (name STRING, industry STRING)

-- Insert relational data
INSERT INTO Person SET name = "Alice", age = 30
INSERT INTO Person SET name = "Bob", age = 25
INSERT INTO Company SET name = "TechCorp", industry = "tech"

-- Add relationships (auto-creates vertices if needed)
-- Via HTTP API:
POST /api/graph/edge
{
  "from": "Person::Alice",
  "to": "Company::TechCorp",
  "label": "works_at",
  "properties": {"role": "engineer", "since": 2022}
}

-- Query: find all people who work at TechCorp
GET /api/graph/neighbors/Company::TechCorp?direction=in&label=works_at
```

## Property Filters in Traversal

Filter traversal results by vertex/edge properties.

### Filter Operators

| Operator | Description | Example |
|----------|-------------|---------|
| `eq` | Equals | `{"property": "status", "op": "eq", "value": "active"}` |
| `neq` | Not equals | `{"property": "status", "op": "neq", "value": "banned"}` |
| `gt` | Greater than | `{"property": "age", "op": "gt", "value": 18}` |
| `lt` | Less than | `{"property": "price", "op": "lt", "value": 100}` |
| `gte` | Greater or equal | `{"property": "score", "op": "gte", "value": 80}` |
| `lte` | Less or equal | `{"property": "rating", "op": "lte", "value": 5}` |
| `contains` | String contains | `{"property": "name", "op": "contains", "value": "alice"}` |

### Example

```bash
curl -X POST http://localhost:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start_id": "Person::Alice",
    "direction": "out",
    "edge_label": "knows",
    "max_depth": 3,
    "algorithm": "bfs",
    "filter": {"property": "age", "op": "gt", "value": 25}
  }'
```

## Direction Control

| Direction | Description |
|-----------|-------------|
| `out` | Follow outgoing edges only |
| `in` | Follow incoming edges only |
| `both` | Follow both directions (default) |

## Graph Export/Import

### Export to JSON

```bash
curl -X POST http://localhost:7912/api/export \
  -H "Content-Type: application/json" \
  -d '{"format": "jsonl", "query": "SELECT * FROM Person"}'
```

### Import from JSON

```bash
curl -X POST http://localhost:7912/api/import \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Person",
    "data": [
      {"name": "Alice", "age": 30},
      {"name": "Bob", "age": 25}
    ]
  }'
```

## Graph Persistence

All graph data is automatically persisted to the LSM engine:
- Vertices: `__graph_v__` prefix
- Edges: `__graph_e__` prefix

Graph state survives server restarts.

## DoS Protection

Maximum 1 million vertices per graph to prevent memory exhaustion.

## Performance Tips

| Tip | Description |
|-----|-------------|
| Use integer-index BFS | For large graphs, integer-indexed adjacency lists avoid string allocations |
| Filter early | Apply property filters during traversal, not after |
| Limit depth | Use `max_depth` to prevent exponential expansion |
| Use edge labels | Filtering by edge label reduces traversal scope |
