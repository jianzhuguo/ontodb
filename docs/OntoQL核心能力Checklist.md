# OntoQL 核心能力 Checklist

> **目标**：强化 OntoQL 差异化优势——文档、建模工具、推理性能
> **基线**：OntoDB v0.7.0
> **优先级**：P0 核心能力补全 > 文档 > 推理优化 > 建模工具

---

## 一、OntoQL 文档（1-2 周）

> ✅ 已完成 → `docs/OntoQL语法参考手册.md`

### 1.1 语法参考手册
- [x] CREATE ONTOLOGY 语法（类、属性、关系、约束）
- [x] CLASS 定义（SUBCLASS OF、EQUIVALENT TO、DISJOINT WITH）
- [x] PROPERTY 定义（DOMAIN、RANGE、REQUIRED、TRANSITIVE、SYMMETRIC、INVERSE OF、FUNCTIONAL）
- [x] UNIQUE 约束语法
- [x] INSERT / UPDATE / DELETE 语法
- [x] SELECT 语法（WHERE、ORDER BY、LIMIT、OFFSET）

### 1.2 查询语法手册
- [x] MATCH 查询：`MATCH (v: Class) WHERE ... RETURN ...`
- [x] GRAPH MATCH 查询：`GRAPH MATCH (a:Class) -[edge]-> (b:Class) RETURN ...`
- [x] 子类自动展开规则
- [x] 传递属性自动展开规则
- [x] 逆属性自动推导规则
- [x] 对称属性自动推导规则
- [x] 等价类/等价属性映射规则

### 1.3 高级特性文档
- [x] 混合查询（语义 + 结构 + 向量）
- [x] 跨模态融合查询
- [x] 活数据（衰减/激活/DBA 视图）— 详见 `docs/活数据最小闭环实施Checklist.md`
- [x] 本体推理规则详解（7 条 OWL 2 RL 规则）

### 1.4 示例库
- [x] 医疗场景本体 + 查询示例
- [x] 生物信息场景本体 + 查询示例
- [x] 知识图谱场景本体 + 查询示例
- [x] RAG 场景本体 + 查询示例
- [x] 企业 OA 场景本体 + 查询示例

### 1.5 迁移指南
- [x] SQL → OntoQL 迁移指南（JOIN → 图遍历、UNION → 子类、CTE → 传递属性）
- [x] PostgreSQL → OntoDB 数据迁移指南
- [x] 本体建模最佳实践

---

## 二、P0 核心能力补全（代码实现缺口）

> 代码审计发现的实现缺口，直接影响产品竞争力

### 2.1 GRAPH MATCH 执行器 🔴 最高优先级
> ✅ 已完成
- [x] 完善 `parse_graph_match`：解析 `(a:Label) -[e:edge]-> (b:Label)` 为真实 GraphPattern
- [x] 实现 `execute_graph_match`：桥接 GraphPattern → TraversalEngine
- [x] 支持多跳匹配：`(a) -[e1]-> (b) -[e2]-> (c)`
- [x] 支持 WHERE 过滤：对节点属性和边属性过滤
- [x] 支持 RETURN 投影：返回节点属性、边属性
- [x] 集成本体推理：子类自动展开、逆属性双向遍历、目标节点子类扩展

### 2.2 GRAPH SHORTEST PATH 执行器 🔴
> ✅ 已完成
- [x] 实现 `execute_graph_shortest_path`
- [x] 支持 MAX DEPTH 限制
- [x] 返回路径节点序列

### 2.3 本体验证（循环继承检测）🔴
> ✅ 已完成
- [x] 实现 `Ontology::validate()` 方法
- [x] 检测循环继承：A extends B extends A → 报错
- [x] 检测循环等价：A equiv B equiv A → 报错
- [x] 在 CREATE ONTOLOGY 时自动调用验证
- [ ] 在 `POST /api/ontology/:name/validate` 暴露验证 API（待建模工具阶段）

### 2.4 真增量推理（替换伪增量）🟡
> **现状**: `reason_incremental` 只是合并后全量推理。规则级 `new_facts` 增量已存在。
- [ ] 实现依赖图：记录哪个推导事实依赖哪些原始事实
- [ ] 实现增量删除传播：原始事实被删除时，级联删除依赖它的推导事实
- [ ] 实现脏标记机制：标记哪些实体/属性发生了变化
- [ ] `reason_incremental` 改为真正增量：只对脏区域重新推理
- [ ] 基准测试：增量推理 vs 全量推理（100/1000/10000 节点）

### 2.5 递归 CTE 支持 🟡
> ✅ 已完成（SQL 兼容性框架的一部分）
- [x] 递归 CTE 迭代执行：物化 → 扫描 → 合并 → 重复直到不动点
- [x] UNION 去重 / UNION ALL 保留重复
- [x] 最大迭代次数限制（100 次）

### 2.6 RANK/DENSE_RANK 正确实现 🟡
> ✅ 已完成（SQL 兼容性框架的一部分）
- [x] RANK：按 ORDER BY 列排序，相同值同名次，跳过中间名次
- [x] DENSE_RANK：按 ORDER BY 列排序，相同值同名次，不跳过

### 2.7 推理结果溯源（EXPLAIN REASONING）🟢
> ✅ 已完成
- [x] 支持 `EXPLAIN REASONING <query>` 语法
- [x] 返回推导链：规则名 + 前提三元组 → 结论三元组
- [x] 包含汇总：原始事实数、推导事实数、迭代次数、各规则应用次数
- [ ] HTTP API 暴露推理溯源（待建模工具阶段）

---

## 三、推理性能优化（1-2 周）

### 3.1 推理结果缓存
> ✅ 已完成
- [x] 优化 `class_hierarchy_cache` 失效策略：精细失效（按 ontology name）
- [x] 实现 `transitive_closure_cache`：传递闭包结果缓存结构
- [x] 实现 `inverse_property_cache`：逆属性映射缓存（从 merged ontology 合并时填充）
- [x] 缓存失效：`invalidate_ontology` 按 ontology name 选择性清除

### 3.2 索引优化
> ✅ 已完成
- [x] 类层次索引：merged ontology 缓存，避免每次查询都 scan_prefix
- [x] 属性索引：class_properties 缓存，快速查找类的所有属性
- [x] 逆属性索引：inverse_property 缓存，从 merged ontology 合并时填充
- [ ] 三元组索引：SPO / POS / OSP 三向索引（triple_store 已有部分，后续优化）

### 3.3 并行化
> ✅ 已完成
- [x] 规则并行应用：`reason_parallel` 使用 `std::thread::scope` 并行执行独立规则
- [ ] 批量推理：大本体图分批处理（后续按需优化）

### 3.4 性能测试
> ✅ 已完成（测试脚本）
- [x] 基准测试脚本：`benches/reasoning_benchmark.py`
- [x] 覆盖场景：小/中/大本体推理、缓存命中率、MATCH+推理、EXPLAIN REASONING
- [ ] 实际运行基准测试（需启动 ontodb-server）

---

## 四、本体建模工具（2-3 周）

> **现状分析**：本体操作已通过 `POST /api/query` + `CREATE ONTOLOGY` 语法全部可用。
> 缺的是**专用 REST 端点**和**可视化 UI**。

### 4.1 后端 API
> **已有能力**（通过 `/api/query` + OntoQL 语法）：
> - ✅ 创建本体：`CREATE ONTOLOGY` + `OntologyParser` + `Ontology::validate()`
> - ✅ 删除本体：`DROP ONTOLOGY`（http.rs:883）
> - ✅ Schema 自省：`GET /api/schema`（返回 classes、indexes、vector_indexes）
> - ✅ 本体验证：`Ontology::validate()` 在 CREATE 时自动调用
>
> **需要新增**的专用 REST 端点：
- [ ] `GET /api/ontology` — 列出所有本体（目前需查 schema）
- [ ] `GET /api/ontology/:name` — 获取本体详情（类、属性、关系图）
- [ ] `PUT /api/ontology/:name` — 更新本体（目前需 DROP + CREATE）
- [ ] `GET /api/ontology/:name/graph` — 获取本体关系图 JSON（D3/Cytoscape 可消费）
- [ ] `POST /api/ontology/:name/validate` — 独立验证端点（已有逻辑，需暴露 API）

### 4.2 Web UI（扩展 console.html）
- [ ] 本体列表页：展示所有本体，支持搜索
- [ ] 本体编辑器：
  - [ ] 类节点可视化（拖拽创建/编辑）
  - [ ] 属性编辑面板（名称、类型、约束）
  - [ ] 关系连线（SUBCLASS OF、属性关联）
  - [ ] 传递/对称/逆属性标记
  - [ ] UNIQUE 约束配置
- [ ] 本体预览：实时展示生成的 CREATE ONTOLOGY SQL
- [ ] 本体验证：点击验证按钮，展示错误/警告
- [ ] 关系图可视化：D3.js 或 Cytoscape.js 渲染类层次图

### 4.3 本体模板
- [ ] 预置模板：医疗场景本体
- [ ] 预置模板：企业 OA 本体
- [ ] 预置模板：知识图谱本体
- [ ] 预置模板：生物信息本体
- [ ] 一键导入模板

### 4.4 本体导入导出
- [ ] 导出为 OWL/RDF 格式
- [ ] 从 OWL/RDF 格式导入
- [ ] 导出为 JSON 格式
- [ ] 从 JSON 格式导入

---

## 五、实施顺序（修订 v4）

> **原则**：聚焦 OntoQL 独有能力（GRAPH MATCH + 推理），SQL 兼容性框架保持完整可用

```
第 1 周: 2.1 GRAPH MATCH + 2.2 SHORTEST PATH + 2.3 本体验证 + 2.5 递归 CTE + 2.6 RANK ✅
第 2 周: 2.7 推理溯源 + GRAPH MATCH 集成推理 ✅
第 3 周: 三、推理缓存 + 索引 + 并行优化 + 性能测试 ✅
第 4 周: 2.4 真增量推理（依赖图 + 增量删除传播）
第 5-6 周: 四、后端本体 API 端点 + 模板 + 导入导出（Web UI 独立产品）
```

---

## 六、工作量总结（修订 v4）

| 阶段 | 内容 | 工作量 | 状态 |
|------|------|--------|------|
| 一、文档 | 语法参考 + 查询手册 + 示例库 + 迁移指南 | 1-2 周 | ✅ 已完成 |
| 二、P0 核心补全 | GRAPH MATCH + SHORTEST PATH + 本体验证 + 推理溯源 + CTE + RANK | 1-2 周 | ✅ 已完成 |
| 三、推理优化 | 缓存 + 索引 + 并行 + 基准测试 | 1 周 | ✅ 已完成 |
| 四、本体 API | 后端 REST 端点 + 模板 + OWL/RDF 导入导出 | 1 周 | 🔲 待实施 |
| 五、真增量推理 | 依赖图 + 增量删除传播 | 1-2 周 | 🔲 待实施 |
| **合计** | | **5-8 周** | |

> **Web UI（本体建模工具）** 建议作为独立产品开发，通过 HTTP API 对接 OntoDB，不计入数据库工作量。

---

**© 2026 原点价值 / OntoValue Technology**
