# TypeScript/JavaScript SDK

Official TypeScript/JavaScript client for the OntoDB HTTP API.

## Installation

```bash
npm install ontodb
# or
yarn add ontodb
# or
pnpm add ontodb
```

## Quick start

```typescript
import { OntoDBClient } from 'ontodb';

const db = new OntoDBClient({ baseUrl: 'http://localhost:7912' });

// SQL query
const result = await db.query('SELECT * FROM Product WHERE price > 100');
console.log(result.data);
```

## API

### `new OntoDBClient(options?)`

Create a client instance.

Options:
- `baseUrl` — Server URL (default: `http://localhost:7912`)
- `apiKey` — API key for authentication
- `timeout` — Request timeout in ms (default: `30000`)

### `db.query<T>(sql)` → `ApiResponse<T>`

Execute a SQL query.

### `db.sparql<T>(query)` → `ApiResponse<T>`

Execute a SPARQL query.

### `db.vectorSearch(request)` → `ApiResponse`

Perform vector similarity search.

### `db.hybridSearch(request)` → `ApiResponse`

Hybrid SQL + vector search.

### `db.addVertex(vertex)` → `ApiResponse`

Add a graph vertex.

### `db.getVertex(id)` → `Vertex`

Get a vertex by ID.

### `db.deleteVertex(id)` → `void`

Delete a vertex and its edges.

### `db.addEdge(edge)` → `ApiResponse`

Add a graph edge.

### `db.getNeighbors(id)` → neighbors result

Get neighbors of a vertex.

### `db.traverse(request)` → `TraversalResult`

BFS/DFS graph traversal.

### `db.shortestPath(from, to, maxDepth?)` → `ShortestPathResult`

Find shortest path.

### `db.backup(path)` → `BackupResult`

Full backup.

### `db.backupIncremental(path, since)` → `BackupResult`

Incremental backup.

### `db.verifyBackup(path)` → verification result

Verify backup integrity.

### `db.flush()` → `void`

Flush MemTable to disk.

### `db.health()` → `HealthResponse`

Health check.

### `db.schema()` → schema info

Schema introspection.

## Error handling

```typescript
import { OntoDBClient, OntoDBError } from 'ontodb';

try {
  await db.query('SELECT * FROM nonexistent');
} catch (err) {
  if (err instanceof OntoDBError) {
    console.error(`API error (${err.statusCode}): ${err.message}`);
  }
}
```

## TypeScript types

All request/response types are exported:

```typescript
import type {
  ApiResponse,
  Vertex,
  Edge,
  TraverseRequest,
  TraversalResult,
  ShortestPathResult,
  HealthResponse,
  BackupResult,
  OntoDBClientOptions,
} from 'ontodb';
```

See `sdk/typescript/README.md` for the full reference.
