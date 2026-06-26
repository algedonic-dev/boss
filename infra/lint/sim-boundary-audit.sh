#!/usr/bin/env bash
# Sim/system boundary audit.
#
# Rule: code that runs OUTSIDE the system (the simulator, tenant
# engine drivers, seed loaders) must interact with the system
# only through its public HTTP API. That means a sim-side crate's
# Cargo.toml may not declare a path dep on a system-side
# implementation crate (boss-{accounts,commerce,inventory,...})
# — those crates bundle the Postgres adapter, the HTTP handlers,
# and internal types in one binary surface. Importing them lets
# the simulator reach behind the API and write directly to the
# domain's state, defeating the whole point of running the
# simulator AS a pressure test of the API.
#
# What's allowed:
#   - boss-{domain}-client crates (HTTP contract types + thin client)
#   - boss-core, boss-ports, boss-nats, boss-jobs (shared primitives
#     + the Job/Step registry the sim necessarily speaks)
#   - boss-clock-client, boss-policy-client (cross-cutting client deps)
#   - any non-boss-* dep
#
# What's banned:
#   - boss-{accounts, assets, commerce, inventory, ledger, messages,
#           ml-plugins, people, products, shipping, catalog}
#     — the implementation crates in crates/modules/
#
# Allowlist below grandfathers in the 6 current violators discovered
# at v1.0.10 audit time. Migrating each off the allowlist is the
# follow-up slice — moves domain types the sim needs into the
# matching *-client crate (creating one when none exists yet) and
# switches the dep to *-client. The lint fails the build the moment
# a NEW sim-side dep on an impl crate lands, so we can't drift further
# while we're paying off the existing six.
#
# Wire into CI alongside tier-import-audit.sh. Exit 0 = clean,
# 1 = unexpected violation (allowlisted entries don't count).

set -euo pipefail

cd "$(dirname "$0")/../.."

# Sim-side crates: anything that drives the system from outside.
# Find them by location convention:
#   - crates/orchestrators/boss-sim         (the simulator core)
#   - crates/tenants/*-engine               (every tenant engine)
# Seed loader binaries live inside their parent impl crate today
# (boss_operator_baseline_seed.rs is in boss-people, etc.) — those
# are tracked in the seed-loader-boundary follow-up; this lint covers
# the cleanly-separable sim-side crates only.
SIM_SIDE_CRATES=()
for path in \
    crates/orchestrators/boss-sim \
    crates/tenants/boss-brewery-engine \
    crates/tenants/boss-used-device-shop-engine
do
    [[ -f "$path/Cargo.toml" ]] && SIM_SIDE_CRATES+=("$path")
done

# Banned impl crates. Mechanically: every directory under
# crates/modules/ that doesn't end in -client.
BANNED_CRATES=()
for path in crates/modules/*/; do
    name=$(basename "$path")
    [[ "$name" == *-client ]] && continue
    BANNED_CRATES+=("$name")
done

# Direct database drivers. The impl-crate ban above stops the Postgres
# ADAPTERS; this stops a sim-side crate from declaring a raw DB driver
# (e.g. sqlx) that would let it reach behind the public API straight
# into the database. No allowlist — direct DB access from the sim is
# never acceptable. (The simulator only ever touches the system through
# the public HTTP API.)
BANNED_DB_DRIVERS=("sqlx" "tokio-postgres" "deadpool-postgres" "diesel" "postgres" "rusqlite")

# Direct message-bus / event-store crates. Same rule as the DB drivers:
# a sim-side crate pulling boss-nats (the bus) or boss-events (the event
# store) could publish or persist behind the public API. The sim emits
# only through the public HTTP API (LiveApiOutput -> POST /api/...).
BANNED_BUS_CRATES=("boss-nats" "boss-events")

# Allowlist of CURRENT violators. Each entry is
# `<sim-crate-path>::<banned-dep-name>`. The lint succeeds when a
# violation appears here, fails when it doesn't. Empty the list
# once the migration is complete.
ALLOWLIST=(
    # boss-sim — the simulator core.
    "crates/orchestrators/boss-sim::boss-assets"
    "crates/orchestrators/boss-sim::boss-catalog"
    "crates/orchestrators/boss-sim::boss-commerce"
    "crates/orchestrators/boss-sim::boss-shipping"
    # boss-brewery-engine — Algedonic Ales tenant engine.
    "crates/tenants/boss-brewery-engine::boss-accounts"
    "crates/tenants/boss-brewery-engine::boss-inventory"
)

is_allowlisted() {
    local key="$1"
    for entry in "${ALLOWLIST[@]}"; do
        [[ "$entry" == "$key" ]] && return 0
    done
    return 1
}

violations=0
unexpected_violations=()
allowlisted_seen=0
total_deps_audited=0

for crate_path in "${SIM_SIDE_CRATES[@]}"; do
    toml="$crate_path/Cargo.toml"
    crate_name=$(basename "$crate_path")
    while IFS= read -r dep; do
        [[ -z "$dep" ]] && continue
        total_deps_audited=$((total_deps_audited + 1))
        for banned in "${BANNED_CRATES[@]}"; do
            if [[ "$dep" == "$banned" ]]; then
                key="$crate_path::$banned"
                if is_allowlisted "$key"; then
                    allowlisted_seen=$((allowlisted_seen + 1))
                else
                    unexpected_violations+=("$crate_name → $banned (in $toml)")
                    violations=$((violations + 1))
                fi
                break
            fi
        done
        for db in "${BANNED_DB_DRIVERS[@]}"; do
            if [[ "$dep" == "$db" ]]; then
                unexpected_violations+=("$crate_name → $dep (direct DB driver — the sim must use the public API)")
                violations=$((violations + 1))
                break
            fi
        done
        for bus in "${BANNED_BUS_CRATES[@]}"; do
            if [[ "$dep" == "$bus" ]]; then
                unexpected_violations+=("$crate_name → $dep (direct bus/event-store — the sim must use the public API)")
                violations=$((violations + 1))
                break
            fi
        done
    done < <(awk -F= '
        /^\[/  { in_deps = ($0 ~ /^\[(dependencies|dev-dependencies|build-dependencies)/); next }
        in_deps && /^[a-zA-Z0-9_-]+\s*=/ {
            name=$1
            gsub(/[[:space:]]/, "", name)
            print name
        }
    ' "$toml")
done

# Detect stale allowlist entries (allowlisted violation no longer
# present in source). Either the migration moved forward and the
# entry should be removed, or the dep was renamed and the entry is
# misspelled. Either way, surface it.
stale_entries=()
for entry in "${ALLOWLIST[@]}"; do
    IFS='::' read -r crate_path banned <<<"${entry//::/$'\x01'}"
    # The above split is fragile because `::` is the entry separator
    # AND a literal in the entry. Simpler: rsplit on the last `::`.
    crate_path="${entry%::*}"
    banned="${entry##*::}"
    if [[ ! -f "$crate_path/Cargo.toml" ]]; then
        stale_entries+=("$entry (crate path doesn't exist)")
        continue
    fi
    if ! grep -qE "^${banned}\s*=" "$crate_path/Cargo.toml"; then
        stale_entries+=("$entry (dep no longer declared — remove from allowlist)")
    fi
done

# Report.
if [[ ${#unexpected_violations[@]} -gt 0 ]]; then
    echo "sim-boundary-audit: ${#unexpected_violations[@]} unexpected violation(s)"
    echo
    for v in "${unexpected_violations[@]}"; do
        echo "  $v"
    done
    echo
    echo "Sim-side crates must depend on boss-{domain}-client for HTTP contracts,"
    echo "not on the impl crate. If the contract you need isn't in the client crate"
    echo "yet, move it there first; if the client crate doesn't exist, create one."
    echo
    if [[ ${#stale_entries[@]} -gt 0 ]]; then
        echo "Also: ${#stale_entries[@]} stale allowlist entry/entries:"
        for s in "${stale_entries[@]}"; do echo "  $s"; done
        echo
    fi
    exit 1
fi

if [[ ${#stale_entries[@]} -gt 0 ]]; then
    echo "sim-boundary-audit: ${#stale_entries[@]} stale allowlist entry/entries"
    echo
    for s in "${stale_entries[@]}"; do
        echo "  $s"
    done
    echo
    echo "Remove these from ALLOWLIST in $(basename "$0")."
    exit 1
fi

echo "sim-boundary-audit: clean"
echo "  ${#SIM_SIDE_CRATES[@]} sim-side crate(s) audited"
echo "  $total_deps_audited dep declarations scanned"
echo "  ${#ALLOWLIST[@]} allowlisted violator(s) (migrate to *-client to retire)"
exit 0
