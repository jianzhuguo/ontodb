# OntoDB 商业版发布路线图（路径C + 延迟开源）

> 制定时间: 2026-08-08
> 团队规模: 1-2 人
> 策略: 商业版先行 → 市场验证 → 6-12 个月后分离开源版

---

## 一、核心原则

1. **聚焦再聚焦** — 1-2 人团队不能同时做所有事，每个阶段只做最高 ROI 的事
2. **从第一天就为分离做准备** — 虽然延迟开源，但代码架构要支持未来切割，不要事后重构
3. **卖点先行** — OntoDB 的独特价值是"本体推理 + 多模统一"，这是竞品没有的，所有优先级围绕这个展开
4. **用户驱动** — 先找到愿意付费的早期用户，用他们的反馈倒推优先级

---

## 二、开源/商业边界预设计（现在就做，成本最低）

### 推荐的模块划分

```
┌─────────────────────────────────────────────────────────────────┐
│                    未来开源版（Apache-2.0）                       │
│                                                                 │
│  onto-core        核心类型、错误、BinaryRow                       │
│  onto-storage     LSM-Tree 引擎、WAL、SSTable、MVCC              │
│  onto-ontology    OWL-lite 模型、推理器、RDF 导入导出              │
│  onto-query       SQL 解析器、执行器、SPARQL、优化器               │
│  onto-graph       属性图模型、BFS/DFS 遍历                        │
│  onto-cli         CLI 客户端                                     │
│  sdk/python       Python SDK                                     │
│                                                                 │
│  特征: 单机、完整查询能力、本体推理                                │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    商业版（Proprietary）                          │
│                                                                 │
│  onto-enterprise-cluster   Raft 共识、多副本、自动故障转移         │
│  onto-enterprise-sharding  数据分片、跨分片查询                    │
│  onto-enterprise-security  LDAP/SAML、审计日志、数据加密           │
│  onto-enterprise-backup    增量备份、PITR、异地备份                │
│  onto-enterprise-observability  高级监控、慢查询分析、自动调优      │
│  onto-enterprise-web       Web 管理控制台                         │
│  onto-enterprise-connectors  Kafka/CDC、外部数据源连接器            │
│                                                                 │
│  特征: 分布式、企业安全、运维工具、连接器                          │
└─────────────────────────────────────────────────────────────────┘
```

### 现在需要做的架构准备（1-2 天）

1. **Cargo workspace 中为商业 crate 预留目录**
   - 创建 `crates/onto-enterprise/` 目录（可以先放空的 lib.rs）
   - 商业功能通过 feature flag 控制: `cargo build --features enterprise`

2. **License 文件分层**
   - 根目录保持 Apache-2.0（开源部分）
   - `crates/onto-enterprise/` 放商业 LICENSE

3. **配置中的功能开关**
   - 在 `StorageOptions` / server 启动参数中预留 `--enterprise` flag
   - 未启用时商业功能完全不编译（零开销）

---

## 三、分阶段路线图

### Phase 0: 架构准备 + 安全修复（第 1 周）

**目标：为后续所有工作打好地基**

- [x] 修复 `.gitignore` — 添加 `config/api_keys.json`、`.env` ✅
- [x] 创建 `crates/onto-enterprise/` 骨架目录 ✅
- [x] 添加 `SECURITY.md`（安全策略 + 漏洞报告流程）✅
- [x] 添加 `CONTRIBUTING.md`（贡献指南）✅
- [x] 整理 CHANGELOG — 将 133 个 commit 的变更归档 ✅
- [ ] 创建 GitHub Release v0.1.0-alpha（tag + release notes）

**交付物：** 干净的仓库、安全的密钥管理、第一个 release tag

---

### Phase 1: 商业 MVP（第 2-8 周）

**目标：找到第一批愿意试用的用户**

按优先级排序（1-2 人团队只能做 Top 3-4）：

#### P0 — 必须做（第 2-4 周）

1. **健康检查真实探测**（2 天）
   - `/api/health` 实际检查存储引擎连接、WAL 状态、内存使用
   - `/api/health/ready` 检查是否能接受查询

2. **Graph API 端点真实实现**（3 天）
   - 当前部分图 HTTP handler 返回 mock 数据
   - 接入实际的 GraphStore

3. **OpenAPI spec**（3 天）
   - 为所有 HTTP 端点编写 OpenAPI 3.0 规范
   - 集成 Swagger UI（`/api/docs` 端点）
   - 自动生成 Python/JavaScript/Go SDK

4. **Fuzz 测试**（3 天）
   - SQL parser fuzz（`cargo-fuzz`）
   - 存储引擎 put/get fuzz
   - SPARQL parser fuzz

#### P1 — 应该做（第 5-6 周）

5. **代码覆盖率 + CI 集成**（1 天）
   - `cargo-llvm-cov` 集成到 CI
   - 设置最低覆盖率阈值（建议 70%）

6. **增量备份 + 备份验证**（3 天）
   - 基于 WAL 的增量备份
   - 备份校验和验证
   - `BACKUP VERIFY` 命令

7. **慢查询日志**（1 天）
   - 超过阈值的查询自动记录
   - 可配置阈值（默认 1s）

#### P2 — 锦上添花（第 7-8 周）

8. **JavaScript/TypeScript SDK**（5 天）
   - 基于 OpenAPI spec 自动生成基础
   - 手写高级封装

9. **示例项目**（3 天）
   - RAG 应用示例（向量搜索 + 本体推理）
   - 知识图谱示例（图遍历 + SPARQL）
   - 多模态查询示例（SQL + 向量 + 图混合）

10. **文档站**（3 天）
    - mdBook 或 Docusaurus
    - 快速开始、概念指南、API 参考、部署指南

**交付物：** 可以给早期用户试用的完整产品

---

### Phase 2: 生产加固 + 市场验证（第 9-20 周）

**目标：让早期用户成功上线，收集反馈**

#### 根据用户反馈调整优先级，但以下通常是必做的：

11. **性能压测报告**（1 周）
    - YCSB 风格基准测试
    - 与 Redis/PostgreSQL/Elasticsearch 对比
    - 公开发布基准测试结果

12. **企业安全特性**（2 周）
    - TLS 客户端证书认证
    - 查询审计日志
    - 数据加密（静态 + 传输）

13. **Web 管理控制台 MVP**（3 周）
    - 查询编辑器 + 结果可视化
    - Schema 浏览
    - 监控仪表板（嵌入 Grafana 或自建）
    - 用户/密钥管理

14. **连接器**（2 周）
    - PostgreSQL wire protocol 兼容（让 psql 能连）
    - 或 MySQL protocol 兼容
    - 这样用户可以用现有工具连接

15. **Raft 共识（如果用户需要分布式）**（4-6 周）
    - 基于 openraft 0.9 完成集成
    - 2-3 节点集群
    - 自动故障转移
    - 这是商业版核心卖点

**交付物：** 有付费用户的产品

---

### Phase 3: 开源分离（第 6-12 个月）

**前提：商业版已验证 PMF（Product-Market Fit）**

16. **代码切割**
    - 将 `onto-enterprise-*` crate 从主仓库分离
    - 开源部分保留 Apache-2.0
    - 商业部分独立仓库 + 专有 license

17. **开源版发布**
    - 发布到 crates.io
    - Docker Hub 官方镜像
    - 完整文档站
    - 社区基础设施（Discord/论坛、Issue 模板）

18. **商业模式确定**
    - 选项 A: 开源核心 + 商业插件（GitLab 模式）
    - 选项 B: 开源 + 托管服务（PlanetScale 模式）
    - 选项 C: 开源 + 企业 license（MongoDB 模式）

---

## 四、时间线总览

```
Week 1     ██████ Phase 0: 架构准备 + 安全修复
Week 2-4   ██████████████ Phase 1 P0: 健康检查、Graph API、OpenAPI、Fuzz
Week 5-6   ██████████ Phase 1 P1: 覆盖率、增量备份、慢查询
Week 7-8   ██████████ Phase 1 P2: JS SDK、示例、文档站
Week 9-12  ████████████████ Phase 2: 性能压测 + 安全 + Web 控制台
Week 13-16 ████████████████ Phase 2: 连接器 + Raft（如需）
Week 17-20 ████████████████ Phase 2: 市场验证 + 用户反馈
Month 6-12 ████████████████████████████ Phase 3: 开源分离
```

---

## 五、关键里程碑

| 里程碑 | 时间 | 标志 |
|--------|------|------|
| M1: 安全发布 | 第 1 周 | v0.1.0-alpha tag，.gitignore 修复 |
| M2: 可试用 | 第 4 周 | OpenAPI spec + Swagger UI，早期用户可试用 |
| M3: 可上线 | 第 8 周 | 文档站 + SDK + 示例，用户可自行部署 |
| M4: 有收入 | 第 16 周 | 至少 1 个付费用户或明确付费意向 |
| M5: 开源 | 第 12 个月 | 开源版发布，社区启动 |

---

## 六、风险与应对

| 风险 | 概率 | 影响 | 应对 |
|------|------|------|------|
| 1-2 人精力不足 | 高 | 延期 | 严格砍 scope，P2 可以全部推迟 |
| Raft 集成复杂度超预期 | 中 | 商业版缺少分布式能力 | 先做主从复制（比 Raft 简单），Raft 作为 v2 |
| 找不到早期用户 | 中 | 无法验证 PMF | 先在 HN/Reddit/语义网社区推广免费试用 |
| 竞品（SurrealDB/TiDB）抢先 | 低 | 市场空间缩小 | OntoDB 的本体推理是独特卖点，差异化足够 |
| 开源时机过早 | 中 | 商业收入受损 | 严格执行延迟策略，PMF 验证后再开源 |

---

## 七、竞品定位参考

| 竞品 | 定位 | OntoDB 差异化 |
|------|------|--------------|
| SurrealDB | 多模数据库 | OntoDB 有本体推理 + SPARQL，SurrealDB 没有 |
| Neo4j | 图数据库 | OntoDB 是多模统一，不需要额外配向量库 |
| Pinecone | 向量数据库 | OntoDB 有完整 SQL + 图 + 本体，不只是向量 |
| PostgreSQL + pgvector | 通用 + 向量 | OntoDB 有原生本体推理，不需要拼凑 |
| TiDB | 分布式 SQL | OntoDB 有语义能力，TiDB 没有 |

**OntoDB 的独特卖点：本体推理 + 多模统一查询。这是唯一一个在数据库内核里嵌入 OWL 推理的项目。**

---

## 八、立即可执行的下一步

确认此计划后，第一个任务是 **Phase 0**：

1. 修复 `.gitignore`（5 分钟）
2. 创建 `crates/onto-enterprise/` 骨架（10 分钟）
3. 添加 `SECURITY.md`（30 分钟）
4. 整理 CHANGELOG（1 小时）
5. 创建 v0.1.0-alpha release tag（30 分钟）

总计约 2 小时即可完成 Phase 0。
