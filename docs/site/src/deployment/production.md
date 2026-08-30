# Production Deployment

## Hardware Recommendations

| Workload | CPU | Memory | Storage |
|----------|-----|--------|---------|
| Development | 2 cores | 4 GB | 20 GB SSD |
| Small production | 4 cores | 16 GB | 100 GB SSD |
| Medium production | 8 cores | 32 GB | 500 GB NVMe |
| Large production | 16+ cores | 64+ GB | 1+ TB NVMe |

## System Configuration

### File Descriptors

OntoDB uses many file handles for LSM-Tree operations:

```bash
# /etc/security/limits.conf
ontodb soft nofile 65536
ontodb hard nofile 65536
```

### Memory

Set MemTable size based on available RAM:

```bash
# 16GB RAM → 64MB MemTable
./ontodb-server --memtable-size 67108864

# 64GB RAM → 256MB MemTable
./ontodb-server --memtable-size 268435456
```

## Security

### API Key Authentication

```bash
./ontodb-server --auth --api-key-file /etc/ontodb/api-keys.json
```

API keys file format:
```json
{
  "keys": {
    "your-api-key": {
      "name": "production-app",
      "permissions": ["read", "write"]
    }
  }
}
```

### TLS

```bash
./ontodb-server \
  --tls-cert /etc/ontodb/cert.pem \
  --tls-key /etc/ontodb/key.pem
```

## Monitoring

### Prometheus Metrics

OntoDB exposes metrics at `/metrics`:

```bash
curl http://localhost:7912/metrics
```

Key metrics:
- `ontodb_queries_total` — Total queries
- `ontodb_query_duration_seconds` — Query latency
- `ontodb_storage_entries` — Total stored entries
- `ontodb_cache_hit_rate` — Cache hit rate

### Health Check

```bash
curl http://localhost:7912/api/health
```

## Backup

```bash
# Create backup
curl -X POST http://localhost:7912/api/admin/backup \
  -H "Authorization: Bearer your-admin-key" \
  -d '{"path": "/backups/ontodb-2026-08-30"}'
```

## Tuning

### Write-Heavy Workloads

```bash
./ontodb-server --memtable-size 268435456  # 256MB
```

### Read-Heavy Workloads

```bash
./ontodb-server --memtable-size 67108864   # 64MB (more compaction)
```

### Rate Limiting

```bash
# Disable for internal services
./ontodb-server --no-rate-limit

# Custom limits
./ontodb-server --rate-limit-rpm 1000 --rate-limit-burst 50
```
