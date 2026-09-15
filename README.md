# OntoDB — 本体驱动的六模态语义数据库

**[English](README.en.md)** | 中文

**全球首个将 OWL 推理引擎嵌入数据库内核的多模态语义数据库**

---

## 版本对比

| 功能 | 社区版（BUSL-1.1） | 企业版（商业许可） |
|------|-------------------|------------------|
| **存储引擎** | ✅ LSM-Tree + MVCC + WAL | ✅ 同社区版 |
| **六模态统一存储** | ✅ 关系+图+向量+时序+空间+本体 | ✅ 同社区版 |
| **SQL/OntoQL 查询** | ✅ | ✅ |
| **SPARQL** | ✅ | ✅ |
| **HNSW 向量索引** | ✅ | ✅ |
| **OWL 2 RL 推理** | ✅ 传递闭包+子类传播+对称/逆属性 | ✅ 同社区版 |
| **推理链追溯** | ✅ | ✅ |
| **七步写入联动** | ✅ | ✅ |
| **PostgreSQL/MySQL 兼容** | ✅ | ✅ |
| **TLS 安全传输** | ✅ | ✅ |
| **审计完整性链** | ✅ | ✅ |
| **CDC 变更数据捕获** | ✅ | ✅ |
| **数据分片** | ✅ 类/范围/哈希 | ✅ 企业级分片+跨分片查询 |
| **Raft 分布式复制** | ✅ | ✅ |
| **基本规则API** | ✅ DSL解析+CRUD+热更新 | ✅ 同社区版 |
| **边缘设备支持** | ✅ onto-edge | ✅ onto-edge |
| **插件框架** | ✅ | ✅ |
| 高级推理引擎 | ❌ | ✅ 完整DSL+OWL集成+分布式推理+性能分析 |
| 集群自动故障转移 | ❌ | ✅ |
| 跨分片查询 | ❌ | ✅ 分布式聚合 |
| 慢查询监控 | ❌ | ✅ 可观测性 |
| 全量备份 | ❌ | ✅ |
| RBAC 三权分立 | ❌ | ✅ |
| LDAP/SAML 集成 | ❌ | ✅ |
| 数据脱敏 | ❌ | ✅ |
| SM4/AES 加密 | ❌ | ✅ |
| KMS 密钥管理 | ❌ | ✅ |
| 审计日志轮转+保留 | ❌ | ✅ |
| CRC 数据校验 | ❌ | ✅ |
| 滚动升级 | ❌ | ✅ |

---

## 性能基准

| 指标 | 数值 |
|------|------|
| 写入吞吐量 | 863,618 ops/s |
| 读取吞吐量 | 1,256,518 ops/s |
| 批量写入 | 1,082,230 ops/s |
| HNSW 向量召回率 | 100%（ef_search=200，延迟341µs） |
| 8线程并发加速 | 1.82x |
| GIS 空间关系判断 | ≤1µs |
| TSM 压缩率 | 60%+ |
| OWL 推理（10万三元组） | 5ms |

---

## 快速开始

### 从源码编译

```bash
# 前置要求：Rust 1.70+
git clone https://gitee.com/ontovalue/ontodb.git
cd ontodb
cargo build --release

# 启动服务器
./target/release/ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

### Docker

```bash
docker run -p 7912:7912 ontodb/ontodb-server --data-dir /data --http 0.0.0.0:7912
```

---

## 核心能力

### 1. 六模态统一存储

```sql
-- 关系型
CREATE VERTEX TABLE users (name STRING, age INT);
INSERT INTO users (name, age) VALUES ('Alice', 30);

-- 图遍历
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;

-- 向量搜索
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2] TOP 10;

-- 时序查询
SELECT * FROM sensor_data WHERE time > '2024-01-01' LIMIT 100;

-- 空间查询
SELECT * FROM pois WHERE ST_Distance(location, ST_Point(116.4, 39.9)) < 1000;
```

### 2. 本体内核推理

```sql
-- 定义本体
CREATE CLASS Device;
CREATE CLASS Sensor SUBCLASS OF Device;
CREATE CLASS TemperatureSensor SUBCLASS OF Sensor;

-- 插入实例
INSERT INTO TemperatureSensor (id, location) VALUES ('T1', 'Factory_A');

-- 查询时自动推理展开
SELECT * FROM Device;  -- 自动包含所有子类实例
```

### 3. 七步写入联动

一次 INSERT 自动完成：
1. 文档写入
2. 图顶点创建
3. rdf:type 三元组生成
4. 属性三元组生成
5. OWL 推理
6. 向量索引更新
7. B+Tree 索引更新

---

## 项目结构

```
ontodb/
├── crates/
│   ├── onto-core/          # 核心类型、空间索引、时间序列
│   ├── onto-storage/       # LSM-Tree 存储引擎（WAL/MVCC/HNSW/TSM）
│   ├── onto-ontology/      # OWL 2 RL 推理引擎
│   ├── onto-query/         # 查询引擎（SQL/OntoQL/SPARQL）
│   ├── onto-graph/         # 图数据模型+遍历
│   ├── onto-server/        # HTTP/PG/MySQL 服务器
│   ├── onto-cli/           # 命令行工具
│   ├── onto-plugin/        # 插件框架
│   ├── onto-edge/          # 边缘设备 SDK
│   ├── onto-sharding/      # 数据分片
│   ├── onto-raft/          # Raft 共识
│   └── ontodb-rules/       # 基本规则 API 服务
├── sdk/                    # 多语言 SDK（Go/Python/TypeScript）
├── examples/               # 示例应用+规则模板
├── frontend/               # 规则编辑器 Web UI
└── docs/                   # 文档
```

---

## 许可证

OntoDB 采用双许可模式：

| 版本 | 许可证 | 说明 |
|------|--------|------|
| **社区版** | BUSL-1.1 | 免费使用，禁止提供DBaaS托管服务，2031-09-15转Apache-2.0 |
| **企业版** | 商业许可 | 全功能，需购买许可证 |

**社区版（BUSL-1.1）：**
- 允许：内部使用、本地部署、二次开发、非商业分发
- 允许：企业自用、SaaS产品中嵌入使用
- 禁止：提供OntoDB作为云数据库服务（DBaaS）
- 2031-09-15自动转为 Apache License 2.0
- 详见 [LICENSE](LICENSE)

**企业版（商业许可）：**
- 高级推理引擎（完整DSL+OWL集成+分布式推理+性能分析）
- 安全合规（RBAC+LDAP+SM4/AES加密+KMS+审计保留）
- 高可用（集群故障转移+跨分片查询+备份恢复）
- 详见 [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- 联系方式：license@ontovalue.com

---

## 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md)。
