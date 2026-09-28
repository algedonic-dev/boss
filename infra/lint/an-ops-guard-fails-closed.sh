#!/usr/bin/env bash
# an-ops-guard-fails-closed.sh — a host script's guard fails CLOSED: a
# read that failed is never recorded as an empty answer or a plausible
# number, and a MUTATING verb's script runs under nounset and pipefail.
#
# WHY THIS EXISTS (design 3036296f mechanism B, David 2026-09-27; backlog
# fdbb447e). Asked to "crest the hill of reliability", the day's reds and
# review holds were counted, and most were one class: silence that reads
# as success. The daily prune's probe counted a refused run as proof; the
# data move's proof could not tell the old copy from the new; guards
# ended in `|| true`, and failed reads were recorded as a value
# (`|| echo 0`). On the host-mutating verbs (commission-a-disk,
# move-forgejo-data, the unattended prune) the adversarial review caught
# every instance and the gate caught none. A reviewer who has to find the
# same three spellings by eye in every car is the check that stops
# running; this lint is the gate's half of that review.
#
# THE RULE. Over every tracked file under infra/forge/ and infra/ops/ —
# plus every script a MUTATING verb names, WHEREVER it lives (boss-gcp's
# verbs run infra/gcp/*) — three shapes are refused:
#
#   swallowed-read          a variable set from a command substitution
#                           whose failure is swallowed into it:
#                               x=$(cmd || true)     x=$(cmd) || :
#                               x=$(cmd) || x=""
#                           A failed read and an empty answer become the
#                           same value, and the next line's guard reads
#                           an outage as "nothing there".
#   failed-read-as-number   a failed read recorded as a number:
#                               $(cat f || echo 0)   x=$(cmd) || x=0
#                           Worse than empty: 0 is a plausible reading,
#                           so nothing downstream can tell.
#   no-pipefail             a script a MUTATING verb runs (argv[0] of a
#                           verb whose `about` says MUTATING — the same
#                           roster the-controls-are-bounded-verbs derives)
#                           without `set -u` and `set -o pipefail`: a
#                           failed producer in a pipeline is invisible,
#                           and an unset guard variable reads as empty.
#
# THE SAFE TWINS, which pass and which the refusal names:
#   * tolerate the ONE status that means "no answer", refuse the rest —
#         n=$(grep -c . "$f") || [ $? -eq 1 ] || refuse "cannot read $f"
#   * record the failure in its own variable —
#         declared=$(list_them) || unreadable=1
#   * a sentinel no reading can equal (`|| echo none`), or an explicit
#     empty arm the very next line refuses on.
#   * `cmd || true` on a command that sets nothing is not a guard.
#
# WHY `-e` IS NOT REQUIRED. The packet's wording is `set -euo pipefail`;
# the approved design's is "a missing pipefail". Measured on origin/main
# d8393e48: all 22 scripts the MUTATING verbs run carry `-u` and
# pipefail, and 16 of them run `set -uo pipefail` without `-e` and check
# each step by hand (`|| refuse …`). Requiring `-e` would change the
# control flow of 16 mutating scripts in one car, which is exactly the change q2 of that
# design puts behind an adversarial review. It is a follow-up, not this
# lint.
#
# THE ALLOWLIST is infra/lint/an-ops-guard-fails-closed-allow.txt, one
# entry per line, `<rule> <path> <count> <reason>`. The count must EQUAL
# the file's findings under that rule (fixing a site without lowering it
# leaves a hole shaped like it), and the reason is REQUIRED: an allowance
# nobody can read is a skip. An entry naming a missing file, or excusing
# nothing, is refused (lib/allowlist.sh; backlog cdf2d959).
#
# WHAT AN EXACT COUNT COSTS, measured the day it landed. A car that only
# edits an allowlisted script does not move its count, and a car that
# adds or removes a site is told so on its own gate. The cost is a car
# gated BEFORE this lint existed: train #752 removed two ops-runner.sh
# sites green, and this car's dock re-gate went red on the stale 3.
# That window closes once the lint is on main, because every later gate
# runs it.
#
# NOT COVERED, each measured or reasoned rather than missed:
#   * a substitution split across lines (`x=$(\n … || true\n)`) — git
#     grep is line-based.
#   * `$(cmd || true)` used inline in a test rather than assigned — it
#     sets no variable; review. Nor one nested in a parameter default,
#     `x="${Y:-$(cmd || true)}"` (merge-tenant-main.sh:92, whose next
#     line refuses an empty owner).
#   * scripts a MUTATING script sources or calls, when they live outside
#     infra/forge and infra/ops — only argv[0] is on the roster.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  every guard fails closed, or is allowed with its reason
#   1  a finding, or an allowlist entry that is wrong — the author's to fix
#   3  the tree could not be read, or there is no jq to derive the
#      MUTATING roster — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/an-ops-guard-fails-closed.sh
#   infra/lint/an-ops-guard-fails-closed.sh --self-test
set -uo pipefail

NAME="an-ops-guard-fails-closed"
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lint/lib/pattern-scan.sh
. "$LINT_DIR/lib/pattern-scan.sh" || exit 3
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh" || exit 3
cd "$LINT_DIR/../.." || exit 3

JQ="${LINT_JQ:-jq}"
ALLOW_FILE="infra/lint/$NAME-allow.txt"

# --- the classifier ------------------------------------------------------
# One quote class, so each regex below can say "optionally quoted".
Q="[\"']"
# `|| echo 0`, `|| printf '12'` — anywhere on the line: the shape that
# shipped sat inside an arithmetic expansion, not an assignment.
RE_ECHO_NUMBER='\|\|[[:space:]]*(echo|printf)[[:space:]]+(-[A-Za-z]+[[:space:]]+)?'"$Q"'?-?[0-9]+'"$Q"'?([[:space:];)`]|$)'
# An assignment from a command substitution; group 2 is the variable.
RE_ASSIGN='(^|[[:space:];&|(!])([A-Za-z_][A-Za-z_0-9]*)=("?\$\(|`)'
RE_SWALLOW_TRUE='\|\|[[:space:]]*(true|:)([[:space:];)"`]|$)'
# The coarse prefilter git grep runs; classify() decides.
PREFILTER='\|\|[[:space:]]*(true|:|echo|printf|[A-Za-z_][A-Za-z_0-9]*=)'

# classify <code> — sets RULE to the rule the line breaks, or to "".
# A global rather than a substitution: it runs once per prefilter hit.
classify() {
    local code="$1" trimmed v re
    RULE=""
    trimmed="${code#"${code%%[![:space:]]*}"}"
    case "$trimmed" in '#'*) return 0 ;; esac
    if [[ $code =~ $RE_ECHO_NUMBER ]]; then RULE=failed-read-as-number; return 0; fi
    [[ $code =~ $RE_ASSIGN ]] || return 0
    v="${BASH_REMATCH[2]}"
    re='\|\|[[:space:]]*'"$v"'='"$Q"'?-?[0-9]+'"$Q"'?([[:space:];)]|$)'
    if [[ $code =~ $re ]]; then RULE=failed-read-as-number; return 0; fi
    if [[ $code =~ $RE_SWALLOW_TRUE ]]; then RULE=swallowed-read; return 0; fi
    re='\|\|[[:space:]]*'"$v"'=(""|'"''"')?([[:space:];)]|$)'
    if [[ $code =~ $re ]]; then RULE=swallowed-read; return 0; fi
    return 0
}

# strict_mode_gap < script — prints what the script's `set` lines leave
# off (`nounset`, `pipefail`), empty when both are on. A comment is not
# a `set` line.
RE_PIPEFAIL='^[[:space:]]*set[[:space:]]+([^#]*[[:space:]])?-[A-Za-z]*o[[:space:]]+pipefail'
RE_NOUNSET='^[[:space:]]*set[[:space:]]+([^#]*[[:space:]])?(-[A-Za-z]*u[A-Za-z]*|-o[[:space:]]+nounset)([[:space:]]|$)'
strict_mode_gap() {
    local line pf=0 nu=0 gap=""
    while IFS= read -r line || [ -n "$line" ]; do
        [[ $line =~ $RE_PIPEFAIL ]] && pf=1
        [[ $line =~ $RE_NOUNSET ]] && nu=1
    done
    [ "$nu" = 1 ] || gap="nounset"
    [ "$pf" = 1 ] || gap="${gap:+$gap and }pipefail"
    printf '%s' "$gap"
}

# --- self-test -----------------------------------------------------------
# Runs on every invocation: a regex that stopped matching passes every
# file, and only a line it must refuse tells that from a clean tree.
self_test() {
    local line want got
    while IFS='|' read -r want line; do
        [ -n "$line" ] || continue
        classify "$line"
        got="${RULE:-pass}"
        if [ "$got" != "$want" ]; then
            echo "$NAME: self-test FAILED — expected $want, got $got, for: $line" >&2
            return 1
        fi
    done <<'CASES'
swallowed-read|n=$(grep -c . "$f" || true)
swallowed-read|ARGS_LINE="$(grep -m1 '^args = ' "$RULE" || true)"
swallowed-read|v=$(cmd) || :
swallowed-read|    local w; w=$(cmd 2>/dev/null) || w=""
swallowed-read|[ -n "$a" ] && s=$(date -u -d "$a" +%s 2>/dev/null || true)
failed-read-as-number|dark=$(( $(cat "$STATE" 2>/dev/null || echo 0) + 1 ))
failed-read-as-number|n=$(cat f || printf '12')
failed-read-as-number|mtime=$(stat -c %Y "$lock" 2>/dev/null) || mtime=0
pass|n=$(grep -c . "$f") || [ $? -eq 1 ]
pass|rm -f "$lock" || true
pass|last=$(cat "$f" 2>/dev/null || echo none)
pass|declared=$(list_them) || unreadable=1
pass|x=$(cmd) || x=$(fallback)
pass|# z=$(cmd || true) and $(cat f || echo 0), quoted in prose
CASES
    local gap
    gap="$(strict_mode_gap <<<$'#!/usr/bin/env bash\nset -euo pipefail\n')"
    [ -z "$gap" ] || { echo "$NAME: self-test FAILED — set -euo pipefail read as missing $gap" >&2; return 1; }
    gap="$(strict_mode_gap <<<$'#!/usr/bin/env bash\nset -uo pipefail\n')"
    [ -z "$gap" ] || { echo "$NAME: self-test FAILED — set -uo pipefail read as missing $gap" >&2; return 1; }
    gap="$(strict_mode_gap <<<$'#!/usr/bin/env bash\nset -eu\n')"
    [ "$gap" = "pipefail" ] || { echo "$NAME: self-test FAILED — set -eu read as missing '$gap', want pipefail" >&2; return 1; }
    gap="$(strict_mode_gap <<<$'#!/usr/bin/env bash\nset -o pipefail\n')"
    [ "$gap" = "nounset" ] || { echo "$NAME: self-test FAILED — set -o pipefail read as missing '$gap', want nounset" >&2; return 1; }
    gap="$(strict_mode_gap <<<$'#!/usr/bin/env bash\n# set -euo pipefail\n')"
    [ "$gap" = "nounset and pipefail" ] || { echo "$NAME: self-test FAILED — a commented set line counted ('$gap')" >&2; return 1; }
    echo "$NAME: self-test ok — five swallowed reads and three failed-reads-as-numbers refused, six safe twins passed; strict mode read from set lines, never from a comment"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the MUTATING roster --------------------------------------------------
command -v "$JQ" >/dev/null 2>&1 || {
    echo "$NAME: CANNOT ANSWER — no '$JQ' on this machine, so the MUTATING roster (infra/ops/verbs/*.json) cannot be derived (exit 3, not clean)" >&2
    exit 3
}
# The allowlist is the directory infra/ops/verbs/, assembled by the one
# script the runner itself uses (5086842d) — read here the same way.
verbs_json="$(sh infra/ops/verbs-allowlist.sh infra/ops/verbs)" || {
    echo "$NAME: infra/ops/verbs-allowlist.sh could not assemble infra/ops/verbs/ (its refusal is above)" >&2
    exit 1
}
roster="$("$JQ" -r '.verbs | to_entries[] | select((.value.about // "") | contains("MUTATING")) | "\(.key)\t\(.value.argv[0] // "")"' <<<"$verbs_json")" || {
    echo "$NAME: jq could not read the assembled verb table" >&2
    exit 1
}
[ -n "$roster" ] || {
    echo "$NAME: no verb under infra/ops/verbs declares MUTATING — the roster derivation broke, or the word moved; refusing rather than checking nothing" >&2
    exit 1
}

declare -A verbs_of=()
while IFS=$'\t' read -r verb script; do
    [ -n "$verb" ] || continue
    case "$script" in
        */*) ;;
        *) continue ;;   # a bare command on PATH (`boss`, `systemctl`) is not a script in this tree
    esac
    verbs_of["$script"]="${verbs_of[$script]:+${verbs_of[$script]}, }$verb"
done <<<"$roster"

# --- the findings ----------------------------------------------------------
declare -A found=() where=()
fail=0

# Sorted, so two runs over one tree print one scanned line.
scripts=()
while IFS= read -r script; do
    [ -n "$script" ] && scripts+=("$script")
done <<EOF
$(printf '%s\n' "${!verbs_of[@]}" | sort)
EOF
for script in "${scripts[@]}"; do
    if [ ! -f "$script" ]; then
        echo "$NAME: FAIL MUTATING verb(s) ${verbs_of[$script]} run $script, which is not a file in this tree" >&2
        fail=1
        continue
    fi
    gap="$(strict_mode_gap <"$script")"
    if [ -n "$gap" ]; then
        found["no-pipefail $script"]=1
        where["no-pipefail $script"]=$'\n'"    $script: run by MUTATING verb(s) ${verbs_of[$script]}, and no set line turns on $gap"
    fi
done

# Prose and verb data are not shell. The exclusions name their directory
# on purpose: measured on this pod's git 2.39.5 (2026-09-27), `git
# ls-files -- infra/forge/ ':!*.md'` lists NOTHING, while
# `':!infra/forge/*.md'` lists the 78 it should. Every exclusion measured
# that did this (`*.md`, `*.ts`, `*.zzz`, `*zzz`) was no longer than the
# positive pathspecs' common prefix, and every one that worked
# (`*.test.ts`, `*.test.zzz`, a directory-named one) was longer. git grep is
# not affected, so the scan would have run and the scanned line read 0.
hits=$(pattern_scan "$PREFILTER" \
    --exclude ':!infra/forge/*.md' \
    --exclude ':!infra/ops/*.md' \
    --exclude ':!infra/forge/*.json' \
    --exclude ':!infra/ops/*.json' \
    -- 'infra/forge/' 'infra/ops/' "${scripts[@]}") || exit $?

while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    file="${hit%%:*}"
    rest="${hit#*:}"
    lineno="${rest%%:*}"
    code="${rest#*:}"
    classify "$code"
    [ -n "$RULE" ] || continue
    key="$RULE $file"
    found["$key"]=$(( ${found[$key]:-0} + 1 ))
    where["$key"]="${where[$key]:-}"$'\n'"    $file:$lineno: $code"
done <<<"$hits"

# --- the allowlist ---------------------------------------------------------
[ -f "$ALLOW_FILE" ] || {
    echo "$NAME: $ALLOW_FILE is missing — an empty allowlist is a file with no entries, not an absent one" >&2
    exit 1
}
declare -A allowed=()
allow_paths=()
n=0
while IFS= read -r entry || [ -n "$entry" ]; do
    n=$((n + 1))
    trimmed="${entry#"${entry%%[![:space:]]*}"}"
    case "$trimmed" in ''|'#'*) continue ;; esac
    read -r rule path count reason <<<"$trimmed"
    case "$rule" in
        swallowed-read|failed-read-as-number|no-pipefail) ;;
        *) echo "$NAME: $ALLOW_FILE:$n: unknown rule '$rule' (swallowed-read, failed-read-as-number, no-pipefail)" >&2; fail=1; continue ;;
    esac
    case "${count:-empty}" in
        empty|*[!0-9]*|0) echo "$NAME: $ALLOW_FILE:$n: the count must be a positive number of sites, got '${count:-}'" >&2; fail=1; continue ;;
    esac
    if [ -z "${reason:-}" ]; then
        echo "$NAME: $ALLOW_FILE:$n: '$rule $path' carries no reason — an allowance nobody can read is a skip" >&2
        fail=1
        continue
    fi
    if [ -n "${allowed[$rule $path]:-}" ]; then
        echo "$NAME: $ALLOW_FILE:$n: '$rule $path' is listed twice" >&2
        fail=1
        continue
    fi
    allowed["$rule $path"]="$count"
    allow_paths+=("$path")
done <"$ALLOW_FILE"
allowlist_paths_exist "$NAME" ${allow_paths[@]+"${allow_paths[@]}"}

sites_allowed=0
while IFS= read -r key; do
    [ -n "$key" ] || continue
    have="${found[$key]}"
    allow="${allowed[$key]:-0}"
    if [ "$have" -gt "$allow" ]; then
        echo "$NAME: FAIL ${key%% *} in ${key#* } — $have site(s), allowance $allow:${where[$key]}" >&2
        fail=1
    elif [ "$have" -lt "$allow" ]; then
        echo "$NAME: FAIL ${key#* } — allowance $allow for ${key%% *}, but only $have site(s) left; lower it to match" >&2
        fail=1
    else
        sites_allowed=$((sites_allowed + have))
    fi
done <<EOF
$(printf '%s\n' "${!found[@]}" | sort)
EOF
for key in "${!allowed[@]}"; do
    if [ -z "${found[$key]:-}" ]; then
        echo "$NAME: FAIL stale allowance '$key' in $ALLOW_FILE — it excuses nothing this run; delete it" >&2
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    cat >&2 <<'MSG'

A guard in a host script must fail CLOSED (design 3036296f mechanism B).
A read that failed is not an empty answer and not a zero; say which it
was, and let the guard refuse on the failure:

    n=$(grep -c . "$f") || [ $? -eq 1 ] || refuse "cannot read $f"
    declared=$(list_them) || unreadable=1      # the failure has its own name
    case "${x:-empty}" in empty|*[!0-9]*) refuse "…" ;; esac

and a MUTATING verb's script runs under `set -uo pipefail` (or -euo).
When a site is genuinely safe, add it to
infra/lint/an-ops-guard-fails-closed-allow.txt as
`<rule> <path> <count> <reason>` — the reason says why THIS failure
cannot open the guard, so the next reviewer can check it rather than
trust it.
MSG
    exit 1
fi

echo "$NAME: ok — no guard fails open outside its allowance ($sites_allowed allowed site(s) in ${#allowed[@]} entr(ies)); ${#scripts[@]} MUTATING verb script(s) run under nounset and pipefail"
exit 0
