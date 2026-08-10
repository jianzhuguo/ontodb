# OntoDB 最佳实践

> 生产环境使用指南

---

## 1. 数据建模

### 1.1 选择合适的表类型

```sql
-- 顶点表：用于实体（用户、产品、订单）
CREATE VERTEX TABLE users (
    id STRING,
    name STRING,
    age INT
);

-- 边表：用于关系（关注、购买、属于）
CREATE EDGE TABLE follows (
    from_id STRING,
    to_id STRING,
    since INT
);
```

### 1.2 主键设计

```sql
-- 推荐：使用有意义的业务 ID
CREATE VERTEX TABLE orders (
    order_id STRING,      -- "ORD-2026-001"
    user_id STRING,       -- "USR-001"
    amount DOUBLE
);

-- 避免：使用自增 ID（不利于分布式）
```

### 1.3 索引策略

```sql
-- 为高频查询字段创建索引
CREATE INDEX ON users (email);
CREATE INDEX ON orders (user_id, created_at);

-- 为向量搜索创建向量索引
CREATE VECTOR INDEX ON documents (embedding) DIMENSIONS 128 METRIC cosine;

-- 为图遍历创建边索引
CREATE EDGE INDEX ON follows (from_id);
```

---

## 2. 查询优化

### 2.1 使用 LIMIT

```sql
-- 好：限制返回行数
SELECT * FROM users WHERE age > 25 LIMIT 100;

-- 避免：无限制查询大表
SELECT * FROM users;
```

### 2.2 使用索引字段过滤

```sql
-- 好：使用索引字段
SELECT * FROM orders WHERE user_id = 'USR-001' AND created_at > '2026-01-01';

-- 避免：全表扫描
SELECT * FROM orders WHERE amount > 100;
```

### 2.3 批量操作

```sql
-- 好：批量插入
BATCH INSERT INTO users (id, name, age) VALUES
    ('u1', 'Alice', 30),
    ('u2', 'Bob', 25),
    ('u3', 'Charlie', 35);

-- 避免：逐条插入
INSERT INTO users (id, name) VALUES ('u1', 'Alice');
INSERT INTO users (id, name) VALUES ('u2', 'Bob');
```

### 2.4 向量搜索优化

```sql
-- 好：先过滤再搜索
VECTOR SEARCH ON documents (embedding)
    QUERY [0.1, 0.2, ...]
    TOP 10
    WHERE category = '技术';

-- 避免：全量搜索后过滤
```

---

## 3. 事务使用

### 3.1 保持事务短小

```sql
-- 好：短事务
BEGIN;
UPDATE accounts SET balance = balance - 100 WHERE id = 'a1';
UPDATE accounts SET balance = balance + 100 WHERE id = 'a2';
COMMIT;

-- 避免：长事务（持有锁时间过长）
BEGIN;
-- ... 大量操作 ...
COMMIT;
```

### 3.2 使用合适的隔离级别

```sql
-- 默认快照隔离，适合读多写少场景
BEGIN;
SELECT * FROM users WHERE id = 'u1';  -- 看到一致快照
UPDATE users SET age = 31 WHERE id = 'u1';
COMMIT;
```

---

## 4. 向量搜索最佳实践

### 4.1 选择合适的维度

| 维度 | 适用场景 | 构建时间 | 查询延迟 |
|------|---------|---------|---------|
| 64 | 简单文本匹配 | 快 | <0.5ms |
| 128 | 通用文本/图像 | 中等 | <1ms |
| 256 | 高精度语义 | 较慢 | <2ms |
| 512 | 多模态融合 | 慢 | <5ms |

### 4.2 选择合适的距离度量

```sql
-- 余弦相似度：适合文本嵌入
CREATE VECTOR INDEX ON docs (emb) DIMENSIONS 128 METRIC cosine;

-- 欧氏距离：适合图像特征
CREATE VECTOR INDEX ON images (feat) DIMENSIONS 256 METRIC euclidean;
```

### 4.3 混合查询

```sql
-- SQL 过滤 + 向量排序：最佳实践
SELECT title, VECTOR_DISTANCE(embedding, [0.1, 0.2, ...]) as score
FROM documents
WHERE category = '技术' AND year > 2020
ORDER BY score
LIMIT 10;
```

---

## 5. 图查询最佳实践

### 5.1 限制遍历深度

```sql
-- 好：限制深度
GRAPH TRAVERSE FROM 'Person::1' OUT DEPTH 3;

-- 避免：无深度限制（可能导致全图扫描）
GRAPH TRAVERSE FROM 'Person::1' OUT;
```

### 5.2 使用边标签过滤

```sql
-- 好：指定边标签
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 2;

-- 避免：遍历所有边类型
GRAPH TRAVERSE FROM 'Person::1' OUT DEPTH 2;
```

---

## 6. 本体推理最佳实践

### 6.1 设计清晰的类层次

```sql
-- 好：清晰的继承关系
CREATE ONTOLOGY MyOntology (
    CLASS Animal,
    CLASS Dog SUBCLASS OF Animal,
    CLASS Cat SUBCLASS OF Animal,
    CLASS Pet SUBCLASS OF Animal
);

-- 避免：过深的继承层次
CREATE ONTOLOGY DeepOntology (
    CLASS A,
    CLASS B SUBCLASS OF A,
    CLASS C SUBCLASS OF B,
    CLASS D SUBCLASS OF C,
    CLASS E SUBCLASS OF D  -- 5层太深
);
```

### 6.2 使用推理解释

```sql
-- 查看推导链
EXPLAIN SELECT * FROM Animal WHERE hasName = '旺财';
```

---

## 7. 安全最佳实践

### 7.1 启用认证

```bash
# 生产环境必须启用认证
./ontodb-server --auth --api-key "strong-random-key"
```

### 7.2 使用 TLS

```bash
# 生产环境必须使用 TLS
./ontodb-server --tls-cert cert.pem --tls-key key.pem
```

### 7.3 限制访问 IP

```json
// config/api_keys.json
{
  "keys": [{
    "key": "your-key",
    "ip_whitelist": ["10.0.0.0/8", "192.168.1.0/24"]
  }]
}
```

---

## 8. 监控最佳实践

### 8.1 关键指标

| 指标 | 阈值 | 说明 |
|------|------|------|
| QPS | <100K | 单机上限 |
| P99 延迟 | <100ms | 查询延迟 |
| 错误率 | <0.1% | 查询错误 |
| 内存使用 | <80% | 避免 OOM |
| 磁盘使用 | <80% | 避免写满 |

### 8.2 告警配置

```yaml
# 关键告警
- OntoDB 实例宕机
- 查询 P99 > 1s
- 错误率 > 1%
- 磁盘使用 > 80%
- 内存使用 > 80%
```

---

## 9. 备份最佳实践

### 9.1 备份策略

| 频率 | 类型 | 保留时间 |
|------|------|---------|
| 每日 | 全量备份 | 30 天 |
| 每小时 | 增量备份 | 7 天 |
| 实时 | WAL 归档 | 3 天 |

### 9.2 恢复演练

```bash
# 定期演练恢复流程
# 1. 停止服务
# 2. 恢复备份
# 3. 验证数据
# 4. 启动服务
```

---

## 10. 常见反模式

| 反模式 | 问题 | 正确做法 |
|--------|------|---------|
| 无 LIMIT 查询 | 内存溢出 | 始终加 LIMIT |
| 频繁小事务 | WAL 累积 | 批量提交 |
| 过深图遍历 | 性能差 | 限制深度 |
| 无索引查询 | 全表扫描 | 创建索引 |
| 大向量维度 | 内存占用高 | 选择合适维度 |
| 长事务 | 锁竞争 | 保持短小 |
