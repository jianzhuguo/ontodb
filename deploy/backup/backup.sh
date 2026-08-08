#!/bin/sh
# OntoDB scheduled backup script
#
# Triggers a backup via the HTTP API, then prunes backups older than
# BACKUP_RETENTION_DAYS (default: 7).
#
# This container runs as a one-shot job. Use an external cron or
# Docker Swarm/CronJob to schedule it (e.g., daily at 03:00).

set -eu

ONTODB_URL="${ONTODB_URL:-http://ontodb:7912}"
BACKUP_DIR="/data/backup"
RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-7}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_PATH="${BACKUP_DIR}/${TIMESTAMP}"

echo "=== OntoDB Backup: ${TIMESTAMP} ==="

# 1. Trigger backup via API
echo "Calling ${ONTODB_URL}/api/backup ..."
RESPONSE=$(curl -sf -X POST "${ONTODB_URL}/api/backup" \
  -H "Content-Type: application/json" \
  -d "{\"path\": \"${BACKUP_PATH}\"}" 2>&1) || {
    echo "ERROR: Backup API call failed"
    echo "$RESPONSE"
    exit 1
  }

echo "Backup response: ${RESPONSE}"

# 2. Verify backup directory exists and has files
if [ ! -d "${BACKUP_PATH}" ]; then
  echo "ERROR: Backup directory not created: ${BACKUP_PATH}"
  exit 1
fi

FILE_COUNT=$(find "${BACKUP_PATH}" -type f | wc -l)
echo "Backup contains ${FILE_COUNT} files"

# 3. Prune old backups
echo "Pruning backups older than ${RETENTION_DAYS} days..."
PRUNED=0
for dir in "${BACKUP_DIR}"/*/; do
  [ -d "${dir}" ] || continue
  dir_name=$(basename "${dir}")
  # Only prune directories matching the timestamp pattern
  if echo "${dir_name}" | grep -qE '^[0-9]{8}_[0-9]{6}$'; then
    # Check if directory is older than retention period
    find "${dir}" -maxdepth 0 -mtime +${RETENTION_DAYS} -type d | while read old_dir; do
      echo "Removing old backup: ${old_dir}"
      rm -rf "${old_dir}"
      PRUNED=$((PRUNED + 1))
    done
  fi
done

echo "=== Backup completed successfully ==="
echo "  Path: ${BACKUP_PATH}"
echo "  Files: ${FILE_COUNT}"
