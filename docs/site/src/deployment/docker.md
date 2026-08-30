# Docker Deployment

## Quick Start

```bash
docker run -d \
  --name ontodb \
  -p 7912:7912 \
  -v ontodb_data:/data \
  ontodb/ontodb:latest \
  --data-dir /data --http 0.0.0.0:7912
```

## Docker Compose

```yaml
version: '3.8'
services:
  ontodb:
    image: ontodb/ontodb:latest
    ports:
      - "7912:7912"
    volumes:
      - ontodb_data:/data
    command: --data-dir /data --http 0.0.0.0:7912 --no-rate-limit
    restart: unless-stopped

volumes:
  ontodb_data:
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `STORAGE_DATA_DIR` | Data directory | `./ontodb_data` |
| `SERVER_HTTP` | HTTP listen address | `127.0.0.1:7912` |
| `AUTH_ENABLED` | Enable API key auth | `false` |
| `AUTH_API_KEYS_FILE` | API keys JSON file | - |
| `RATE_LIMIT_DISABLED` | Disable rate limiting | `false` |

## Building from Dockerfile

```bash
git clone https://github.com/ontodb/ontodb.git
cd ontodb
docker build -t ontodb .
docker run -p 7912:7912 ontodb
```

## Data Persistence

OntoDB stores all data in the `--data-dir` directory. Mount a volume to persist data:

```bash
docker run -d -v /host/data:/data ontodb/ontodb:latest --data-dir /data --http 0.0.0.0:7912
```

## Health Check

```bash
curl http://localhost:7912/api/health
```
