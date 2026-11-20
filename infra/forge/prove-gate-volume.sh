#!/usr/bin/env bash
# prove-gate-volume.sh — prove ONE node's gate volume is the gate disk,
# and write the marker run.sh refuses to gate without. The ops verb
# `prove-gate-volume`.
#
#   prove-gate-volume.sh <node>
#
# WHY IT EXISTS (backlog ae59464b; door 3 of 3 of the volume move David
# asked for on 88b35891 and ordered on 2026-11-07 ~02:00Z). run.sh's
# gate-disk guard refuses every gate whose runs directory has no
# /gate-runs/.gate-volume — the marker a proof Job writes once it has
# SHOWN the claim `gate` is one filesystem that is not the install disk's
# EPHEMERAL. The only proof ever run (52ea56ac, 2026-09-30) was a Job made
# by hand from a run, so moving w-1's gate volume onto a new disk would
# have stopped every gate with no door to start them again. The Job is
# now infra/gate-runner/gate-volume-proof.yaml; this script applies it
# through the forge's ops_kubectl door, waits, prints the pod's WHOLE log,
# and judges it.
#
# THE BOUNDS, each a read, all before the one write (exit 78, nothing
# created):
#   - the node is one DNS label, a talos-worker the estate registry
#     declares once, and the cluster agrees (cluster-node-lib.sh) — a
#     control plane is refused before any door opens;
#   - the node is not cordoned (the proof's pod could not be scheduled);
#   - claim boss-dev/gate exists and is Bound, and its PersistentVolume's
#     node affinity names this node;
#   - no gate is in flight: no pod in boss-dev that is not Succeeded or
#     Failed mounts the claim (a gate, its seed refresh), and no Job
#     labelled app=gate-runner is unfinished (admitted, no pod yet). A
#     proof beside a running gate would race a workspace on the volume it
#     is judging; the volume move holds gate launches first;
#   - no UNFINISHED Job named gate-volume-proof stands in boss-dev: one
#     still running may be writing the marker, and is never touched.
#
# IT REMOVES ONE JOB, AND ONLY A FINISHED PREVIOUS PROOF (backlog ae59464b
# decided it). The Job's name is fixed, so the API server refuses a second
# proof while one stands, and its ttlSecondsAfterFinished is an hour — the
# floor every_job_bounds_its_finished_history.rs holds every Job to, so a
# failed proof's log outlives the failure. An hour is too long for a
# retry to wait while every gate is stopped, so when a previous proof
# stands FINISHED (Complete or Failed) this verb first prints that Job's
# whole log onto its own run — capture before delete, the log the TTL
# exists to keep — and then deletes it with DeleteOptions
# preconditions.uid, the uid it just read, and reads the name back gone
# before it creates anything. Any other object, a running proof, or a
# name that changed hands between the read and the delete is left alone.
# It is declared in the tree, so delete-orphan-object refuses it by
# design: this is the one door that removes it early.
#
# THE EFFECT IS READ BACK THROUGH THE PROOF'S OWN LOG, and judged here
# again rather than trusted: the Job must be Complete, its pod must have
# exited 0 ON THIS NODE, the log must carry one devices line whose seed
# and runs devices are equal and not EPHEMERAL's, one PROOF-OK line, and
# one marker line naming this node and that device. Only then is the line
# the verb file declares as `effect` printed. Anything else is FAILED,
# with the log already printed above it.
#
# Exit 78 is a refusal (nothing was created); exit 1 a proof that failed,
# a Job that never ran, or a cluster that could not be read.
set -uo pipefail

ME=prove-gate-volume
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

NS=boss-dev
CLAIM=gate
JOB=gate-volume-proof
MANIFEST="$HERE/../gate-runner/gate-volume-proof.yaml"
POLL_S=5
MARGIN_S=60
# Consecutive polls that could not read the cluster before the wait
# gives up: half a minute of a dark API server is a blip, not an answer.
UNREAD_MAX=6

words() { tr '\n' ' ' < "$1" | cut -c1-600; }

[ $# -eq 1 ] || node_refuse "usage: prove-gate-volume.sh <node> — one node, nothing else (got $# arguments)"
NODE="$1"
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the cluster's answers cannot be read, and nothing was created"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory, and nothing was created"
trap 'rm -rf "$WORK"' EXIT

# --- what this checkout declares ----------------------------------------
[ -r "$MANIFEST" ] || node_refuse "this checkout carries no infra/gate-runner/gate-volume-proof.yaml — there is no declared proof to run"
DEADLINE="$(sed -n 's/^  activeDeadlineSeconds: *//p' "$MANIFEST")"
case "${DEADLINE:-empty}" in
    empty | *[!0-9]*) node_refuse "the proof manifest does not declare exactly one whole-second activeDeadlineSeconds (read: '$(printf '%s' "$DEADLINE" | tr '\n' ' ')')" ;;
esac
TTL="$(sed -n 's/^  ttlSecondsAfterFinished: *//p' "$MANIFEST")"
[ "$(grep -v '^ *#' "$MANIFEST" | grep -c '[$]GATE_PROOF_NODE')" = 1 ] \
    || node_refuse "the proof manifest does not carry exactly one code line naming GATE_PROOF_NODE — the node it would be pinned to cannot be filled in"
# The node is one DNS label (node_check_id), so it is safe inside sed.
sed "s/[$]GATE_PROOF_NODE/$NODE/g" "$MANIFEST" > "$WORK/job.yaml"
WAIT_S=$((DEADLINE + MARGIN_S))

# --- the node -----------------------------------------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
node_read_cluster "$NODE"
[ "$(node_unschedulable)" = false ] \
    || node_refuse "$NODE is cordoned (spec.unschedulable=true) — the proof's pod could not be scheduled there; uncordon it first (uncordon-node $NODE)"

# kget <out-file> <kubectl get args…> — one bounded read into a file.
# Status is kubectl's; its words are in $WORK/k.err.
kget() {
    local out="$1"
    shift
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K get "$@" --request-timeout=20s > "$out" 2> "$WORK/k.err"
}

# --- the claim ----------------------------------------------------------
kget "$WORK/pvc.json" pvc "$CLAIM" -n "$NS" --ignore-not-found -o json \
    || node_fail "could not read claim $NS/$CLAIM: $(words "$WORK/k.err")— nothing was created"
jq_doc_file "$WORK/pvc.json" \
    || node_refuse "claim $NS/$CLAIM does not exist — gate-seed-local.yaml declares it; the converge has not applied it"
PHASE="$(jq -r '.status.phase // "no phase"' "$WORK/pvc.json")"
[ "$PHASE" = Bound ] \
    || node_refuse "claim $NS/$CLAIM is $PHASE, not Bound — there is no volume to prove yet"
PV="$(jq -r '.spec.volumeName // ""' "$WORK/pvc.json")"
[[ "$PV" =~ ^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]] \
    || node_refuse "claim $NS/$CLAIM is Bound but names the volume '$PV', which is not a volume name"
kget "$WORK/pv.json" pv "$PV" --ignore-not-found -o json \
    || node_fail "could not read PersistentVolume $PV: $(words "$WORK/k.err")— nothing was created"
jq_doc_file "$WORK/pv.json" \
    || node_refuse "claim $NS/$CLAIM is Bound to PersistentVolume $PV, which does not exist"
PV_NODES="$(jq -r '[.spec.nodeAffinity.required.nodeSelectorTerms[]?.matchExpressions[]?
        | select(.key == "kubernetes.io/hostname" and .operator == "In") | .values[]] | unique | join(",")' "$WORK/pv.json" 2>/dev/null)"
[ "$PV_NODES" = "$NODE" ] \
    || node_refuse "claim $NS/$CLAIM is Bound to PersistentVolume $PV, whose node affinity names '${PV_NODES:-no node}', not $NODE — the gate volume is not on the node this verb was asked to prove"
PV_PATH="$(jq -r '.spec.local.path // "no local path"' "$WORK/pv.json")"

# --- nothing in flight --------------------------------------------------
kget "$WORK/pods.json" pods -n "$NS" -o json \
    || node_fail "could not list the pods in $NS: $(words "$WORK/k.err")— nothing was created"
kget "$WORK/gatejobs.json" jobs -n "$NS" -l app=gate-runner -o json \
    || node_fail "could not list the gate Jobs in $NS: $(words "$WORK/k.err")— nothing was created"
if ! jq_doc_file "$WORK/pods.json" || ! jq_doc_file "$WORK/gatejobs.json" \
        || ! busy="$(jq -r -n --arg c "$CLAIM" --slurpfile p "$WORK/pods.json" --slurpfile j "$WORK/gatejobs.json" '
            [ ($p[0].items[] | select((.status.phase // "") != "Succeeded" and (.status.phase // "") != "Failed")
                | select(any(.spec.volumes[]?; .persistentVolumeClaim.claimName? == $c))
                | "pod \(.metadata.name) (\(.status.phase // "no phase")\(if .spec.nodeName then " on " + .spec.nodeName else "" end))"),
              ($j[0].items[] | select(([.status.conditions[]? | select((.type == "Complete" or .type == "Failed") and .status == "True")] | length) == 0)
                | "gate Job \(.metadata.name) (unfinished, active \(.status.active // 0))") ] | join("; ")' 2> "$WORK/busy.err")"; then
    node_fail "the pod and Job lists of $NS would not parse ($(words "$WORK/busy.err")) — nothing was created"
fi
[ -z "$busy" ] \
    || node_refuse "a gate is in flight on claim $NS/$CLAIM: $busy. The proof would race a workspace on the volume it judges; hold gate launches and let the bays empty first"

# job_log <job uid> <label> — print the WHOLE log of the newest pod of the
# Job with that uid (by its controller-uid label, so a lingering pod of an
# earlier Job of the same name is never read as this one's), into $LOG and
# indented onto stdout. Sets POD, RAN_ON and EXIT_CODE ("" when unread).
job_log() {
    POD=""
    RAN_ON=""
    EXIT_CODE=""
    : > "$LOG"
    if kget "$WORK/jp.json" pods -n "$NS" -l "batch.kubernetes.io/controller-uid=$1" -o json && jq_doc_file "$WORK/jp.json"; then
        POD="$(jq -r '.items | sort_by(.metadata.creationTimestamp) | last | .metadata.name // empty' "$WORK/jp.json" 2>/dev/null)"
        RAN_ON="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | .spec.nodeName // empty' "$WORK/jp.json" 2>/dev/null)"
        EXIT_CODE="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | [.status.containerStatuses[]? | .state.terminated.exitCode | select(. != null)] | last // empty' "$WORK/jp.json" 2>/dev/null)"
    fi
    if [ -z "$POD" ]; then
        echo "$ME: $2 (Job uid $1) left no pod this run could read, so there is no log to print"
        return 0
    fi
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K logs "$POD" -n "$NS" -c proof --request-timeout=20s > "$LOG" 2> "$WORK/logs.err"; then
        echo "$ME: $2's own output (the whole log of pod $POD, on ${RAN_ON:-an unread node}):"
        sed 's/^/    /' "$LOG"
    else
        : > "$LOG"
        echo "$ME: the log of pod $POD ($2) could not be read: $(words "$WORK/logs.err")"
    fi
}
LOG="$WORK/log"

# --- no proof running; a finished one is captured and removed -------------
kget "$WORK/old.json" job "$JOB" -n "$NS" --ignore-not-found -o json \
    || node_fail "could not read Job $NS/$JOB: $(words "$WORK/k.err")— nothing was created"
STOOD="none"
if jq_doc_file "$WORK/old.json"; then
    OLD_UID="$(jq -r '.metadata.uid // empty' "$WORK/old.json" 2>/dev/null)"
    OLD_DONE="$(jq -r '[.status.conditions[]? | select((.type == "Complete" or .type == "Failed") and .status == "True") | .type] | first // empty' "$WORK/old.json" 2>/dev/null)"
    OLD_SAID="uid ${OLD_UID:-unread}, created $(jq -r '.metadata.creationTimestamp // "unread"' "$WORK/old.json" 2>/dev/null)"
    [ -n "$OLD_DONE" ] \
        || node_refuse "Job $NS/$JOB already stands and has NOT finished ($OLD_SAID) — it may still be writing the marker, and this verb never touches a running proof. It ends by its deadline (${DEADLINE}s); run this again then"
    [ -n "$OLD_UID" ] \
        || node_fail "Job $NS/$JOB stands finished ($OLD_DONE) but its uid could not be read, so it cannot be removed safely — nothing was created"
    echo "$ME: a previous proof stands, finished $OLD_DONE ($OLD_SAID); its whole log is printed here before it is removed, because its ttlSecondsAfterFinished (${TTL:-unread}s) is what would otherwise have kept it"
    job_log "$OLD_UID" "the previous proof"
    jq -n --arg uid "$OLD_UID" '{kind: "DeleteOptions", apiVersion: "v1", propagationPolicy: "Background", preconditions: {uid: $uid}}' > "$WORK/del.json"
    read -r -a KI <<< "$(ops_kubectl --stdin)"
    if ! "${KI[@]}" delete --raw "/apis/batch/v1/namespaces/$NS/jobs/$JOB" -f - --request-timeout=30s \
            < "$WORK/del.json" > "$WORK/del.out" 2> "$WORK/del.err"; then
        node_fail "the delete of the previous proof Job $NS/$JOB (uid $OLD_UID) was refused or failed: $(words "$WORK/del.err")— a precondition refusal means another Job now holds the name; nothing was created"
    fi
    # Gone is a READ: the name answering nothing, or another uid.
    gone=""
    for _ in 1 2 3 4 5 6; do
        if kget "$WORK/back.json" job "$JOB" -n "$NS" --ignore-not-found -o json; then
            if ! jq_doc_file "$WORK/back.json"; then gone=1; break; fi
            [ "$(jq -r '.metadata.uid // empty' "$WORK/back.json" 2>/dev/null)" = "$OLD_UID" ] || { gone=1; break; }
        fi
        sleep "$POLL_S"
    done
    [ -n "$gone" ] \
        || node_fail "the previous proof Job $NS/$JOB (uid $OLD_UID) was deleted and is not read back gone — nothing was created; read it with kubectl -n $NS get job $JOB"
    echo "$ME: removed the previous proof Job $NS/$JOB (uid $OLD_UID; read back gone)"
    STOOD="the previous one (uid $OLD_UID, $OLD_DONE) captured and removed"
fi

# --- the one write ------------------------------------------------------
echo "$ME: every bound holds — $NODE is a talos-worker at $NODE_ADDRESS, not cordoned; claim $NS/$CLAIM is Bound to $PV ($PV_PATH) on $NODE; no gate in flight on it; no proof running ($STOOD). Creating Job $NS/$JOB from infra/gate-runner/gate-volume-proof.yaml, pinned to $NODE."
read -r -a KI <<< "$(ops_kubectl --stdin)"
if ! "${KI[@]}" create -f - -n "$NS" -o json --request-timeout=30s \
        < "$WORK/job.yaml" > "$WORK/created.json" 2> "$WORK/create.err"; then
    node_fail "the API server did not create Job $NS/$JOB: $(words "$WORK/create.err")— read it with kubectl -n $NS get job $JOB before running this again"
fi
UID_MADE="$(jq -r '.metadata.uid // empty' "$WORK/created.json" 2>/dev/null)"
[ -n "$UID_MADE" ] \
    || node_fail "the create of Job $NS/$JOB answered success but gave no uid ($(head -c 200 "$WORK/created.json" | tr '\n' ' ')) — what it made cannot be told from another Job of that name"
echo "$ME: created Job $NS/$JOB (uid $UID_MADE); waiting up to ${WAIT_S}s (its own deadline of ${DEADLINE}s and ${MARGIN_S}s for the controller)"

# --- the wait -----------------------------------------------------------
t0=$SECONDS
polls=0
dark=0
OUTCOME=""
WHY=""
while :; do
    # The polls' count, or the wall clock when the reads themselves were
    # slow — whichever is further on.
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
            WHY="the cluster could not be read $UNREAD_MAX times running ($(words "$WORK/k.err")) — whether the proof ran is UNKNOWN"
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

# --- the proof's own record: its WHOLE log --------------------------------
POD=""
RAN_ON=""
EXIT_CODE=""
: > "$LOG"
if [ "$OUTCOME" != dark ] && [ "$OUTCOME" != gone ]; then
    job_log "$UID_MADE" "the proof"
fi

# --- the verdict --------------------------------------------------------
[ "$OUTCOME" = complete ] || node_fail "the proof did not succeed: $WHY. No marker is vouched for by this run; gates stay refused until a proof succeeds"

reason=""
devices="$(sed -n 's/^gate-volume-proof: devices gate-seed=\([0-9][0-9]*\) gate-runs=\([0-9][0-9]*\) etc-hosts(EPHEMERAL)=\([0-9][0-9]*\)$/\1 \2 \3/p' "$LOG")"
read -r d_seed d_runs d_eph extra <<< "$devices"
marker="$(sed -n 's|^gate-volume-proof: marker /gate-runs/\.gate-volume reads: \(proven .*\)$|\1|p' "$LOG")"
m_node="$(printf '%s\n' "$marker" | sed -n 's/^proven [^ ]* node=\([^ ]*\) .*/\1/p')"
m_dev="$(printf '%s\n' "$marker" | sed -n 's/.* device=\([0-9][0-9]*\) .*/\1/p')"
if [ "${EXIT_CODE:-unread}" != 0 ]; then
    reason="the Job says Complete but its pod's exit code reads '${EXIT_CODE:-unread}'"
elif [ "$RAN_ON" != "$NODE" ]; then
    reason="its pod ran on '${RAN_ON:-an unread node}', not $NODE"
elif [ "$(printf '%s\n' "$devices" | grep -c .)" != 1 ] || [ -n "${extra:-}" ]; then
    reason="its log does not carry exactly one devices line"
elif [ "$d_seed" != "$d_runs" ]; then
    reason="its log reads the seed on device $d_seed and the runs directory on device $d_runs"
elif [ "$d_runs" = "$d_eph" ]; then
    reason="its log reads the runs directory on EPHEMERAL's device ($d_eph)"
elif [ "$(grep -c '^gate-volume-proof: PROOF-OK: ' "$LOG")" != 1 ]; then
    reason="its log does not carry exactly one PROOF-OK line"
elif [ "$(printf '%s\n' "$marker" | grep -c .)" != 1 ]; then
    reason="its log does not read the marker /gate-runs/.gate-volume back exactly once"
elif [ "$m_node" != "$NODE" ]; then
    reason="the marker it read back names node '${m_node:-none}', not $NODE"
elif [ "$m_dev" != "$d_runs" ]; then
    reason="the marker it read back names device '${m_dev:-none}', not the runs directory's $d_runs"
fi
[ -z "$reason" ] || node_fail "Job $NS/$JOB completed, and the proof is not vouched for: $reason. Its whole log is above"

echo "$ME: PROVEN $NODE device $d_runs marker /gate-runs/.gate-volume — Job $JOB completed on $RAN_ON, and its log reads the marker back: proven ${marker#proven }"
