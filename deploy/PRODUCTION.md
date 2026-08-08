# OntoDB Production Deployment Guide

## Prerequisites

- Docker 24+ and Docker Compose v2
- 2 CPU cores, 4GB RAM minimum (8GB+ recommended)
- SSD storage for data volume
- TLS certificates (or generate self-signed for testing)

## Quick Start

```bash
# 1. Clone the repository
git clone https://github.com/ontodb/ontodb.git
cd ontodb

# 2. Create environment config
cp .env.example .env
# Edit .env to set ports, resource limits, passwords, etc.

# 3. Generate API keys (IMPORTANT: change the default keys!)
cp config/api_keys.example.json config/api_keys.json
# Edit config/api_keys.json — replace CHANGE_ME_* with real secrets

# 4. Generate TLS certificates (self-signed for testing)
mkdir -p deploy/nginx/certs
openssl req -x509 -nodes -days 365 -newkey rsa:2048 \
  -keyout deploy/nginx/certs/server.key \
  -out deploy/nginx/certs/server.crt \
  -subj "/CN=ontodb.local"

# 5. Start all services
docker compose up -d

# 6. Verify
curl -k https://localhost/api/health
```

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Client                                                 │
└──────┬──────────────────────────────────────────────────┘
       │ :443 (HTTPS)
┌──────▼──────────────────────────────────────────────────┐
│  Nginx (TLS termination, rate limiting)                 │
└──────┬──────────────────────────────────────────────────┘
       │ :7912 (HTTP, internal)
┌──────▼──────────────────────────────────────────────────┐
│  OntoDB Server                                          │
│  - SQL / SPARQL / MATCH / Vector Search                 │
│  - Ontology reasoning (OWL-lite)                        │
│  - LSM-Tree storage engine                              │
└──────┬──────────────┬───────────────────────────────────┘
       │              │
  /data/ontodb    /data/backup
  (persistent)    (scheduled)
```

Additional services:
- **Prometheus** (:9090) — metrics collection
- **Grafana** (:3000) — monitoring dashboards
- **Backup** — one-shot container, schedule via host cron

## Services

| Service | Port | Description |
|---------|------|-------------|
| ontodb | 7912, 7913 | Database (HTTP API + TCP CLI) |
| nginx | 80, 443 | TLS reverse proxy |
| prometheus | 9090 | Metrics collection |
| grafana | 3000 | Dashboards (admin / configured password) |
| backup | — | One-shot backup job |

## Configuration

### Environment Variables (.env)

| Variable | Default | Description |
|----------|---------|-------------|
| `ONTODB_HTTP_PORT` | 7912 | HTTP API port on host |
| `ONTODB_TCP_PORT` | 7913 | TCP CLI port on host |
| `ONTODB_MEMORY_LIMIT` | 2g | Container memory limit |
| `ONTODB_CPU_LIMIT` | 2.0 | Container CPU limit |
| `ONTODB_RATE_LIMIT` | 300 | Requests per minute per API key |
| `ONTODB_BURST_SIZE` | 50 | Rate limit burst size |
| `HTTPS_PORT` | 443 | HTTPS port on host |
| `GRAFANA_PORT` | 3000 | Grafana port on host |
| `GRAFANA_ADMIN_PASSWORD` | admin | Grafana admin password |
| `BACKUP_RETENTION_DAYS` | 7 | Days to keep backups |

### API Keys (config/api_keys.json)

Three permission levels:

| Level | Allowed Operations |
|-------|-------------------|
| `ReadOnly` | SELECT, EXPLAIN, ANALYZE, MATCH, VECTOR SEARCH |
| `ReadWrite` | All ReadOnly + INSERT, UPDATE, DELETE, UPSERT, IMPORT |
| `Admin` | All ReadWrite + CREATE/DROP ONTOLOGY, INDEX, BACKUP, FLUSH |

Pass keys via:
- `Authorization: Bearer <key>` header
- `X-API-Key: <key>` header
- `?api_key=<key>` query parameter

## Backup & Restore

### Automatic Backups

The `backup` service runs as a one-shot container. Schedule it with host cron:

```bash
# Edit crontab
crontab -e

# Add: run backup daily at 03:00
0 3 * * * cd /path/to/ontodb && docker compose run --rm backup
```

### Manual Backup

```bash
# Via API
curl -X POST https://localhost/api/backup \
  -H "Authorization: Bearer YOUR_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"path": "/data/backup/manual_$(date +%Y%m%d)"}'

# Flush first for consistency
curl -X POST https://localhost/api/flush \
  -H "Authorization: Bearer YOUR_ADMIN_KEY"
```

### Restore

```bash
# 1. Stop the database
docker compose stop ontodb

# 2. Restore from backup (runs restore then exits)
docker compose run --rm ontodb ontodb-server \
  --data-dir /data/ontodb \
  --http 0.0.0.0:7912 \
  --listen 0.0.0.0:7913 \
  --restore-from /data/backup/20260808_030000

# 3. Restart
docker compose up -d ontodb
```

Or use the SQL command via CLI:

```bash
docker compose exec ontodb ontodb-cli \
  -q "RESTORE FROM '/data/backup/20260808_030000'"
```

## Monitoring

### Grafana

1. Open `http://localhost:3000` (or your configured port)
2. Login with `admin` / your configured password
3. The OntoDB dashboard is auto-provisioned

### Prometheus

Query metrics directly:

```bash
curl http://localhost:9090/api/v1/query?query=ontodb_queries_total
```

### Key Metrics

| Metric | Description |
|--------|-------------|
| `ontodb_queries_total` | Total queries by type |
| `ontodb_query_duration_seconds` | Query latency histogram |
| `ontodb_http_connections_active` | Active HTTP connections |
| `ontodb_tcp_connections_active` | Active TCP connections |
| `ontodb_sstable_count` | Number of SSTables |
| `ontodb_storage_entries` | Total stored entries |
| `ontodb_compactions_total` | Compaction operations |
| `ontodb_auth_attempts_total` | Auth attempts |
| `ontodb_rate_limited_total` | Rate-limited requests |

## TLS Certificates

### Production (Let's Encrypt)

```bash
# Install certbot
apt install certbot

# Obtain certificate
certbot certonly --standalone -d ontodb.yourdomain.com

# Copy to nginx certs directory
cp /etc/letsencrypt/live/ontodb.yourdomain.com/fullchain.pem deploy/nginx/certs/server.crt
cp /etc/letsencrypt/live/ontodb.yourdomain.com/privkey.pem deploy/nginx/certs/server.key

# Reload nginx
docker compose restart nginx
```

### Self-Signed (Testing Only)

```bash
openssl req -x509 -nodes -days 365 -newkey rsa:2048 \
  -keyout deploy/nginx/certs/server.key \
  -out deploy/nginx/certs/server.crt \
  -subj "/CN=ontodb.local"
```

## Performance Tuning

### Memory

Set `ONTODB_MEMORY_LIMIT` based on your data size:
- Small (<1GB data): 1-2g
- Medium (1-10GB data): 4-8g
- Large (>10GB data): 16g+

### SSD

OntoDB uses LSM-Tree storage. SSDs are strongly recommended. HDDs will work but with significantly higher write amplification.

### File Descriptors

For high-concurrency workloads, increase the host's file descriptor limit:

```bash
# /etc/security/limits.conf
ontodb soft nofile 65536
ontodb hard nofile 65536
```

### MemTable Size

Increase for write-heavy workloads (default 4MB):

```bash
# In docker-compose.yml, add to ontodb command:
- "--memtable-size=16777216"  # 16MB
```

## Troubleshooting

### Container won't start

```bash
docker compose logs ontodb
```

### Health check failing

```bash
docker compose exec ontodb curl http://localhost:7912/api/health
```

### Slow queries

```bash
# Via API
curl -X POST https://localhost/api/query \
  -H "Authorization: Bearer YOUR_KEY" \
  -d '{"query": "EXPLAIN SELECT * FROM Product WHERE price > 100"}'
```

### Backup verification

```bash
# Check backup contents
docker compose exec ontodb ls -la /data/backup/

# Verify manifest
docker compose exec ontodb cat /data/backup/20260808_030000/manifest.json
```
