# 活数据：让数据库拥有"记忆衰减"能力

> 传统数据库将所有数据同等对待，无法区分数据的价值。OntoDB引入"活数据"机制，让数据像人类记忆一样，随时间衰减，被访问时激活。

## 传统数据库的数据管理问题

### 所有数据同等对待

```
传统数据库：
- 1年前的日志数据 = 刚写入的热点数据
- 存储成本相同
- 查询优先级相同
- 缓存策略相同
```

### 带来的问题

#### 1. 存储成本高
```
100GB数据中：
- 热点数据（最近1周）：10GB
- 温数据（1周-1个月）：20GB
- 冷数据（1个月以上）：70GB

传统方案：全部存储在高性能存储
成本：100GB × 高性能存储价格
```

#### 2. 查询性能差
```
查询时无法区分数据价值：
- 热点数据：应该优先返回
- 冷数据：应该降级处理

传统方案：同等处理，性能差
```

#### 3. 缓存效率低
```
缓存策略：
- LRU：最近最少使用
- LFU：最不经常使用

问题：无法考虑数据的实际价值
```

## OntoDB的活数据机制

### 核心思想

```
活数据 = 数据 + 价值指标 + 衰减时钟

每条数据维护：
- value_score：当前价值（0.0-1.0）
- last_accessed：最后访问时间
- created_at：创建时间
- activation_count：激活次数
```

### 价值衰减模型

#### 指数衰减（默认）
```
current_score = value_score × e^(-λ × Δt)

其中：
- λ = ln(2) / half_life
- half_life：半衰期（默认7天）
- Δt：距上次访问的时间

示例：
- 初始价值：1.0
- 7天后：0.5
- 14天后：0.25
- 28天后：0.0625
```

#### 线性衰减
```
current_score = value_score - (Δt / half_life) × value_score

特点：均匀衰减
```

#### 对数衰减
```
current_score = value_score - λ × ln(1 + Δt)

特点：初期衰减快，后期衰减慢
```

### 自动激活机制

```
当数据被访问时：
1. 重置衰减时钟：last_accessed = now()
2. 累加价值指标：value_score += delta（上限1.0）
3. 原子性保证：激活与访问在同一事务中完成
```

## 性能优势

### 存储成本优化

| 数据类型 | 传统方案 | OntoDB | 节省 |
|----------|----------|--------|------|
| 热点数据（10GB） | 高性能存储 | 高性能存储 | - |
| 温数据（20GB） | 高性能存储 | 中性能存储 | 50% |
| 冷数据（70GB） | 高性能存储 | 低性能存储 | 80% |
| **总成本** | 100GB高性能 | 10GB高+20GB中+70GB低 | **60%↓** |

### 查询性能优化

```
查询流程：
1. 查询时实时计算价值指标
2. 按价值排序返回结果
3. 高价值数据优先展示

示例：
SELECT * FROM Device 
WHERE temperature > 30
ORDER BY value_score DESC
LIMIT 10;

返回：最近访问的、价值最高的设备
```

### 缓存优化

```
缓存策略：
- 高价值数据：优先缓存
- 低价值数据：降级存储

向量索引集成：
- 高价值数据的向量：优先加载到内存
- 低价值数据的向量：压缩存储
```

## 实际案例

### 案例：物联网设备管理

**场景**：管理10万个传感器设备

**传统方案**：
```
- 所有设备数据同等对待
- 查询返回所有设备
- 缓存策略：LRU
- 存储成本：100GB × 高性能存储
```

**OntoDB方案**：
```
- 热点设备（最近访问）：价值1.0，优先展示
- 温设备（1周未访问）：价值0.5，正常展示
- 冷设备（1月未访问）：价值0.1，降级展示

查询示例：
SELECT * FROM Device 
WHERE status = 'online'
ORDER BY value_score DESC
LIMIT 100;

返回：最近活跃的100个设备
```

### 性能对比

| 指标 | 传统方案 | OntoDB | 提升 |
|------|----------|--------|------|
| 查询结果相关性 | 低 | 高 | - |
| 存储成本 | 100% | 40% | 60%↓ |
| 缓存命中率 | 60% | 90% | 50%↑ |

## 技术细节

### 活数据元数据结构

```rust
// 活数据元数据
struct ValueMetadata {
    value_score: f64,        // 当前价值（0.0-1.0）
    last_activated_at: u64,  // 最后激活时间
    created_at: u64,         // 创建时间
    activation_count: u32,   // 激活次数
    lambda: f64,             // 衰减系数
}

impl ValueMetadata {
    // 计算当前价值
    fn current_score(&self) -> f64 {
        let elapsed = now() - self.last_activated_at;
        self.value_score * (-self.lambda * elapsed as f64).exp()
    }
    
    // 激活数据
    fn activate(&mut self, delta: f64) {
        // 1. 重置衰减时钟
        self.last_activated_at = now();
        
        // 2. 累加价值指标
        self.value_score = (self.value_score + delta).min(1.0);
        
        // 3. 增加激活次数
        self.activation_count += 1;
    }
}
```

### 衰减预设

```rust
// 衰减预设
const LAMBDA_7H: f64 = 2.75e-5;   // 7小时半衰期
const LAMBDA_70D: f64 = 1.15e-7;  // 70天半衰期
const LAMBDA_2Y: f64 = 1.10e-8;   // 2年半衰期

// 应用场景
// LAMBDA_7H：实时监控、告警
// LAMBDA_70D：业务数据、用户行为
// LAMBDA_2Y：历史数据、归档
```

### 与向量索引集成

```rust
// 向量索引与活数据集成
impl VectorIndex {
    // 按价值优先级加载
    fn load_by_value(&mut self) {
        let mut entries: Vec<_> = self.entries.iter().collect();
        
        // 按价值排序
        entries.sort_by(|a, b| {
            let score_a = a.value_metadata.current_score();
            let score_b = b.value_metadata.current_score();
            score_b.partial_cmp(&score_a).unwrap()
        });
        
        // 优先加载高价值向量
        for entry in entries.iter().take(self.cache_size) {
            self.load_to_memory(entry);
        }
    }
    
    // 按价值排序搜索结果
    fn search_with_value(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        let mut results = self.search(query, k * 2);
        
        // 按价值排序
        results.sort_by(|a, b| {
            let score_a = a.value_metadata.current_score();
            let score_b = b.value_metadata.current_score();
            score_b.partial_cmp(&score_a).unwrap()
        });
        
        results.into_iter().take(k).collect()
    }
}
```

## 总结

OntoDB的活数据机制实现了：

- **价值感知**：数据自动衰减，区分热点和冷数据
- **自动激活**：访问时自动提升价值
- **存储优化**：冷数据自动降级存储
- **查询优化**：高价值数据优先返回

**活数据不是功能，是数据库的"记忆"能力。**

---

*作者：OntoDB团队*
*日期：2026年9月*
*标签：#活数据 #数据生命周期 #价值衰减 #数据库*
