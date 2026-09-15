# OntoDB — 本体驱动的六模态语义数据库

**[English](README.en.md)** | 中文

**全球首个将 OWL 推理引擎嵌入数据库内核的多模态语义数据库**

---

## 版本对比

| 功能 | 社区版（开源） | 企业版（商业） |
|------|--------------|---------------|
| **存储引擎** | ✅ LSM-Tree | ✅ LSM-Tree |
| **六模态统一存储** | ✅ 关系+图+向量+时序+空间+本体 | ✅ 同社区版 |
| **SQL/OntoQL 查询** | ✅ | ✅ |
| **HNSW 向量索引** | ✅ | ✅ |
| **基础 OWL 推理** | ✅ 7条规则+增量不动点 | ✅ 同社区版 |
| **推理链追溯** | ✅ | ✅ |
| **七步写入联动** | ✅ | ✅ |
| **统一实体标识** | ✅ | ✅ |
| **PostgreSQL/MySQL 兼容** | ✅ | ✅ |
| **边缘设备支持** | ✅ onto-edge | ✅ onto-edge |
| 自定义规则引擎 | ❌ | ✅ |
| 规则热更新 | ❌ | ✅ |
| 分布式推理 | ❌ | ✅ |
| 大批量推理优化 | ❌ | ✅ |
| 数据加密 (AES-256) | ❌ | ✅ |
| RBAC 权限控制 | ❌ | ✅ |
| 审计日志 | ❌ | ✅ |
| 数据脱敏 | ❌ | ✅ |
| Raft 分布式复制 | ❌ | ✅ |
| 数据分片 | ❌ | ✅ |
| 增量备份/恢复 | ❌ | ✅ |
| 滚动升级 | ❌ | ✅ |
| LDAP 集成 | ❌ | ✅ |
| 可观测性 | ❌ | ✅ |
| CRC 数据校验 | ❌ | ✅ |

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
git clone https://github.com/ontodb/ontodb.git
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
│   ├── onto-core/          # 核心类型、Trait
│   ├── onto-storage/       # LSM-Tree 存储引擎
│   ├── onto-ontology/      # 本体推理引擎（开源）
│   ├── onto-query/         # 查询引擎（SQL/OntoQL）
│   ├── onto-graph/         # 图数据模型
│   ├── onto-server/        # HTTP 服务器
│   ├── onto-cli/           # 命令行工具
│   ├── onto-plugin/        # 插件框架
│   ├── onto-edge/          # 边缘设备
│   ├── onto-edge-esp32/    # ESP32 支持
│   └── onto-enterprise/    # 企业版功能（不开源）
├── sdk/                    # 多语言 SDK
├── examples/               # 示例应用
├── docs/                   # 文档
└── frontend/               # Web 控制台
```

---

## 许可证

OntoDB 采用双许可模式：

| 版本 | 许可证 | 说明 |
|------|--------|------|
| **社区版** | AGPL-3.0 | 免费使用，修改后需开源 |
| **企业版** | 商业许可 | 生产环境使用，需购买许可证 |

**社区版（AGPL-3.0）：**
- 可以免费使用、修改、分享
- 如果提供网络服务，修改后的代码必须开源
- 包含完整的基础推理引擎（7条OWL规则+增量不动点）
- 详见 [LICENSE](LICENSE)

**企业版（商业许可）：**
- 生产环境使用
- 不需要开源修改后的代码
- 包含高级推理（自定义规则/热更新/分布式推理）
- 包含安全合规（加密/RBAC/审计）
- 包含高可用（Raft/分片/备份）
- 详见 [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- 联系方式：license@ontovalue.com

---

## 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md)。
