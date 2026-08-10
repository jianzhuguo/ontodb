# OntoDB 部署指南

> 生产环境部署最佳实践

---

## 1. 单机部署

### 1.1 硬件推荐

| 场景 | CPU | 内存 | 磁盘 | 说明 |
|------|-----|------|------|------|
| 开发/测试 | 2 核 | 4 GB | 50 GB SSD | 个人开发 |
| 小型生产 | 4 核 | 16 GB | 200 GB NVMe | 10 万级数据 |
| 中型生产 | 8 核 | 32 GB | 500 GB NVMe | 百万级数据 |
| 大型生产 | 16+ 核 | 64 GB | 1+ TB NVMe | 千万级数据 |

### 1.2 系统配置

```bash
# 增加文件描述符限制
echo "* soft nofile 65536" >> /etc/security/limits.conf
echo "* hard nofile 65536" >> /etc/security/limits.conf

# 增加虚拟内存
echo "vm.max_map_count=262144" >> /etc/sysctl.conf
sysctl -p

# 禁用 THP (Transparent Huge Pages)
echo never > /sys/kernel/mm/transparent_hugepage/enabled
```

### 1.3 systemd 服务

```ini
# /etc/systemd/system/ontodb.service
[Unit]
Description=OntoDB Semantic Database
After=network.target

[Service]
Type=simple
User=ontodb
Group=ontodb
ExecStart=/opt/ontodb/bin/ontodb-server \
    --data-dir /var/lib/ontodb \
    --http 0.0.0.0:7912 \
    --auth \
    --api-key-file /etc/ontodb/api-key
Restart=on-failure
RestartSec=5
LimitNOFILE=65536
LimitNPROC=4096

# Security
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/ontodb

[Install]
WantedBy=multi-user.target
```

```bash
# 启动服务
sudo systemctl daemon-reload
sudo systemctl enable ontodb
sudo systemctl start ontodb

# 查看状态
sudo systemctl status ontodb
journalctl -u ontodb -f
```

---

## 2. Docker 部署

### 2.1 单节点

```bash
docker run -d \
    --name ontodb \
    --restart unless-stopped \
    -p 7912:7912 \
    -p 7913:7913 \
    -v ontodb-data:/data \
    -e ONTODB_HTTP=0.0.0.0:7912 \
    ontodb/ontodb:latest
```

### 2.2 Docker Compose (生产配置)

```yaml
version: '3.8'

services:
  ontodb:
    image: ontodb/ontodb:latest
    container_name: ontodb
    restart: unless-stopped
    ports:
      - "7912:7912"
      - "7913:7913"
    volumes:
      - ontodb-data:/data
      - ./ontodb.toml:/etc/ontodb/ontodb.toml:ro
    environment:
      - ONTODB_CONFIG=/etc/ontodb/ontodb.toml
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:7912/api/health"]
      interval: 30s
      timeout: 3s
      retries: 3
      start_period: 10s
    deploy:
      resources:
        limits:
          cpus: '4'
          memory: 8G
        reservations:
          cpus: '2'
          memory: 4G

  prometheus:
    image: prom/prometheus:latest
    ports:
      - "9090:9090"
    volumes:
      - ./monitoring/prometheus.yml:/etc/prometheus/prometheus.yml
    depends_on:
      - ontodb

  grafana:
    image: grafana/grafana:latest
    ports:
      - "3000:3000"
    volumes:
      - ./monitoring/grafana:/etc/grafana/provisioning/dashboards
    depends_on:
      - prometheus

volumes:
  ontodb-data:
```

---

## 3. 集群部署

### 3.1 三节点集群

```bash
# 节点 1
./ontodb-server --data-dir /data --http 0.0.0.0:7912 \
    --raft --raft-id 1 --raft-peers "2@node2:7913,3@node3:7913"

# 节点 2
./ontodb-server --data-dir /data --http 0.0.0.0:7912 \
    --raft --raft-id 2 --raft-peers "1@node1:7913,3@node3:7913"

# 节点 3
./ontodb-server --data-dir /data --http 0.0.0.0:7912 \
    --raft --raft-id 3 --raft-peers "1@node1:7913,2@node2:7913"
```

### 3.2 负载均衡 (Nginx)

```nginx
upstream ontodb_cluster {
    least_conn;
    server node1:7912 weight=1;
    server node2:7912 weight=1;
    server node3:7912 weight=1;
}

server {
    listen 80;
    server_name ontodb.example.com;

    location / {
        proxy_pass http://ontodb_cluster;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_read_timeout 300s;
    }
}
```

---

## 4. 备份策略

### 4.1 自动备份脚本

```bash
#!/bin/bash
# /opt/ontodb/scripts/backup.sh

BACKUP_DIR="/var/backups/ontodb"
DATE=$(date +%Y%m%d_%H%M%S)
RETENTION_DAYS=30

# 创建备份
curl -X POST http://localhost:7912/api/backup \
    -H "Content-Type: application/json" \
    -d "{\"path\": \"$BACKUP_DIR/backup_$DATE.ontodb\"}"

# 清理旧备份
find $BACKUP_DIR -name "*.ontodb" -mtime +$RETENTION_DAYS -delete

# 记录日志
echo "[$(date)] Backup completed: backup_$DATE.ontodb" >> /var/log/ontodb/backup.log
```

```bash
# 每日凌晨 2 点执行
echo "0 2 * * * /opt/ontodb/scripts/backup.sh" | crontab -
```

### 4.2 备份验证

```bash
# 定期验证备份完整性
curl -X POST http://localhost:7912/api/backup/verify \
    -H "Content-Type: application/json" \
    -d '{"path": "/var/backups/ontodb/backup_20260810.ontodb"}'
```

---

## 5. 监控告警

### 5.1 Prometheus 配置

```yaml
# monitoring/prometheus.yml
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: 'ontodb'
    static_configs:
      - targets: ['ontodb:7912']
    metrics_path: '/metrics'
```

### 5.2 告警规则

```yaml
# monitoring/alerts.yml
groups:
  - name: ontodb
    rules:
      - alert: OntoDBDown
        expr: up{job="ontodb"} == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "OntoDB instance is down"

      - alert: HighQueryLatency
        expr: ontodb_query_latency_p99_ms > 1000
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High query latency detected"

      - alert: HighErrorRate
        expr: rate(ontodb_query_errors[5m]) > 0.1
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "High error rate detected"

      - alert: DiskSpaceLow
        expr: ontodb_disk_usage_bytes / 1073741824 > 80
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Disk usage above 80GB"
```

---

## 6. 安全加固

### 6.1 启用 TLS

```bash
# 生成自签名证书（开发环境）
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -days 365 -nodes

# 启动时启用 TLS
./ontodb-server --tls-cert cert.pem --tls-key key.pem
```

### 6.2 启用认证

```bash
# 生成 API Key
openssl rand -hex 32

# 启动时启用认证
./ontodb-server --auth --api-key "your-generated-key"
```

### 6.3 防火墙配置

```bash
# 只允许内网访问
ufw allow from 10.0.0.0/8 to any port 7912
ufw allow from 172.16.0.0/12 to any port 7912
ufw allow from 192.168.0.0/16 to any port 7912
ufw deny 7912
```

---

## 7. 性能调优

### 7.1 内存配置

```toml
# ontodb.toml
[performance]
memtable_size_mb = 128      # 增大可减少 flush 频率
block_cache_mb = 512        # 增大可提升读取性能
compaction_threads = 4      # 后台压缩线程数
```

### 7.2 磁盘配置

```bash
# 使用 XFS 文件系统
mkfs.xfs /dev/sdb
mount -o noatime,nodiratime /dev/sdb /var/lib/ontodb

# 使用 I/O 调度器
echo noop > /sys/block/sdb/queue/scheduler
```

---

## 8. 故障排查

### 8.1 常见问题

| 问题 | 原因 | 解决方案 |
|------|------|---------|
| 启动失败 | 数据目录权限不足 | `chown -R ontodb:ontodb /var/lib/ontodb` |
| 连接被拒 | 防火墙阻止 | 检查 `ufw status` |
| 查询超时 | 数据量太大 | 添加 LIMIT，优化 WHERE |
| 内存不足 | Cache 太大 | 减小 `block_cache_mb` |
| 磁盘满 | WAL 累积 | 清理旧数据或扩容 |

### 8.2 日志查看

```bash
# 实时日志
journalctl -u ontodb -f

# 错误日志
journalctl -u ontodb -p err

# 最近日志
journalctl -u ontodb --since "1 hour ago"
```

### 8.3 健康检查

```bash
# 完整健康检查
curl http://localhost:7912/api/health | jq

# 指标检查
curl http://localhost:7912/api/metrics | jq
```
