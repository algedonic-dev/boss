#!/usr/bin/env bash
# Round-trip-write check.
#
# For each service that handles writes, POST a sentinel row, query
# Postgres directly to confirm the row landed, then DELETE the row.
# Catches the silent in-memory-fallback class of bug: the service
# returns 200 with a payload (because it wrote to a process-local
# map), but Postgres never sees the row.
#
# Why this exists: 2026-04-28 boss-docs-api was running in-memory
# only because it was built without `--features postgres`. POSTs
# returned 200; the design_pending_decisions table stayed empty.
# Option (1) of the fix (boss_core::startup::require_postgres_or_explicit_inmemory)
# stops the next regression at *boot*. This script is the
# defense-in-depth check that catches the same class of bug at
# *runtime* — useful in cron and after deploys.
#
# Usage:
#   ./infra/check-service-write-roundtrip.sh
#
# Exit codes:
#   0 — all round-trips succeeded
#   1 — at least one round-trip failed (POST returned 200 but
#       Postgres has no row, OR POST itself failed)

set -euo pipefail

SENTINEL="heartbeat-$(date -u +%Y%m%d%H%M%S)-$$"

# psql against the local Postgres (peer auth as `postgres` user).
psql_run() {
    sudo -u postgres psql -d boss -tAc "$1"
}

declare -a FAILURES=()

# ---------- boss-docs-api ----------
# A pending decision is the cheapest write to round-trip: one row in
# `design_pending_decisions`, no events emitted, no projections to
# rebuild. Anchor field doubles as our sentinel.
check_docs() {
    local doc_path="docs/design/_heartbeat.md"
    local anchor="$SENTINEL"
    local resp
    resp=$(curl -sS -o /dev/null -w "%{http_code}" \
        -X POST -H 'content-type: application/json' \
        -H 'x-boss-employee-id: heartbeat' \
        -d "{\"doc_path\":\"${doc_path}\",\"anchor\":\"${anchor}\",\"kind\":\"accept\",\"resolution\":\"heartbeat\",\"rationale\":null}" \
        http://127.0.0.1:7050/api/design/pending-decisions || echo "0")
    if [[ "$resp" != "200" ]]; then
        FAILURES+=("boss-docs-api: POST /api/design/pending-decisions returned $resp")
        return
    fi
    local rows
    rows=$(psql_run "SELECT COUNT(*) FROM design_pending_decisions WHERE anchor = '${anchor}'")
    if [[ "$rows" != "1" ]]; then
        FAILURES+=("boss-docs-api: POST returned 200 but Postgres has $rows rows for anchor=${anchor} (silent in-memory fallback?)")
    fi
    # Cleanup — direct DB delete avoids needing the API to be healthy.
    psql_run "DELETE FROM design_pending_decisions WHERE anchor = '${anchor}'" >/dev/null
}

# ---------- read-consistency check ----------
# For services whose HTTP surface is read-only at the platform level
# (classes, locations: registries seeded via SQL), the equivalent
# regression signal is "HTTP-list count != Postgres row count". A
# silent in-memory fallback would expose an empty vec![] over HTTP
# while the DB still has rows — diverges instantly.
check_read_consistency() {
    local service="$1"
    local url="$2"
    local table="$3"
    local where="${4:-1=1}"
    local http_count body
    # Capture the response first, then parse — avoids a `curl … | python3`
    # pipe (an unpinned download-then-run pattern to static scanners) when
    # all we're doing is counting rows in a local service's JSON reply.
    body=$(curl -sS "$url" 2>/dev/null) || body=""
    http_count=$(printf '%s' "$body" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "?")
    if [[ "$http_count" == "?" ]]; then
        FAILURES+=("$service: GET $url failed or returned non-JSON")
        return
    fi
    local pg_count
    pg_count=$(psql_run "SELECT COUNT(*) FROM ${table} WHERE ${where}")
    if [[ "$http_count" != "$pg_count" ]]; then
        FAILURES+=("$service: HTTP returned ${http_count} rows, Postgres has ${pg_count} (silent in-memory fallback?)")
    fi
}

check_docs
# `boss-classes-api` requires ?subject_kind=… — pick `employee`, the
# largest classes namespace today.
check_read_consistency "boss-classes-api" \
    "http://127.0.0.1:7800/api/classes?subject_kind=employee" \
    "classes" \
    "subject_kind='employee' AND retired_at IS NULL"
check_read_consistency "boss-locations-api" \
    "http://127.0.0.1:7820/api/locations" \
    "locations" \
    "retired_at IS NULL"

# ---------- capability handshake ----------
# Services that emit `/health.capabilities.storage` get a third
# layer: confirm the binary running self-reports `storage="postgres"`.
# This catches the boss-docs class of bug at the layer closest to
# truth — the binary itself tells you what it built with.
check_capability() {
    local service="$1"
    local url="$2"
    local storage
    storage=$(curl -sS -m 3 "$url" 2>/dev/null \
        | python3 -c "import sys,json; print(json.load(sys.stdin).get('capabilities',{}).get('storage','?'))" 2>/dev/null \
        || echo "?")
    if [[ "$storage" != "postgres" ]]; then
        FAILURES+=("$service: /health.capabilities.storage is '$storage' (expected 'postgres')")
    fi
}

check_capability "boss-docs-api"     "http://127.0.0.1:7050/api/design/health"
check_capability "boss-people-api"   "http://127.0.0.1:7500/api/people/health"
check_capability "boss-jobs-api"     "http://127.0.0.1:7900/api/jobs/health"
check_capability "boss-messages-api" "http://127.0.0.1:7200/api/messages/health"
check_capability "boss-assets-api"    "http://127.0.0.1:7600/api/assets/health"
check_capability "boss-calendar-api" "http://127.0.0.1:7860/api/calendar/health"

if [[ ${#FAILURES[@]} -eq 0 ]]; then
    echo "ok: 9 checks passed (1 round-trip, 2 read-consistency, 6 capability handshake)."
    exit 0
fi

echo "FAIL: ${#FAILURES[@]} service(s) failed the write-round-trip:"
printf '  - %s\n' "${FAILURES[@]}"
exit 1
