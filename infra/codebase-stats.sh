#!/usr/bin/env bash
# Snapshot lines-of-code across the BOSS repo, grouped by the
# five-tier crate organization and by language surface.
#
# Run from repo root. No deps beyond bash + find + wc + python3.
#
# Used to regenerate the "By the numbers" block in README.md
# whenever the proportions shift meaningfully. Treat the README
# numbers as a snapshot, not a CI invariant — drift is expected
# as the codebase evolves; the *ratio* is the load-bearing
# claim ("Tier 1 ≈ Tier 2 in size; tenant code is tiny").
set -euo pipefail

cd "$(dirname "$0")/.."

count_rust() {
    local dir="$1"
    find "$dir" -name "*.rs" -not -path "*/target/*" -print0 |
        xargs -0 wc -l 2>/dev/null |
        awk '/total$/ { print $1; found=1 } END { if (!found) print 0 }'
}

count_files() {
    local dir="$1"; local glob="$2"
    find "$dir" -name "$glob" \
        -not -path "*/target/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/dist/*" \
        -print 2>/dev/null | wc -l
}

count_lines() {
    local dir="$1"; local glob="$2"
    find "$dir" -name "$glob" \
        -not -path "*/target/*" \
        -not -path "*/node_modules/*" \
        -not -path "*/dist/*" \
        -print0 2>/dev/null |
        xargs -0 wc -l 2>/dev/null |
        awk '/total$/ { print $1; found=1 } END { if (!found) print 0 }'
}

crate_count() {
    local dir="$1"
    find "$dir" -maxdepth 1 -mindepth 1 -type d 2>/dev/null | wc -l
}

printf '%-48s %8s %10s\n' 'Tier (Rust)' 'Crates' 'Lines'
printf -- '%.0s-' {1..70}; printf '\n'

declare -a tiers=(
    "crates/core|Tier 1: state-machine OS"
    "crates/modules|Tier 2: company-modeling layer"
    "crates/orchestrators|cross-tier orchestrators"
    "crates/bridges|sim bridges"
    "crates/tenants|Tier 3: tenants"
)
total_crates=0
total_lines=0
for entry in "${tiers[@]}"; do
    path="${entry%%|*}"
    label="${entry##*|}"
    n=$(crate_count "$path")
    l=$(count_rust "$path")
    printf '%-48s %8d %10s\n' "$label" "$n" "$(printf "%'d" "$l")"
    total_crates=$((total_crates + n))
    total_lines=$((total_lines + l))
done
printf -- '%.0s-' {1..70}; printf '\n'
printf '%-48s %8d %10s\n' 'TOTAL (Rust)' "$total_crates" "$(printf "%'d" "$total_lines")"

printf '\n%-48s %8s %10s\n' 'Non-Rust surface' 'Files' 'Lines'
printf -- '%.0s-' {1..70}; printf '\n'

declare -a surfaces=(
    "apps/web/src|*.svelte|Web app (Svelte)"
    "apps/web/src|*.ts|Web app (TypeScript)"
    "infra/step-plugins|*.js|Step plugins (JS)"
    "docs/formal|*.tla|Formal specs (TLA+)"
    "docs|*.md|Design + runbook docs (md)"
    "infra|*.sql|Infra SQL"
    "infra|*.sh|Infra shell"
    "examples|*.toml|Tenant TOML seeds"
    "examples|*.json|Tenant JSON data"
)
for entry in "${surfaces[@]}"; do
    path="${entry%%|*}"
    rest="${entry#*|}"
    glob="${rest%%|*}"
    label="${rest##*|}"
    n=$(count_files "$path" "$glob")
    l=$(count_lines "$path" "$glob")
    printf '%-48s %8d %10s\n' "$label" "$n" "$(printf "%'d" "$l")"
done
