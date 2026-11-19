#!/usr/bin/env bash
# a-name-only-diff-lists-the-old-path — a decision that reads `git diff
# --name-only` or `--name-status` says `--no-renames`, so a file MOVED
# across a boundary is listed under BOTH its paths.
#
# WHY. Porcelain `git diff` detects renames by default (diff.renames has
# been on since git 2.9): a file moved from infra/gate-runner/x.sh to
# infra/lint/x.sh prints only the NEW path. Every reader that classes a
# change by path then misses the old one. Measured twice on 2026-11-02:
#   - dock_regate::read_base listed main's changes that way, so a gate
#     input moved OUT of infra/gate-runner/ or infra/lint/lib/ showed
#     only its destination, the car classed `Unchanged`, and it boarded
#     without the re-gate it was owed (item 38fc5084, landed review of
#     car dfe15039, finding 2);
#   - the same blind spot in the K2 car at freshness.rs:536, reproduced
#     by the conductor the same day.
# A rule two cars got wrong in one day is not a rule; this is the
# mechanism (CLAUDE.md §9a). Plumbing is exempt: `diff-tree`,
# `diff-index` and `diff-files` detect no renames unless asked.
#
# WHAT IT CHECKS. Every tracked line under crates/orchestrators/boss-cli/src
# and infra/ that invokes a diff with --name-only or --name-status must
# carry --no-renames on the same line (or use a plumbing command). A
# whole-line comment is prose, not a decision, and is skipped — the
# argument that counts is the one the shell or Command receives. A read
# that says --find-renames chose renames on purpose and is exempt too.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
# shellcheck source=infra/lint/lib/pattern-scan.sh
. infra/lint/lib/pattern-scan.sh || exit 3

LINT=a-name-only-diff-lists-the-old-path
# A `diff`, `show` or `log` word (shell: `git diff`; Rust: `"diff",`) followed on
# the line by a name-only or name-status flag.
hits=$(pattern_scan '(^|[^-A-Za-z_])(diff|show|log)["'"'"']?[^|&;]*--name-(only|status)' -- 'crates/orchestrators/boss-cli/src' 'infra/') || exit $?
# The decisions among the hits: not a whole-line comment, not plumbing,
# not already saying --no-renames.
bad=$(printf '%s\n' "$hits" | grep -v '^$' \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(#|//)' \
    | grep -vE 'diff-(tree|index|files)' \
    | grep -vE -- '--no-renames|--find-renames' || true)
# Rust spells an argv one literal per line, so the flag and the verb may
# sit lines apart: a second pass over the Rust files reads each
# `"--name-only"` / `"--name-status"` literal against the fifteen lines
# above it and the five below. A `"diff"`, `"log"` or `"show"` literal
# above with no `"--no-renames"` in that window is the same decision,
# split across lines; a plumbing verb there is exempt as above.
multiline=$(git_answer "$LINT" 0 ls-files -- 'crates/orchestrators/boss-cli/src/*.rs' 'crates/orchestrators/boss-cli/src/**/*.rs' ':!infra/lint/a-name-only-diff-lists-the-old-path.sh') || exit $?
multiline_hits=$(while IFS= read -r rs; do
    [ -n "$rs" ] || continue
    awk -v file="$rs" '
        { line[NR] = $0 }
        END {
            for (i = 1; i <= NR; i++) {
                if (line[i] !~ /"--name-(only|status)"/) continue
                if (line[i] ~ /^[[:space:]]*(\/\/|#)/) continue
                verb = 0; safe = 0
                lo = i - 15; if (lo < 1) lo = 1
                hi = i + 5; if (hi > NR) hi = NR
                for (j = lo; j <= hi; j++) {
                    if (line[j] ~ /"(diff|log|show)"/) verb = 1
                    if (line[j] ~ /"(diff-tree|diff-index|diff-files|ls-tree|ls-files|merge-tree)"/) safe = 1
                    if (line[j] ~ /"--(no|find)-renames"/) safe = 1
                }
                if (verb && !safe) printf "%s:%d:%s\n", file, i, line[i]
            }
        }' "$rs"
done <<EOF
$multiline
EOF
)
[ -z "$multiline_hits" ] || bad="${bad:+$bad
}$multiline_hits"
if [ -n "$bad" ]; then
    echo "$LINT: a name-only diff hides a moved file's old path — say --no-renames (or use diff-tree/diff-index/diff-files, which detect none):" >&2
    printf '%s\n' "$bad" | sed 's/^/    /' >&2
    exit 1
fi
echo "$LINT: ok — every name-only diff under boss-cli and infra lists a moved file under both its paths"
