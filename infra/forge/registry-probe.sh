#!/usr/bin/env bash
# registry-probe.sh — prove ONE worker pulls from the forge registry, by
# running the tree's one-shot Job on it and removing it. The ops verb
# `registry-probe`.
#
#   registry-probe.sh <node>
#
# WHY IT EXISTS (backlog 01927489). join-a-node's `verify-registry` step
# asks for `image_pulled`. w-2's join (829f75a8) answered it with a Job
# David wrote and applied by hand, and w-3's join (aca68557) stood at that
# step for want of a door. The Job is now infra/forge/registry-probe.yaml;
# this script applies it through the forge's ops_kubectl door, waits,
# prints the pod's WHOLE log and the kubelet's pull events, removes the Job
# by its uid, and judges what it read.
#
# THE BOUNDS, each a read, all before the one write (exit 78, nothing
# created) — prove-gate-volume's, which this verb is shaped on:
#   - the node is one DNS label, a talos-worker the estate registry
#     declares once, and the cluster agrees (cluster-node-lib.sh) — a
#     control plane is refused before any door opens;
#   - the node is not cordoned (the probe's pod could not be scheduled);
#   - BOSS_FORGE_REGISTRY_HOST names one host[:port] (the image is that
#     registry's, and no address is spelled in the tree);
#   - no UNFINISHED Job named registry-probe stands in boss-dev — one still
#     running is never touched; it ends by its own deadline.
#
# WHAT IT WRITES, AND ONLY THAT. One create: the tree's Job, its node and
# registry filled in. Two deletes at most, each of a Job named
# registry-probe in boss-dev by the uid just read (DeleteOptions
# preconditions.uid — a name that changed hands is refused by the API
# server, never deleted) and read back gone: a FINISHED previous probe,
# after its whole log is printed onto this run (capture before delete),
# and the Job this run made, after its log is printed — whatever the
# probe's outcome, because the probe is this verb's own and its record is
# the run, not the Job. The Job's ttlSecondsAfterFinished (an hour) is the
# backstop for a run that dies before its delete.
#
# THE EFFECT is printed only when the Job is Complete, its pod exited 0 ON
# THIS NODE, the kubelet reports the image by the manifest's digest, the
# log carries the probe's one line naming this node, and the Job is read
# back gone. Anything else is FAILED (exit 1), with the log above it.
set -uo pipefail

ME=registry-probe
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

NS=boss-dev
JOB=registry-probe
MANIFEST="$HERE/registry-probe.yaml"
POLL_S=5
MARGIN_S=60
UNREAD_MAX=6

words() { tr '\n' ' ' < "$1" | cut -c1-600; }

[ $# -eq 1 ] || node_refuse "usage: registry-probe.sh <node> — one node, nothing else (got $# arguments)"
NODE="$1"
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the cluster's answers cannot be read, and nothing was created"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory, and nothing was created"
trap 'rm -rf "$WORK"' EXIT

# --- what this checkout declares ----------------------------------------
[ -r "$MANIFEST" ] || node_refuse "this checkout carries no infra/forge/registry-probe.yaml — there is no declared probe to run"
DEADLINE="$(sed -n 's/^  activeDeadlineSeconds: *//p' "$MANIFEST")"
case "${DEADLINE:-empty}" in
    empty | *[!0-9]*) node_refuse "the probe manifest does not declare exactly one whole-second activeDeadlineSeconds (read: '$(printf '%s' "$DEADLINE" | tr '\n' ' ')')" ;;
esac
for word in REGISTRY_PROBE_NODE BOSS_FORGE_REGISTRY_HOST; do
    [ "$(grep -v '^ *#' "$MANIFEST" | grep -c "[\$]$word")" = 1 ] \
        || node_refuse "the probe manifest does not carry exactly one code line naming $word — it cannot be filled in"
done
REG="${BOSS_FORGE_REGISTRY_HOST:-}"
[[ "$REG" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]] \
    || node_refuse "BOSS_FORGE_REGISTRY_HOST is '${REG}', not one registry host[:port] — it comes from /etc/boss/sor.env, rendered from infra/estate/estate.toml; without it there is no image to name"
# Both words are bounded above (a DNS label; a host[:port]), so safe in sed.
sed -e "s/[\$]REGISTRY_PROBE_NODE/$NODE/g" -e "s/[\$]BOSS_FORGE_REGISTRY_HOST/$REG/g" "$MANIFEST" > "$WORK/job.yaml"
IMAGE="$(sed -n 's/^ *image: *//p' "$WORK/job.yaml")"
DIGEST="${IMAGE##*@}"
[[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || node_refuse "the probe manifest names its image '$IMAGE', not one image by sha256 digest — a pull that cannot be held to a digest proves less than it says"
WAIT_S=$((DEADLINE + MARGIN_S))

# --- the node -----------------------------------------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
node_read_cluster "$NODE"
[ "$(node_unschedulable)" = false ] \
    || node_refuse "$NODE is cordoned (spec.unschedulable=true) — the probe's pod could not be scheduled there; uncordon it first (uncordon-node $NODE)"

# kget <out-file> <kubectl get args…> — one bounded read into a file.
kget() {
    local out="$1"
    shift
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K get "$@" --request-timeout=20s > "$out" 2> "$WORK/k.err"
}

LOG="$WORK/log"
# job_log <job uid> <label> — print the WHOLE log of the newest pod of the
# Job with that uid (by its controller-uid label, so a lingering pod of an
# earlier Job of the same name is never read as this one's), and the
# kubelet's events for that pod. Sets POD, RAN_ON, EXIT_CODE and IMAGE_ID
# ("" when unread).
job_log() {
    POD=""
    RAN_ON=""
    EXIT_CODE=""
    IMAGE_ID=""
    : > "$LOG"
    if kget "$WORK/jp.json" pods -n "$NS" -l "batch.kubernetes.io/controller-uid=$1" -o json && jq_doc_file "$WORK/jp.json"; then
        POD="$(jq -r '.items | sort_by(.metadata.creationTimestamp) | last | .metadata.name // empty' "$WORK/jp.json" 2>/dev/null)"
        RAN_ON="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | .spec.nodeName // empty' "$WORK/jp.json" 2>/dev/null)"
        EXIT_CODE="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | [.status.containerStatuses[]? | .state.terminated.exitCode | select(. != null)] | last // empty' "$WORK/jp.json" 2>/dev/null)"
        IMAGE_ID="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | [.status.containerStatuses[]? | .imageID // empty] | last // empty' "$WORK/jp.json" 2>/dev/null)"
    fi
    if [ -z "$POD" ]; then
        echo "$ME: $2 (Job uid $1) left no pod this run could read, so there is no log to print"
        return 0
    fi
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K logs "$POD" -n "$NS" -c probe --request-timeout=20s > "$LOG" 2> "$WORK/logs.err"; then
        echo "$ME: $2's own output (the whole log of pod $POD, on ${RAN_ON:-an unread node}):"
        sed 's/^/    /' "$LOG"
    else
        : > "$LOG"
        echo "$ME: the log of pod $POD ($2) could not be read: $(words "$WORK/logs.err")"
    fi
    # The kubelet's own account of the pull. Evidence, not the verdict:
    # events are rate-limited and expire, so an unread one is said, not failed.
    if kget "$WORK/ev.json" events -n "$NS" --field-selector "involvedObject.name=$POD" -o json && jq_doc_file "$WORK/ev.json"; then
        jq -r '[.items[]? | select(.reason == "Pulling" or .reason == "Pulled" or .reason == "Failed" or .reason == "BackOff" or .reason == "ErrImagePull")]
            | sort_by(.lastTimestamp // .eventTime // "")[] | "event \(.reason): \(.message // "")"' "$WORK/ev.json" 2>/dev/null
    else
        echo "$ME: the events of pod $POD could not be read: $(words "$WORK/k.err")"
    fi
}

# remove_job <uid> <label> — delete Job $NS/$JOB by that uid and read the
# name back gone (answering nothing, or another uid). Returns 1 with WHY
# set when it could not.
remove_job() {
    local uid="$1" gone=""
    jq -n --arg uid "$uid" '{kind: "DeleteOptions", apiVersion: "v1", propagationPolicy: "Background", preconditions: {uid: $uid}}' > "$WORK/del.json"
    read -r -a KI <<< "$(ops_kubectl --stdin)"
    if ! "${KI[@]}" delete --raw "/apis/batch/v1/namespaces/$NS/jobs/$JOB" -f - --request-timeout=30s \
            < "$WORK/del.json" > "$WORK/del.out" 2> "$WORK/del.err"; then
        WHY="the delete of $2 Job $NS/$JOB (uid $uid) was refused or failed: $(words "$WORK/del.err")"
        return 1
    fi
    for _ in 1 2 3 4 5 6; do
        if kget "$WORK/back.json" job "$JOB" -n "$NS" --ignore-not-found -o json; then
            if ! jq_doc_file "$WORK/back.json"; then gone=1; break; fi
            [ "$(jq -r '.metadata.uid // empty' "$WORK/back.json" 2>/dev/null)" = "$uid" ] || { gone=1; break; }
        fi
        sleep "$POLL_S"
    done
    if [ -z "$gone" ]; then
        WHY="$2 Job $NS/$JOB (uid $uid) was deleted and is not read back gone — read it with kubectl -n $NS get job $JOB; its ttlSecondsAfterFinished removes it an hour after it finished"
        return 1
    fi
    echo "$ME: removed $2 Job $NS/$JOB (uid $uid; read back gone)"
}

# --- no probe running; a finished one is captured and removed -------------
kget "$WORK/old.json" job "$JOB" -n "$NS" --ignore-not-found -o json \
    || node_fail "could not read Job $NS/$JOB: $(words "$WORK/k.err")— nothing was created"
STOOD="none"
if jq_doc_file "$WORK/old.json"; then
    OLD_UID="$(jq -r '.metadata.uid // empty' "$WORK/old.json" 2>/dev/null)"
    OLD_DONE="$(jq -r '[.status.conditions[]? | select((.type == "Complete" or .type == "Failed") and .status == "True") | .type] | first // empty' "$WORK/old.json" 2>/dev/null)"
    OLD_SAID="uid ${OLD_UID:-unread}, created $(jq -r '.metadata.creationTimestamp // "unread"' "$WORK/old.json" 2>/dev/null)"
    [ -n "$OLD_DONE" ] \
        || node_refuse "Job $NS/$JOB already stands and has NOT finished ($OLD_SAID) — another probe is running, and this verb never touches a running probe. It ends by its deadline (${DEADLINE}s); run this again then"
    [ -n "$OLD_UID" ] \
        || node_fail "Job $NS/$JOB stands finished ($OLD_DONE) but its uid could not be read, so it cannot be removed safely — nothing was created"
    echo "$ME: a previous probe stands, finished $OLD_DONE ($OLD_SAID); its whole log is printed here before it is removed"
    job_log "$OLD_UID" "the previous probe"
    remove_job "$OLD_UID" "the previous probe" || node_fail "$WHY — nothing was created"
    STOOD="the previous one (uid $OLD_UID, $OLD_DONE) captured and removed"
fi

# --- the one create -------------------------------------------------------
echo "$ME: every bound holds — $NODE is a talos-worker at $NODE_ADDRESS, not cordoned; no probe running ($STOOD). Creating Job $NS/$JOB from infra/forge/registry-probe.yaml, pinned to $NODE, pulling $IMAGE (imagePullPolicy Always)."
read -r -a KI <<< "$(ops_kubectl --stdin)"
if ! "${KI[@]}" create -f - -n "$NS" -o json --request-timeout=30s \
        < "$WORK/job.yaml" > "$WORK/created.json" 2> "$WORK/create.err"; then
    node_fail "the API server did not create Job $NS/$JOB: $(words "$WORK/create.err")— read it with kubectl -n $NS get job $JOB before running this again"
fi
UID_MADE="$(jq -r '.metadata.uid // empty' "$WORK/created.json" 2>/dev/null)"
[ -n "$UID_MADE" ] \
    || node_fail "the create of Job $NS/$JOB answered success but gave no uid ($(head -c 200 "$WORK/created.json" | tr '\n' ' ')) — what it made cannot be told from another Job of that name; its ttlSecondsAfterFinished removes it an hour after it finishes"
echo "$ME: created Job $NS/$JOB (uid $UID_MADE); waiting up to ${WAIT_S}s (its own deadline of ${DEADLINE}s and ${MARGIN_S}s for the controller)"

# --- the wait -----------------------------------------------------------
t0=$SECONDS
polls=0
dark=0
OUTCOME=""
WHY=""
while :; do
    waited=$((polls * POLL_S))
    [ $((SECONDS - t0)) -le "$waited" ] || waited=$((SECONDS - t0))
    if kget "$WORK/job.json" job "$JOB" -n "$NS" --ignore-not-found -o json; then
        dark=0
        if [ "$(jq -r '.metadata.uid // empty' "$WORK/job.json" 2>/dev/null)" != "$UID_MADE" ]; then
            OUTCOME=gone
            WHY="Job $NS/$JOB (uid $UID_MADE) was removed, or replaced, while this run waited on it"
            break
        fi
        state="$(jq -r '[.status.conditions[]? | select(.status == "True")] as $c
            | if ($c | map(select(.type == "Failed")) | length) > 0 then ($c | map(select(.type == "Failed"))[0] | "failed\t\(.reason // "?"): \(.message // "")")
              elif ($c | map(select(.type == "Complete")) | length) > 0 then "complete\t"
              else "running\t" end' "$WORK/job.json" 2>/dev/null)"
        case "$state" in
            complete*) OUTCOME=complete; break ;;
            failed*) OUTCOME=failed; WHY="the Job's Failed condition says $(printf '%s' "$state" | cut -f2)"; break ;;
        esac
    else
        dark=$((dark + 1))
        if [ "$dark" -ge "$UNREAD_MAX" ]; then
            OUTCOME=dark
            WHY="the cluster could not be read $UNREAD_MAX times running ($(words "$WORK/k.err")) — whether the probe ran is UNKNOWN"
            break
        fi
    fi
    if [ "$waited" -ge "$WAIT_S" ]; then
        OUTCOME=timeout
        WHY="it had neither completed nor failed after ${waited}s, past its own deadline of ${DEADLINE}s"
        break
    fi
    sleep "$POLL_S"
    polls=$((polls + 1))
done

# --- the probe's own record, then its removal -----------------------------
POD=""
RAN_ON=""
EXIT_CODE=""
IMAGE_ID=""
: > "$LOG"
REMOVED=""
REMOVE_WHY=""
if [ "$OUTCOME" != gone ]; then
    [ "$OUTCOME" = dark ] || job_log "$UID_MADE" "the probe"
    PROBE_WHY="$WHY"
    if remove_job "$UID_MADE" "this run's"; then REMOVED=1; else REMOVE_WHY="$WHY"; fi
    WHY="$PROBE_WHY"
fi

# --- the verdict --------------------------------------------------------
[ "$OUTCOME" = complete ] || node_fail "the probe did not show $NODE pulling $IMAGE: $WHY${REMOVE_WHY:+; and $REMOVE_WHY}"

reason=""
if [ "${EXIT_CODE:-unread}" != 0 ]; then
    reason="the Job says Complete but its pod's exit code reads '${EXIT_CODE:-unread}'"
elif [ "$RAN_ON" != "$NODE" ]; then
    reason="its pod ran on '${RAN_ON:-an unread node}', not $NODE"
elif [ "${IMAGE_ID##*@}" != "$DIGEST" ]; then
    reason="the kubelet reports the image as '${IMAGE_ID:-unread}', not the digest $DIGEST the manifest names"
elif [ "$(grep -c "^registry-probe: ran on node $NODE, pod $POD, " "$LOG")" != 1 ]; then
    reason="its log does not carry exactly one line saying it ran on node $NODE as pod $POD"
fi
[ -z "$reason" ] || node_fail "Job $NS/$JOB completed, and the pull is not vouched for: $reason. Its whole log is above${REMOVE_WHY:+; and $REMOVE_WHY}"
[ -n "$REMOVED" ] || node_fail "the probe pulled $IMAGE on $NODE, and the run did not clean up after itself: $REMOVE_WHY"

echo "$ME: PULLED $IMAGE on $NODE — pod $POD pulled it from $REG and exited 0 on $RAN_ON (imageID $IMAGE_ID); Job $NS/$JOB (uid $UID_MADE) removed and read back gone"
