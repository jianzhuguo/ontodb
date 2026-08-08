# Property Graph

OntoDB includes a native property graph model with CRUD operations, traversal, and path finding.

## Data model

- **Vertex** — A node with an ID, labels, and key-value properties
- **Edge** — A relationship with an ID, source/target vertices, a label, and properties

## CRUD operations

### Add vertex

```bash
curl -X POST http://localhost:7912/api/graph/vertex \
  -H "Content-Type: application/json" \
  -d '{
    "id": "alice",
    "labels": ["Person"],
    "properties": {"name": "Alice", "age": 30}
  }'
```

### Add edge

```bash
curl -X POST http://localhost:7912/api/graph/edge \
  -H "Content-Type: application/json" \
  -d '{
    "id": "e1",
    "from": "alice",
    "to": "bob",
    "label": "KNOWS",
    "properties": {"since": 2020}
  }'
```

### Get vertex

```bash
curl http://localhost:7912/api/graph/vertex/alice
```

### Delete vertex

```bash
curl -X DELETE http://localhost:7912/api/graph/vertex/alice
```

## Traversal

### BFS

```bash
curl -X POST http://localhost:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "alice",
    "direction": "out",
    "max_depth": 3,
    "algorithm": "bfs"
  }'
```

### DFS

```bash
curl -X POST http://localhost:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "alice",
    "direction": "out",
    "max_depth": 3,
    "algorithm": "dfs"
  }'
```

### Filtered traversal

Only follow edges with a specific label:

```json
{
  "start": "alice",
  "direction": "out",
  "max_depth": 2,
  "edge_label": "KNOWS"
}
```

## Shortest path

```bash
curl -X POST http://localhost:7912/api/graph/shortest-path \
  -H "Content-Type: application/json" \
  -d '{"from": "alice", "to": "dave", "max_depth": 5}'
```

Response:
```json
{
  "success": true,
  "data": {
    "from": "alice",
    "to": "dave",
    "found": true,
    "length": 2,
    "path": {
      "vertex_ids": ["alice", "bob", "dave"],
      "edge_ids": ["e1", "e3"]
    }
  }
}
```

## Neighbors

```bash
curl http://localhost:7912/api/graph/neighbors/alice
```

## Python SDK

```python
from ontodb import OntoDBClient

db = OntoDBClient("http://localhost:7912")

db.add_vertex({"id": "alice", "labels": ["Person"], "properties": {"name": "Alice"}})
db.add_edge({"id": "e1", "from": "alice", "to": "bob", "label": "KNOWS"})

result = db.traverse(start="alice", direction="out", max_depth=2)
```

## TypeScript SDK

```typescript
import { OntoDBClient } from 'ontodb';

const db = new OntoDBClient({ baseUrl: 'http://localhost:7912' });

await db.addVertex({ id: 'alice', labels: ['Person'], properties: { name: 'Alice' } });
await db.addEdge({ id: 'e1', from: 'alice', to: 'bob', label: 'KNOWS' });

const result = await db.traverse({ start: 'alice', direction: 'out', max_depth: 2 });
```
