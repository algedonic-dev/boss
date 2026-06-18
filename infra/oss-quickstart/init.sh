#!/usr/bin/env bash
# Init container — runs once per fresh database, then exits 0.
#
# init runs PRE-API: the boss-services container (which brings the API
# stack up) starts only after this exits. So init does only what can be
# done without the services:
#   1. Wait for Postgres.
#   2. Apply the per-module schema.
#   3. Provision the bootstrap-admin's local-auth credential (a file write).
#   4. Prime the formula clock to the demo epoch.
#
# Everything that goes through the public API — the operator-baseline +
# bootstrap-admin EMPLOYEE, the brewery tenant (classes, JobKinds, policy,
# accounts/vendors/data), and the sim that builds the demo live — is run by
# boss-services (services-launcher.sh) once the API is up. That's why
# operator/employee seeding can't live here: boss-operator-baseline-seed
# POSTs /api/people, which isn't listening during init.
#
# Idempotent: if the schema already exists a prior init ran — exit 0 and
# let boss-services re-seed (its seeds are 409-tolerant). Clean restart is
# `docker compose down -v` then `up`.

set -euo pipefail

REPO=/opt/boss
EMAIL="${BOSS_BOOTSTRAP_ADMIN_EMAIL:?BOSS_BOOTSTRAP_ADMIN_EMAIL must be set}"
EMAIL="${EMAIL,,}"

echo "==> boss-init starting"
echo "    bootstrap-admin: $EMAIL"
echo "    mode:            live sim from empty"

# ---- 1. wait for Postgres ----------------------------------------------------

for i in $(seq 1 30); do
    if pg_isready -h "$PGHOST" -U "$PGUSER" -q; then
        break
    fi
    echo "    waiting for postgres ($i/30)..."
    sleep 2
done

# ---- 2. already initialized? -------------------------------------------------
# "schema present" (subject_kinds exists) means a prior init ran. The
# operator-baseline + brewery seed + sim are (re-)run by boss-services on
# every up and are idempotent, so there's nothing to redo here — exit 0.
# A clean restart is `docker compose down -v` (wipes the volume) then `up`.
SUBJECT_KINDS_EXISTS=$(psql -At -c "SELECT to_regclass('subject_kinds')" 2>/dev/null || echo "")
if [[ -n "$SUBJECT_KINDS_EXISTS" ]]; then
    echo "==> Database already initialized (schema present). Nothing to do."
    echo "    boss-services re-seeds the tenant on every up; clean restart:"
    echo "    docker compose down -v  &&  docker compose up"
    exit 0
fi

# ---- 3. apply schema ---------------------------------------------------------

echo "==> [1/3] applying per-module schema (manifest)"
"$REPO/infra/postgres/apply-schema.sh" | psql -v ON_ERROR_STOP=1 >/dev/null

# The demo builds itself live: boss-services seeds the operator-baseline +
# brewery tenant through the public API and starts the sim, which grows the
# audit_log from empty. There's no bulk seed load and no pre-API rebuild —
# audit_log is empty until the services run (see services-launcher.sh).

# ---- 4. provision the bootstrap-admin credential -----------------------------
# The bootstrap-admin EMPLOYEE is seeded post-API by boss-services
# (services-launcher.sh → seed-operator-baseline.sh, which reads
# BOSS_BOOTSTRAP_ADMIN_EMAIL). Here we write only the matching local-auth
# credential — a file, no API needed. v1 uses a fixed default ("change-me")
# the operator MUST rotate via `boss-auth set $EMAIL` after first login. The
# file lives under /var/lib/boss/auth/credentials.toml, persisted via the
# docker volume so it survives container recreation.
echo "==> [2/3] provisioning bootstrap-admin credential"
DEFAULT_PASSWORD="${BOSS_BOOTSTRAP_ADMIN_PASSWORD:-change-me}"
export BOSS_AUTH_FILE="${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}"
mkdir -p "$(dirname "$BOSS_AUTH_FILE")"
# `boss-auth set` is a no-flag CLI: piped stdin is the new password.
# Don't suppress stderr — when this fails, the actual error is the
# whole story (missing dir perms, tty detection, etc.).
if echo "$DEFAULT_PASSWORD" | boss-auth set "$EMAIL"; then
    echo "    ✓ Credential set for $EMAIL (password: $DEFAULT_PASSWORD)"
    echo "    ⚠  Rotate it with: docker compose exec boss-services boss-auth set $EMAIL"
else
    echo "    WARN: failed to provision credential for $EMAIL — see stderr above"
fi

# ---- 5. prime the formula clock for the live playground ----------------------
# The brewery-sim is clock-authoritative: it reads /api/clock/now to pick the
# sim-day to advance, and boss-clock-api runs in sim mode (BOSS_CLOCK_MODE=sim
# in compose), reading sim_clock at startup. Prime the row to the demo epoch
# (fixed 2025-04-01, override via BOSS_DEMO_EPOCH_START) so the playground
# ticks forward at 1000x instead of sitting frozen at wall-time. The post-API
# seeds (in services-launcher.sh) run against this clock so their events land
# on day 0. This is a direct sim_clock write because clock-api isn't up yet.
DEMO_EPOCH="${BOSS_DEMO_EPOCH_START:-2025-04-01}"
echo "==> [3/3] priming sim_clock to $DEMO_EPOCH for the live playground"
# epoch_end = epoch_start + 365 gives the playground a 12-month range; without
# an epoch_end past epoch_start the loop is zero-length and the sim auto-pauses
# on the first tick ('epoch complete'), leaving the demo frozen.
if psql -v ON_ERROR_STOP=1 -c "
    INSERT INTO sim_clock
        (id, epoch_start_date, epoch_end_date, warp_factor, wall_anchor,
         paused, paused_offset_seconds, restart_in_progress)
    VALUES
        (1, DATE '$DEMO_EPOCH', DATE '$DEMO_EPOCH' + 365, 1000, NOW(),
         false, 0, false)
    ON CONFLICT (id) DO UPDATE SET
        epoch_start_date = EXCLUDED.epoch_start_date,
        epoch_end_date   = EXCLUDED.epoch_end_date,
        warp_factor      = EXCLUDED.warp_factor,
        wall_anchor      = EXCLUDED.wall_anchor;" >/dev/null; then
    echo "    ✓ formula clock primed to $DEMO_EPOCH @ 1000x warp"
else
    echo "    WARN: sim_clock prime failed; playground will sit at wall-time" >&2
fi

echo "==> boss-init done."
