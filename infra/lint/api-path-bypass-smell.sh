#!/usr/bin/env bash
#
# api-path-bypass-smell — the categorical guard against "end-arounds":
# loading DATA into the database outside the public API.
#
# THE PRINCIPLE
# -------------
# Domain + seed data is written by a SERVICE (behind its port adapter)
# reacting to a public-API call — never poured straight into the DB by a
# script or a seed binary. A direct-DB data write skips policy, validation,
# and the audit/event path the rest of the system is built on; the sim and
# the model then drift (see docs/design/seed-vs-emergent-state.md and the
# "prepare the model via the API" rule). This lint fails CI on any such
# end-around so they can't creep back in.
#
# WHAT IS ALLOWED (never flagged)
# -------------------------------
#   - service adapters:  crates/*/*/src/postgres.rs   (the Pg port impls —
#                        the ONE legitimate place domain tables are written)
#   - rebuilders:        crates/*/*/src/rebuild.rs    (recompute projections
#                        FROM the audit_log — derived state, not new data)
#   - schema / DDL:      CREATE / DROP / ALTER, dropdb / createdb, apply-schema
#   - reads:             SELECT / to_regclass / EXISTS …
#   - known stop-gaps:   the ALLOWLIST below — each tied to a reason. Clearing
#                        an entry must re-trip the lint.
#
# WHAT IS FLAGGED
# ---------------
#   1. shell scripts (infra/**/*.sh): a `psql` that runs write-DML
#      (INSERT/UPDATE…SET/COPY/DELETE) or `-f`-loads a tenant-seed .sql.
#   2. seed/sim Rust binaries (crates/**/src/bin/**, boss-sim): a sqlx write
#      (INSERT/UPDATE…SET/DELETE) — those belong in a service adapter, reached
#      over HTTP, not in a one-shot bin.
#
# Usage:  infra/lint/api-path-bypass-smell.sh [--strict]
#   --strict ignores the allowlist (use when clearing a stop-gap).
#
# The CI hook lives in .github/workflows/ci.yml alongside the other lints.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

STRICT=0
[[ "${1:-}" == "--strict" ]] && STRICT=1

# Known stop-gaps: "<path-fragment>::<reason>". A hit whose file path contains
# the fragment is skipped (unless --strict). Removing an entry re-trips the
# lint — the workflow is: land the API path, drop the entry.
ALLOWLIST=(
    # init runs pre-API (clock-api isn't up yet), so the demo-epoch sim_clock
    # prime is a direct control-plane write here — the API path can't reach it
    # this early. Tenant DATA seeding (classes/JobKinds/policy/accounts/…) has
    # moved onto the converged 'boss-brewery-sim prepare' step (post-API).
    "infra/oss-quickstart/init.sh::pre-API sim_clock prime — control-plane, clock-api not up at init"
    # The restart-epoch baseline marker is clock control-plane with no API yet.
    # Follow-up: a clock-api 'stamp current state as baseline' endpoint.
    "infra/seed-brewery-tenant.sh::sim_clock restart-baseline marker — control-plane, no API yet"
    # Write-roundtrip *diagnostic*: it writes then asserts the write is visible.
    # Not data-loading; the DELETE is its own cleanup.
    "infra/check-service-write-roundtrip.sh::diagnostic write-roundtrip probe"
    # Retention/GC, not data-loading: purges expired message events on a timer.
    "crates/modules/boss-messages/src/bin/boss_messages_events_purge.rs::message-events retention GC"
)

allowlisted() {  # $1 = "file:line:content"
    [[ "$STRICT" == 1 ]] && return 1
    local file="${1%%:*}"
    for entry in "${ALLOWLIST[@]}"; do
        local frag="${entry%%::*}"
        [[ "$file" == *"$frag"* ]] && return 0
    done
    return 1
}

DML_RE='(INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[A-Za-z_."]+[[:space:]]+SET|COPY[[:space:]]+[A-Za-z_."]+|DELETE[[:space:]]+FROM)'
hits=0

report() {  # $1 = category, $2 = "file:line:content"
    if allowlisted "$2"; then return; fi
    echo "  [$1] ${2}"
    hits=$((hits + 1))
}

# --- 1. shell: psql write-DML (skip comment lines + pure DDL) -----------------
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    # strip "file:line:" to test the content for a leading comment
    content="${line#*:*:}"
    [[ "$content" =~ ^[[:space:]]*# ]] && continue
    report "shell-dml" "$line"
done < <(grep -rnE "$DML_RE" infra --include='*.sh' 2>/dev/null | grep -ivE 'CREATE[[:space:]]|DROP[[:space:]]|ALTER[[:space:]]' || true)

# --- 2. shell: psql -f loading a tenant-seed .sql -----------------------------
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    content="${line#*:*:}"
    [[ "$content" =~ ^[[:space:]]*# ]] && continue
    report "shell-seed-sql" "$line"
done < <(grep -rnE 'psql[^|]*-f[[:space:]]+[^ ]*(classes|seed|playground)[^ ]*\.sql' infra --include='*.sh' 2>/dev/null || true)

# --- 3. rust: sqlx write-DML in seed/sim binaries -----------------------------
# Scope to bin entrypoints + the sim; exclude the legit writers (adapters,
# rebuilders) and tests.
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    report "rust-bin-sqlx" "$line"
done < <(
    grep -rnE 'INSERT[[:space:]]+INTO|UPDATE[[:space:]]+[A-Za-z_]+[[:space:]]+SET|DELETE[[:space:]]+FROM' \
        crates --include='*.rs' 2>/dev/null \
    | grep -E '/src/bin/|/boss-sim/' \
    | grep -viE 'postgres\.rs|rebuild\.rs|/tests/|in_memory|#\[cfg\(test\)\]' \
    || true
)

if [[ "$hits" -gt 0 ]]; then
    echo ""
    echo "api-path-bypass-smell: $hits direct-DB data write(s) bypassing the public API." >&2
    echo "Load data through the API (a service behind its port), or — if genuinely" >&2
    echo "control-plane/maintenance — add an ALLOWLIST entry with a reason." >&2
    exit 1
fi

echo "api-path-bypass-smell: clean — no direct-DB data-write end-arounds."
