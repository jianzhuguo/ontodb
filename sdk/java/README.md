# OntoDB Java SDK

Java client for [OntoDB](https://ontodb.io) — the ontology-driven semantic multi-modal database.

Requires Java 11+.

## Installation

### Maven

```xml
<dependency>
    <groupId>io.ontodb</groupId>
    <artifactId>ontodb-java</artifactId>
    <version>0.3.0</version>
</dependency>
```

### Gradle

```groovy
implementation 'io.ontodb:ontodb-java:0.3.0'
```

## Quick Start

```java
import io.ontodb.OntoDBClient;
import java.util.*;

public class Main {
    public static void main(String[] args) throws Exception {
        try (OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-key")) {
            // SQL query
            List<Map<String, Object>> rows = client.query("SELECT * FROM users LIMIT 10");
            for (Map<String, Object> row : rows) {
                System.out.println(row.get("name") + " " + row.get("age"));
            }
        }
    }
}
```

## Authentication

```java
OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-secret-key");
```

## Configuration

```java
OntoDBClient client = new OntoDBClient(
    "http://localhost:7912",  // Server URL
    "your-key",               // API key (nullable)
    60,                       // Timeout in seconds
    5                         // Max retries
);
```

## SQL Queries

```java
// Simple query
List<Map<String, Object>> rows = client.query("SELECT * FROM products WHERE price > 100");

// Execute statement
client.execute("INSERT INTO users (name, age) VALUES ('Alice', 30)");

// Batch insert
List<Map<String, Object>> users = new ArrayList<>();
users.add(Map.of("name", "Bob", "age", 25));
users.add(Map.of("name", "Charlie", "age", 35));
client.insertMany("users", users);
```

## Vector Search

```java
double[] vector = {0.1, 0.2, 0.3, 0.4, 0.5};
List<Map<String, Object>> results = client.vectorSearch(
    "documents", "embedding", vector, 5
);
for (Map<String, Object> r : results) {
    System.out.println(r.get("title") + " " + r.get("_score"));
}
```

## SPARQL Queries

```java
List<Map<String, Object>> results = client.sparql(
    "PREFIX ex: <http://example.org/>" +
    "SELECT ?name WHERE { ?p ex:name ?name }"
);
```

## Graph Operations

```java
// Traverse graph
Map<String, Object> result = client.graphTraverse("Person::1", "out", 3);
System.out.println(result.get("vertices"));

// Shortest path
List<String> path = client.graphShortestPath("Person::1", "Person::5");
System.out.println(String.join(" -> ", path));
```

## Health & Readiness

```java
// Check health
Map<String, Object> health = client.health();
System.out.println(health.get("status")); // "ok"

// Quick readiness check
if (client.isReady()) {
    System.out.println("Server is ready");
}
```

## Backup & Restore

```java
// Create backup
client.backup("/backups/2026-08-10.ontodb");

// Restore
client.restore("/backups/2026-08-10.ontodb");
```

## Error Handling

```java
import io.ontodb.*;

try {
    List<Map<String, Object>> rows = client.query("SELECT * FROM nonexistent");
} catch (QueryException e) {
    System.err.println("Query error: " + e.getMessage());
} catch (ConnectionException e) {
    System.err.println("Connection error: " + e.getMessage());
} catch (AuthenticationException e) {
    System.err.println("Auth error: " + e.getMessage());
} catch (OntoDBException e) {
    System.err.println("Error: " + e.getMessage());
}
```

## API Reference

### Constructor

| Constructor | Description |
|-------------|-------------|
| `OntoDBClient(baseUrl)` | Create client |
| `OntoDBClient(baseUrl, apiKey)` | Create with API key |
| `OntoDBClient(baseUrl, apiKey, timeout, retries)` | Full options |

### SQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `query(sql)` | Execute SQL query | `List<Map>` |
| `execute(sql)` | Execute SQL statement | `void` |
| `insertMany(table, rows)` | Batch insert | `void` |

### Vector Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `vectorSearch(table, column, vector, topK)` | Vector search | `List<Map>` |
| `vectorSearch(table, column, vector, topK, filter)` | With filter | `List<Map>` |

### SPARQL Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `sparql(query)` | Execute SPARQL | `List<Map>` |

### Graph Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `graphTraverse(startId, direction, depth)` | Graph traversal | `Map` |
| `graphShortestPath(fromId, toId)` | Shortest path | `List<String>` |

### System Methods

| Method | Description | Returns |
|--------|-------------|---------|
| `health()` | Health check | `Map` |
| `isReady()` | Readiness check | `boolean` |
| `schema()` | Schema info | `Map` |
| `backup(path)` | Create backup | `void` |
| `restore(path)` | Restore backup | `void` |

## License

Apache License 2.0
