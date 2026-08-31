# OntoDB 命名空间 + 继承修复设计文档

## 设计原则

**命名空间只用于本体层面，不影响文档存储格式。**

## 实现内容

### 1. 命名空间支持（仅本体层面）

**新增语法：**
```sql
-- 创建命名空间
CREATE NAMESPACE hr;

-- 删除命名空间
DROP NAMESPACE hr;

-- 切换当前命名空间
USE NAMESPACE hr;
```

**存储格式：**
```
__ns__hr                    ← 命名空间元数据
__ontology__hr::shop        ← 命名空间内的本体
__ontology__hr::Person      ← 命名空间内的本体
Product::doc1               ← 文档（保持原格式，不加命名空间前缀）
```

### 2. 继承修复

**之前的问题：**
```sql
CREATE CLASS Person                    → 本体 "Person"
CREATE CLASS Employee EXTENDS Person   → ❌ 失败！找不到 Person
```

**修复后：**
```sql
CREATE CLASS Person                    → 本体 "_default::Person"
CREATE CLASS Employee EXTENDS Person   → ✅ 成功！自动查找全局本体
```

**实现方式：**
- 创建本体时，合并同命名空间内所有已有本体
- 验证继承引用时，检查合并后的本体集合
- 文档键保持 `class::id` 格式不变

### 3. 多项目组织

```sql
-- 项目A的本体
CREATE NAMESPACE projectA;
CREATE ONTOLOGY projectA.user_mgmt (
    CLASS User,
    CLASS Admin EXTENDS User
);

-- 项目B的本体
CREATE NAMESPACE projectB;
CREATE ONTOLOGY projectB.products (
    CLASS Product,
    CLASS Order
);
```

**注意：** 文档仍然使用 `class::id` 格式，不同项目的同名类会共享文档空间。如果需要完全隔离，应使用独立数据库实例。

## 修改的文件

| 文件 | 变更 |
|------|------|
| `crates/onto-ontology/src/model.rs` | 添加 `Namespace` 结构体，`Ontology` 增加 `namespace` 字段 |
| `crates/onto-ontology/src/store.rs` | 支持命名空间键格式，添加命名空间 CRUD 操作 |
| `crates/onto-ontology/src/lib.rs` | 导出新类型 |
| `crates/onto-query/src/ontoql.rs` | 添加 `CREATE/DROP/USE NAMESPACE` 解析 |
| `crates/onto-query/src/parser.rs` | 添加 `QueryAst` 命名空间变体 |
| `crates/onto-query/src/executor.rs` | 命名空间操作处理，继承验证修复 |

## 迁移工具

**位置：** `tools/migrate-namespace/`

**用法：**
```bash
# 预览迁移
cargo run --bin migrate-namespace -- --data-dir ./data --dry-run

# 执行迁移（仅迁移本体）
cargo run --bin migrate-namespace -- --data-dir ./data

# 回滚迁移
cargo run --bin rollback-namespace -- ./data
```

## 向后兼容

- ✅ 文档键格式不变（`class::id`）
- ✅ 查询代码无需修改
- ✅ 现有数据无需迁移文档
- ✅ 本体自动支持命名空间（可选）

## 限制

1. 文档层面没有命名空间隔离
2. 不同命名空间的同名类会共享文档空间
3. 如需完全隔离，建议使用独立数据库实例
