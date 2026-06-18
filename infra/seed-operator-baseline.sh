#!/usr/bin/env bash
# seed-operator-baseline.sh — POST the platform operator-baseline
# (emp-audit + the bootstrap-admin, injected from
# BOSS_BOOTSTRAP_ADMIN_EMAIL or the first BOSS_AUTH_FILE credential)
# to a RUNNING people-api.
#
# Post-API by design: boss-operator-baseline-seed POSTs /api/people —
# the policy-checked, service-mediated path — so the API stack must be
# up. (It used to write audit_log directly via sqlx; that end-around
# was removed, which is why the old pre-API call sites in the docker
# init container + the bare-metal quickstart broke.) This script is the
# converged post-API seed step, called by the quickstart launchers just
# before the brewery tenant seed — the platform-baseline sibling of
# infra/seed-brewery-tenant.sh, and the same thing reset-to-baseline
# does inline at its step 6.
#
# Binaries are PATH-resolved: docker ships them in /usr/local/bin;
# bare-metal callers prepend target/release. Idempotent (409 on a
# duplicate id = skip); retries while the people-api finishes binding.

set -euo pipefail

SEEDS_TOML="${BOSS_OPERATOR_BASELINE_TOML:-/opt/boss/infra/operator-baseline/operator_hires.toml}"

if ! command -v boss-operator-baseline-seed >/dev/null 2>&1; then
    echo "    WARN: boss-operator-baseline-seed not on PATH — admin login won't work" >&2
    exit 0
fi

echo "    seeding operator-baseline + bootstrap-admin via /api/people"
ok=
for attempt in 1 2 3 4 5 6; do
    # BOSS_BOOTSTRAP_ADMIN_EMAIL / BOSS_AUTH_FILE are read from the
    # environment by the binary to inject the platform-admin row.
    if boss-operator-baseline-seed --seed-path "$SEEDS_TOML" >/dev/null 2>&1; then
        ok=1
        echo "    ✓ operator-baseline seeded"
        break
    fi
    echo "    (people-api not ready; retry $attempt)"
    sleep 3
done
[[ -n "$ok" ]] || echo "    WARN: operator-baseline seed failed — admin login may not work" >&2
