#!/usr/bin/env bash
# Backup critical Boss configuration files.
#
# Saves: gateway session key, service configs,
# and a Postgres dump. Output: timestamped tarball in /var/backups/boss/.
#
# Usage: sudo ./infra/backup.sh

set -euo pipefail

BACKUP_DIR="/var/backups/boss"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
WORKDIR=$(mktemp -d)
DEST="${WORKDIR}/boss-backup-${TIMESTAMP}"
mkdir -p "${DEST}" "${BACKUP_DIR}"

echo "Boss backup — ${TIMESTAMP}"

# Gateway secrets
echo "  copying gateway config..."
cp -a /etc/boss-gateway/ "${DEST}/boss-gateway/" 2>/dev/null || echo "  (skipped — /etc/boss-gateway/ not found)"

# Service configs
echo "  copying service configs..."
for f in /etc/boss-*.toml; do
  [ -f "$f" ] && cp "$f" "${DEST}/"
done

# Postgres dump
echo "  dumping Postgres..."
sudo -u postgres pg_dump boss --no-owner --no-privileges > "${DEST}/boss.sql" 2>/dev/null || echo "  (skipped — pg_dump failed)"

# Systemd units
echo "  copying systemd units..."
mkdir -p "${DEST}/systemd"
for f in /etc/systemd/system/boss-*.service; do
  [ -f "$f" ] && cp "$f" "${DEST}/systemd/"
done

# Create tarball
TARBALL="${BACKUP_DIR}/boss-backup-${TIMESTAMP}.tar.gz"
tar -czf "${TARBALL}" -C "${WORKDIR}" "boss-backup-${TIMESTAMP}"
rm -rf "${WORKDIR}"

SIZE=$(du -sh "${TARBALL}" | cut -f1)
echo "  backup: ${TARBALL} (${SIZE})"

# Keep only the last 5 backups
ls -t "${BACKUP_DIR}"/boss-backup-*.tar.gz 2>/dev/null | tail -n +6 | xargs rm -f 2>/dev/null || true

echo "done."
