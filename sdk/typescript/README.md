# OntoDB TypeScript/JavaScript SDK

Official client library for the [OntoDB](https://gitee.com/ontovalue/ontodb) HTTP API.

## Installation

```bash
npm install ontodb
# or
yarn add ontodb
# or
pnpm add ontodb
```

## Quick Start

```typescript
import { OntoDBClient } from 'ontodb';

const db = new OntoDBClient({
  baseUrl: 'http://localhost:7912',
  apiKey: 'your-api-key',  // optional
});
```

## SQL Queries

```typescript
// SELECT
const result = await db.query('SELECT * FROM Product WHERE price > 100');
console.log(result.data); // [{ id: "1", name: "Widget", price: 199 }, ...]

// INSERT
await db.query('INSERT INTO Product (id, name, price) VALUES ("1", "Widget", 199)');

// CREATE CLASS
await db.query('CREATE CLASS Product (id STRING, name STRING, price FLOAT)');
```

## Vector Search

```typescript
// Create a vector index
await db.query('CREATE VECTOR INDEX ON Product (embedding) DIM 128 METRIC cosine');

// Search for similar vectors
const results = await db.vectorSearch({
  class: 'Product',
  column: 'embedding',
  query_vector: [0.1, 0.2, 0.3, /* ... */],
  top_k: 10,
  filter: 'price > 50',
});
```

## SPARQL

```typescript
const result = await db.sparql(`
  SELECT ?name WHERE {
    ?x <name> ?name .
    ?x <age> ?age .
    FILTER (?age > 25)
  }
`);
```

## Graph Operations

```typescript
// Add vertices
await db.addVertex({ id: 'alice', labels: ['Person'], properties: { name: 'Alice', age: 30 } });
await db.addVertex({ id: 'bob', labels: ['Person'], properties: { name: 'Bob', age: 25 } });

// Add edge
await db.addEdge({ id: 'e1', from: 'alice', to: 'bob', label: 'KNOWS', properties: { since: 2020 } });

// Get vertex
const alice = await db.getVertex('alice');

// Get neighbors
const neighbors = await db.getNeighbors('alice');

// Traverse
const traversal = await db.traverse({
  start: 'alice',
  direction: 'out',
  max_depth: 3,
  edge_label: 'KNOWS',
});

// Shortest path
const path = await db.shortestPath('alice', 'bob');
```

## Health Checks

```typescript
// Comprehensive health check (probes storage + query engine)
const health = await db.health();
console.log(health.status); // 'ok' or 'degraded'

// Kubernetes probes
const isReady = await db.ready();
const isAlive = await db.alive();
```

## Backup & Restore

```typescript
// Full backup
const backup = await db.backup('/backups/2026-08-08');
console.log(backup.files, backup.total_bytes);

// Incremental backup (only changed files since last backup)
const incremental = await db.backupIncremental('/backups/incremental', '2026-08-08T00:00:00Z');

// Verify backup integrity
const verification = await db.verifyBackup('/backups/2026-08-08');
```

## Error Handling

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

## Configuration

| Option | Default | Description |
|--------|---------|-------------|
| `baseUrl` | `http://localhost:7912` | OntoDB server URL |
| `apiKey` | — | API key for authentication |
| `timeout` | `30000` | Request timeout in ms |

## License

Apache-2.0
