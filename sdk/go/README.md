# OntoDB Go SDK

Go client library for [OntoDB](https://ontodb.ai) â€?the ontology-driven semantic multi-modal database.

## Installation

```bash
go get github.com/ontodb/ontodb-go
```

## Quick Start

```go
package main

import (
    "fmt"
    "log"

    "github.com/ontodb/ontodb-go"
)

func main() {
    client, err := ontodb.New("http://localhost:7912", ontodb.WithAPIKey("your-key"))
    if err != nil {
        log.Fatal(err)
    }
    defer client.Close()

    // Execute SQL
    rows, err := client.Query("SELECT * FROM users LIMIT 10")
    if err != nil {
        log.Fatal(err)
    }
    for _, row := range rows {
        fmt.Println(row["name"], row["age"])
    }
}
```

## Authentication

```go
client, _ := ontodb.New("http://localhost:7912",
    ontodb.WithAPIKey("your-secret-key"),
)
```

## Configuration Options

```go
client, _ := ontodb.New("http://localhost:7912",
    ontodb.WithAPIKey("your-key"),           // API key
    ontodb.WithTimeout(60*time.Second),      // Request timeout
    ontodb.WithMaxRetries(5),                // Max retries
    ontodb.WithHTTPClient(customClient),     // Custom HTTP client
)
```

## SQL Queries

```go
// Simple query
rows, err := client.Query("SELECT * FROM products WHERE price > 100")
for _, row := range rows {
    fmt.Println(row["name"], row["price"])
}

// Execute statement
err := client.Execute("INSERT INTO users (name, age) VALUES ('Alice', 30)")

// Batch insert
err := client.InsertMany("users", []map[string]interface{}{
    {"name": "Bob", "age": 25},
    {"name": "Charlie", "age": 35},
})
```

## Vector Search

```go
results, err := client.VectorSearch("documents", "embedding", []float64{0.1, 0.2, 0.3}, 5)
for _, r := range results {
    fmt.Println(r["title"], r["_score"])
}
```

## SPARQL Queries

```go
rows, err := client.Sparql(`
    PREFIX ex: <http://example.org/>
    SELECT ?name WHERE { ?p ex:name ?name }
`)
```

## Graph Operations

```go
// Traverse graph
result, err := client.GraphTraverse("Person::1", "out", 3)
fmt.Println(result["vertices"])

// Shortest path
path, err := client.GraphShortestPath("Person::1", "Person::5")
fmt.Println(strings.Join(path, " -> "))
```

## Context Support

All methods have a `*Context` variant for context support:

```go
ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
defer cancel()

rows, err := client.QueryContext(ctx, "SELECT * FROM users")
```

## Health & Readiness

```go
// Check health
health, err := client.Health()
fmt.Println(health["status"]) // "ok"

// Quick readiness check
if client.IsReady() {
    fmt.Println("Server is ready")
}
```

## Backup & Restore

```go
// Create backup
err := client.Backup("/backups/2026-08-10.ontodb")

// Restore
err := client.Restore("/backups/2026-08-10.ontodb")
```

## Error Handling

```go
rows, err := client.Query("SELECT * FROM nonexistent")
if err != nil {
    switch e := err.(type) {
    case *ontodb.QueryError:
        fmt.Println("Query error:", e.Message)
    case *ontodb.ConnectionError:
        fmt.Println("Connection error:", e.Err)
    case *ontodb.AuthenticationError:
        fmt.Println("Auth error:", e.Message)
    case *ontodb.RateLimitError:
        fmt.Println("Rate limited:", e.Message)
    default:
        fmt.Println("Unknown error:", err)
    }
}
```

## API Reference

### Client Creation

| Function | Description |
|----------|-------------|
| `New(baseURL, ...Option)` | Create new client |
| `WithAPIKey(key)` | Set API key |
| `WithTimeout(d)` | Set request timeout |
| `WithMaxRetries(n)` | Set max retries |
| `WithHTTPClient(hc)` | Set custom HTTP client |

### SQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `Query(sql)` | Execute SQL query | `([]map, error)` |
| `Execute(sql)` | Execute SQL statement | `error` |
| `InsertMany(table, rows)` | Batch insert | `error` |

### Vector Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `VectorSearch(table, column, vector, topK)` | Vector search | `([]map, error)` |

### SPARQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `Sparql(query)` | Execute SPARQL | `([]map, error)` |

### Graph Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `GraphTraverse(startID, direction, depth)` | Graph traversal | `(map, error)` |
| `GraphShortestPath(fromID, toID)` | Shortest path | `([]string, error)` |

### System Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `Health()` | Health check | `(map, error)` |
| `IsReady()` | Readiness check | `bool` |
| `Schema()` | Schema info | `(map, error)` |
| `Backup(path)` | Create backup | `error` |
| `Restore(path)` | Restore backup | `error` |

## License

Apache License 2.0
