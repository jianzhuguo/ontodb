# Changelog

## [Unreleased]

### feat: BinaryRow 二进制行格式 — 全扫描路径集成 + 性能基准测试

**核心改动：**
- 新增 `onto-core::binary_row` 模块：紧凑二进制行格式（u16字段数 + 类型标签 + 偏移量索引），支持 Null/Bool/Int/Float/String/Object/Array
- `onto-storage::engine::parse_doc_bytes`：存储层统一入口，BinaryRow 优先 → JSON fallback
- `onto-query::executor` 全部扫描路径集成 BinaryRow：
  - `plan_seq_scan` / `plan_seq_scan_read`：三级优化（fast byte reject → BinaryRow filter → JSON fallback）
  - `plan_index_scan` / `plan_index_scan_read`：通过 `fetch_rows_by_pks` → `simd_parse_row`
  - `plan_index_lookup` / `plan_index_lookup_read`：同上
  - `execute_match_read`：BinaryRow 优先 + `class_in_hierarchy` 快速过滤
  - `execute_vector_search_txn`：filter 扫描 + 结果获取均走 BinaryRow 路径
  - `execute_analyze` / `execute_analyze_read`：统计收集走 BinaryRow

**性能基准（5000 行，debug build）：**

| 操作 | BinaryRow | JSON | 加速比 |
|------|-----------|------|--------|
| parse + to_map | 4.3 us | 5.6 us | 1.30x |
| 字段查找 (find_field) | 1.3 us | 5.8 us | 4.59x |
| 过滤求值 (price > 5000) | 1.0 us | 5.7 us | 5.45x |

端到端查询延迟：
- 全表扫描 65ms，带过滤 40-44ms（read path 39ms）
- MATCH 语义查询 54-64ms
- Index scan 44ms

**文件：**
- `crates/onto-core/src/binary_row.rs`（新）
- `crates/onto-core/src/lib.rs`
- `crates/onto-query/src/executor.rs`
- `crates/onto-query/src/binary_row_bench.rs`（新）
- `crates/onto-query/src/lib.rs`
- `crates/onto-storage/src/engine.rs`
