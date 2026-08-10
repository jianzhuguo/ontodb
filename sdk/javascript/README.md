# OntoDB JavaScript/TypeScript SDK

JavaScript/TypeScript client for [OntoDB](https://ontodb.io) — the ontology-driven semantic multi-modal database.

Works in **Node.js** (16+) and **modern browsers**.

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
import { OntoDB } from 'ontodb';

const db = new OntoDB('http://localhost:7912');

// Execute SQL
const rows = await db.query('SELECT * FROM users LIMIT 10');
for (const row of rows) {
  console.log(row.name, row.age);
}

// Insert data
await db.execute("INSERT INTO users (name, age) VALUES ('Alice', 30)");

// Batch insert
await db.insertMany('users', [
  { name: 'Bob', age: 25 },
  { name: 'Charlie', age: 35 },
]);
```

## Authentication

```typescript
const db = new OntoDB('http://localhost:7912', {
  apiKey: 'your-secret-key',
});
```

## SQL Queries

```typescript
// Simple query
const rows = await db.query('SELECT * FROM products WHERE price > 100');

// With timeout
const rows = await db.query('SELECT * FROM large_table', 60_000);

// Multiple queries
const results = await db.queryMany([
  'SELECT COUNT(*) FROM users',
  'SELECT COUNT(*) FROM orders',
]);

// Typed results
interface User {
  name: string;
  age: number;
}
const users = await db.query<User>('SELECT * FROM users');
```

## Vector Search

```typescript
// Search similar vectors
const results = await db.vectorSearch('documents', 'embedding', [0.1, 0.2, 0.3, ...], {
  topK: 5,
  filter: "category = '技术'",
});

for (const r of results) {
  console.log(r.title, r._score);
}
```

## Hybrid Search (SQL + Vector)

```typescript
const results = await db.hybridSearch(
  'documents',
  'embedding',
  [0.1, 0.2, 0.3, ...],
  "category = '技术' AND year > 2020",
  10,
);
```

## SPARQL Queries

```typescript
const results = await db.sparql(`
  PREFIX ex: <http://example.org/>
  SELECT ?name ?age
  WHERE {
    ?person rdf:type ex:Employee .
    ?person ex:name ?name .
    ?person ex:age ?age .
    FILTER(?age > 30)
  }
  ORDER BY ?name
`);
```

## Graph Operations

```typescript
// Traverse graph
const result = await db.graphTraverse('Person::1', {
  direction: 'out',
  edgeLabel: 'knows',
  depth: 3,
  algorithm: 'bfs',
});

console.log(result.vertices);
console.log(result.edges);

// Shortest path
const path = await db.graphShortestPath('Person::1', 'Person::5');
console.log(path.join(' -> '));
```

## Schema Introspection

```typescript
const schema = await db.schema();
for (const table of schema.tables ?? []) {
  console.log(`Table: ${table.name}`);
  for (const col of table.columns ?? []) {
    console.log(`  ${col.name}: ${col.type}`);
  }
}
```

## Health & Metrics

```typescript
// Check health
const health = await db.health();
console.log(health.status); // "ok"

// Quick readiness check
if (await db.isReady()) {
  console.log('Server is ready');
}
```

## Backup & Restore

```typescript
// Create backup
await db.backup('/backups/2026-08-10.ontodb');

// Restore from backup
await db.restore('/backups/2026-08-10.ontodb');
```

## Error Handling

```typescript
import { OntoDB, QueryError, ConnectionError, AuthenticationError } from 'ontodb';

const db = new OntoDB('http://localhost:7912', { apiKey: 'your-key' });

try {
  const rows = await db.query('SELECT * FROM nonexistent_table');
} catch (err) {
  if (err instanceof QueryError) {
    console.error('Query failed:', err.message);
  } else if (err instanceof ConnectionError) {
    console.error('Connection failed:', err.message);
  } else if (err instanceof AuthenticationError) {
    console.error('Auth failed:', err.message);
  }
}
```

## Configuration

```typescript
const db = new OntoDB('http://localhost:7912', {
  apiKey: 'your-key',        // API key (optional)
  timeout: 30_000,           // Default timeout in ms
  maxRetries: 3,             // Max retries on failure
  headers: {                 // Custom headers
    'X-Custom': 'value',
  },
});
```

## API Reference

### `new OntoDB(baseUrl, options?)`

#### SQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `query(sql, timeout?)` | Execute SQL query | `Promise<T[]>` |
| `execute(sql, timeout?)` | Execute SQL statement | `Promise<ApiResponse>` |
| `queryMany(sqls, timeout?)` | Execute multiple queries | `Promise<T[][]>` |
| `insertMany(table, rows, timeout?)` | Batch insert rows | `Promise<ApiResponse>` |

#### Vector Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `vectorSearch(table, column, vector, options?)` | Vector similarity search | `Promise<T[]>` |
| `hybridSearch(table, vectorColumn, vector, filter?, topK?, timeout?)` | SQL + vector search | `Promise<T[]>` |

#### SPARQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `sparql(query, timeout?)` | Execute SPARQL query | `Promise<T[]>` |

#### Graph Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `graphTraverse(startId, options?)` | Graph traversal | `Promise<{vertices, edges}>` |
| `graphShortestPath(fromId, toId, timeout?)` | Shortest path | `Promise<string[]>` |

#### System Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `schema(timeout?)` | Get database schema | `Promise<SchemaInfo>` |
| `health(timeout?)` | Check server health | `Promise<HealthStatus>` |
| `isReady()` | Quick readiness check | `Promise<boolean>` |
| `backup(path, timeout?)` | Create backup | `Promise<ApiResponse>` |
| `restore(path, timeout?)` | Restore from backup | `Promise<ApiResponse>` |

## License

Apache License 2.0
