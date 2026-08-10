# OntoDB 常见问题 (FAQ)

---

## 安装部署

### Q: 如何安装 OntoDB？

```bash
# Linux 一键安装
curl -fsSL https://get.ontodb.io/install.sh | bash

# Docker
docker run -d -p 7912:7912 -v ontodb-data:/data ontodb/ontodb:latest
```

### Q: 支持哪些操作系统？

- Linux x86_64 (Ubuntu 20.04+, CentOS 8+)
- Windows 10/11 x86_64
- macOS (Intel/Apple Silicon)
- Docker (所有平台)

### Q: 最低硬件要求？

- CPU: 2 核
- 内存: 2 GB
- 磁盘: 10 GB SSD

### Q: 如何启动服务器？

```bash
./ontodb-server --data-dir ./data --http 127.0.0.1:7912
```

---

## 连接访问

### Q: 有哪些访问方式？

| 方式 | 端口 | 说明 |
|------|------|------|
| HTTP REST API | 7912 | 主要接口 |
| PostgreSQL Wire | 7913 | psql/pgAdmin |
| MySQL Wire | 7914 | mysql/Workbench |
| Web 控制台 | 7912/console | 浏览器 |
| CLI | — | 命令行工具 |

### Q: 如何使用 psql 连接？

```bash
psql -h 127.0.0.1 -p 7913 -U ontodb
```

### Q: 如何启用认证？

```bash
./ontodb-server --auth --api-key "your-secret-key"

# 使用时带上 API Key
curl -H "Authorization: Bearer your-secret-key" \
  http://127.0.0.1:7912/api/query \
  -d '{"query": "SELECT * FROM users"}'
```

---

## SQL 查询

### Q: 支持哪些 SQL 语句？

- DDL: CREATE TABLE, CREATE INDEX, DROP TABLE
- DML: INSERT, UPDATE, DELETE, BATCH INSERT
- DQL: SELECT, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT
- 高级: JOIN, CTE, 窗口函数, 子查询

### Q: 如何创建向量索引？

```sql
CREATE VECTOR INDEX ON documents (embedding)
    DIMENSIONS 128
    METRIC cosine;
```

### Q: 如何进行向量搜索？

```sql
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ...]
    TOP 10;
```

### Q: 如何进行图遍历？

```sql
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;
```

### Q: 查询超时怎么办？

```sql
-- 添加 LIMIT
SELECT * FROM large_table LIMIT 1000;

-- 优化 WHERE 条件
SELECT * FROM users WHERE id = 'u1';  -- 使用索引字段
```

---

## 性能

### Q: 写入性能是多少？

单机约 80 万 ops/s（NVMe SSD）。

### Q: 读取性能是多少？

单机约 120 万 ops/s。

### Q: 如何提升性能？

1. 增加 `memtable_size_mb`（减少 flush）
2. 增加 `block_cache_mb`（提升读取）
3. 使用 NVMe SSD
4. 批量写入代替逐条写入

### Q: 内存占用多少？

默认约 400 MB（64 MB MemTable + 256 MB Cache + 其他）。

---

## 向量搜索

### Q: 支持哪些距离度量？

- `cosine` — 余弦相似度
- `euclidean` — 欧氏距离
- `dot` — 点积

### Q: 最大支持多少维？

4096 维。

### Q: 构建 HNSW 索引需要多久？

5000 × 128D 向量约 2.7 秒。

### Q: 如何混合 SQL 和向量搜索？

```sql
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '技术'
ORDER BY score
LIMIT 5;
```

---

## 图查询

### Q: 最大遍历深度是多少？

默认最大 100 跳。

### Q: 如何找最短路径？

```sql
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

### Q: 如何过滤边类型？

```sql
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 2;
```

---

## 本体推理

### Q: 支持哪些推理规则？

- CaxSco — 类继承
- CaxEqc — 等价类
- PrpSpo — 属性继承
- PrpEqp — 等价属性
- PrpInv — 反向属性
- PrpTrp — 传递属性
- PrpSymp — 对称属性

### Q: 如何查看推导链？

```sql
EXPLAIN SELECT * FROM Animal WHERE hasName = '旺财';
```

---

## 备份恢复

### Q: 如何备份？

```bash
curl -X POST http://localhost:7912/api/backup \
  -d '{"path": "/backups/backup.ontodb"}'
```

### Q: 如何恢复？

```bash
curl -X POST http://localhost:7912/api/restore \
  -d '{"path": "/backups/backup.ontodb"}'
```

### Q: 支持增量备份吗？

支持。

```bash
curl -X POST http://localhost:7912/api/backup/incremental \
  -d '{"path": "/backups/incr.ontodb"}'
```

---

## 集群

### Q: 如何部署集群？

```bash
# 节点 1
./ontodb-server --raft --raft-id 1 --raft-peers "2@node2:7913"

# 节点 2
./ontodb-server --raft --raft-id 2 --raft-peers "1@node1:7913"
```

### Q: 最小集群规模？

3 节点（满足 Quorum）。

### Q: 如何实现负载均衡？

使用 Nginx 反向代理。

---

## SDK

### Q: 支持哪些语言？

- Python: `pip install ontodb`
- JavaScript/TypeScript: `npm install ontodb`
- Go: `go get github.com/ontodb/ontodb-go`
- Java: Maven `io.ontodb:ontodb-java`

### Q: Python SDK 示例？

```python
from ontodb import OntoDB

db = OntoDB("http://localhost:7912", api_key="your-key")
rows = db.query("SELECT * FROM users")
```

---

## 故障排查

### Q: 连接被拒绝？

1. 检查服务器是否启动: `ps aux | grep ontodb`
2. 检查端口: `netstat -tlnp | grep 7912`
3. 检查防火墙: `ufw status`

### Q: 查询超时？

1. 添加 LIMIT
2. 优化 WHERE 条件
3. 创建索引

### Q: 内存不足？

减小配置:
```toml
memtable_size_mb = 32
block_cache_mb = 128
```

### Q: 磁盘满？

1. 清理旧数据
2. 扩容磁盘
3. 启用压缩

### Q: 如何查看日志？

```bash
# systemd
journalctl -u ontodb -f

# Docker
docker logs -f ontodb
```
