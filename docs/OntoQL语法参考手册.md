# OntoQL 语法参考手册

> **版本**: OntoDB v0.7.0  
> **日期**: 2026-08-28  
> **状态**: 基于代码实现的完整语法文档

---

## 目录

1. [概述](#1-概述)
2. [数据类型](#2-数据类型)
3. [CREATE ONTOLOGY — 本体定义](#3-create-ontology--本体定义)
4. [CLASS 定义](#4-class-定义)
5. [PROPERTY 定义](#5-property-定义)
6. [UNIQUE 约束](#6-unique-约束)
7. [INSERT / UPDATE / DELETE](#7-insert--update--delete)
8. [SELECT 查询](#8-select-查询)
9. [MATCH 查询](#9-match-查询)
10. [GRAPH MATCH 查询](#10-graph-match-查询)
11. [子类自动展开规则](#11-子类自动展开规则)
12. [传递属性自动展开规则](#12-传递属性自动展开规则)
13. [逆属性自动推导规则](#13-逆属性自动推导规则)
14. [对称属性自动推导规则](#14-对称属性自动推导规则)
15. [等价类/等价属性映射规则](#15-等价类等价属性映射规则)
16. [混合查询（语义 + 结构 + 向量）](#16-混合查询语义--结构--向量)
17. [跨模态融合查询](#17-跨模态融合查询)
18. [活数据（衰减/激活/DBA 视图）](#18-活数据衰减激活dba-视图)
19. [本体推理规则详解（7 条 OWL 2 RL 规则）](#19-本体推理规则详解7-条-owl-2-rl-规则)
20. [示例库](#20-示例库)
21. [迁移指南](#21-迁移指南)

---

## 1. 概述

OntoQL 是 OntoDB 的本体驱动查询语言，在标准 SQL 基础上扩展了语义查询能力。它支持：

- **本体定义**：声明类层次、属性约束、推理规则
- **语义查询**：基于类继承的自动展开查询
- **图查询**：GRAPH MATCH 模式匹配
- **向量搜索**：HNSW 向量索引 + 相似度搜索
- **混合查询**：SQL 过滤 + 向量相似度 + 语义推理的融合查询
- **自动推理**：7 条 OWL 2 RL 规则，查询时自动推导隐含知识

### 核心优势

| 特性 | 传统 SQL | OntoQL |
|------|----------|--------|
| 模式定义 | DDL (CREATE TABLE) | 本体 (CREATE ONTOLOGY) |
| 继承关系 | 应用层维护 | TBox 声明式定义 |
| 查询展开 | 手动 JOIN | 自动类层次展开 |
| 类型判断 | CASE WHEN / 应用层 | `__class__` 自动标注 |
| 属性继承 | 需要 UNION | 自动继承 |
| 推理能力 | 无 | 7 条 OWL 2 RL 规则 |

---

## 2. 数据类型

OntoDB 支持以下数据类型：

| 类型 | 别名 | 说明 |
|------|------|------|
| `STRING` | `STR`, `VARCHAR`, `TEXT` | 字符串 |
| `INT64` | `INT`, `INTEGER`, `BIGINT` | 64 位整数 |
| `FLOAT64` | `FLOAT`, `DOUBLE`, `DECIMAL`, `NUMERIC` | 64 位浮点数 |
| `BOOL` | `BOOLEAN` | 布尔值 |
| `BYTES` | `BINARY`, `BLOB` | 二进制数据 |
| `ARRAY` | `LIST` | 数组（用于向量存储） |
| `OBJECT` | `MAP`, `JSON` | JSON 对象 |

---

## 3. CREATE ONTOLOGY — 本体定义

### 语法

```sql
CREATE ONTOLOGY <ontology_name> (
  <class_definitions>,
  <property_definitions>
)
```

### 完整示例

```sql
CREATE ONTOLOGY company (
  -- 类定义
  CLASS Person,
  CLASS Employee SUBCLASS OF Person,
  CLASS Manager SUBCLASS OF Employee,
  CLASS Developer SUBCLASS OF Employee,
  
  -- 属性定义
  PROPERTY name ON Person TYPE STRING REQUIRED,
  PROPERTY age ON Person TYPE INT64,
  PROPERTY email ON Person TYPE STRING,
  PROPERTY salary ON Employee TYPE FLOAT64,
  PROPERTY department ON Employee TYPE STRING,
  PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages,
  PROPERTY manages ON Manager TYPE STRING INVERSE OF reportsTo,
  PROPERTY ancestor ON Person TYPE STRING TRANSITIVE,
  PROPERTY friendOf ON Person TYPE STRING SYMMETRIC,
  PROPERTY worksUnder ON Employee TYPE STRING SUBPROPERTY OF reportsTo
)
```

### 语义说明

- `CREATE ONTOLOGY` 定义一个命名的本体，包含类和属性
- 本体存储在 LSM-Tree 存储引擎中，键前缀为 `__ontology__`
- 支持多个本体共存，查询时自动查找相关本体

---

## 4. CLASS 定义

### 基本语法

```sql
CLASS <class_name>
CLASS <class_name> SUBCLASS OF <parent_class>
CLASS <class_name> SUBCLASS OF <parent1>, <parent2>  -- 多继承
```

### 类层次关系

```sql
CLASS Animal,
CLASS Dog SUBCLASS OF Animal,
CLASS Cat SUBCLASS OF Animal,
CLASS Husky SUBCLASS OF Dog
```

**继承链**: `Husky → Dog → Animal`

### EQUIVALENT TO — 等价类

```sql
CLASS Worker,
CLASS Employee,
CLASS Worker EQUIVALENT TO Employee
```

**语义**: Worker 和 Employee 是同一概念的不同名称。查询 `Worker` 时自动包含 `Employee` 的实例。

### DISJOINT WITH — 不相交类

```sql
CLASS Dog,
CLASS Cat,
CLASS Dog DISJOINT WITH Cat,
CLASS Cat DISJOINT WITH Dog
```

**语义**: 一个实体不能同时是 Dog 和 Cat。推理引擎会在一致性检查时检测违反情况。

### 类类型

```sql
-- 普通类（默认）
CLASS Person,

-- 枚举类
CLASS Season ENUM ('Spring', 'Summer', 'Autumn', 'Winter'),

-- 联合类
CLASS WorkingStudent UNION (Employee, Student),

-- 交集类
CLASS WorkingStudent INTERSECTION (Employee, Student)
```

### OWL 限制（Restrictions）

```sql
-- 至少有一个值来自指定类
CLASS Parent (
  hasChild SOME VALUES FROM Person
)

-- 所有值必须来自指定类
CLASS PetOwner (
  hasPet ALL VALUES FROM Animal
)

-- 属性必须有特定值
CLASS USCitizen (
  nationality HAS VALUE 'US'
)

-- 最小基数
CLASS Parent (
  hasChild MIN CARDINALITY 1
)

-- 最大基数
CLASS Couple (
  hasSpouse MAX CARDINALITY 1
)

-- 精确基数
CLASS Couple (
  hasSpouse EXACT CARDINALITY 1
)
```

---

## 5. PROPERTY 定义

### 基本语法

```sql
PROPERTY <name> ON <domain_class> TYPE <data_type> [constraints...]
```

### 完整约束列表

```sql
-- 必填属性
PROPERTY name ON Person TYPE STRING REQUIRED

-- 多值属性
PROPERTY hobbies ON Person TYPE STRING MULTI_VALUED

-- 传递属性
PROPERTY ancestor ON Person TYPE STRING TRANSITIVE

-- 对称属性
PROPERTY friendOf ON Person TYPE STRING SYMMETRIC

-- 函数属性（每个实例最多一个值）
PROPERTY ssn ON Person TYPE STRING FUNCTIONAL

-- 逆属性
PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages
PROPERTY manages ON Manager TYPE STRING INVERSE OF reportsTo

-- 子属性
PROPERTY worksUnder ON Employee TYPE STRING SUBPROPERTY OF reportsTo

-- 等价属性
PROPERTY email ON Person TYPE STRING EQUIVALENT PROPERTY emailAddress
```

### 属性约束详解

| 约束 | 说明 | 推理效果 |
|------|------|----------|
| `REQUIRED` | 必填，INSERT 时验证 | 写入时校验 |
| `MULTI_VALUED` | 可有多个值 | 无特殊推理 |
| `TRANSITIVE` | 传递性 | 自动推导传递闭包 |
| `SYMMETRIC` | 对称性 | 自动推导反向关系 |
| `FUNCTIONAL` | 函数性（最多一个值） | 一致性检查 |
| `INVERSE OF` | 逆属性 | 自动推导反向三元组 |
| `SUBPROPERTY OF` | 子属性 | 自动提升为父属性 |
| `EQUIVALENT PROPERTY` | 等价属性 | 自动映射 |

---

## 6. UNIQUE 约束

### 单列唯一约束

```sql
CLASS User (
  email STRING UNIQUE,
  username STRING UNIQUE
)
```

### 复合唯一约束

```sql
CLASS Enrollment (
  studentId STRING,
  courseId STRING,
  UNIQUE (studentId, courseId)
)
```

### 约束验证

INSERT 时自动验证唯一约束，违反时返回错误：

```json
{
  "success": false,
  "error": "Unique constraint violation: email already exists"
}
```

---

## 7. INSERT / UPDATE / DELETE

### INSERT — 插入数据

#### 基本语法

```sql
INSERT INTO <class_name> (<columns>) VALUES (<values>)
```

#### 示例

```sql
-- 插入单条记录
INSERT INTO Person (name, age, email) VALUES ('Alice', 30, 'alice@example.com')

-- 插入员工（自动继承 Person 属性）
INSERT INTO Employee (name, age, email, salary, department) 
VALUES ('Bob', 35, 'bob@example.com', 80000, 'Engineering')

-- 批量插入
INSERT INTO Person (name, age) VALUES 
('Charlie', 25),
('David', 28),
('Eve', 32)

-- UPSERT（存在则更新）
INSERT INTO Person (name, age, email) VALUES ('Alice', 31, 'alice@new.com')
ON CONFLICT (name) DO UPDATE SET age = 31, email = 'alice@new.com'

-- 批量 UPSERT
INSERT INTO Person (name, age) VALUES 
('Alice', 31),
('Bob', 36)
ON CONFLICT (name) DO UPDATE SET age = VALUES.age
```

#### INSERT INTO ... SELECT

```sql
INSERT INTO Employee (name, age, email, salary)
SELECT name, age, email, 50000 FROM Person WHERE age > 25
```

### UPDATE — 更新数据

```sql
UPDATE <class_name> SET <column> = <value> [, ...] [WHERE <condition>]
```

#### 示例

```sql
-- 更新单个字段
UPDATE Person SET age = 31 WHERE name = 'Alice'

-- 更新多个字段
UPDATE Employee SET salary = 90000, department = 'Management' 
WHERE name = 'Bob'

-- 条件更新
UPDATE Product SET price = price * 0.9 WHERE category = 'Electronics'
```

### DELETE — 删除数据

```sql
DELETE FROM <class_name> [WHERE <condition>]
```

#### 示例

```sql
-- 删除特定记录
DELETE FROM Person WHERE name = 'Alice'

-- 条件删除
DELETE FROM Product WHERE price < 10

-- 删除所有记录（危险操作）
DELETE FROM Person
```

---

## 8. SELECT 查询

### 基本语法

```sql
SELECT [DISTINCT] <columns>
FROM <class_name> [AS <alias>]
[JOIN <table> [AS <alias>] ON <condition>]
[WHERE <condition>]
[GROUP BY <columns>]
[HAVING <condition>]
[ORDER BY <column> [ASC|DESC] [, ...]]
[LIMIT <n> [OFFSET <m>]]
```

### 列选择

```sql
-- 选择所有列
SELECT * FROM Person

-- 选择特定列
SELECT name, age FROM Person

-- 带别名
SELECT name AS 姓名, age AS 年龄 FROM Person

-- DISTINCT
SELECT DISTINCT department FROM Employee

-- 聚合函数
SELECT 
  COUNT(*) AS total,
  AVG(age) AS avg_age,
  MAX(salary) AS max_salary,
  MIN(salary) AS min_salary,
  SUM(salary) AS total_salary
FROM Employee

-- 表达式
SELECT name, age * 2 AS double_age FROM Person

-- 内置函数
SELECT 
  COALESCE(nickname, name) AS display_name,
  CONCAT(first_name, ' ', last_name) AS full_name,
  UPPER(email) AS email_upper,
  LENGTH(name) AS name_length,
  ABS(balance) AS abs_balance,
  ROUND(price, 2) AS rounded_price
FROM User
```

### WHERE 条件

#### 比较运算符

```sql
-- 等于
SELECT * FROM Person WHERE name = 'Alice'

-- 不等于
SELECT * FROM Person WHERE status != 'inactive'
SELECT * FROM Person WHERE status <> 'inactive'

-- 大于/小于
SELECT * FROM Product WHERE price > 100
SELECT * FROM Product WHERE price < 50
SELECT * FROM Product WHERE price >= 100
SELECT * FROM Product WHERE price <= 50

-- BETWEEN
SELECT * FROM Product WHERE price BETWEEN 10 AND 100

-- IN
SELECT * FROM Person WHERE age IN (25, 30, 35)

-- IN 子查询
SELECT * FROM Employee WHERE department IN (
  SELECT name FROM Department WHERE location = 'Beijing'
)

-- LIKE 模式匹配
SELECT * FROM Person WHERE name LIKE 'A%'
SELECT * FROM Person WHERE email LIKE '%@example.com'

-- IS NULL / IS NOT NULL
SELECT * FROM Person WHERE email IS NULL
SELECT * FROM Person WHERE email IS NOT NULL
```

#### 逻辑运算符

```sql
-- AND
SELECT * FROM Product WHERE price > 100 AND category = 'Electronics'

-- OR
SELECT * FROM Person WHERE age < 25 OR age > 60

-- NOT
SELECT * FROM Person WHERE NOT (age < 18)

-- 组合（AND 优先级高于 OR）
SELECT * FROM Product 
WHERE (category = 'Electronics' OR category = 'Books') AND price > 50

-- EXISTS
SELECT * FROM Employee e WHERE EXISTS (
  SELECT 1 FROM Department d WHERE d.manager_id = e.id
)

-- NOT EXISTS
SELECT * FROM Employee e WHERE NOT EXISTS (
  SELECT 1 FROM Project p WHERE p.lead_id = e.id
)
```

### JOIN — 连接查询

```sql
-- INNER JOIN
SELECT e.name, d.name AS department
FROM Employee e
JOIN Department d ON e.department_id = d.id

-- LEFT JOIN
SELECT e.name, p.name AS project
FROM Employee e
LEFT JOIN Project p ON e.id = p.lead_id

-- RIGHT JOIN
SELECT e.name, d.name AS department
FROM Employee e
RIGHT JOIN Department d ON e.department_id = d.id

-- FULL OUTER JOIN
SELECT e.name, d.name AS department
FROM Employee e
FULL OUTER JOIN Department d ON e.department_id = d.id

-- 多表连接
SELECT e.name, d.name AS department, p.name AS project
FROM Employee e
JOIN Department d ON e.department_id = d.id
LEFT JOIN Project p ON e.id = p.lead_id
```

### GROUP BY — 分组聚合

```sql
-- 基本分组
SELECT department, COUNT(*) AS count
FROM Employee
GROUP BY department

-- 带 HAVING 过滤
SELECT department, AVG(salary) AS avg_salary
FROM Employee
GROUP BY department
HAVING AVG(salary) > 50000

-- 多列分组
SELECT department, title, COUNT(*) AS count
FROM Employee
GROUP BY department, title
```

### ORDER BY — 排序

```sql
-- 单列排序
SELECT * FROM Person ORDER BY age

-- 降序
SELECT * FROM Product ORDER BY price DESC

-- 多列排序
SELECT * FROM Employee ORDER BY department ASC, salary DESC

-- 带 LIMIT
SELECT * FROM Product ORDER BY price DESC LIMIT 10

-- 带 OFFSET（分页）
SELECT * FROM Product ORDER BY price DESC LIMIT 10 OFFSET 20
```

### UNION — 合并查询

```sql
-- UNION（去重）
SELECT name FROM Employee WHERE department = 'Engineering'
UNION
SELECT name FROM Employee WHERE department = 'Sales'

-- UNION ALL（保留重复）
SELECT name FROM Employee WHERE department = 'Engineering'
UNION ALL
SELECT name FROM Employee WHERE department = 'Sales'
```

### WITH — 公共表表达式（CTE）

```sql
-- 基本 CTE
WITH high_salary AS (
  SELECT * FROM Employee WHERE salary > 100000
)
SELECT name, salary FROM high_salary ORDER BY salary DESC

-- 递归 CTE
WITH RECURSIVE org_tree AS (
  SELECT id, name, manager_id, 1 AS level
  FROM Employee WHERE manager_id IS NULL
  UNION ALL
  SELECT e.id, e.name, e.manager_id, t.level + 1
  FROM Employee e
  JOIN org_tree t ON e.manager_id = t.id
)
SELECT * FROM org_tree ORDER BY level
```

### 窗口函数

```sql
-- ROW_NUMBER
SELECT 
  name,
  department,
  salary,
  ROW_NUMBER() OVER (PARTITION BY department ORDER BY salary DESC) AS rank
FROM Employee

-- RANK / DENSE_RANK
SELECT 
  name,
  salary,
  RANK() OVER (ORDER BY salary DESC) AS rank,
  DENSE_RANK() OVER (ORDER BY salary DESC) AS dense_rank
FROM Employee

-- LAG / LEAD
SELECT 
  name,
  salary,
  LAG(salary, 1, 0) OVER (ORDER BY hire_date) AS prev_salary,
  LEAD(salary, 1, 0) OVER (ORDER BY hire_date) AS next_salary
FROM Employee

-- 累计求和
SELECT 
  name,
  salary,
  SUM(salary) OVER (ORDER BY hire_date) AS running_total
FROM Employee

-- 移动平均
SELECT 
  name,
  salary,
  AVG(salary) OVER (
    ORDER BY hire_date 
    ROWS BETWEEN 2 PRECEDING AND CURRENT ROW
  ) AS moving_avg
FROM Employee
```

### EXPLAIN — 执行计划

```sql
-- 查看执行计划
EXPLAIN SELECT * FROM Person WHERE age > 25

-- 带分析的执行计划
EXPLAIN ANALYZE SELECT * FROM Person WHERE age > 25
```

---

## 9. MATCH 查询

MATCH 是 OntoQL 的语义查询扩展，基于类进行查询，自动处理继承关系。

### 基本语法

```sql
MATCH (<variable>: <ClassName>) [WHERE <condition>] RETURN <columns>
```

### 示例

```sql
-- 查询所有 Person
MATCH (p: Person) RETURN name, age

-- 带条件过滤
MATCH (p: Person) WHERE age > 25 RETURN name, age

-- 查询 Employee（自动包含 Manager、Developer 等子类）
MATCH (e: Employee) WHERE salary > 50000 RETURN name, salary, department

-- 查询 Manager（只返回 Manager 实例）
MATCH (m: Manager) RETURN name, department
```

### MATCH vs SELECT

```sql
-- 这两个查询等价（在存在本体定义时）
MATCH (e: Employee) WHERE age > 30 RETURN name, age
SELECT name, age FROM Employee WHERE age > 30

-- 但 MATCH 更语义化，明确表达了"查询 Employee 类的实例"
```

### 语义扩展

MATCH 查询会自动应用本体推理：

1. **子类展开**: 查询 `Employee` 时自动包含 `Manager`、`Developer` 等子类
2. **等价类映射**: 如果 `Worker EQUIVALENT TO Employee`，查询 `Worker` 也返回 `Employee` 实例
3. **传递属性**: 查询传递属性时自动推导传递闭包
4. **逆属性**: 查询逆属性时自动推导反向关系

---

## 10. GRAPH MATCH 查询

GRAPH MATCH 用于图结构查询，支持模式匹配和路径遍历。

### 基本语法

```sql
GRAPH MATCH (<node_var>: <label>) -[<edge_var>: <edge_label>]-> (<node_var>: <label>)
[WHERE <condition>]
RETURN <columns>
```

### 示例

```sql
-- 查找所有 "认识" 关系
GRAPH MATCH (a:Person) -[e:knows]-> (b:Person)
RETURN a.name, b.name

-- 带条件过滤
GRAPH MATCH (a:Person) -[e:knows]-> (b:Person)
WHERE a.age > 25
RETURN a.name, b.name, e.since

-- 多跳查询
GRAPH MATCH (a:Person) -[e1:knows]-> (b:Person) -[e2:knows]-> (c:Person)
RETURN a.name, b.name, c.name

-- 无向边（对称属性自动处理）
GRAPH MATCH (a:Person) -[e:friendOf]-> (b:Person)
RETURN a.name, b.name
```

### 图遍历

```sql
-- 从指定节点开始遍历
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3

-- 向内遍历
GRAPH TRAVERSE FROM 'Person::1' IN LABEL 'manages' DEPTH 2

-- 双向遍历
GRAPH TRAVERSE FROM 'Person::1' BOTH LABEL 'friendOf' DEPTH 1

-- 带条件过滤
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3
WHERE age > 25
```

### 最短路径

```sql
-- 查找两个节点之间的最短路径
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5'

-- 限制最大深度
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5' MAX DEPTH 5
```

### 图算法

OntoDB 内置 6 种图算法：

| 算法 | 说明 | 用途 |
|------|------|------|
| PageRank | 节点重要性排序 | 社交网络影响力分析 |
| Shortest Path | 最短路径查找 | 路径规划 |
| Connected Components | 连通分量 | 社区发现 |
| Triangle Count | 三角形计数 | 网络密度分析 |
| BFS | 广度优先搜索 | 层次遍历 |
| Louvain | 社区检测 | 社交网络聚类 |

---

## 11. 子类自动展开规则

### 规则说明

当查询一个类时，OntoDB 自动展开所有子类，无需手动 JOIN 或 UNION。

### 示例

```sql
-- 本体定义
CREATE ONTOLOGY animals (
  CLASS Animal,
  CLASS Dog SUBCLASS OF Animal,
  CLASS Cat SUBCLASS OF Animal,
  CLASS Husky SUBCLASS OF Dog,
  CLASS Persian SUBCLASS OF Cat
)

-- 查询 Animal 时自动包含所有子类
SELECT * FROM Animal
-- 实际返回: Animal, Dog, Cat, Husky, Persian 的所有实例

-- 查询 Dog 时自动包含 Husky
SELECT * FROM Dog
-- 实际返回: Dog, Husky 的所有实例
```

### 展开机制

1. **查询解析**: 识别查询中的类名
2. **本体查找**: 从存储引擎加载本体定义
3. **类层次构建**: 构建完整的类继承树
4. **子类收集**: 递归收集所有子类（包括等价类）
5. **多类扫描**: 扫描所有相关类的数据
6. **结果合并**: 合并结果并添加 `__class__` 标注

### 性能优化

- **前缀扫描**: 使用 LSM-Tree 前缀扫描，高效读取多个类的数据
- **索引利用**: 自动利用类级别的索引
- **流式处理**: 避免一次性加载所有数据到内存

---

## 12. 传递属性自动展开规则

### 规则说明

标记为 `TRANSITIVE` 的属性会自动推导传递闭包。

### 示例

```sql
-- 定义传递属性
CREATE ONTOLOGY family (
  CLASS Person,
  PROPERTY ancestor ON Person TYPE STRING TRANSITIVE
)

-- 插入数据
INSERT INTO Person (name) VALUES ('Alice'), ('Bob'), ('Charlie'), ('Dave')
INSERT INTO ancestor (subject, object) VALUES 
  ('Alice', 'Bob'),
  ('Bob', 'Charlie'),
  ('Charlie', 'Dave')

-- 查询时自动推导传递闭包
SELECT * FROM ancestor WHERE subject = 'Alice'
-- 返回: Bob, Charlie, Dave（自动推导 Alice → Bob → Charlie → Dave）
```

### 推理过程

```
原始事实:
  Alice ancestor Bob
  Bob ancestor Charlie
  Charlie ancestor Dave

推理结果:
  Alice ancestor Charlie  (Alice→Bob + Bob→Charlie)
  Alice ancestor Dave     (Alice→Bob + Bob→Charlie + Charlie→Dave)
  Bob ancestor Dave       (Bob→Charlie + Charlie→Dave)
```

### 性能优化

- **增量推理**: 只对新增的三元组应用传递规则
- **索引优化**: 使用 `obj_map` 和 `subj_map` 快速查找
- **缓存机制**: 传递闭包结果可缓存

---

## 13. 逆属性自动推导规则

### 规则说明

标记为 `INVERSE OF` 的属性会自动推导反向关系。

### 示例

```sql
-- 定义逆属性
CREATE ONTOLOGY company (
  CLASS Person,
  CLASS Employee SUBCLASS OF Person,
  CLASS Manager SUBCLASS OF Employee,
  PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages,
  PROPERTY manages ON Manager TYPE STRING INVERSE OF reportsTo
)

-- 插入数据
INSERT INTO Manager (name) VALUES ('Alice')
INSERT INTO Employee (name, reportsTo) VALUES ('Bob', 'Alice')

-- 查询 manages 时自动推导
SELECT * FROM manages
-- 返回: Alice manages Bob（自动从 Bob reportsTo Alice 推导）

-- 查询 reportsTo 时自动推导
SELECT * FROM reportsTo
-- 返回: Bob reportsTo Alice（自动从 Alice manages Bob 推导）
```

### 推理过程

```
原始事实:
  Bob reportsTo Alice

推理结果:
  Alice manages Bob  (逆属性推导)
```

### 双向推导

逆属性推导是双向的：
- 如果定义 `P INVERSE OF Q`，则 `x P y` → `y Q x`
- 如果定义 `Q INVERSE OF P`，则 `x Q y` → `y P x`

---

## 14. 对称属性自动推导规则

### 规则说明

标记为 `SYMMETRIC` 的属性会自动推导反向关系。

### 示例

```sql
-- 定义对称属性
CREATE ONTOLOGY social (
  CLASS Person,
  PROPERTY friendOf ON Person TYPE STRING SYMMETRIC
)

-- 插入数据
INSERT INTO Person (name) VALUES ('Alice'), ('Bob')
INSERT INTO friendOf (subject, object) VALUES ('Alice', 'Bob')

-- 查询时自动推导对称关系
SELECT * FROM friendOf
-- 返回:
--   Alice friendOf Bob  (原始事实)
--   Bob friendOf Alice  (对称推导)
```

### 推理过程

```
原始事实:
  Alice friendOf Bob

推理结果:
  Bob friendOf Alice  (对称推导)
```

### 与逆属性的区别

| 特性 | 对称属性 | 逆属性 |
|------|----------|--------|
| 定义方式 | `SYMMETRIC` | `INVERSE OF <prop>` |
| 推导方向 | 同一属性的反向 | 不同属性之间的反向 |
| 示例 | `friendOf` | `manages` ↔ `reportsTo` |
| 语义 | "A 是 B 的朋友" ↔ "B 是 A 的朋友" | "A 管理 B" ↔ "B 向 A 汇报" |

---

## 15. 等价类/等价属性映射规则

### 等价类

```sql
-- 定义等价类
CREATE ONTOLOGY org (
  CLASS Employee,
  CLASS Worker,
  CLASS Employee EQUIVALENT TO Worker,
  CLASS Worker EQUIVALENT TO Employee
)

-- 插入数据
INSERT INTO Employee (name) VALUES ('Alice')
INSERT INTO Worker (name) VALUES ('Bob')

-- 查询 Employee 时自动包含 Worker 的实例
SELECT * FROM Employee
-- 返回: Alice (Employee), Bob (Worker)

-- 查询 Worker 时自动包含 Employee 的实例
SELECT * FROM Worker
-- 返回: Alice (Employee), Bob (Worker)
```

### 等价属性

```sql
-- 定义等价属性
CREATE ONTOLOGY user (
  CLASS User,
  PROPERTY email ON User TYPE STRING EQUIVALENT PROPERTY emailAddress
)

-- 插入数据
INSERT INTO User (name, email) VALUES ('Alice', 'alice@example.com')

-- 查询 emailAddress 时自动映射到 email
SELECT * FROM User WHERE emailAddress = 'alice@example.com'
-- 等价于: SELECT * FROM User WHERE email = 'alice@example.com'
```

### 子属性提升

```sql
-- 定义子属性
CREATE ONTOLOGY company (
  CLASS Employee,
  PROPERTY reportsTo ON Employee TYPE STRING,
  PROPERTY worksUnder ON Employee TYPE STRING SUBPROPERTY OF reportsTo
)

-- 插入数据
INSERT INTO Employee (name, worksUnder) VALUES ('Bob', 'Alice')

-- 查询 reportsTo 时自动包含 worksUnder 的数据
SELECT * FROM reportsTo
-- 返回: Bob reportsTo Alice（自动从 worksUnder 提升）
```

---

## 16. 混合查询（语义 + 结构 + 向量）

OntoDB 支持将语义查询、结构化查询和向量搜索融合在一起。

### 向量索引创建

```sql
-- 创建向量索引
CREATE VECTOR INDEX ON Product (embedding) 
  METRIC cosine 
  DIMENSION 128 
  M 16 
  EF_CONSTRUCTION 200 
  EF_SEARCH 100
```

参数说明：

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `METRIC` | 距离度量：`cosine`, `l2`, `inner_product` | `cosine` |
| `DIMENSION` | 向量维度 | 必填 |
| `M` | HNSW 图的连接数 | 16 |
| `EF_CONSTRUCTION` | 构建时的搜索范围 | 200 |
| `EF_SEARCH` | 查询时的搜索范围 | 100 |

### 向量搜索

```sql
-- 基本向量搜索
VECTOR SEARCH ON Product (embedding) 
QUERY [0.1, 0.2, 0.3, ...] 
TOP 10

-- 带条件过滤的向量搜索
VECTOR SEARCH ON Product (embedding) 
QUERY [0.1, 0.2, 0.3, ...] 
TOP 10
WHERE category = 'Electronics' AND price > 100
```

### 混合查询示例

```sql
-- SQL + 向量搜索融合
SELECT name, price, 
  VECTOR_DISTANCE(embedding, [0.1, 0.2, 0.3, ...]) AS score
FROM Product
WHERE category = 'Electronics' AND price > 100
ORDER BY score
LIMIT 10

-- 语义 + 向量搜索融合
MATCH (p: Product) 
WHERE category = 'Electronics'
RETURN name, price, 
  VECTOR_DISTANCE(embedding, [0.1, 0.2, 0.3, ...]) AS score
ORDER BY score
LIMIT 10
```

### HTTP API 混合查询

```bash
# 向量搜索 + SQL 过滤
curl -X POST http://127.0.0.1:7912/api/hybrid/query \
  -H "Content-Type: application/json" \
  -d '{
    "sql_filter": "SELECT * FROM Product WHERE price > 100 AND category = \'Electronics\'",
    "vector_column": "embedding",
    "query_vector": [0.1, 0.2, 0.3, ...],
    "top_k": 10,
    "class": "Product"
  }'
```

---

## 17. 跨模态融合查询

OntoDB 支持关系型、图、向量、时序、空间、本体六种模态的统一查询。

### 六模态统一查询

```sql
-- 关系型 + 图 + 向量融合查询
SELECT 
  p.name,
  p.department,
  COUNT(e.id) AS employee_count,
  VECTOR_DISTANCE(p.profile_embedding, [0.1, 0.2, ...]) AS similarity
FROM Project p
JOIN Employee e ON p.id = e.project_id
WHERE p.status = 'active'
GROUP BY p.id
HAVING COUNT(e.id) > 5
ORDER BY similarity
LIMIT 10
```

### 时序查询

```sql
-- 创建时序表
CREATE TIMESERIES sensor_data (
  device_id STRING,
  temperature FLOAT64,
  humidity FLOAT64,
  timestamp TIMESTAMP
)

-- 时序聚合查询
SELECT 
  device_id,
  AVG(temperature) AS avg_temp,
  MAX(humidity) AS max_humidity
FROM sensor_data
WHERE timestamp > NOW() - INTERVAL '1 hour'
GROUP BY device_id
```

### 空间查询

```sql
-- 创建空间表
CREATE SPATIAL TABLE locations (
  name STRING,
  coordinates POINT,
  area POLYGON
)

-- 空间查询
SELECT name FROM locations
WHERE ST_Within(coordinates, ST_MakeEnvelope(0, 0, 100, 100))
```

---

## 18. 活数据（衰减/激活/DBA 视图）

活数据（Live Data）是 OntoDB 的实时数据监控机制，支持数据衰减、激活和 DBA 视图。

### 概念说明

| 概念 | 说明 |
|------|------|
| **衰减（Decay）** | 数据随时间自动降低权重或过期 |
| **激活（Activation）** | 数据被访问时提升权重 |
| **DBA 视图** | 管理员查看数据活跃度的视图 |

### 使用场景

- **热点数据识别**: 自动识别频繁访问的数据
- **缓存优化**: 优先缓存活跃数据
- **数据生命周期**: 自动管理数据过期
- **性能监控**: 实时监控数据访问模式

### API 端点

```bash
# 获取活数据视图
GET /api/livedata

# 获取特定类的活数据
GET /api/livedata/:class

# 配置衰减策略
POST /api/livedata/config
{
  "class": "Product",
  "decay_rate": 0.1,
  "activation_boost": 0.5,
  "ttl": 3600
}
```

---

## 19. 本体推理规则详解（7 条 OWL 2 RL 规则）

OntoDB 实现了 7 条 OWL 2 RL 推理规则，查询时自动应用。

### 规则总览

| 规则 ID | 名称 | 说明 | 示例 |
|---------|------|------|------|
| Cax-sco | 子类传播 | `x type A`, `A subClassOf B` → `x type B` | Alice 是 Manager → Alice 是 Employee |
| Cax-eqc | 等价类传播 | `x type A`, `A equiv B` → `x type B` | Alice 是 Worker → Alice 是 Employee |
| Prp-spo | 子属性传播 | `x P y`, `P subPropOf Q` → `x Q y` | Alice worksUnder Bob → Alice reportsTo Bob |
| Prp-eqp | 等价属性传播 | `x P y`, `P equivProp Q` → `x Q y` | Alice email a@b.com → Alice emailAddress a@b.com |
| Prp-inv | 逆属性推导 | `x P y`, `P inverseOf Q` → `y Q x` | Bob reportsTo Alice → Alice manages Bob |
| Prp-trp | 传递闭包 | `x P y`, `y P z` → `x P z` | Alice ancestor Bob, Bob ancestor Charlie → Alice ancestor Charlie |
| Prp-symp | 对称推导 | `x P y`, `P symmetric` → `y P x` | Alice friendOf Bob → Bob friendOf Alice |

### 规则详解

#### Cax-sco — 子类传播

```sql
-- 本体定义
CLASS Person,
CLASS Employee SUBCLASS OF Person,
CLASS Manager SUBCLASS OF Employee

-- 事实
INSERT INTO Manager (name) VALUES ('Alice')

-- 推理结果
-- (Alice, rdf:type, Manager)  ← 原始事实
-- (Alice, rdf:type, Employee) ← Cax-sco 推导
-- (Alice, rdf:type, Person)   ← Cax-sco 推导
```

#### Cax-eqc — 等价类传播

```sql
-- 本体定义
CLASS Employee,
CLASS Worker EQUIVALENT TO Employee

-- 事实
INSERT INTO Employee (name) VALUES ('Alice')

-- 推理结果
-- (Alice, rdf:type, Employee) ← 原始事实
-- (Alice, rdf:type, Worker)   ← Cax-eqc 推导
```

#### Prp-spo — 子属性传播

```sql
-- 本体定义
PROPERTY reportsTo ON Employee TYPE STRING,
PROPERTY worksUnder ON Employee TYPE STRING SUBPROPERTY OF reportsTo

-- 事实
INSERT INTO Employee (name, worksUnder) VALUES ('Bob', 'Alice')

-- 推理结果
-- (Bob, worksUnder, Alice) ← 原始事实
-- (Bob, reportsTo, Alice)  ← Prp-spo 推导
```

#### Prp-eqp — 等价属性传播

```sql
-- 本体定义
PROPERTY email ON User TYPE STRING,
PROPERTY emailAddress ON User TYPE STRING EQUIVALENT PROPERTY email

-- 事实
INSERT INTO User (name, email) VALUES ('Alice', 'alice@example.com')

-- 推理结果
-- (Alice, email, alice@example.com)        ← 原始事实
-- (Alice, emailAddress, alice@example.com) ← Prp-eqp 推导
```

#### Prp-inv — 逆属性推导

```sql
-- 本体定义
PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages,
PROPERTY manages ON Manager TYPE STRING INVERSE OF reportsTo

-- 事实
INSERT INTO Employee (name, reportsTo) VALUES ('Bob', 'Alice')

-- 推理结果
-- (Bob, reportsTo, Alice) ← 原始事实
-- (Alice, manages, Bob)   ← Prp-inv 推导
```

#### Prp-trp — 传递闭包

```sql
-- 本体定义
PROPERTY ancestor ON Person TYPE STRING TRANSITIVE

-- 事实
INSERT INTO ancestor (subject, object) VALUES 
  ('Alice', 'Bob'),
  ('Bob', 'Charlie'),
  ('Charlie', 'Dave')

-- 推理结果
-- (Alice, ancestor, Bob)     ← 原始事实
-- (Bob, ancestor, Charlie)   ← 原始事实
-- (Charlie, ancestor, Dave)  ← 原始事实
-- (Alice, ancestor, Charlie) ← Prp-trp 推导
-- (Alice, ancestor, Dave)    ← Prp-trp 推导
-- (Bob, ancestor, Dave)      ← Prp-trp 推导
```

#### Prp-symp — 对称推导

```sql
-- 本体定义
PROPERTY friendOf ON Person TYPE STRING SYMMETRIC

-- 事实
INSERT INTO Person (name) VALUES ('Alice'), ('Bob')
INSERT INTO friendOf (subject, object) VALUES ('Alice', 'Bob')

-- 推理结果
-- (Alice, friendOf, Bob) ← 原始事实
-- (Bob, friendOf, Alice) ← Prp-symp 推导
```

### 推理引擎架构

```
┌─────────────────────────────────────────────────┐
│                  Reasoner                        │
├─────────────────────────────────────────────────┤
│  Input: ontology + facts                         │
│  Output: all_facts + inferred + violations       │
├─────────────────────────────────────────────────┤
│  1. Load ontology (classes + properties)         │
│  2. Initialize fact set                          │
│  3. Loop until fixed point:                      │
│     a. Apply all 7 rules                         │
│     b. Add new inferred facts                    │
│     c. Check for convergence                     │
│  4. Check consistency (disjoint violations)      │
│  5. Return results                               │
└─────────────────────────────────────────────────┘
```

### 推理结果查询

```sql
-- 查询推理结果（包含原始事实 + 推导事实）
SELECT * FROM ancestor WHERE subject = 'Alice'

-- 查看推导链
EXPLAIN SELECT * FROM ancestor WHERE subject = 'Alice'
```

### 增量推理

OntoDB 支持增量推理，只对新增/变更的三元组应用规则：

```python
# 增量推理 API
result = reasoner.reason_incremental(
    existing_facts=existing_facts,
    added=new_facts,
    removed=removed_facts
)
```

### 缓存机制

- **类层次缓存**: 缓存类继承关系，避免重复计算
- **传递闭包缓存**: 缓存传递属性的推导结果
- **逆属性缓存**: 缓存逆属性映射关系
- **缓存失效**: 本体变更时自动清除相关缓存

---

## 20. 示例库

### 医疗场景

```sql
-- 医疗本体定义
CREATE ONTOLOGY medical (
  CLASS Patient,
  CLASS Doctor,
  CLASS Nurse,
  CLASS Hospital,
  
  CLASS Inpatient SUBCLASS OF Patient,
  CLASS Outpatient SUBCLASS OF Patient,
  
  PROPERTY name ON Patient TYPE STRING REQUIRED,
  PROPERTY age ON Patient TYPE INT64,
  PROPERTY diagnosis ON Patient TYPE STRING,
  PROPERTY treats ON Doctor TYPE STRING INVERSE OF treatedBy,
  PROPERTY treatedBy ON Patient TYPE STRING INVERSE OF treats,
  PROPERTY supervises ON Doctor TYPE STRING INVERSE OF supervisedBy,
  PROPERTY supervisedBy ON Nurse TYPE STRING INVERSE OF supervises
)

-- 插入数据
INSERT INTO Doctor (name) VALUES ('Dr. Smith')
INSERT INTO Inpatient (name, age, diagnosis, treatedBy) 
VALUES ('Alice', 45, 'Pneumonia', 'Dr. Smith')

-- 查询所有患者（自动包含 Inpatient 和 Outpatient）
MATCH (p: Patient) RETURN name, age, diagnosis

-- 查询 Dr. Smith 的所有患者
SELECT p.name, p.diagnosis 
FROM Patient p
WHERE p.treatedBy = 'Dr. Smith'
```

### 生物信息场景

```sql
-- 生物信息本体
CREATE ONTOLOGY bio (
  CLASS Organism,
  CLASS Animal SUBCLASS OF Organism,
  CLASS Plant SUBCLASS OF Organism,
  CLASS Mammal SUBCLASS OF Animal,
  CLASS Reptile SUBCLASS OF Animal,
  CLASS Dog SUBCLASS OF Mammal,
  CLASS Cat SUBCLASS OF Mammal,
  
  PROPERTY hasGene ON Organism TYPE STRING,
  PROPERTY relatedTo ON Organism TYPE STRING TRANSITIVE,
  PROPERTY compatibleWith ON Organism TYPE STRING SYMMETRIC
)

-- 查询所有动物（自动包含 Mammal, Reptile, Dog, Cat）
SELECT * FROM Animal WHERE hasGene = 'GENE_X'
```

### 知识图谱场景

```sql
-- 知识图谱本体
CREATE ONTOLOGY knowledge (
  CLASS Entity,
  CLASS Person SUBCLASS OF Entity,
  CLASS Organization SUBCLASS OF Entity,
  CLASS Location SUBCLASS OF Entity,
  CLASS Event SUBCLASS OF Entity,
  
  PROPERTY name ON Entity TYPE STRING REQUIRED,
  PROPERTY worksAt ON Person TYPE STRING INVERSE OF employs,
  PROPERTY employs ON Organization TYPE STRING INVERSE OF worksAt,
  PROPERTY locatedIn ON Entity TYPE STRING TRANSITIVE,
  PROPERTY relatedTo ON Entity TYPE STRING SYMMETRIC,
  PROPERTY partOf ON Organization TYPE STRING TRANSITIVE
)

-- 查询某组织的所有员工
SELECT p.name 
FROM Person p
WHERE p.worksAt = 'Acme Corp'

-- 查询与某人相关的所有实体
GRAPH MATCH (a:Person) -[e:relatedTo]-> (b:Entity)
WHERE a.name = 'Alice'
RETURN b.name, b.__class__
```

### RAG 场景

```sql
-- RAG 本体
CREATE ONTOLOGY rag (
  CLASS Document,
  CLASS Chunk SUBCLASS OF Document,
  CLASS Query,
  
  PROPERTY content ON Document TYPE STRING REQUIRED,
  PROPERTY embedding ON Document TYPE ARRAY,
  PROPERTY belongsTo ON Chunk TYPE STRING INVERSE OF hasChunks,
  PROPERTY hasChunks ON Document TYPE STRING INVERSE OF belongsTo,
  PROPERTY relevance ON Query TYPE FLOAT64
)

-- 创建向量索引
CREATE VECTOR INDEX ON Document (embedding) 
  METRIC cosine DIMENSION 768

-- RAG 查询：语义搜索 + 向量相似度
SELECT 
  d.content,
  VECTOR_DISTANCE(d.embedding, [0.1, 0.2, ...]) AS score
FROM Document d
WHERE d.__class__ = 'Chunk'
ORDER BY score
LIMIT 5
```

### 企业 OA 场景

```sql
-- 企业 OA 本体
CREATE ONTOLOGY enterprise (
  CLASS Employee,
  CLASS Department,
  CLASS Project,
  CLASS Task,
  CLASS LeaveRequest,
  
  CLASS Manager SUBCLASS OF Employee,
  CLASS Developer SUBCLASS OF Employee,
  CLASS Designer SUBCLASS OF Employee,
  
  PROPERTY name ON Employee TYPE STRING REQUIRED,
  PROPERTY email ON Employee TYPE STRING REQUIRED,
  PROPERTY department ON Employee TYPE STRING,
  PROPERTY salary ON Employee TYPE FLOAT64,
  PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages,
  PROPERTY manages ON Manager TYPE STRING INVERSE OF reportsTo,
  PROPERTY assignedTo ON Task TYPE STRING,
  PROPERTY status ON Task TYPE STRING
)

-- 查询部门经理
SELECT name, email FROM Manager WHERE department = 'Engineering'

-- 查询所有向某经理汇报的员工（自动包含子类）
SELECT name, department 
FROM Employee 
WHERE reportsTo = 'Alice'

-- 查询某项目的所有任务
SELECT t.name, t.status, e.name AS assignee
FROM Task t
JOIN Employee e ON t.assignedTo = e.name
WHERE t.project = 'Project X'
```

---

## 21. 迁移指南

### SQL → OntoQL 迁移

#### JOIN → 图遍历

**传统 SQL:**
```sql
SELECT e.name, d.name AS department
FROM Employee e
JOIN Department d ON e.department_id = d.id
```

**OntoQL:**
```sql
-- 方式 1: 使用本体关系
SELECT e.name, e.department 
FROM Employee e

-- 方式 2: 使用图查询
GRAPH MATCH (e:Employee) -[r:worksIn]-> (d:Department)
RETURN e.name, d.name
```

#### UNION → 子类

**传统 SQL:**
```sql
SELECT name, 'Employee' AS type FROM Employee
UNION ALL
SELECT name, 'Manager' AS type FROM Manager
UNION ALL
SELECT name, 'Developer' AS type FROM Developer
```

**OntoQL:**
```sql
-- 自动包含所有子类
SELECT name, __class__ AS type FROM Employee
```

#### CTE → 传递属性

**传统 SQL:**
```sql
WITH RECURSIVE org_tree AS (
  SELECT id, name, manager_id, 1 AS level
  FROM Employee WHERE manager_id IS NULL
  UNION ALL
  SELECT e.id, e.name, e.manager_id, t.level + 1
  FROM Employee e
  JOIN org_tree t ON e.manager_id = t.id
)
SELECT * FROM org_tree
```

**OntoQL:**
```sql
-- 使用传递属性自动推导
SELECT * FROM reportsTo WHERE subject = 'CEO'
```

#### CASE WHEN → 类层次

**传统 SQL:**
```sql
SELECT 
  name,
  CASE 
    WHEN type = 'sensor' THEN 'Sensor'
    WHEN type = 'camera' THEN 'Camera'
    WHEN type = 'actuator' THEN 'Actuator'
    ELSE 'Unknown'
  END AS device_type
FROM Device
```

**OntoQL:**
```sql
-- 使用类层次自动标注
SELECT name, __class__ AS device_type FROM Device
```

### PostgreSQL → OntoDB 数据迁移

#### 步骤 1: 导出 PostgreSQL 数据

```bash
# 导出为 CSV
psql -d mydb -c "\COPY (SELECT * FROM employees) TO 'employees.csv' WITH CSV HEADER"

# 导出为 JSON
psql -d mydb -c "\COPY (SELECT row_to_json(t) FROM employees t) TO 'employees.json'"
```

#### 步骤 2: 创建 OntoDB 本体

```sql
CREATE ONTOLOGY my_app (
  CLASS Employee,
  CLASS Department,
  
  PROPERTY name ON Employee TYPE STRING REQUIRED,
  PROPERTY email ON Employee TYPE STRING,
  PROPERTY salary ON Employee TYPE FLOAT64,
  PROPERTY department ON Employee TYPE STRING,
  PROPERTY hire_date ON Employee TYPE STRING
)
```

#### 步骤 3: 导入数据

```sql
-- 方式 1: 使用 IMPORT
IMPORT INTO Employee FROM CSV 'employees.csv'

-- 方式 2: 使用 COPY（更快，无事务）
COPY Employee FROM 'employees.csv'

-- 方式 3: 使用 INSERT ... SELECT
INSERT INTO Employee (name, email, salary, department)
SELECT name, email, salary, department FROM temp_employees
```

#### 步骤 4: 验证数据

```sql
-- 检查记录数
SELECT COUNT(*) FROM Employee

-- 检查数据样本
SELECT * FROM Employee LIMIT 10

-- 检查类层次
SELECT __class__, COUNT(*) FROM Employee GROUP BY __class__
```

### 本体建模最佳实践

#### 1. 类层次设计

```sql
-- 好的设计：清晰的层次结构
CLASS Animal,
CLASS Dog SUBCLASS OF Animal,
CLASS Cat SUBCLASS OF Animal

-- 不好的设计：过深的层次
CLASS Animal,
CLASS Mammal SUBCLASS OF Animal,
CLASS DomesticMammal SUBCLASS OF Mammal,
CLASS Dog SUBCLASS OF DomesticMammal
-- 建议：最多 3-4 层
```

#### 2. 属性设计

```sql
-- 好的设计：明确的属性语义
PROPERTY reportsTo ON Employee TYPE STRING INVERSE OF manages

-- 不好的设计：模糊的属性名
PROPERTY rel1 ON Employee TYPE STRING
```

#### 3. 约束使用

```sql
-- 好的设计：使用合适的约束
PROPERTY email ON User TYPE STRING REQUIRED FUNCTIONAL

-- 不好的设计：过度约束
PROPERTY name ON User TYPE STRING REQUIRED FUNCTIONAL UNIQUE
-- 建议：只在必要时使用约束
```

#### 4. 索引策略

```sql
-- 为常用查询字段创建索引
CREATE INDEX ON Employee (department)
CREATE INDEX ON Employee (email)

-- 为向量搜索创建索引
CREATE VECTOR INDEX ON Document (embedding) 
  METRIC cosine DIMENSION 768
```

#### 5. 查询优化

```sql
-- 好的查询：利用类层次
SELECT * FROM Employee WHERE department = 'Engineering'

-- 不好的查询：手动展开类
SELECT * FROM Employee WHERE __class__ = 'Employee' AND department = 'Engineering'
UNION ALL
SELECT * FROM Manager WHERE department = 'Engineering'
UNION ALL
SELECT * FROM Developer WHERE department = 'Engineering'
```

---

## 附录 A: 错误码

| 错误码 | 说明 |
|--------|------|
| `PARSE_ERROR` | SQL 解析错误 |
| `VALIDATION_ERROR` | 数据验证错误 |
| `UNIQUE_VIOLATION` | 唯一约束违反 |
| `TYPE_ERROR` | 类型不匹配 |
| `NOT_FOUND` | 记录不存在 |
| `PERMISSION_DENIED` | 权限不足 |
| `RATE_LIMITED` | 请求频率超限 |

## 附录 B: 性能基准

| 操作 | 性能 |
|------|------|
| 写入吞吐量 | 863,618 ops/s |
| 读取吞吐量 | 1,256,518 ops/s |
| 批量写入 | 1,082,230 ops/s |
| HNSW 向量搜索 | 100% 召回率，341µs |
| 子类展开查询 | ~2.5ms |
| 推理（100 节点） | <1ms |
| 推理（1000 节点） | ~10ms |
| 推理（10000 节点） | ~100ms |

---

**© 2026 原点价值 / OntoValue Technology**  
**文档版本**: v1.0  
**最后更新**: 2026-08-28
