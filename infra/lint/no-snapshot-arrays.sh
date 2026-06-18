#!/usr/bin/env bash
# Lint against the "snapshot, not projection" anti-pattern that
# repeatedly bit the SPA — hand-maintained arrays of facts that
# silently drift from their real source of truth (boss-ports::lib
# for the service/port map, the brewery seed for employees, etc.).
#
# Checks:
#
#   1. `apps/web/src/_generated/ports.ts` is present and lists the
#      same service names as `boss-ports-list --json`. Catches the
#      case where someone committed without re-running the SPA
#      build, or edited the generated file by hand.
#
#   2. The two prior known-bad SPA files (`dev-server.ts` and
#      `it/monitoring/MonitoringPage.svelte`) don't reintroduce a
#      hand-maintained service list. Both must import from
#      `_generated/ports`; the lint just greps for the import.
#
# Usage: infra/lint/no-snapshot-arrays.sh
#
# Exit codes: 0 clean / 1 drift.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED="$(printf '\033[31m')"
GREEN="$(printf '\033[32m')"
RESET="$(printf '\033[0m')"

failures=0
fail() { printf "  %sFAIL%s %s\n" "$RED" "$RESET" "$1"; failures=$((failures + 1)); }
ok()   { printf "  %sok%s   %s\n" "$GREEN" "$RESET" "$1"; }

PORTS_TS="apps/web/src/_generated/ports.ts"
PORTS_BIN="${PORTS_BIN:-target/release/boss-ports-list}"
[[ -x "$PORTS_BIN" ]] || PORTS_BIN="$(command -v boss-ports-list 2>/dev/null)"

# --- 1. ports.ts matches the Rust source -------------------------------

printf '\n%s\n' "[1/2] _generated/ports.ts ↔ boss-ports::lib"

if [[ ! -f "$PORTS_TS" ]]; then
    fail "$PORTS_TS missing — run \`bun run build\` in apps/web/"
elif [[ -z "$PORTS_BIN" || ! -x "$PORTS_BIN" ]]; then
    fail "boss-ports-list not found (set PORTS_BIN= or build the workspace)"
else
    # Compare the SET of service names. Order + scratch ports are
    # checked in boss-ports-list's own unit tests; this lint just
    # asserts that the two sources agree on which services exist.
    expected_names="$("$PORTS_BIN" --json |
        python3 -c 'import sys,json; print("\n".join(sorted(s["name"] for s in json.load(sys.stdin))))')"
    actual_names="$(grep -oE '"name":\s*"[a-z-]+"' "$PORTS_TS" |
        sed -E 's/.*"name":\s*"([a-z-]+)".*/\1/' | sort -u)"
    if [[ "$expected_names" == "$actual_names" ]]; then
        ok "$PORTS_TS lists the same $(echo "$expected_names" | wc -l) services as boss-ports-list"
    else
        fail "service-name set diverges:"
        diff <(echo "$expected_names") <(echo "$actual_names") |
            sed 's/^/         /'
        echo "       Fix: \`cd apps/web && bun run build\` regenerates from boss-ports-list."
    fi
fi

# --- 2. dev-server.ts + MonitoringPage.svelte import from _generated ---

printf '\n%s\n' "[2/2] known service-list consumers import from _generated/ports"

for f in \
    apps/web/src/dev-server.ts \
    apps/web/src/it/monitoring/MonitoringPage.svelte
do
    if [[ ! -f "$f" ]]; then
        fail "$f missing (expected to exist)"
        continue
    fi
    if grep -qE "from '[./]*_generated/ports'" "$f"; then
        ok "$f imports from _generated/ports"
    else
        fail "$f does NOT import _generated/ports — likely reintroduced a hand-maintained service list"
    fi
done

# --- summary -----------------------------------------------------------

printf '\n'
if (( failures == 0 )); then
    printf '%sok%s no snapshot-array drift detected\n' "$GREEN" "$RESET"
    exit 0
else
    printf '%sFAIL%s %d snapshot-array drift issue(s) found\n' "$RED" "$RESET" "$failures"
    exit 1
fi
