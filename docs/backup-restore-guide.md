# OntoDB 备份恢复操作手册

> **版本**：v1.0 | **日期**：2026-08-29

---

## 一、备份方式概览

| 方式 | 适用场景 | 停机要求 | 数据一致性 |
|------|---------|---------|-----------|
| 文件复制（冷备份） | 开发测试 | 需要停机 | 完全一致 |
| 文件复制（热备份） | 生产环境 | 不停机 | 可能丢失最后几秒 |
| CLI dump（逻辑备份） | 跨版本迁移 | 不停机 | 完全一致 |
| WAL 归档 | 增量备份 | 不停机 | 完全一致 |

---

## 二、冷备份（停机备份）

最简单的备份方式，适合开发测试环境。

```bash
# 1. 停止 OntoDB
sudo systemctl stop ontodb

# 2. 备份数据目录
sudo tar czf /backup/ontodb-$(date +%Y%m%d).tar.gz -C /var/lib ontodb

# 3. 启动 OntoDB
sudo systemctl start ontodb

# 4. 验证备份
tar tzf /backup/ontodb-$(date +%Y%m%d).tar.gz | head -20
```

---

## 三、热备份（不停机）

适合生产环境，使用 API flush + 文件复制。

```bash
# 1. 触发 MemTable flush（确保数据落盘）
curl -X POST http://localhost:7912/api/flush

# 2. 等待 flush 完成
sleep 2

# 3. 复制数据目录
cp -r /var/lib/ontodb /backup/ontodb-$(date +%Y%m%d)

# 4. 验证备份
ls -lh /backup/ontodb-$(date +%Y%m%d)/
```

**注意事项**：
- flush 后到复制完成期间的新写入可能不在备份中
- 对于大多数场景，这个窗口（几秒）可以接受
- 如果需要严格一致性，使用 CLI dump

---

## 四、CLI 逻辑备份（推荐）

最灵活的备份方式，支持跨版本迁移。

### 4.1 全库导出

```bash
# JSONL 格式（推荐）
ontodb-cli dump -o /backup/ontodb-full.jsonl

# CSV 格式
ontodb-cli dump -o /backup/ontodb-full.csv -f csv
```

### 4.2 单表导出

```bash
# 导出 Product 表
ontodb-cli dump -c Product -o /backup/product.jsonl

# 导出指定表为 CSV
ontodb-cli dump -c BioTask -o /backup/biotask.csv -f csv
```

### 4.3 恢复

```bash
# 恢复全库
ontodb-cli restore -i /backup/ontodb-full.jsonl

# 恢复单表
ontodb-cli restore -i /backup/product.jsonl -c Product

# 跳过错误继续恢复
ontodb-cli restore -i /backup/ontodb-full.jsonl --skip-errors
```

### 4.4 远程备份

```bash
# 从远程服务器导出
ontodb-cli -a 192.168.1.100:7913 dump -o /backup/remote.jsonl

# 恢复到远程服务器
ontodb-cli -a 192.168.1.100:7913 restore -i /backup/remote.jsonl
```

---

## 五、WAL 归档（增量备份）

适合需要增量备份的场景。

### 5.1 启用 WAL 归档

```bash
# 启动时指定归档目录
ontodb-server --data-dir /var/lib/ontodb --wal-archive-dir /backup/wal-archive

# 或通过环境变量
export WAL_ARCHIVE_DIR=/backup/wal-archive
ontodb-server --data-dir /var/lib/ontodb
```

### 5.2 归档文件管理

```bash
# 查看归档文件
ls -lh /backup/wal-archive/

# 归档文件命名格式
# wal_{timestamp}_{sequence}.log
# 示例: wal_1724836800_00000042.log
```

### 5.3 配置项

| 配置 | 默认值 | 说明 |
|------|--------|------|
| `wal_archive_dir` | 无（禁用） | 归档目录路径 |
| `wal_archive_max_files` | 100 | 最大归档文件数，超出自动清理 |

---

## 六、自动备份脚本

### 6.1 使用提供的脚本

```bash
# 复制脚本
cp scripts/backup.sh /opt/ontodb/
chmod +x /opt/ontodb/backup.sh

# 手动执行
/opt/ontodb/backup.sh /var/lib/ontodb /backup/ontodb 7

# 配置 crontab（每天凌晨 3 点）
crontab -e
# 添加: 0 3 * * * /opt/ontodb/backup.sh /var/lib/ontodb /backup/ontodb 7
```

### 6.2 脚本功能

- 自动检测 OntoDB 是否运行（在线/离线模式）
- 在线模式自动触发 flush
- 支持配置保留天数，自动清理旧备份
- 生成备份元信息（backup-info.json）

---

## 七、恢复操作

### 7.1 从文件备份恢复

```bash
# 1. 停止 OntoDB
sudo systemctl stop ontodb

# 2. 清空数据目录
sudo rm -rf /var/lib/ontodb/*

# 3. 解压备份
sudo tar xzf /backup/ontodb-20260829.tar.gz -C /var/lib

# 4. 修复权限
sudo chown -R ontodb:ontodb /var/lib/ontodb

# 5. 启动 OntoDB
sudo systemctl start ontodb

# 6. 验证
curl http://localhost:7912/api/health
```

### 7.2 从逻辑备份恢复

```bash
# 1. 确保 OntoDB 正在运行
curl http://localhost:7912/api/health

# 2. 恢复数据
ontodb-cli restore -i /backup/ontodb-full.jsonl

# 3. 验证数据
ontodb-cli -q "SELECT * FROM Product LIMIT 5"
```

### 7.3 从 WAL 归档恢复

```bash
# 1. 停止 OntoDB
sudo systemctl stop ontodb

# 2. 恢复数据目录快照（基础）
sudo cp -r /backup/ontodb-snapshot/* /var/lib/ontodb/

# 3. 重放 WAL 归档（按时间顺序）
for wal in $(ls /backup/wal-archive/wal_*.log | sort); do
    cp "$wal" /var/lib/ontodb/wal.log
done

# 4. 启动 OntoDB（自动重放 WAL）
sudo systemctl start ontodb
```

---

## 八、备份验证

```bash
# 使用 API 验证备份
curl -X POST http://localhost:7912/api/backup/verify \
  -H "Content-Type: application/json" \
  -d '{"path": "/backup/ontodb-20260829"}'
```

---

## 九、最佳实践

| 场景 | 推荐方案 | 频率 |
|------|---------|------|
| 开发测试 | 冷备份 | 每天 |
| 生产环境 | CLI dump + WAL 归档 | 每天全量 + 持续增量 |
| 跨版本迁移 | CLI dump | 升级前 |
| 灾难恢复 | 文件备份 + WAL 归档 | 每周全量 + 持续增量 |

---

**© 2026 原点价值 / OntoValue Technology**
