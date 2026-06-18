#!/usr/bin/env bash
# infra/oss-quickstart/quickstart.sh — five-minute clone-to-SPA path
# for evaluators kicking the OSS tires.
#
# What this does, in order:
#   1. Check the four hard prerequisites (Rust, Bun, Postgres, NATS).
#      Bail with a one-line install hint per missing tool.
#   2. Prompt for a bootstrap-admin email (or accept --email=...).
#      Becomes the seed Employee row + the local-dev session
#      identity the SPA opens with.
#   3. Run the existing infra/bootstrap-local.sh — cargo build,
#      DB drop+recreate (empty — the live sim builds the demo), SPA build.
#   4. Insert the bootstrap-admin Employee with role='platform-admin'
#      so the operator has full permissions out of the box.
#   5. Start every service as a background process (PIDs in ~/.boss-pids)
#      including boss-gateway with BOSS_DEMO_MODE=1 +
#      BOSS_AUTH_PROVIDER=local-auth. Operator opens
#      http://127.0.0.1:4443 and is in — anonymous visitors get
#      `audit-readonly` (everything renders, no writes); log in
#      at /login with the bootstrap-admin email + 'change-me' to
#      upgrade to platform-admin.
#
# What this is NOT: a production setup. The gateway runs the
# file-backed local-auth provider (Argon2id-hashed credentials,
# signed session cookies, admin-issued password reset tokens)
# next to BOSS_DEMO_MODE — real auth gates writes, demo mode
# gates reads. No SSO, no MFA, no account lockout, no rate
# limiting, no edge-tier hardening. This is the OSS evaluation
# path; production-grade deployments are tracked under the
# "Production Infrastructure Default" + "IAM Integration" TODO
# entries.
#
# Re-run safe: idempotent against an existing DB. Drops the live
# state and re-seeds the tenant, so any in-flight changes are lost —
# which is the right semantic for a "give me a clean evaluation
# playground" runbook.
#
# Usage:
#   ./infra/oss-quickstart/quickstart.sh
#   ./infra/oss-quickstart/quickstart.sh --email=ops@example.com
#   ./infra/oss-quickstart/quickstart.sh --skip-build  # DB + SPA only
#   ./infra/oss-quickstart/quickstart.sh --no-start    # don't auto-start
#
# When the script returns, the SPA is at http://127.0.0.1:4443.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EMAIL=""
SKIP_BUILD=0
SKIP_SPA=0
NO_START=0
SKIP_PREFLIGHT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --email=*)     EMAIL="${1#--email=}" ;;
        --skip-build)  SKIP_BUILD=1 ;;
        --skip-spa)    SKIP_SPA=1 ;;
        --no-start)    NO_START=1 ;;
        --skip-preflight) SKIP_PREFLIGHT=1 ;;
        --help|-h)     sed -n '2,30p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
    shift
done

# ---- 1. prereqs --------------------------------------------------------------

missing=()
command -v cargo >/dev/null 2>&1 || missing+=("cargo — install via rustup: https://rustup.rs/")
command -v bun   >/dev/null 2>&1 || missing+=("bun   — install: curl -fsSL https://bun.sh/install | bash")
command -v psql  >/dev/null 2>&1 || missing+=("psql  — install: postgresql-client (apt) or brew install libpq")

# Postgres + NATS are listening?
if ! pg_isready -h 127.0.0.1 -p 5432 -q 2>/dev/null; then
    missing+=("postgres on 127.0.0.1:5432 — install + start postgres@16")
fi
if ! (echo > /dev/tcp/127.0.0.1/4222) >/dev/null 2>&1; then
    missing+=("nats on 127.0.0.1:4222 — install nats-server: https://nats.io/download/")
fi

if [[ ${#missing[@]} -gt 0 ]]; then
    echo "==> Quickstart can't proceed — missing prerequisites:"
    for m in "${missing[@]}"; do echo "    - $m"; done
    echo ""
    echo "    Install the missing pieces, then re-run this script."
    exit 1
fi

# ---- 1b. preflight: expose conflicts with a prior install / running services -
# Catch leftover state BEFORE we build + reset the DB, so a stale BOSS process
# or an occupied gateway port surfaces as a clear message instead of a cryptic
# failure later. Bare-metal mode: the prereq Postgres/NATS are expected (not
# flagged); blocking conflicts (gateway port 4443 busy, prior BOSS services
# still running) abort here — the fix is usually `kill $(cat ~/.boss-pids)`.
# The "DB already initialized" note is a warning only: this script resets the
# DB by design. Override with --skip-preflight.
if [[ "$SKIP_PREFLIGHT" -eq 0 ]]; then
    if ! "$REPO_ROOT/infra/oss-quickstart/preflight.sh" --mode bare-metal; then
        echo ""
        echo "==> Quickstart halted: preflight found blocking conflicts (above)." >&2
        echo "    Resolve them, or re-run with --skip-preflight to proceed anyway." >&2
        exit 1
    fi
fi

# ---- 2. bootstrap-admin email -----------------------------------------------

if [[ -z "$EMAIL" ]]; then
    echo ""
    echo "===================================================================="
    echo "  BOSS OSS quickstart — single-VM evaluation setup"
    echo "===================================================================="
    echo ""
    echo "  This script will:"
    echo "    - drop + recreate the local 'boss' Postgres database (empty)"
    echo "    - build the SPA + every service"
    echo "    - start the live brewery sim — it builds the 12-month demo from empty"
    echo "    - start the stack on http://127.0.0.1:4443"
    echo ""
    echo "  ⚠  Not production-ready. Demo mode is on: anonymous visitors"
    echo "     get audit-readonly (everything renders, no writes). The"
    echo "     'first user' below becomes the seed admin Employee — log"
    echo "     in at /login with their email + 'change-me' to get write"
    echo "     access. Rotate the password right after."
    echo ""
    read -r -p "  Bootstrap-admin email: " EMAIL
fi
if [[ -z "$EMAIL" ]] || [[ "$EMAIL" != *@*.* ]]; then
    echo "ERROR: '$EMAIL' doesn't look like a valid email." >&2
    exit 2
fi
EMAIL="${EMAIL,,}"  # lowercase

echo ""
echo "==> bootstrap-admin email: $EMAIL"

# ---- (the live sim builds the demo from empty) ------------------------------

# ---- 3. run existing bootstrap-local.sh -------------------------------------

BOOTSTRAP_FLAGS=()
[[ "$SKIP_BUILD" -eq 1 ]] && BOOTSTRAP_FLAGS+=(--skip-build)
[[ "$SKIP_SPA"   -eq 1 ]] && BOOTSTRAP_FLAGS+=(--skip-spa)

# Auto-skip the DB bootstrap if `boss` is already populated (audit_log
# has >1000 rows — the live sim has been running). Re-bootstrapping drops
# + recreates the DB, wiping a running demo; skip it when the operator is
# just iterating on .env or restarting services.
EXISTING_ROWS=$(PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -At \
    -c "SELECT COUNT(*) FROM audit_log" 2>/dev/null || echo 0)
if [[ "$EXISTING_ROWS" -gt 1000 ]]; then
    echo ""
    echo "==> DB 'boss' already has $EXISTING_ROWS audit_log rows — skipping seed."
    echo "    To force a fresh reseed, drop the DB first:"
    echo "      sudo -u postgres dropdb boss"
    BOOTSTRAP_FLAGS+=(--skip-db)
fi

echo ""
echo "==> Running infra/bootstrap-local.sh (build + DB + SPA)..."
"$REPO_ROOT/infra/bootstrap-local.sh" "${BOOTSTRAP_FLAGS[@]}"

# ---- 4. provision the bootstrap-admin login --------------------------------

echo ""
echo "==> Provisioning bootstrap-admin login..."

# The bootstrap-admin EMPLOYEE is seeded post-API by the start step below
# (bootstrap-local.sh --start → seed-operator-baseline.sh → POST /api/people).
# boss-operator-baseline-seed goes through the public API now, so it needs the
# stack up — it can't run here, before services start. Export the email so the
# start step injects the platform-admin row; here we only provision the
# matching local-auth credential (a file write, API-independent).
export BOSS_BOOTSTRAP_ADMIN_EMAIL="$EMAIL"

# ---- 4b. seed bootstrap-admin's password in the local-auth file --
# Mirrors what the docker quickstart's init.sh does. Path matches
# the gateway's BOSS_AUTH_FILE default. Operator MUST rotate
# afterwards via `boss-auth set $EMAIL`.
DEFAULT_PASSWORD="${BOSS_BOOTSTRAP_ADMIN_PASSWORD:-change-me}"
AUTH_FILE="${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}"
if [[ -x "$REPO_ROOT/target/release/boss-auth" ]]; then
    sudo mkdir -p "$(dirname "$AUTH_FILE")"
    sudo chown "$USER:$USER" "$(dirname "$AUTH_FILE")"
    if echo "$DEFAULT_PASSWORD" | BOSS_AUTH_FILE="$AUTH_FILE" \
        "$REPO_ROOT/target/release/boss-auth" set "$EMAIL" >/dev/null 2>&1; then
        echo "    ✓ Credential set for $EMAIL (password: $DEFAULT_PASSWORD)"
        echo "    ⚠  Rotate it: BOSS_AUTH_FILE=$AUTH_FILE $REPO_ROOT/target/release/boss-auth set $EMAIL"
    else
        echo "    WARN: failed to provision credential for $EMAIL — set manually after first run"
    fi
else
    echo "    SKIP: boss-auth binary not built — login won't work until you re-run with --skip-spa"
fi

# ---- 5. start the stack ------------------------------------------------------

if [[ "$NO_START" -eq 0 ]]; then
    echo ""
    echo "==> Starting services in the background..."
    # bootstrap-local.sh --start launches every service as a plain
    # background process (logs -> ~/.boss-logs/, PIDs -> ~/.boss-pids).
    "$REPO_ROOT/infra/bootstrap-local.sh" --skip-build --skip-spa --start || true
fi

echo ""
echo "===================================================================="
echo "  Quickstart complete."
echo "===================================================================="
echo ""
echo "  SPA:        http://127.0.0.1:4443  (anonymous = audit-readonly demo)"
echo "  Login:      http://127.0.0.1:4443/login  → upgrades to platform-admin"
echo "  Bootstrap:  $EMAIL  (role=platform-admin)"
echo ""
echo "  Stop:    kill \$(cat ~/.boss-pids)"
echo "  Re-run:  ./infra/oss-quickstart/quickstart.sh --email=$EMAIL"
echo ""
echo "  ⚠  Not production-ready. Real auth, HA topology, and email-OTP"
echo "     reset land under the Production Infrastructure + IAM"
echo "     Integration TODOs (see TODO.md)."
echo ""
