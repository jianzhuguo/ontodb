# OntoDB 用户手册

> 版本：v0.6.2 | 更新日期�?026-08-27

---

## 目录

1. [快速入门](#1-快速入�?
2. [安装部署](#2-安装部署)
3. [SQL 语法](#3-sql-语法)
4. [向量搜索](#4-向量搜索)
5. [图查询](#5-图查�?
6. [SPARQL 查询](#6-sparql-查询)
7. [本体推理](#7-本体推理)
8. [事务管理](#8-事务管理)
9. [备份恢复](#9-备份恢复)
10. [安全配置](#10-安全配置)
11. [性能调优](#11-性能调优)
12. [故障排查](#12-故障排查)

---

## 1. 快速入�?

### 1.1 三分钟上�?

```bash
# 1. 下载并解�?
wget https://release.ontodb.ai/ontodb-v0.6.2-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.2-linux-x86_64.tar.gz

# 2. 启动服务�?
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# 3. 创建表并插入数据（OntoQL 语法�?
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CREATE CLASS users"}'

curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT INTO users SET name = \"Alice\", age = 30"}'

# 4. 查询数据
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users"}'
```

### 1.2 使用 CLI

```bash
# 连接到服务器
./ontodb-cli 127.0.0.1:7912

# 交互式查�?
ontodb> SELECT * FROM users;
┌─────────┬─────�?
�?name    �?age �?
├─────────┼─────�?
�?Alice   �?30  �?
└─────────┴─────�?
(1 row)
```

### 1.3 使用 Web 控制�?

打开浏览器访�?`http://127.0.0.1:7912/console`，支持：
- SQL 编辑器（语法高亮�?
- 查询历史
- Schema 浏览
- 实时指标

---

## 2. 安装部署

### 2.1 系统要求

| 项目 | 最低要�?| 推荐配置 |
|------|---------|---------|
| CPU | 2 �?| 8 �?|
| 内存 | 2 GB | 16 GB |
| 磁盘 | 10 GB SSD | 100 GB NVMe SSD |
| 操作系统 | Linux x86_64 / Windows 10+ | Ubuntu 22.04 / Windows 11 |

### 2.2 Linux 安装

```bash
# 一键安�?
curl -fsSL https://get.ontodb.ai/install.sh | bash

# 或手动安�?
wget https://release.ontodb.ai/ontodb-v0.6.2-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.2-linux-x86_64.tar.gz -C /opt/ontodb
export PATH=$PATH:/opt/ontodb/bin
```

### 2.3 Windows 安装

```powershell
# PowerShell 一键安�?
irm https://get.ontodb.ai/install.ps1 | iex

# 或手动解压到目录
```

### 2.4 Docker 部署

```bash
# 单节�?
docker run -d --name ontodb \
  -p 7912:7912 -p 7913:7913 \
  -v ontodb-data:/data \
  ontodb/ontodb:latest

# 使用 docker-compose
curl -O https://raw.githubusercontent.com/ontodb/ontodb/main/docker-compose.yml
docker-compose up -d
```

### 2.5 源码编译

```bash
# 前置要求：Rust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# 二进制文件位�?target/release/
```

### 2.6 启动参数

| 参数 | 说明 | 默认�?|
|------|------|--------|
| `--data-dir` | 数据目录 | `./data` |
| `--http` | HTTP 监听地址 | `127.0.0.1:7912` |
| `--auth` | 启用 API Key 认证 | `false` |
| `--api-key` | API 密钥 | 自动生成 |
| `--no-rate-limit` | 禁用速率限制 | `false` |
| `--tls-cert` | TLS 证书文件 | �?|
| `--tls-key` | TLS 私钥文件 | �?|
| `--encryption-enabled` | 启用存储加密 | `false` |
| `--master-key-source` | 主密钥来�?| `env:ONTO_MASTER_KEY` |

---

## 3. SQL 语法

### 3.1 DDL（数据定义）

```sql
-- 创建类（推荐使用 OntoQL 语法�?
CREATE CLASS users

-- 创建带继承的�?
CREATE CLASS Employee EXTENDS Person

-- 创建完整本体（多个类 + 属性）
CREATE ONTOLOGY MyApp (
    CLASS Product,
    CLASS Order,
    PROPERTY name DOMAIN Product RANGE STRING,
    PROPERTY price DOMAIN Product RANGE FLOAT64
)

-- 兼容旧语法（仍可用）
CREATE VERTEX TABLE users (
    id STRING,
    name STRING,
    age INT,
    email STRING
);
```

-- 创建索引
CREATE INDEX ON users (name);
CREATE INDEX ON users (age, name);

-- 创建向量索引
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;

-- 创建物化视图
CREATE MATERIALIZED VIEW user_stats AS
SELECT age, COUNT(*) as cnt FROM users GROUP BY age;
```

### 3.2 DML（数据操作）

```sql
-- 插入
INSERT INTO users (id, name, age, email) VALUES
    ('u1', 'Alice', 30, 'alice@example.com'),
    ('u2', 'Bob', 25, 'bob@example.com');

-- 批量插入
BATCH INSERT INTO users (id, name, age) VALUES
    ('u3', 'Charlie', 35),
    ('u4', 'Diana', 28),
    ('u5', 'Eve', 42);

-- 更新
UPDATE users SET age = 31 WHERE name = 'Alice';

-- 删除
DELETE FROM users WHERE age < 20;

-- UPSERT (存在则更新，不存在则插入)
INSERT INTO users (id, name, age) VALUES ('u1', 'Alice Updated', 31)
ON CONFLICT (id) DO UPDATE SET name = excluded.name, age = excluded.age;
```

### 3.3 DQL（数据查询）

```sql
-- 基础查询
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;

-- 聚合查询
SELECT age, COUNT(*) as count, AVG(age) as avg_age
FROM users
GROUP BY age
HAVING COUNT(*) > 1;

-- JOIN 查询
SELECT u.name, COUNT(k.to_id) as friend_count
FROM users u
LEFT JOIN knows k ON u.id = k.from_id
GROUP BY u.name;

-- 子查�?
SELECT * FROM users WHERE age > (SELECT AVG(age) FROM users);

-- CTE (公共表表达式)
WITH active_users AS (
    SELECT * FROM users WHERE age > 20
)
SELECT * FROM active_users WHERE name LIKE 'A%';

-- 窗口函数
SELECT name, age,
    ROW_NUMBER() OVER (ORDER BY age DESC) as rank,
    AVG(age) OVER () as avg_age
FROM users;

-- 图遍历查�?
GRAPH TRAVERSE FROM 'users::u1' OUT LABEL 'knows' DEPTH 3;

-- 最短路�?
GRAPH SHORTEST PATH FROM 'users::u1' TO 'users::u5';
```

### 3.4 导入导出

```sql
-- �?CSV 导入
COPY users FROM '/path/to/users.csv' FORMAT CSV HEADER;

-- 导出到文�?
COPY (SELECT * FROM users) TO '/path/to/export.csv' FORMAT CSV;

-- �?JSON 导入
IMPORT users FROM '/path/to/users.json' FORMAT JSON;
```

### 3.5 OntoQL 语法（本体查询语言�?

OntoQL �?OntoDB 的本体查询语言，在 SQL 基础上增加了**类继承、属性语义、三元组操作、本体推�?*能力�?

#### 3.5.1 本体定义

```sql
-- 创建单个类（自动创建同名本体�?
CREATE CLASS Dog

-- 创建带继承的�?
CREATE CLASS Dog EXTENDS Animal

-- 创建完整本体（多个类 + 属�?组织在一起）
CREATE ONTOLOGY BioCompute (
    CLASS BioTask,
    CLASS ScreenResult,
    CLASS ScreenHit EXTENDS MeasurableEntity,
    PROPERTY task_name DOMAIN BioTask RANGE STRING,
    PROPERTY data_type DOMAIN BioTask RANGE STRING,
    PROPERTY status DOMAIN BioTask RANGE STRING,
    PROPERTY value_density DOMAIN BioTask RANGE FLOAT64
)

-- 创建共享基类本体
CREATE ONTOLOGY SharedBase (
    CLASS TimestampedEntity,
    CLASS OwnedEntity EXTENDS TimestampedEntity,
    CLASS MeasurableEntity EXTENDS OwnedEntity,
    PROPERTY created_at DOMAIN TimestampedEntity RANGE FLOAT64,
    PROPERTY owner DOMAIN OwnedEntity RANGE STRING,
    PROPERTY project_id DOMAIN OwnedEntity RANGE STRING,
    PROPERTY value_score DOMAIN MeasurableEntity RANGE FLOAT64
)
```

#### 3.5.2 删除

```sql
-- 删除单个类（对应 CREATE CLASS�?
DROP CLASS Dog

-- 删除整个本体（对�?CREATE ONTOLOGY�?
DROP ONTOLOGY BioCompute
```

**对应关系**：`CREATE CLASS` �?`DROP CLASS`，`CREATE ONTOLOGY` �?`DROP ONTOLOGY`

#### 3.5.3 数据操作（SET 语法�?

```sql
-- INSERT（OntoQL SET 语法，比 SQL VALUES 更简洁）
INSERT INTO BioTask SET
    task_name = 'sample.fastq',
    data_type = 'fastq',
    status = 'completed',
    value_density = 0.75,
    owner = 'lab-01',
    created_at = 1724800000.0

-- SELECT（与 SQL 相同�?
SELECT * FROM BioTask WHERE status = 'completed'
SELECT * FROM BioTask WHERE owner = 'lab-01' ORDER BY value_density DESC

-- UPDATE / DELETE（与 SQL 相同�?
UPDATE BioTask SET status = 'archived' WHERE owner = 'lab-01'
DELETE FROM BioTask WHERE status = 'failed'
```

#### 3.5.4 继承查询

```sql
-- 查询 Animal 会自动返�?Dog、Cat、Bird 等所有子类实�?
SELECT * FROM Animal

-- 查询 Device 会自动返�?Sensor、TempSensor、SmartLight �?
SELECT * FROM Device

-- 查看实体的真实类�?
SELECT name, __class__ FROM Device
```

#### 3.5.5 三元组操作（RDF�?

```sql
-- 插入三元�?
INSERT TRIPLE SET subject = "dog1", predicate = "rdf:type", object = "Dog"
INSERT TRIPLE SET subject = "dog1", predicate = "name", object = "Rex"

-- 批量插入
INSERT TRIPLES (subject, predicate, object) VALUES
    ("dog1", "rdf:type", "Dog"),
    ("dog1", "name", "Rex"),
    ("dog1", "age", "3")

-- 查询三元�?
SELECT TRIPLE
SELECT TRIPLE WHERE subject = "dog1"
SELECT TRIPLE WHERE predicate = "rdf:type" LIMIT 10

-- 删除三元�?
DELETE TRIPLE SET subject = "dog1", predicate = "name", object = "Rex"
```

#### 3.5.6 推理查询

```sql
-- 使用 INFER 关键字开�?OWL 推理
SELECT * FROM Animal INFER @onto(scope=SUBCLASS)

-- 推理会自动展开类层次：
-- Animal �?Mammal �?Dog, Cat
-- Animal �?Bird �?Eagle
-- 查询 Animal 返回所有子类实�?
```

#### 3.5.7 事务

```sql
BEGIN
INSERT INTO BioTask SET task_name = 'txn-test', status = 'new'
UPDATE BioTask SET status = 'committed' WHERE task_name = 'txn-test'
COMMIT

-- 或回�?
BEGIN
DELETE FROM BioTask WHERE task_name = 'txn-test'
ROLLBACK
```

#### 3.5.8 OntoQL vs SQL vs SPARQL 对照

| 操作 | SQL | OntoQL | SPARQL |
|------|-----|--------|--------|
| 建表 | `CREATE ONTOLOGY X (CLASS Y)` | `CREATE CLASS Y` | 不支�?|
| 建库 | `CREATE ONTOLOGY X (...)` | `CREATE ONTOLOGY X (...)` | 不支�?|
| 删表 | 不支�?| `DROP CLASS Y` | 不支�?|
| 删库 | 不支�?| `DROP ONTOLOGY X` | 不支�?|
| 插数�?| `INSERT INTO T (...) VALUES (...)` | `INSERT INTO T SET col=val` | 不支�?|
| 查数�?| `SELECT * FROM T` | `SELECT * FROM T` | `SELECT ?x WHERE {?x rdf:type T}` |
| 继承查询 | 不支�?| `SELECT * FROM Animal`（自动展开�?| `?x rdf:type/rdfs:subClassOf* Animal` |
| 三元�?| 不支�?| `INSERT TRIPLE SET ...` | `INSERT DATA { ... }` |

> 应用层日�?CRUD �?SQL 即可。管理操作（建本�?加属�?DROP）用 OntoQL。SPARQL 适合知识图谱集成�?

---

## 4. 向量搜索

### 4.1 创建向量索引

```sql
-- 创建128维向量索引（余弦相似度）
CREATE VECTOR INDEX ON documents (embedding)
    DIMENSIONS 128
    METRIC cosine;

-- 创建256维向量索引（欧氏距离�?
CREATE VECTOR INDEX ON images (feature_vector)
    DIMENSIONS 256
    METRIC euclidean;
```

### 4.2 向量搜索

```sql
-- 基础向量搜索
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ..., 0.128]
    TOP 10;

-- 带过滤条件的向量搜索
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, 0.3, ..., 0.128]
    TOP 10
    WHERE category = '技�?;
```

### 4.3 混合查询（SQL + 向量�?

```sql
-- SQL 过滤 + 向量排序
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '技�? AND year > 2020
ORDER BY score
LIMIT 5;
```

### 4.4 HTTP API

```bash
# 向量搜索
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "top_k": 10,
    "filter": "category = \"技术\""
  }'

# 混合查询
curl -X POST http://127.0.0.1:7912/api/hybrid/query \
  -H "Content-Type: application/json" \
  -d '{
    "class": "documents",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3],
    "filter": "year > 2020",
    "top_k": 5
  }'
```

---

## 5. 图查�?

### 5.1 创建图结�?

```sql
-- 创建顶点
INSERT VERTEX Person (id, name, age) VALUES ('p1', 'Alice', 30);
INSERT VERTEX Person (id, name, age) VALUES ('p2', 'Bob', 25);

-- 创建�?
INSERT EDGE knows (from_id, to_id, since) VALUES ('p1', 'p2', 2020);
```

### 5.2 图遍�?

```sql
-- BFS 遍历（从 p1 出发�? 跳）
GRAPH TRAVERSE FROM 'Person::p1' OUT LABEL 'knows' DEPTH 3;

-- DFS 遍历
GRAPH TRAVERSE FROM 'Person::p1' OUT DEPTH 5 ALGORITHM dfs;

-- 带过滤的遍历
GRAPH TRAVERSE FROM 'Person::p1' OUT DEPTH 2 WHERE age > 25;

-- 最短路�?
GRAPH SHORTEST PATH FROM 'Person::p1' TO 'Person::p5';
```

### 5.3 HTTP API

```bash
# 图遍�?
curl -X POST http://127.0.0.1:7912/api/graph/traverse \
  -H "Content-Type: application/json" \
  -d '{
    "start": "Person::p1",
    "direction": "out",
    "edge_label": "knows",
    "max_depth": 3,
    "algorithm": "bfs"
  }'

# 最短路�?
curl -X POST http://127.0.0.1:7912/api/graph/shortest-path \
  -H "Content-Type: application/json" \
  -d '{"from": "Person::p1", "to": "Person::p5"}'
```

---

## 6. SPARQL 查询

### 6.1 基础查询

```sparql
-- 查询所�?Person
SELECT ?name ?age
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    ?person :age ?age .
}
ORDER BY ?name
LIMIT 10;
```

### 6.2 过滤查询

```sparql
-- 过滤年龄大于 25 �?Person
SELECT ?name ?age
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    ?person :age ?age .
    FILTER(?age > 25)
}
ORDER BY ?age DESC;
```

### 6.3 OPTIONAL 查询

```sparql
-- 查询 Person 及其可选的邮箱
SELECT ?name ?email
WHERE {
    ?person rdf:type :Person .
    ?person :name ?name .
    OPTIONAL { ?person :email ?email }
};
```

### 6.4 CONSTRUCT 查询

```sparql
-- 构造新�?RDF �?
CONSTRUCT {
    ?person :hasFriend ?friend .
}
WHERE {
    ?person :knows ?friend .
    ?friend :age ?age .
    FILTER(?age > 20)
};
```

### 6.5 ASK 查询

```sparql
-- 检查是否存�?
ASK {
    ?person rdf:type :Person .
    ?person :name "Alice" .
};
```

---

## 7. 本体推理

### 7.1 创建本体

```sql
CREATE ONTOLOGY MyOntology (
    CLASS Animal,
    CLASS Dog SUBCLASS OF Animal,
    CLASS Cat SUBCLASS OF Animal,
    CLASS Pet SUBCLASS OF Animal,
    CLASS GuardDog SUBCLASS OF Dog,
    
    PROPERTY hasName DOMAIN Animal RANGE STRING,
    PROPERTY hasAge DOMAIN Animal RANGE INT,
    PROPERTY belongsTo DOMAIN Pet RANGE Person,
    
    CLASS Person,
    PROPERTY owns DOMAIN Person RANGE Pet
);
```

### 7.2 自动推理

```sql
-- 插入实例
INSERT INTO Dog (hasName, hasAge) VALUES ('旺财', 3);

-- 查询所�?Animal（Dog 自动包含在内�?
SELECT * FROM Animal;
-- 结果包含：旺财（Dog �?Animal 的子类）

-- 查询所�?Pet（Dog 也是 Pet�?
SELECT * FROM Pet;
```

### 7.3 推理规则

OntoDB 支持 7 �?OWL 2 RL 推理规则�?

| 规则 | 说明 | 示例 |
|------|------|------|
| CaxSco | 类继承推�?| Dog �?Animal �?旺财 �?Animal |
| CaxEqc | 等价类推�?| Dog �?Canine �?旺财 �?Canine |
| PrpSpo | 属性继承推�?| hasOwner �?hasBelonging �?传�?|
| PrpEqp | 等价属性推�?| hasName �?getName �?语义别名 |
| PrpInv | 反向属性推�?| owns �?ownedBy |
| PrpTrp | 传递属性推�?| ancestorOf 传�?|
| PrpSymp | 对称属性推�?| friendOf 对称 |

### 7.4 推理解释

```sql
-- 查看推导�?
EXPLAIN SELECT * FROM Animal WHERE hasName = '旺财';

-- 输出�?
-- 旺财 �?Dog (直接断言)
-- Dog �?Animal (本体规则)
-- �?旺财 �?Animal (推导)
```

---

## 8. 事务管理

### 8.1 基础事务

```sql
-- 开始事�?
BEGIN;

-- 执行操作
INSERT INTO users (name, age) VALUES ('Alice', 30);
UPDATE accounts SET balance = balance - 100 WHERE user = 'Alice';

-- 提交事务
COMMIT;

-- 或回�?
ROLLBACK;
```

### 8.2 隔离级别

OntoDB 使用 **快照隔离**（Snapshot Isolation）：
- 每个事务看到一致的数据快照
- 写入冲突时自动回�?
- 适合读多写少的场�?

---

## 9. 备份恢复

### 9.1 全量备份

```sql
-- SQL 方式
BACKUP TO '/backups/full-2026-08-10.ontodb';

-- HTTP API
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/full-2026-08-10.ontodb"}'
```

### 9.2 增量备份

```sql
-- 增量备份（仅备份变更部分�?
BACKUP INCREMENTAL TO '/backups/incr-2026-08-10.ontodb';
```

### 9.3 恢复

```sql
-- SQL 方式
RESTORE FROM '/backups/full-2026-08-10.ontodb';

-- HTTP API
curl -X POST http://127.0.0.1:7912/api/restore \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/full-2026-08-10.ontodb"}'
```

### 9.4 自动备份脚本

```bash
#!/bin/bash
# 每日凌晨 2 点自动备�?
BACKUP_DIR="/backups/ontodb"
DATE=$(date +%Y%m%d)
curl -X POST http://127.0.0.1:7912/api/backup \
  -H "Content-Type: application/json" \
  -d "{\"path\": \"$BACKUP_DIR/backup-$DATE.ontodb\"}"
```

---

## 10. 安全配置

### 10.1 API Key 认证

```bash
# 启动时启用认�?
./ontodb-server --auth --api-key your-secret-key

# 使用 API Key 访问
curl -H "Authorization: Bearer your-secret-key" \
  http://127.0.0.1:7912/api/query \
  -d '{"query": "SELECT * FROM users"}'
```

### 10.2 TLS/HTTPS

```bash
# 使用证书启动
./ontodb-server --tls-cert cert.pem --tls-key key.pem

# 自签名证书（开发环境）
./ontodb-server --tls-cert self-signed.crt --tls-key self-signed.key
```

### 10.3 IP 白名�?

```json
// config/api_keys.json
{
  "keys": [
    {
      "key": "your-api-key",
      "name": "Admin",
      "permissions": ["read", "write", "admin"],
      "ip_whitelist": ["192.168.1.0/24", "10.0.0.1"]
    }
  ]
}
```

### 10.4 存储加密

```bash
# 使用环境变量存储主密�?
export ONTO_MASTER_KEY=$(openssl rand -hex 32)
./ontodb-server --encryption-enabled --master-key-source env:ONTO_MASTER_KEY

# 使用 KMS
./ontodb-server --encryption-enabled \
  --master-key-source "kms|https://vault.example.com/v1/transit|ontodb-master|hvs.xxx"
```

### 10.5 RBAC（企业版�?

```sql
-- 创建角色
CREATE ROLE SystemAdmin;
CREATE ROLE SecurityAdmin;
CREATE ROLE AuditAdmin;

-- 分配权限
GRANT ALL ON * TO SystemAdmin;
GRANT READ ON * TO SecurityAdmin;
GRANT SELECT ON audit_logs TO AuditAdmin;

-- 分配用户角色
GRANT SystemAdmin TO user 'admin';
```

---

## 11. 性能调优

### 11.1 配置参数

```toml
# ontodb.toml

[data]
dir = "/data/ontodb"

[performance]
# MemTable 大小（增大可减少 flush 频率�?
memtable_size_mb = 128

# Block Cache 大小（增大可提升读取性能�?
block_cache_mb = 512

# WAL fsync 策略
sync_wal_on_commit = false  # 设为 true 可保证持久性，但降低性能

# 后台压缩线程�?
compaction_threads = 4

[server]
# 连接池大�?
max_connections = 1000

# 请求超时
request_timeout_secs = 30
```

### 11.2 基准测试

```bash
# 运行基准测试
cargo bench --bench lock_contention -p onto-storage
cargo bench --bench batch_import -p onto-storage

# 预期结果�?
# 写入: 767,561 ops/s
# 读取: 1,176,147 ops/s
# 批量写入: 967,453 ops/s
```

### 11.3 监控指标

```bash
# Prometheus 指标
curl http://127.0.0.1:7912/metrics

# JSON 指标
curl http://127.0.0.1:7912/api/metrics

# 关键指标�?
# - ontodb_queries_total: 总查询数
# - ontodb_query_latency: 查询延迟分布
# - ontodb_cache_hits: 缓存命中�?
# - ontodb_memtable_size_bytes: MemTable 大小
# - ontodb_disk_usage_bytes: 磁盘使用�?
```

---

## 12. 故障排查

### 12.1 常见问题

| 问题 | 原因 | 解决方案 |
|------|------|---------|
| 连接被拒�?| 服务器未启动或端口错�?| 检�?`ps aux | grep ontodb` 和端�?|
| 查询超时 | 查询太复杂或数据量太�?| 添加 LIMIT，优�?WHERE 条件 |
| 内存不足 | MemTable �?Cache 太大 | 减小 `memtable_size_mb` �?`block_cache_mb` |
| 磁盘�?| WAL �?SSTable 累积 | 清理旧数据或扩容磁盘 |
| 认证失败 | API Key 错误 | 检�?`--api-key` 配置 |

### 12.2 日志查看

```bash
# 启用调试日志
RUST_LOG=debug ./ontodb-server --data-dir ./data

# 查看特定模块日志
RUST_LOG=onto_storage=debug,onto_query=info ./ontodb-server
```

### 12.3 健康检�?

```bash
# 健康检�?
curl http://127.0.0.1:7912/api/health
# {"status": "ok", "version": "0.3.0"}

# 就绪检查（K8s�?
curl http://127.0.0.1:7912/api/health/ready

# 存活检查（K8s�?
curl http://127.0.0.1:7912/api/health/live
```

---

## 附录

### A. 错误�?

| 错误�?| 说明 | 处理建议 |
|--------|------|---------|
| 400 | 请求格式错误 | 检�?JSON 格式 |
| 401 | 认证失败 | 检�?API Key |
| 429 | 速率限制 | 等待或禁用限�?|
| 500 | 服务器内部错�?| 查看日志 |

### B. 端口说明

| 端口 | 协议 | 说明 |
|------|------|------|
| 7912 | HTTP | REST API |
| 7913 | TCP | PostgreSQL Wire Protocol |
| 7914 | TCP | MySQL Wire Protocol |

### C. 数据类型

| 类型 | 说明 | 示例 |
|------|------|------|
| STRING | 字符�?| `'hello'` |
| INT | 64位整�?| `42` |
| DOUBLE | 64位浮点数 | `3.14` |
| BOOL | 布尔�?| `TRUE` / `FALSE` |
| ARRAY | 数组 | `[1, 2, 3]` |
| JSON | JSON 对象 | `{"key": "value"}` |
| BLOB | 二进制数�?| `'\x010203'` |
