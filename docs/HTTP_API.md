# OntoDB HTTP API Documentation

## Overview

OntoDB provides a RESTful HTTP API for executing SQL queries, SPARQL queries, vector similarity searches, and hybrid queries that combine SQL filtering with vector search. It also includes ontology-driven semantic features with OWL-lite reasoning.

## Features

- **SQL Query Engine**: Full SQL support with JOINs, aggregates, window functions, CTEs, transactions
- **SPARQL Endpoint**: W3C SPARQL 1.1 subset support with automatic SQL translation
- **Vector Search**: HNSW-based similarity search with L2, Cosine, InnerProduct metrics
- **Ontology Engine**: OWL-lite model with 7 inference rules (subclass, inverse, transitive, symmetric, etc.)
- **RDF Import/Export**: Turtle parsing, N-Triples and JSON-LD export
- **Schema Introspection**: GET /api/schema for database metadata

## Quick Start

### Starting the HTTP Server

```bash
# Start HTTP API server on default port 8080 (no auth)
ontodb-server --http 127.0.0.1:8080

# Start with authentication enabled
ontodb-server --http 127.0.0.1:8080 --auth --api-keys-file config/api_keys.json

# Start with custom rate limiting
ontodb-server --http 127.0.0.1:8080 --auth --api-keys-file config/api_keys.json --rate-limit 120 --burst-size 20

# Disable rate limiting
ontodb-server --http 127.0.0.1:8080 --auth --api-keys-file config/api_keys.json --no-rate-limit
```

### Base URL

```
http://127.0.0.1:8080
```

## API Endpoints

### 1. Health Check & Monitoring

#### GET `/api/health` - Comprehensive Health Check

Returns server health status with uptime and component checks.

```json
{
  "status": "ok",
  "version": "0.1.0",
  "engine": "OntoDB",
  "uptime_seconds": 3600,
  "checks": {
    "storage": "ok",
    "query_engine": "ok"
  }
}
```

#### GET `/api/health/ready` - Readiness Probe (Kubernetes)

Returns 200 if the server is ready to accept traffic.

```json
{"status": "ready"}
```

#### GET `/api/health/live` - Liveness Probe (Kubernetes)

Returns 200 if the server is alive.

```json
{"status": "alive"}
```

#### GET `/metrics` - Prometheus Metrics

Returns metrics in Prometheus exposition format. Use with Prometheus, Grafana, or any compatible monitoring system.

```bash
curl http://127.0.0.1:8080/metrics
```

Example output:
```
# HELP ontodb_info OntoDB server information
# TYPE ontodb_info gauge
ontodb_info{version="0.1.0"} 1
# HELP ontodb_uptime_seconds Server uptime in seconds
# TYPE ontodb_uptime_seconds gauge
ontodb_uptime_seconds 3600
# HELP ontodb_queries_total Total queries received
# TYPE ontodb_queries_total counter
ontodb_queries_total 1542
# HELP ontodb_queries_by_type Total queries by type
# TYPE ontodb_queries_by_type counter
ontodb_queries_by_type{type="select"} 1200
ontodb_queries_by_type{type="insert"} 200
ontodb_queries_by_type{type="vector_search"} 142
# HELP ontodb_query_duration_seconds Query execution latency in seconds
# TYPE ontodb_query_duration_seconds histogram
ontodb_query_duration_seconds{le="0.001"} 500
ontodb_query_duration_seconds{le="0.01"} 1200
ontodb_query_duration_seconds{le="0.1"} 1500
ontodb_query_duration_seconds{le="+Inf"} 1542
ontodb_query_duration_seconds_sum 12.5
ontodb_query_duration_seconds_count 1542
```

#### GET `/api/metrics` - JSON Metrics

Returns metrics in JSON format for programmatic access.

```bash
curl http://127.0.0.1:8080/api/metrics
```

Response:
```json
{
  "server": {
    "version": "0.1.0",
    "uptime_seconds": 3600
  },
  "queries": {
    "total": 1542,
    "by_type": {
      "select": 1200,
      "insert": 200,
      "update": 0,
      "delete": 0,
      "vector_search": 142,
      "other": 0
    },
    "errors": 5,
    "parse_errors": 2
  },
  "vector_search": {
    "results_total": 710
  },
  "connections": {
    "http_total": 500,
    "http_active": 3,
    "tcp_total": 0,
    "tcp_active": 0
  },
  "auth": {
    "attempts": 500,
    "successes": 498,
    "failures": 2
  },
  "rate_limiting": {
    "limited_total": 10
  },
  "storage": {
    "sstable_count": 5,
    "entries": 10000,
    "compactions": 3
  }
}
```

#### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `ontodb_info` | gauge | Server version info |
| `ontodb_uptime_seconds` | gauge | Server uptime |
| `ontodb_queries_total` | counter | Total queries received |
| `ontodb_queries_by_type` | counter | Queries by type (select/insert/update/delete/vector_search) |
| `ontodb_query_errors_total` | counter | Query execution errors |
| `ontodb_parse_errors_total` | counter | Query parse errors |
| `ontodb_query_duration_seconds` | histogram | Query latency distribution |
| `ontodb_vector_search_duration_seconds` | histogram | Vector search latency |
| `ontodb_vector_search_results_total` | counter | Total vector search results |
| `ontodb_http_connections_total` | counter | Total HTTP connections |
| `ontodb_http_connections_active` | gauge | Active HTTP connections |
| `ontodb_tcp_connections_total` | counter | Total TCP connections |
| `ontodb_tcp_connections_active` | gauge | Active TCP connections |
| `ontodb_auth_attempts_total` | counter | Authentication attempts |
| `ontodb_auth_successes_total` | counter | Successful auth |
| `ontodb_auth_failures_total` | counter | Failed auth |
| `ontodb_rate_limited_total` | counter | Rate-limited requests |
| `ontodb_sstable_count` | gauge | Number of SSTables |
| `ontodb_storage_entries` | gauge | Total storage entries |
| `ontodb_compactions_total` | counter | Compactions performed |

#### Prometheus Configuration

Add to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'ontodb'
    static_configs:
      - targets: ['localhost:8080']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

#### Grafana Dashboard

Import the OntoDB Grafana dashboard for pre-built visualizations of all metrics.

#### Examples

```bash
# Health check
curl http://127.0.0.1:8080/api/health

# Readiness probe
curl http://127.0.0.1:8080/api/health/ready

# Prometheus metrics
curl http://127.0.0.1:8080/metrics

# JSON metrics
curl http://127.0.0.1:8080/api/metrics
```

---

### 2. SQL Query Execution

**POST** `/api/query`

Execute any SQL or OntoDB semantic query.

#### Request Body

```json
{
  "query": "SELECT * FROM Product WHERE price > 100",
  "pretty": false
}
```

| Field   | Type    | Required | Description                          |
|---------|---------|----------|--------------------------------------|
| query   | string  | Yes      | SQL or OntoDB query string           |
| pretty  | boolean | No       | Pretty-print JSON results (default: false) |

#### Supported Query Types

- **SELECT** - Query data with filtering, sorting, aggregation
- **INSERT** - Insert new records
- **UPDATE** - Update existing records
- **DELETE** - Delete records
- **MATCH** - OntoDB semantic class-based queries
- **CREATE ONTOLOGY** - Define ontology schemas
- **CREATE INDEX** - Create secondary indexes
- **CREATE VECTOR INDEX** - Create vector similarity indexes
- **VECTOR SEARCH** - Perform vector similarity search
- **UNION / UNION ALL** - Combine multiple queries

#### Response

```json
{
  "success": true,
  "data": [
    {"name": "iPhone", "price": 999, "__class__": "Product"},
    {"name": "iPad", "price": 799, "__class__": "Product"}
  ],
  "elapsed_ms": 1.23
}
```

#### Examples

```bash
# SELECT query
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT name, price FROM Product WHERE price > 100 LIMIT 10"}'

# INSERT query
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Product (name, price) VALUES ('\"iPhone 15\"', 999)"}'

# MATCH query (OntoDB semantic extension)
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "MATCH (p: Product) WHERE price > 500 RETURN name, price"}'

# Aggregation query
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT category, COUNT(*) as cnt, AVG(price) as avg_price FROM Product GROUP BY category"}'
```

---

### 3. Vector Search

**POST** `/api/vector/search`

Perform vector similarity search on indexed vector columns.

#### Request Body

```json
{
  "class": "Product",
  "column": "embedding",
  "query_vector": [0.1, 0.2, 0.3, 0.4],
  "top_k": 10,
  "filter": "price > 100"
}
```

| Field        | Type     | Required | Description                              |
|--------------|----------|----------|------------------------------------------|
| class        | string   | Yes      | Target class (table) name                |
| column       | string   | Yes      | Vector column name                       |
| query_vector | float[]  | Yes      | Query vector for similarity search       |
| top_k        | integer  | Yes      | Number of top results to return          |
| filter       | string   | No       | SQL WHERE clause for hybrid filtering    |

#### Response

```json
{
  "success": true,
  "data": [
    {
      "name": "iPhone",
      "price": 999,
      "embedding": [0.1, 0.2, 0.3, 0.4],
      "_distance": 0.05,
      "__class__": "Product"
    },
    {
      "name": "iPad",
      "price": 799,
      "embedding": [0.15, 0.25, 0.35, 0.45],
      "_distance": 0.12,
      "__class__": "Product"
    }
  ],
  "elapsed_ms": 2.34
}
```

#### Examples

```bash
# Basic vector search
curl -X POST http://127.0.0.1:8080/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, 0.4],
    "top_k": 5
  }'

# Vector search with filter (hybrid query)
curl -X POST http://127.0.0.1:8080/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, 0.4],
    "top_k": 10,
    "filter": "price > 100 AND category = '\"Electronics\"'"
  }'
```

---

### 4. Hybrid Query (SQL + Vector Search)

**POST** `/api/hybrid/query`

Execute a hybrid query that combines SQL filtering with vector similarity ranking.

#### Request Body

```json
{
  "sql_filter": "SELECT * FROM Product WHERE price > 100",
  "vector_column": "embedding",
  "query_vector": [0.1, 0.2, 0.3, 0.4],
  "top_k": 10,
  "class": "Product"
}
```

| Field         | Type     | Required | Description                                    |
|---------------|----------|----------|------------------------------------------------|
| sql_filter    | string   | Yes      | SQL query for filtering                        |
| vector_column | string   | Yes      | Vector column for similarity ranking           |
| query_vector  | float[]  | Yes      | Query vector for similarity search             |
| top_k         | integer  | Yes      | Number of top results to return                |
| class         | string   | No       | Target class name (inferred from SQL if omitted)|

#### Response

Same as vector search response.

#### Example

```bash
curl -X POST http://127.0.0.1:8080/api/hybrid/query \
  -H "Content-Type: application/json" \
  -d '{
    "sql_filter": "SELECT * FROM Product WHERE price > 100 AND category = '\"Electronics\"'",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, 0.4],
    "top_k": 5,
    "class": "Product"
  }'
```

---

### 5. Schema Introspection

**GET** `/api/schema`

Get database schema information including classes, indexes, and vector indexes.

#### Response

```json
{
  "classes": ["Product", "Category"],
  "indexes": [
    {"class": "Product", "column": "price"}
  ],
  "vector_indexes": [
    {"class": "Product", "column": "embedding", "dimension": 128, "metric": "cosine"}
  ]
}
```

#### Example

```bash
curl http://127.0.0.1:8080/api/schema
```

---

## Error Handling

All endpoints return a standard error response format:

```json
{
  "success": false,
  "error": "Parse error: expected 'FROM' at position 15"
}
```

### HTTP Status Codes

| Code | Description                               |
|------|-------------------------------------------|
| 200  | Success                                   |
| 400  | Bad Request (parse error, invalid input)  |
| 401  | Unauthorized (missing or invalid API key) |
| 403  | Forbidden (insufficient permissions)      |
| 429  | Too Many Requests (rate limit exceeded)   |
| 500  | Internal Server Error (execution error)   |

---

## Common Workflows

### 1. Create Schema and Insert Data

```bash
# Create ontology
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE ONTOLOGY shop (CLASS Product (name STRING REQUIRED, price FLOAT64, category STRING, embedding ARRAY))"}'

# Create vector index
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 128"}'

# Insert data with vector
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Product (name, price, category, embedding) VALUES ('\"iPhone 15\"', 999, '\"Electronics\"', '\"[0.1, 0.2, 0.3, ...]\"')"}'
```

### 2. Search for Similar Products

```bash
curl -X POST http://127.0.0.1:8080/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, ...],
    "top_k": 5
  }'
```

### 3. Hybrid Search: Filter + Vector Similarity

```bash
curl -X POST http://127.0.0.1:8080/api/hybrid/query \
  -H "Content-Type: application/json" \
  -d '{
    "sql_filter": "SELECT * FROM Product WHERE price < 1000 AND category = '\"Electronics\"'",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, ...],
    "top_k": 3,
    "class": "Product"
  }'
```

### 4. SPARQL Query Endpoint

Execute SPARQL queries against the ontology store. Queries are automatically translated to SQL and executed.

#### POST `/sparql` - Execute SPARQL Query

**Request:**
```json
{
  "query": "PREFIX ex: <http://example.org/> SELECT ?name WHERE { ?x rdf:type ex:Person . ?x ex:name ?name . }"
}
```

**Response (W3C SPARQL Results JSON Format):**
```json
{
  "success": true,
  "data": {
    "head": {
      "vars": ["?name"]
    },
    "results": {
      "bindings": [
        {
          "?name": {
            "type": "literal",
            "value": "Alice"
          }
        },
        {
          "?name": {
            "type": "literal",
            "value": "Bob"
          }
        }
      ]
    }
  },
  "elapsed_ms": 1.23
}
```

**Supported SPARQL features:**
- `SELECT` with variables (`?x`, `?y`)
- `WHERE` with triple patterns
- `FILTER` with comparisons (`=`, `!=`, `>`, `<`, `>=`, `<=`), `regex()`, `bound()`
- Logical operators (`&&`, `||`, `!`)
- `ORDER BY` (ASC/DESC)
- `LIMIT`, `OFFSET`
- `PREFIX` declarations

**Example with FILTER:**
```json
{
  "query": "PREFIX ex: <http://example.org/> SELECT ?name WHERE { ?x rdf:type ex:Person . ?x ex:name ?name . FILTER(?name = \"Alice\") }"
}
```

**Example with LIMIT:**
```json
{
  "query": "SELECT ?x ?y WHERE { ?x rdf:type <http://example.org/Person> . ?x <http://example.org/name> ?y . } LIMIT 10"
}
```

### 5. Schema Introspection

#### GET `/api/schema` - Get Database Schema

Returns complete schema information including ontologies, classes, properties, indexes, and vector indexes.

**Response:**
```json
{
  "success": true,
  "data": {
    "ontologies": [
      {
        "name": "my_ontology",
        "classes": {
          "Person": {
            "type": "Normal",
            "superclasses": [],
            "equivalent_classes": [],
            "disjoint_with": [],
            "properties": ["name", "age"]
          },
          "Employee": {
            "type": "Normal",
            "superclasses": ["Person"],
            "equivalent_classes": [],
            "disjoint_with": [],
            "properties": ["employeeId", "department"]
          }
        },
        "properties": {
          "name": {
            "domain": "Person",
            "range": "STRING",
            "required": true,
            "multi_valued": false,
            "is_transitive": false,
            "is_symmetric": false,
            "is_functional": true
          },
          "reportsTo": {
            "domain": "Employee",
            "range": "STRING",
            "required": false,
            "multi_valued": false,
            "inverse_of": "manages",
            "is_transitive": true,
            "is_symmetric": false,
            "is_functional": false
          }
        }
      }
    ],
    "indexes": [...],
    "vector_indexes": [...]
  },
  "elapsed_ms": 0.5
}
```

---

## Ontology & Reasoning

OntoDB includes an OWL-lite ontology engine with automatic reasoning:

### Creating an Ontology

```sql
CREATE ONTOLOGY my_ontology (
  CLASS Person,
  CLASS Employee SUBCLASS OF Person,
  CLASS Manager SUBCLASS OF Employee,
  PROPERTY name ON Person TYPE STRING REQUIRED,
  PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages,
  PROPERTY ancestor ON Person TYPE STRING TRANSITIVE,
  PROPERTY friendOf ON Person TYPE STRING SYMMETRIC
)
```

### Inference Rules

The following OWL-lite rules are automatically applied during query execution:

| Rule | Description | Example |
|------|-------------|---------|
| Cax-sco | Subclass propagation | If `x type Employee` then `x type Person` |
| Cax-eqc | Equivalent class | If `x type Worker` and `Worker equiv Employee` then `x type Employee` |
| Prp-spo | Subproperty | If `x worksUnder y` and `worksUnder subPropertyOf reportsTo` then `x reportsTo y` |
| Prp-eqp | Equivalent property | If `x email y` and `email equiv emailAddress` then `x emailAddress y` |
| Prp-inv | Inverse property | If `x reportsTo y` then `y manages x` |
| Prp-trp | Transitive closure | If `x ancestor y` and `y ancestor z` then `x ancestor z` |
| Prp-symp | Symmetric | If `x friendOf y` then `y friendOf x` |

### OWL Restrictions (Write-time Validation)

Restrictions are enforced during INSERT:

- `hasValue` - Property must have specific value
- `minCardinality` - Minimum number of values
- `maxCardinality` - Maximum number of values
- `exactCardinality` - Exact number of values
- `someValuesFrom` - At least one value from class
- `allValuesFrom` - All values from class

### RDF Import/Export

```rust
// Parse Turtle
let mut parser = onto_ontology::TurtleParser::new();
let ontology = parser.parse(turtle_input, "my_ontology")?;

// Export N-Triples
let ntriples = onto_ontology::to_ntriples(&ontology);

// Export JSON-LD
let jsonld = onto_ontology::to_jsonld(&ontology);
```

---

## Authentication

OntoDB supports API key authentication for securing HTTP API access.

### Enabling Authentication

```bash
# Enable auth with API keys file
ontodb-server --http 127.0.0.1:8080 --auth --api-keys-file config/api_keys.json
```

### API Keys Configuration

Create a JSON file with your API keys:

```json
{
  "enabled": true,
  "keys": [
    {
      "key": "your-secret-api-key-1",
      "description": "Admin key",
      "permission": "Admin",
      "rate_limit": 120
    },
    {
      "key": "your-secret-api-key-2",
      "description": "Read-only dashboard",
      "permission": "ReadOnly",
      "rate_limit": 30
    }
  ],
  "default_permission": "ReadOnly"
}
```

### Permission Levels

| Permission | Description |
|------------|-------------|
| `ReadOnly` | SELECT, VECTOR SEARCH, schema queries only |
| `ReadWrite` | All operations except schema modifications |
| `Admin` | Full access including schema changes |

### Providing API Keys

Three ways to provide your API key:

#### 1. Authorization Header (Recommended)

```bash
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Authorization: Bearer your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product"}'
```

#### 2. X-API-Key Header

```bash
curl -X POST http://127.0.0.1:8080/api/query \
  -H "X-API-Key: your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product"}'
```

#### 3. Query Parameter (Less Secure)

```bash
curl -X POST "http://127.0.0.1:8080/api/query?api_key=your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product"}'
```

### Authentication Errors

```json
// Missing API key
{
  "success": false,
  "error": "Missing API key. Provide via 'Authorization: Bearer <key>' or 'X-API-Key: <key>' header"
}

// Invalid API key
{
  "success": false,
  "error": "Invalid API key"
}

// Insufficient permissions
{
  "success": false,
  "error": "Insufficient permissions. Required: ReadWrite, your key: 'Read-only dashboard'"
}
```

---

## Rate Limiting

OntoDB implements token bucket rate limiting to protect against abuse.

### Rate Limit Configuration

```bash
# Custom rate limit (requests per minute)
ontodb-server --http 127.0.0.1:8080 --auth --api-keys-file config/api_keys.json --rate-limit 120

# Custom burst size
ontodb-server --http 127.0.0.1:8080 --rate-limit 60 --burst-size 20

# Disable rate limiting
ontodb-server --http 127.0.0.1:8080 --no-rate-limit
```

### Rate Limit Headers

All responses include rate limit information:

```
X-RateLimit-Limit: 60          # Maximum requests per minute
X-RateLimit-Remaining: 58      # Remaining requests in current window
X-RateLimit-Reset: 45          # Seconds until rate limit resets
```

### Per-Key Rate Limits

Each API key can have its own rate limit:

```json
{
  "key": "high-priority-key",
  "rate_limit": 200  // 200 requests per minute
}
```

If no per-key limit is set, the default rate limit applies.

### Rate Limit Errors

When rate limited, you'll receive a `429 Too Many Requests` response:

```json
{
  "success": false,
  "error": "Rate limit exceeded. Retry after 45 seconds"
}
```

Headers:
```
HTTP/1.1 429 Too Many Requests
Retry-After: 45
X-RateLimit-Limit: 0
X-RateLimit-Remaining: 0
```

### Burst Handling

The `--burst-size` parameter allows short bursts above the steady rate:

- Default burst size: 10 requests
- Allows up to 10 requests instantly, then refills at the configured RPM rate
- Useful for clients that send multiple requests in quick succession

---

## Performance Tips

1. **Use indexes**: Create secondary indexes on frequently filtered columns
2. **Use vector indexes**: Create HNSW vector indexes for efficient similarity search
3. **Limit results**: Always use `LIMIT` or `top_k` to avoid returning excessive data
4. **Hybrid queries**: Combine SQL filters with vector search for better relevance and performance

---

## Client Libraries

### Python

```python
import requests

API_KEY = "your-secret-api-key"
BASE_URL = "http://127.0.0.1:8080"

headers = {
    "Authorization": f"Bearer {API_KEY}",
    "Content-Type": "application/json"
}

# Execute SQL query
response = requests.post(f"{BASE_URL}/api/query", json={
    "query": "SELECT * FROM Product WHERE price > 100"
}, headers=headers)
print(response.json())

# Vector search
response = requests.post(f"{BASE_URL}/api/vector/search", json={
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "top_k": 5
}, headers=headers)
print(response.json())

# Check rate limits
print(f"Remaining: {response.headers.get('X-RateLimit-Remaining')}")
print(f"Reset: {response.headers.get('X-RateLimit-Reset')}s")
```

### JavaScript

```javascript
const API_KEY = "your-secret-api-key";
const BASE_URL = "http://127.0.0.1:8080";

const headers = {
  "Authorization": `Bearer ${API_KEY}`,
  "Content-Type": "application/json"
};

// Execute SQL query
const response = await fetch(`${BASE_URL}/api/query`, {
  method: "POST",
  headers,
  body: JSON.stringify({
    query: "SELECT * FROM Product WHERE price > 100"
  })
});
const data = await response.json();
console.log(data);

// Check rate limits
console.log(`Remaining: ${response.headers.get('X-RateLimit-Remaining')}`);
console.log(`Reset: ${response.headers.get('X-RateLimit-Reset')}s`);

// Vector search
const vectorResponse = await fetch(`${BASE_URL}/api/vector/search`, {
  method: "POST",
  headers,
  body: JSON.stringify({
    class: "Product",
    column: "embedding",
    query_vector: [0.1, 0.2, 0.3],
    top_k: 5
  })
});
const results = await vectorResponse.json();
console.log(results);
```

### cURL

```bash
# Simple query with API key
curl -X POST http://127.0.0.1:8080/api/query \
  -H "Authorization: Bearer your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM Product LIMIT 10"}'

# Vector search with X-API-Key header
curl -X POST http://127.0.0.1:8080/api/vector/search \
  -H "X-API-Key: your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{
    "class": "Product",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "top_k": 5
  }'

# Check rate limit headers
curl -v -X POST http://127.0.0.1:8080/api/query \
  -H "Authorization: Bearer your-secret-api-key" \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT 1"}' 2>&1 | grep -i "x-ratelimit"
```
