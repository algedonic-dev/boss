#!/bin/bash
# Post-replay verification — runs after `boss-rebuild-all` (or the
# regen script's step 8) to confirm the projection rebuild landed
# clean data via the live HTTP APIs.
#
# Tenant-shaped: this script's brewery floor values are calibrated
# for a 365-day Algedonic Ales sim. A shorter run (e.g. the 14-day
# `BOSS_REGEN_DAYS=14` dry-run) will fail the floor checks — drop
# the `--strict` flag to skip the floors and keep just the
# tenant-neutral health gate.
#
# Per the CLAUDE.md / memory `feedback_no_raw_sql_for_diagnostics`:
# this script uses the HTTP APIs + `boss` CLI only. No raw psql.
#
# Usage:
#   ./infra/verify-replay.sh           # tenant-neutral health + brewery floors
#   ./infra/verify-replay.sh --loose   # skip brewery floors (for short sims / non-brewery tenants)
#
# Exit codes:
#   0 — all checks passed
#   1 — one or more checks failed (details printed above)

set -uo pipefail

STRICT=1
for arg in "$@"; do
    case "$arg" in
        --loose|--no-strict) STRICT=0 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

RESET="$(printf '\033[0m')"
GREEN="$(printf '\033[32m')"
RED="$(printf '\033[31m')"
YELLOW="$(printf '\033[33m')"
BOLD="$(printf '\033[1m')"

failures=0

ok()      { printf "  %sPASS%s %s\n" "$GREEN" "$RESET" "$1"; }
fail()    { printf "  %sFAIL%s %s\n" "$RED" "$RESET" "$1"; failures=$((failures + 1)); }
warn()    { printf "  %sWARN%s %s\n" "$YELLOW" "$RESET" "$1"; }
section() { printf "\n%s%s%s\n" "$BOLD" "$1" "$RESET"; }

# x-boss-user header attaching the deploy-time platform-admin
# identity. Required because the API endpoints policy-gate; an
# anonymous curl gets `guest` which can only read JobKinds.
BOSS_USER='{"id":"verify-replay","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# Per-tenant HTTP API endpoint check. Reads the live API, extracts a
# count via the python expression, fails if the count is below floor.
# Floors are brewery-scale (365-day sim, ~700-person roster, ~50
# accounts, ~10 vendors, ~600K audit_log rows).
check_api_count() {
    local name="$1" url="$2" jq_expr="$3" floor="$4"
    local body actual
    body=$(curl -sf -m 5 -H "x-boss-user: $BOSS_USER" "$url" 2>/dev/null)
    if [[ -z "$body" ]]; then
        fail "$name: query failed ($url)"
        return
    fi
    actual=$(printf '%s' "$body" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    print($jq_expr)
except Exception:
    print('')
" 2>/dev/null)
    if [[ -z "$actual" ]]; then
        fail "$name: parse failed ($url)"
        return
    fi
    if (( actual >= floor )); then
        ok "$name: $actual (>= $floor)"
    else
        fail "$name: $actual (expected >= $floor)"
    fi
}

# ---------------------------------------------------------------------------
section "1. Service health (tenant-neutral)"

if ! command -v boss >/dev/null 2>&1; then
    warn "boss CLI not on PATH — skipping service-health gate"
else
    status_json=$(boss status --json 2>/dev/null || echo "")
    if [[ -z "$status_json" ]]; then
        fail "boss status --json returned nothing"
    else
        bad=$(echo "$status_json" | python3 -c "
import sys, json
d = json.load(sys.stdin)
bad = [s for s in d.get('services', []) if s.get('systemd') != 'active' or s.get('health') != 'ok']
print('\n'.join(f\"{s['name']}: systemd={s.get('systemd')} health={s.get('health')}\" for s in bad))
" 2>/dev/null || echo "parse-error")
        if [[ -z "$bad" ]]; then
            ok "every service is active + healthy"
        else
            while IFS= read -r line; do
                fail "service degraded: $line"
            done <<< "$bad"
        fi
    fi
fi

# ---------------------------------------------------------------------------
section "2. Core API endpoints return projected data"

# These are the HTTP surfaces that should respond 200 + carry at
# least one projected row after rebuild. Tenant-neutral floors
# (1 row each) so any deployment with seeded data passes.
check_api_count "kb models"        "http://127.0.0.1:7750/api/catalog/models"           "len(d)"     1
check_api_count "employees"        "http://127.0.0.1:7500/api/people"                   "len(d) if isinstance(d, list) else d.get('total', 0)" 1
# NB: /api/people/accounts is served by accounts-api (7550), NOT
# people-api (7500) — 7500 reads "accounts" as an employee id and
# 404s, which failed this gate on every box after the accounts split.
check_api_count "accounts"         "http://127.0.0.1:7550/api/people/accounts"          "len(d) if isinstance(d, list) else d.get('total', 0)" 1
check_api_count "vendors"          "http://127.0.0.1:7300/api/inventory/vendors"        "len(d)"     1
check_api_count "assets"    "http://127.0.0.1:7600/api/assets?limit=1"    "d.get('total', 0)"      1
check_api_count "jobs"             "http://127.0.0.1:7900/api/jobs?limit=1"             "d.get('total', 0)"      1
check_api_count "JobKinds"         "http://127.0.0.1:7900/api/jobs/kinds"               "len(d)"     1

# ---------------------------------------------------------------------------
if (( STRICT )); then
    section "3. Brewery 365-day sim floors (Algedonic Ales)"
    # Calibrated against the 2026-07-06 365d regen baseline:
    #   410 employees (the slimmed seed roster) · ~50 accounts ·
    #   ~20 vendors · 25,675 jobs · 22,704 invoices.
    # Floors set well below the baseline so a healthy regen clears
    # them with margin; a regression that drops the count by an
    # order of magnitude trips the gate.
    check_api_count "brewery employees"    "http://127.0.0.1:7500/api/people"                  "len(d) if isinstance(d, list) else d.get('total', 0)"  300
    check_api_count "brewery accounts"     "http://127.0.0.1:7550/api/people/accounts"         "len(d) if isinstance(d, list) else d.get('total', 0)"   30
    check_api_count "brewery vendors"      "http://127.0.0.1:7300/api/inventory/vendors"        "len(d)"               5
    check_api_count "brewery jobs"         "http://127.0.0.1:7900/api/jobs?limit=1"             "d.get('total', 0)"  500
    check_api_count "brewery invoices"     "http://127.0.0.1:7400/api/commerce/invoices?limit=1" "d.get('total', 0)" 100

    # COGS floor + gross-margin sanity band. The 2026-07-10 year run
    # passed EVERY structural gate (conservation exact, determinism,
    # deep replay 0/0) with COGS = $0 and GM = 100%: the consume rule's
    # events weren't captured by the JetStream stream, so the flow was
    # dead air — no errors, no dead-letters, nothing moved on either
    # side of any invariant. Conservation cannot see a flow that never
    # fires; only a magnitude floor can. Floors sit far below the
    # healthy year (~$2M COGS, ~77% GM) so honest model drift clears
    # them while a dead flow trips loudly.
    IS_JSON=$(curl -sf -m 10 -H "x-boss-user: $BOSS_USER"         "http://127.0.0.1:7080/api/ledger/income-statement?from=2025-04-01&to=2026-03-31" 2>/dev/null || echo "")
    if [[ -z "$IS_JSON" ]]; then
        fail "income statement unreachable (ledger 7080)"
    else
        COGS_CENTS=$(printf '%s' "$IS_JSON" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(sum(r.get('amount_cents',0) for r in d.get('cogs',[])))" 2>/dev/null || echo 0)
        REV_CENTS=$(printf '%s' "$IS_JSON" | python3 -c "
import json,sys; print(json.load(sys.stdin).get('total_revenue_cents',0))" 2>/dev/null || echo 0)
        if (( COGS_CENTS >= 50000000 )); then
            ok "full-year COGS: \$$((COGS_CENTS / 100)) (>= \$500,000 floor)"
        else
            fail "full-year COGS \$$((COGS_CENTS / 100)) below the \$500,000 floor — a COGS flow is dead (recognition rule not firing?)"
        fi
        if (( REV_CENTS > 0 )); then
            GM_PCT=$(( (REV_CENTS - COGS_CENTS) * 100 / REV_CENTS ))
            if (( GM_PCT >= 40 && GM_PCT <= 95 )); then
                ok "gross margin ${GM_PCT}% (sanity band 40-95%)"
            else
                fail "gross margin ${GM_PCT}% outside the 40-95% sanity band — COGS or revenue flow is broken"
            fi
        fi
    fi
fi

# ---------------------------------------------------------------------------
section "4. Step-plugin bundles are reachable"

# Catches the deploy gap that landed Job-Detail pages with broken
# step surfaces (2026-05-25): the step_plugins registry referenced
# `/plugins/checklist.js` etc., but the install dir
# `/var/lib/boss/step-plugins/` was empty — every plugin URL 404'd.
# Walk the registry, GET each frontend_url through the gateway,
# assert 200.
plugins_json=$(curl -sf -m 5 -H "x-boss-user: $BOSS_USER" \
    "http://127.0.0.1:7900/api/jobs/step-plugins" 2>/dev/null || echo "")
if [[ -z "$plugins_json" ]]; then
    warn "step-plugins registry unreachable — skipping bundle check"
elif [[ ! -d /var/lib/boss/step-plugins ]]; then
    # bootstrap-vm.sh installs the API stack only (no deploy-web.sh),
    # so a headless regen box legitimately has no bundles — that's a
    # deploy shape, not a deploy gap. The FAIL stays for boxes that
    # HAVE the dir but are missing files (the 2026-05-25 class).
    warn "no /var/lib/boss/step-plugins on this box (headless install) — skipping bundle check"
else
    # gateway serves /plugins/* on its public port (4443) — check via
    # localhost loopback to avoid the CF Access redirect.
    while IFS= read -r url; do
        [[ -z "$url" ]] && continue
        status=$(curl -sk -o /dev/null -w '%{http_code}' -m 5 \
            "http://127.0.0.1:4443/plugins/${url}" 2>/dev/null || echo "000")
        if [[ "$status" == "200" ]]; then
            ok "/plugins/${url}: $status"
        else
            fail "/plugins/${url}: $status (bundle missing from /var/lib/boss/step-plugins?)"
        fi
    done < <(printf '%s' "$plugins_json" | python3 -c "
import sys, json
try:
    for p in json.load(sys.stdin):
        if p.get('status') == 'active':
            print(p.get('frontend_url', ''))
except Exception:
    pass
")
fi

# ---------------------------------------------------------------------------
section "5. Perf snapshot (gateway, advisory)"

perf=$(curl -sf -m 3 "http://127.0.0.1:4443/api/gateway/perf" 2>/dev/null || echo "")
if [[ -z "$perf" ]]; then
    warn "gateway /api/gateway/perf unreachable — skip"
else
    slow=$(echo "$perf" | python3 -c "
import sys, json
d = json.load(sys.stdin)
for e in d.get('endpoints', []):
    if e.get('p95_ms', 0) > 500:
        print(f\"{e.get('method','?')} {e.get('path','?')}: p95={e['p95_ms']:.0f}ms count={e.get('count',0)}\")
" 2>/dev/null || echo "")
    if [[ -z "$slow" ]]; then
        ok "no endpoints with p95 > 500ms in recorded traffic"
    else
        while IFS= read -r line; do
            warn "slow endpoint: $line"
        done <<< "$slow"
    fi
fi

# ---------------------------------------------------------------------------
section "Summary"

if (( failures == 0 )); then
    printf "\n%sOK%s all checks passed\n" "$GREEN" "$RESET"
    exit 0
else
    printf "\n%sFAIL%s %d checks failed\n" "$RED" "$RESET" "$failures"
    exit 1
fi
