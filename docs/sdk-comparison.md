# OntoDB SDK 对比

> 各语言 SDK 功能对比和使用指南

---

## 功能对比表

| 功能 | Python | JavaScript/TS | Go | Java |
|------|--------|---------------|-----|------|
| **SQL 查询** | ✅ | ✅ | ✅ | ✅ |
| **批量插入** | ✅ | ✅ | ✅ | ✅ |
| **向量搜索** | ✅ | ✅ | ✅ | ✅ |
| **混合查询** | ✅ | ✅ | ✅ | ✅ |
| **SPARQL** | ✅ | ✅ | ✅ | ✅ |
| **图遍历** | ✅ | ✅ | ✅ | ✅ |
| **最短路径** | ✅ | ✅ | ✅ | ✅ |
| **Schema** | ✅ | ✅ | ✅ | ✅ |
| **健康检查** | ✅ | ✅ | ✅ | ✅ |
| **备份/恢复** | ✅ | ✅ | ✅ | ✅ |
| **Context 支持** | ✅ | ✅ | ✅ | ✅ |
| **自动重试** | ✅ | ✅ | ✅ | ✅ |
| **TypeScript 类型** | — | ✅ | — | — |
| **Async/Await** | ✅ | ✅ | — | — |
| **连接池** | ✅ | ✅ | ✅ | ✅ |

## 安装方式

| 语言 | 安装命令 |
|------|---------|
| Python | `pip install ontodb` |
| JavaScript/TS | `npm install ontodb` |
| Go | `go get github.com/ontodb/ontodb-go` |
| Java | Maven: `io.ontodb:ontodb-java:0.5.6` |

## 快速对比

### Python

```python
from ontodb import OntoDB

db = OntoDB("http://localhost:7912", api_key="your-key")
rows = db.query("SELECT * FROM users")
results = db.vector_search("docs", "embedding", [0.1, 0.2], top_k=5)
```

### JavaScript/TypeScript

```typescript
import { OntoDB } from 'ontodb';

const db = new OntoDB('http://localhost:7912', { apiKey: 'your-key' });
const rows = await db.query('SELECT * FROM users');
const results = await db.vectorSearch('docs', 'embedding', [0.1, 0.2], { topK: 5 });
```

### Go

```go
client, _ := ontodb.New("http://localhost:7912", ontodb.WithAPIKey("your-key"))
rows, _ := client.Query("SELECT * FROM users")
results, _ := client.VectorSearch("docs", "embedding", []float64{0.1, 0.2}, 5)
```

### Java

```java
OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-key");
List<Map<String, Object>> rows = client.query("SELECT * FROM users");
List<Map<String, Object>> results = client.vectorSearch("docs", "embedding", new double[]{0.1, 0.2}, 5);
```

## 性能对比

| 操作 | Python | JS/TS | Go | Java |
|------|--------|-------|-----|------|
| 冷启动 | ~100ms | ~50ms | ~10ms | ~200ms |
| 查询延迟 | ~1ms | ~1ms | ~0.5ms | ~1ms |
| 内存占用 | ~30MB | ~50MB | ~10MB | ~100MB |

## 选择建议

| 场景 | 推荐 SDK |
|------|---------|
| 数据分析/脚本 | Python |
| Web 前端/Node.js | JavaScript/TS |
| 微服务/高并发 | Go |
| 企业应用/Spring | Java |

## 错误处理对比

### Python

```python
from ontodb import OntoDB, QueryError, ConnectionError

try:
    rows = db.query("SELECT * FROM nonexistent")
except QueryError as e:
    print(f"Query error: {e}")
except ConnectionError as e:
    print(f"Connection error: {e}")
```

### JavaScript/TypeScript

```typescript
import { QueryError, ConnectionError } from 'ontodb';

try {
    const rows = await db.query('SELECT * FROM nonexistent');
} catch (e) {
    if (e instanceof QueryError) console.error('Query error:', e.message);
    if (e instanceof ConnectionError) console.error('Connection error:', e.message);
}
```

### Go

```go
rows, err := client.Query("SELECT * FROM nonexistent")
if err != nil {
    switch e := err.(type) {
    case *ontodb.QueryError:
        fmt.Println("Query error:", e.Message)
    case *ontodb.ConnectionError:
        fmt.Println("Connection error:", e.Err)
    }
}
```

### Java

```java
try {
    List<Map<String, Object>> rows = client.query("SELECT * FROM nonexistent");
} catch (QueryException e) {
    System.err.println("Query error: " + e.getMessage());
} catch (ConnectionException e) {
    System.err.println("Connection error: " + e.getMessage());
}
```
