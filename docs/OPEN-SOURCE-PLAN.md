# OntoDB 开源规划

## 核心原则

- **开源社区版**：让用户 30 秒跑起来，形成用户基础
- **保留企业版**：推理引擎 + 分布式 + 安全合规 = 付费功能
- **专利保护方法**：不管代码开不开源，专利都保护你的方法

---

## 开源模块（Apache 2.0）

| 模块 | 说明 | 开源理由 |
|------|------|---------|
| **onto-core** | 核心类型、Trait、错误定义 | 基础依赖，必须开 |
| **onto-storage** | LSM-Tree 存储引擎 | 让用户能跑起来，性能数据可验证 |
| **onto-query** | SQL 解析器、优化器、执行器 | 基础查询能力，吸引用户 |
| **onto-graph** | 图数据模型、遍历引擎 | 图查询是基础能力 |
| **onto-server** | 服务器主程序 | 让用户能部署 |
| **onto-cli** | 命令行工具 | 降低使用门槛 |
| **onto-plugin** | 插件框架 | 生态建设需要 |

**开源后用户能做什么：**
- SQL 查询（SELECT/INSERT/UPDATE/DELETE）
- 图遍历（GRAPH MATCH）
- 向量搜索（HNSW）
- 单机部署
- 插件扩展

**开源后用户不能做什么：**
- 本体推理（OWL 推理引擎不开）
- 活数据（衰减/激活/价值评分不开）
- 分布式集群（Raft 不开）
- 数据加密/RBAC/审计（企业版不开）

---

## 保留模块（商业许可）

| 模块 | 说明 | 保留理由 |
|------|------|---------|
| **onto-ontology** | OWL 推理引擎、本体模型、推理存储 | **核心差异化**，PostgreSQL/Milvus 都没有 |
| **onto-enterprise** | 企业版功能（见下方明细） | 付费转化的核心 |
| **onto-raft** | Raft 分布式复制 | 企业级部署必需 |
| **onto-sharding** | 数据分片 | 大规模部署必需 |

### onto-enterprise 保留的功能明细

| 功能 | 说明 | 为什么保留 |
|------|------|-----------|
| **encryption** | 数据加密（AES-256） | 金融/政府客户必需，付费点 |
| **rbac** | 角色权限控制 | 企业安全合规 |
| **audit_retention** | 审计日志 | 合规要求 |
| **data_masking** | 数据脱敏 | 隐私保护 |
| **backup** | 增量备份/恢复 | 数据安全 |
| **cluster** | 集群管理 | 高可用 |
| **cluster_router** | 集群路由 | 分布式查询 |
| **cross_shard** | 跨分片查询 | 大规模场景 |
| **data_migration** | 数据迁移 | 上线必需 |
| **rolling_upgrade** | 滚动升级 | 运维便利 |
| **observability** | 可观测性 | 监控告警 |
| **crc_validation** | CRC 数据校验 | 数据完整性 |
| **kms** | 密钥管理 | 安全合规 |
| **ldap** | LDAP 集成 | 企业认证 |

---

## 边缘版本（开源，但有限制）

| 模块 | 说明 | 策略 |
|------|------|------|
| **onto-edge** | 边缘设备运行时 | 开源，吸引 IoT 用户 |
| **onto-edge-bin** | 边缘二进制 | 开源 |
| **onto-edge-esp32** | ESP32 支持 | 开源，社区贡献 |

边缘版本开源自的是吸引 IoT/嵌入式用户，他们未来可能升级到企业版。

---

## 开源后的商业版 SKU

| 版本 | 价格 | 包含内容 |
|------|------|---------|
| **社区版** | 免费（Apache 2.0） | 存储 + 查询 + 图 + 向量 + 单机 |
| **推理版** | ¥50-100万/年 | 社区版 + OWL 推理引擎 + 活数据 |
| **企业标准版** | ¥200-500万/年 | 推理版 + Raft 集群 + 分片 + 加密 + RBAC |
| **企业旗舰版** | ¥500-1000万/年 | 企业版 + Studio + 专属支持 + SLA |

---

## 开源节奏

| 阶段 | 时间 | 动作 |
|------|------|------|
| **第1周** | 现在 | 清理代码，写 README，配置 CI |
| **第2周** | +1周 | 开源社区版（onto-core/storage/query/graph/server/cli） |
| **第3周** | +2周 | 写技术文章，发掘金/知乎/公众号 |
| **第1个月** | +4周 | 在 GitHub Issues 收集反馈，找试用用户 |
| **第2-3个月** | +8周 | 找到1-2个深度用户，推动付费转化 |
| **第6个月** | +24周 | 用案例去融种子轮 |

---

## 代码清理清单

开源前需要做的：

- [x] 删除所有 `test_data/` 目录下的临时文件
- [x] 删除所有 `*.py` 调试脚本（27 个）
- [x] 删除 `server.log`、`stderr.txt`、`stdout.txt`
- [x] 删除内部文档草稿（可行性分析、任务检查点等）
- [x] 配置 Feature Flag（运行时方案）
- [ ] 清理 `Cargo.lock` 中的本地路径依赖
- [ ] 写英文 README（国际用户）
- [ ] 写中文 README（国内用户）
- [ ] 写 Quick Start（30秒跑起来）
- [ ] 写 CONTRIBUTING.md
- [ ] 配置 GitHub Actions CI
- [ ] 添加 LICENSE 文件（Apache 2.0）

---

## Feature Flag 设计

采用**运行时 feature flag**，而非编译时。所有代码全量编译，功能是否启用由 feature flag 控制。

```toml
# Cargo.toml
[features]
default = ["ontology", "enterprise", "raft", "sharding"]

# Feature flags control RUNTIME behavior, not compilation.
# Community edition ships with default features disabled.
ontology = []
raft = []
sharding = []
enterprise = []
enterprise-standard = ["enterprise"]
enterprise-gov = ["enterprise"]
```

### 构建方式

```bash
# 社区版（不启用企业功能）
cargo build --release --no-default-features

# 企业标准版
cargo build --release --features enterprise-standard

# 企业政务/金融版
cargo build --release --features enterprise-gov
```

### 运行时控制

社区版启动时，enterprise/raft/sharding/ontology 模块虽然编译了，但运行时检查 feature flag，未启用的功能不会激活。企业版需要 license key 才能启用完整功能。

### 为什么用运行时而不是编译时

| 方案 | 优点 | 缺点 |
|------|------|------|
| **编译时 feature flag** | 代码完全隔离 | 需要大量 `#[cfg]`，维护成本高，31+ 处需要修改 |
| **运行时 feature flag**（采用） | 一份代码，维护简单 | 代码全编译，但功能受控 |

运行时方案更适合初创阶段 — 快速迭代，不需要维护多份编译配置。
