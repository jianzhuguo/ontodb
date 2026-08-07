# OntoDB Kubernetes Deployment Guide

## Prerequisites

- Kubernetes 1.19+
- kubectl configured
- (Optional) Prometheus Operator for metrics collection
- (Optional) Grafana for visualization

## Quick Start

### 1. Create Namespace

```bash
kubectl apply -f deploy/k8s/namespace.yaml
```

### 2. Create ConfigMap

```bash
kubectl apply -f deploy/k8s/configmap.yaml
```

### 3. Create PersistentVolumeClaim

```bash
kubectl apply -f deploy/k8s/pvc.yaml
```

### 4. Create Deployment

```bash
kubectl apply -f deploy/k8s/deployment.yaml
```

### 5. Create Service

```bash
kubectl apply -f deploy/k8s/service.yaml
```

### 6. (Optional) Enable Prometheus Monitoring

If using Prometheus Operator:

```bash
kubectl apply -f deploy/k8s/servicemonitor.yaml
```

If using standalone Prometheus:

```bash
# Copy prometheus.yaml to your Prometheus configuration
cp deploy/k8s/prometheus.yaml /etc/prometheus/conf.d/ontodb.yaml
```

### 7. (Optional) Enable Auto-scaling

```bash
kubectl apply -f deploy/k8s/hpa.yaml
```

## Accessing OntoDB

### From within the cluster

```bash
# TCP connection
ontodb-cli --host ontodb --port 7913

# HTTP API
curl http://ontodb:7912/api/health
```

### From outside the cluster

```bash
# Port forward
kubectl port-forward svc/ontodb 7912:7912 7913:7913 -n ontodb

# Then access locally
curl http://localhost:7912/api/health
```

## Monitoring

### Prometheus Metrics

OntoDB exposes metrics at `/metrics` in Prometheus format:

```bash
curl http://ontodb:7912/metrics
```

Key metrics:
- `ontodb_queries_total` - Total queries by type
- `ontodb_query_duration_seconds` - Query latency histogram
- `ontodb_http_connections_active` - Active HTTP connections
- `ontodb_tcp_connections_active` - Active TCP connections
- `ontodb_sstable_count` - Number of SSTables
- `ontodb_storage_entries` - Total storage entries

### Grafana Dashboard

Import `deploy/k8s/grafana-dashboard.json` into Grafana for pre-built visualizations.

## Configuration

All configuration is via environment variables in `configmap.yaml`:

| Variable | Default | Description |
|----------|---------|-------------|
| `STORAGE_DATA_DIR` | `/data/ontodb` | Data directory |
| `STORAGE_MEMTABLE_SIZE` | `4194304` | MemTable size (bytes) |
| `STORAGE_BLOCK_SIZE` | `4096` | Block size (bytes) |
| `STORAGE_NUM_LEVELS` | `7` | LSM-Tree levels |
| `STORAGE_COMPRESSION_LEVEL` | `3` | zstd compression (0-21) |
| `SERVER_LISTEN` | `0.0.0.0:7913` | TCP listen address |
| `SERVER_HTTP` | `0.0.0.0:7912` | HTTP listen address |
| `RATE_LIMIT_ENABLED` | `true` | Enable rate limiting |
| `RATE_LIMIT_RPM` | `60` | Requests per minute |
| `RATE_LIMIT_BURST` | `10` | Burst size |

## Backup

```bash
# Create backup job
kubectl create job ontodb-backup --from=cronjob/ontodb-backup -n ontodb

# Or use the backup API
curl -X POST http://ontodb:7912/api/backup -d '{"path": "/data/backup"}'
```

## Troubleshooting

### Pod not starting

```bash
kubectl logs -f deployment/ontodb -n ontodb
kubectl describe pod -l app.kubernetes.io/name=ontodb -n ontodb
```

### Health check failing

```bash
kubectl exec -it deployment/ontodb -n ontodb -- curl http://localhost:7912/api/health
```

### Storage issues

```bash
kubectl exec -it deployment/ontodb -n ontodb -- ls -la /data/ontodb
```
