# Deployment

## Docker

The simplest way to run OntoDB:

```bash
docker run -d \
  --name ontodb \
  -p 7912:7912 \
  -p 7913:7913 \
  -v ontodb_data:/data \
  ontodb/ontodb:latest
```

With authentication:

```bash
docker run -d \
  --name ontodb \
  -p 7912:7912 \
  -v ontodb_data:/data \
  -v ./config:/config \
  ontodb/ontodb:latest \
  --http 0.0.0.0:7912 \
  --auth \
  --api-keys-file /config/api_keys.json
```

## Docker Compose

Full stack with monitoring:

```bash
cd deploy
docker-compose up -d
```

This starts:
- OntoDB server
- Nginx TLS proxy
- Prometheus metrics
- Grafana dashboards

## Kubernetes

Deployment manifests are in `deploy/kubernetes/`:

```bash
kubectl apply -f deploy/kubernetes/
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `SERVER_LISTEN` | `127.0.0.1:7913` | TCP listen address |
| `SERVER_HTTP` | — | HTTP listen address |
| `AUTH_ENABLED` | `false` | Enable API key auth |
| `AUTH_API_KEYS_FILE` | — | Path to API keys JSON |
| `RATE_LIMIT_RPM` | `60` | Requests per minute |
| `RATE_LIMIT_BURST` | `10` | Burst size |

## TLS

For production, place OntoDB behind a TLS proxy (Nginx, Caddy, or cloud LB):

```nginx
server {
    listen 443 ssl;
    server_name db.example.com;

    ssl_certificate /etc/ssl/certs/server.crt;
    ssl_certificate_key /etc/ssl/private/server.key;

    location / {
        proxy_pass http://127.0.0.1:7912;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

## Monitoring

### Prometheus

Scrape endpoint: `http://localhost:7912/metrics`

### Grafana

Pre-built dashboards in `deploy/monitoring/grafana-dashboard.json`.

### Health checks

```bash
# Liveness
curl http://localhost:7912/api/health/live

# Readiness
curl http://localhost:7912/api/health/ready
```

See `deploy/PRODUCTION.md` for the full production deployment guide.
