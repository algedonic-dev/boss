#!/usr/bin/env bash
# check-node-trim.sh — read, from OUTSIDE its namespace, whether the build
# node's daily trim ran, and what it told the drives. The ops verb
# `check-node-trim`; it takes nothing and writes nothing.
#
# WHY IT EXISTS (backlog 33e62de8; review 8b1981da of the trim car
# 90d654c6, findings F1 to F3). CronJob boss-node-trim is ONE attempt a
# day (12:40Z, Forbid, activeDeadlineSeconds 1800, backoffLimit 0) in the
# one namespace labelled privileged. It is given no machine token and no
# door to the jobs API, by decision: the pod that holds a node capability
# must not also hold a credential on the system of record. So an image
# that will not pull, a node that is down, a pod the admission policy
# refuses, and a trim that exits RED each left a failed or absent Job that
# no packet, alarm or roster read. Twice in two days the gates slowed and
# threw false reds before anyone looked at the drive.
#
# WHY HERE. The reader cannot live beside the trim: that namespace's
# admission policy refuses a Role, a RoleBinding, a ServiceAccount token
# and any second workload (the admission file beside its manifest). The
# forge already holds the admin kubeconfig David placed (ops_kubectl,
# infra/estate/ops-credentials.sh), so the read is a forge verb: no new
# grant anywhere, and nothing added to the namespace.
#
# WHO RUNS IT. The dispatcher rule check-node-trim-daily files this verb
# as an ops-request every day, and watch-check-node-trim-daily judges its
# last line. The request's subject is `node-trim`, an identity that rule
# alone files, so the cadence silence sweep derives a daily
# `ops-request/node-trim` from the rule registry: a reader that stops
# running is itself noticed.
#
# WHAT IT READS — five `kubectl get` and at most one `kubectl logs`:
#   * the namespace list, as a CONTROL: an answer that does not name
#     kube-system is not this cluster's answer, and an empty CronJob list
#     from it would be a wrong target answering instead of erroring;
#   * the CronJobs, Jobs, Pods and Events of that one namespace ($NS), each
#     as a LIST. A list that reads back empty is a positive "not there";
#     no stderr text is parsed to tell absent from unreachable.
#
# WHAT IT SAYS:
#   * the last success, from status.lastSuccessfulTime (the controller's
#     own record, never a clock kept here), and its age in whole hours —
#     or, when the trim has never succeeded, the hours the CronJob has
#     existed without one;
#   * the newest FINISHED Job's outcome and the cause of a failure, from
#     what still stands: the Job's Failed condition, its pod's exit code,
#     waiting reason or unschedulable condition, and the Warning events
#     naming either. A Job that fails on its deadline has its pod deleted
#     and events live about an hour, so a cause this run can no longer
#     read is SAID to be unread rather than guessed. The finished Job is
#     judged, not merely the newest: a check that lands inside a run's 30
#     minutes would otherwise see that day's Job only as "running" and
#     never as failed;
#   * each NVMe's discard counters before and after, as the trim's own
#     script printed them (pod termination message; the log when the
#     message carries none). The fstrim byte count is deliberately NOT
#     read: two hand trims a day apart printed identical numbers (review
#     F2), so it says what the filesystem offered, not what the drive was
#     told. `moved` counts the devices whose counters rose.
#
# THE VERDICT is the last line,
#   READ — state <present|not-yet|missing>, missing <0|1>, hours <h>,
#   failed <0|1>, unmoved <0|1>, last_success <time|never>,
#   job <name|none>, outcome <succeeded|failed|none>, reason <Token>,
#   devices <n>, moved <m>
# and the THRESHOLD IS NOT HERE: 26 hours, and what else is an alarm, are
# the watch rule's `when`
# (infra/dispatcher/rules/watch-check-node-trim-daily.toml), registry data
# an operator can change without a deploy.
# The numbers are what the `when` reads (a word compared inside a string
# inside a TOML string is one lexer too many); `state` says the same for a
# person.
#   state not-yet   no such CronJob, and this checkout's tree does not
#                   declare one: the trim car has not landed. Never an
#                   alarm: every number it prints is 0.
#   state missing   no such CronJob, and this tree DOES declare it
#                   (the manifest named for the namespace, $MANIFEST):
#                   a converge that is held or failed, or an object
#                   someone deleted. An alarm — a deleted CronJob that
#                   read `not-yet` for ever would be the silence this
#                   exists to end.
#   unmoved 1       the newest finished Job SUCCEEDED and no device's
#                   counter rose, or none could be read: a success with no
#                   evidence is not a pass.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look (the API dark, one
# list unread, an answer that is not a list) is CANNOT ANSWER, exit 1,
# with NO verdict line — which the watch alarms on as a failed read. It is
# never "the trim failed" and never "ok". Exit 78 is a usage refusal.
set -uo pipefail

ME=check-node-trim
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

NS=boss-node-maintenance
CJ=boss-node-trim
# The tree's own declaration of the CronJob: what tells "not landed yet"
# from "declared and not there". A knob only so a test never reads the
# real tree.
MANIFEST="${BOSS_NODE_TRIM_MANIFEST:-$HERE/../cluster/manifests/$NS.yaml}"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — this says nothing about the trim: it was not read, which is neither a failed trim nor a healthy one"; }

[ $# -eq 0 ] || refuse "usage: check-node-trim.sh — the check takes nothing"
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the cluster's answer cannot be read, and no evidence is not an answer"

NOW="${BOSS_NODE_TRIM_NOW:-$(date -u +%s)}"
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

# read_list <resource> <file> [-n <namespace>] — a list into $WORK/<file>,
# or CANNOT ANSWER naming the resource and the door's own words.
read_list() {
    local res="$1" file="$2"
    shift 2
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get "$res" "$@" -o json --request-timeout=20s > "$WORK/$file" 2> "$WORK/$file.err"; then
        unread "$res: $(tr '\n' ' ' < "$WORK/$file.err" | cut -c1-400)"
    fi
    jq_doc_file "$WORK/$file" && jq -e '.items | type == "array"' "$WORK/$file" > /dev/null 2>&1 \
        || unread "$res did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/$file" | tr '\n' ' ')')"
}

# hours_since <RFC 3339 time> — whole hours from it to NOW, never negative.
hours_since() {
    local s
    case "${1:-empty}" in empty) return 1 ;; esac
    s="$(date -u -d "$1" +%s 2> /dev/null)"
    case "${s:-empty}" in empty | *[!0-9]*) return 1 ;; esac
    s=$(((NOW - s) / 3600))
    [ "$s" -ge 0 ] || s=0
    printf '%s\n' "$s"
}

verdict() {
    local missing=0
    [ "$1" != missing ] || missing=1
    echo "$ME: READ — state $1, missing $missing, hours $2, failed $3, unmoved $4, last_success $5, job $6, outcome $7, reason $8, devices $9, moved ${10}"
}

# --- the control, then the CronJob -----------------------------------------------
read_list namespaces namespaces.json
# By jq's OUTPUT, never its exit: silence is then a refusal (infra/lib/jq.sh).
control="$(jq -r 'if ([.items[].metadata.name] | index("kube-system")) != null then "ours" else "not-ours" end' "$WORK/namespaces.json" 2> /dev/null)"
[ "$control" = ours ] \
    || unread "the namespace list names no kube-system, so it is not this cluster's answer and an empty list from it would prove nothing"
ns_there="$(jq -r --arg n "$NS" 'if ([.items[].metadata.name] | index($n)) != null then "present" else "ABSENT" end' "$WORK/namespaces.json")"
echo "$ME: namespace $NS is $ns_there ($(jq -r '.items | length' "$WORK/namespaces.json") namespaces read, kube-system among them)"

read_list cronjobs.batch cronjobs.json -n "$NS"
held="$(jq -r --arg n "$CJ" '[.items[] | select(.metadata.name == $n)] | length' "$WORK/cronjobs.json" 2> /dev/null)"
case "${held:-empty}" in
    empty | *[!0-9]*) unread "the CronJob list of $NS could not be counted" ;;
esac
if [ "$held" -ne 1 ]; then
    if [ -f "$MANIFEST" ] && grep -qE "^  name: $CJ\$" "$MANIFEST"; then
        echo "$ME: MISSING — CronJob $CJ is not in namespace $NS, and this checkout's tree declares it ($(basename "$MANIFEST")). Either the cluster converge that applies it is held or failed, or the object was deleted: no trim is scheduled. Read the newest converge packet, and the namespace's label and admission policy ($NS-admission.yaml)"
        verdict missing 0 0 0 never none none Missing 0 0
    else
        echo "$ME: NOT YET — CronJob $CJ is not in namespace $NS, and this checkout's tree does not declare one: the trim has not landed here. Nothing to judge"
        verdict not-yet 0 0 0 never none none NotYet 0 0
    fi
    exit 0
fi
jq --arg n "$CJ" '[.items[] | select(.metadata.name == $n)][0]' "$WORK/cronjobs.json" > "$WORK/cj.json"

created="$(jq -r '.metadata.creationTimestamp // ""' "$WORK/cj.json")"
last_ok="$(jq -r '.status.lastSuccessfulTime // ""' "$WORK/cj.json")"
last_sched="$(jq -r '.status.lastScheduleTime // "never"' "$WORK/cj.json")"
echo "$ME: CronJob $CJ: schedule $(jq -r '.spec.schedule // "unread"' "$WORK/cj.json"), suspend $(jq -r '.spec.suspend // false' "$WORK/cj.json"), created ${created:-unread}, last scheduled $last_sched, active now $(jq -r '(.status.active // []) | length' "$WORK/cj.json")"

if [ -n "$last_ok" ]; then
    hours="$(hours_since "$last_ok")" \
        || unread "status.lastSuccessfulTime '$last_ok' is not a time, so the age of the last trim is unknown"
    echo "$ME: LAST SUCCESS — $last_ok, $hours hour(s) ago"
    last_word="$last_ok"
else
    hours="$(hours_since "$created")" \
        || unread "CronJob $CJ has never succeeded and its creationTimestamp '$created' is not a time, so how long it has gone without a trim is unknown"
    echo "$ME: NEVER SUCCEEDED — CronJob $CJ has no status.lastSuccessfulTime; it has existed $hours hour(s) (created $created)"
    last_word=never
fi

# --- its Jobs --------------------------------------------------------------------
read_list jobs.batch jobs.json -n "$NS"
read_list pods pods.json -n "$NS"
read_list events events.json -n "$NS"

# The CronJob's own Jobs, newest first; `done` is the Job's terminal
# condition, if it has one.
jq --arg n "$CJ" '[.items[]
        | select([(.metadata.ownerReferences // [])[] | select(.kind == "CronJob" and .name == $n)] | length > 0)
        | { name: .metadata.name, created: (.metadata.creationTimestamp // ""),
            started: (.status.startTime // ""), ended: (.status.completionTime // ""),
            done: ([(.status.conditions // [])[] | select(.status == "True" and (.type == "Complete" or .type == "Failed"))][0] // null) }]
    | sort_by(.created) | reverse' "$WORK/jobs.json" > "$WORK/own.json" 2> "$WORK/own.err" \
    || unread "the Job list would not parse: $(tr '\n' ' ' < "$WORK/own.err" | cut -c1-300)"
echo "$ME: $(jq -r 'length' "$WORK/own.json") Job(s) of $CJ kept (the CronJob keeps its last three of each kind)"

# pod_of <job> — the Job's newest pod into $WORK/pod.json (null if none).
pod_of() {
    jq --arg j "$1" '[.items[] | select((.metadata.labels // {})["job-name"] == $j or (.metadata.labels // {})["batch.kubernetes.io/job-name"] == $j)]
        | sort_by(.metadata.creationTimestamp // "") | last // null' "$WORK/pods.json" > "$WORK/pod.json"
}

# events_of <job> — Warning events naming the Job or a pod of it, one line
# each: `<Kind>/<name> <Reason> x<count>: <message>`.
events_of() {
    jq -r --arg j "$1" '.items[]
        | select(.type == "Warning")
        | select((.involvedObject.kind == "Job" and .involvedObject.name == $j)
              or (.involvedObject.kind == "Pod" and ((.involvedObject.name // "") | startswith($j + "-"))))
        | "\(.involvedObject.kind)/\(.involvedObject.name) \(.reason // "NoReason") x\(.count // 1): \((.message // "") | gsub("[\n\r\t]"; " ") | .[0:300])"' \
        "$WORK/events.json" | sort -u > "$WORK/job.events"
}

# pod_state — one line saying where $WORK/pod.json stands, and POD_REASON,
# the token for it (empty when the pod finished clean or there is none).
# Call it redirected to a file, never in $(…), or POD_REASON is lost.
pod_state() {
    POD_REASON=""
    if [ "$(jq -r 'type' "$WORK/pod.json")" != object ]; then
        echo "no pod of the Job remains"
        return 0
    fi
    local name phase exit_code why waiting sched
    name="$(jq -r '.metadata.name' "$WORK/pod.json")"
    phase="$(jq -r '.status.phase // "unread"' "$WORK/pod.json")"
    exit_code="$(jq -r '[(.status.containerStatuses // [])[] | .state.terminated.exitCode // empty][0] // ""' "$WORK/pod.json")"
    why="$(jq -r '[(.status.containerStatuses // [])[] | .state.terminated.reason // empty][0] // ""' "$WORK/pod.json")"
    waiting="$(jq -r '[(.status.containerStatuses // [])[] | .state.waiting | select(. != null)
            | "\(.reason // "Waiting"): \((.message // "") | gsub("[\n\r\t]"; " ") | .[0:300])"][0] // ""' "$WORK/pod.json")"
    sched="$(jq -r '[(.status.conditions // [])[] | select(.type == "PodScheduled" and .status == "False")
            | "\(.reason // "Unschedulable"): \((.message // "") | gsub("[\n\r\t]"; " ") | .[0:300])"][0] // ""' "$WORK/pod.json")"
    case "${exit_code:-empty}" in
        empty) ;;
        *[!0-9]*) ;;
        0) echo "pod $name finished, exit 0"; return 0 ;;
        *)
            POD_REASON="exit-$exit_code"
            case "$why" in OOMKilled | Evicted) POD_REASON="$why" ;; esac
            echo "pod $name exited $exit_code (${why:-no reason given})"
            return 0
            ;;
    esac
    if [ -n "$waiting" ]; then
        POD_REASON="${waiting%%:*}"
        echo "pod $name is $phase, its container waiting — $waiting"
    elif [ -n "$sched" ]; then
        POD_REASON="${sched%%:*}"
        echo "pod $name was not scheduled — $sched"
    else
        echo "pod $name is $phase"
    fi
}

# counters <job> — the trim's own before/after discard counters, from
# the pod's termination message, else its log. Sets DEVICES and MOVED and
# prints one COUNTERS line per device.
DEVICES=0
MOVED=0
counters() {
    : > "$WORK/said"
    local name from="the pod's termination message"
    if [ "$(jq -r 'type' "$WORK/pod.json")" = object ]; then
        name="$(jq -r '.metadata.name' "$WORK/pod.json")"
        jq -r '[(.status.containerStatuses // [])[] | .state.terminated.message // empty] | join("\n")' "$WORK/pod.json" > "$WORK/said"
        if ! grep -qE '^node-trim: (before|after) ' "$WORK/said"; then
            from="the pod's log"
            # shellcheck disable=SC2086
            if ! $K logs "$name" -n "$NS" --tail=400 --request-timeout=20s > "$WORK/said" 2> "$WORK/log.err"; then
                echo "$ME: COUNTERS — none read: the termination message of pod $name carries none, and its log could not be read ($(tr '\n' ' ' < "$WORK/log.err" | cut -c1-200))"
                return 0
            fi
        fi
    else
        echo "$ME: COUNTERS — none read: no pod of Job $1 remains to carry them"
        return 0
    fi
    # `node-trim: <before|after> <path>: discards=<n> sectors=<n>`, the
    # trim script's own line. A device counts only with BOTH readings.
    awk '
        $1 == "node-trim:" && ($2 == "before" || $2 == "after") && $4 ~ /^discards=[0-9]+$/ && $5 ~ /^sectors=[0-9]+$/ {
            dev = $3; sub(/:$/, "", dev); sub(/^.*\//, "", dev)
            d = $4; sub(/^discards=/, "", d); s = $5; sub(/^sectors=/, "", s)
            if ($2 == "before") { bd[dev] = d; bs[dev] = s } else { ad[dev] = d; asec[dev] = s }
            if (!(dev in seen)) { seen[dev] = 1; order[++n] = dev }
        }
        END {
            for (i = 1; i <= n; i++) {
                dev = order[i]
                if (!(dev in bd) || !(dev in ad)) continue
                rose = (ad[dev] + 0 > bd[dev] + 0 || asec[dev] + 0 > bs[dev] + 0) ? 1 : 0
                printf "%s %s %s %s %s %d\n", dev, bd[dev], ad[dev], bs[dev], asec[dev], rose
            }
        }' "$WORK/said" > "$WORK/devs"
    DEVICES="$(grep -c . "$WORK/devs")"
    MOVED="$(awk '$6 == 1' "$WORK/devs" | grep -c .)"
    if [ "$DEVICES" -eq 0 ]; then
        echo "$ME: COUNTERS — none read: $from of pod $name carries no before and after discard counters"
        return 0
    fi
    awk -v me="$ME" -v from="$from" '{
        printf "%s: COUNTERS — %s: completed discards %s -> %s (+%d), discarded sectors %s -> %s (+%d)%s [%s]\n",
            me, $1, $2, $3, $3 - $2, $4, $5, $5 - $4, ($6 == 1 ? "" : " — DID NOT MOVE"), from }' "$WORK/devs"
    # The trim script closes with one OK or RED line; RED lines carry
    # its own reason.
    grep -E '^node-trim: (RED|OK)' "$WORK/said" | cut -c1-300 | sed "s/^/$ME: THE TRIM SAID — /"
    return 0
}

# A Job still without a terminal condition: said, never judged.
flight="$(jq -r '.[0] | select(. != null and .done == null) | .name' "$WORK/own.json")"
if [ -n "$flight" ]; then
    pod_of "$flight"
    events_of "$flight"
    pod_state > "$WORK/pod.line"
    echo "$ME: IN FLIGHT — Job $flight (created $(jq -r '.[0].created' "$WORK/own.json")) has not finished: $(cat "$WORK/pod.line")"
    sed "s/^/$ME: IN FLIGHT EVENT — /" "$WORK/job.events"
fi

job="$(jq -r '[.[] | select(.done != null)][0].name // ""' "$WORK/own.json")"
if [ -z "$job" ]; then
    echo "$ME: NO FINISHED JOB — no Job of $CJ has finished among those kept, so there is no run to read an outcome or counters from"
    verdict present "$hours" 0 0 "$last_word" none none NoFinishedJob 0 0
    exit 0
fi
jq --arg j "$job" '[.[] | select(.name == $j)][0]' "$WORK/own.json" > "$WORK/job.json"
kind="$(jq -r '.done.type' "$WORK/job.json")"
pod_of "$job"
events_of "$job"

if [ "$kind" = Complete ]; then
    pod_state > "$WORK/pod.line"
    echo "$ME: NEWEST FINISHED JOB — $job succeeded (started $(jq -r '.started' "$WORK/job.json"), completed $(jq -r '.ended' "$WORK/job.json")): $(cat "$WORK/pod.line")"
    counters "$job"
    unmoved=0
    if [ "$MOVED" -eq 0 ]; then
        unmoved=1
        echo "$ME: NO EVIDENCE — Job $job succeeded and no device's discard counters rose ($DEVICES read): the Job's exit says the trim ran, and nothing here says a drive was told anything"
    fi
    verdict present "$hours" 0 "$unmoved" "$last_word" "$job" succeeded Complete "$DEVICES" "$MOVED"
    exit 0
fi

# --- a failed Job, and why --------------------------------------------------------
job_reason="$(jq -r '.done.reason // "Failed"' "$WORK/job.json")"
job_msg="$(jq -r '(.done.message // "no message") | gsub("[\n\r\t]"; " ") | .[0:300]' "$WORK/job.json")"
# Into a file, not $(…): POD_REASON must be set in THIS shell.
pod_state > "$WORK/pod.line"
state_line="$(cat "$WORK/pod.line")"
reason="$job_reason"
if [ -n "$POD_REASON" ]; then
    reason="$POD_REASON"
elif grep -q ' FailedCreate x' "$WORK/job.events"; then
    reason=FailedCreate
fi
# The token rides a verdict a regex reads: one word of letters, digits
# and hyphens, whatever the cluster wrote.
reason="$(printf '%s' "$reason" | tr -c 'A-Za-z0-9-' '-' | cut -c1-60)"
echo "$ME: NEWEST FINISHED JOB — $job FAILED (created $(jq -r '.created' "$WORK/job.json")): $job_reason — $job_msg"
echo "$ME: CAUSE — $reason: $state_line"
if [ -s "$WORK/job.events" ]; then
    sed "s/^/$ME: EVENT — /" "$WORK/job.events"
else
    echo "$ME: EVENT — none remain for Job $job (events live about an hour). A Job that fails on its deadline with no pod left never ran one to the end: an image that would not pull, a node that was down, and a pod refused at admission all end this way, and which it was is no longer readable here"
fi
counters "$job"
verdict present "$hours" 1 0 "$last_word" "$job" failed "$reason" "$DEVICES" "$MOVED"
exit 0
