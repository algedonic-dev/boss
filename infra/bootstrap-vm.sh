#!/usr/bin/env bash
# bootstrap-vm.sh — single-command BOSS install on a fresh Ubuntu 24.04 VM.
#
# Sequence:
#   0. apt packages (postgres, build deps, unzip)
#   1. Rust toolchain
#   2. Bun
#   3. Postgres role + DB bootstrap
#   4. NATS
#   5. cargo build --release --workspace (+ postgres-feature follow-ups)
#   6. Install binaries to /usr/local/bin
#   7. Re-seed registries (now that binaries are on PATH)
#   8. Deploy services
#   9. Tenant-specific seed (brewery OR used-device-shop)
#  10. Health-probe report
#
# Usage:
#   sudo TENANT=brewery        /opt/boss/infra/bootstrap-vm.sh
#   sudo TENANT=device-shop    /opt/boss/infra/bootstrap-vm.sh
#
# Designed to be idempotent — re-runs skip already-completed work
# where it's cheap to check. Intended for fresh-VM install
# verification ahead of a release cut.

set -euo pipefail

TENANT="${TENANT:-brewery}"
REPO_ROOT="${REPO_ROOT:-/opt/boss}"
DEV_USER="${DEV_USER:-boss}"

case "$TENANT" in
    brewery|device-shop) ;;
    *)
        echo "TENANT must be 'brewery' or 'device-shop'" >&2
        exit 1
        ;;
esac

log() { echo "[bootstrap-vm $(date +%H:%M:%S) $TENANT] $*"; }

log "== 0 — apt packages =="
DEBIAN_FRONTEND=noninteractive apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential pkg-config libssl-dev curl ca-certificates \
    unzip jq git postgresql postgresql-contrib

log "== 1 — Rust toolchain =="
if ! sudo -u "$DEV_USER" -i bash -c 'command -v cargo' >/dev/null 2>&1; then
    sudo -u "$DEV_USER" -i bash -lc '
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --default-toolchain stable --profile minimal'
fi

log "== 2 — Bun =="
if ! sudo -u "$DEV_USER" -i bash -c 'command -v bun' >/dev/null 2>&1; then
    sudo -u "$DEV_USER" -i bash -lc 'curl -fsSL https://bun.sh/install | bash'
fi

log "== 3 — Postgres role + databases =="
if ! sudo -u postgres psql -d postgres -tc "SELECT 1 FROM pg_roles WHERE rolname='boss'" | grep -q 1; then
    sudo -u postgres psql -d postgres -c \
        "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss'"
fi

# The ~24 API services each run an sqlx pool (default max 10 connections),
# so the cluster needs well above Postgres's default max_connections=100 —
# at 100 the pools saturate it ("FATAL: sorry, too many clients already")
# and burst load (a warp-2000 regen, JetStream redelivery) starves accepts
# until requests fail at the connection level. 400 matches bootstrap-local.sh,
# the docker quickstart, and the documented playground setting (see
# infra/deploy-services.sh). Needs a full restart, so it runs here — before
# any service connects.
CURRENT_MAXCONN=$(sudo -u postgres psql -tAc "SHOW max_connections" 2>/dev/null || echo 0)
if [[ "${CURRENT_MAXCONN:-0}" -lt 400 ]]; then
    log "raising Postgres max_connections ($CURRENT_MAXCONN -> 400) for the service stack"
    sudo -u postgres psql -c "ALTER SYSTEM SET max_connections = 400" >/dev/null
    systemctl restart postgresql
    for _ in $(seq 1 30); do pg_isready -h 127.0.0.1 -p 5432 -q && break; sleep 1; done
fi

"$REPO_ROOT/infra/postgres/bootstrap-boss.sh"
"$REPO_ROOT/infra/postgres/bootstrap-scratch.sh"

log "== 4 — NATS =="
if ! systemctl is-active --quiet nats-server; then
    "$REPO_ROOT/infra/nats/setup.sh"
fi

log "== 5 — cargo build (pass 1: workspace) =="
sudo -u "$DEV_USER" -i bash -lc "cd $REPO_ROOT && cargo build --release --workspace"

log "== 5b — cargo build (pass 2: postgres-feature binaries) =="
sudo -u "$DEV_USER" -i bash -lc "cd $REPO_ROOT && cargo build --release \
    -p boss-policy        --bin boss-policy-api        --features postgres \
    -p boss-docs          --bin boss-docs-api          --features postgres \
    -p boss-classes       --bin boss-classes-api       --features postgres \
    -p boss-locations     --bin boss-locations-api     --features postgres \
    -p boss-subject-kinds --bin boss-subject-kinds-api --features postgres \
    -p boss-events        --bin boss-events-api        --features events-api \
    -p boss-accounts      --bin boss-accounts-api      --features accounts-api"

log "== 6 — install binaries =="
cd "$REPO_ROOT/target/release"
# Stamp every built bin as current — same reasoning as build-release.sh:
# after a clean build every binary IS up to date (cargo rebuilt or
# verified it), but cargo leaves a skipped bin's mtime untouched, and a
# re-checkout refreshes every SOURCE mtime — so the deploy freshness
# guard false-flags untouched binaries as stale on the second run of
# this script (bit the 2026-07-08 regen-VM rerun).
find . -maxdepth 1 -type f -executable -name 'boss-*' -exec touch {} +
install -m 755 -t /usr/local/bin/ $(ls boss-* | grep -v '\.d$')

log "== 7 — re-seed registries =="
"$REPO_ROOT/infra/postgres/bootstrap-boss.sh"

log "== 8 — deploy services =="
"$REPO_ROOT/infra/deploy-services.sh" prod

log "== 9 — tenant seed =="
case "$TENANT" in
    brewery)
        # Converged tenant prepare: classes + JobKinds + policy + data
        # (operators / employees / accounts / vendors / opening
        # balances) in one call — the same prepare_model the live demo
        # (seed-brewery-tenant.sh) and CI (validate-brewery-sim.sh)
        # run. Binaries were installed to /usr/local/bin in step 6, so
        # resolve boss-brewery-sim by bare name.
        if command -v boss-brewery-sim >/dev/null; then
            BOSS_SIM_SEEDS_DIR="$REPO_ROOT/examples/brewery/seeds" \
                boss-brewery-sim prepare || log "WARN: brewery prepare exit non-zero"
        else
            log "WARN: boss-brewery-sim not on PATH — brewery tenant not seeded"
        fi
        ;;
    device-shop)
        if command -v boss-used-device-shop-engine >/dev/null; then
            log "device-shop tenant — engine binary present"
        else
            log "WARN: boss-used-device-shop-engine not on PATH"
        fi
        ;;
esac

log "== 10 — health probes =="
sleep 3
for svc_port in \
    jobs:7900 commerce:7400 inventory:7300 assets:7600 \
    shipping:7100 messages:7200 people:7500 catalog:7750 \
    calendar:7860 events:7150 accounts:7550 \
    clock:7060 ml:7070 ledger:7080 content:7090 \
    docs:7050 policy:7250 classes:7800 locations:7820 \
    subject-kinds:7830 products:7840 observability:7880; do
    IFS=: read -r name port <<<"$svc_port"
    case "$name" in
        observability) path="/api/health" ;;
        classes|locations|subject-kinds|events|accounts)
            path="/api/$name/health"
            ;;
        docs)          path="/api/design/health" ;;
        *)             path="/api/$name/health" ;;
    esac
    code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$port$path" || echo "000")
    printf "  %-18s %-4s %s\n" "$name" "$code" "$path"
done

log "== done — tenant=$TENANT =="
