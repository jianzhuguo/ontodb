# OntoDB 补强计划 Checklist

> **目标**：补齐 OntoDB 相对 PostgreSQL 的关键欠缺能力
> **基线**：当前 OntoDB v0.6.2 + 活数据升级前基线
> **优先级**：运维能力 > 查询能力 > 生态能力

---

## 第一阶段：运维能力补齐（1-2 周）— 大部分已完成

### 1.1 逻辑备份工具（ontodb-dump）— ✅ 已完成
- [x] 设计备份格式（JSON Lines / CSV）— 已实现 JSONL + CSV
- [x] 实现 `onto-cli dump` 命令：全库导出
- [x] 实现 `onto-cli dump --class BioTask`：单表导出
- [x] 实现 `onto-cli dump --format json|csv`：格式选择
- [x] 实现 `onto-cli dump --output backup.json`：输出到文件
- [ ] 支持 HTTP API 导出端点：`GET /api/export`
- [ ] 测试：全库导出 → 清空 → 恢复 → 数据一致（需要运行服务器）

### 1.2 逻辑恢复工具（ontodb-restore）
- [x] 实现 `onto-cli restore` 命令：从备份文件恢复
- [x] 实现 `onto-cli restore --file backup.json`：指定文件
- [x] 实现 `onto-cli restore --class BioTask`：单表恢复
- [ ] 支持 HTTP API 恢复端点：`POST /api/import`
- [ ] 测试：备份 → 恢复 → 查询验证数据完整性（需要运行服务器）

### 1.3 WAL 归档（增量备份基础）
- [x] 设计 WAL 归档策略（文件轮转 + 归档目录）✅
- [x] 实现 WAL 归档后台任务（flush 时自动归档）✅
- [x] 配置项：`wal_archive_dir`、`wal_archive_max_files` ✅
- [x] 自动清理旧归档文件 ✅
- [x] 测试：归档目录下有 WAL 文件 + 清理逻辑 ✅ (2 个测试通过)

### 1.4 自动备份脚本
- [ ] 提供 `scripts/backup.sh` 模板
- [ ] 支持 crontab 定时备份
- [ ] 支持备份保留天数配置
- [ ] 文档：备份恢复操作手册

---

## 第二阶段：查询能力增强（3-4 周）

### 2.1 窗口函数
- [x] `ROW_NUMBER() OVER (PARTITION BY ... ORDER BY ...)` ✅
- [x] `RANK() OVER (...)` ✅
- [x] `DENSE_RANK() OVER (...)` ✅
- [x] `LAG(col, n) OVER (...)` ✅
- [x] `LEAD(col, n) OVER (...)` ✅
- [x] `SUM/AVG/COUNT/MIN/MAX(col) OVER (PARTITION BY ... ORDER BY ... ROWS BETWEEN ...)` ✅
- [x] 测试：各窗口函数的正确性和边界情况 ✅ (5 个测试通过)

### 2.2 CTE（WITH 子句）
- [x] 普通 CTE：`WITH t AS (SELECT ...) SELECT FROM t` ✅
- [x] 递归 CTE：`WITH RECURSIVE t AS (...) SELECT FROM t` ✅
- [x] 多 CTE 链式引用 ✅
- [x] 测试：CTE 嵌套、递归深度限制 ✅ (2 个测试通过)

### 2.3 唯一约束
- [x] 建表时支持 `UNIQUE(col)` 语法 ✅
- [x] INSERT/UPDATE 时检查唯一性 ✅
- [x] 唯一索引实现（扫描校验）✅
- [x] 测试：重复插入被拒绝、更新冲突检测 ✅ (6 个测试通过)

### 2.4 布尔表达式增强
- [ ] `IN (val1, val2, ...)` 支持
- [ ] `BETWEEN val1 AND val2` 支持
- [ ] `IS NULL / IS NOT NULL` 支持（如果缺失）
- [ ] 测试：各表达式的正确性

---

## 第三阶段：生态补齐（持续）

### 3.1 ORM 对接（Python SQLAlchemy）
- [ ] 编写 SQLAlchemy dialect（ontodb:// 协议）
- [ ] 支持基本 CRUD 操作
- [ ] 发布到 PyPI：`sqlalchemy-ontodb`
- [ ] 测试：SQLAlchemy ORM 增删改查

### 3.2 ORM 对接（Rust SQLx）
- [ ] 编写 SQLx driver
- [ ] 支持 `sqlx::query!` 宏
- [ ] 测试：SQLx 基本操作

### 3.3 HTTP API 增强
- [ ] 批量操作 API：`POST /api/batch`
- [ ] 事务 API：`POST /api/transaction/begin` → `commit` / `rollback`
- [ ] 游标 API：`POST /api/cursor`（大结果集分页）
- [ ] OpenAPI 文档自动生成

### 3.4 客户端 SDK 增强
- [ ] Python SDK：补充 ORM 集成文档
- [ ] Go SDK：补充 context 支持
- [ ] JavaScript/TypeScript SDK：补充 Promise/async 支持

---

## 排除项（不做）

- ~~存储过程~~（应用层处理，趋势下降）
- ~~触发器~~（微服务架构下使用率极低）
- ~~完整 SQL 标准兼容~~（OntoQL 差异化更有价值）
- ~~物化视图~~（低优先级，后续考虑）
- ~~外键约束~~（应用层保证，LSM-Tree 实现成本高）

---

## 工作量总结

| 阶段 | 内容 | 工作量 |
|------|------|--------|
| 第一阶段 | 逻辑备份/恢复 + WAL 归档 | 1-2 周 |
| 第二阶段 | 窗口函数 + CTE + 唯一约束 | 3-4 周 |
| 第三阶段 | ORM + HTTP API 增强 | 持续 |
| **合计** | | **5-7 周** |

---

## 实施顺序

```
1. [DONE] ontodb-dump 逻辑备份工具
2. [DONE] ontodb-restore 恢复工具
3. [DONE] 窗口函数 ROW_NUMBER / RANK / DENSE_RANK / LAG / LEAD / 聚合
4. [DONE] CTE WITH 子句（含递归 CTE）
5. [DONE] 唯一约束（单列 + 复合唯一约束）
6. [DONE] WAL 归档（自动归档 + 清理）
7. [TODO] ORM 对接（持续）
```

---

**© 2026 原点价值 / OntoValue Technology**
