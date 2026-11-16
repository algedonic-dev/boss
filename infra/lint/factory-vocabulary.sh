#!/usr/bin/env bash
# factory-vocabulary — a file this change ADDS does not use a retired
# factory word: `mover`, `territory`, `machinery`, or `machine` as an
# executor kind anywhere; and, in `crates/` or `apps/`, `equipment` as a
# station kind, `bay`, `automation` as a group, or a literal
# `/algedonic/…` address.
#
# WHY. The factory taxonomy (docs/design/factory-taxonomy.md, design
# 4348b857, David 2026-11-02) sorted the vocabulary: an executor is a
# human, a robot or an AUTOMATON; the old words name the same things
# under other names ("mover" retired outright, "territory" is a station
# on IT's floor, "machinery" is the machines of a station) and `machine`
# as an executor kind is the one David replaced, because `machine`
# already means a state machine, a machine token and the machine door in
# about a thousand files. The mapping is the design's table; this lint is
# its mechanism M5 (item 10281dd8): a sentence in a doc renames nothing,
# and a rename sweep is the merge-conflict shape CLAUDE.md §9a warns of,
# so the retired words are refused where they are NEW and left where they
# are history, renamed as a file is touched.
#
# THE V2 WORDS (station tree T4, backlog 3c5115ee; design 4b27a65a,
# docs/design/factory-taxonomy-v2.md). The station tree retires four more
# things v1 still had: the generic/equipment split (a station is one
# thing, standing or ad hoc), bays (a station's capacity), automation as
# a group of its own (they are the machines of a station), and any
# address spelled in code (station-tree.md §8.1: code never names an
# address; it lives in a row). These four are refused in CODE only,
# `crates/` and `apps/`. The registry rows under `infra/platform/` must
# still carry v1's `kind = "equipment"` and `bays` while the station tree
# bundle is additive, because the last-converged build a rollback lands
# on reads them; a doc quotes them as history. Code is where a new file
# either learns the new words or deepens the retired split.
#
# THE CUTOVER. Written 2026-11-03 by the car that filed this lint, and
# extended 2026-11-06 for the v2 words (CUTOVER and CUTOVER_V2 below are
# the record). "Newer than the cutover" is decided WITHOUT history,
# because the gate's clone is fifty commits deep
# (infra/gate-runner/run.sh fetches --depth 50) and a shallow boundary
# would make every old file look newly added. A file is NEW when the
# change being gated ADDS it against the merge-base with the trunk:
# `git diff --no-renames --diff-filter=A --name-only`. That is also how
# a file arrives on main after the cutover, so every file main holds
# that is younger than the stamp passed this check on its way in, and
# every older one is history. That is the grandfathering: there is no
# baseline list to keep, because the trunk IS the baseline. `--no-renames`
# on purpose: a MOVED file is an added file to this check, so it is
# cleaned when it moves. A checkout with no trunk ref has no baseline to
# class a file as added, so it skips loudly (exit 0, the "what did this
# branch add" judgement of lib/trunk-ref.sh); git not answering refuses
# (exit 3).
#
# HOW EACH WORD IS RECOGNISED, so both sides are mechanical:
#   * mover, territory, machinery — the whole word, any case, anywhere
#     in an added file. The taxonomy retired them outright, so an added
#     file has no legal use of them. The files that must name them, the
#     designs that retire them, are exempt by name below, as is this lint
#     and its own proof.
#   * machine — only where it is the VALUE of an executor kind or of a
#     `kind`: `executor_kind` or `ExecutorKind` and `machine` on one line (any case, which
#     covers `ExecutorKind::Machine`), `kind` `=`/`:` `"machine"` or
#     `'machine'`, or the phrases `machine executor`, `executor kind
#     machine`, `machine as an executor`, `machines are executors`.
#     `machine token`, `state machine`, `machine door`, `machine_token`,
#     `MachineToken`, `machine gate` match none of these and pass.
#   * equipment as a kind (code only) — `kind` `=`/`:` a quoted
#     `equipment`, `Kind::Equipment` (any case, so `StationKind::` too),
#     or the phrase `equipment station`. `equipment` as a plain noun
#     passes.
#   * bay (code only) — `bay` or `bays` as a whole word, any case:
#     `bays: 2`, `gate-bay-0`, "two bays". `GATE_BAYS` and `bay_count`
#     are one identifier with the word inside it and pass, as `-w` would
#     have them.
#   * automation as a group (code only) — `automation` or `automations`
#     quoted on its own (`'automation'` as a lane or a family name), the
#     phrases `automation group|family|lane|region`, or `group`,
#     `family`, `lane` or `region` `=`/`:` a quoted `automation`. The
#     actor prefix `automation:<name>` is an id, not a group, and passes.
#   * a literal address (code only) — `/algedonic` starting a path that
#     nothing precedes but a quote, a space or punctuation, and that ends
#     at `/`, a quote, a space or the line's end: `"/algedonic/engineering"`.
#     A URL or a file path that merely contains the segment
#     (`/work/algedonic/…`, `/algedonic-dev/`) passes.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read; no added file uses a retired word, or there is
#      no trunk ref to class a file as added (said out loud)
#   1  an added file uses a retired word — the author's to rename
#   3  git could not answer — nothing was read; never `clean`
#
# USAGE
#   infra/lint/factory-vocabulary.sh
set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3

LINT=factory-vocabulary
CUTOVER="20261103T013000Z"
CUTOVER_V2="20261106T204156Z"
# Files that must name the retired words: the designs that retire them,
# this lint, and this lint's proof (a path naming the lint under tests/,
# the convention of lib/pattern-scan.sh, spelled with either separator
# because a Rust test file cannot carry a hyphen).
EXEMPT_EXACT="docs/design/factory-taxonomy.md docs/design/factory-taxonomy-v2.md docs/design/station-tree.md infra/lint/factory-vocabulary.sh"
is_exempt() { # path
    local p="$1" e
    for e in $EXEMPT_EXACT; do [ "$p" = "$e" ] && return 0; done
    case "$p" in
        */tests/*factory-vocabulary*|*/tests/*factory_vocabulary*) return 0 ;;
    esac
    return 1
}

tracked=$(git_answer "$LINT" 0 ls-files) || exit $?
n_tracked=$(printf '%s\n' "$tracked" | grep -c '[^[:space:]]')

trunk=$(resolve_trunk_ref "$LINT")
status=$?
case "$status" in
    0) ;;
    1)
        lint_scanned "$LINT" "$n_tracked" "tracked file(s); no trunk ref here (tried $(trunk_candidates)), so no file can be classed as added — skipped, not clean"
        exit 0 ;;
    *) exit "$LINT_CANNOT_ANSWER" ;;
esac
base=$(resolve_merge_base "$LINT" "$trunk") || exit $?
added_all=$(git_answer "$LINT" 0 diff --no-renames --diff-filter=A --name-only "$base" HEAD) || exit $?

added=()
code=()
while IFS= read -r f; do
    [ -n "$f" ] || continue
    is_exempt "$f" && continue
    added+=("$f")
    case "$f" in crates/*|apps/*) code+=("$f") ;; esac
done <<EOF
$added_all
EOF
n_added=${#added[@]}
n_code=${#code[@]}
lint_scanned "$LINT" "$n_tracked" "tracked file(s), $n_added added against $trunk (merge-base ${base:0:12}) since the $CUTOVER cutover, $n_code of them code under crates/ or apps/ held to the $CUTOVER_V2 words"

# A quote of any of the three kinds, for the patterns below.
Q="[\"'\`]"
hits=""
if [ "$n_added" -gt 0 ]; then
    # Whole-word, any case; -I skips binaries. Exit 1 is "no match".
    retired=$(git_answer "$LINT" 0,1 grep -n -I -i -w -E 'mover|territory|machinery' -- "${added[@]}")
    status=$?; [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && exit "$LINT_CANNOT_ANSWER"
    kind=$(git_answer "$LINT" 0,1 grep -n -I -i -E \
        -e 'executor_?kind.*machine' \
        -e "kind[[:space:]]*[:=][[:space:]]*${Q}machine${Q}" \
        -e 'machine executor|executor kind machine|machines? as (an )?executors?|machines? (is|are) (an )?executors?' \
        -- "${added[@]}")
    status=$?; [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && exit "$LINT_CANNOT_ANSWER"
    hits=$(printf '%s\n%s\n' "$retired" "$kind")
fi
if [ "$n_code" -gt 0 ]; then
    v2=$(git_answer "$LINT" 0,1 grep -n -I -i -E \
        -e "kind[[:space:]]*[:=][[:space:]]*${Q}equipment${Q}" \
        -e 'kind::equipment' \
        -e 'equipment[[:space:]]+stations?' \
        -e '(^|[^[:alnum:]_])bays?([^[:alnum:]_]|$)' \
        -e "${Q}automations?${Q}" \
        -e 'automations?[[:space:]]+(group|family|lane|region)s?([^[:alnum:]_]|$)' \
        -e "(group|family|lane|region)[[:space:]]*[:=][[:space:]]*${Q}automations?${Q}" \
        -- "${code[@]}")
    status=$?; [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && exit "$LINT_CANNOT_ANSWER"
    address=$(git_answer "$LINT" 0,1 grep -n -I -E \
        -e "(^|[^[:alnum:]_.~/-])/algedonic(/|${Q}|[[:space:]]|$)" \
        -- "${code[@]}")
    status=$?; [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && exit "$LINT_CANNOT_ANSWER"
    hits=$(printf '%s\n%s\n%s\n' "$hits" "$v2" "$address")
fi
hits=$(printf '%s\n' "$hits" | grep -v '^$' | sort -u -t: -k1,1 -k2,2n)

if [ -n "$hits" ]; then
    {
        echo "$LINT: an added file uses a retired factory word — the taxonomy's words are human | robot | automaton for executors, and a station (standing or ad hoc) with its machines and capacity for places (docs/design/factory-taxonomy-v2.md, the mapping tables):"
        printf '%s\n' "$hits" | sed 's/^/    /'
        echo "    mover -> retired (say what moves it: a machine of a station, a rule); territory -> station; machinery -> the machines of a station; machine as an executor kind -> automaton. machine token, state machine and the machine door are not executor uses and pass."
        echo "    In crates/ and apps/: equipment as a station kind -> a standing station with machines; bay(s) -> the station's capacity; automation as a group -> the machines of their station; a literal /algedonic/... address -> read the station from its row by id (station-tree.md §8.1)."
    } >&2
    exit 1
fi
echo "$LINT: ok — no file added since the $CUTOVER cutover uses mover, territory, machinery, or machine as an executor kind, and no code file added since $CUTOVER_V2 uses equipment as a kind, bay, automation as a group, or a literal address"
