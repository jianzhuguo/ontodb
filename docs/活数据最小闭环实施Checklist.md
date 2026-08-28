# OntoDB 活数据最小闭环实施 Checklist

> **目标**：以最小改动量实现"衰减 + 激活 + DBA 视图"闭环
> **预估工期**：2-3 周
> **基线提交**：`acc7bbd chore: 活数据升级前基线快照`
> **状态**：待实施
> **变更**：v2 — 砍掉原生向量化阶段（ONNX 是 C 依赖，有崩溃风险；向量化已通过 HTTP 侧车实现）

---

## ~~阶段一：原生向量化接入~~ → 已排除

> **排除理由**：
> 1. ONNX Runtime 是 C 库，Rust 通过 FFI 调用有段错误/内存泄漏风险，可导致数据库进程崩溃
> 2. 当前向量化已通过 **HTTP 侧车模式**实现（零 ML 依赖），架构正确
> 3. 活数据（衰减/激活/价值评分）与向量化是**独立功能**，不应耦合
> 4. 向量化原生化可作为独立项目后续评估，不应阻塞活数据交付

---

## 阶段一：价值元数据基础设施（第 1 周）

> **已有基础**：`value_meta.rs` 已实现 `ValueMetadata` 结构体、`current_score()`、`activate()`、`ValueScorer`、lambda 常量（`LAMBDA_7H`/`LAMBDA_70D`/`LAMBDA_2Y`），8 个单元测试通过。
> **待完成**：接入 LsmEngine 读写 API + StorageOptions 配置。

### 1.1 ValueMetadata 结构
- [x] 新建 `onto-storage/src/value_meta.rs`
- [x] 定义 `ValueMetadata` 结构体（base_score, value_score, lambda, last_activated_at, created_at, activation_count）
- [x] 实现 `current_score()` 方法（衰减公式 `value_score × e^(-λ × Δt)`）
- [x] 实现 `meta_key(class, pk)` → `__val_meta__::{class}::{pk}`
- [x] 导出到 `lib.rs`（`pub use value_meta::{ValueMetadata, ValueScorer, LAMBDA_7H, LAMBDA_70D, LAMBDA_2Y}`）

### 1.2 LsmEngine 读写 API
- [x] `get_value_meta(class, pk) -> Result<Option<ValueMetadata>>`（读取 + 计算衰减）
- [x] `put_value_meta(class, pk, meta) -> Result<()>`（写入 `__val_meta__` key）
- [x] `activate(class, pk, delta, reason) -> Result<()>`（重置 last_activated_at + 提升 value_score）
- [x] `get_value_score(class, pk) -> Result<f64>`（获取衰减后分数，默认 1.0）

### 1.3 默认 λ 安全策略
- [x] `LAMBDA_7H`（7 小时半衰期）、`LAMBDA_70D`（70 天半衰期）、`LAMBDA_2Y`（2 年半衰期）
- [x] `StorageOptions` 支持 `default_lambda` 和 `value_scorer_enabled`
- [x] 老数据无 `__val_meta__` key → 默认 value_score = 1.0，不受衰减影响

### 1.4 测试
- [x] 单元测试：衰减公式计算正确性（不同 λ、不同时间间隔）
- [x] 单元测试：activate 重置 last_activated_at + 提升 value_score（上限 1.0）
- [x] 单元测试：老数据无 meta → 返回 None → 默认 1.0
- [x] 集成测试：LsmEngine get/put_value_meta 端到端（7 个测试）
- [x] 集成测试：activate 创建/更新 meta
- [x] 集成测试：StorageOptions 默认值验证

---

## 阶段二：写入时自动评估 + 激活机制（第 2 周）

### 2.1 写入时自动评估
- [x] INSERT 时调用 ValueScorer 评估价值（在 executor 层，不在存储引擎层）
- [x] ValueScorer 已实现：规则引擎（文本内容 +0.2，字段完整度 +0.1，大小适中 +0.1，有类标识 +0.1，默认 0.5）
- [x] 评估结果写入 `__val_meta__` key
- [x] `StorageOptions` 支持 `value_scorer_enabled: bool`（默认 false，需显式开启）
- [x] 开关关闭时零开销（不调用 ValueScorer，不写 meta key）

### 2.2 激活事件 API
- [x] `SYSTEM ACTIVATE '<Class>::<pk>' '<reason>'` SQL 语法支持
- [x] 激活 delta +0.5（上限 1.0），重置 last_activated_at
- [ ] `ValueEvent` 枚举定义（Citation, QuerySpike, ManualHeat, ExternalSignal）— 后续扩展
- [ ] 事件日志写入 `__val_event__` key — 后续扩展

### 2.3 SQL 接口
- [x] `SYSTEM ACTIVATE 'BioTask::001' 'clinical_urgent'` 支持
- [ ] 批量激活：`SELECT system.activate_batch('Class', 'condition', 'reason')`（可选，后续）

### 2.4 测试
- [x] 全量测试 673 个通过，0 回归

---

## 阶段三：DBA 系统视图 + 查询集成（第 3 周）

### 3.1 温度分布视图
- [x] `SELECT * FROM system.data_temperature` 路由到专用查询逻辑
- [x] 扫描 `__val_meta__` 前缀 key，计算 current_score，按 hot/warm/cold 分桶统计
- [x] 输出：tier, row_count, avg_score

### 3.2 价值事件流视图
- [x] `SELECT * FROM system.value_events` 路由到专用查询逻辑
- [x] 扫描 `__val_event__` 前缀 key，按 timestamp 降序排列
- [ ] 事件日志写入 `__val_event__` key — 需要配合事件 API 扩展

### 3.3 衰减预测视图
- [x] `SELECT * FROM system.value_decay_prediction` 路由到专用查询逻辑
- [x] 计算 `predicted_cold_at = last_activated_at + ln(value_score / 0.4) / lambda`
- [x] 输出：entity, cur_score, lambda, predicted_cold_at

### 3.4 查询层集成
- [x] `SYSTEM ACTIVATE` SQL 接口
- [ ] `ORDER BY value_score DESC` 查询集成 — 后续按需
- [ ] `WHERE value_score > 0.7` 过滤 — 后续按需

### 3.5 事件日志清理
- [ ] 后台清理 7 天前的 `__val_event__` key — 后续按需

### 3.6 测试
- [x] 全量测试 673 个通过，0 回归

---

## 明确排除项（不在本次范围内）

- ~~原生向量化（ONNX）~~ — C 依赖有崩溃风险，已通过 HTTP 侧车实现
- ~~传播机制~~（默认关闭，后续单独做）
- ~~价值感知 Compaction~~（高风险，非必须）
- ~~优先级调度~~（服务器层改动）
- ~~分层存储~~（单机场景不需要）
- ~~插件框架接入~~（核心能力原生化，插件层留给用户扩展）

---

## 预估工作量（修订 v2）

| 阶段 | 工作量 | 关键风险 | 状态 |
|------|--------|---------|------|
| 阶段一：价值元数据 + LsmEngine API | 3-4 天 | `__val_meta__` key 命名规范 | ✅ 已完成 |
| 阶段二：评估 + 激活 + SQL 接口 | 1 周 | ValueScorer 规则合理性 | ✅ 已完成 |
| 阶段三：DBA 视图 + 查询集成 | 3-4 天 | 视图路由实现 | ✅ 已完成 |
| **合计** | **2-3 周** | | **✅ 全部完成** |

---

## 性能影响预期（修订 v2）

| 场景 | 影响 | 原因 |
|------|------|------|
| value_scorer_enabled=false 时的写入 | **零开销** | 不调用 ValueScorer，不写 meta key |
| value_scorer_enabled=true 时的写入 | +几µs/条 | 纯规则引擎评估 + 一次 LSM put（在锁外） |
| 普通读取 | **零开销** | 不查 __val_meta__ |
| 带 value_score 排序的读取 | +1-2µs/条 | 多一次 LSM get + 浮点乘法 |
| DBA 视图查询 | +几ms | 低频扫描 |

---

## 存储设计

### 价值元数据：独立 key 方案（不改 Entry 结构）

```
key:   __val_meta__::{class}::{pk}
value: JSON {
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
value: JSON {
    entity: String,
    event_type: String,
    detail: String,
    timestamp: u64,
}
```

---

**© 2026 原点价值 / OntoValue Technology**
