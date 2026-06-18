#!/usr/bin/env bash
#
# Seed bypass-smell lint — enforces the principle in
# `docs/design/seed-vs-emergent-state.md`: seed files set initial
# conditions only, never downstream artifacts that should emerge
# from a Job step.done.
#
# This lint scans every `examples/<tenant>/seeds/sql/*.sql` file
# for `INSERT INTO <denylist>` statements. Hits are reported with
# file + line number + matched table; any hit returns a non-zero
# exit code. The protocol-derivation lives in
# `docs/design/correctness-protocol.md`; the CI hook lives in
# `.github/workflows/ci.yml`.
#
# Existing files known to violate the principle are listed in
# the ALLOWLIST below as known stop-gaps tied to TODO entries.
# New seed files MUST pass without an allowlist entry.
#
# Usage:
#   infra/lint/seed-bypass-smell.sh [--strict]
#
# In strict mode, the allowlist is ignored — useful when
# clearing a stop-gap to verify it's actually clean.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Tables whose rows should only come from a Job step.done +
# matching SideEffectHandler. Any seed-side INSERT INTO against
# these tables is bypass smell.
DENYLIST=(
    "invoices"
    "invoice_line_items"
    "gl_journal_entries"
    "gl_journal_lines"
    "financial_facts"
    "vendor_invoices"
    "payroll_runs"
    "payroll_run_lines"
    "shipments"
    "tax_filings"
)

# Known stop-gaps. Each entry is a file path *relative to the
# repo root*. Removing an entry should cause the lint to flag
# the violation — the workflow is: complete the matching
# JobKind protocol-proof, delete the stop-gap seed, drop the
# allowlist entry. Tracked under "Job-level financial fact
# production (close the GL gap)" in TODO.
ALLOWLIST=(
    # All previous entries were the playground_*.sql bundles under
    # examples/brewery/seeds/sql/. Those files have been retired
    # — see docs/design/projection-rebuilders.md § F. The bootstrap
    # path is now `--audit-log-seed` + boss-rebuild-all, so any
    # projection content lands via the event log rather than direct
    # INSERTs. The allowlist is intentionally empty so any new
    # direct-INSERT seed files trip the smell check on their first
    # commit.
)

STRICT=0
if [[ "${1:-}" == "--strict" ]]; then
    STRICT=1
fi

cd "$REPO_ROOT"

# Build a single grep -E pattern from the denylist. Match
# `INSERT INTO <table>` case-insensitively, allowing for
# whitespace + comments after the table name.
PATTERN="^[[:space:]]*INSERT[[:space:]]+INTO[[:space:]]+($(IFS='|'; echo "${DENYLIST[*]}"))\\b"

# Find every seed sql file under examples/<tenant>/seeds/sql/.
SEED_FILES=()
while IFS= read -r f; do
    SEED_FILES+=("$f")
done < <(find examples -type f -path '*/seeds/sql/*.sql' | sort)

if [[ ${#SEED_FILES[@]} -eq 0 ]]; then
    echo "seed-bypass-smell: no seed sql files under examples/*/seeds/sql/"
    exit 0
fi

is_allowlisted() {
    local target="$1"
    if (( STRICT )); then
        return 1
    fi
    local entry
    for entry in "${ALLOWLIST[@]}"; do
        if [[ "$target" == "$entry" ]]; then
            return 0
        fi
    done
    return 1
}

violations=0
allowed_with_hits=()

for f in "${SEED_FILES[@]}"; do
    # Pull every matching line + line number. Use grep -i so
    # case quirks don't sneak past.
    matches=$(grep -niE "$PATTERN" "$f" || true)
    if [[ -z "$matches" ]]; then
        continue
    fi

    if is_allowlisted "$f"; then
        allowed_with_hits+=("$f")
        continue
    fi

    echo
    echo "[BYPASS] $f — seed file inserts directly into a projection table:"
    while IFS= read -r line; do
        echo "    $line"
    done <<< "$matches"
    violations=$((violations + 1))
done

echo
if (( violations > 0 )); then
    cat <<'EOF'
seed-bypass-smell: bypass detected.

The flagged seed files insert rows into projection tables that
should come from a Job step.done emitting a financial_fact (or
similar). Per docs/design/seed-vs-emergent-state.md, seeds set
initial conditions only. Fix: extend a JobKind so the simulator
produces this artifact, or accept the empty view until the
model catches up.

If this is a known stop-gap that's been deliberately accepted
for now, add it to the ALLOWLIST in this script with a comment
referencing the matching TODO entry.
EOF
    exit 1
fi

echo "seed-bypass-smell: clean (${#SEED_FILES[@]} seed file(s) checked)."
if (( ${#allowed_with_hits[@]} > 0 )); then
    echo
    echo "  $(echo ${#allowed_with_hits[@]}) known stop-gap(s) skipped via ALLOWLIST:"
    for f in "${allowed_with_hits[@]}"; do
        echo "    $f"
    done
    echo
    echo "  Run with --strict to verify a stop-gap has actually been cleared."
fi
exit 0
