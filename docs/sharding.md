# OntoDB 数据分片指南

## 概述

OntoDB 支持数据水平分片，可将大规模数据分散到多个分片中，提升查询性能和存储容量。

## 分片策略

### 1. Class-based（基于类的分片）

将不同的类（表）分配到不同的分片。

**适用场景：**
- 多租户系统，每个租户的数据隔离
- 按业务模块分离数据
- 数据量不均匀的表

**配置示例：**
```json
{
  "class_strategies": {
    "User": {"ClassBased": {"shard": 1}},
    "Order": {"ClassBased": {"shard": 2}},
    "Product": {"ClassBased": {"shard": 3}}
  }
}
```

### 2. Range-based（基于范围的分片）

按主键范围将数据分割到不同分片。

**适用场景：**
- 时间序列数据（按时间范围分片）
- 有序数据（按字母/数字范围分片）
- 需要范围查询的场景

**配置示例：**
```json
{
  "class_strategies": {
    "Log": {
      "RangeBased": {
        "ranges": [
          {"end_key": "2024-01", "shard": 0},
          {"end_key": "2024-07", "shard": 1},
          {"end_key": "2025-01", "shard": 2}
        ]
      }
    }
  }
}
```

### 3. Hash-based（基于哈希的分片）

按主键哈希值将数据均匀分配到各分片。

**适用场景：**
- 数据均匀分布
- 无明显热点
- 需要负载均衡

**配置示例：**
```json
{
  "class_strategies": {
    "User": {
      "HashBased": {
        "num_shards": 4,
        "slot_map": [0, 0, 1, 1]
      }
    }
  }
}
```

## 配置文件

### 完整配置示例

创建 `config/sharding.json`：

```json
{
  "default_shard": 0,
  "shards": {
    "0": {
      "id": 0,
      "name": "default",
      "raft_group": null,
      "replicas": [0],
      "is_primary": true
    },
    "1": {
      "id": 1,
      "name": "shard-beijing",
      "raft_group": 1,
      "replicas": [0, 1],
      "is_primary": true
    },
    "2": {
      "id": 2,
      "name": "shard-shanghai",
      "raft_group": 2,
      "replicas": [0, 2],
      "is_primary": true
    }
  },
  "class_strategies": {
    "User": {"HashBased": {"num_shards": 4, "slot_map": [0, 0, 1, 1]}},
    "Order": {"ClassBased": {"shard": 1}},
    "Product": {"RangeBased": {"ranges": [
      {"end_key": "m", "shard": 0},
      {"end_key": "t", "shard": 1},
      {"end_key": "z", "shard": 2}
    ]}}
  }
}
```

**注意：** `shards` 字段使用 HashMap 格式（键为分片 ID 的字符串形式），而非数组。

## 启动服务器

```bash
# 启用分片
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --sharding-enabled --sharding-config config/sharding.json

# 指定默认分片
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --sharding-enabled --default-shard 1

# 配置迁移参数
./ontodb-server --data-dir ./data --http 127.0.0.1:7912 --sharding-enabled \
  --migration-batch-size 5000 \
  --max-concurrent-migrations 4
```

## API 操作

### 分片管理

```bash
# 查看分片配置
curl http://127.0.0.1:7912/api/sharding/config

# 添加分片
curl -X POST http://127.0.0.1:7912/api/sharding/shard \
  -H "Content-Type: application/json" \
  -d '{"id": 3, "name": "shard-guangzhou"}'

# 为类分配分片策略
curl -X POST http://127.0.0.1:7912/api/sharding/class \
  -H "Content-Type: application/json" \
  -d '{"class": "User", "strategy": "hash", "num_shards": 4, "slot_map": [0, 0, 1, 1]}'

# 查看分片状态
curl http://127.0.0.1:7912/api/sharding/status
```

### 分片扩容

```bash
# 添加分片并自动重新平衡
curl -X POST http://127.0.0.1:7912/api/sharding/scale/add \
  -H "Content-Type: application/json" \
  -d '{"shard": {"id": 3, "name": "shard-new"}, "rebalance": true}'

# 移除分片（数据迁移到目标分片）
curl -X POST http://127.0.0.1:7912/api/sharding/scale/remove \
  -H "Content-Type: application/json" \
  -d '{"shard_id": 2, "target_shard": 1}'
```

### 数据迁移

```bash
# 创建迁移任务
curl -X POST http://127.0.0.1:7912/api/sharding/migrate \
  -H "Content-Type: application/json" \
  -d '{"source_shard": 0, "target_shard": 1, "class": "User"}'

# 更新迁移进度
curl -X PUT http://127.0.0.1:7912/api/sharding/migrate/progress \
  -H "Content-Type: application/json" \
  -d '{"migration_id": "mig_0_1_0", "total_records": 10000, "migrated_records": 5000}'

# 完成迁移
curl -X POST http://127.0.0.1:7912/api/sharding/migrate/complete \
  -H "Content-Type: application/json" \
  -d '{"migration_id": "mig_0_1_0"}'

# 取消迁移
curl -X POST http://127.0.0.1:7912/api/sharding/migrate/cancel \
  -H "Content-Type: application/json" \
  -d '{"migration_id": "mig_0_1_0"}'

# 查看迁移历史
curl http://127.0.0.1:7912/api/sharding/migrations
```

### 分片分裂

```bash
# 将分片分裂为多个新分片
curl -X POST http://127.0.0.1:7912/api/sharding/split \
  -H "Content-Type: application/json" \
  -d '{
    "source_shard": 0,
    "new_shards": [
      {"id": 3, "name": "new-shard-1"},
      {"id": 4, "name": "new-shard-2"}
    ],
    "strategy": "even"
  }'
```

### 重新平衡

```bash
# 重新平衡所有类
curl -X POST http://127.0.0.1:7912/api/sharding/rebalance \
  -H "Content-Type: application/json" \
  -d '{"classes": []}'

# 重新平衡指定类
curl -X POST http://127.0.0.1:7912/api/sharding/rebalance \
  -H "Content-Type: application/json" \
  -d '{"classes": ["User", "Order"]}'
```

## 查询路由

启用分片后，查询会自动路由到正确的分片。查询结果中包含 `__shard__` 字段：

```json
{
  "__class__": "User",
  "__pk__": "User::001",
  "__shard__": "Single(1)",
  "name": "Alice",
  "age": 30
}
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `SHARDING_ENABLED` | 启用分片 | `false` |
| `SHARDING_CONFIG_FILE` | 配置文件路径 | 无 |
| `SHARDING_DEFAULT_SHARD` | 默认分片 ID | `0` |
| `SHARDING_AUTO_REBALANCE` | 自动重新平衡 | `true` |
| `SHARDING_MIGRATION_BATCH_SIZE` | 迁移批次大小 | `1000` |
| `SHARDING_MIGRATION_PROGRESS_INTERVAL` | 进度更新间隔(秒) | `5` |
| `SHARDING_MAX_CONCURRENT_MIGRATIONS` | 最大并发迁移数 | `2` |
| `SHARDING_HEALTH_CHECK_INTERVAL` | 健康检查间隔(秒) | `30` |
| `SHARDING_SPLIT_ENABLED` | 启用分片分裂 | `true` |
| `SHARDING_MAX_SPLIT_SHARDS` | 最大分裂分片数 | `4` |

## 最佳实践

1. **选择合适的分片键**
   - 选择查询频率高的字段作为分片键
   - 避免热点数据集中在同一分片
   - 考虑数据分布的均匀性

2. **分片数量规划**
   - 根据数据量和查询负载规划分片数量
   - 预留扩容空间
   - 分片数量建议为 2 的幂次方（便于哈希分布）

3. **迁移策略**
   - 在业务低峰期进行迁移
   - 使用批量迁移减少对业务的影响
   - 监控迁移进度和系统负载

4. **监控告警**
   - 监控各分片的数据量和查询延迟
   - 设置分片健康检查告警
   - 定期检查分片分布是否均匀
