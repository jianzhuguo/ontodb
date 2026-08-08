# OntoDB 统一实体锚点设计

> 核心思想：三模态（关系型、图、向量）通过 `{class}::{pk}` 共享同一个实体标识，
> 任何模态的变更自动同步到其他模态。

---

## 1. 问题现状

```
当前：三个独立的 ID 空间，互不关联

关系型 LSM          图 GraphStore         向量 HNSW
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│Product::001 │    │  "v1"       │    │Product::001 │ ← 共享 ✅
│Product::002 │    │  "v2"       │    │Product::002 │
│Employee::01 │    │  "v3"       │    │Employee::01 │
└─────────────┘    └─────────────┘    └─────────────┘
      ↕                   ✗                  ↕
   向量共享key          完全孤立           关系型共享key
```

```
目标：统一实体锚点

┌──────────────────────────────────────────────┐
│            Entity: Product::001              │
│  ┌──────────┐ ┌──────────┐ ┌──────────────┐ │
│  │ 关系型   │ │ 图       │ │ 向量         │ │
│  │ name: .. │ │ edges:   │ │ embedding:   │ │
│  │ price:.. │ │ [→Cat]   │ │ [0.1,0.2..]  │ │
│  └──────────┘ └──────────┘ └──────────────┘ │
│       ↕            ↕              ↕          │
│       └────────────┼──────────────┘          │
│            统一锚点 {class}::{pk}            │
└──────────────────────────────────────────────┘
```

---

## 2. 设计方案

### 2.1 统一实体 ID

```rust
/// 统一实体标识 — 三模态共享的锚点
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EntityId {
    /// 类名 (如 "Product", "Employee")
    pub class: String,
    /// 主键 (如 "000001", "uuid-xxx")
    pub pk: String,
}

impl EntityId {
    /// 转为 LSM key bytes — 关系型和向量的统一键
    pub fn to_lsm_key(&self) -> Vec<u8> {
        format!("{}::{}", self.class, self.pk).into_bytes()
    }

    /// 转为图顶点 ID — 图的统一键
    pub fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }

    /// 从 LSM key 解析
    pub fn from_lsm_key(key: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(key).ok()?;
        let (class, pk) = s.split_once("::")?;
        Some(Self { class: class.to_string(), pk: pk.to_string() })
    }
}
```

### 2.2 图存储改造

**核心变更**：GraphStore 的顶点 ID 从自定义字符串改为 `{class}::{pk}`。

```rust
// 之前
let v = Vertex::new("v1", vec!["Person".to_string()]);

// 之后 — 图顶点和关系型记录共享同一个 ID
let v = Vertex::new("Person::000001", vec!["Person".to_string()]);
```

**GraphStore 新增方法**：

```rust
impl GraphStore {
    /// 从关系型记录自动创建图顶点
    /// 当 INSERT INTO Product ... 时自动调用
    pub fn upsert_vertex_from_entity(&self, entity_id: &EntityId, labels: &[String]) {
        let vertex_id = entity_id.to_vertex_id();
        if self.get_vertex(&vertex_id).is_none() {
            let vertex = Vertex::new(&vertex_id, labels.to_vec());
            let _ = self.add_vertex(vertex);
        }
    }

    /// 根据实体 ID 删除图顶点（级联删除边）
    /// 当 DELETE FROM Product WHERE ... 时自动调用
    pub fn delete_vertex_by_entity(&self, entity_id: &EntityId) {
        let vertex_id = entity_id.to_vertex_id();
        let _ = self.delete_vertex(&vertex_id);
    }

    /// 根据实体 ID 添加关系（边）
    /// 支持 INSERT EDGE 语义
    pub fn add_relationship(
        &self,
        from: &EntityId,
        to: &EntityId,
        label: &str,
    ) -> Result<(), GraphError> {
        let edge_id = format!("{}->{}::{}", from.to_vertex_id(), to.to_vertex_id(), label);
        let edge = Edge::new(&edge_id, &from.to_vertex_id(), &to.to_vertex_id(), label);
        self.add_edge(edge)
    }

    /// 查询实体的关系（邻居）
    pub fn get_entity_neighbors(
        &self,
        entity_id: &EntityId,
        direction: Direction,
        edge_label: Option<&str>,
    ) -> Vec<EntityId> {
        let vertex_id = entity_id.to_vertex_id();
        let neighbors = match direction {
            Direction::Out => self.get_out_edges(&vertex_id),
            Direction::In => self.get_in_edges(&vertex_id),
            Direction::Both => {
                let mut e = self.get_out_edges(&vertex_id);
                e.extend(self.get_in_edges(&vertex_id));
                e
            }
        };

        neighbors.iter()
            .filter(|e| edge_label.map_or(true, |l| e.label == l))
            .filter_map(|e| {
                let target_id = match direction {
                    Direction::In => &e.from,
                    _ => &e.to,
                };
                EntityId::from_lsm_key(target_id.as_bytes())
            })
            .collect()
    }
}
```

### 2.3 向量索引改造

**当前已是共享锚点**，无需大改。只需确认：

```rust
// vector/manager.rs — 已经使用 doc_key（即 LSM key）作为锚点
pub fn index_vector(&mut self, doc_key: &[u8], class: &str, column: &str, vector: Vec<f32>)
// doc_key 就是 {class}::{pk}，和关系型共享 ✅
```

### 2.4 查询执行器联动

**核心变更**：DML 操作自动同步到图存储。

```rust
// executor.rs — INSERT 时自动创建图顶点
fn execute_insert_txn(&self, engine: &LsmEngine, txn_id: u64, class: &str, ...) -> Result<QueryResult> {
    let key = self.generate_doc_key(class);
    let entity_id = EntityId { class: class.to_string(), pk: extract_pk(&key) };

    // 1. 写入关系型数据
    engine.txn_put(txn_id, key.clone(), value)?;

    // 2. 自动同步到图存储
    self.graph.upsert_vertex_from_entity(&entity_id, &[class.to_string()]);

    // 3. 自动同步到向量索引（已有逻辑）
    self.index_vectors_if_needed(&key, class, &doc);

    Ok(QueryResult::Success("1 row inserted".to_string()))
}

// DELETE 时自动删除图顶点
fn execute_delete_txn(&self, engine: &LsmEngine, txn_id: u64, class: &str, ...) -> Result<QueryResult> {
    // ... 删除关系型数据 ...

    // 自动同步到图存储
    for (key, _) in &deleted_rows {
        if let Some(entity_id) = EntityId::from_lsm_key(key) {
            self.graph.delete_vertex_by_entity(&entity_id);
        }
    }
}
```

### 2.5 SPARQL 三元组原生存储

**新增**：持久化三元组索引，支持 SPO/POS/OSP 三种查询模式。

```rust
/// 三元组索引 — 存储在 LSM 中
/// Key 格式: __triple__{S}__{P}__{O}
/// 辅助索引: __triple_pos__{P}__{O}__{S}
///           __triple_osp__{O}__{S}__{P}

pub struct TripleStore {
    engine: Arc<LsmEngine>,
}

impl TripleStore {
    /// 持久化一个三元组
    pub fn add_triple(&self, subject: &EntityId, predicate: &str, object: &str) -> Result<()> {
        let key = format!("__triple__{}__{}__{}", subject.to_vertex_id(), predicate, object);
        self.engine.put(key.into_bytes(), vec![])?;

        // 辅助索引: POS
        let pos_key = format!("__triple_pos__{}__{}__{}", predicate, object, subject.to_vertex_id());
        self.engine.put(pos_key.into_bytes(), vec![])?;

        // 辅助索引: OSP
        let osp_key = format!("__triple_osp__{}__{}__{}", object, subject.to_vertex_id(), predicate);
        self.engine.put(osp_key.into_bytes(), vec![])?;

        Ok(())
    }

    /// SPO 查询: 给定主语和谓语，查找宾语
    pub fn lookup_spo(&self, subject: &EntityId, predicate: &str) -> Result<Vec<String>> {
        let prefix = format!("__triple__{}__{}__", subject.to_vertex_id(), predicate);
        let results = self.engine.scan_prefix(prefix.as_bytes())?;
        Ok(results.into_iter().filter_map(|(k, _)| {
            let s = std::str::from_utf8(&k).ok()?;
            let parts: Vec<&str> = s.splitn(4, "__").collect();
            parts.get(3).map(|o| o.to_string())
        }).collect())
    }

    /// POS 查询: 给定谓语和宾语，查找主语
    pub fn lookup_pos(&self, predicate: &str, object: &str) -> Result<Vec<EntityId>> {
        let prefix = format!("__triple_pos__{}__{}__", predicate, object);
        let results = self.engine.scan_prefix(prefix.as_bytes())?;
        Ok(results.into_iter().filter_map(|(k, _)| {
            let s = std::str::from_utf8(&k).ok()?;
            let parts: Vec<&str> = s.splitn(4, "__").collect();
            parts.get(3).and_then(|id| EntityId::from_lsm_key(id.as_bytes()))
        }).collect())
    }
}
```

---

## 3. 数据流

### 3.1 写入流程

```
用户: INSERT INTO Product (name, price, embedding) VALUES ('iPhone', 999, '[0.1,0.2,...]')

         ┌──────────────────────────────────────────────┐
         │              QueryExecutor                    │
         │  1. 生成 EntityId = Product::000001           │
         │  2. 构建文档 {name, price, embedding}         │
         └──────────┬───────────┬───────────┬───────────┘
                    │           │           │
         ┌──────────▼──┐ ┌──────▼──────┐ ┌──▼───────────┐
         │ LSM Engine  │ │ GraphStore  │ │ VectorIndex  │
         │ put(key,    │ │ upsert_     │ │ index_vector │
         │     value)  │ │ vertex()    │ │ (doc_key,    │
         │             │ │             │ │  embedding)  │
         └─────────────┘ └─────────────┘ └──────────────┘
              关系型           图              向量
              写入           自动创建         自动索引
```

### 3.2 查询流程

```
用户: SELECT * FROM Product WHERE embedding <-> query_vec < 0.5

         ┌──────────────────────────────────────────────┐
         │              QueryExecutor                    │
         │  1. 向量搜索 → 得到 [Product::001, Product::003] │
         │  2. 关系型查询 → 过滤 price > 500              │
         │  3. 返回融合结果                               │
         └──────────────────────────────────────────────┘

用户: MATCH (p: Product)-[:belongs_to]->(c: Category) WHERE c.name = 'Electronics'

         ┌──────────────────────────────────────────────┐
         │              QueryExecutor                    │
         │  1. 图遍历 → 得到 [Product::001, Product::002] │
         │  2. 关系型查询 → 获取详细属性                   │
         │  3. 返回融合结果                               │
         └──────────────────────────────────────────────┘
```

### 3.3 推理流程

```
用户: INSERT INTO Employee (name, reports_to) VALUES ('Alice', 'Bob')

         ┌──────────────────────────────────────────────┐
         │              Reasoner                         │
         │  1. 写入关系型: Employee::Alice                │
         │  2. 写入图: Alice -[reports_to]-> Bob          │
         │  3. 推理: PrpInv → Bob -[manages]-> Alice      │
         │  4. 推理: CaxSco → Alice type Person           │
         │  5. 三元组: (Alice, rdf:type, Person)          │
         └──────────────────────────────────────────────┘
```

---

## 4. 实现计划

### Phase A: EntityId 基础 (1天)
- [ ] 定义 `EntityId` 结构体
- [ ] 实现 `to_lsm_key()`, `to_vertex_id()`, `from_lsm_key()`
- [ ] 单元测试

### Phase B: 图存储接入 (2天)
- [ ] GraphStore 新增 `upsert_vertex_from_entity()`
- [ ] GraphStore 新增 `delete_vertex_by_entity()`
- [ ] GraphStore 新增 `add_relationship()`
- [ ] GraphStore 新增 `get_entity_neighbors()`
- [ ] 图顶点 ID 从自定义字符串改为 `{class}::{pk}`
- [ ] 向后兼容：支持旧格式 ID 查询

### Phase C: DML 联动 (2天)
- [ ] INSERT 自动创建图顶点
- [ ] DELETE 自动删除图顶点
- [ ] UPDATE 自动更新图顶点属性
- [ ] IMPORT/COPY 批量同步到图

### Phase D: 三元组持久化 (2天)
- [ ] `TripleStore` 实现 SPO/POS/OSP 三种索引
- [ ] SPARQL INSERT 自动持久化三元组
- [ ] SPARQL SELECT 优先查三元组索引
- [ ] 推理结果自动持久化为三元组

### Phase E: 查询融合 (2天)
- [ ] 向量搜索结果自动关联关系型属性
- [ ] 图遍历结果自动关联关系型属性
- [ ] 混合查询: 向量 + 图 + 关系型联合过滤
- [ ] SPARQL 查询下推到三元组索引

---

## 5. 性能影响评估

| 操作 | 当前 | 改造后 | 影响 |
|------|------|--------|------|
| INSERT | 1 次 LSM put | 1 put + 1 图顶点 + N 三元组 | +10-20% 开销 |
| DELETE | 1 次 LSM delete | 1 delete + 1 图删除 | +5-10% 开销 |
| 向量搜索 | 不变 | 不变 | 无影响 |
| 图遍历 | 内存邻接表 | 不变 | 无影响 |
| SPARQL | 翻译成 SQL | 直接查三元组索引 | **快 2-5x** |

**关键优化**：
- 图顶点创建可以异步（写入后队列化）
- 三元组写入可以批量（COPY 场景）
- 向量索引已经是共享锚点，无需改动

---

## 6. 兼容性

- **向后兼容**：旧的图顶点 ID（"v1"）仍然可以查询
- **渐进迁移**：新数据自动使用统一锚点，旧数据可选迁移
- **API 不变**：SQL/SPARQL 接口不变，底层自动联动
