#!/bin/bash
# OntoDB 自动备份脚本
# 用法: ./backup.sh [数据目录] [备份目录] [保留天数]
#
# 示例:
#   ./backup.sh /var/lib/ontodb /backup/ontodb 7
#   ./backup.sh                          # 使用默认值
#
# 配合 crontab 使用:
#   0 3 * * * /opt/ontodb/scripts/backup.sh /var/lib/ontodb /backup/ontodb 7

set -euo pipefail

# 默认配置
DATA_DIR="${1:-/var/lib/ontodb}"
BACKUP_DIR="${2:-/backup/ontodb}"
RETENTION_DAYS="${3:-7}"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
BACKUP_NAME="ontodb-backup-${TIMESTAMP}"

# 日志函数
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"
}

error() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: $*" >&2
    exit 1
}

# 检查数据目录
if [ ! -d "${DATA_DIR}" ]; then
    error "Data directory not found: ${DATA_DIR}"
fi

# 创建备份目录
mkdir -p "${BACKUP_DIR}" || error "Failed to create backup directory: ${BACKUP_DIR}"

# 检查是否有正在运行的 OntoDB 进程
ONTO_PID=$(pgrep -f "ontodb-server" || true)
if [ -n "${ONTO_PID}" ]; then
    log "OntoDB server is running (PID: ${ONTO_PID}), using file copy backup"
    USE_FLUSH=true
else
    log "OntoDB server is not running, using direct file copy"
    USE_FLUSH=false
fi

# 如果服务器在运行，先 flush MemTable 到 SSTable
if [ "${USE_FLUSH}" = true ]; then
    log "Flushing MemTable to SSTable..."
    curl -s -X POST http://localhost:7912/api/flush > /dev/null 2>&1 || true
    sleep 2
fi

# 创建备份
log "Starting backup: ${BACKUP_NAME}"
BACKUP_PATH="${BACKUP_DIR}/${BACKUP_NAME}"
mkdir -p "${BACKUP_PATH}"

# 复制数据文件
log "Copying data files from ${DATA_DIR}..."
cp -r "${DATA_DIR}/"* "${BACKUP_PATH}/" 2>/dev/null || true

# 复制 WAL 文件（如果存在）
if [ -f "${DATA_DIR}/wal.log" ]; then
    cp "${DATA_DIR}/wal.log" "${BACKUP_PATH}/"
    log "WAL file copied"
fi

# 复制 SSTable 文件
SST_COUNT=$(find "${DATA_DIR}" -name "*.sst" 2>/dev/null | wc -l)
if [ "${SST_COUNT}" -gt 0 ]; then
    cp "${DATA_DIR}"/*.sst "${BACKUP_PATH}/" 2>/dev/null || true
    log "Copied ${SST_COUNT} SSTable files"
fi

# 复制索引文件
if [ -d "${DATA_DIR}/indexes" ]; then
    cp -r "${DATA_DIR}/indexes" "${BACKUP_PATH}/"
    log "Index files copied"
fi

# 创建备份元信息
cat > "${BACKUP_PATH}/backup-info.json" << EOF
{
    "backup_name": "${BACKUP_NAME}",
    "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "data_dir": "${DATA_DIR}",
    "sstable_count": ${SST_COUNT},
    "backup_type": "$([ "${USE_FLUSH}" = true ] && echo "online" || echo "offline")"
}
EOF

# 计算备份大小
BACKUP_SIZE=$(du -sh "${BACKUP_PATH}" | cut -f1)
log "Backup completed: ${BACKUP_PATH} (${BACKUP_SIZE})"

# 清理旧备份
if [ "${RETENTION_DAYS}" -gt 0 ]; then
    log "Cleaning up backups older than ${RETENTION_DAYS} days..."
    DELETED=$(find "${BACKUP_DIR}" -maxdepth 1 -name "ontodb-backup-*" -type d -mtime +${RETENTION_DAYS} -exec rm -rf {} \; -print | wc -l)
    if [ "${DELETED}" -gt 0 ]; then
        log "Deleted ${DELETED} old backup(s)"
    else
        log "No old backups to delete"
    fi
fi

# 列出当前备份
log "Current backups:"
ls -lh "${BACKUP_DIR}" | grep "ontodb-backup-" || log "  (none)"

log "Backup script finished successfully"
