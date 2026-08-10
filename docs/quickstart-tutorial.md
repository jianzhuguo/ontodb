# OntoDB 快速入门教程

> 10 分钟学会 OntoDB 的核心功能

---

## 第 1 步：启动服务器

```bash
# 下载并解压
wget https://release.ontodb.io/ontodb-v0.3.0-linux-x86_64.tar.gz
tar xzf ontodb-v0.3.0-linux-x86_64.tar.gz
cd ontodb-v0.3.0

# 启动
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit
```

看到 `HTTP API server listening on 127.0.0.1:7912` 表示启动成功。

---

## 第 2 步：创建表并插入数据

```bash
# 创建用户表
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE VERTEX TABLE users (name STRING, age INT, city STRING)"}'

# 插入数据
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users (name, age, city) VALUES (\"Alice\", 30, \"北京\")"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users (name, age, city) VALUES (\"Bob\", 25, \"上海\")"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users (name, age, city) VALUES (\"Charlie\", 35, \"北京\")"}'
```

---

## 第 3 步：查询数据

```bash
# 查询所有用户
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users"}'

# 条件查询
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users WHERE age > 28"}'

# 聚合查询
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT city, COUNT(*) as count FROM users GROUP BY city"}'
```

---

## 第 4 步：向量搜索

```bash
# 创建文档表
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE VERTEX TABLE documents (title STRING, content STRING, embedding ARRAY)"}'

# 插入带向量的文档
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO documents (title, content, embedding) VALUES (\"Rust入门\", \"Rust是系统编程语言\", [0.1, 0.2, 0.3, 0.4, 0.5])"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO documents (title, content, embedding) VALUES (\"Python教程\", \"Python是脚本语言\", [0.2, 0.3, 0.4, 0.5, 0.6])"}'

# 向量搜索（找最相似的文档）
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "column": "embedding",
    "query_vector": [0.15, 0.25, 0.35, 0.45, 0.55],
    "top_k": 2
  }'
```

---

## 第 5 步：图查询

```bash
# 创建人物表
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE VERTEX TABLE Person (name STRING, age INT)"}'

# 创建关系表
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE EDGE TABLE knows (from_id STRING, to_id STRING)"}'

# 插入人物
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person (name, age) VALUES (\"Alice\", 30)"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person (name, age) VALUES (\"Bob\", 25)"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO Person (name, age) VALUES (\"Charlie\", 35)"}'

# 插入关系
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO knows (from_id, to_id) VALUES (\"Person::1\", \"Person::2\")"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO knows (from_id, to_id) VALUES (\"Person::2\", \"Person::3\")"}'

# 图遍历（从 Alice 出发，2 跳）
curl -X POST http://127.0.0.1:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "Person::1",
    "direction": "out",
    "max_depth": 2
  }'
```

---

## 第 6 步：使用 Python SDK

```bash
pip install ontodb
```

```python
from ontodb import OntoDB

# 连接
db = OntoDB("http://localhost:7912")

# 查询
rows = db.query("SELECT * FROM users")
for row in rows:
    print(f"{row['name']}: {row['age']}岁, {row['city']}")

# 向量搜索
results = db.vector_search("documents", "embedding", [0.15, 0.25, 0.35, 0.45, 0.55], top_k=2)
for r in results:
    print(f"{r['title']} (相似度: {r.get('_score', 'N/A')})")

# 批量插入
db.insert_many("users", [
    {"name": "David", "age": 28, "city": "深圳"},
    {"name": "Eve", "age": 32, "city": "杭州"},
])
```

---

## 第 7 步：查看监控

```bash
# 健康检查
curl http://127.0.0.1:7912/api/health

# 获取指标
curl http://127.0.0.1:7912/api/metrics

# Prometheus 格式指标
curl http://127.0.0.1:7912/metrics

# 打开 Web 控制台
# 浏览器访问 http://127.0.0.1:7912/console
```

---

## 下一步

- 阅读 [用户手册](user-manual.md) 了解更多功能
- 查看 [API 文档](http://127.0.0.1:7912/api/docs) 了解所有接口
- 探索 [数字军师](/digital-advisor) 决策智能系统
- 探索 [数字孪生](/digital-twin) 监控大屏

---

## 常见问题

**Q: 如何修改端口？**
```bash
./ontodb-server --http 0.0.0.0:8080
```

**Q: 如何启用认证？**
```bash
./ontodb-server --auth --api-key my-secret-key
```

**Q: 数据存储在哪里？**
默认在 `--data-dir` 指定的目录，通常是 `./data/`

**Q: 如何备份？**
```bash
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/my-backup.ontodb"}'
```

**Q: 支持哪些客户端？**
- HTTP REST API（任何语言）
- PostgreSQL 客户端（psql、pgAdmin 等）
- MySQL 客户端（mysql、MySQL Workbench 等）
- Python SDK、JavaScript SDK、Go SDK、Java SDK
