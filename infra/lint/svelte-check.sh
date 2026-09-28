#!/usr/bin/env bash
# consist: skip — bun install plus a typecheck: minutes and a network fetch, and it exits 1 on a box without bun; the gate runs it in its web phase
#
# svelte-check over apps/web — the type gate for the frontend.
#
# WHY THIS EXISTS. Until 2026-08-13 infra/gate.sh ran fifteen Rust
# checks and zero frontend ones, so every .svelte and .ts file in
# apps/web reached main with its types unchecked by CI. The trigger was
# a web-only car (the KB page) whose only type verification was an
# svelte-check I happened to run by hand; nothing in the pipeline would
# have caught a mistake. A 53k-line frontend with no type gate is not a
# smaller risk than a workspace with fifteen.
#
# WHAT IT RUNS. `bun run typecheck` in apps/web, which is
# `svelte-check --tsconfig ./tsconfig.json`. Exits 0 on warnings and
# non-zero on errors — verified both directions in the CI image before
# this was wired in: the clean tree reports "0 errors and 63 warnings"
# and exits 0, and a planted `const n: number = "not a number"` reports
# "1 error" and exits 1. A check that cannot go red is decoration.
#
# WARNINGS ARE A RATCHET, not a flag (backlog 68056e4e, 2026-09-27).
# The 63 warnings were deliberately not fatal when this was wired in —
# failing on them would have meant fixing 63 things before the gate
# could be turned on at all. But nothing read them either, and by
# 2026-09-24 there were 74 in 28 files: a check nobody reads is a check
# that is not running (CLAUDE.md §Diagnosis). So every file is now held
# to its count in apps/web/svelte-check-warnings.txt (`<count>\t<path>`,
# the path as svelte-check prints it, relative to apps/web). A file with
# MORE warnings than its line fails the check, naming the file and both
# counts; a file the baseline does not name has a baseline of 0. FEWER
# passes, and the check names the line to lower — a burn-down car edits
# its own file's line and no other, so two of them never collide.
# `--output machine` is what makes the count a reading rather than a
# parse of prose, and its COMPLETED line is checked against the lines
# counted: a run whose record the parser did not understand exits 3,
# never "no new warnings".
#
# WHAT IT READS (backlog c78e8dbb, 2026-09-27). apps/web/tsconfig.json
# includes tests/**/*.ts and the Playwright configs, so the ~130 mocked
# specs are type-checked here too. Until then nothing checked them; the
# first run over them found 21 errors in 10 specs.
#
# USAGE
#   infra/lint/svelte-check.sh                           the gate's check
#   infra/lint/svelte-check.sh --judge <output> <baseline>
#       the ratchet alone, over a saved `--output machine` run — what
#       crates/core/boss-testing/tests/svelte_check_sh.rs drives.
#
# EXIT STATUS: 0 clean · 1 errors, or a new warning · 3 the output was
# never read (no COMPLETED line, or a count that disagrees with it).
#
# PUPPETEER_SKIP_DOWNLOAD. boss-web depends on puppeteer, whose
# postinstall downloads a browser. In the CI container that download
# fails, and because bun aborts the whole install on a failed
# postinstall, NOTHING gets installed — svelte-check included, which
# then fails with "command not found" and looks like a missing tool
# rather than a failed install. Skipping the download is not a
# workaround for a broken dependency; a type check has no use for a
# browser binary.
#
# bun must exist. It is listed in infra/forge/boss-ci/required-tools.txt
# so the locomotive check names its absence in seconds. Locally, this
# refuses rather than skipping: a check that silently passes when its
# tool is missing reports success while verifying nothing, which is the
# defect class this repo has spent the day removing.
set -uo pipefail

# The ratchet: one `--output machine` run against the baseline. Machine
# lines are `<ts> WARNING "<file>" <line>:<col> "<message>"`, and the run
# ends `<ts> COMPLETED <n> FILES <e> ERRORS <w> WARNINGS <f> FILES_...`.
# Findings go through `sort` so two runs over one tree print the same
# lines in the same order.
judge() { # output baseline
    local out="$1" base="$2"
    if [ ! -r "$out" ]; then
        echo "svelte-check: cannot read the output $out — never read, not clean" >&2
        return 3
    fi
    if [ ! -r "$base" ]; then
        echo "svelte-check: cannot read the baseline $base — never read, not clean" >&2
        return 3
    fi
    awk -F'\t' -v baseline="$base" '
        FNR == NR {
            if ($0 ~ /^#/ || $0 ~ /^[ \t]*$/) next
            base[$2] = $1 + 0
            based += $1
            next
        }
        {
            n = split($0, f, " ")
            if (f[2] == "COMPLETED") {
                completed = 1
                for (i = 3; i <= n; i++) if (f[i] == "WARNINGS") claimed = f[i - 1] + 0
            } else if (f[2] == "WARNING") {
                rest = substr($0, index($0, "\"") + 1)
                cur[substr(rest, 1, index(rest, "\"") - 1)]++
                total++
            }
        }
        END {
            if (!completed) {
                print "svelte-check: the output has no COMPLETED line — the run was never read, not clean" > "/dev/stderr"
                exit 3
            }
            if (claimed != total + 0) {
                printf "svelte-check: the run says %d warnings and %d were counted — its record was not understood, not clean\n", claimed, total > "/dev/stderr"
                exit 3
            }
            bad = 0
            for (file in cur) {
                b = (file in base) ? base[file] : 0
                if (cur[file] > b) {
                    printf "svelte-check: %s: %d warning(s), baseline %d — a NEW warning; its WARNING line is above, fix it\n", file, cur[file], b | "sort"
                    bad++
                }
            }
            for (file in base) {
                c = (file in cur) ? cur[file] : 0
                if (c < base[file]) {
                    how = c == 0 ? "delete its line" : "set its line to " c
                    printf "svelte-check: %s: %d warning(s), baseline %d — fewer; lower %s: %s\n", file, c, base[file], baseline, how | "sort"
                }
            }
            close("sort")
            printf "svelte-check: %d warning(s) against a baseline of %d; %d file(s) gained one\n", total, based, bad
            exit bad ? 1 : 0
        }
    ' "$base" "$out"
}

if [ "${1:-}" = "--judge" ]; then
    if [ "$#" -ne 3 ]; then
        echo "usage: $0 --judge <svelte-check --output machine file> <baseline>" >&2
        exit 2
    fi
    judge "$2" "$3"
    exit $?
fi

cd "$(dirname "$0")/../.." || exit 1

if ! command -v bun >/dev/null 2>&1; then
    echo "svelte-check: bun not found — install it (https://bun.sh) or run the gate in the CI image." >&2
    echo "              Refusing to skip: a check that passes without running verifies nothing." >&2
    exit 1
fi

# The install log goes to a directory THIS RUN OWNS. It used to be a
# fixed /tmp/boss-bun-install.log, which is fine on a single-user box and
# wrong on the long-lived dev pod: the second uid to run the lint roster
# cannot write the first uid's file, so the redirect fails, the `if !`
# branch fires, and a working tree is reported as "bun install failed"
# with a log it also cannot read. Measured in the sibling case on
# 2026-09-11 (packet 5bf96e72).
log_dir="$(mktemp -d)"
trap 'rm -rf "$log_dir"' EXIT
log="$log_dir/bun-install.log"

# --frozen-lockfile so CI cannot silently resolve a different tree than
# the lockfile records.
if ! PUPPETEER_SKIP_DOWNLOAD=1 bun install --frozen-lockfile >"$log" 2>&1; then
    echo "svelte-check: bun install failed" >&2
    # ALL of it, not a tail: the log dies with this run, so a reduction
    # here throws away the only copy (CLAUDE.md §Diagnosis).
    cat "$log" >&2
    exit 1
fi

cd apps/web || exit 1
# Captured, then printed WHOLE before any verdict: the judge reads the
# file, the reader of the gate log reads every line svelte-check wrote.
out="$log_dir/svelte-check.out"
bun run typecheck --output machine >"$out" 2>&1
rc=$?
cat "$out"
if [ "$rc" -ne 0 ]; then
    echo "svelte-check: exit $rc — each ERROR line above is one to fix" >&2
    exit 1
fi
judge "$out" svelte-check-warnings.txt
