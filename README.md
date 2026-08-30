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

### 方式一：从源码编译

```bash
# 前置要求：Rust 1.70+
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# 启动服务器
./target/release/ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

### 方式二：Docker

```bash
docker run -p 7912:7912 ontodb/ontodb-server --data-dir /data --http 0.0.0.0:7912
```

## 启动服务器

```bash
# 基本启动
./ontodb-server --data-dir ./data --http 127.0.0.1:7912

# 启用认证
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --auth --api-key your-secret-key

# 禁用速率限制（开发/测试）
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --no-rate-limit
```

### 命令行参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--data-dir` | 数据目录 | `./data` |
| `--http` | HTTP 监听地址 | `127.0.0.1:7912` |
| `--auth` | 启用 API Key 认证 | `false` |
| `--api-key` | API 密钥 | 自动生成 |
| `--no-rate-limit` | 禁用速率限制 | `false` |

## 访问方式

### HTTP REST API

```bash
# 健康检查
curl http://127.0.0.1:7912/api/health

# 执行 OntoQL
curl -X POST http://127.0.0.1:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT * FROM users LIMIT 10"}'

# 向量搜索
curl -X POST http://127.0.0.1:7912/api/vector/search \
  -H "Content-Type: application/json" \
  -d '{"class": "documents", "column": "embedding", "query_vector": [0.1, 0.2], "top_k": 5}'
```

### Web 控制台

打开浏览器访问：`http://127.0.0.1:7912`

### SDK

| 语言 | 目录 |
|------|------|
| Python | `sdk/python/` |
| JavaScript/TypeScript | `sdk/javascript/` / `sdk/typescript/` |
| Go | `sdk/go/` |
| Java | `sdk/java/` |

## SQL 语法示例

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
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2, 0.3] TOP 10;
```

### 图查询

```sql
-- 图遍历
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;

-- 最短路径
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5';
```

## 项目结构

```
ontodb/
├── crates/
│   ├── onto-core/          # 核心类型、Trait
│   ├── onto-storage/       # LSM-Tree 存储引擎
│   ├── onto-query/         # 查询引擎（SQL/OntoQL）
│   ├── onto-graph/         # 图数据模型
│   ├── onto-ontology/      # 本体推理引擎
│   ├── onto-server/        # HTTP 服务器
│   ├── onto-cli/           # 命令行工具
│   ├── onto-enterprise/    # 企业版功能
│   ├── onto-raft/          # Raft 分布式
│   ├── onto-sharding/      # 数据分片
│   ├── onto-plugin/        # 插件框架
│   ├── onto-edge/          # 边缘设备
│   └── onto-edge-esp32/    # ESP32 支持
├── sdk/                    # 多语言 SDK
├── examples/               # 示例应用
├── docs/                   # 文档
└── frontend/               # Web 控制台
```

## 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

OntoDB 采用双许可证模式：

| 版本 | 许可证 | 说明 |
|------|--------|------|
| **社区版** | AGPL-3.0 | 免费使用，修改后需开源 |
| **企业版** | 商业许可 | 生产环境使用，需购买许可证 |

**社区版（AGPL-3.0）**：
- 可以免费使用、修改、分发
- 如果提供网络服务，修改后的代码必须开源
- 详见 [LICENSE](LICENSE)

**企业版（商业许可）**：
- 生产环境使用
- 不需要开源修改后的代码
- 包含技术支持和 SLA
- 详见 [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- 联系方式：license@ontodb.ai
