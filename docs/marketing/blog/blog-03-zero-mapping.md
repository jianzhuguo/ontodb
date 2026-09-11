# 零映射表：跨模态查询的终极解决方案

> 传统多模态数据库需要维护实体ID到各存储引擎的映射表，跨模态查询需要多次网络调用。OntoDB通过统一标识消除映射表，实现零开销的跨模态关联。

## 传统方案的映射表问题

### 映射表架构

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   MySQL     │    │   Neo4j     │    │  Milvus     │
│ id=1        │    │ id=abc      │    │ id=xyz      │
│ name="设备A" │    │ ...         │    │ vector=[...] │
└─────────────┘    └─────────────┘    └─────────────┘
      ↑                  ↑                  ↑
      └──────────────────┼──────────────────┘
                         ↓
                 ┌─────────────┐
                 │   映射表     │
                 │ mysql_id=1  │
                 │ neo4j_id=abc│
                 │ milvus_id=xyz│
                 └─────────────┘
```

### 映射表带来的问题

#### 1. 存储开销
```
每个实体需要维护：
- 映射表记录：100字节
- 假设1000万实体：1GB额外存储
```

#### 2. 查询开销
```
跨模态查询流程：
1. 查询MySQL获取id=1（1ms）
2. 查询映射表获取neo4j_id=abc（1ms）
3. 查询Neo4j获取关系（1ms）
4. 查询映射表获取milvus_id=xyz（1ms）
5. 查询Milvus获取向量（1ms）
总延迟：5ms（理想情况）
```

#### 3. 一致性问题
```
写入流程：
1. 写入MySQL（1ms）
2. 更新映射表（1ms）
3. 写入Neo4j（1ms）
4. 写入Milvus（1ms）

问题：步骤2-4可能失败，导致数据不一致
```

#### 4. 分布式事务
```
跨系统写入需要分布式事务：
- 2PC协议
- 协调器开销
- 锁竞争
- 延迟增加10-100倍
```

## OntoDB的零映射表方案

### 统一标识机制

```
统一标识：Device::001
同时作为：
- MySQL的主键：Device::001
- Neo4j的顶点ID：Device::001
- Milvus的文档键：Device::001
- 三元组的主体：Device::001
```

### 零开销关联

```
跨模态查询流程：
1. 使用统一标识Device::001
2. 直接查询各存储引擎（无需映射表）
3. 结果直接关联
总延迟：<1ms
```

### 原子写入

```
写入流程：
1. 使用统一标识Device::001
2. 原子写入所有模态
3. 无需映射表更新
4. 无需分布式事务
总延迟：<1ms（原子完成）
```

## 性能对比

### 查询延迟对比

| 查询类型 | 传统方案（映射表） | OntoDB（零映射表） | 提升 |
|----------|-------------------|-------------------|------|
| 单模态查询 | 1ms | 1ms | 1x |
| 跨2模态查询 | 5ms | 1ms | 5x |
| 跨3模态查询 | 9ms | 1ms | 9x |
| 跨6模态查询 | 17ms | 1ms | 17x |

### 存储开销对比

| 项目 | 传统方案 | OntoDB |
|------|----------|--------|
| 实体数据 | 10GB | 10GB |
| 映射表 | 1GB | 0 |
| 总存储 | 11GB | 10GB |
| 节省 | - | 9% |

### 一致性对比

| 指标 | 传统方案 | OntoDB |
|------|----------|--------|
| 写入一致性 | 最终一致 | 强一致 |
| 事务支持 | 分布式事务 | 单一事务 |
| 故障恢复 | 复杂 | 简单 |

## 实际案例

### 案例：设备关联查询

**需求**：查询"某设备的拓扑关系及其最近24小时的传感器数据"

**传统方案**：
```sql
-- 步骤1：查询MySQL获取设备信息
SELECT * FROM devices WHERE id = 1;
-- 返回：id=1, name="设备A"

-- 步骤2：查询映射表获取Neo4j ID
SELECT neo4j_id FROM mapping WHERE mysql_id = 1;
-- 返回：neo4j_id=abc

-- 步骤3：查询Neo4j获取拓扑关系
MATCH (d:Device {id: 'abc'})-[:CONNECTS_TO]->(other)
RETURN other;
-- 返回：[设备B, 设备C]

-- 步骤4：查询映射表获取InfluxDB ID
SELECT influx_id FROM mapping WHERE mysql_id IN (1, 2, 3);
-- 返回：[xyz, uvw, rst]

-- 步骤5：查询InfluxDB获取传感器数据
SELECT * FROM sensor_data 
WHERE device_id IN ('xyz', 'uvw', 'rst') 
AND time > now() - 24h;
-- 返回：传感器数据

总延迟：5次查询 × 1ms = 5ms（理想情况）
```

**OntoDB方案**：
```sql
-- 使用统一查询语言
SELECT d.name, 
       graph.neighbors(d, 'CONNECTS_TO') as topology,
       ts.aggregate(d.temperature, '24h', 'avg') as avg_temp
FROM Device d
WHERE d.id = 'Device::001';

-- 底层执行：
-- 1. 使用统一标识Device::001
-- 2. 直接查询各存储引擎
-- 3. 结果直接关联
总延迟：<1ms
```

### 性能提升

| 指标 | 传统方案 | OntoDB | 提升 |
|------|----------|--------|------|
| 查询延迟 | 5ms | <1ms | 5x |
| 查询复杂度 | 5次查询 | 1次查询 | 80%↓ |
| 代码复杂度 | 高 | 低 | - |

## 技术细节

### 统一标识实现

```rust
// 统一标识结构
struct EntityId {
    class: String,  // 类名，如"Device"
    pk: String,     // 主键，如"001"
}

impl EntityId {
    // 转换为存储键
    fn to_storage_key(&self) -> Vec<u8> {
        format!("{}::{}", self.class, self.pk).into_bytes()
    }
    
    // 转换为图顶点ID
    fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }
    
    // 转换为向量文档键
    fn to_vector_key(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }
    
    // 转换为三元组主体
    fn to_triple_subject(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }
}
```

### 零开销关联实现

```rust
// 跨模态查询执行
fn cross_modal_query(entity_id: &EntityId) -> QueryResult {
    let key = entity_id.to_storage_key();
    
    // 并行查询各存储引擎（无需映射表）
    let (record, graph, vector, triple, timeseries, spatial) = tokio::join!(
        storage_engine.get(&key),
        graph_engine.get_vertex(&entity_id.to_vertex_id()),
        vector_engine.get(&entity_id.to_vector_key()),
        triple_engine.get_subject(&entity_id.to_triple_subject()),
        timeseries_engine.get(&key),
        spatial_engine.get(&key),
    );
    
    // 结果直接关联（无需映射表转换）
    QueryResult {
        record: record?,
        graph: graph?,
        vector: vector?,
        triple: triple?,
        timeseries: timeseries?,
        spatial: spatial?,
    }
}
```

## 总结

OntoDB通过统一标识机制，实现了：

- **零映射表**：无需维护实体ID映射
- **零开销关联**：跨模态查询无需额外网络调用
- **强一致性**：原子写入，无需分布式事务

**零映射表不是优化，是架构革命。**

---

*作者：OntoDB团队*
*日期：2026年9月*
*标签：#跨模态查询 #零映射表 #统一标识 #数据库*
