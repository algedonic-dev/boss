#!/usr/bin/env bash
# seed-brewery-tenant.sh — publish the brewery tenant onto a running
# BOSS stack via the converged prepare step: classes, JobKinds, tenant
# policy grants, and the seed data (operators / employees / accounts /
# vendors / messages / FG + raw + asset opening balances), then stamp
# the reset baseline. The seeding is one `boss-brewery-sim prepare`
# call (the prepare_model lib fn) so this path and the offline regen
# drive identical code instead of drifting.
#
# Called by both quickstart launchers (bare-metal bootstrap-local.sh
# and docker services-launcher.sh) just before the brewery sim starts.
# None of this is carried by the audit_log any more — the demo builds
# itself live from an empty log — so the live sim needs these entities
# to exist up front or its job/invoice posts 404 and the playground
# sits idle.
#
# Binaries are PATH-resolved: docker ships them in /usr/local/bin;
# bare-metal callers prepend target/release. Idempotent — safe to
# re-run. Event time on the seeded rows is pinned to the demo epoch
# (BOSS_DEMO_EPOCH_START) so they land on day 0, not wall-time.

set -euo pipefail

EPOCH_START="${BOSS_DEMO_EPOCH_START:-2025-04-01}"
SEEDS_DIR="${BOSS_SIM_SEEDS_DIR:-/opt/boss/examples/brewery/seeds}"

# Seed the whole brewery tenant model — classes, JobKinds, policy
# grants, and data (operators / employees / accounts / vendors /
# messages / FG + raw + asset opening balances) — through the public
# API in one call. `boss-brewery-sim prepare` drives the converged
# prepare_model lib fn (classes → JobKinds → policy → data), routing
# to each service's own port. Retry while the stack finishes binding
# (the launcher calls this the moment it reaches the sim in its start
# order). Idempotent + retry-safe: each sub-step skips rows that
# already exist, so a retry after a transient bind failure resumes
# cleanly. BOSS_SIM_SEEDS_DIR points at the tenant bundle;
# BOSS_EPOCH_START pins seeded event time to the demo epoch (the
# seeder rebases the clock to (epoch − 1 day) internally).
# Wait for every service prepare writes through to be reachable before
# seeding. On a cold stack the ~24 services take 30-90s to finish binding +
# startup reconciliation; prepare hard-fails on the first unreachable core
# service (classes/jobs/policy/people/accounts) and silently SKIPS the
# base_reachable-guarded optional seeds (catalog/assets/FG), leaving a
# half-seeded, idle demo. Gating on health up front lets prepare run once
# against a fully-up stack instead of racing it. Ports come from
# boss-ports-list (the canonical table shared with the binaries), so a port
# rename lands here for free. Install-only path (Playwright seeds via
# boss-brewery-data-seed, not this script), so waiting for every service is
# always correct here. Degrades to a no-op if the tooling is absent.
wait_for_stack() {
    command -v boss-ports-list >/dev/null 2>&1 || return 0
    command -v curl >/dev/null 2>&1 || return 0
    # subject-kinds joined the list with Q6: prepare step 1c mints the
    # company identity through it (hard-fail on refusal), so a cold
    # stack must have it bound before prepare starts.
    local svcs="classes subject-kinds jobs policy people accounts inventory messages content calendar products ledger catalog assets"
    declare -A PORT
    while IFS=: read -r name prod _; do [[ -n "$name" ]] && PORT["$name"]="$prod"; done < <(boss-ports-list --paired 2>/dev/null)
    while IFS=: read -r name prod;  do [[ -n "$name" ]] && PORT["$name"]="$prod"; done < <(boss-ports-list --solo 2>/dev/null)
    local deadline=$(( SECONDS + 150 )) svc port code pending
    while :; do
        pending=""
        for svc in $svcs; do
            port="${PORT[$svc]:-}"; [[ -z "$port" ]] && continue
            code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$port/api/$svc/health" 2>/dev/null || echo 000)
            [[ "$code" == "200" ]] || pending="$pending$svc "
        done
        [[ -z "$pending" ]] && { echo "    stack ready — all seed targets healthy"; return 0; }
        (( SECONDS >= deadline )) && { echo "    WARN: stack not fully ready after 150s (pending: $pending) — seeding anyway" >&2; return 0; }
        echo "    waiting for stack (pending: $pending)"; sleep 3
    done
}

if command -v boss-brewery-sim >/dev/null 2>&1; then
    echo "    preparing brewery tenant model (boss-brewery-sim prepare)"
    wait_for_stack
    ok=
    for attempt in 1 2 3; do
        if BOSS_SIM_SEEDS_DIR="$SEEDS_DIR" BOSS_EPOCH_START="$EPOCH_START" \
            boss-brewery-sim prepare >/dev/null 2>&1; then
            ok=1; echo "    ✓ brewery tenant prepared"; break
        fi
        echo "    (prepare attempt $attempt failed; retrying)"; sleep 3
    done
    [[ -n "$ok" ]] || echo "    WARN: brewery prepare failed — sim will idle" >&2
else
    echo "    WARN: boss-brewery-sim not on PATH — sim will idle" >&2
fi

# Stamp the reset baseline: the panel's Reset button (restart-epoch) trims
# the audit_log back to this id, so it must be MAX *after* the tenant seed
# and *before* the sim's first tick — i.e. "seeded day 0". The endpoint
# self-heals if this is unset, but captures too late (after sim activity),
# so set it explicitly now while the log holds only the seed.
if command -v psql >/dev/null 2>&1 && [[ -n "${BOSS_POSTGRES_URL:-}" ]]; then
    if psql "$BOSS_POSTGRES_URL" -v ON_ERROR_STOP=1 -c \
        "UPDATE sim_clock SET epoch_baseline_audit_id = (SELECT COALESCE(MAX(id),0) FROM audit_log) WHERE id = 1;" \
        >/dev/null 2>&1; then
        echo "    ✓ reset baseline stamped (seeded day 0)"
    else
        echo "    WARN: reset-baseline stamp failed; restart-epoch will self-heal" >&2
    fi
fi
