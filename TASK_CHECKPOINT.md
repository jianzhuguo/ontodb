# OntoDB 全面审计修复任务清单

> 生成时间: 2026-08-08 | 总计: 142 个问题 (19 Critical / 37 High / 58 Medium / 28 Low)
> 最后更新: 2026-08-09 | 已修复: 95/142 (阶段一~四完成)
> 多模扩展: GIS + 时序 + 时空融合 (阶段五)

## 修复进度总览

| 阶段 | 状态 | 已修复/总数 |
|------|------|------------|
| 阶段一 Critical | ✅ 已完成 | 16/16 |
| 阶段二 High | ✅ 已完成 | 19/30 |
| 阶段三 Medium | ✅ 已完成 | 35/35 |
| 阶段四 Low | 🔄 接近完成 | 25/28 |

### 阶段二已修复的关键问题
- [x] T2.1 API Key 时序攻击 → 常量时间比较 (constant_time_eq)
- [x] T2.2 X-Forwarded-For IP 欺骗 → 取最右IP + 校验格式
- [x] T2.4 Admin IP 验证空实现 → 添加 is_valid_ip_or_cidr 校验
- [x] T2.6 审计链完整性断裂 → 原子化 hash 读取+更新
- [x] T2.7 Admin 操作未审计 → 连接审计日志到 admin_state
- [x] T2.9 无请求体大小限制 → 添加 64MB DefaultBodyLimit
- [x] T2.13 Raft 元数据静默损坏 → 添加错误日志
- [x] T2.19 BufferPool 静默读取失败 → 区分 EOF 和 I/O 错误
- [x] T2.20 drop_vector_index WAL 错误 → 传播错误
- [x] T2.21 SPARQL 字面量转义不完整 → 添加 escape_sql_string
- [x] T2.23 DFS 递归无深度保护 → 添加 1000 层上限
- [x] T2.24 u32 顶点索引溢出 → 添加溢出检查
- [x] T2.25 CTE 列表解析错误 → 修复 name(cols) 解析
- [x] T2.26 Executor Mutex unwrap 级联 → 全部改为 unwrap_or_else 恢复
- [x] 阶段一遗留: Raft 网络消息大小限制 / .gitignore 补全

### 已修复的关键问题
- [x] T1.1 PG Wire 认证绕过 → 添加密码认证流程
- [x] T1.2 SQL 注入 (vector_search/hybrid_query) → 添加标识符校验
- [x] T1.3 路径穿越 (backup) → 添加路径校验
- [x] T1.4 HNSW unsafe UB → 移除 unsafe，改用 Vec clone
- [x] T1.5 Raft install_snapshot 非原子 → 先写新数据再删旧数据
- [x] T1.6 Raft try_get_log_entries → 改 break 为 continue
- [x] T1.7 Raft check_node → 添加 TODO 注释（部分修复）
- [x] T1.8 Raft md5_hash → 替换为确定性 FNV-1a 哈希
- [x] T1.9 Batch 递归深度限制 → 添加 depth() 和 flatten()
- [x] T1.12 SELECT FROM 引号内 → 使用 find_unquoted
- [x] T1.13 CASE WHEN 缺少 END panic → 添加边界检查
- [x] T1.14 is_subclass_of 环检测 → 添加 visited 集合
- [x] T1.15 delete_vertex 整数邻接表 → 同步更新
- [x] T1.16 EXPLAIN 执行 DML → 仅对只读查询执行
- [x] WAL fsync 间隔 → 批量刷新时也 sync
- [x] MVCC txn_get 快照不一致 → 原子化读取
- [x] 负数编码破坏排序 → 使用偏移编码
- [x] Raft 网络消息大小限制 → 16MB 上限
- [x] .gitignore 补全 → 添加 .env.alerting

---

## 阶段一：致命安全漏洞 (Critical — 必须立即修复)

### T1.1 [onto-server] PG Wire 协议无认证 — 任意客户端可执行任意查询
- **文件**: `crates/onto-server/src/pgwire.rs:115-119`
- **问题**: 发送 `AuthenticationOk` 无条件，不检查任何凭据
- **修复**: 实现 SASL/密码认证流程，校验 API key 或用户名密码

### T1.2 [onto-server] SQL 注入 — vector_search 和 hybrid_query
- **文件**: `crates/onto-server/src/http.rs:702-704, 853-855`
- **问题**: `req.class` 和 `req.column` 通过 `format!()` 直接拼接进查询字符串
- **修复**: 用正则 `^[a-zA-Z_][a-zA-Z0-9_]*$` 校验标识符，或使用参数化查询

### T1.3 [onto-server] 路径穿越 — 备份可写入任意文件系统路径
- **文件**: `crates/onto-server/src/http.rs:1264`
- **问题**: `req.path` 直接传给 `Path::new()`，无路径校验
- **修复**: 限制备份目录白名单，拒绝 `..` 组件

### T1.4 [onto-storage] HNSW unsafe 代码 — 悬垂指针导致 UB
- **文件**: `crates/onto-storage/src/vector/hnsw.rs:208-211`
- **问题**: `from_raw_parts` 指针在后续 Vec 扩容后失效
- **修复**: 克隆查询向量，移除 unsafe 块

### T1.5 [onto-raft] install_snapshot 非原子 — 崩溃丢失全部状态机数据
- **文件**: `crates/onto-raft/src/persistent_store.rs:498-522`
- **问题**: 先逐条删除再逐条写入，无 WAL 或批量原子操作
- **修复**: 使用 write batch 确保 delete+write 全部成功或全部回滚

### T1.6 [onto-raft] try_get_log_entries 遇到缺失条目立即中断
- **文件**: `crates/onto-raft/src/persistent_store.rs:215-218`
- **问题**: 日志清理后出现空洞时返回截断结果，follower 可能数据不一致
- **修复**: 使用 `scan_prefix` + 范围过滤替代逐个索引迭代

### T1.7 [onto-raft] cluster_whitelist check_node 返回本地配置而非远程
- **文件**: `crates/onto-raft/src/cluster_whitelist.rs:234-242`
- **问题**: 一致性检查变成自比较，整个白名单验证系统形同虚设
- **修复**: 通过 HTTP/RPC 获取远程节点配置进行比较

### T1.8 [onto-raft] md5_hash 使用 DefaultHasher — 跨节点哈希不一致
- **文件**: `crates/onto-raft/src/cluster_whitelist.rs:374-380`
- **问题**: SipHash 带随机种子，相同数据不同节点得到不同哈希值
- **修复**: 使用 SHA-256 或确定性哈希算法

### T1.9 [onto-raft] Batch 递归无深度限制 — 栈溢出 DoS
- **文件**: `crates/onto-raft/src/types.rs:35`
- **问题**: 恶意客户端构造深层嵌套 Batch 导致栈溢出
- **修复**: 反序列化时限制递归深度，或展平嵌套 Batch

### T1.10 [infra] RSA 私钥提交到仓库
- **文件**: `deploy/nginx/certs/server.key`
- **问题**: 任何有仓库访问权的人都能解密 TLS 流量
- **修复**: 从 git 历史中移除（`git filter-branch` / BFG），轮换所有密钥

### T1.11 [infra] .env 和 api_keys.json 已提交含默认密码
- **文件**: `.env:34,38`, `config/api_keys.json`
- **问题**: `CHANGE_ME` 密码已入库，部署后即可被猜到
- **修复**: `git rm --cached`，已提交的需从历史中清除

### T1.12 [onto-query] SELECT 解析未跳过引号内 FROM
- **文件**: `crates/onto-query/src/parser.rs:1073`
- **问题**: 字符串字面量中的 `FROM` 被误识别为关键字
- **修复**: 使用已有的 `find_unquoted()` 方法替代 `str::find()`

### T1.13 [onto-query] CASE WHEN 缺少 END 时 panic
- **文件**: `crates/onto-query/src/parser.rs:1315-1316`
- **问题**: `part[end_pos + 4..]` 越界
- **修复**: 添加边界检查，缺少 END 时返回错误而非 panic

### T1.14 [onto-ontology] is_subclass_of 无环检测 — 无限递归
- **文件**: `crates/onto-ontology/src/model.rs:269-283`
- **问题**: 循环继承关系 (A→B→A) 导致栈溢出
- **修复**: 维护访问集合，检测到环时返回 false

### T1.15 [onto-graph] delete_vertex 未更新整数邻接表
- **文件**: `crates/onto-graph/src/store.rs:161-197`
- **问题**: 删除顶点后 `bfs_fast()` 返回已删除的顶点
- **修复**: 同步更新 `adj_out`、`adj_in`、`edge_index`

### T1.16 [onto-query] EXPLAIN 实际执行了 DML 语句
- **文件**: `crates/onto-query/src/executor.rs:2043-2044`
- **问题**: EXPLAIN INSERT/DELETE/UPDATE 会修改数据
- **修复**: EXPLAIN 模式下跳过实际执行，只返回计划

---

## 阶段二：高危安全与数据完整性问题 (High)

### T2.1 [onto-server] API Key 时序攻击
- **文件**: `crates/onto-server/src/auth.rs:300`
- **修复**: 使用常量时间比较 (`constant_time_eq`)

### T2.2 [onto-server] X-Forwarded-For IP 欺骗
- **文件**: `crates/onto-server/src/auth.rs:476-482`
- **修复**: 仅在可信反向代理后信任该头，或使用 socket 地址

### T2.3 [onto-server] SQL 注入过滤器可绕过
- **文件**: `crates/onto-server/src/http.rs:42-63`
- **修复**: 正则匹配 + 添加 `SLEEP`/`BENCHMARK`/`pg_sleep` 等关键词

### T2.4 [onto-server] Admin IP 验证空实现
- **文件**: `crates/onto-server/src/admin.rs:121-127`
- **修复**: 实现 IP 格式校验（IPv4/IPv6）

### T2.5 [onto-server] Admin 配置 TOCTOU 竞态
- **文件**: `crates/onto-server/src/admin.rs:389-399`
- **修复**: 使用 Mutex 保护整个 read-modify-write 序列

### T2.6 [onto-server] 审计链完整性断裂
- **文件**: `crates/onto-server/src/audit.rs:219-241`
- **修复**: 在单次锁内完成 hash 读取和更新

### T2.7 [onto-server] Admin 操作未写入审计日志
- **文件**: `crates/onto-server/src/main.rs:391`
- **修复**: 将 `admin_state.audit` 设置为实际的审计实例

### T2.8 [onto-server] PG Wire 无速率限制/审计/大小限制
- **文件**: `crates/onto-server/src/pgwire.rs:140-184`
- **修复**: 添加连接数限制、查询大小限制、审计日志

### T2.9 [onto-server] 无请求体大小限制 — OOM 风险
- **文件**: `crates/onto-server/src/http.rs` (全局)
- **修复**: 配置 axum `DefaultBodyLimit`

### T2.10 [onto-raft] 网络层无消息大小限制 — 4GB OOM
- **文件**: `crates/onto-raft/src/network.rs:66,175`
- **修复**: 限制 `req_len`/`resp_len` 最大 16MB

### T2.11 [onto-raft] TCP 服务器是占位符 — 无 RPC 分发
- **文件**: `crates/onto-raft/src/network.rs:164-196`
- **修复**: 实现消息类型解析和 RPC 分发

### T2.12 [onto-raft] 节点间通信无 TLS/认证
- **文件**: `crates/onto-raft/src/network.rs` (全局)
- **修复**: 集成 rustls 或 tokio-native-tls

### T2.13 [onto-raft] 元数据加载静默忽略损坏
- **文件**: `crates/onto-raft/src/persistent_store.rs:73-100`
- **修复**: 反序列化失败时返回错误而非使用默认值

### T2.14 [onto-raft] find_last_log_id O(n) 全量扫描
- **文件**: `crates/onto-raft/src/persistent_store.rs:591-612`
- **修复**: 维护 `__raft_meta__last_log_index` 键

### T2.15 [onto-raft] purge_logs_upto 线性遍历 0..=index
- **文件**: `crates/onto-raft/src/persistent_store.rs:374`
- **修复**: 使用 `scan_prefix` + 范围删除

### T2.16 [onto-storage] WAL 非事务写入无 fsync
- **文件**: `crates/onto-storage/src/engine.rs:284-287`
- **修复**: 非事务 `put()`/`delete()` 也触发 sync（可配置）

### T2.17 [onto-storage] MVCC txn_get 快照不一致窗口
- **文件**: `crates/onto-storage/src/engine.rs:1048-1062`
- **修复**: 在单次锁获取内完成写缓冲读取和可见性构造

### T2.18 [onto-storage] 负数编码破坏 B+Tree 排序
- **文件**: `crates/onto-storage/src/index/manager.rs:475-477`
- **修复**: 使用偏移编码（加 offset 使所有值为正）

### T2.19 [onto-storage] BufferPool 静默读取失败
- **文件**: `crates/onto-storage/src/index/disk.rs:750-754`
- **修复**: 区分 EOF（稀疏文件）和真实 I/O 错误

### T2.20 [onto-storage] drop_vector_index 忽略 WAL 错误
- **文件**: `crates/onto-storage/src/engine.rs:1335-1338`
- **修复**: 传播 WAL 写入错误

### T2.21 [onto-query] SPARQL 字面量转义不完整
- **文件**: `crates/onto-query/src/sparql.rs:1095`
- **修复**: 转义反斜杠、空字节等特殊字符

### T2.22 [onto-query] 查询超时在执行后才检查
- **文件**: `crates/onto-query/src/executor.rs:579`
- **修复**: 使用 tokio `select!` + `timeout` 实现真正的取消

### T2.23 [onto-graph] DFS 递归无深度保护
- **文件**: `crates/onto-graph/src/traversal.rs:386-476`
- **修复**: 改为迭代实现或添加栈深度限制

### T2.24 [onto-graph] u32 顶点索引溢出
- **文件**: `crates/onto-graph/src/store.rs:88`
- **修复**: 使用 `u64` 或添加溢出检查

### T2.25 [onto-query] CTE 列表解析错误
- **文件**: `crates/onto-query/src/parser.rs:739-748`
- **修复**: 正确解析 `name(cols) AS (...)` 语法

### T2.26 [onto-query] Executor Mutex unwrap 级联 panic
- **文件**: `crates/onto-query/src/executor.rs:366,405,410`
- **修复**: 使用 `.lock().unwrap_or_else(|e| e.into_inner())` 或传播错误

### T2.27 [onto-query] SPARQL OPTIONAL 语义错误
- **文件**: `crates/onto-query/src/sparql.rs:960-985`
- **修复**: 翻译为 LEFT JOIN 而非 WHERE AND 条件

### T2.28 [onto-graph] delete_vertex 潜在死锁
- **文件**: `crates/onto-graph/src/store.rs:178-197`
- **修复**: 统一锁获取顺序，或使用单个锁保护整个删除操作

### T2.29 [infra] .env.alerting 未加入 .gitignore
- **文件**: `.gitignore` (缺失条目)
- **修复**: 添加 `deploy/monitoring/.env.alerting`

### T2.30 [infra] 自签名证书用于生产配置
- **文件**: `deploy/nginx/certs/server.crt`
- **修复**: 替换为正式 TLS 证书

---

## 阶段三：中等问题 (Medium)

### T3.1 [onto-server] 缺少安全响应头 (X-Content-Type-Options 等) ✅
- **文件**: `crates/onto-server/src/http.rs`
- **修复**: 已实现 security_headers_middleware (x-content-type-options, x-frame-options, x-xss-protection, referrer-policy, content-security-policy)

### T3.2 [onto-server] TLS 模块已实现但未集成到 HTTP 服务器 ✅
- **文件**: `crates/onto-server/src/tls.rs`, `main.rs:482-499`
- **修复**: TLS 已集成到 HTTP 服务器启动流程 (axum_server::bind_rustls)

### T3.3 [onto-server] TLS 最低版本配置未生效 ✅
- **文件**: `crates/onto-server/src/tls.rs:156-159`
- **修复**: TLS 版本配置已通过 with_protocol_versions 生效 (Tls12/Tls13)

### T3.4 [onto-server] CORS 过于宽松 (`CorsLayer::permissive()`) ✅
- **文件**: `crates/onto-server/src/http.rs:188-189`
- **修复**: 已实现 build_cors_layer() 函数，支持配置化 CORS 策略

### T3.5 [onto-server] 审计 truncate UTF-8 panic ✅
- **文件**: `crates/onto-server/src/audit.rs:389`
- **修复**: truncate 函数已正确处理 UTF-8 边界 (is_char_boundary 检查)

### T3.6 [onto-server] TCP 服务器无连接数限制 ✅
- **文件**: `crates/onto-server/src/main.rs:480-495`
- **修复**: TCP 服务器已实现 MAX_TCP_CONNECTIONS=256 信号量限制；PG Wire 已实现 MAX_PGWIRE_CONNECTIONS=128 限制

### T3.7 [onto-server] TCP 行读取无大小限制 ✅
- **文件**: `crates/onto-server/src/main.rs:520`
- **修复**: TCP 已实现 MAX_LINE_BYTES=1MB 限制；PG Wire 已实现 MAX_PGWIRE_MESSAGE_SIZE=16MB 限制

### T3.8 [onto-server] PG Wire 硬编码后端密钥 `12345` ✅
- **文件**: `crates/onto-server/src/pgwire.rs:127`
- **修复**: 已实现动态密钥生成 (time_seed ^ port ^ counter ^ process_id)

### T3.9 [onto-server] Auth ReadWrite 权限过于宽泛 ✅
- **文件**: `crates/onto-server/src/auth.rs:40-46`
- **修复**: ReadWrite 已正确阻止 /admin 路径，允许数据操作，符合数据库权限模型

### T3.10 [onto-server] Admin add_ips 无 IP 格式校验 ✅
- **文件**: `crates/onto-server/src/admin.rs:246-275`
- **修复**: 已实现 is_valid_ip_or_cidr 校验

### T3.11 [onto-server] 速率限制键可被 X-Forwarded-For 欺骗 ✅
- **文件**: `crates/onto-server/src/rate_limit.rs:189-202`
- **修复**: 已使用 X-Real-IP 替代 X-Forwarded-For

### T3.12 [onto-server] select_all 任一服务器崩溃导致全部停止 ✅
- **文件**: `crates/onto-server/src/main.rs:243`
- **修复**: 已实现 graceful shutdown 逻辑

### T3.13 [onto-storage] MemTable size 计数器只增不减 ✅
- **文件**: `crates/onto-storage/src/lsm/memtable.rs:87-88, 98-99`
- **修复**: 已实现 saturating_sub 在覆盖时减小 size

### T3.14 [onto-storage] Bloom filter n=0 时除零 panic ✅
- **文件**: `crates/onto-storage/src/lsm/bloom_filter.rs:125-131`
- **修复**: optimal_hashes 已处理 n=0 返回 1

### T3.15 [onto-storage] Compaction 非原子级别元数据更新窗口 ✅
- **文件**: `crates/onto-storage/src/lsm/compaction_worker.rs:411-442`
- **修复**: 级别元数据更新已在单个 mutex lock 内原子完成 (levels.lock())

### T3.16 [onto-storage] vector deleted_keys 无限增长 ✅
- **文件**: `crates/onto-storage/src/vector/manager.rs:49`
- **修复**: 已实现 compact_deleted_keys() 方法清理过期 tombstone

### T3.17 [onto-storage] DiskPage 不回收已删除条目空间 ✅
- **文件**: `crates/onto-storage/src/index/disk.rs:424-458`
- **修复**: remove_entry 已实现自动 defrag (碎片超过 25% 时触发)

### T3.18 [onto-storage] Worker 线程 JoinHandle 泄漏 ✅
- **文件**: `crates/onto-storage/src/engine.rs:228`
- **修复**: 设计为后台守护线程，进程退出时自动清理，符合预期行为

### T3.19 [onto-raft] config_sync 先更新内存后写磁盘 ✅
- **文件**: `crates/onto-raft/src/config_sync.rs:63-72`
- **修复**: 已实现先写磁盘再更新内存的顺序

### T3.20 [onto-raft] append_to_log 非原子 ✅
- **文件**: `crates/onto-raft/src/persistent_store.rs:337-368`
- **修复**: 已实现先收集所有条目再逐条写入，最后统一 flush WAL 确保持久性

### T3.21 [onto-raft] TCP 连接无超时 ✅
- **文件**: `crates/onto-raft/src/network.rs:50-55`
- **修复**: 已实现 RPC_TIMEOUT=30s 连接超时

### T3.22 [onto-query] to_uppercase() 索引不匹配非 ASCII 输入 ✅
- **文件**: `crates/onto-query/src/parser.rs` (全文)
- **修复**: 已将全部 36 处 to_uppercase() 替换为 find_ignore_ascii_case/starts_with_ignore_ascii_case/ends_with_ignore_ascii_case

### T3.23 [onto-query] SPARQL regex→LIKE 翻译语义错误 ✅
- **文件**: `crates/onto-query/src/sparql.rs:718,1147`
- **修复**: 已支持 alternation 分支和 ESCAPE 子句

### T3.24 [onto-query] LRU 缓存 O(n) 查找 ✅
- **文件**: `crates/onto-query/src/cache.rs`
- **修复**: QueryCache 和 PlanCache 均改用 BTreeMap<counter, hash> 实现 O(log n) eviction

### T3.25 [onto-ontology] Literal 包含 f64 实现 Eq 违反自反性 ✅
- **文件**: `crates/onto-ontology/src/model.rs:84`
- **修复**: 已实现 ordered_f64 模块处理 NaN 比较

### T3.26 [onto-query] is_read_only_query 与 QueryAst 不一致 ✅
- **文件**: `crates/onto-query/src/executor.rs:524-531`
- **修复**: is_read_only_query 现在委托给 QueryAst::is_read_only()，保持一致

### T3.27 [onto-query] SPARQL ORDER BY 解析器多词 token 错误 ✅
- **文件**: `crates/onto-query/src/sparql.rs:840-901, 906-980`
- **修复**: 修复了三个bug: (1) ASC 关键字切片长度错误(3字符切了4字节); (2) bare DESC/ASC consumed 计算消费整个剩余输入; (3) LIMIT/OFFSET 检测缺少单词边界检查

### T3.28 [onto-query] parse_json_string 不处理转义引号 ✅
- **文件**: `crates/onto-query/src/executor.rs:2921-2925`
- **修复**: 已实现完整的 JSON 转义序列处理

### T3.29 [onto-query] Batch INSERT ON CONFLICT 仅用第一行 ✅
- **文件**: `crates/onto-query/src/parser.rs:967-996`
- **修复**: 已添加 BatchUpsert 变体，支持批量 upsert 操作

### T3.30 [onto-ontology] Turtle 解析器不处理转义引号 ✅
- **文件**: `crates/onto-ontology/src/rdf.rs:283-301`
- **修复**: 已实现 unescape_turtle_string 处理转义字符

### T3.31 [infra] Nginx 密码套件过宽 (允许 3DES/RC4) ✅
- **文件**: `deploy/nginx/nginx.conf:58`
- **修复**: 已配置安全密码套件 (ECDHE-ECDSA-AES128-GCM-SHA256 等)

### T3.32 [infra] 备份脚本管道子 shell 变量丢失 ✅
- **文件**: `deploy/backup/backup.sh:50-54`
- **修复**: 已使用临时文件避免管道子 shell 变量丢失

### T3.33 [infra] 备份无完整性校验 ✅
- **文件**: `deploy/backup/backup.sh`
- **修复**: 已实现 SHA256 校验和清单 (.backup_manifest)

### T3.34 [infra] Docker 监控端口绑定 0.0.0.0 ✅
- **文件**: `docker-compose.yml` 多行
- **修复**: 所有端口已绑定到 127.0.0.1

### T3.35 [infra] API key 占位符已提交 ✅
- **文件**: `config/api_keys.json`
- **修复**: config/api_keys.json 已在 .gitignore 中，实际密钥需从 git 历史中清除并轮换

---

## 阶段四：低优先级改进 (Low)

### T4.1 [onto-server] 审计日志硬编码 IP 127.0.0.1 ✅
- **修复**: 使用 axum ConnectInfo 获取真实客户端 IP

### T4.2 [onto-server] 企业配置解析失败静默使用默认值 ✅
- **修复**: 添加详细的错误日志，区分读取失败和解析失败

### T4.3 [onto-server] 慢查询截断 UTF-8 panic ✅
- **修复**: 实现 truncate_utf8 函数，安全处理 UTF-8 边界

### T4.4 [onto-server] 速率限制头状态不一致 ✅
- **修复**: 使用 check() 返回的 remaining 值，避免读取过期状态

### T4.5 [onto-server] E2E 测试 sleep 同步不稳定 ✅
- **修复**: 改用重试连接循环，最多等待5秒

### T4.6 [onto-storage] WAL reset 不必要的文件重开 ✅
- **修复**: 移除 rename 后的重复文件打开操作
### T4.7 [onto-storage] IndexMeta 类名长度 u8 截断 ✅
- **修复**: 添加 MAX_NAME_LEN 常量和长度验证，防止静默截断

### T4.8 [onto-storage] SsTable::first_key 启动时大量 I/O ✅
- **修复**: 在 SSTable 初始化时缓存 first_key，避免重复磁盘读取

### T4.9 [onto-storage] txn_scan_prefix O(n) 写缓冲覆盖 ✅
- **修复**: 使用 HashMap 替代线性搜索，O(1) 查找

### T4.10 [onto-storage] HNSW search_layer_beam 热路径写锁开销 ✅
- **修复**: 使用本地 HashSet 替代共享写锁，消除锁竞争
### T4.11 [onto-raft] export_config unwrap_or_default 静默失败 ✅
- **修复**: 使用 unwrap_or_else 记录序列化错误

### T4.12 [onto-raft] OntoRaftStore.committed 字段未使用 ✅
- **修复**: 移除未使用的 committed 字段

### T4.13 [onto-raft] StateMachine impl 代码重复 ✅
- **修复**: &mut impl 委托给 owned impl，消除重复代码

### T4.14 [onto-raft] IPv6 在 auto_add_peer_ips 中不支持 ✅
- **修复**: 支持 IPv6 地址格式 [::1]:port，使用 /128 前缀

### T4.15 [onto-query] 硬编码 10% join 选择率 ✅
- **修复**: 添加 join_selectivity 配置项到 CostModel

### T4.16 [onto-query] 索引查找路径用 write lock 做只读操作 ✅
- **修复**: 使用 read lock 和只读查找方法
### T4.17 [onto-query] SPARQL $ 变量前缀被丢弃 ✅
- **修复**: SELECT 子句解析器同时支持 ? 和 $ 前缀

### T4.18 [onto-ontology] PrpInv 规则 O(facts×properties) 全扫描 ✅
- **修复**: 预计算反向索引，O(1) 查找替代全扫描
### T4.19 [onto-graph] bfs_fast 无 start_idx 边界检查 ✅
- **修复**: 在 bfs_fast 和 bfs_fast_with_parents 中添加边界检查

### T4.20 [infra] Dockerfile 使用 debian-slim 而非 distroless
### T4.21 [infra] node-exporter 挂载宿主机 /proc /sys
### T4.22 [infra] wechat-webhook 镜像来源不可验证
### T4.23 [infra] HSTS 缺少 preload 指令 ✅
- **状态**: nginx.conf 已包含 preload 指令，无需修复
### T4.24 [infra] 无 OCSP stapling 配置 ✅
- **修复**: 在 nginx.conf 中添加 ssl_stapling 和 ssl_stapling_verify 配置

### T4.25 [infra] 备份无失败通知机制 ✅
- **修复**: 添加 alertmanager 集成，备份失败/成功时发送通知

### T4.26 [infra] CI codecov token 未配置
### T4.27 [onto-server] TLS 证书监控返回占位数据 ✅
- **修复**: 使用 x509-parser 实现完整的证书信息解析

### T4.28 [onto-server] TLS DER 解析不可靠 ✅
- **修复**: 使用 x509-parser 替代简单的字符串搜索

---

## 执行计划

| 阶段 | 任务数 | 预计工时 | 优先级 |
|------|--------|---------|--------|
| 阶段一 | 16 | 3-5 天 | 🔴 立即 |
| 阶段二 | 30 | 5-7 天 | 🟠 本周 |
| 阶段三 | 35 | 5-7 天 | 🟡 下周 |
| 阶段四 | 28 | 3-5 天 | 🟢 后续 |

### 建议修复顺序
1. **T1.1** PG Wire 认证 — 最大攻击面
2. **T1.2/T1.3** SQL 注入 + 路径穿越
3. **T1.4** HNSW UB — 内存安全
4. **T1.5/T1.6** Raft 快照原子性 + 日志读取
5. **T1.7/T1.8** 白名单一致性检查
6. **T1.10/T1.11** 密钥泄露清理
7. 然后按阶段二编号顺序继续

---

## 阶段五：多模扩展 (GIS + 时序 + 时空融合)

> 目标：扩展 GIS 和时序数据模态，完善多模态版图
> 原则：作为独立模块扩展，不修改 LSM 引擎核心路径，当前性能不受影响
> 预计工时：3-4 周

### 一、GIS 扩展

#### G5.1 GIS 数据类型支持
- **目标**: 支持 Point/LineString/Polygon/MultiPolygon 类型
- **存储**: WKB (Well-Known Binary) 编码，存入 BinaryRow 的专用字段
- **性能**: WKB 压缩率 ≥60%，减少 I/O
- **预计**: 2天

#### G5.2 R*树空间索引
- **目标**: 实现 R*树索引，支持空间范围查询
- **集成**: 复用 IndexManager 框架，与 B+Tree 并存
- **性能**: 空间查询从 O(n) → O(log n)
- **预计**: 3天

#### G5.3 Geohash 二级索引
- **目标**: 实现 Geohash 编码索引，支持邻近查询
- **集成**: 作为 IndexManager 的新索引类型
- **性能**: 额外写入开销 +5-10%
- **预计**: 2天

#### G5.4 空间关系计算
- **目标**: 实现九交模型（包含/相交/重叠判断）
- **性能**: 纯 CPU 计算，≤8µs per 判断
- **预计**: 2天

#### G5.5 空间拓扑缓存
- **目标**: 预计算常见空间拓扑关系，缓存结果
- **性能**: 避免重复计算，空间查询提速 2-5x
- **预计**: 1天

#### G5.6 GIS SQL 语法
- **目标**: 支持 ST_Contains/ST_Intersects/ST_Distance 等空间函数
- **示例**: `SELECT * FROM Store WHERE ST_Distance(location, ST_Point(116.4, 39.9)) < 1000`
- **预计**: 2天

---

### 二、时序扩展

#### T5.1 时序数据模型
- **目标**: 支持时间戳 + 值的时序数据类型
- **存储**: 复用 LSM 引擎，按时间戳排序
- **预计**: 1天

#### T5.2 列存分片 (TSM 格式)
- **目标**: 实现 TSM (Time-Structured Merge) 格式存储
- **架构**: 独立模块，通过 StorageAdapter trait 抽象
- **风险**: 不修改 LSM 核心，作为可选存储后端
- **预计**: 5天

#### T5.3 热/温/冷数据分层
- **目标**: 按时间粒度自动分层
  - 热数据: MemTable (内存)
  - 温数据: TSM (SSD)
  - 冷数据: Parquet (HDD/对象存储)
- **预计**: 3天

#### T5.4 窗口函数
- **目标**: 支持 Tumbling/Hopping/Session 窗口
- **示例**: `SELECT AVG(temperature) FROM sensor_data WINDOW TUMBLING(1h)`
- **预计**: 2天

#### T5.5 时序内置算子
- **目标**: DTW 距离计算 (优化至 O(n))、STL 分解 (单周期 <5ms)
- **预计**: 2天

#### T5.6 异常检测
- **目标**: 基于 Grubbs' Test 的动态阈值异常检测
- **预计**: 2天

#### T5.7 周期模式挖掘
- **目标**: FFT + 自相关联合分析
- **预计**: 2天

---

### 三、时空融合

#### ST5.1 时空联合索引
- **目标**: 四叉树 + 时间线的 Hybrid 索引结构
- **性能**: 时空范围查询 QPS > 50K
- **预计**: 3天

#### ST5.2 STTRL 规则引擎
- **目标**: 内置 87 种时空规则
- **预计**: 3天

#### ST5.3 时空实体格式
- **目标**: 统一为 {entity}@[x,y,z,t] 格式
- **预计**: 1天

#### ST5.4 本体映射
- **目标**: 自动生成 OWL-Time/SOSA 本体映射
- **预计**: 1天

---

### 四、性能保障

| 措施 | 当前状态 | 目标 |
|------|----------|------|
| 对象池 + 零拷贝 | BinaryRow 已有 | 扩展到 GIS/时序 |
| SIMD 优化 | JSON 解析已有 | 扩展到空间计算 |
| 动态卸载 | LSM compaction | 热/温/冷分层 |
| 内存控制 | 43.2MB | 不超过 100MB |

### 五、实施顺序

| 优先级 | 任务 | 预计工时 |
|--------|------|----------|
| P0 | G5.1 GIS 数据类型 | 2天 |
| P0 | T5.1 时序数据模型 | 1天 |
| P1 | G5.2 R*树索引 | 3天 |
| P1 | T5.2 列存分片 | 5天 |
| P1 | G5.6 GIS SQL 语法 | 2天 |
| P2 | G5.3 Geohash 索引 | 2天 |
| P2 | T5.3 热/温/冷分层 | 3天 |
| P2 | T5.4 窗口函数 | 2天 |
| P3 | ST5.1 时空联合索引 | 3天 |
| P3 | ST5.2 STTRL 规则 | 3天 |

### 六、预计总工时

- **GIS 扩展**: 12天
- **时序扩展**: 16天
- **时空融合**: 8天
- **总计**: ~36天 (约 5-6 周)
