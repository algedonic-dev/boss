#!/usr/bin/env bash
# check-suspended-chores.sh — read which cluster chores stand suspended,
# and since when. The ops verb `check-suspended-chores`; it takes nothing
# and writes nothing.
#
# WHY IT EXISTS (backlog 87e08026, car 2 of d28c0051). A chore is a
# CronJob labelled boss-chore=true. Suspending one (spec.suspend=true) is
# a legitimate act — boss/boss-search-reindex was suspended by hand at
# 2026-11-11 05:02Z while its temp spill was diagnosed (832386eb) — and
# nothing reported it afterwards: the converge's client-side apply does
# not own spec.suspend, so it KEEPS a live true across every apply, and
# check-manifests-applied.sh only asks that each CronJob exists. A
# suspension nobody resumes therefore stands for ever, in silence, and
# the chore's own verdict packets simply stop arriving.
#
# WHO RUNS IT. The dispatcher rule check-suspended-chores-daily files this
# verb for the forge every day, and watch-check-suspended-chores-daily
# judges its last line with ops.judge, filing one urgent alarm per pass
# whose verdict names every chore past the threshold. The THRESHOLD IS NOT
# HERE: 24 hours lives in that rule's `when`, registry data an operator
# can change without a deploy. The shape is check-longhorn-drain-policy's
# (infra/forge/longhorn-drain-policy.sh --check): the forge holds the
# admin kubeconfig David placed (ops_kubectl, infra/estate/ops-credentials.sh),
# so the read needs no new grant anywhere.
#
# WHAT IT READS — one `kubectl get`: every CronJob in every namespace
# carrying boss-chore=true, as a list, WITH its managedFields (kubectl
# hides them unless --show-managed-fields). The label is the one the
# chore manifests and the converge's pinning already use; nothing else
# is shared with any other script.
#
# HOW LONG A SUSPENSION HAS STOOD is the API server's own record, never a
# clock this script kept: the managedFields entry that OWNS f:spec.f:suspend
# (a status write owns none), its `time`. Measured live 2026-11-11 09:06Z:
# boss-search-reindex's f:spec.f:suspend is owned by manager kubectl-patch
# at 05:02:13Z, and kubectl-client-side-apply (the converge, 08:48:46Z)
# does not own it. That time is when that manager last changed ANY field
# it owns, so the age read can understate a suspension, never overstate
# it — an alarm a little late, never a false one. Several owners: the
# latest time, as the Longhorn check reads `value`.
#
# THE CONTROL. Every chore this tree runs carries the label, so an answer
# naming NO boss-chore CronJob is not "none suspended": it is a wrong
# target or a dark selector answering instead of erroring, and it is
# CANNOT ANSWER.
#
# THE VERDICT is the last line,
#   READ — suspended <n>, oldest_hours <h>, chores <ns/name,...|none>
# where `oldest_hours` is the longest-standing suspension in whole hours
# (0 when none) and `chores` names every suspended chore, sorted.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look, an answer that is
# not a list, an empty chore list, a suspended list that could not be
# sorted, or a suspended chore whose suspension time cannot be read
# (unowned, or owned by an undated entry) is CANNOT ANSWER, exit 1, with NO verdict line —
# which the watch alarms on as a failed read. Exit 78 is a usage refusal.
set -uo pipefail

ME=check-suspended-chores
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LABEL=boss-chore=true

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — this says nothing about the chores: they were not read, which is neither suspended nor running"; }

[ $# -eq 0 ] || refuse "usage: check-suspended-chores.sh — the check takes nothing"
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the cluster's answer cannot be read, and no evidence is not an answer"

NOW="${BOSS_SUSPENDED_CHORES_NOW:-$(date -u +%s)}"
case "${NOW:-empty}" in
    empty | *[!0-9]*) fail "the clock '$NOW' is not whole seconds, so no age can be stated" ;;
esac

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    unread "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K get cronjobs.batch -A -l "$LABEL" -o json --show-managed-fields --request-timeout=20s \
        > "$WORK/chores.json" 2> "$WORK/chores.err"; then
    unread "cronjobs labelled $LABEL: $(tr '\n' ' ' < "$WORK/chores.err" | cut -c1-400)"
fi
jq_doc_file "$WORK/chores.json" && jq -e '.items | type == "array"' "$WORK/chores.json" > /dev/null 2>&1 \
    || unread "the cronjobs labelled $LABEL did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/chores.json" | tr '\n' ' ')')"

# By jq's OUTPUT, never its exit: silence is then a refusal (infra/lib/jq.sh).
total="$(jq -r '.items | length' "$WORK/chores.json" 2> /dev/null)"
case "${total:-empty}" in
    empty | *[!0-9]*) unread "the cronjobs labelled $LABEL could not be counted" ;;
esac
[ "$total" -gt 0 ] \
    || unread "no CronJob in any namespace carries $LABEL, and every chore this tree runs does — an empty answer is a wrong target or a dark selector, not 'none suspended'"

# One line per SUSPENDED chore, sorted: namespace/name, the latest time of
# a managedFields entry owning f:spec.f:suspend (`unowned` when none does,
# `undated` when entries own it and none carries a time — two different
# facts, so two different messages: review run 7682a54f, F3), the
# managers at that time (every owner, when undated), lastScheduleTime,
# and the schedule. No field is ever empty: a tab is whitespace to
# `read`, so an empty field would collapse and shift every field after it.
if ! jq -r '.items[]
        | select(.spec.suspend == true)
        | [(.metadata.managedFields // [])[]
           | select((.subresource // "") == ""
                    and ([(.fieldsV1 // {})["f:spec"] // {} | objects | has("f:suspend")] | any))] as $own
        | ([$own[] | .time // empty | select(. != "")] | sort | last
           // (if ($own | length) == 0 then "unowned" else "undated" end)) as $since
        | ([$own[] | select(.time == $since or $since == "undated") | .manager // "an unnamed manager"]
           | unique | join(", ")
           | if . == "" then "no manager" else . end) as $who
        | [ "\(.metadata.namespace)/\(.metadata.name)", $since, $who,
            (.status.lastScheduleTime // "never"), (.spec.schedule // "unread") ]
        | @tsv' "$WORK/chores.json" > "$WORK/suspended.tsv" 2> "$WORK/suspended.err"; then
    unread "the suspended chores would not parse: $(tr '\n' ' ' < "$WORK/suspended.err" | cut -c1-400)"
fi
# By byte, so the verdict names the chores in one order on every host.
# Its exit is read: GNU sort truncates its -o target BEFORE it writes, so
# a sort that dies mid-write (ENOSPC on $WORK, the forge's disk-floor
# failure) leaves this list empty or short, and read on, an empty list is
# "suspended 0" and no alarm — no evidence read as a pass (review run
# 7682a54f, F2).
if ! LC_ALL=C sort -o "$WORK/suspended.tsv" "$WORK/suspended.tsv" 2> "$WORK/sort.err"; then
    unread "the suspended chores could not be sorted: $(tr '\n' ' ' < "$WORK/sort.err" | cut -c1-400)"
fi

n=0
oldest=0
names=""
while IFS=$'\t' read -r chore since who last schedule; do
    case "${since:-unowned}" in
        unowned) unread "chore $chore is suspended and no managedFields entry owns spec.suspend (read with --show-managed-fields), so how long it has stood is unknown" ;;
        undated) unread "chore $chore is suspended and $who owns spec.suspend and its entry carries no time (read with --show-managed-fields), so how long it has stood is unknown" ;;
    esac
    since_s="$(date -u -d "$since" +%s 2> /dev/null)"
    case "${since_s:-empty}" in
        empty | *[!0-9]*) unread "chore $chore is suspended and its suspension time '$since' is not a time, so how long it has stood is unknown" ;;
    esac
    hours=$(((NOW - since_s) / 3600))
    [ "$hours" -ge 0 ] || hours=0
    echo "$ME: SUSPENDED — $chore since $since (${hours} hour(s)), set by ${who:-an unnamed manager}; last scheduled $last; schedule $schedule. The converge keeps spec.suspend, so it stands until someone resumes it"
    n=$((n + 1))
    [ "$hours" -le "$oldest" ] || oldest="$hours"
    names="${names:+$names,}$chore"
done < "$WORK/suspended.tsv"

echo "$ME: $total chore(s) labelled $LABEL read, $n suspended"
echo "$ME: READ — suspended $n, oldest_hours $oldest, chores ${names:-none}"
exit 0
