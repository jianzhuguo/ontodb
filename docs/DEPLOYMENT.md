# OntoDB Deployment Guide

## Quick Start

### Prerequisites

- **OS**: Linux (x86_64 or aarch64), macOS (Apple Silicon), Windows (x86_64)
- **Disk**: SSD recommended for production workloads
- **Memory**: 512MB minimum, 4GB+ recommended

### Option 1: Docker (Recommended)

```bash
# Pull and run
docker run -d \
  --name ontodb \
  -p 7912:7912 \
  -p 7913:7913 \
  -v ontodb-data:/data \
  ontodb:latest

# Verify
curl http://localhost:7912/api/health
```

### Option 2: Docker Compose

```yaml
# docker-compose.yml
services:
  ontodb:
    build: .
    ports:
      - "7912:7912"
      - "7913:7913"
    volumes:
      - ontodb-data:/data
    restart: unless-stopped

volumes:
  ontodb-data:
```

```bash
docker compose up -d
```

### Option 3: Build from Source

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/ontodb/ontodb.git
cd ontodb
cargo build --release

# Binaries are at:
#   target/release/ontodb-server
#   target/release/ontodb-cli
```

---

## Server Configuration

### Command-Line Options

| Flag | Default | Description |
|------|---------|-------------|
| `--data-dir <path>` | `./ontodb_data` | Directory for all persistent data |
| `--memtable-size <bytes>` | `4194304` (4MB) | MemTable size before flush to SSTable |
| `--listen <addr:port>` | `127.0.0.1:7913` | TCP listen address for CLI connections |
| `--http <addr:port>` | _(disabled)_ | HTTP API listen address (enables REST API) |
| `--auth` | `false` | Enable API key authentication |
| `--api-keys-file <path>` | _(none)_ | Path to API keys JSON file |
| `--rate-limit <rpm>` | `60` | Default requests per minute per key |
| `--burst-size <n>` | `10` | Rate limit burst size |
| `--no-rate-limit` | `false` | Disable rate limiting entirely |
| `-i` / `--interactive` | `false` | Run in interactive REPL mode |

### Example: Production HTTP Server

```bash
ontodb-server \
  --data-dir /var/lib/ontodb \
  --http 0.0.0.0:7912 \
  --listen 127.0.0.1:7913 \
  --auth \
  --api-keys-file /etc/ontodb/api_keys.json \
  --rate-limit 300 \
  --burst-size 50
```

### Example: Development Server

```bash
# No auth, no rate limit, local access only
ontodb-server \
  --data-dir ./dev_data \
  --http 127.0.0.1:7912 \
  --no-rate-limit
```

---

## Authentication

### API Key Configuration

Create a JSON file at `/etc/ontodb/api_keys.json`:

```json
{
  "enabled": true,
  "keys": [
    {
      "key": "your-admin-secret-key-here",
      "description": "Admin key with full access",
      "permission": "Admin",
      "rate_limit": 120
    },
    {
      "key": "readonly-dashboard-key",
      "description": "Read-only access for monitoring dashboards",
      "permission": "ReadOnly",
      "rate_limit": 60
    },
    {
      "key": "app-readwrite-key",
      "description": "Application read-write access",
      "permission": "ReadWrite",
      "rate_limit": 120
    }
  ],
  "default_permission": "ReadOnly"
}
```

### Permission Levels

| Level | Allowed Operations |
|-------|-------------------|
| `ReadOnly` | SELECT, EXPLAIN, ANALYZE, MATCH, VECTOR SEARCH, schema introspection |
| `ReadWrite` | All ReadOnly + INSERT, UPDATE, DELETE, UPSERT, IMPORT |
| `Admin` | All ReadWrite + CREATE/DROP ONTOLOGY, CREATE/DROP INDEX, BEGIN/COMMIT/ROLLBACK |

### Passing API Keys

Three methods (pick one per request):

```bash
# 1. Authorization header
curl -H "Authorization: Bearer your-admin-secret-key-here" \
  http://localhost:7912/api/query \
  -d '{"query": "SELECT * FROM Product"}'

# 2. X-API-Key header
curl -H "X-API-Key: your-admin-secret-key-here" \
  http://localhost:7912/api/query \
  -d '{"query": "SELECT * FROM Product"}'

```

---

## TLS / HTTPS

OntoDB does not terminate TLS itself. Use a reverse proxy:

### nginx

```nginx
server {
    listen 443 ssl;
    server_name ontodb.example.com;

    ssl_certificate /etc/ssl/certs/ontodb.pem;
    ssl_certificate_key /etc/ssl/private/ontodb.key;

    location / {
        proxy_pass http://127.0.0.1:7912;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket support (if needed in future)
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";

        # Timeouts for long queries
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
```

### Caddy

```
ontodb.example.com {
    reverse_proxy localhost:7912
}
```

---

## Kubernetes Deployment

### Deployment + Service

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: ontodb
spec:
  replicas: 1
  selector:
    matchLabels:
      app: ontodb
  template:
    metadata:
      labels:
        app: ontodb
    spec:
      containers:
        - name: ontodb
          image: ontodb:latest
          args:
            - "--data-dir"
            - "/data"
            - "--http"
            - "0.0.0.0:7912"
            - "--listen"
            - "0.0.0.0:7913"
            - "--auth"
            - "--api-keys-file"
            - "/etc/ontodb/api_keys.json"
          ports:
            - containerPort: 7912
              name: http
            - containerPort: 7913
              name: tcp
          livenessProbe:
            httpGet:
              path: /api/health/live
              port: 7912
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /api/health/ready
              port: 7912
            initialDelaySeconds: 5
            periodSeconds: 10
          volumeMounts:
            - name: data
              mountPath: /data
            - name: config
              mountPath: /etc/ontodb
              readOnly: true
          resources:
            requests:
              memory: "256Mi"
              cpu: "100m"
            limits:
              memory: "2Gi"
              cpu: "1000m"
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: ontodb-data
        - name: config
          secret:
            secretName: ontodb-api-keys
---
apiVersion: v1
kind: Service
metadata:
  name: ontodb
spec:
  selector:
    app: ontodb
  ports:
    - name: http
      port: 7912
      targetPort: 7912
    - name: tcp
      port: 7913
      targetPort: 7913
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: ontodb-data
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 10Gi
```

### Create the API keys secret

```bash
kubectl create secret generic ontodb-api-keys \
  --from-file=api_keys.json=/path/to/your/api_keys.json
```

---

## Monitoring

### Prometheus Metrics

OntoDB exposes Prometheus metrics at `/metrics`:

```yaml
# prometheus.yml scrape config
scrape_configs:
  - job_name: 'ontodb'
    static_configs:
      - targets: ['ontodb:7912']
    metrics_path: /metrics
```

### Key Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `ontodb_queries_total` | Counter | Total queries by type (SELECT/INSERT/UPDATE/DELETE) |
| `ontodb_query_duration_seconds` | Histogram | Query latency distribution |
| `ontodb_tcp_connections_active` | Gauge | Active TCP connections |
| `ontodb_auth_attempts_total` | Counter | Authentication attempts (success/failure) |
| `ontodb_rate_limited_total` | Counter | Rate-limited requests |
| `ontodb_sstables_total` | Gauge | Number of SSTables |
| `ontodb_storage_entries_total` | Gauge | Total stored entries |
| `ontodb_compactions_total` | Counter | Compaction operations |

### Health Check Endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /api/health` | Full health check (storage + query engine status) |
| `GET /api/health/ready` | Kubernetes readiness probe |
| `GET /api/health/live` | Kubernetes liveness probe |

---

## Backup and Restore

### Backup

```bash
# Via CLI (interactive)
ontodb-cli -q "BACKUP TO '/backup/ontodb-$(date +%Y%m%d)'"

# Or copy data directory while server is running (after flush)
curl -X POST http://localhost:7912/api/query \
  -d '{"query": "FLUSH"}'
cp -r /var/lib/ontodb /backup/ontodb-$(date +%Y%m%d)
```

### Restore

```bash
# Stop the server, replace data directory, restart
systemctl stop ontodb
rm -rf /var/lib/ontodb/*
cp -r /backup/ontodb-20260807/* /var/lib/ontodb/
systemctl start ontodb
```

---

## Systemd Service

```ini
# /etc/systemd/system/ontodb.service
[Unit]
Description=OntoDB Semantic Database
After=network.target

[Service]
Type=simple
User=ontodb
Group=ontodb
ExecStart=/usr/local/bin/ontodb-server \
  --data-dir /var/lib/ontodb \
  --http 0.0.0.0:7912 \
  --listen 127.0.0.1:7913 \
  --auth \
  --api-keys-file /etc/ontodb/api_keys.json
Restart=on-failure
RestartSec=5
LimitNOFILE=65536

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/ontodb

[Install]
WantedBy=multi-user.target
```

```bash
# Setup
sudo useradd -r -s /bin/false ontodb
sudo mkdir -p /var/lib/ontodb /etc/ontodb
sudo chown ontodb:ontodb /var/lib/ontodb
sudo cp target/release/ontodb-server /usr/local/bin/
sudo cp config/api_keys.example.json /etc/ontodb/api_keys.json

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable ontodb
sudo systemctl start ontodb
sudo systemctl status ontodb
```

---

## Performance Tuning

### MemTable Size

Larger MemTables reduce flush frequency but increase memory usage:

```bash
# 16MB MemTable (default 4MB)
ontodb-server --memtable-size 16777216
```

### SSD vs HDD

OntoDB uses an LSM-Tree storage engine. SSDs are strongly recommended for production. HDDs will work but with significantly higher write amplification and read latency.

### File Descriptors

For high-concurrency workloads, increase the file descriptor limit:

```bash
# Temporary
ulimit -n 65536

# Permanent (add to /etc/security/limits.conf)
ontodb soft nofile 65536
ontodb hard nofile 65536
```

---

## Troubleshooting

### Server won't start: "address already in use"

Another process is using the port. Find and stop it:

```bash
# Linux
lsof -i :7912
kill <pid>

# Windows
netstat -ano | findstr :7912
taskkill /PID <pid> /F
```

### Data directory permissions

```bash
# Ensure the ontodb user owns the data directory
sudo chown -R ontodb:ontodb /var/lib/ontodb
```

### Slow queries

```bash
# Use EXPLAIN to see the execution plan
curl http://localhost:7912/api/query \
  -d '{"query": "EXPLAIN SELECT * FROM Product WHERE price > 100"}'

# Check Prometheus metrics for query latency
curl http://localhost:7912/metrics | grep query_duration
```

### Checking server logs

```bash
# Docker
docker logs ontodb

# Systemd
journalctl -u ontodb -f
```
