# Sharding Management

OntoDB supports horizontal scaling through data sharding.

## Sharding Strategies

| Strategy | Description | Use Case |
|----------|-------------|----------|
| **Class-based** | Different classes on different shards | Multi-tenant, data isolation |
| **Range-based** | Key ranges determine shard | Time-series, sequential IDs |
| **Hash-based** | FNV-1a hash of primary key | Even distribution, no hotspots |

## Configuration

### Via Config File

```json
{
  "default_shard": 0,
  "shards": {
    "0": {"host": "node1", "port": 7912},
    "1": {"host": "node2", "port": 7912},
    "2": {"host": "node3", "port": 7912}
  },
  "class_strategies": {
    "users": {"type": "hash", "num_shards": 3},
    "logs": {"type": "range", "shard": 1},
    "products": {"type": "class", "shard": 0}
  }
}
```

### Via HTTP API

```bash
# Get current sharding config
GET /api/sharding/config

# Update sharding config
PUT /api/sharding/config
{
  "default_shard": 0,
  "shards": {...},
  "class_strategies": {...}
}
```

## Shard Management

### Add Shard

```bash
POST /api/sharding/shard
{
  "shard_id": 3,
  "host": "node4",
  "port": 7912
}
```

### Assign Class to Shard

```bash
POST /api/sharding/class
{
  "class_name": "logs",
  "strategy": "range",
  "shard": 1
}
```

### Get Sharding Status

```bash
GET /api/sharding/status
```

Response:
```json
{
  "shards": {
    "0": {"status": "active", "entries": 100000},
    "1": {"status": "active", "entries": 50000},
    "2": {"status": "active", "entries": 75000}
  },
  "total_entries": 225000
}
```

## Data Migration

### Start Migration

```bash
POST /api/sharding/migrate
{
  "from_shard": 0,
  "to_shard": 3,
  "class_name": "users"
}
```

### Update Migration Progress

```bash
PUT /api/sharding/migrate/progress
{
  "migration_id": "mig_abc123",
  "progress": 50
}
```

### Complete Migration

```bash
POST /api/sharding/migrate/complete
{
  "migration_id": "mig_abc123"
}
```

### Cancel Migration

```bash
POST /api/sharding/migrate/cancel
{
  "migration_id": "mig_abc123"
}
```

### List Migrations

```bash
GET /api/sharding/migrations
```

## Shard Operations

### Rebalance

Redistribute classes evenly across shards:

```bash
POST /api/sharding/rebalance
```

### Add Shard and Rebalance

```bash
POST /api/sharding/scale/add
{
  "shard_id": 3,
  "host": "node4",
  "port": 7912,
  "auto_rebalance": true
}
```

### Remove Shard

```bash
POST /api/sharding/scale/remove
{
  "from_shard": 2,
  "to_shard": 0
}
```

### Split Shard

```bash
POST /api/sharding/split
{
  "shard_id": 0,
  "strategy": "hash",
  "new_shard_id": 3
}
```

Split strategies: `even`, `range`, `hash`

## Query Routing

OntoDB automatically routes queries to the correct shard:

- **Point queries** (GET/PUT/DELETE by key) → single shard
- **Full scans** → all relevant shards
- **Range queries** → overlapping shards

```bash
# This query is automatically routed to the correct shard
curl -X POST http://localhost:7912/api/query \
  -d '{"query": "SELECT * FROM users WHERE id = \"user_123\""}'
```

## Shard Statistics

```bash
GET /api/sharding/status
```

Per-shard statistics include:
- Entry count
- Status (active, migrating, offline, splitting, rebalancing)
- Storage size
