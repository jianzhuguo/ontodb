# OntoDB 活数据最小闭环实施 Checklist

> **目标**：以最小改动量实现"衰减 + 激活 + DBA 视图"闭环，同时完成向量化能力原生化
> **预估工期**：3-4 周
> **基线提交**：`acc7bbd chore: 活数据升级前基线快照`
> **状态**：待确认

---

## 阶段一：原生向量化接入（第 1 周）

### 1.1 存储引擎接入 EmbeddingModel
- [ ] `onto-storage/Cargo.toml` 添加 `onto-embed` 依赖
- [ ] `StorageOptions` 新增 `embedding_config: Option<EmbeddingConfig>` 字段
- [ ] `LsmEngine` 新增 `embedding_model: Option<Arc<EmbeddingModel>>` 字段
- [ ] `LsmEngine::open()` 中根据配置加载模型（模型目录不存在则跳过，不报错）

### 1.2 commit_txn 加 Phase 0
- [ ] Phase 0：从 txn_manager 取出写入 buffer（极短锁）
- [ ] Phase 0：对每条 Put 操作，判断 class 是否配置了 embedding
- [ ] Phase 0：有配置 → 反序列化文档 → 提取文本 → ONNX 推理 → 写入 `__auto_embedding__` → 重序列化
- [ ] Phase 0：无配置或推理失败 → 原样传递（宽容模式）
- [ ] Phase 1 复用 Phase 0 已解析的 doc，避免二次 `parse_doc_bytes`

### 1.3 put() / put_batch() 接入
- [ ] `put()` 在 write_state 锁之前调用 `maybe_embed(&key, &value)`
- [ ] `put_batch()` 在 write_state 锁之前批量调用 `maybe_embed`
- [ ] 错误处理：推理失败 → 日志告警 + 存原始文档

### 1.4 配置项
- [ ] `config/ontodb.toml` 或 StorageOptions 支持 `embedding_model_dir`、`embedding_enabled_classes`、`embedding_fail_policy`
- [ ] 默认不启用（model_dir 为空则跳过全部逻辑，零开销）

### 1.5 测试
- [ ] 单元测试：无 embedding 配置 → put 正常，无额外开销
- [ ] 单元测试：有 embedding 配置 → 文档自动包含 `__auto_embedding__` 字段
- [ ] 单元测试：推理失败 → 宽容模式，文档正常存储（无向量）
- [ ] 集成测试：INSERT → HNSW 向量索引自动更新 → 向量搜索能查到

---

## 阶段二：价值元数据基础设施（第 2 周前半）

### 2.1 ValueMetadata 结构
- [ ] 新建 `onto-storage/src/value_meta.rs`
- [ ] 定义 `ValueMetadata` 结构体（base_score, value_score, lambda, last_activated_at, created_at, activation_count）
- [ ] 实现 `current_score()` 方法（衰减公式 `value_score × e^(-λ × Δt)`）
- [ ] 实现 `meta_key(class, pk)` → `__val_meta__::{class}::{pk}`

### 2.2 LsmEngine 读写 API
- [ ] `get_value_meta(class, pk) -> Result<Option<ValueMetadata>>`（读取 + 计算衰减）
- [ ] `put_value_meta(class, pk, meta) -> Result<()>`（写入 `__val_meta__` key）
- [ ] `activate(class, pk, delta, reason) -> Result<()>`（重置 last_activated_at + 提升 value_score）

### 2.3 默认 λ 安全策略
- [ ] 默认 λ = 0.001（约 2 年减半，宁可永远热着）
- [ ] `StorageOptions` 支持 `default_lambda` 和 `class_lambda: HashMap<String, f64>`
- [ ] 老数据无 `__val_meta__` key → 默认 value_score = 1.0，不受衰减影响

### 2.4 测试
- [ ] 单元测试：衰减公式计算正确性（不同 λ、不同时间间隔）
- [ ] 单元测试：activate 重置 last_activated_at + 提升 value_score（上限 1.0）
- [ ] 单元测试：老数据无 meta → 返回 None → 默认 1.0

---

## 阶段三：写入时自动评估 + 激活机制（第 2 周后半 ~ 第 3 周前半）

### 3.1 写入时自动评估
- [ ] Phase 0 扩展：embedding 完成后，调用 ValueScorer 评估价值
- [ ] ValueScorer 初始版本：规则引擎（有 embedding +0.3，字段完整 +0.2，大小适中 +0.1，默认 0.5）
- [ ] 评估结果写入 `__val_meta__` key
- [ ] `StorageOptions` 支持 `value_scorer_enabled: bool`（默认 false，需显式开启）

### 3.2 激活事件 API
- [ ] `ValueEvent` 枚举定义（Citation, QuerySpike, ManualHeat, ExternalSignal）
- [ ] `handle_value_event(event) -> Result<()>` 实现
- [ ] ManualHeat：重置 last_activated_at + 提升 value_score +0.5（上限 1.0）
- [ ] 事件日志写入 `__val_event__::{timestamp}::{seq}` 前缀 key

### 3.3 SQL 接口
- [ ] `SELECT system.activate('Class::pk', 'reason')` 支持
- [ ] 批量激活：`SELECT system.activate_batch('Class', 'condition', 'reason')`（可选，后续）

### 3.4 测试
- [ ] 单元测试：INSERT 自动写入 `__val_meta__` key
- [ ] 单元测试：ManualHeat 后 value_score 提升 + last_activated_at 重置
- [ ] 单元测试：激活后衰减从新起点重新计算

---

## 阶段四：DBA 系统视图（第 3 周后半）

### 4.1 温度分布视图
- [ ] `SELECT * FROM system.data_temperature` 路由到专用查询逻辑
- [ ] 扫描 `__val_meta__` 前缀 key，计算 current_score，按 hot/warm/cold 分桶统计
- [ ] 输出：tier, row_count, avg_score, min_score, max_score

### 4.2 价值事件流视图
- [ ] `SELECT * FROM system.value_events ORDER BY timestamp DESC LIMIT N`
- [ ] 扫描 `__val_event__` 前缀 key
- [ ] 输出：timestamp, entity, type, detail

### 4.3 衰减预测视图
- [ ] `SELECT * FROM system.value_decay_prediction WHERE class = 'X'`
- [ ] 计算 `predicted_cold_at = last_activated_at + ln(value_score / 0.4) / lambda`
- [ ] 输出：entity, cur_score, lambda, predicted_cold_at

### 4.4 传播配置视图（预留）
- [ ] `SELECT * FROM system.propagation_config` 返回默认配置（enabled: false）
- [ ] 为后续传播机制预留接口

### 4.5 测试
- [ ] 集成测试：插入数据后查询 data_temperature → 正确分桶
- [ ] 集成测试：触发激活事件后查询 value_events → 日志可见
- [ ] 集成测试：查询 value_decay_prediction → 时间预测合理

---

## 阶段五：查询层集成 + 端到端测试（第 4 周）

### 5.1 查询时衰减排序
- [ ] executor 支持 `ORDER BY value_score DESC` → 读取 `__val_meta__` 计算 current_score 排序
- [ ] `WHERE value_score > 0.7` → 扫描时过滤低价值条目

### 5.2 与向量搜索联动
- [ ] 语义检索 + 价值排序：`ORDER BY embedding <-> vector, value_score DESC`

### 5.3 事件日志清理
- [ ] 后台清理 7 天前的 `__val_event__` key（复用 compaction worker 线程或独立定时任务）

### 5.4 端到端验收
- [ ] 全链路：INSERT 文档 → 自动向量化 + 自动评分 → 查询返回价值排序结果 → DBA 视图可观测
- [ ] 全链路：INSERT → 3 个月后查询 → 衰减可见 → 手动加热 → 价值恢复
- [ ] 性能回归：无 embedding 配置时 put/commit_txn 延迟无变化

---

## 明确排除项（不在本次范围内）

- ~~传播机制~~（默认关闭，后续单独做）
- ~~价值感知 Compaction~~（高风险，非必须）
- ~~优先级调度~~（服务器层改动）
- ~~分层存储~~（单机场景不需要）
- ~~插件框架接入~~（核心能力原生化，插件层留给用户扩展）

---

## 预估工作量

| 阶段 | 工作量 | 关键风险 |
|------|--------|---------|
| 阶段一：原生向量化 | 1 周 | Phase 0 序列化复用 |
| 阶段二：价值元数据 | 3-4 天 | __val_meta__ key 命名规范 |
| 阶段三：评估 + 激活 | 1 周 | ValueScorer 规则合理性 |
| 阶段四：DBA 视图 | 3-4 天 | 视图路由实现 |
| 阶段五：查询集成 + 测试 | 3-4 天 | 性能回归验证 |
| **合计** | **3-4 周** | |

---

## 性能影响预期

| 场景 | 影响 | 原因 |
|------|------|------|
| 无 embedding 配置的写入 | **零开销** | 快速跳过 |
| 有 embedding 的写入 | +1-5ms/条 | ONNX 推理，在锁外 |
| 普通读取 | **零开销** | 不查 __val_meta__ |
| 带 value_score 排序的读取 | +1-2µs/条 | 多一次 LSM get + 浮点乘法 |
| DBA 视图查询 | +几ms | 低频扫描 |

---

## 存储设计

### 价值元数据：独立 key 方案（不改 Entry 结构）

```
key:   __val_meta__::{class}::{pk}
value: BinaryRow {
    base_score: f64,
    value_score: f64,
    lambda: f64,
    last_activated_at: u64,
    created_at: u64,
    activation_count: u32,
}
```

### 事件日志

```
key:   __val_event__::{timestamp}::{seq}
value: BinaryRow {
    entity: String,
    event_type: String,
    detail: String,
    timestamp: u64,
}
```

---

**© 2026 原点价值 / OntoValue Technology**
