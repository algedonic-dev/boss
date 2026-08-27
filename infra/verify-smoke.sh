#!/bin/bash
# Smoke verification — asserts that every operating motion fired at
# least once during a short sim run. Pair of verify-replay.sh which
# is calibrated for the 365-day Algedonic Ales sim; verify-smoke is
# the "did anything happen at all" gate for short / dry-run sims.
#
# Per the CLAUDE.md / memory `feedback_no_raw_sql_for_diagnostics`:
# this script uses HTTP APIs only. No raw psql.
#
# Run after `BOSS_REGEN_DAYS=14 sudo ./infra/postgres/validate-brewery-sim.sh`
# (or any short brewery sim).
#
# Usage:
#   ./infra/verify-smoke.sh
#
# Exit codes:
#   0 — every motion produced at least one row
#   1 — one or more motions produced zero rows

set -uo pipefail

RESET="$(printf '\033[0m')"
GREEN="$(printf '\033[32m')"
RED="$(printf '\033[31m')"
BOLD="$(printf '\033[1m')"

failures=0

ok()      { printf "  %sPASS%s %s\n" "$GREEN" "$RESET" "$1"; }
fail()    { printf "  %sFAIL%s %s\n" "$RED" "$RESET" "$1"; failures=$((failures + 1)); }
section() { printf "\n%s%s%s\n" "$BOLD" "$1" "$RESET"; }

# x-boss-user header — endpoints policy-gate, anonymous curl
# would land as `guest` which only has Workflow read.
BOSS_USER='{"id":"verify-smoke","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# Smoke check: hit an HTTP API, fail if the count is 0.
expect_at_least_one() {
    local label="$1" url="$2" jq_expr="$3"
    local body actual
    body=$(curl -sf -m 5 -H "x-boss-user: $BOSS_USER" "$url" 2>/dev/null)
    if [[ -z "$body" ]]; then
        fail "$label: query failed ($url)"
        return
    fi
    actual=$(printf '%s' "$body" | jq -r "$jq_expr" 2>/dev/null)
    if [[ -z "$actual" ]]; then
        fail "$label: parse failed ($url)"
        return
    fi
    if (( actual >= 1 )); then
        ok "$label: $actual"
    else
        fail "$label: 0 rows (operating motion did not fire)"
    fi
}

section "Reference data (seeded at first boot)"
expect_at_least_one "kb models"   "http://127.0.0.1:7750/api/catalog/models"           "length"
expect_at_least_one "employees"   "http://127.0.0.1:7500/api/people"                   'if type == "array" then length else (.total // 0) end'
expect_at_least_one "accounts"    "http://127.0.0.1:7500/api/people/accounts"          'if type == "array" then length else (.total // 0) end'
expect_at_least_one "vendors"     "http://127.0.0.1:7300/api/inventory/vendors"        "length"

section "Coordination primitives (every brewery sim emits these)"
expect_at_least_one "Workflows"    "http://127.0.0.1:7900/api/workflows"               "length"
expect_at_least_one "jobs"        "http://127.0.0.1:7900/api/jobs?limit=1"             ".total // 0"
expect_at_least_one "assets" "http://127.0.0.1:7600/api/assets?limit=1"  ".total // 0"

section "Operational motions (sales + finance + shipping)"
expect_at_least_one "invoices"            "http://127.0.0.1:7400/api/commerce/invoices?limit=1"      ".total // 0"
expect_at_least_one "shipments"           "http://127.0.0.1:7100/api/shipping/shipments?limit=1"     ".total // 0"
expect_at_least_one "purchase orders"     "http://127.0.0.1:7300/api/inventory/purchase-orders?limit=1" ".total // 0"

section "Audit + ledger backbone"
expect_at_least_one "policy rules"    "http://127.0.0.1:7250/api/policy/rules"                  "length"
expect_at_least_one "ledger summary"  "http://127.0.0.1:7080/api/ledger/summary"                'if . == null or . == false or . == 0 or . == "" or . == {} or . == [] then 0 else 1 end'
expect_at_least_one "messages"        "http://127.0.0.1:7200/api/messages?limit=1"              ".total // 0"

section "Summary"

if (( failures == 0 )); then
    printf "\n%sOK%s every operating motion fired at least once\n" "$GREEN" "$RESET"
    exit 0
else
    printf "\n%sFAIL%s %d operating motion(s) did not fire\n" "$RED" "$RESET" "$failures"
    exit 1
fi
