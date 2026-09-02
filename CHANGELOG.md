# Changelog

## [0.7.0] - 2026-09-01

### 🚀 新增功能

#### 命名空间支持
- 添加 `CREATE/DROP/USE NAMESPACE` 语法
- 支持多项目数据隔离
- 命名空间内的本体组织

#### 本体继承修复
- 修复跨本体继承验证问题
- 支持 `CREATE CLASS Employee EXTENDS Person` 跨本体引用
- 创建本体时自动合并同命名空间内的本体

#### AI 框架集成
- 添加 LangChain 向量存储适配器
- 添加 LlamaIndex Reader 和 VectorStore 适配器
- 添加内置 RAG 支持
- 添加 Embedding 工具类（OpenAI、Cohere）

#### 企业版分离
- 实现社区版/企业版双许可模式
- 添加 License 验证模块
- Feature Flags 控制企业功能编译
- 添加构建脚本（build.sh/build.ps1）

### 🔧 性能优化

#### 存储引擎
- 集成 MemoryManager 自适应内存管理
- SSTable Index Block 添加 CRC32 校验
- SST Cache 升级为 RwLock 提升并发
- WAL 文件大小上限和自动轮转
- WAL 同步线程优雅关闭
- MemTable 大小计算包含 BTreeMap 节点开销

#### 查询引擎
- 添加参数化查询支持（$1, :param）
- 自动统计信息收集
- 直方图选择性估计
- LIMIT 通过 JOIN 下推
- 向量搜索自适应预过滤/后过滤

#### 推理引擎
- 实现真正的增量推理
- 传递属性推理内存限制
- 推理缓存细粒度失效

### 🛡️ 安全修复

- import/export 端点添加标识符验证
- Cursor 分页添加签名保护
- SPARQL 路由统一（/sparql 和 /api/sparql）
- 写-写冲突检测

### 🐛 问题修复

- 修复事务内快照读取
- 修复 SSTable Index CRC 校验兼容性
- 修复 Digital Twin 页面数据持久化
- 修复 DROP ONTOLOGY 命名空间支持
- 修复 zstd 解压失败静默回退

### 📦 API 变更

- 新增 `POST /api/restore` 端点
- 新增 `POST /api/digital-twin/layout` 端点
- 新增 `GET /api/digital-twin/layout` 端点
- 新增 `POST /api/sparql` 路由别名
- 默认速率限制提升至 600/min

### 📊 性能基准

| 指标 | 结果 |
|------|------|
| 写入吞吐 | 887K writes/sec |
| 点查吞吐 | 882K reads/sec |
| 批量写入 | 1.15M rows/sec |
| 向量搜索 | 415µs latency |
| 本体推理 | 56µs (small) |

### 🧪 测试覆盖

- onto-core: 82 tests
- onto-storage: 165 tests
- onto-ontology: 69 tests
- onto-query: 201 tests
- **总计: 517 tests passed**
