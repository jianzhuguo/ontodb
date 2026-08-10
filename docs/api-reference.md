# OntoDB API 参考手册

> 版本：v0.5.0 | 更新日期：2026-08-10

---

## 概述

OntoDB 提供 RESTful HTTP API，所有请求/响应均使用 JSON 格式。

**Base URL**: `http://127.0.0.1:7912`

**认证**: Bearer Token（可选）
```
Authorization: Bearer your-api-key
```

---

## 1. 查询接口

### 1.1 执行 SQL

```http
POST /api/query
Content-Type: application/json

{
  "query": "SELECT * FROM users LIMIT 10"
}
```

**响应**:
```json
{
  "data": [
    {"name": "Alice", "age": 30},
    {"name": "Bob", "age": 25}
  ],
  "elapsed_ms": 1.23
}
```

**错误响应**:
```json
{
  "error": "table not found: users"
}
```

### 1.2 向量搜索

```http
POST /api/vector/search
Content-Type: application/json

{
  "class": "documents",
  "column": "embedding",
  "query_vector": [0.1, 0.2, 0.3, ...],
  "top_k": 10,
  "filter": "category = '技术'"
}
```

**响应**:
```json
{
  "data": [
    {"title": "Rust入门", "_score": 0.95},
    {"title": "Python教程", "_score": 0.82}
  ]
}
```

### 1.3 混合查询

```http
POST /api/hybrid/query
Content-Type: application/json

{
  "class": "documents",
  "vector_column": "embedding",
  "query_vector": [0.1, 0.2, ...],
  "filter": "year > 2020",
  "top_k": 5
}
```

### 1.4 SPARQL 查询

```http
POST /api/sparql
Content-Type: application/json

{
  "query": "SELECT ?name WHERE { ?p ex:name ?name }"
}
```

---

## 2. 图操作

### 2.1 图遍历

```http
POST /api/graph/traverse
Content-Type: application/json

{
  "start": "Person::1",
  "direction": "out",
  "edge_label": "knows",
  "max_depth": 3,
  "algorithm": "bfs"
}
```

**参数**:
| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| start | string | 是 | 起始节点 ID |
| direction | string | 否 | `out`/`in`/`both`，默认 `out` |
| edge_label | string | 否 | 边标签过滤 |
| max_depth | int | 否 | 最大深度，默认 3 |
| algorithm | string | 否 | `bfs`/`dfs`，默认 `bfs` |

### 2.2 最短路径

```http
POST /api/graph/shortest-path
Content-Type: application/json

{
  "from": "Person::1",
  "to": "Person::5"
}
```

---

## 3. Schema

### 3.1 获取 Schema

```http
GET /api/schema
```

**响应**:
```json
{
  "data": {
    "tables": [
      {
        "name": "users",
        "columns": [
          {"name": "id", "type": "STRING"},
          {"name": "name", "type": "STRING"},
          {"name": "age", "type": "INT"}
        ]
      }
    ]
  }
}
```

---

## 4. 健康与指标

### 4.1 健康检查

```http
GET /api/health
```

**响应**:
```json
{
  "status": "ok",
  "version": "0.5.0",
  "uptime_seconds": 3600
}
```

### 4.2 就绪探针（K8s）

```http
GET /api/health/ready
```

### 4.3 存活探针（K8s）

```http
GET /api/health/live
```

### 4.4 JSON 指标

```http
GET /api/metrics
```

**响应**:
```json
{
  "queries": {
    "total": 12345,
    "select": 10000,
    "insert": 2000,
    "update": 300,
    "delete": 45
  },
  "connections": {
    "http": {"active": 10, "total": 100},
    "tcp": {"active": 5, "total": 50}
  },
  "storage": {
    "entries": 50000,
    "memtable_size_bytes": 1048576,
    "disk_usage_bytes": 10485760
  }
}
```

### 4.5 Prometheus 指标

```http
GET /metrics
```

返回 Prometheus exposition format。

---

## 5. 备份恢复

### 5.1 全量备份

```http
POST /api/backup
Content-Type: application/json

{
  "path": "/backups/full-2026-08-10.ontodb"
}
```

### 5.2 增量备份

```http
POST /api/backup/incremental
Content-Type: application/json

{
  "path": "/backups/incr-2026-08-10.ontodb"
}
```

### 5.3 恢复

```http
POST /api/restore
Content-Type: application/json

{
  "path": "/backups/full-2026-08-10.ontodb"
}
```

---

## 6. 集群

### 6.1 集群状态

```http
GET /api/cluster
```

**响应**:
```json
{
  "mode": "standalone",
  "node_id": "1",
  "uptime_seconds": 3600,
  "version": "0.5.0"
}
```

---

## 7. 管理接口

### 7.1 API Key 管理

```http
GET /api/admin/keys
POST /api/admin/keys
DELETE /api/admin/keys/{key_id}
```

### 7.2 IP 白名单

```http
GET /api/admin/ips
POST /api/admin/ips
DELETE /api/admin/ips/{ip}
```

---

## 8. Web 接口

### 8.1 Web 控制台

```http
GET /console
```

### 8.2 Swagger UI

```http
GET /api/docs
```

### 8.3 数字孪生

```http
GET /digital-twin
```

### 8.4 数字军师

```http
GET /digital-advisor
```

---

## 9. 错误码

| HTTP 状态码 | 说明 |
|------------|------|
| 200 | 成功 |
| 400 | 请求格式错误 |
| 401 | 认证失败 |
| 404 | 资源不存在 |
| 429 | 速率限制 |
| 500 | 服务器内部错误 |

---

## 10. SDK 示例

### Python

```python
from ontodb import OntoDB

db = OntoDB("http://localhost:7912", api_key="your-key")
rows = db.query("SELECT * FROM users")
```

### JavaScript/TypeScript

```typescript
import { OntoDB } from 'ontodb';

const db = new OntoDB('http://localhost:7912', { apiKey: 'your-key' });
const rows = await db.query('SELECT * FROM users');
```

### Go

```go
client, _ := ontodb.New("http://localhost:7912", ontodb.WithAPIKey("your-key"))
rows, _ := client.Query("SELECT * FROM users")
```

### Java

```java
OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-key");
List<Map<String, Object>> rows = client.query("SELECT * FROM users");
```
