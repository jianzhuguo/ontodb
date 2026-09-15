# OntoDB 用户指南

## 目录

1. [快速开始](#快速开始)
2. [安装部署](#安装部署)
3. [连接方式](#连接方式)
4. [SQL 语法](#sql-语法)
5. [向量搜索](#向量搜索)
6. [图查询](#图查询)
7. [SPARQL 查询](#sparql-查询)
8. [数据导入导出](#数据导入导出)
9. [安全配置](#安全配置)
10. [监控运维](#监控运维)
11. [常见问题](#常见问题)

---

## 快速开始

### 1. 启动服务器

```bash
# 最简启动（开发环境）
ontodb-server --data-dir ./mydata --http 0.0.0.0:7912

# 生产环境启动
ontodb-server \
  --data-dir /data/ontodb \
  --http 0.0.0.0:7912 \
  --auth \
  --api-keys-file config/api_keys.json \
  --audit \
  --tls-cert /etc/ssl/server.crt \
  --tls-key /etc/ssl/server.key
```

### 2. 创建本体（Schema）

```sql
CREATE ONTOLOGY shop (
  CLASS Product,
  CLASS Category,
  CLASS ElectronicProduct SUBCLASSOF Product,
  PROPERTY name DOMAIN Product RANGE STRING,
  PROPERTY price DOMAIN Product RANGE FLOAT64,
  PROPERTY category DOMAIN Product RANGE STRING,
  PROPERTY embedding DOMAIN Product RANGE VECTOR
);
```

### 3. 插入数据

```sql
INSERT INTO Product (name, price, category) VALUES ('iPhone 15', 999.0, 'Electronics');
INSERT INTO Product (name, price, category) VALUES ('MacBook Pro', 2499.0, 'Electronics');
INSERT INTO Product (name, price, category) VALUES ('AirPods', 249.0, 'Audio');
```

### 4. 查询数据

```sql
-- 基本查询
SELECT * FROM Product WHERE price > 500;

-- 聚合查询
SELECT category, COUNT(*) as cnt, AVG(price) as avg_price 
FROM Product 
GROUP BY category;

-- 排序和分页
SELECT name, price FROM Product ORDER BY price DESC LIMIT 10;
```

---

## 安装部署

### 从源码编译

```bash
# 前置要求：Rust 1.70+
git clone https://gitee.com/ontovalue/ontodb.git
cd ontodb
cargo build --release
```

### Docker 部署

```bash
# 构建镜像
docker build -t ontodb .

# 运行
docker run -d \
  -p 7912:7912 \
  -v ontodb-data:/data \
  --name ontodb \
  ontodb --data-dir /data --http 0.0.0.0:7912
```

### Docker Compose 部署（推荐）

```bash
# 复制配置文件
cp .env.example .env
cp config/api_keys.example.json config/api_keys.json

# 编辑配置
vim .env

# 启动所有服务
docker compose up -d

# 查看状态
docker compose ps
```

---

## 连接方式

### HTTP REST API

```bash
# 健康检查
curl http://localhost:7912/api/health

# 执行查询
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_API_KEY" \
  -d '{"query": "SELECT * FROM Product"}'
```

### PostgreSQL 客户端

```bash
# 使用 psql
psql -h 127.0.0.1 -p 5432 -d ontodb

# 使用 DBeaver 或 pgAdmin
# 主机: 127.0.0.1
# 端口: 5432
# 数据库: ontodb
# 用户名: root
# 密码: YOUR_API_KEY
```

### MySQL 客户端

```bash
# 使用 mysql CLI
mysql -h 127.0.0.1 -P 3306 -u root -p

# 使用 Navicat 或 MySQL Workbench
# 主机: 127.0.0.1
# 端口: 3306
# 用户名: root
# 密码: YOUR_API_KEY
```

### TCP CLI

```bash
# 使用 telnet
telnet 127.0.0.1 7913

# 使用 nc
nc 127.0.0.1 7913
```

---

## SQL 语法

### DDL（数据定义）

```sql
-- 创建本体
CREATE ONTOLOGY mydb (
  CLASS User,
  CLASS Post,
  PROPERTY name DOMAIN User RANGE STRING,
  PROPERTY email DOMAIN User RANGE STRING,
  PROPERTY title DOMAIN Post RANGE STRING,
  PROPERTY author DOMAIN Post RANGE STRING
);

-- 创建索引
CREATE INDEX User(email);
CREATE INDEX Post(author);

-- 创建向量索引
CREATE VECTOR INDEX Product(embedding) 
  DIMENSION 128 
  METRIC cosine 
  M 16 
  EF_CONSTRUCTION 200;
```

### DML（数据操作）

```sql
-- 插入
INSERT INTO User (name, email) VALUES ('Alice', 'alice@example.com');

-- 批量插入
INSERT INTO User (name, email) VALUES 
  ('Bob', 'bob@example.com'),
  ('Charlie', 'charlie@example.com');

-- 更新
UPDATE User SET name = 'Alice Smith' WHERE email = 'alice@example.com';

-- 删除
DELETE FROM User WHERE name = 'Bob';

-- 批量导入
COPY Product FROM 'data.csv' FORMAT CSV;
IMPORT INTO Product FROM JSON 'data.json';
```

### 查询

```sql
-- 基本查询
SELECT * FROM Product;
SELECT name, price FROM Product WHERE price > 100;

-- 聚合
SELECT category, COUNT(*) FROM Product GROUP BY category;
SELECT AVG(price), MAX(price), MIN(price) FROM Product;

-- 排序和分页
SELECT * FROM Product ORDER BY price DESC LIMIT 10 OFFSET 20;

-- JOIN
SELECT u.name, p.title 
FROM User u 
JOIN Post p ON u.id = p.author_id;

-- 子查询
SELECT * FROM Product WHERE price > (SELECT AVG(price) FROM Product);

-- CTE
WITH expensive AS (
  SELECT * FROM Product WHERE price > 1000
)
SELECT name, price FROM expensive ORDER BY price DESC;
```

### 事务

```sql
BEGIN;
INSERT INTO User (name) VALUES ('Dave');
UPDATE Account SET balance = balance - 100 WHERE user = 'Dave';
COMMIT;

-- 或回滚
BEGIN;
INSERT INTO User (name) VALUES ('Eve');
ROLLBACK;
```

---

## 向量搜索

### 创建向量索引

```sql
CREATE VECTOR INDEX Product(embedding) 
  DIMENSION 128 
  METRIC cosine 
  M 16 
  EF_CONSTRUCTION 200 
  EF_SEARCH 100;
```

### 插入向量数据

```sql
INSERT INTO Product (name, embedding) 
VALUES ('iPhone', '[0.1, 0.2, 0.3, ...]');
```

### 向量搜索

```sql
-- 基本向量搜索
VECTOR SEARCH ON Product (embedding) 
  QUERY [0.1, 0.2, 0.3, ...] 
  TOP 10;

-- 带过滤的向量搜索
VECTOR SEARCH ON Product (embedding) 
  QUERY [0.1, 0.2, 0.3, ...] 
  TOP 10 
  WHERE price > 100;
```

---

## 图查询

### 创建图顶点和边

```sql
-- 创建顶点
INSERT VERTEX (id, labels, properties) VALUES 
  ('v1', ['Person'], '{"name": "Alice"}'),
  ('v2', ['Person'], '{"name": "Bob"}');

-- 创建边
INSERT EDGE (id, from, to, label, properties) VALUES 
  ('e1', 'v1', 'v2', 'knows', '{"since": 2020}');
```

### 图遍历

```sql
-- BFS 遍历
GRAPH TRAVERSE FROM 'v1' 
  DIRECTION out 
  MAX_DEPTH 3 
  EDGE_LABEL knows;

-- 最短路径
GRAPH SHORTEST-PATH FROM 'v1' TO 'v3' MAX_DEPTH 10;
```

---

## SPARQL 查询

```sparql
# 基本查询
SELECT ?name WHERE {
  ?x rdf:type ex:Person .
  ?x ex:name ?name .
}

# 带过滤
SELECT ?name ?age WHERE {
  ?x rdf:type ex:Person .
  ?x ex:name ?name .
  ?x ex:age ?age .
  FILTER(?age > 25)
}

# OPTIONAL
SELECT ?name ?email WHERE {
  ?x rdf:type ex:Person .
  ?x ex:name ?name .
  OPTIONAL { ?x ex:email ?email }
}
```

---

## 数据导入导出

### CSV 导入

```sql
-- 使用 COPY 命令（最快）
COPY Product FROM 'products.csv' FORMAT CSV;

-- CSV 格式要求：
-- 第一行为列名
-- 数据用逗号分隔
-- 字符串用双引号包裹
```

### JSON 导入

```sql
-- JSON 数组格式
IMPORT INTO Product FROM JSON 'products.json';

-- JSON Lines 格式（每行一个 JSON 对象）
IMPORT INTO Product FROM JSON 'products.jsonl';
```

### 备份与恢复

```sql
-- 全量备份
BACKUP TO '/data/backup/full_20260809';

-- 增量备份
BACKUP INCREMENTAL TO '/data/backup/incr_20260809' 
  SINCE '2026-08-08T00:00:00Z';

-- 验证备份
BACKUP VERIFY '/data/backup/full_20260809';
```

---

## 安全配置

### API Key 认证

```bash
# 启用认证
ontodb-server --auth --api-keys-file config/api_keys.json

# config/api_keys.json 格式
{
  "keys": [
    {
      "key": "your-secret-key-here",
      "description": "Admin key",
      "permission": "Admin",
      "rate_limit": 1000
    }
  ]
}
```

### TLS/HTTPS

```bash
# 启用 HTTPS
ontodb-server \
  --tls-cert /etc/ssl/server.crt \
  --tls-key /etc/ssl/server.key \
  --tls-min-version 1.2
```

### 安全审计

```bash
# 运行安全检查
ontodb-server --security-audit

# 输出示例：
# Score: 85/100
# ✅ AUTH-001: Authentication enabled
# ❌ TLS-001: TLS not enabled
# ✅ NET-001: Rate limiting enabled
```

---

## 监控运维

### Prometheus 指标

```bash
# 获取指标
curl http://localhost:7912/metrics

# 主要指标：
# - ontodb_queries_total: 查询总数
# - ontodb_query_latency_seconds: 查询延迟
# - ontodb_storage_entries: 存储条目数
# - ontodb_connections_active: 活跃连接数
```

### 健康检查

```bash
# 综合健康检查
curl http://localhost:7912/api/health

# 就绪探针（Kubernetes）
curl http://localhost:7912/api/health/ready

# 存活探针（Kubernetes）
curl http://localhost:7912/api/health/live
```

### 日志管理

```bash
# 审计日志目录
ls audit_logs/

# 日志格式：audit_YYYYMMDD.jsonl
# 自动清理：默认保留 180 天
```

---

## 常见问题

### Q: 如何提高写入性能？

```bash
# 使用批量导入
COPY Product FROM 'data.csv' FORMAT CSV;

# 调整 WAL 同步策略（降低持久性保证）
ontodb-server --data-dir ./data  # sync_wal_on_commit=false（默认）
```

### Q: 如何提高查询性能？

```sql
-- 创建索引
CREATE INDEX Product(price);
CREATE INDEX Product(category);

-- 使用 EXPLAIN 查看执行计划
EXPLAIN SELECT * FROM Product WHERE price > 100;
```

### Q: 向量搜索召回率低怎么办？

```sql
-- 增加 ef_search 参数
-- 在查询时动态调整
SET ef_search = 400;
VECTOR SEARCH ON Product (embedding) QUERY [...] TOP 10;
```

### Q: 如何监控慢查询？

```bash
# 启动时设置慢查询阈值（默认 1 秒）
ontodb-server --data-dir ./data

# 查看慢查询日志
grep "slow_query" logs/*.log
```

### Q: 如何扩展集群？

```bash
# 启动 Raft 节点
ontodb-server \
  --data-dir ./data \
  --raft-node-id 1 \
  --raft-listen 0.0.0.0:9000 \
  --raft-peers "2=192.168.1.2:9000,3=192.168.1.3:9000"
```

### Q: CDC 如何配置？

```bash
# 启用 Kafka CDC
ontodb-server \
  --cdc-enabled \
  --cdc-kafka-brokers "kafka1:9092,kafka2:9092" \
  --cdc-topic "ontodb-changes"
```

---

## 更多资源

- [API 文档](http://localhost:7912/api/docs)
- [OpenAPI 规范](http://localhost:7912/api/openapi.json)
- [GitHub 仓库](https://gitee.com/ontovalue/ontodb)
- [问题反馈](https://gitee.com/ontovalue/ontodb/issues)
