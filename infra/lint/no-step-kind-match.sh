#!/usr/bin/env bash
# no-step-kind-match — no core code may match on a step-kind name
# (docs/architecture-decisions.md §Step types are property bundles).
# Code dispatches on properties and mechanism enums; kind names are
# data (registry rows + rule topics + seeds).
#
# Enforcement shape mirrors infra/lint/tier-import-audit.sh: grep,
# explicit allow-list, non-zero exit on anything new. The allow-list
# is a RATCHET — today it holds only the two permanent platform
# pins; remove an entry in the same change that converts its site.
# Never add an entry without a decision recorded in
# docs/architecture-decisions.md justifying it.
#
# Scope: crates/core, crates/modules, apps/web/src. Tests and seeds
# are exempt (tests pin fixtures; seeds ARE the data).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# The kind vocabulary, read live from the registry seed so the lint
# tracks the catalog without a second hand-maintained list.
KINDS=$(grep '^kind = ' crates/core/boss-jobs/seeds/step_types.toml \
    | sed 's/kind = "\(.*\)"/\1/' | paste -sd'|' -)

PATTERN="kind[)]?[[:space:]]*(==|===|!=|!==)[[:space:]]*[\"'](${KINDS})[\"']|matches!\([^,]*kind[^,]*,[[:space:]]*\"(${KINDS})\"|WHERE[[:space:]]+s\.kind[[:space:]]*=[[:space:]]*'(${KINDS})'"

# Allow-list: file => max permitted match count.
#   - boss-jobs http/steps.rs / DesignReviewPage: the two platform-
#     pinned rows (job-kind-publish, review-design) — permanent pins.
#     (steps.rs is where job-kind-publish landed after http.rs was
#     split into the http/ module directory.)
#   - apps/web/src/debug/: dev-only demo driver, not a core surface.
declare -A ALLOW=(
    ["crates/core/boss-jobs/src/http/steps.rs"]=1
    ["apps/web/src/it/design/DesignReviewPage.svelte"]=1
)

declare -A counts files
fail=0
while IFS= read -r line; do
    f="${line%%:*}"
    case "$f" in
        */tests/*|*/debug/*) continue ;;
    esac
    counts["$f"]=$(( ${counts["$f"]:-0} + 1 ))
    files["$f"]="${files[$f]:-}"$'\n'"  $line"
done < <(grep -rnE "$PATTERN" crates/core crates/modules apps/web/src \
    --include="*.rs" --include="*.svelte" --include="*.ts" 2>/dev/null \
    | grep -v 'surfaceOf(' || true)
# surfaceOf(kind) === '<id>' compares SURFACE ids (registry data),
# not kind names — the ids share spellings with the kinds they ship
# for, so the call shape is excluded explicitly.

printf '%s\n' "[no-step-kind-match] kinds tracked: $(grep -c '^kind = ' crates/core/boss-jobs/seeds/step_types.toml)"
for f in "${!counts[@]}"; do
    allowed="${ALLOW[$f]:-0}"
    if (( counts[$f] > allowed )); then
        echo "FAIL $f — ${counts[$f]} kind-name match(es), allow-list permits $allowed:"
        echo "${files[$f]}"
        fail=1
    fi
done

if (( fail )); then
    echo
    echo "Step-kind names are data (architecture-decisions.md, the"
    echo "step-types-are-property-bundles section). Dispatch on a property"
    echo "(executor, completion authority, a metadata field, ux id) or"
    echo "move the behavior to a rule/registry row. Do not grow the"
    echo "allow-list without a recorded decision."
    exit 1
fi
echo "ok: no unapproved step-kind matches"
