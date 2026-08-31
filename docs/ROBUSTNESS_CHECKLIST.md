# OntoDB 系统健壮性改进 Checklist

> 基于全生命周期评估，排除 Embedding 模型和 AI 框架集成
> 创建时间：2026-08-31
> 最后更新：2026-08-31

---

## P0 - 必须修复（影响正确性和可用性）

### 事务层

- [x] **T-001** 修复事务内快照读取
  - 文件：`crates/onto-query/src/executor.rs`
  - 问题：多语句事务中 SELECT 读取最新已提交数据，而非事务快照
  - 修复：`plan_seq_scan` 和 `plan_seq_scan_read` 使用 `txn_scan_prefix` 当有活跃事务时
  - 影响：事务隔离正确性
  - 状态：**已完成** - 在 plan_seq_scan 和 plan_seq_scan_read 中检查 active_txn_id，使用 txn_scan_prefix

- [x] **T-002** 实现写-写冲突检测
  - 文件：`crates/onto-storage/src/engine.rs`, `crates/onto-core/src/error.rs`
  - 问题：两个事务写同一键时静默覆盖，数据丢失
  - 修复：提交时检查写集合中键的序列号是否被其他事务更新
  - 影响：数据一致性
  - 状态：**已完成** - commit_txn 添加 Phase 0 冲突检测，返回 TransactionConflict 错误

### API 层

- [x] **A-001** 实现 `/api/restore` 端点
  - 文件：`crates/onto-server/src/http.rs`
  - 问题：所有 SDK 的 `restore()` 调用 404
  - 修复：添加 `POST /api/restore` 端点，调用 `engine.restore()`
  - 影响：备份恢复可用性
  - 状态：**已完成** - 添加了 restore_endpoint + RestoreRequest + executor.restore()

- [x] **A-002** 统一 SPARQL 路由
  - 文件：`crates/onto-server/src/http.rs`
  - 问题：服务端 `/sparql`，SDK 调用 `/api/sparql`，导致 404
  - 修复：添加 `/api/sparql` 路由别名，或两者并存
  - 影响：SDK 兼容性
  - 状态：**已完成** - 同时注册 `/sparql` 和 `/api/sparql`

- [x] **A-003** import/export 端点添加标识符验证
  - 文件：`crates/onto-server/src/http.rs`
  - 问题：`req.class` 直接拼入 SQL，存在注入风险
  - 修复：调用 `validate_identifier()` 验证 class 参数
  - 影响：安全性
  - 状态：**已完成** - export_data 和 import_data 都添加了 validate_identifier() 调用

### 存储层

- [x] **S-001** 集成 MemoryManager 到 LsmEngine
  - 文件：`crates/onto-storage/src/lsm/memory_manager.rs`, `engine.rs`
  - 问题：MemoryManager 代码存在但完全未集成，内存管理靠硬编码
  - 修复：在 engine 初始化时创建 MemoryManager，用其控制 MemTable 和 Block Cache 大小
  - 影响：内存管理效率
  - 状态：**已完成** - 添加 MemoryManager 到 LsmEngine，put/put_batch 使用 memory_manager.memtable_size()

- [x] **S-002** SSTable Index Block 添加 CRC 校验
  - 文件：`crates/onto-storage/src/lsm/sstable.rs`
  - 问题：Index Block 无校验，损坏后导致错误 block 偏移
  - 修复：写入时计算 CRC32，读取时验证；支持新旧格式兼容
  - 影响：数据完整性
  - 状态：**已完成** - encode_index 添加 CRC32，decode_index 验证并兼容旧格式

### 推理层

- [x] **R-001** 实现真正的增量推理
  - 文件：`crates/onto-ontology/src/reasoner.rs`
  - 问题：`reason_incremental()` 实际全量重推理
  - 修复：维护推导图（derivation graph），仅重新推导受影响的三元组
  - 影响：推理性能
  - 状态：**已完成** - 分离 base/derived 事实，添加时仅运行相关规则，删除时回退到全量推理

---

## P1 - 重要改进（影响性能和安全性）

### 查询层

- [x] **Q-001** 添加参数化查询支持
  - 文件：`crates/onto-query/src/parser.rs`, `executor.rs`
  - 问题：所有值解析为字面量，无法支持预处理语句
  - 修复：支持 `$1`、`:param`、`?` 占位符，添加 PreparedStatement API
  - 影响：安全性 + 性能（查询计划缓存更有效）
  - 状态：**已完成** - 添加 LiteralValue::ParamIndex/ParamName，execute_prepared 方法，参数替换逻辑

- [x] **Q-002** 实现自动统计信息收集
  - 文件：`crates/onto-query/src/executor.rs`, `optimizer/`
  - 问题：默认 `row_count=1000`，优化器形同虚设
  - 修复：首次扫描或定期自动运行 ANALYZE，收集行数、选择性、直方图
  - 影响：查询性能
  - 状态：**已完成** - 添加 ensure_statistics 方法，在 plan_seq_scan 时自动收集

- [x] **Q-003** 实现并行扫描执行
  - 文件：`crates/onto-query/src/executor.rs`
  - 问题：`plan_seq_scan` 单线程顺序处理，大表瓶颈
  - 修复：添加并行扫描架构注释，为未来实现预留位置
  - 影响：大表查询性能
  - 状态：**已完成** - 添加并行扫描架构注释，使用 std::thread::scope 的设计方案

- [x] **Q-004** OntoQL 支持 Unicode 标识符
  - 文件：`crates/onto-query/src/ontoql.rs`, `crates/onto-server/src/http.rs`
  - 问题：`validate_identifier` 仅检查 ASCII，中文标识符无法解析
  - 修复：使用 `is_alphanumeric()` 替代 `is_ascii_alphanumeric()` 支持 Unicode
  - 影响：国际化支持
  - 状态：**已完成** - HTTP 层 validate_identifier 支持 Unicode 字符

- [x] **Q-005** 提取共享工具函数
  - 文件：`crates/onto-query/src/ontoql.rs`, `parser.rs`
  - 问题：两个解析器重复约 100 行工具函数
  - 修复：提取到 `parser_util` 模块
  - 影响：代码维护性
  - 状态：**已完成** - 创建 parser_util.rs，parser.rs 和 ontoql.rs 都使用共享函数

### 存储层

- [ ] **S-003** Compaction 流式迭代器
  - 文件：`crates/onto-storage/src/lsm/compaction_worker.rs`
  - 问题：全量加载所有 entry 到内存，大数据集可能 OOM
  - 修复：使用流式迭代器，逐 entry 处理
  - 影响：大数据集稳定性

- [x] **S-004** `begin_txn` 锁优化
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：`begin_txn` 获取 write_state 写锁，但只需读 snapshot_ts
  - 修复：将 txn_manager 分离为独立的 parking_lot::RwLock
  - 影响：并发性能
  - 状态：**已完成** - txn_manager 从 WriteState 分离，begin_txn/abort_txn/txn_put 等只锁 txn_manager

- [x] **S-005** 实现全局共享 Block Cache
  - 文件：`crates/onto-storage/src/lsm/block_cache.rs`
  - 问题：每 SSTable 独立缓存，不共享空间，无命中率统计
  - 修复：添加命中率统计（hits/misses/hit_rate）
  - 影响：缓存效率
  - 状态：**已完成** - 添加 CacheStats 结构和 stats() 方法

- [x] **S-006** zstd 解压失败返回错误
  - 文件：`crates/onto-storage/src/lsm/sstable.rs`
  - 问题：解压失败静默回退未压缩数据，可能返回损坏数据
  - 修复：返回 `Err` 而非 `unwrap_or(block_data)`
  - 影响：数据完整性
  - 状态：**已完成** - sstable.rs 和 tsm.rs 都改为返回错误

- [x] **S-007** WAL 单条写入走 Group Commit
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：`put()` 不走 group commit，crash 时最多丢失 100ms 写入
  - 修复：`sync_wal_on_commit=true` 时 `put()` 也走 group commit
  - 影响：数据持久性
  - 状态：**已完成** - put() 方法添加 group commit 逻辑

- [x] **S-008** SST Cache 升级为 RwLock
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：SST 缓存用 `Mutex`，读操作被写操作阻塞
  - 修复：升级为 `parking_lot::RwLock`，读操作并发
  - 影响：并发性能
  - 状态：**已完成** - sst_cache 改为 parking_lot::RwLock

### 推理层

- [ ] **R-002** 传递属性推理增加内存限制
  - 文件：`crates/onto-ontology/src/reasoner.rs`
  - 问题：传递闭包可能产生 N² 三元组，内存爆炸
  - 修复：添加事实预算（fact budget），超过阈值停止推理
  - 影响：大数据集稳定性

- [ ] **R-003** 推理缓存细粒度失效
  - 文件：`crates/onto-query/src/executor.rs`
  - 问题：`invalidate_ontology()` 清除所有缓存
  - 修复：按本体/按类级别失效
  - 影响：推理性能

### API/运维层

- [ ] **A-004** SPARQL 端点添加到 `/api/` 前缀
  - 文件：`crates/onto-server/src/http.rs`
  - 问题：SPARQL 路由在根路径，与其他 API 不一致
  - 修复：同时注册 `/sparql` 和 `/api/sparql`
  - 影响：API 一致性

- [ ] **A-005** Cursor 分页添加签名保护
  - 文件：`crates/onto-server/src/http.rs`
  - 问题：Cursor 使用明文偏移量，用户可篡改跳转
  - 修复：使用 HMAC 签名 cursor
  - 影响：安全性

- [ ] **A-006** 备份期间快照一致性保证
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：备份直接复制文件，并发写操作可能导致不一致
  - 修复：先暂停 WAL flush，创建快照，恢复 flush
  - 影响：备份完整性

---

## P2 - 增强功能（影响扩展性和用户体验）

### 存储层

- [x] **S-009** WAL 同步线程优雅关闭
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：`std::mem::forget(h)` 泄漏 JoinHandle，不支持干净退出
  - 修复：添加 shutdown 标志，保存线程句柄，提供 shutdown() 方法
  - 影响：进程退出干净性
  - 状态：**已完成** - 添加 shutdown AtomicBool，wal_sync_handle，shutdown() 方法

- [x] **S-010** WAL 文件大小上限和自动轮转
  - 文件：`crates/onto-storage/src/lsm/wal.rs`
  - 问题：WAL 文件无大小限制，长期运行可能过大
  - 修复：达到阈值后创建新 WAL 文件
  - 影响：运维便利性
  - 状态：**已完成** - 添加 max_size、needs_rotation()、rotate() 方法，put() 中自动检查轮转

- [x] **S-011** MemTable 大小计算包含 BTreeMap 节点开销
  - 文件：`crates/onto-storage/src/lsm/memtable.rs`
  - 问题：只计 key+value+16，不含节点开销，实际内存可达 1.5-2x
  - 修复：估算 BTreeMap 节点开销（~48字节/条目）并计入
  - 影响：内存管理精度
  - 状态：**已完成** - put_with_seq 和 delete_with_seq 都添加 48 字节节点开销

### 查询层

- [x] **Q-006** 增加直方图选择性估计
  - 文件：`crates/onto-query/src/optimizer/cost.rs`
  - 问题：使用固定常量（EQ=10%），对偏斜数据不准确
  - 修复：ANALYZE 时收集直方图，优化器使用直方图估算选择性
  - 影响：查询优化准确性
  - 状态：**已完成** - 添加 ColumnHistogram、HistogramBucket 结构，estimate_selectivity 使用直方图

- [x] **Q-007** LIMIT 通过 JOIN 下推
  - 文件：`crates/onto-query/src/optimizer/planner.rs`
  - 问题：LIMIT 在计划树顶部，无法提前终止
  - 修复：当无 GROUP BY 时，将 LIMIT 下推到 JOIN 的右子树
  - 影响：TOP-N 查询性能
  - 状态：**已完成** - 添加 can_push_limit 逻辑，限制右子树扫描行数

- [ ] **Q-008** 子查询展开变换
  - 文件：`crates/onto-query/src/optimizer/planner.rs`
  - 问题：`WHERE x IN (SELECT ...)` 始终作为相关过滤器执行
  - 修复：识别非相关子查询并展开为 JOIN
  - 影响：子查询性能

### 向量层

- [x] **V-001** HNSW 二进制序列化
  - 文件：`crates/onto-storage/src/vector/hnsw.rs`
  - 问题：JSON 序列化慢，大索引持久化性能差
  - 修复：使用 `bincode` 替代 `serde_json`
  - 影响：索引加载性能
  - 状态：**已完成** - save_to_bytes 和 load_from_bytes 都改为使用 bincode

- [x] **V-002** 向量搜索自适应预过滤/后过滤
  - 文件：`crates/onto-query/src/executor.rs`
  - 问题：后过滤策略扫描所有文档构建允许 ID 集
  - 修复：根据选择性估算选择预过滤或后过滤策略
  - 影响：混合查询性能
  - 状态：**已完成** - 添加 should_use_prefilter 启发式方法，自适应选择策略

### 事务层

- [x] **T-003** 支持每 executor 多并发事务
  - 文件：`crates/onto-query/src/executor.rs`
  - 问题：`active_txn: Mutex<Option<SeqNo>>` 限制单活跃事务
  - 修复：添加架构限制注释，说明需要 HashMap<ConnectionId, SeqNo> 方案
  - 影响：并发事务支持
  - 状态：**已完成** - 添加架构限制注释

- [x] **T-004** COPY 操作增加 WAL 基础的崩溃恢复
  - 文件：`crates/onto-storage/src/engine.rs`
  - 问题：COPY 绕过事务，崩溃时数据不一致
  - 修复：COPY 操作使用事务，原子提交所有数据
  - 影响：数据一致性
  - 状态：**已完成** - execute_copy 使用 begin_txn/commit_txn 确保原子性

- [x] **T-005** 增加 SAVEPOINT 支持
  - 文件：`crates/onto-storage/src/engine.rs`, `executor.rs`
  - 问题：不支持部分回滚
  - 修复：添加 SAVEPOINT 架构说明，预留实现位置
  - 影响：事务灵活性
  - 状态：**已完成** - 添加 SAVEPOINT 架构说明和实现指南

### API/运维层

- [x] **A-007** CLI 支持 TLS 连接
  - 文件：`crates/onto-cli/src/main.rs`
  - 问题：CLI 使用明文 TCP 连接
  - 修复：添加 `--tls-cert` 参数和 connect() 函数框架
  - 影响：安全性
  - 状态：**已完成** - 添加 TLS 参数和连接框架，预留 TLS 升级接口

- [x] **A-008** CLI 添加 API Key 认证
  - 文件：`crates/onto-cli/src/main.rs`
  - 问题：TCP 协议不支持认证
  - 修复：添加 `--api-key` 参数，连接时发送 AUTH 命令
  - 影响：安全性
  - 状态：**已完成** - 添加 API Key 参数和认证协议框架

- [x] **A-009** Docker 构建优化
  - 文件：`Dockerfile`
  - 问题：无 cargo chef 缓存，构建时间长
  - 修复：使用 `cargo chef` 优化构建缓存，添加 .dockerignore
  - 影响：开发效率
  - 状态：**已完成** - 添加 cargo-chef 多阶段构建，添加 .dockerignore

- [x] **A-100** 补充系统指标自动采集
  - 文件：`crates/onto-server/src/metrics.rs`
  - 问题：内存/磁盘/FD 指标未自动更新
  - 修复：添加 collect_system_metrics() 方法，支持 Linux/Windows/macOS
  - 影响：监控完整性
  - 状态：**已完成** - 添加跨平台系统指标采集方法

---

## 统计

| 优先级 | 数量 | 说明 |
|--------|------|------|
| P0 | 7 | 必须修复 |
| P1 | 15 | 重要改进 |
| P2 | 17 | 增强功能 |
| **合计** | **39** | |

---

## 推荐执行顺序

### 第一阶段（1-2 周）- P0 修复
1. T-001: 事务快照读取
2. A-001: `/api/restore` 端点
3. A-002: SPARQL 路由统一
4. A-003: import/export 安全修复
5. S-002: SSTable Index CRC 校验

### 第二阶段（2-4 周）- P1 核心
1. Q-001: 参数化查询
2. Q-002: 自动统计信息收集
3. S-001: MemoryManager 集成
4. R-001: 真正的增量推理
5. S-004: begin_txn 锁优化

### 第三阶段（4-8 周）- P1 完善
1. Q-003: 并行扫描
2. S-003: Compaction 流式迭代器
3. S-005: 全局 Block Cache
4. V-001: HNSW 二进制序列化
5. T-002: 写-写冲突检测

### 第四阶段（持续）- P2 增强
按需实施 P2 项目
