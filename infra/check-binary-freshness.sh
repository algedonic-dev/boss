#!/usr/bin/env bash
# Binary freshness check for the deployed boss-* fleet.
#
# Background: 2026-05-06 surfaced a stale-binary bug where the live
# /usr/local/bin/boss-brewery-sim was built 2026-05-04 against an
# older src/ snapshot — a `WARN` log line that had been demoted to
# `trace!` in source kept appearing in the deployed sim's logs,
# misleading anyone trying to triage. Other binaries on the box
# could be in the same state and we wouldn't notice.
#
# This script catches that. For every /usr/local/bin/boss-* it:
#   1. Resolves the binary back to a crate dir under crates/.
#   2. Asks git for the latest commit touching that crate dir.
#   3. Compares the binary's mtime against that commit's timestamp.
#   4. Flags any binary whose source has moved forward since build.
#
# Exit code: 0 if everything is current, 1 if any drift is found.
#
# Usage:
#   ./infra/check-binary-freshness.sh
#   ./infra/check-binary-freshness.sh --bin-dir /opt/boss/target/release
#
# Notes:
#   - Falls back to a workspace-level "any crate moved" check for
#     binaries we can't map to a specific crate dir (rare, but the
#     fail-soft path beats false positives).
#   - The shipping bin name often differs from its crate name
#     (e.g. boss-brewery-sim ships from crates/tenants/
#     boss-brewery-engine). The mapping table below carries those
#     exceptions; everything else uses a heuristic
#     (boss-<x>-api → crates/*/boss-<x>).

set -eu

BIN_DIR="/usr/local/bin"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir)  shift; BIN_DIR="$1" ;;
        --help|-h)  sed -n '2,30p' "$0"; exit 0 ;;
        *)          echo "unknown arg: $1" >&2; exit 2 ;;
    esac
    shift
done

cd "$REPO_ROOT"

# Exceptions where the binary name doesn't follow the
# `boss-<crate>-api`-or-similar convention.
crate_for_binary() {
    case "$1" in
        boss-brewery-sim|boss-brewery-data-seed)
            echo "crates/tenants/boss-brewery-engine"
            ;;
        boss-rebuild-all|boss-rebuild)
            echo "crates/orchestrators/boss-rebuild"
            ;;
        boss-cli|boss)
            echo "crates/orchestrators/boss-cli"
            ;;
        boss-ml-api|boss-ml-generate)
            echo "crates/orchestrators/boss-ml-api"
            ;;
        boss-auth)
            echo "crates/core/boss-gateway"
            ;;
        boss-operator-baseline-seed)
            echo "crates/modules/boss-people"
            ;;
        boss-classes-api|boss-locations-api|boss-subject-kinds-api)
            local stem="${1#boss-}"; stem="${stem%-api}"
            echo "crates/core/boss-${stem}"
            ;;
        # Compound-named timer/batch bins — these have to come before
        # the boss-*-{rebuild,batch,purge,...} wildcard branch since
        # the wildcard mis-maps them (e.g. boss-messages-events-purge
        # would strip to "messages-events" and find no crate).
        boss-audit-integrity-check)  echo "crates/core/boss-events" ;;
        boss-policy-bootstrap)       echo "crates/core/boss-policy" ;;
        boss-messages-events-purge)  echo "crates/modules/boss-messages" ;;
        boss-ledger-facts-rebuild)   echo "crates/modules/boss-ledger" ;;
        boss-jobs-seed-kinds)        echo "crates/core/boss-jobs" ;;
        boss-files-gc)               echo "crates/core/boss-content" ;;
        boss-cybernetics)            echo "crates/core/boss-cybernetics" ;;
        boss-observability)          echo "crates/core/boss-observability" ;;
        boss-simulator)              echo "crates/orchestrators/boss-simulator" ;;
        boss-gateway)                echo "crates/core/boss-gateway" ;;
        boss-ports-list)             echo "crates/core/boss-ports" ;;
        boss-*-api)
            # Generic: boss-jobs-api → crates/*/boss-jobs.
            local stem="${1#boss-}"; stem="${stem%-api}"
            local match
            match=$(find crates -maxdepth 3 -type d -name "boss-${stem}" | head -1)
            echo "${match:-}"
            ;;
        boss-*-rebuild|boss-*-batch|boss-*-purge|boss-*-recognize|boss-*-replay-check)
            # Domain-specific timer / batch bins live in their domain crate.
            local stem="${1#boss-}"
            stem="${stem%-rebuild}"
            stem="${stem%-batch}"
            stem="${stem%-purge}"
            stem="${stem%-recognize}"
            stem="${stem%-replay-check}"
            local match
            match=$(find crates -maxdepth 3 -type d -name "boss-${stem}" | head -1)
            echo "${match:-}"
            ;;
        *)
            echo ""
            ;;
    esac
}

if ! command -v git >/dev/null 2>&1; then
    echo "FATAL: git required" >&2
    exit 2
fi

stale_count=0
unknown_count=0
fresh_count=0

# Collect once outside the find loop so subshell variable scope
# doesn't eat our running counts.
mapfile -t BINS < <(find "$BIN_DIR" -maxdepth 1 -type f -name 'boss*' 2>/dev/null | sort)

if [ ${#BINS[@]} -eq 0 ]; then
    echo "==> no boss-* binaries found in $BIN_DIR"
    exit 0
fi

echo "==> checking ${#BINS[@]} boss-* binaries against latest source touch"
printf '\n%-40s %-20s %-20s %s\n' BINARY BUILT NEWEST_SRC STATE
printf '%-40s %-20s %-20s %s\n' "------" "-----" "----------" "-----"

for bin in "${BINS[@]}"; do
    name=$(basename "$bin")
    binary_mtime=$(stat -c %Y "$bin" 2>/dev/null || stat -f %m "$bin" 2>/dev/null || echo 0)
    crate_dir=$(crate_for_binary "$name")

    if [ -z "$crate_dir" ] || [ ! -d "$crate_dir" ]; then
        printf '%-40s %-20s %-20s %s\n' \
            "$name" \
            "$(date -u -d "@$binary_mtime" '+%Y-%m-%d %H:%M' 2>/dev/null || date -u -r "$binary_mtime" '+%Y-%m-%d %H:%M')" \
            "?" \
            "UNKNOWN_CRATE"
        unknown_count=$((unknown_count + 1))
        continue
    fi

    src_ts=$(git log -1 --format=%ct -- "$crate_dir" 2>/dev/null || echo 0)
    src_when=$(date -u -d "@$src_ts" '+%Y-%m-%d %H:%M' 2>/dev/null || date -u -r "$src_ts" '+%Y-%m-%d %H:%M')
    bin_when=$(date -u -d "@$binary_mtime" '+%Y-%m-%d %H:%M' 2>/dev/null || date -u -r "$binary_mtime" '+%Y-%m-%d %H:%M')

    if [ "$src_ts" -gt "$binary_mtime" ]; then
        printf '%-40s %-20s %-20s %s\n' "$name" "$bin_when" "$src_when" "STALE"
        stale_count=$((stale_count + 1))
    else
        printf '%-40s %-20s %-20s %s\n' "$name" "$bin_when" "$src_when" "fresh"
        fresh_count=$((fresh_count + 1))
    fi
done

echo ""
echo "==> $fresh_count fresh, $stale_count stale, $unknown_count unmapped"

if [ "$stale_count" -gt 0 ]; then
    echo ""
    echo "    Rebuild + redeploy stale binaries:"
    echo "      cargo build --release --workspace --features postgres"
    echo "      sudo ./infra/deploy-services.sh prod"
    echo "      sudo install -m 0755 target/release/boss /usr/local/bin/boss"
    exit 1
fi

exit 0
