#!/usr/bin/env bash
# Pull a sanitized prod snapshot to the current dev DB so bug repros
# against prod state stay possible.
#
# Usage:
#   sudo ./infra/dev-refresh.sh                              # default source: boss-manager-1
#   sudo BOSS_REFRESH_SOURCE=boss-prod-1 ./infra/dev-refresh.sh
#   sudo BOSS_REFRESH_DEST_DB=boss_scratch ./infra/dev-refresh.sh
#
# Sequence:
#   1. SSH to source host, run `pg_dump audit_log` → gzipped stream.
#   2. Stream-pipe down to local /tmp/dev-refresh-$(date).sql.gz.
#   3. Stop services that hold connections to the dest DB.
#   4. Drop + recreate dest DB; apply the per-module schema.
#   5. Apply the audit_log dump.
#   6. Run `boss-rebuild-all --database-url postgres://boss@.../$DEST`
#      to project audit_log → every projection, exactly the way a
#      fresh deploy would replay the canonical seed.
#   7. Restart services.
#
# What is NOT pulled: messages_events (90-day retention sweep
# semantically separate; never load-bearing for repro), and any
# deploy-side state in /etc (configs, certs). Those live in
# `infra/backup.sh` snapshots that the operator can apply
# separately if a config-shaped repro is needed.
#
# PII / scrub posture: the demo data Algedonic Ales ships is already
# fully synthetic (LLM-generated fictional names + fabricated emails;
# operator-baseline operators carry role-string names per
# `infra/operator-baseline/operator_hires.toml`). No scrub step
# required for the OSS-demo tenant. Real-tenant deployments running
# on this script MUST add a scrub pass here before the dump leaves
# the source — TODO: `infra/dev-refresh-scrub.sh` once a
# real-tenant deploy lands.

set -euo pipefail

SOURCE_HOST="${BOSS_REFRESH_SOURCE:-boss-manager-1}"
SOURCE_DB="${BOSS_REFRESH_SOURCE_DB:-boss}"
DEST_DB="${BOSS_REFRESH_DEST_DB:-boss}"
DEST_USER="${BOSS_REFRESH_DEST_USER:-boss}"
TMP_DIR="${BOSS_REFRESH_TMP:-/tmp}"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DUMP_PATH="${TMP_DIR}/dev-refresh-${SOURCE_HOST}-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"

echo "==> dev-refresh: source=${SOURCE_HOST}.${SOURCE_DB} → local.${DEST_DB}"
echo "    dump:  ${DUMP_PATH}"

# -- Step 1+2: SSH + pull audit_log -------------------------------
echo "==> [1/7] dumping audit_log on ${SOURCE_HOST}"
# Plain `ssh` with operator-supplied keys (the SSH-CA flow was ripped
# in v0.1; see `infra/blueprints/ssh-ca/` to opt back in). Piping
# `pg_dump --no-owner --no-acl` through gzip keeps the stream small.
# The remote side runs as the postgres superuser via `sudo -u postgres`.
# If the source operator has a different layout, override
# BOSS_REFRESH_SOURCE_PGDUMP_CMD.
PGDUMP_CMD="${BOSS_REFRESH_SOURCE_PGDUMP_CMD:-sudo -n -u postgres pg_dump -t audit_log --no-owner --no-acl --data-only ${SOURCE_DB} | gzip}"
ssh -o BatchMode=yes "${SOURCE_HOST}" "${PGDUMP_CMD}" > "${DUMP_PATH}"
echo "    pulled $(stat -c%s "${DUMP_PATH}") bytes"

# -- Step 3: stop services holding connections --------------------
echo "==> [3/7] stopping local services that hold ${DEST_DB} connections"
SERVICES=(
    boss-jobs-api
    boss-policy-api
    boss-people-api
    boss-commerce-api
    boss-inventory-api
    boss-shipping-api
    boss-assets-api
    boss-catalog-api
    boss-ledger-api
    boss-content-api
    boss-messages-api
    boss-ml-api
    boss-products-api
    boss-dispatcher
    boss-brewery-sim
)
for svc in "${SERVICES[@]}"; do
    sudo systemctl stop "$svc" 2>/dev/null || true
done

# -- Step 4: drop + recreate dest DB + apply schema ---------------
echo "==> [4/7] reapplying schema to ${DEST_DB}"
"${REPO_ROOT}/infra/postgres/bootstrap-db.sh" "${DEST_DB}"

# -- Step 5: apply the dump ---------------------------------------
echo "==> [5/7] importing audit_log dump"
zcat "${DUMP_PATH}" | sudo -n -u postgres psql -d "${DEST_DB}" >/dev/null
audit_count=$(sudo -n -u postgres psql -d "${DEST_DB}" -tAc \
    "SELECT COUNT(*) FROM audit_log")
echo "    imported ${audit_count} audit_log rows"

# -- Step 6: rebuild projections ----------------------------------
echo "==> [6/7] rebuilding projections from audit_log"
DEST_URL="postgres://${DEST_USER}:${DEST_USER}@127.0.0.1/${DEST_DB}"
boss-rebuild-all --database-url "${DEST_URL}"

# -- Step 7: restart services -------------------------------------
echo "==> [7/7] restarting services"
START_ORDER=(
    boss-policy-api
    boss-people-api
    boss-commerce-api
    boss-inventory-api
    boss-shipping-api
    boss-assets-api
    boss-catalog-api
    boss-ledger-api
    boss-content-api
    boss-messages-api
    boss-ml-api
    boss-products-api
    boss-jobs-api
    boss-dispatcher
    boss-brewery-sim
)
for svc in "${START_ORDER[@]}"; do
    sudo systemctl start "$svc" 2>/dev/null || true
done

echo
echo "==> done. ${audit_count} audit_log rows from ${SOURCE_HOST} now in ${DEST_DB}."
echo "    dump retained at ${DUMP_PATH} (delete when done with the repro)."
