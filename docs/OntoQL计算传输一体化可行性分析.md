# OntoQL 作为计算+传输单元 — 可行性分析

## 一、现状盘点

### 已有的计算能力

OntoQL 已经支持的 SQL 计算：

| 类别 | 支持 | 位置 |
|------|------|------|
| 条件判断 | CASE WHEN ... THEN ... ELSE ... END | executor.rs:10257 |
| 空值处理 | COALESCE, NULLIF | executor.rs:7379-7393 |
| 字符串 | CONCAT, SUBSTRING, UPPER, LOWER, LENGTH, TRIM | executor.rs:7394-7465 |
| 数学 | ABS, ROUND | executor.rs:7452-7475 |
| 聚合 | COUNT, SUM, AVG, MIN, MAX + GROUP BY + HAVING | executor.rs (多处) |
| 子查询 | 标量子查询、IN 子查询 | parser.rs:501 |
| CTE | WITH ... AS (SELECT ...) | executor.rs:4856 |
| 排序 | ORDER BY ASC/DESC | 已支持 |
| 连接 | INNER/LEFT/RIGHT JOIN | 已支持 |

**结论：计算层已经够用，缺的不是计算能力，是"计算完之后怎么办"。**

### 已有的传输能力

| 机制 | 方向 | 触发方式 | 位置 |
|------|------|---------|------|
| CDC Kafka | 出站 | WAL 变更自动触发 | cdc.rs:95 |
| CDC Webhook | 出站 | WAL 变更自动触发 | cdc.rs:540-575 |
| HTTP API | 双向 | 客户端主动请求 | http.rs:442 |
| PG Wire | 入站 | 客户端连接 | pgwire.rs |
| INFER REMOTE @mind | 出站 | 查询时触发 | ontoql.rs:874-878 |

**关键发现：CDC Webhook 已经用 reqwest 做 HTTP 出站调用，架构可复用。**

### 缺失的关键能力

```
❌ SEND TO / PIPE TO 语法 — 查询结果直接发到外部
❌ UDF / 自定义函数 — 在 OntoQL 内嵌业务逻辑
❌ 远程数据库连接器 — 直接查/写外部数据库
❌ 数据格式转换 — JSON/CSV/Protobuf 输出
❌ 异步/流式执行 — 查询结果边算边发
```

---

## 二、架构设计：OntoQL 计算传输一体化

### 目标语句

```sql
-- 1. 查询结果直接推送到 HTTP 端点
SELECT tool_name, COUNT(*) as cnt 
FROM MimoToolCall 
GROUP BY tool_name
SEND TO HTTP 'https://api.example.com/ingest' FORMAT JSON;

-- 2. 条件触发推送（告警场景）
SELECT * FROM TemperatureReading 
WHERE temperature > 35
SEND TO HTTP 'https://alert.example.com/webhook' FORMAT JSON;

-- 3. 管道模式：查询 → 计算 → 推送
WITH stats AS (
  SELECT session_id, COUNT(*) as calls, 
         SUM(has_output) as success
  FROM MimoToolCall 
  GROUP BY session_id
)
SELECT *, ROUND(success * 100.0 / calls, 1) as success_rate
FROM stats
SEND TO HTTP 'https://dashboard.example.com/api' FORMAT JSON;

-- 4. 推送到 OntoDB 集群节点
SELECT * FROM BizMetrics WHERE metric = '营收'
SEND TO ONTODB '10.0.0.2:7913' CLASS 'RemoteMetrics';
```

### 执行流程

```
OntoQL Parser
    ↓
Query Executor (计算层 — 已有)
    ↓
QueryResult::Rows(Vec<Map<String, Value>>)
    ↓
┌─────────────────────────────────┐
│  Transport Layer (新增)          │
│  ┌───────────┐ ┌──────────────┐ │
│  │ HTTP Sink │ │ OntoDB Sink  │ │
│  │ (reqwest) │ │ (TCP client) │ │
│  └───────────┘ └──────────────┘ │
│  ┌───────────┐ ┌──────────────┐ │
│  │ Kafka Sink│ │ WebSocket    │ │
│  │ (已有)    │ │ Sink         │ │
│  └───────────┘ └──────────────┘ │
└─────────────────────────────────┘
    ↓
外部系统接收数据
```

---

## 三、可行性评估

### 3.1 Parser 层改动

**难度：低**

在 ontoql.rs 的 SELECT 解析末尾加 `SEND TO` 分支：

```rust
// 现有：SELECT 解析完成后返回
// 新增：检查是否有 SEND TO 后缀
if starts_with_ignore_ascii_case(rest, "SEND TO") {
    let endpoint = parse_endpoint(rest);
    let format = parse_format(rest); // JSON/CSV/BinaryRow
    return Ok(OntoQLAst::SendTo { query, endpoint, format });
}
```

AST 新增一个节点：
```rust
SendTo {
    query: Box<QueryAst>,
    endpoint: SendEndpoint, // HTTP / ONTODB / KAFKA / WS
    format: OutputFormat,   // JSON / CSV / BINARY
}
```

**工作量：2-3 天**

### 3.2 Executor 层改动

**难度：中**

```rust
// executor.rs 新增
fn execute_send_to(&self, query: &QueryAst, endpoint: &SendEndpoint, format: &OutputFormat) -> Result<QueryResult> {
    // 1. 先执行查询
    let result = self.execute(query)?;
    let rows = match result {
        QueryResult::Rows(r) => r,
        _ => return Err(CoreError::InvalidArgument("SEND TO requires a SELECT query")),
    };
    
    // 2. 格式化
    let payload = match format {
        OutputFormat::Json => serde_json::to_string(&rows)?,
        OutputFormat::Csv => rows_to_csv(&rows),
        OutputFormat::Binary => rows_to_binary(&rows),
    };
    
    // 3. 发送到目标
    match endpoint {
        SendEndpoint::Http(url) => {
            reqwest::Client::new()
                .post(url)
                .header("Content-Type", "application/json")
                .body(payload)
                .send()
                .await?;
        }
        SendEndpoint::Ontodb(addr, class) => {
            // 通过 TCP 连接远程 OntoDB，批量 INSERT
        }
        SendEndpoint::Kafka(broker, topic) => {
            // 复用已有 CdcKafkaPublisher
        }
    }
    
    Ok(QueryResult::Success(format!("{} rows sent to {}", rows.len(), endpoint)))
}
```

**关键点：CDC Webhook 已经验证了 reqwest 出站调用的模式，直接复用。**

**工作量：1 周**

### 3.3 Transport Layer

**难度：中**

```rust
// 新建 crates/onto-query/src/transport.rs

pub trait DataSink {
    fn send(&self, payload: &[u8], content_type: &str) -> Result<()>;
}

pub struct HttpSink { url: String }
pub struct OntoDbSink { addr: String }
pub struct KafkaSink { brokers: String, topic: String }
pub struct WebSocketSink { url: String }
```

已有依赖：
- `reqwest` — CDC Webhook 已用，直接复用
- `rdkafka` — CDC Kafka 已用
- `tokio` — 异步运行时已有

**工作量：3-4 天**

### 3.4 格式转换

**难度：低**

```rust
fn rows_to_csv(rows: &[Map<String, Value>]) -> String {
    // header + rows
}

fn rows_to_ndjson(rows: &[Map<String, Value>]) -> String {
    // 每行一个 JSON 对象
}
```

**工作量：1 天**

---

## 四、好处分析

### 对比传统方案

| 维度 | 传统方案 (ETL) | OntoQL 一体化 |
|------|---------------|--------------|
| 架构 | DB → App → Transform → Send | DB 直接算+发 |
| 延迟 | 3 层跳转，秒级 | 1 层，毫秒级 |
| 运维 | 3 个组件要维护 | 1 个组件 |
| 一致性 | 跨组件可能丢数据 | 同进程，ACID 保证 |
| 开发成本 | 写 ETL 脚本 | 一条 SQL |
| 复杂度 | O(n) 个中间件 | O(1) |

### 具体价值

**1. 消除 ETL 中间层**

```
传统：OntoDB → Python脚本 → Transform → HTTP POST → 目标系统
新：  OntoDB SELECT ... SEND TO HTTP 'url'
```

**2. 实时告警不用写代码**

```sql
-- 温度超限自动推送告警
SELECT sensor_id, temperature, timestamp 
FROM TemperatureReading 
WHERE temperature > 35
SEND TO HTTP 'https://alert.example.com/webhook';
```

**3. 数据同步一条 SQL**

```sql
-- 把 OntoDB 数据同步到远程 PostgreSQL
SELECT * FROM MimoToolCall 
SEND TO HTTP 'https://pg-proxy.example.com/ingest' 
FORMAT JSON;
```

**4. 聚合结果实时推送仪表盘**

```sql
-- 每次查询自动刷新 dashboard
SELECT 
  tool_name, 
  COUNT(*) as cnt,
  ROUND(AVG(has_output) * 100, 1) as success_rate
FROM MimoToolCall 
GROUP BY tool_name
SEND TO HTTP 'https://grafana.example.com/api/push';
```

**5. 跨 OntoDB 节点数据分发**

```sql
-- 把热数据推到边缘节点
SELECT * FROM SensorData 
WHERE status = 'critical'
SEND TO ONTODB '10.0.0.2:7913' CLASS 'EdgeAlert';
```

---

## 五、风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| 目标系统不可用 | SEND TO 失败 | 加重试队列 + 死信队列 |
| 大结果集阻塞 | 查询卡住 | 分批发送 + 流式执行 |
| 安全风险 | 任意 URL 调用 | 白名单机制 + 认证 |
| 异步执行复杂度 | 引入 tokio 依赖 | 已有 tokio，复用 |

---

## 六、实施路径

| 阶段 | 内容 | 工期 | 优先级 |
|------|------|------|--------|
| P0 | SEND TO HTTP 'url' FORMAT JSON | 1 周 | 最高 |
| P1 | 重试 + 错误处理 + 白名单 | 3 天 | 高 |
| P2 | SEND TO ONTODB (跨节点) | 1 周 | 中 |
| P3 | SEND TO KAFKA (复用 CDC) | 3 天 | 中 |
| P4 | UDF / 自定义函数 | 2 周 | 低 |

**最小可用版本：1 周。** 只需在 parser 加 `SEND TO` 语法，executor 调用已有 `reqwest` 发 HTTP POST。
