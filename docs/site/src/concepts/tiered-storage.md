# Tiered Storage

OntoDB automatically moves data between storage tiers based on access patterns.

## Architecture

```
┌─────────────────────────────────────────────┐
│              Hot Tier (MemTable)             │
│  In-memory, fastest access, limited size     │
│  Auto-flush when size threshold reached      │
├─────────────────────────────────────────────┤
│              Warm Tier (TSM on SSD)          │
│  Column-oriented compressed blocks           │
│  Time-series optimized                       │
├─────────────────────────────────────────────┤
│              Cold Tier (Parquet on HDD)      │
│  Long-term archival, lowest cost             │
│  Migrated by age threshold                   │
└─────────────────────────────────────────────┘
```

## How It Works

1. **Write** → Data enters Hot tier (MemTable)
2. **Auto-flush** → When MemTable reaches size threshold, flush to Warm tier
3. **Auto-migrate** → When Warm data exceeds age threshold, migrate to Cold tier
4. **Query** → Transparent query across all tiers

## Configuration

```bash
# Hot tier: MemTable size (default 4MB, max 256MB)
./ontodb-server --memtable-size 67108864  # 64MB

# Warm tier: TSM files on SSD
# Cold tier: Parquet files on HDD (configured separately)
```

## Time-Series Compression

The Warm tier uses TSM (Time-Series Merge) format with:

| Encoding | Use Case | Compression |
|----------|----------|-------------|
| Delta timestamp | Timestamps | ~90% |
| Gorilla float | Float values | ~80% |
| Integer delta | Counters | ~85% |
| zstd block | All data | ~60% |

## Continuous Queries

Periodic aggregation queries on time-series data:

```sql
-- Create continuous query
CREATE CONTINUOUS QUERY avg_temp ON SensorData
  RESAMPLE EVERY 5m
  SELECT AVG(temperature) as avg_temp
  FROM SensorData
  GROUP BY time(1h)
```

Aggregation types: `Mean`, `Sum`, `Min`, `Max`, `Count`, `StdDev`, `Percentile`
