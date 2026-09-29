#!/usr/bin/env bash
#
# break-glass-awaiting-re-enrolment-only-shrinks — the list of break-glass
# records excused from answering the door may lose rows, never gain them.
#
# THE HOLE (backlog edf403aa, from the adversarial review of car 1c4c100a,
# 2026-09-28). Both break-glass keys were enrolled under the playground
# host and stranded by the 2026-09-16 cutover (793a3c33): no key could
# open the emergency door for six days and the committed records looked
# healthy. boss-gateway's the_break_glass_records_answer_the_door_they_serve
# now holds every record to the door BOSS_PUBLIC_URL names, except the rows
# of infra/lint/break-glass-awaiting-re-enrolment.txt — the two keys still
# awaiting re-enrolment. That test fails when a listed record is replaced,
# so a row cannot outlive its reason. Nothing stopped a row being ADDED:
# when the door next moves, a car that also lists both keys under the door
# it abandoned passes both of the test's checks, and the incident is back
# behind a two-line diff only review would see. "The list may only shrink"
# was a sentence in a doc comment. This makes it a check.
#
# THE CHECKED PROPERTY. Every row in the list — identified by its record,
# `label rp_id`; the justification columns may be reworded — must already
# be on the trunk at the merge-base. A set ratchet, not a count: swapping
# one row for another keeps the count and is still an addition.
#
# WHAT IT READS. The list as it stands in the WORKING TREE (so a builder's
# pre-flight before the commit sees the verdict the gate will; the gate's
# checkout is the pushed commit, so there the two are one), against the
# list at the merge-base with the trunk (lib/trunk-ref.sh; BOSS_TRUNK_REF
# overrides).
#
# TWO BASELINES THAT ARE NOT A ROW LIST. The list absent at the merge-base
# is either this branch introducing it — nothing to ratchet against, said
# out loud, exit 0 — or the trunk having DELETED it, in which case the
# baseline is the empty set and every row is an addition: a list that was
# retired does not come back with a fresh allowance. Told apart by whether
# the path has any history at the merge-base.
#
# Usage: infra/lint/break-glass-awaiting-re-enrolment-only-shrinks.sh
# Exit:  0 no row added / 1 a row added, or no trunk ref in this checkout
#        (a ratchet that cannot see its baseline certifies nothing) /
#        3 git could not answer, so nothing was read (lib/git-answer.sh)

set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3

LINT=break-glass-awaiting-re-enrolment-only-shrinks
LIST="infra/lint/break-glass-awaiting-re-enrolment.txt"

# `label<TAB>rp_id`, one per row, sorted: the record a row excuses. The
# same parse on both sides, so a re-spacing is not read as an addition.
rows() {
    awk 'NF && $1 !~ /^#/ { print $1 "\t" $2 }' | LC_ALL=C sort -u
}

git_can_answer "$LINT" || exit $?

BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates))" >&2
    echo "  Cannot tell which rows the trunk already carries without one. Fetch the trunk." >&2
    exit 1
fi
MB=$(resolve_merge_base "$LINT" "$BASE" HEAD) || exit $?

NOW=""
if [ -f "$LIST" ]; then
    NOW=$(rows < "$LIST")
fi

# Is there a baseline at all, asked separately from reading it: `ls-tree`
# answers present / absent / a real failure, where a `git show` would
# fold "absent" into "git failed".
AT_BASE=$(git_answer "$LINT" 0 ls-tree --name-only "$MB" -- "$LIST") || exit $?
if [ -n "$AT_BASE" ]; then
    # Two statements, not a pipeline: through a pipe the status bash
    # reports is the parser's, and the read's refusal would be lost.
    WAS_FILE=$(git_answer "$LINT" 0 show "$MB:$LIST") || exit $?
    WAS=$(printf '%s\n' "$WAS_FILE" | rows)
else
    EVER=$(git_answer "$LINT" 0 log -1 --format=%h "$MB" -- "$LIST") || exit $?
    if [ -z "$EVER" ]; then
        echo "$LINT: $LIST is introduced by this branch — no trunk list to ratchet against"
        echo "$LINT: clean"
        exit 0
    fi
    # Deleted on the trunk at $EVER: the baseline is the empty set.
    WAS=""
fi

ADDED=$(LC_ALL=C comm -13 <(printf '%s\n' "$WAS" | awk 'NF') <(printf '%s\n' "$NOW" | awk 'NF'))
if [ -z "$ADDED" ]; then
    echo "$LINT: $(printf '%s\n' "$NOW" | awk 'NF' | wc -l | tr -d ' ') row(s), none new against $BASE"
    echo "$LINT: clean"
    exit 0
fi

echo "VIOLATION: $LINT"
echo "    $LIST gained a row the trunk ($BASE) does not carry:"
while IFS=$'\t' read -r label rp; do
    [ -n "${label:-}" ] || continue
    echo "      $label  $rp"
done <<< "$ADDED"
echo "    The list excuses break-glass records bound to a door BOSS_PUBLIC_URL no"
echo "    longer names, and it may only SHRINK (backlog edf403aa). A change to"
echo "    BOSS_PUBLIC_URL re-enrols every break-glass key in the same breath, a"
echo "    break-glass-enrolment packet per key, and commits the new records; it"
echo "    does not waive them here. That waiver is how 1c4c100a left no key able"
echo "    to open the emergency door for six days."
exit 1
