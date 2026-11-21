#!/usr/bin/env bash
# drill-gcs-restore.sh — restore the newest offsite dump into a scratch
# database on the data node's postgres disk and compare it with the live
# system of record. TWO ops verbs, one script:
#   drill-gcs-restore    (no argument) — the bounds, then the create; it
#                        RETURNS with the drill running;
#   read-restore-drill   (--read) — reads the drill: progress while it
#                        runs, removing nothing; once it has finished,
#                        the whole logs, the placement, the verdict, and
#                        only then the removal.
#
# WHY IT EXISTS (backlog 156eb6e5; design 3c475a19 car 4, the Postgres
# move to w-3). That car's risk section asks for a verified backup
# first: "a one-off restore of the newest dump into a scratch database on
# w-3's postgres disk, compared with the live system of record on row
# counts of audit_log, jobs, steps and the newest audit id, recorded on
# the car. A backup nobody has restored is a belief." check-gcs-backup
# proves the newest boss-<stamp>.sql.gz is whole; nothing restored one.
#
# WHY TWO VERBS (backlog a1cbd16c). The first real run, ops-request
# 24fd49e7 (2026-11-14 20:25-21:11Z), was one verb that created the Job
# and WAITED on it — its deadline plus 120 s — inside the forge's single
# ops-runner lane, so every forge request (the converge after a merge
# among them) queued behind it for 47 minutes (alarm 58c101a7). It then
# failed on the Job's DeadlineExceeded at 2700 s, and the Job controller,
# which deletes the pod it was running on DeadlineExceeded, had already
# taken the restore container's log: the post-wait reads found no pod,
# and the removal took the rest. The record could not say how far the
# 7.5 GB dump's restore got. So the start returns (the converge verb's
# `systemctl start --no-block` shape), the read is a second request the
# start's answer names, the pod ends itself at an inner deadline below
# the Job's backstop (restore-drill-pod.sh), and nothing is removed
# before the logs are on the request. No clock rule files the read: the
# start's answer names it, and a finished drill waits a day for it (the
# Job's ttlSecondsAfterFinished).
#
# WHAT THE START DOES. Through the forge's ops_kubectl door it creates
# ONE Job, boss/boss-restore-drill, from this checkout's
# infra/forge/restore-drill-job.yaml (the forge registry host filled in,
# nothing else), and one ConfigMap of the same name carrying the two
# scripts the pod runs (infra/forge/gcs-readback.py and
# infra/forge/restore-drill-pod.sh), owned by that Job. The Job's pod:
#   - is pinned by nodeSelector boss.dev/purpose=database (the label the
#     estate registry gives w-3 and the postgres move uses);
#   - gets a scratch claim as a generic EPHEMERAL volume of StorageClass
#     longhorn-restore-drill (one replica, diskSelector postgres —
#     Longhorn disk u-postgres), which Kubernetes creates for the pod
#     and deletes with it;
#   - `fetch` (initContainer) is gcs-readback.py, the same selection and
#     validation check-gcs-backup runs, keeping the pinned bytes it
#     verified as the dump. The Secret boss/boss-gcs-offsite is MOUNTED
#     into that container (0400, the uploader's own mount) — this host
#     never reads its value at all;
#   - `restore` initdb's a fresh postgres 16 (the dump's own image) on the
#     claim, restores the dump with ON_ERROR_STOP, printing progress and
#     bounded by its inner deadline, counts audit_log, jobs and steps and
#     reads max(audit id), then reads the same four from the LIVE system
#     of record READ-ONLY (Secret boss-secrets database-url as an
#     environment variable, never printed) and whether the live log
#     still holds the dump's newest audit id.
# It never execs into, reads the volume of, or writes to the live
# database or its Pod: the only live contact is read-only SQL sessions.
#
# THE BOUNDS, all reads, all before the one create (exit 78, nothing
# created): the StorageClass is the tree's (driver.longhorn.io, one
# replica, diskSelector postgres); exactly one Node carries
# boss.dev/purpose=database and it is not cordoned; both Secrets exist
# (read by NAME only — jsonpath {.metadata.name}); and NO drill stands.
# A finished one is not removed by the start: its log is the record, and
# only the read, which puts it on a request first, removes it.
#
# WHAT IT REMOVES, AND ONLY THAT. The Job it made, by its uid
# (DeleteOptions preconditions.uid, propagationPolicy Foreground): the
# start on a failure after its create (an EXIT trap, TERM included); the
# read once the drill has finished and its logs and placement are
# printed — or once its pod has waited on a reason it cannot start from
# (an image it cannot pull) for STUCK_AFTER_S, when it has run nothing
# and has no log to lose. The pod, the ephemeral claim (owned by the pod)
# and the ConfigMap (owned by the Job) go with it by garbage collection,
# and the read reads all three back gone before it prints its effect.
# The Job's ttlSecondsAfterFinished (a day) is the backstop for a drill
# nobody read.
#
# THE VERDICT (design 3c475a19), the read's: the fetch receipt verified
# the newest dump; the pod exited 0 on the data node; the claim's every
# Longhorn replica sits on that node at /var/mnt/postgres; the dump
# restored at least one audit row; dump <= live on audit_log, jobs and
# steps, and on the newest audit id; the live log holds exactly one row
# at the dump's newest audit id; and the scratch objects are read back
# gone. The live counts run ahead of the dump by the hours since 09:10Z,
# which is why the comparison is <= and the dump's stamp is recorded
# beside them. The record is one `receipt: {json}` line — with the
# restore's measured seconds and bytes, the next deadline's sizing —
# then the effect line. A read of a drill still running exits 75, the
# not-yet every reader of an ops-request already knows.
set -uo pipefail
umask 077

case "$#:${1:-}" in
    0:) MODE=start; ME=drill-gcs-restore ;;
    1:--read) MODE=read; ME=read-restore-drill ;;
    *)
        echo "drill-gcs-restore: REFUSED — this script takes no argument (drill-gcs-restore) or exactly --read (read-restore-drill), and got $#: nothing was created or removed" >&2
        exit 78
        ;;
esac
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

NS=boss
NAME=boss-restore-drill
LABEL="app=$NAME"
CLASS=longhorn-restore-drill
PURPOSE=database
DISK_PATH=/var/mnt/postgres
MANIFEST="$HERE/restore-drill-job.yaml"
POLL_S="${DRILL_POLL_S:-10}"
# A pod that cannot start (an image it cannot pull, a Secret key it
# cannot mount) waits forever, with no log to lose; a read that finds
# its pod waiting on such a reason this long after it was made ends it
# rather than leave it to the Job's backstop.
STUCK_AFTER_S="${DRILL_STUCK_AFTER_S:-900}"
READ_VERB="boss ops forge read-restore-drill --wait"
START_VERB="boss ops forge drill-gcs-restore --wait"
NOTHING="nothing was created"
[ "$MODE" = read ] && NOTHING="nothing was removed"

fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
words() { tr '\n' ' ' < "$1" | cut -c1-600; }

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — $NOTHING"
[ -r "$MANIFEST" ] || refuse "this checkout carries no infra/forge/restore-drill-job.yaml — $NOTHING"
for f in gcs-readback.py restore-drill-pod.sh; do
    [ -r "$HERE/$f" ] || refuse "this checkout carries no infra/forge/$f — $NOTHING"
done
# The two deadlines, from the manifest (one definition): the Job's
# backstop, and the restore container's own, which must sit below it or
# the controller deletes the pod — and its log — first (24fd49e7).
DEADLINE="$(sed -n 's/^  activeDeadlineSeconds: *//p' "$MANIFEST")"
case "${DEADLINE:-empty}" in
    empty | *[!0-9]*) refuse "the drill manifest does not declare one whole-second activeDeadlineSeconds — $NOTHING" ;;
esac
INNER="$(sed -n 's/^ *- {name: DRILL_RESTORE_DEADLINE_S, value: "\([0-9]*\)"}$/\1/p' "$MANIFEST")"
case "${INNER:-empty}" in
    empty | *[!0-9]*) refuse "the drill manifest does not declare one whole-second DRILL_RESTORE_DEADLINE_S for the restore container — $NOTHING" ;;
esac
[ "$DEADLINE" -gt "$INNER" ] \
    || refuse "the Job's backstop (${DEADLINE}s) is not above the restore's inner deadline (${INNER}s), so the controller would delete the pod and its log before the pod could end itself — $NOTHING"

WORK="$(mktemp -d)" || fail "cannot make a scratch directory — $NOTHING"
JOB_UID=""
CLEANED=""
REMOVE_WHY=""

ops_image_ready > "$WORK/door.log" 2>&1 || fail "the kubectl door image could not be made ready: $(words "$WORK/door.log")— $NOTHING"
K="$(ops_kubectl)"

# kget <out-file> <kubectl get args…> — one bounded read into a file.
kget() {
    local out="$1"
    shift
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K get "$@" --request-timeout=20s > "$out" 2> "$WORK/k.err"
}
# kin <out-file> <kubectl args…> < input — one write with a body on stdin.
kin() {
    local out="$1"
    shift
    local -a KI
    read -r -a KI <<< "$(ops_kubectl --stdin)"
    "${KI[@]}" "$@" --request-timeout=30s > "$out" 2> "$WORK/kin.err"
}

# remove_job <uid> — delete Job $NS/$NAME by that uid (Foreground, so its
# pod goes first), then read back the Job, every claim labelled ours and
# the ConfigMap gone. Returns 1 with REMOVE_WHY set when it could not.
remove_job() {
    local uid="$1" job_gone="" pvc_gone="" cm_gone=""
    jq -n --arg uid "$uid" '{kind: "DeleteOptions", apiVersion: "v1", propagationPolicy: "Foreground", preconditions: {uid: $uid}}' > "$WORK/del.json"
    if ! kin "$WORK/del.out" delete --raw "/apis/batch/v1/namespaces/$NS/jobs/$NAME" -f - < "$WORK/del.json"; then
        # Already gone is gone; anything else is a refusal to report.
        if ! grep -qiE 'not ?found' "$WORK/kin.err"; then
            REMOVE_WHY="the delete of Job $NS/$NAME (uid $uid) was refused or failed: $(words "$WORK/kin.err")"
            return 1
        fi
    fi
    for _ in $(seq 1 30); do
        if [ -z "$job_gone" ] && kget "$WORK/back.json" job "$NAME" -n "$NS" --ignore-not-found -o json; then
            if ! jq_doc_file "$WORK/back.json" || [ "$(jq -r '.metadata.uid // empty' "$WORK/back.json")" != "$uid" ]; then job_gone=1; fi
        fi
        if [ -n "$job_gone" ] && [ -z "$pvc_gone" ] && kget "$WORK/pvcs.json" pvc -n "$NS" -l "$LABEL" -o json; then
            [ "$(jq -r '.items | length' "$WORK/pvcs.json" 2>/dev/null)" = 0 ] && pvc_gone=1
        fi
        if [ -n "$job_gone" ] && [ -z "$cm_gone" ] && kget "$WORK/cm.json" configmap "$NAME" -n "$NS" --ignore-not-found -o json; then
            jq_doc_file "$WORK/cm.json" || cm_gone=1
        fi
        [ -n "$job_gone" ] && [ -n "$pvc_gone" ] && [ -n "$cm_gone" ] && break
        sleep "$POLL_S"
    done
    if [ -z "$job_gone" ] || [ -z "$pvc_gone" ] || [ -z "$cm_gone" ]; then
        REMOVE_WHY="Job $NS/$NAME (uid $uid) was deleted and is not all read back gone (job ${job_gone:+gone}${job_gone:-STANDS}, claim ${pvc_gone:+gone}${pvc_gone:-STANDS}, configmap ${cm_gone:+gone}${cm_gone:-STANDS}) — read it with kubectl -n $NS get job,pvc,configmap -l $LABEL; the Job's ttlSecondsAfterFinished removes it a day after it finished"
        return 1
    fi
    echo "$ME: removed Job $NS/$NAME (uid $uid) — the Job, its scratch claim and its ConfigMap read back gone"
}

# The EXIT trap: once JOB_UID names a Job this run may remove (the start's
# own create, or a finished drill the read has printed), it is removed
# whatever the exit.
cleanup() {
    if [ -n "$JOB_UID" ] && [ -z "$CLEANED" ]; then
        CLEANED=1
        remove_job "$JOB_UID" || echo "$ME: $REMOVE_WHY" >&2
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# job_state <job.json> — `complete`, `failed<TAB><reason>: <message>` or
# `running`, from the Job's own True conditions.
job_state() {
    jq -r '[.status.conditions[]? | select(.status == "True")] as $c
        | if ($c | map(select(.type == "Failed")) | length) > 0 then ($c | map(select(.type == "Failed"))[0] | "failed\t\(.reason // "?"): \(.message // "")")
          elif ($c | map(select(.type == "Complete")) | length) > 0 then "complete\t"
          else "running\t" end' "$1" 2>/dev/null
}

# ============================================================================
# drill-gcs-restore — the bounds, the create, and return.
# ============================================================================
start_drill() {
    local REG
    REG="${BOSS_FORGE_REGISTRY_HOST:-}"
    [[ "$REG" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$ ]] \
        || refuse "BOSS_FORGE_REGISTRY_HOST is '${REG}', not one registry host[:port] (it comes from /etc/boss/sor.env) — nothing was created"

    kget "$WORK/sc.json" storageclass "$CLASS" --ignore-not-found -o json \
        || fail "could not read StorageClass $CLASS: $(words "$WORK/k.err")— nothing was created"
    jq_doc_file "$WORK/sc.json" \
        || refuse "StorageClass $CLASS does not exist — infra/cluster/manifests/longhorn-restore-drill.yaml declares it, and the cluster converge applies it; nothing was created"
    jq -e '.provisioner == "driver.longhorn.io" and .parameters.diskSelector == "postgres" and .parameters.numberOfReplicas == "1"' "$WORK/sc.json" > /dev/null \
        || refuse "StorageClass $CLASS is not the tree's (driver.longhorn.io, diskSelector postgres, one replica): $(jq -c '{provisioner, parameters}' "$WORK/sc.json" 2>/dev/null) — nothing was created"

    kget "$WORK/nodes.json" nodes -l "boss.dev/purpose=$PURPOSE" -o json \
        || fail "could not read the Nodes labelled boss.dev/purpose=$PURPOSE: $(words "$WORK/k.err")— nothing was created"
    local NODES NODE TAINTS DISK
    NODES="$(jq -r '[.items[]?.metadata.name] | join(" ")' "$WORK/nodes.json" 2>/dev/null)"
    [ "$(jq -r '.items | length' "$WORK/nodes.json" 2>/dev/null)" = 1 ] \
        || refuse "exactly one Node must carry boss.dev/purpose=$PURPOSE, and the cluster reads '${NODES}' — the drill would not know which disk it proved; nothing was created"
    NODE="$NODES"
    [ "$(jq -r '.items[0].spec.unschedulable // false' "$WORK/nodes.json")" = false ] \
        || refuse "$NODE is cordoned — the drill's pod could not be scheduled there; nothing was created"
    # The Job tolerates nothing: a taint that repels pods would leave it
    # Pending for its whole deadline, so it is a refusal here instead.
    TAINTS="$(jq -r '[.items[0].spec.taints[]? | select(.effect == "NoSchedule" or .effect == "NoExecute") | "\(.key)=\(.value // ""):\(.effect)"] | join(" ")' "$WORK/nodes.json")"
    [ -z "$TAINTS" ] \
        || refuse "$NODE carries the taint(s) $TAINTS, which the drill's Job does not tolerate — nothing was created"

    # The disk the class selects must exist on THAT node, tagged and
    # schedulable — otherwise the claim cannot place and the pod would
    # wait out its whole deadline attaching nothing.
    kget "$WORK/lhnode.json" nodes.longhorn.io "$NODE" -n longhorn-system -o json \
        || refuse "could not read Longhorn node $NODE: $(words "$WORK/k.err")— nothing was created"
    DISK="$(jq -r --arg p "$DISK_PATH" '[.spec.disks // {} | to_entries[] | select((.value.path | sub("/+$"; "")) == $p
        and ((.value.tags // []) | index("postgres")) and .value.allowScheduling == true) | .key] | first // empty' "$WORK/lhnode.json" 2>/dev/null)"
    [ -n "$DISK" ] \
        || refuse "Longhorn node $NODE has no schedulable disk at $DISK_PATH tagged postgres — add it (add-longhorn-disk $NODE postgres) and tag it (set-longhorn-disk-tags $NODE u-postgres durable,postgres) first; nothing was created"

    local secret
    for secret in boss-gcs-offsite boss-secrets; do
        # By NAME only: jsonpath prints the name and no data reaches a file.
        kget "$WORK/secret.name" secret "$secret" -n "$NS" -o 'jsonpath={.metadata.name}' \
            || refuse "Secret $NS/$secret could not be read by name: $(words "$WORK/k.err")— nothing was created"
        [ "$(cat "$WORK/secret.name")" = "$secret" ] || refuse "Secret $NS/$secret does not exist — nothing was created"
    done

    # --- no drill standing, finished or not ----------------------------------
    kget "$WORK/old.json" job "$NAME" -n "$NS" --ignore-not-found -o json \
        || fail "could not read Job $NS/$NAME: $(words "$WORK/k.err")— nothing was created"
    if jq_doc_file "$WORK/old.json"; then
        [ "$(jq -r '.metadata.labels.app // empty' "$WORK/old.json")" = "$NAME" ] \
            || refuse "a Job $NS/$NAME stands WITHOUT the label $LABEL — it is not this verb's, and this verb never touches it; nothing was created"
        local OLD_UID OLD_STATE
        OLD_UID="$(jq -r '.metadata.uid // empty' "$WORK/old.json")"
        OLD_STATE="$(job_state "$WORK/old.json" | cut -f1)"
        [ "$OLD_STATE" != running ] \
            || refuse "Job $NS/$NAME (uid ${OLD_UID:-unread}) is a drill still running — it is never touched; read its progress with $READ_VERB, and it ends itself by its inner deadline (${INNER}s). Nothing was created"
        refuse "a drill stands FINISHED (${OLD_STATE:-unread}, uid ${OLD_UID:-unread}) and its verdict has not been read — $READ_VERB reads it and removes it; nothing was created"
    fi
    kget "$WORK/left.json" pvc -n "$NS" -l "$LABEL" -o json \
        || fail "could not read the claims labelled $LABEL: $(words "$WORK/k.err")— nothing was created"
    [ "$(jq -r '.items | length' "$WORK/left.json" 2>/dev/null)" = 0 ] \
        || refuse "a claim labelled $LABEL still stands with no drill Job ($(jq -r '[.items[].metadata.name] | join(" ")' "$WORK/left.json")) — read it with kubectl -n $NS get pvc -l $LABEL; nothing was created"
    kget "$WORK/leftcm.json" configmap "$NAME" -n "$NS" --ignore-not-found -o json \
        || fail "could not read ConfigMap $NS/$NAME: $(words "$WORK/k.err")— nothing was created"
    jq_doc_file "$WORK/leftcm.json" \
        && refuse "ConfigMap $NS/$NAME still stands with no drill Job — read it with kubectl -n $NS get configmap $NAME; nothing was created"

    # --- the create ----------------------------------------------------------
    # The registry host is bounded above (host[:port]), so safe in sed.
    sed "s/[\$]BOSS_FORGE_REGISTRY_HOST/$REG/g" "$MANIFEST" > "$WORK/job.yaml"
    echo "$ME: every bound holds — StorageClass $CLASS (diskSelector postgres, one replica), data node $NODE schedulable, both Secrets present by name, no drill standing. Creating Job $NS/$NAME pinned by boss.dev/purpose=$PURPOSE."
    kin "$WORK/created.json" create -f - -n "$NS" -o json < "$WORK/job.yaml" \
        || fail "the API server did not create Job $NS/$NAME: $(words "$WORK/kin.err")"
    JOB_UID="$(jq -r '.metadata.uid // empty' "$WORK/created.json" 2>/dev/null)"
    [ -n "$JOB_UID" ] || fail "the create of Job $NS/$NAME answered success with no uid — its backstop deadline (${DEADLINE}s) ends it, and the next start refuses until it is read"
    jq -n --arg name "$NAME" --arg ns "$NS" --arg uid "$JOB_UID" \
        --rawfile readback "$HERE/gcs-readback.py" --rawfile pod "$HERE/restore-drill-pod.sh" \
        '{apiVersion: "v1", kind: "ConfigMap",
          metadata: {name: $name, namespace: $ns, labels: {app: $name},
            ownerReferences: [{apiVersion: "batch/v1", kind: "Job", name: $name, uid: $uid, blockOwnerDeletion: true}]},
          data: {"gcs-readback.py": $readback, "restore-drill.sh": $pod}}' > "$WORK/cm.json"
    kin "$WORK/cm.out" create -f - -n "$NS" -o json < "$WORK/cm.json" \
        || fail "the API server did not create ConfigMap $NS/$NAME: $(words "$WORK/kin.err")"

    # --- the read-back, and return --------------------------------------------
    kget "$WORK/job.json" job "$NAME" -n "$NS" --ignore-not-found -o json \
        || fail "Job $NS/$NAME (uid $JOB_UID) and its ConfigMap were created, and the Job could not be read back: $(words "$WORK/k.err")"
    [ "$(jq -r '.metadata.uid // empty' "$WORK/job.json" 2>/dev/null)" = "$JOB_UID" ] \
        || fail "Job $NS/$NAME was created with uid $JOB_UID and reads back as '$(jq -r '.metadata.uid // "absent"' "$WORK/job.json" 2>/dev/null)'"
    local CREATED ENDS_BY STARTED_UID
    CREATED="$(jq -r '.metadata.creationTimestamp // empty' "$WORK/job.json")"
    [ -n "$CREATED" ] || CREATED="$(jq -r '.metadata.creationTimestamp // empty' "$WORK/created.json")"
    ENDS_BY="$(jq -rn --arg c "$CREATED" --argjson d "$DEADLINE" '$c | fromdate + $d | todate' 2>/dev/null)" \
        || fail "Job $NS/$NAME (uid $JOB_UID) reads back with no creation time ('$CREATED')"
    # Left standing on purpose: the read removes it once it has finished.
    STARTED_UID="$JOB_UID"
    JOB_UID=""
    echo "$ME: STARTED Job $NS/$NAME (uid $STARTED_UID) at $CREATED — the restore ends itself at its inner deadline of ${INNER}s and the Job's backstop deadline of ${DEADLINE}s ends it by $ENDS_BY at the latest; nothing waits on it here. Read its progress or its verdict with: boss ops forge read-restore-drill --wait (it removes nothing while the drill runs)"
}

# ============================================================================
# read-restore-drill — progress while it runs; the verdict once finished.
# ============================================================================
read_drill() {
    kget "$WORK/job.json" job "$NAME" -n "$NS" --ignore-not-found -o json \
        || fail "could not read Job $NS/$NAME: $(words "$WORK/k.err")— whether a drill stands is UNKNOWN; nothing was removed"
    if ! jq_doc_file "$WORK/job.json"; then
        local left=""
        if kget "$WORK/left.json" pvc -n "$NS" -l "$LABEL" -o json \
            && [ "$(jq -r '.items | length' "$WORK/left.json" 2>/dev/null)" != 0 ]; then
            left=" A claim labelled $LABEL still stands ($(jq -r '[.items[].metadata.name] | join(" ")' "$WORK/left.json")); read it with kubectl -n $NS get pvc -l $LABEL."
        fi
        refuse "no drill stands (no Job $NS/$NAME) — there is nothing to read, and nothing was removed. A drill read once is gone; start one with $START_VERB.$left"
    fi
    [ "$(jq -r '.metadata.labels.app // empty' "$WORK/job.json")" = "$NAME" ] \
        || refuse "a Job $NS/$NAME stands WITHOUT the label $LABEL — it is not this verb's, and this verb never touches it; nothing was removed"
    local UID_READ STATE OUTCOME WHY START_AT
    UID_READ="$(jq -r '.metadata.uid // empty' "$WORK/job.json")"
    [ -n "$UID_READ" ] || fail "Job $NS/$NAME stands and its uid could not be read — nothing was removed"
    START_AT="$(jq -r '.status.startTime // .metadata.creationTimestamp // empty' "$WORK/job.json")"
    STATE="$(job_state "$WORK/job.json")"
    case "$STATE" in
        complete*) OUTCOME=complete; WHY="" ;;
        failed*) OUTCOME=failed; WHY="the Job's Failed condition says $(printf '%s' "$STATE" | cut -f2)" ;;
        *) OUTCOME=running; WHY="" ;;
    esac

    kget "$WORK/pods.json" pods -n "$NS" -l "batch.kubernetes.io/controller-uid=$UID_READ" -o json \
        || fail "could not read the pods of Job $NS/$NAME (uid $UID_READ): $(words "$WORK/k.err")— nothing was removed"
    jq_doc_file "$WORK/pods.json" || echo '{"items":[]}' > "$WORK/pods.json"
    local POD RAN_ON
    POD="$(jq -r '.items | sort_by(.metadata.creationTimestamp) | last | .metadata.name // empty' "$WORK/pods.json")"
    RAN_ON="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | .spec.nodeName // empty' "$WORK/pods.json")"

    if [ "$OUTCOME" = running ]; then
        # A pod that cannot start has run nothing and has no log to lose:
        # past STUCK_AFTER_S it is ended here rather than at the backstop.
        local reason age
        reason="$(jq -r --arg p "$POD" '[.items[] | select(.metadata.name == $p) | .status | (.initContainerStatuses[]?, .containerStatuses[]?) | .state.waiting.reason // empty
            | select(test("^(ErrImagePull|ImagePullBackOff|InvalidImageName|CreateContainerConfigError|CreateContainerError)$"))] | first // empty' "$WORK/pods.json" 2>/dev/null)"
        age="$(jq -r --arg p "$POD" '[.items[] | select(.metadata.name == $p) | .metadata.creationTimestamp | fromdate] | first // empty | now - . | floor' "$WORK/pods.json" 2>/dev/null)"
        case "${age:-empty}" in empty | *[!0-9]*) age=0 ;; esac
        if [ -n "$reason" ] && [ "$age" -ge "$STUCK_AFTER_S" ]; then
            OUTCOME=stuck
            WHY="its pod has been waiting on $reason for ${age}s (past ${STUCK_AFTER_S}s) — it cannot start, so it ran nothing"
        else
            local elapsed c
            elapsed="$(jq -rn --arg s "$START_AT" '$s | fromdate | now - . | floor' 2>/dev/null)"
            echo "$ME: STILL RUNNING — Job $NS/$NAME (uid $UID_READ) started $START_AT, ${elapsed:-an unread number of }s ago; pod ${POD:-not yet made} on ${RAN_ON:-no node yet}${reason:+, waiting on $reason}; the restore ends itself at ${INNER}s and the Job's backstop is ${DEADLINE}s. Nothing was removed; read it again with $READ_VERB."
            for c in fetch restore; do
                [ -n "$POD" ] || break
                # shellcheck disable=SC2086 # K is one line the door prints to be word-split
                if $K logs "$POD" -n "$NS" -c "$c" --tail=40 --request-timeout=20s > "$WORK/$c.tail" 2> "$WORK/logs.err"; then
                    echo "$ME: the $c container's last lines so far:"
                    sed 's/^/    /' "$WORK/$c.tail"
                else
                    echo "$ME: the $c container's log could not be read yet: $(words "$WORK/logs.err")"
                fi
            done
            exit 75
        fi
    fi

    # --- finished: the pod's own record, BEFORE anything is removed ----------
    local FETCH_EXIT RESTORE_EXIT c
    FETCH_EXIT="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | [.status.initContainerStatuses[]? | select(.name == "fetch") | .state.terminated.exitCode | select(. != null)] | last // empty' "$WORK/pods.json")"
    RESTORE_EXIT="$(jq -r --arg p "$POD" '.items[] | select(.metadata.name == $p) | [.status.containerStatuses[]? | select(.name == "restore") | .state.terminated.exitCode | select(. != null)] | last // empty' "$WORK/pods.json")"
    : > "$WORK/fetch.log"
    : > "$WORK/restore.log"
    if [ -n "$POD" ]; then
        # A container that RAN has a log, and that log is the drill's only
        # record: if it cannot be read whole, nothing is removed and the
        # read is retryable (Codex review 32f56fc7). A container that never
        # started (a stuck pull, an init that failed first) has none to lose.
        local unread="" ran
        for c in fetch restore; do
            # shellcheck disable=SC2086 # K is one line the door prints to be word-split
            if $K logs "$POD" -n "$NS" -c "$c" --request-timeout=20s > "$WORK/$c.log" 2> "$WORK/logs.err"; then
                echo "$ME: the $c container's whole log (pod $POD, on ${RAN_ON:-an unread node}):"
                sed 's/^/    /' "$WORK/$c.log"
            else
                : > "$WORK/$c.log"
                echo "$ME: the $c container's log (pod $POD) could not be read: $(words "$WORK/logs.err")"
                ran="$(jq -r --arg p "$POD" --arg c "$c" '[.items[] | select(.metadata.name == $p) | .status | (.initContainerStatuses[]?, .containerStatuses[]?)
                    | select(.name == $c) | select(.state.terminated != null or .state.running != null or .lastState.terminated != null)] | length' "$WORK/pods.json" 2>/dev/null)"
                case "${ran:-unread}" in
                    0) ;;
                    *) unread="${unread:+$unread and }$c" ;;
                esac
            fi
        done
        if [ -n "$unread" ] && [ "$OUTCOME" != stuck ]; then
            echo "$ME: NOT REMOVED — the whole log of the $unread container of pod $POD could not be read, and a log is the drill's only record; Job $NS/$NAME (uid $UID_READ), its pod, scratch claim and ConfigMap are left standing (the Job is kept a day after it finishes). Read it again with $READ_VERB."
            exit 75
        fi
    else
        echo "$ME: no pod of Job $NS/$NAME stands — on DeadlineExceeded the Job controller deletes the pod it was running, and its log with it, so how far this drill got cannot be read from it"
    fi

    # --- where the scratch claim's bytes lived --------------------------------
    local PV=""
    : > "$WORK/replicas.txt"
    if [ -n "$POD" ] && kget "$WORK/claim.json" pvc "$POD-scratch" -n "$NS" --ignore-not-found -o json && jq_doc_file "$WORK/claim.json"; then
        PV="$(jq -r '.spec.volumeName // empty' "$WORK/claim.json")"
    fi
    if [ -n "$PV" ] && kget "$WORK/replicas.json" replicas.longhorn.io -n longhorn-system -l "longhornvolume=$PV" -o json && jq_doc_file "$WORK/replicas.json"; then
        jq -r --arg v "$PV" '.items[]? | select(.spec.volumeName == $v) | "\(.spec.nodeID // "")\t\((.spec.diskPath // "") | sub("/+$"; ""))"' "$WORK/replicas.json" > "$WORK/replicas.txt"
    fi
    # The data node and its postgres disk, read again: the start's bounds
    # named them, and this is a later process.
    local NODE="" DISK=""
    if kget "$WORK/nodes.json" nodes -l "boss.dev/purpose=$PURPOSE" -o json; then
        NODE="$(jq -r '[.items[]?.metadata.name] | join(" ")' "$WORK/nodes.json" 2>/dev/null)"
    fi
    if [ -n "$NODE" ] && kget "$WORK/lhnode.json" nodes.longhorn.io "$NODE" -n longhorn-system -o json; then
        DISK="$(jq -r --arg p "$DISK_PATH" '[.spec.disks // {} | to_entries[] | select((.value.path | sub("/+$"; "")) == $p) | .key] | first // empty' "$WORK/lhnode.json" 2>/dev/null)"
    fi
    # How long the Job ran, from its own times: the number the next sizing
    # of the deadlines reads, whatever the outcome.
    local JOB_S
    JOB_S="$(jq -r '(.status.completionTime // ([.status.conditions[]? | select(.type == "Failed" and .status == "True") | .lastTransitionTime] | first)) as $done
        | if ($done != null and .status.startTime != null) then (($done | fromdate) - (.status.startTime | fromdate)) else empty end' "$WORK/job.json" 2>/dev/null)"

    # --- the removal: the logs are on the record now --------------------------
    local REMOVED=""
    JOB_UID="$UID_READ"
    CLEANED=1
    if remove_job "$JOB_UID"; then REMOVED=1; fi

    # --- the verdict ---------------------------------------------------------
    local LAST reason=""
    LAST="$(grep -E '^restore-drill: (progress|FAILED)' "$WORK/restore.log" | tail -n 2 | tr '\n' ' ' | cut -c1-600)"
    [ "$OUTCOME" = complete ] \
        || fail "the drill did not complete: $WHY${JOB_S:+ (the Job ran ${JOB_S}s)}${LAST:+; the last words of the restore container: $LAST}${REMOVE_WHY:+; and $REMOVE_WHY}"
    sed -n 's/^receipt: //p' "$WORK/fetch.log" > "$WORK/fetch.json"
    sed -n 's/^restore-drill: counts //p' "$WORK/restore.log" > "$WORK/counts.json"
    if [ "${FETCH_EXIT:-unread}" != 0 ] || [ "${RESTORE_EXIT:-unread}" != 0 ]; then
        reason="the Job says Complete but its containers exited fetch '${FETCH_EXIT:-unread}', restore '${RESTORE_EXIT:-unread}'"
    elif [ -z "$NODE" ] || [ "$RAN_ON" != "$NODE" ]; then
        reason="its pod ran on '${RAN_ON:-an unread node}', not the data node ${NODE:-(unread)}"
    elif ! jq_doc_file "$WORK/fetch.json" || ! jq -e -s 'length == 1 and (.[0] | .result == "ok" and .stage == "verified"
            and (.object | test("^boss-[0-9]{8}-[0-9]{6}[.]sql[.]gz$")) and (.sha256 | test("^[a-f0-9]{64}$"))
            and .saved == true)' "$WORK/fetch.json" > /dev/null; then
        reason="the fetch container's log carries no single verified receipt of the dump it kept"
    elif ! jq_doc_file "$WORK/counts.json" || ! jq -e -s 'length == 1 and (.[0] | ([.dump, .live] | all(
            [.audit_log, .jobs, .steps, .max_audit_id] | all(type == "number")))
            and (.restore_seconds | type == "number") and (.restored_bytes | type == "number"))' "$WORK/counts.json" > /dev/null; then
        reason="the restore container's log carries no single line of the four counts on both sides, the restore's seconds and its bytes"
    elif [ ! -s "$WORK/replicas.txt" ] || [ "$(awk -F'\t' -v n="$NODE" -v d="$DISK_PATH" '$1 != n || $2 != d { b++ } END { print b + 0 }' "$WORK/replicas.txt")" != 0 ]; then
        reason="the scratch claim's Longhorn replicas are not all on $NODE at $DISK_PATH (read: $(tr '\t\n' ': ' < "$WORK/replicas.txt" | sed 's/ $//')) — the restore did not exercise the postgres disk"
    fi
    [ -z "$reason" ] || fail "$reason. The logs are above${REMOVE_WHY:+; and $REMOVE_WHY}"

    jq -n --slurpfile fetch "$WORK/fetch.json" --slurpfile counts "$WORK/counts.json" \
        --arg node "$NODE" --arg pod "$POD" --arg pv "$PV" --arg disk "$DISK_PATH" --arg diskname "$DISK" --arg job "$JOB_UID" \
        --arg job_s "${JOB_S:-}" \
        --argjson removed "$([ -n "$REMOVED" ] && echo true || echo false)" \
        '($fetch[0]) as $f | ($counts[0]) as $c
         | ([ "audit_log", "jobs", "steps", "max_audit_id" ] | map({key: ., value: ($c.dump[.] <= $c.live[.])}) | from_entries) as $le
         | {claim: "restore-drill", dump: {object: $f.object, stamp: ($f.object | capture("^boss-(?<s>[0-9]{8}-[0-9]{6})").s),
              generation: $f.generation, sha256: $f.sha256,
              bytes: $f.object_bytes, created_at: $f.object_created_at},
            scratch: {node: $node, pod: $pod, volume: $pv, disk: $diskname, disk_path: $disk, server_version: $c.scratch_server_version,
              restore_seconds: $c.restore_seconds, restored_bytes: $c.restored_bytes, live_roles_created: $c.live_roles_created},
            job_seconds: (if $job_s == "" then null else ($job_s | tonumber) end),
            live: {server_version: $c.live_server_version, read_only: true},
            counts: {dump: $c.dump, live: $c.live}, dump_le_live: $le,
            live_rows_at_dump_max_audit_id: $c.live_rows_at_dump_max_audit_id,
            dump_restored_rows: ($c.dump.audit_log > 0),
            removed: $removed, job_uid: $job}
         | .result = (if (.dump_le_live | all) and .live_rows_at_dump_max_audit_id == 1 and .dump_restored_rows and .removed
                      then "ok" else "failed" end)' > "$WORK/receipt.json" \
        || fail 'the receipt could not be assembled from the two logs'
    jq_doc_file "$WORK/receipt.json" || fail 'the receipt assembled from the two logs is empty'
    echo "receipt: $(jq -c . "$WORK/receipt.json")"

    jq -e '.dump_restored_rows' "$WORK/receipt.json" > /dev/null \
        || fail 'the dump restored no audit_log row — a restore of nothing proves nothing'
    jq -e '.dump_le_live | all' "$WORK/receipt.json" > /dev/null \
        || fail "the dump is AHEAD of the live system of record on $(jq -r '.dump_le_live | to_entries | map(select(.value | not) | .key) | join(", ")' "$WORK/receipt.json") — the live database has lost rows the dump holds"
    jq -e '.live_rows_at_dump_max_audit_id == 1' "$WORK/receipt.json" > /dev/null \
        || fail "the live audit_log holds $(jq -r '.live_rows_at_dump_max_audit_id' "$WORK/receipt.json") row(s) at the dump's newest audit id $(jq -r '.counts.dump.max_audit_id' "$WORK/receipt.json"), not one"
    [ -n "$REMOVED" ] || fail "the restore holds, and the run did not clean up after itself: $REMOVE_WHY"

    # The restore's own seconds and bytes ride the effect line: the
    # number the next sizing of the deadlines reads (24fd49e7 left none).
    printf '%s: RESTORED dump %s (sha256 %s) into a scratch database on the postgres disk of %s in %ss (%s bytes restored) — dump <= live on audit_log, jobs and steps (%s); newest audit id %s present in the live system of record; scratch Job and claim removed and read back gone\n' \
        "$ME" "$(jq -r '.dump.object' "$WORK/receipt.json")" "$(jq -r '.dump.sha256' "$WORK/receipt.json")" "$NODE" \
        "$(jq -r '.scratch.restore_seconds' "$WORK/receipt.json")" "$(jq -r '.scratch.restored_bytes' "$WORK/receipt.json")" \
        "$(jq -r '.counts as $c | "audit_log \($c.dump.audit_log)/\($c.live.audit_log), jobs \($c.dump.jobs)/\($c.live.jobs), steps \($c.dump.steps)/\($c.live.steps)"' "$WORK/receipt.json")" \
        "$(jq -r '.counts.dump.max_audit_id' "$WORK/receipt.json")"
}

case "$MODE" in
    start) start_drill ;;
    read) read_drill ;;
esac
