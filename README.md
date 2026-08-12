# OntoDB — 本体驱动的六模态语义数据库

**[English](README.en.md)** | 中文

<p align="center">
  <b>全球首个将 OWL 推理引擎嵌入数据库内核的六模态统一语义数据库</b>
</p>

---

## 核心特性

| 特性 | 说明 |
|------|------|
| **六模态统一存储** | 关系型 + 图 + 向量 + 时序 + 空间 + 本体，一套 API 统一查询 |
| **数据插入即生成语义** | INSERT 时自动执行 7 步联动：文档写入 → 图顶点 → rdf:type 三元组 → 属性三元组 → OWL 推理 → 向量索引 → B+Tree 索引 |
| **实时本体推理** | 7 条 OWL 2 RL 规则，增量不动点算法，推导链可追溯 |
| **语义向量混合查询** | HNSW 向量索引 + SQL/SPARQL 联合查询 |
| **自适应内存管理** | MemTable 4-256MB 动态调整，Block Cache 16MB-1GB 自适应，系统压力自动收缩 |
| **全域组提交** | put/commit_txn/put_batch 全路径组提交，混合批量策略 |
| **内存安全** | 纯 Rust 实现，零 unsafe 代码块，430 处 expect |
| **PostgreSQL/MySQL 兼容** | 支持 PG Wire 和 MySQL 协议，现有客户端直接连接 |

## 性能基准

| 指标 | 数值 |
|------|------|
| 写入吞吐量 | 863,618 ops/s |
| 读取吞吐量 | 1,256,518 ops/s |
| 批量写入 | 1,082,230 ops/s |
| HNSW 向量召回率 | 100%（ef_search=200，延迟 341µs） |
| 8线程并发加速 | 1.82x |
| GIS 空间关系判断 | ≤8µs |
| TSM 压缩率 | 60%+ |

## 快速开始

### 方式一：下载预编译二进制

```bash
# Linux x86_64
wget https://release.ontodb.io/ontodb-v0.6.0-linux-x86_64.tar.gz
tar xzf ontodb-v0.6.0-linux-x86_64.tar.gz
cd ontodb-v0.6.0

# 启动服务器
./ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

```powershell
# Windows x86_64
Invoke-WebRequest -Uri "https://release.ontodb.io/ontodb-v0.6.0-windows-x86_64.zip" -OutFile ontodb.zip
Expand-Archive ontodb.zip -DestinationPath .
cd ontodb-v0.6.0

# 启动服务器
.\ontodb-server.exe --data-dir .\data --http 0.0.0.0:7912
```

### 方式二：从源码编译

```bash
# 前置要求：Rust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# 二进制文件位于 target/release/
ls target/release/ontodb-server*
```

### 方式三：一键安装脚本

```bash
# Linux/macOS
curl -fsSL https://get.ontodb.io | bash

# Windows (PowerShell)
irm https://get.ontodb.io/install.ps1 | iex
```

## 启动服务器

```bash
# 基本启动
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# 启用认证
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --auth --api-key your-secret-key

# 启用 TLS
./ontodb-server --data-dir ./data --http 0.0.0.0:7912 --tls-cert cert.pem --tls-key key.pem

# 禁用速率限制（开发/测试）
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit

# 启用 Raft 集群
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --raft --raft-id 1 --raft-peers "2@10.0.0.2:7913,3@10.0.0.3:7913"
```

### 命令行参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--data-dir` | 数据目录 | `./data` |
| `--http` | HTTP 监听地址 | `127.0.0.1:7912` |
| `--auth` | 启用 API Key 认证 | `false` |
| `--api-key` | API 密钥 | 自动生成 |
| `--no-rate-limit` | 禁用速率限制 | `false` |
| `--tls-cert` | TLS 证书文件 | 无 |
| `--tls-key` | TLS 私钥文件 | 无 |
| `--raft` | 启用 Raft 集群 | `false` |
| `--raft-id` | 节点 ID | `1` |
| `--raft-peers` | 集群节点列表 | 无 |
| `--encryption-enabled` | 启用加密存储 | `false` |
| `--audit-retention-days` | 审计日志保留天数 | `180` |

## 访问方式

### HTTP REST API

```bash
# 健康检查
curl http://127.0.0.1:7912/api/health

# 执行 SQL
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users LIMIT 10"}'

# 向量搜索
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{"class": "documents", "column": "embedding", "query_vector": [0.1, 0.2, ...], "top_k": 5}'

# SPARQL 查询
curl -X POST http://127.0.0.1:7912/api/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT ?x WHERE { ?x rdf:type :Person }"}'
```

### Web 控制台

打开浏览器访问：`http://127.0.0.1:7912/console`

### PostgreSQL 客户端

```bash
# 使用 psql 连接
psql -h 127.0.0.1 -p 7913 -U ontodb

# 使用任何 PostgreSQL 驱动连接
# JDBC: jdbc:postgresql://127.0.0.1:7913/ontodb
# Python: psycopg2.connect(host='127.0.0.1', port=7913)
```

### MySQL 客户端

```bash
# 使用 mysql 客户端连接
mysql -h 127.0.0.1 -P 7914 -u ontodb
```

### CLI 工具

```bash
# 交互式 REPL
./ontodb-cli 127.0.0.1:7912

# 单次查询
./ontodb-cli 127.0.0.1:7912 -q "SELECT * FROM users"

# 执行脚本文件
./ontodb-cli 127.0.0.1:7912 -f init.sql
```

## SQL 语法示例

### 基础 CRUD

```sql
-- 创建表
CREATE VERTEX TABLE users (name STRING, age INT, email STRING);

-- 插入数据
INSERT INTO users (name, age, email) VALUES ('张三', 30, 'zhangsan@example.com');

-- 查询
SELECT * FROM users WHERE age > 25 ORDER BY name LIMIT 10;

-- 更新
UPDATE users SET age = 31 WHERE name = '张三';

-- 删除
DELETE FROM users WHERE name = '张三';
```

### 向量搜索

```sql
-- 创建向量索引
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;

-- 向量搜索
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2, 0.3, ...] TOP 10;

-- 混合查询（SQL + 向量）
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '技术'
ORDER BY score
LIMIT 5;
```

### 图查询

```sql
-- 创建图
CREATE VERTEX TABLE Person (name STRING);
CREATE EDGE TABLE knows (from_id STRING, to_id STRING, since INT);

-- 图遍历
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;

-- 最短路径
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

### SPARQL

```sparql
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX ex: <http://example.org/>

SELECT ?name ?age
WHERE {
  ?person rdf:type ex:Employee .
  ?person ex:name ?name .
  ?person ex:age ?age .
  FILTER(?age > 30)
}
ORDER BY ?name
LIMIT 10
```

### 本体推理

```sql
-- 创建本体
CREATE ONTOLOGY MyOntology (
  CLASS Animal,
  CLASS Dog SUBCLASS OF Animal,
  CLASS Cat SUBCLASS OF Animal,
  PROPERTY hasName DOMAIN Animal RANGE STRING
);

-- 插入实例
INSERT INTO Dog (hasName) VALUES ('旺财');

-- 自动推理：查询所有 Animal（Dog 自动包含在内）
SELECT * FROM Animal;
```

## 企业版功能

企业版通过 feature flags 启用：

```bash
# 编译企业标准版（集群+分片+备份）
cargo build --release --features enterprise-standard

# 编译政府/金融版（全部功能）
cargo build --release --features enterprise-gov
```

| 功能 | 社区版 | 企业标准版 | 政府/金融版 |
|------|--------|-----------|------------|
| 核心存储引擎 | ✅ | ✅ | ✅ |
| SQL/SPARQL 查询 | ✅ | ✅ | ✅ |
| 向量索引 | ✅ | ✅ | ✅ |
| 图遍历 | ✅ | ✅ | ✅ |
| 本体推理 | ✅ | ✅ | ✅ |
| HTTP API | ✅ | ✅ | ✅ |
| PG/MySQL 协议 | ✅ | ✅ | ✅ |
| 全量备份 | ✅ | ✅ | ✅ |
| 增量备份 | ❌ | ✅ | ✅ |
| Raft 集群 | ❌ | ✅ | ✅ |
| 数据分片 | ❌ | ✅ | ✅ |
| SM4/AES 加密 | ❌ | ❌ | ✅ |
| RBAC 三权分立 | ❌ | ❌ | ✅ |
| 等保2.0审计 | ❌ | ❌ | ✅ |
| 数据脱敏 | ❌ | ❌ | ✅ |
| 滚动升级 | ❌ | ❌ | ✅ |

## 项目结构

```
ontodb/
├── crates/
│   ├── onto-core/          # 核心类型和错误定义
│   ├── onto-storage/       # 存储引擎（LSM-Tree + B+Tree + HNSW + TSM）
│   ├── onto-query/         # 查询引擎（SQL解析 + SPARQL翻译 + 优化器）
│   ├── onto-graph/         # 图引擎（邻接表 + BFS/DFS + 最短路径）
│   ├── onto-ontology/      # 本体引擎（OWL推理 + 三元组存储 + RDF）
│   ├── onto-server/        # HTTP/TCP/PG/MySQL 服务器
│   ├── onto-enterprise/    # 企业版功能（加密/RBAC/审计/备份）
│   ├── onto-raft/          # Raft 共识层
│   ├── onto-cli/           # 命令行工具
│   └── onto-sharding/      # 分片（开发中）
├── benches/                # 性能基准测试
├── tests/                  # 集成测试
└── docs/                   # 文档和专利
```

## API 端点一览

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/health` | 健康检查 |
| GET | `/api/health/ready` | 就绪探针（K8s） |
| GET | `/api/health/live` | 存活探针（K8s） |
| GET | `/api/metrics` | JSON 指标 |
| GET | `/metrics` | Prometheus 指标 |
| POST | `/api/query` | 执行 SQL |
| POST | `/api/vector/search` | 向量相似度搜索 |
| POST | `/api/hybrid/query` | 混合查询（SQL + 向量） |
| POST | `/api/sparql` | SPARQL 查询 |
| GET | `/api/schema` | Schema 自省 |
| POST | `/api/backup` | 全量备份 |
| POST | `/api/backup/incremental` | 增量备份 |
| POST | `/api/restore` | 恢复备份 |
| GET | `/api/cluster` | 集群状态 |
| GET | `/api/docs` | Swagger UI |
| GET | `/console` | Web 控制台 |
| GET | `/digital-twin` | 数字孪生大屏 |
| GET | `/digital-advisor` | 数字军师系统 |

## 许可证

- **社区版**：Apache License 2.0
- **企业版**：商业许可（见 `crates/onto-enterprise/COMMERCIAL_LICENSE.md`）

## 联系我们

- 官网：https://ontodb.io
- 文档：https://docs.ontodb.io
- GitHub：https://github.com/ontodb/ontodb
- 邮箱：contact@ontodb.io
