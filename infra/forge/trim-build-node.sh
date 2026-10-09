#!/usr/bin/env bash
# trim-build-node.sh — run the DECLARED trim of the build node's two
# filesystems once, now, and put its own record on the packet. The ops
# verb `trim-build-node`.
#
#   trim-build-node.sh            (no argument, ever)
#
# WHY IT EXISTS (backlog 30a6bf39; asked for by David, 2026-10-07). Twice
# in two days the gates slowed and threw false reds on loopback timeouts
# until w-1's two filesystems were trimmed — each time by a pod David
# made by hand in kube-system, `privileged: true` with the whole of /var
# mounted (items 17f6170c, ec131700). The scheduled trim landed on train
# #989 as CronJob boss-node-trim, daily at 12:40 UTC. This is the same
# trim at any other hour, with no hand on it: `kubectl create job
# --from=cronjob/boss-node-trim`, so the pod is the CronJob's own
# template and nothing this file wrote.
#
# THE BOUND IS NOT THIS SCRIPT. It is the ValidatingAdmissionPolicy
# node-maintenance-holds-one-workload
# (infra/cluster/manifests/boss-node-maintenance-admission.yaml): the API
# server admits into that namespace a Job only when its pod spec IS the
# trim pod — the image digest, the script byte for byte, the two
# read-only directories, one capability — whoever sends it. So the most
# this door can do, even edited, is start one more of that pod; and that
# pod discards blocks the filesystem already holds free. What this
# script adds is WHEN it may, and an honest report:
#
#   - NO ARGUMENT. No node, path, image, namespace or name comes from
#     the caller: any argument is a refusal. The namespace, the CronJob
#     and the Job's prefix are literals below; the Job's name is that
#     prefix and the UTC second.
#   - NO POD SPEC. This file carries none, and neither does anything it
#     calls. `--from=cronjob/…` copies the server's.
#   - IT REFUSES, CREATING NOTHING (exit 78):
#       when the policy's binding is absent, does not list Deny, names
#       another policy, or is narrowed (matchResources) or parameterised
#       (paramRef); when the policy it names is absent or does not fail
#       closed (failurePolicy Fail);
#       when the CronJob is absent (the namespace has not converged);
#       when the CronJob is SUSPENDED — `.spec.suspend: true` is the one
#       switch a human has in the cluster to stop the trim, and `create
#       job --from` a suspended CronJob runs all the same, so this verb
#       reads the switch and stops at it (review 3e750cce, finding 2);
#       when the CronJob the server holds differs from this checkout's
#       on what is compared (below) — a tree that moved ahead of the
#       converge, or the reverse, is not trimmed with;
#       or when a Job there is unfinished or a pod there is not terminal
#       (the CronJob's own run, or another of these).
#   - IT DELETES ONLY THE JOB IT MADE: by that Job's name, and only
#     while the name still carries the uid the create answered. Never by
#     a label, never a list. A create that did not answer success gave
#     no uid, so nothing is deleted after one — and the name is READ
#     BACK before anything is said about whether a Job was made.
#
# WHAT IS COMPARED, AND WHAT IS NOT (review 3e750cce, finding 3). Of
# the server's CronJob against this checkout's manifest: that it has
# one container, that container's image, its script byte for byte, and
# the Job template's activeDeadlineSeconds, backoffLimit, parallelism
# and completions — the last three because they are how many of the
# admitted pod a Job asks for, which the admission policy does not
# bound. THE REST OF THE POD TEMPLATE IS NOT COMPARED HERE: its
# command, its security settings, its volumes, its node selection. The
# policy judges every one of those at the create, and a Job it refuses
# ends this run FAILED quoting it. A whole-template compare needs the
# manifest read as YAML, which nothing on the forge host does, and the
# list of fields the API server fills in by default has never been
# read off this server: guessed wrong, it would refuse every run. So
# the line printed before the create names what was compared and says
# the rest was not — it does not say the CronJob "is the one this
# checkout declares". Of the policy, only that it exists and fails
# closed is read; its rules are not.
#
# WHAT IS NOT CLOSED: the check and the create are two calls. A
# scheduled run that starts between them gives two trims at once — two
# of the admitted pod on the same filesystems. Both are xfs, and what a
# second trim costs there is NOT MEASURED: XFS is understood to keep no
# record of what it has already discarded, so a second pass would walk
# all free space and discard it again rather than find little left
# (review 3e750cce, finding 5 — stated from kernel behaviour, not
# measured; two live runs a few minutes apart reporting equal bytes
# would show it). No data is at risk either way: fstrim discards only
# free blocks. The CronJob's `concurrencyPolicy: Forbid` governs only
# its own schedule.
#
# WHAT IT REPORTS. The pod's log — `fstrim -v`'s bytes per mount, each
# NVMe's discard counters before and after, its closing OK or RED — or,
# when the log cannot be read, the same lines from the pod's termination
# message, saying which. Then ONE verdict:
#
#   exit 0   the Job completed, its pod exited 0, the record names both
#            mounts trimmed and closes OK, and the Job read back gone.
#            Only then is the line the verb file declares as `effect`
#            printed.
#   exit 1   FAILED — the trim did not run or did not succeed, with the
#            reason the cluster gives: the API server refused the Job or
#            its pod (admission), the pod never started (image pull,
#            unschedulable), the Job's deadline, the pod's exit code, or
#            this script's own wait running out. Also: a trim that
#            succeeded whose Job could not be removed; and a create that
#            did not answer success while a Job of its name is read
#            standing — named, with its uid, and not removed.
#   exit 4   CANNOT ANSWER — the cluster could not be read. Before the
#            create that means nothing was made; after it, the Job's
#            outcome is unknown and is said to be unknown, never failed
#            and never ok. A create that did not answer success and
#            could not be read back is this too: whether a Job stands
#            is unknown, and the name to look for is printed.
#   exit 78  REFUSED — nothing was created.
#
# THE WAIT is the Job's own `activeDeadlineSeconds`, read from the
# manifest beside this file (one number, not two), plus a margin for the
# controller to say so. A pod that has not started within START_GRACE_S
# is failed then, by its waiting reason, rather than at the deadline: an
# image that cannot be pulled never fails a Job by itself. The runner is
# one pass at a time, so a long trim holds every other request for this
# host until it ends; the verb file's `timeout` is above this wait, and a
# test holds the two apart.
#
# A KILL (the runner's timeout, a stop of its unit) still removes the
# Job: the EXIT trap does, by the same uid-held delete.
set -uo pipefail

ME=trim-build-node
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

NS=boss-node-maintenance
CRONJOB=boss-node-trim
BINDING=node-maintenance-holds-one-workload
POLICY=node-maintenance-holds-one-workload
PREFIX=boss-node-trim-manual-
MANIFEST="$HERE/../cluster/manifests/boss-node-maintenance.yaml"
# The two mounts the declared script trims, as its record names them.
MOUNTS="/trim/gate /trim/ephemeral"
START_GRACE_S=180
MARGIN_S=120
POLL_S=10
# Consecutive polls that could not read the cluster before the wait
# gives up: one minute of a dark API server is a blip, not an answer.
UNREAD_MAX=6

refuse() { echo "$ME: REFUSED — $* Nothing was created." >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { echo "$ME: CANNOT ANSWER — $*" >&2; exit 4; }
words() { tr '\n' ' ' < "$1" | cut -c1-600; }

[ $# -eq 0 ] || refuse "this verb takes no argument (got $#): the node, the paths, the image, the namespace and the Job's name are fixed by the tree, never by a caller."
command -v jq >/dev/null 2>&1 || unread "jq is not on PATH — the cluster's answers cannot be read, and nothing was created."

WORK="$(mktemp -d)" || unread "cannot make a scratch directory, and nothing was created."
NAME=""
UID_MADE=""
REMOVED=""
TRIED=""
# shellcheck disable=SC2317 # reached through the trap
on_exit() {
    rc=$?
    if [ -n "$UID_MADE" ] && [ -z "$REMOVED" ] && [ -z "$TRIED" ]; then
        echo "$ME: ending before Job $NAME was removed (exit $rc) — removing it now" >&2
        remove_job || true
    fi
    rm -rf "$WORK"
    exit "$rc"
}
trap on_exit EXIT
trap 'exit 143' TERM
trap 'exit 130' INT

# --- what this checkout declares ---------------------------------------
# Read as LINES, the way the manifest's own pin reads it
# (the_privileged_namespace_holds_one_workload.rs holds every line of
# that file): one `image:`, one `activeDeadlineSeconds:`, and the block
# scalar under `args:`. Anything else found there is a refusal, never a
# guess.
[ -r "$MANIFEST" ] || refuse "this checkout carries no $(basename "$MANIFEST") under infra/cluster/manifests — there is no declared trim to run."
WANT_IMAGE="$(sed -n 's/^ *image: *//p' "$MANIFEST")"
WANT_DEADLINE="$(sed -n 's/^ *activeDeadlineSeconds: *//p' "$MANIFEST")"
case "$WANT_IMAGE" in
    *$'\n'* | '' | *[!A-Za-z0-9.:/@_-]*) refuse "the manifest does not declare exactly one image line (read: '$(printf '%s' "$WANT_IMAGE" | tr '\n' ' ')')." ;;
    *@sha256:*) ;;
    *) refuse "the manifest's image '$WANT_IMAGE' is not pinned by digest." ;;
esac
case "${WANT_DEADLINE:-empty}" in
    empty | *[!0-9]*) refuse "the manifest does not declare exactly one whole-second activeDeadlineSeconds (read: '$(printf '%s' "$WANT_DEADLINE" | tr '\n' ' ')')." ;;
esac
# How many of the pod a Job asks for. backoffLimit is declared (its
# server default is six retries); parallelism and completions are not
# written in the manifest today, and unset means one — on both sides.
WANT_BACKOFF="$(sed -n 's/^ *backoffLimit: *//p' "$MANIFEST")"
WANT_PARALLELISM="$(sed -n 's/^ *parallelism: *//p' "$MANIFEST")"
WANT_COMPLETIONS="$(sed -n 's/^ *completions: *//p' "$MANIFEST")"
WANT_PARALLELISM="${WANT_PARALLELISM:-1}"
WANT_COMPLETIONS="${WANT_COMPLETIONS:-1}"
for pair in "backoffLimit=$WANT_BACKOFF" "parallelism=$WANT_PARALLELISM" "completions=$WANT_COMPLETIONS"; do
    case "${pair#*=}" in
        '' | *[!0-9]*) refuse "the manifest does not declare ${pair%%=*} as one whole number (read: '$(printf '%s' "${pair#*=}" | tr '\n' ' ')')." ;;
    esac
done
# The block: the lines after `args:` / `- |`, down to the first line
# indented less than the block's own first line, that indent removed.
awk '
    blk == 0 && /^ *args: *$/ { blk = 1; next }
    blk == 1 { if ($0 ~ /^ *- \| *$/) { blk = 2; next } else { blk = 0 } }
    blk == 2 {
        if (ind == 0) { match($0, /^ */); ind = RLENGTH }
        if ($0 ~ /^ *$/) { print ""; next }
        match($0, /^ */)
        if (RLENGTH < ind) { blk = 3; next }
        print substr($0, ind + 1)
    }
' "$MANIFEST" > "$WORK/want.script"
[ -s "$WORK/want.script" ] || refuse "the manifest's trim script could not be read out of its args block."
WAIT_S=$((WANT_DEADLINE + MARGIN_S))

# --- the door -----------------------------------------------------------
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    unread "the kubectl door cannot run: $(words "$WORK/door.err")— nothing was created."
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# kget <out-file> <kubectl get args…> — one bounded read into a file.
# Status is kubectl's; its words are in $WORK/k.err.
kget() {
    local out="$1"
    shift
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K get "$@" --request-timeout=20s > "$out" 2> "$WORK/k.err"
}

# --- the bounds, each a read; nothing is written until all hold --------
kget "$WORK/binding.json" validatingadmissionpolicybinding "$BINDING" --ignore-not-found -o json \
    || unread "could not read ValidatingAdmissionPolicyBinding $BINDING: $(words "$WORK/k.err")— nothing was created."
jq_doc_file "$WORK/binding.json" \
    || refuse "ValidatingAdmissionPolicyBinding $BINDING does not exist, so nothing bounds what $NS admits — the namespace has not converged, or the binding was removed."
jq -e '(.spec.validationActions // []) | index("Deny") != null' "$WORK/binding.json" > /dev/null 2>&1 \
    || refuse "ValidatingAdmissionPolicyBinding $BINDING does not Deny (validationActions $(jq -c '.spec.validationActions // null' "$WORK/binding.json" 2>/dev/null)), so nothing bounds what $NS admits."
# A binding by the right name that binds another policy, or binds this
# one to fewer resources or under a parameter, is not the declared
# bound (review 3e750cce, finding 3a).
HAVE_POLICY="$(jq -r '.spec.policyName // ""' "$WORK/binding.json" 2>/dev/null)"
[ "$HAVE_POLICY" = "$POLICY" ] \
    || refuse "ValidatingAdmissionPolicyBinding $BINDING names policy '$HAVE_POLICY', not $POLICY — the declared policy is not what it binds, so nothing this checkout declares bounds what $NS admits."
# Read as a VALUE and compared, not as jq's exit status: an answer jq
# cannot read is then a refusal too, never a pass.
for key in matchResources paramRef; do
    [ "$(jq -r --arg k "$key" '.spec | has($k)' "$WORK/binding.json" 2>/dev/null)" = false ] \
        || refuse "ValidatingAdmissionPolicyBinding $BINDING carries a $key ($(jq -c --arg k "$key" '.spec[$k]' "$WORK/binding.json" 2>/dev/null | cut -c1-300)), which the tree's binding does not: the policy may no longer reach everything in $NS."
done
kget "$WORK/policy.json" validatingadmissionpolicy "$POLICY" --ignore-not-found -o json \
    || unread "could not read ValidatingAdmissionPolicy $POLICY: $(words "$WORK/k.err")— nothing was created."
jq_doc_file "$WORK/policy.json" \
    || refuse "ValidatingAdmissionPolicy $POLICY does not exist — binding $BINDING points at nothing, so nothing bounds what $NS admits."
HAVE_FAILURE="$(jq -r '.spec.failurePolicy // "unset"' "$WORK/policy.json" 2>/dev/null)"
[ "$HAVE_FAILURE" = Fail ] \
    || refuse "ValidatingAdmissionPolicy $POLICY does not fail closed: its failurePolicy is '$HAVE_FAILURE', and the tree declares Fail."

kget "$WORK/cronjob.json" cronjob "$CRONJOB" -n "$NS" --ignore-not-found -o json \
    || unread "could not read CronJob $CRONJOB in $NS: $(words "$WORK/k.err")— nothing was created."
jq_doc_file "$WORK/cronjob.json" \
    || refuse "CronJob $CRONJOB does not exist in $NS — the scheduled trim has not converged onto this cluster, so there is no declared trim to run."

# The stop switch (review 3e750cce, finding 2). Anything but an absent
# or false `suspend` stops here.
[ "$(jq -c '.spec.suspend // false' "$WORK/cronjob.json" 2>/dev/null)" = false ] \
    || refuse "CronJob $CRONJOB is SUSPENDED on the server (.spec.suspend reads $(jq -c '.spec.suspend' "$WORK/cronjob.json" 2>/dev/null)) — that is the in-cluster stop switch on the trim, and this verb does not run past it. Whoever set it decides when the trim runs again."

# The server's CronJob against this checkout's, on what the header says
# is compared.
[ "$(jq -r '.spec.jobTemplate.spec.template.spec.containers | if type == "array" then length else "none" end' "$WORK/cronjob.json" 2>/dev/null)" = 1 ] \
    || refuse "CronJob $CRONJOB on the server does not carry exactly one container — it is not the trim this checkout declares."
HAVE_IMAGE="$(jq -r '.spec.jobTemplate.spec.template.spec.containers[0].image // ""' "$WORK/cronjob.json")"
[ "$HAVE_IMAGE" = "$WANT_IMAGE" ] \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the server's image is '$HAVE_IMAGE' and the manifest declares '$WANT_IMAGE'. A trim is not run with a spec the tree does not declare; the next converge settles which is right."
jq -j '.spec.jobTemplate.spec.template.spec.containers[0].args // [] | if length == 1 then .[0] else "" end' "$WORK/cronjob.json" > "$WORK/have.script"
cmp -s "$WORK/have.script" "$WORK/want.script" \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the script the server holds ($(wc -c < "$WORK/have.script" | tr -d ' ') bytes) is not the manifest's ($(wc -c < "$WORK/want.script" | tr -d ' ') bytes), byte for byte."
HAVE_DEADLINE="$(jq -r '.spec.jobTemplate.spec.activeDeadlineSeconds // ""' "$WORK/cronjob.json")"
[ "$HAVE_DEADLINE" = "$WANT_DEADLINE" ] \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the server's activeDeadlineSeconds is '${HAVE_DEADLINE:-unset}' and the manifest declares $WANT_DEADLINE — this verb's wait is derived from it."
# How many pods the Job may ask for: no admission rule bounds these.
HAVE_BACKOFF="$(jq -r '.spec.jobTemplate.spec.backoffLimit // "unset"' "$WORK/cronjob.json")"
[ "$HAVE_BACKOFF" = "$WANT_BACKOFF" ] \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the server's backoffLimit is '$HAVE_BACKOFF' and the manifest declares $WANT_BACKOFF — a failed trim would be retried as more pods."
HAVE_PARALLELISM="$(jq -r '.spec.jobTemplate.spec.parallelism // 1' "$WORK/cronjob.json")"
[ "$HAVE_PARALLELISM" = "$WANT_PARALLELISM" ] \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the server's parallelism is '$HAVE_PARALLELISM' and the manifest declares $WANT_PARALLELISM (unset is 1) — the Job would ask for that many trim pods at once."
HAVE_COMPLETIONS="$(jq -r '.spec.jobTemplate.spec.completions // 1' "$WORK/cronjob.json")"
[ "$HAVE_COMPLETIONS" = "$WANT_COMPLETIONS" ] \
    || refuse "CronJob $CRONJOB has drifted from this checkout: the server's completions is '$HAVE_COMPLETIONS' and the manifest declares $WANT_COMPLETIONS (unset is 1) — the Job would run the trim that many times."

# No trim in flight: every Job there finished, every pod there terminal.
kget "$WORK/jobs.json" jobs -n "$NS" -o json \
    || unread "could not list the Jobs in $NS: $(words "$WORK/k.err")— nothing was created."
kget "$WORK/pods.json" pods -n "$NS" -o json \
    || unread "could not list the pods in $NS: $(words "$WORK/k.err")— nothing was created."
if ! jq_doc_file "$WORK/jobs.json" || ! jq_doc_file "$WORK/pods.json" \
        || ! busy="$(jq -r -n --slurpfile j "$WORK/jobs.json" --slurpfile p "$WORK/pods.json" '
            [ ($j[0].items[] | select(([.status.conditions[]? | select((.type == "Complete" or .type == "Failed") and .status == "True")] | length) == 0)
                | "Job \(.metadata.name) (created \(.metadata.creationTimestamp // "?"), active \(.status.active // 0))"),
              ($p[0].items[] | select((.status.phase // "") != "Succeeded" and (.status.phase // "") != "Failed")
                | "pod \(.metadata.name) (\(.status.phase // "no phase"))") ] | join("; ")' 2> "$WORK/busy.err")"; then
    unread "the Job and pod lists of $NS would not parse ($(words "$WORK/busy.err")) — nothing was created."
fi
[ -z "$busy" ] || refuse "a trim is already in flight in $NS: $busy. Two at once is not what this verb is for; read that one's outcome, or file this again when it has finished."

# --- what the wait and the removal read, defined before the write so the
# EXIT trap finds remove_job whenever a Job has been made ---------------
# remove_job — delete the Job this run made, and only it: the name must
# still carry UID_MADE when read. Prints what it found. Returns 0 when
# the Job read back gone, 1 otherwise.
remove_job() {
    local now
    TRIED=1
    if ! kget "$WORK/del.json" job "$NAME" -n "$NS" --ignore-not-found -o json; then
        echo "$ME: Job $NAME was NOT removed — it could not be read before the delete: $(words "$WORK/k.err")" >&2
        return 1
    fi
    if ! jq_doc_file "$WORK/del.json"; then
        REMOVED=1
        echo "$ME: Job $NAME is already gone (something else removed it)"
        return 0
    fi
    now="$(jq -r '.metadata.uid // empty' "$WORK/del.json" 2>/dev/null)"
    if [ "$now" != "$UID_MADE" ]; then
        REMOVED=1
        echo "$ME: the name $NAME is now held by uid ${now:-unreadable}, not the Job this run made ($UID_MADE) — left alone" >&2
        return 1
    fi
    # shellcheck disable=SC2086
    if ! $K delete job "$NAME" -n "$NS" --wait=true --timeout=60s --request-timeout=70s \
            > "$WORK/del.out" 2> "$WORK/del.err"; then
        echo "$ME: the delete of Job $NAME failed: $(words "$WORK/del.err")" >&2
    fi
    # A delete's answer is not its effect: read the name back.
    if ! kget "$WORK/back.json" job "$NAME" -n "$NS" --ignore-not-found -o json; then
        echo "$ME: Job $NAME is NOT proven removed — it could not be read back: $(words "$WORK/k.err")" >&2
        return 1
    fi
    if jq_doc_file "$WORK/back.json"; then
        echo "$ME: Job $NAME is STILL THERE after the delete" >&2
        return 1
    fi
    REMOVED=1
    return 0
}

# pods_of_job — the Job's pods into $WORK/jp.json. Status is the read's.
pods_of_job() {
    kget "$WORK/jp.json" pods -n "$NS" -l "job-name=$NAME" -o json && jq_doc_file "$WORK/jp.json"
}

# pod_says — one line per pod of the Job: phase, where it is, and the
# container's waiting reason or its exit.
pod_says() {
    jq -r '.items[] | . as $p
        | ([.status.containerStatuses[]? | .state
            | if .waiting then "waiting: \(.waiting.reason // "?")\(if .waiting.message then " — " + .waiting.message else "" end)"
              elif .terminated then "exited \(.terminated.exitCode) (\(.terminated.reason // "?"))"
              elif .running then "running since \(.running.startedAt // "?")"
              else empty end] | join(", ")) as $c
        | ([.status.conditions[]? | select(.type == "PodScheduled" and .status != "True")
            | "not scheduled: \(.reason // "?")\(if .message then " — " + .message else "" end)"] | join(", ")) as $s
        | "pod \($p.metadata.name): \($p.status.phase // "no phase")\(if $p.spec.nodeName then " on " + $p.spec.nodeName else "" end)\(if $c != "" then "; " + $c else "" end)\(if $s != "" then "; " + $s else "" end)"' \
        "$WORK/jp.json" 2>/dev/null
}

# why_no_pod — the Job's own Warning events (FailedCreate carries the
# admission policy's message), or that there are none to read.
why_no_pod() {
    local ev
    if kget "$WORK/events.json" events -n "$NS" --field-selector "involvedObject.kind=Job,involvedObject.name=$NAME" -o json \
            && ev="$(jq -r '[.items[] | select(.type == "Warning") | "\(.reason): \(.message)"] | unique | join(" | ")' "$WORK/events.json" 2>/dev/null)" \
            && [ -n "$ev" ]; then
        printf '%s' "$ev" | cut -c1-900
    else
        printf 'the Job has no pod and its events name no reason'
    fi
}

# --- the one write -------------------------------------------------------
NAME="$PREFIX$(date -u +%Y%m%d%H%M%S)"
# THIS LINE SAYS WHAT WAS READ, AND NO MORE (review 3e750cce, finding
# 3): each clause is one check above, and what was not compared is said.
echo "$ME: what this run read holds — binding $BINDING names policy $POLICY, lists Deny and carries no matchResources and no paramRef; that policy exists with failurePolicy Fail (its rules were not read); CronJob $CRONJOB on the server is not suspended and equals this checkout's manifest on what was compared: one container, image $WANT_IMAGE, its script byte for byte, activeDeadlineSeconds $WANT_DEADLINE, backoffLimit $WANT_BACKOFF, parallelism $WANT_PARALLELISM, completions $WANT_COMPLETIONS. The rest of its pod template was NOT compared by this run: the admission policy judges it when the Job and its pod are created. In $NS there is no unfinished Job and no non-terminal pod. Creating Job $NAME from that CronJob."
# shellcheck disable=SC2086
if ! $K create job "$NAME" "--from=cronjob/$CRONJOB" -n "$NS" -o json --request-timeout=30s \
        > "$WORK/created.json" 2> "$WORK/create.err"; then
    # A NON-ZERO CREATE IS NOT A JOB THAT WAS NOT MADE (review 3e750cce,
    # finding 1). kubectl exits non-zero for a refusal — the admission
    # policy's sentence, quoted below — and equally for an answer that
    # never arrived: its request timeout, or a connection dropped after
    # the API server committed the Job. So the name is read back, and
    # "made no Job" is printed only behind a read that shows it.
    #
    # NOTHING IS DELETED ON THIS PATH, whatever the read shows. A Job
    # standing under the name is NOT PROVABLY this run's: the name is
    # the UTC second, which a hand run or a second runner can mint too
    # (AlreadyExists is exactly that case), and what ties an object to
    # this run everywhere else in this file is the uid the create
    # answered — which a failed create did not give. So it is reported
    # as standing, by name and uid, and left for a reader. What stands
    # is at most the admitted pod under its own deadline.
    NAME_WAS="$NAME"
    NAME=""
    said="$(words "$WORK/create.err")"
    said="${said% }"
    if ! kget "$WORK/after.json" job "$NAME_WAS" -n "$NS" --ignore-not-found -o json; then
        unread "the create of Job $NAME_WAS did not answer success (${said}) and the name could not be read back ($(words "$WORK/k.err")) — whether a Job was made is UNKNOWN: one may be standing in $NS and running the trim. Nothing was removed by this run. Look for it: kubectl -n $NS get job $NAME_WAS"
    fi
    if [ -s "$WORK/after.json" ]; then
        fail "the create did not answer success (${said}) — and a Job named $NAME_WAS IS STANDING in $NS (uid $(jq -r '.metadata.uid // "unreadable"' "$WORK/after.json" 2>/dev/null), created $(jq -r '.metadata.creationTimestamp // "unreadable"' "$WORK/after.json" 2>/dev/null), active $(jq -r '.status.active // 0' "$WORK/after.json" 2>/dev/null)). It may be this run's, made by a create whose answer was lost, or another's: the create gave no uid, so it is not provably this run's and is NOT removed by this run. It is at most the admitted trim pod, under its own deadline of ${WANT_DEADLINE}s. Read it: kubectl -n $NS get job $NAME_WAS"
    fi
    fail "the API server did not create Job $NAME_WAS: ${said} — read back: no Job of that name exists in $NS, so this run made no Job and removes none."
fi
UID_MADE="$(jq -r '.metadata.uid // empty' "$WORK/created.json" 2>/dev/null)"
if [ -z "$UID_MADE" ]; then
    # The create answered success without the object. Without its uid a
    # later Job of this name could not be told from this one, so this
    # run will not delete by name alone.
    fail "the create of Job $NAME answered success but gave no uid ($(head -c 200 "$WORK/created.json" | tr '\n' ' ')) — the Job, if it exists, is NOT removed by this run, because it cannot be proven to be the one this run made. Read it: kubectl -n $NS get job $NAME"
fi
echo "$ME: created Job $NAME (uid $UID_MADE)"

# --- the wait --------------------------------------------------------------
echo "$ME: waiting for Job $NAME (up to ${WAIT_S}s: its own deadline of ${WANT_DEADLINE}s and ${MARGIN_S}s for the controller; a pod not started within ${START_GRACE_S}s fails then)"
t0=$SECONDS
polls=0
dark=0
OUTCOME=""
WHY=""
while :; do
    waited=$((polls * POLL_S))
    [ $((SECONDS - t0)) -le "$waited" ] || waited=$((SECONDS - t0))
    read_ok=1
    if ! kget "$WORK/job.json" job "$NAME" -n "$NS" --ignore-not-found -o json; then
        read_ok=0
        cp "$WORK/k.err" "$WORK/dark.err"
    elif jq_doc_file "$WORK/job.json" && ! pods_of_job; then
        read_ok=0
        cp "$WORK/k.err" "$WORK/dark.err"
    fi
    if [ "$read_ok" = 1 ]; then
        dark=0
        # No Job of that name (the read answers nothing), or the name
        # under another uid: the Job this run made is gone.
        if [ "$(jq -r '.metadata.uid // empty' "$WORK/job.json" 2>/dev/null)" != "$UID_MADE" ]; then
            OUTCOME=gone
            break
        fi
        state="$(jq -r '[.status.conditions[]? | select(.status == "True")] as $c
            | if ($c | map(select(.type == "Failed")) | length) > 0 then ($c | map(select(.type == "Failed"))[0] | "failed\t\(.reason // "?")\t\(.message // "")")
              elif ($c | map(select(.type == "Complete")) | length) > 0 then "complete\t\t"
              else "running\t\t" end' "$WORK/job.json" 2>/dev/null)"
        case "$state" in
            complete*) OUTCOME=complete; break ;;
            failed*)
                OUTCOME=failed
                WHY="the Job's Failed condition says $(printf '%s' "$state" | cut -f2): $(printf '%s' "$state" | cut -f3)"
                break ;;
        esac
        started="$(jq -r '[.items[] | select((.status.phase // "") as $ph | $ph == "Running" or $ph == "Succeeded" or $ph == "Failed"
            or ([.status.containerStatuses[]? | select(.state.running or .state.terminated)] | length) > 0)] | length' "$WORK/jp.json" 2>/dev/null)"
        if [ "${started:-0}" = 0 ] && [ "$waited" -ge "$START_GRACE_S" ]; then
            OUTCOME=never-started
            if [ "$(jq -r '.items | length' "$WORK/jp.json")" = 0 ]; then
                WHY="no pod was created for it in ${waited}s — $(why_no_pod)"
            else
                WHY="its pod had not started after ${waited}s — $(pod_says | tr '\n' ' ')"
            fi
            break
        fi
    else
        dark=$((dark + 1))
        if [ "$dark" -ge "$UNREAD_MAX" ]; then
            OUTCOME=dark
            break
        fi
    fi
    if [ "$waited" -ge "$WAIT_S" ]; then
        OUTCOME=timeout
        WHY="it had neither completed nor failed after ${waited}s, past its own deadline of ${WANT_DEADLINE}s — $(pod_says | tr '\n' ' ')"
        break
    fi
    sleep "$POLL_S"
    polls=$((polls + 1))
done

# --- the trim's own record ---------------------------------------------------
# From the log; from the termination message when the log cannot be
# read; and said to be absent when neither can.
RECORD="$WORK/record"
: > "$RECORD"
if [ "$OUTCOME" != dark ] && [ "$OUTCOME" != gone ]; then
    pods_of_job || :
    pod="$(jq -r '.items | sort_by(.metadata.creationTimestamp) | last | .metadata.name // empty' "$WORK/jp.json" 2>/dev/null)"
    if [ -n "$pod" ]; then
        pod_says | sed "s/^/$ME: /"
        # shellcheck disable=SC2086
        if $K logs "$pod" -n "$NS" -c trim --tail=200 --request-timeout=20s > "$RECORD" 2> "$WORK/logs.err" && [ -s "$RECORD" ]; then
            echo "$ME: the trim's own output (the log of $pod):"
        else
            jq -r --arg p "$pod" '.items[] | select(.metadata.name == $p) | .status.containerStatuses[]? | .state.terminated.message // empty' "$WORK/jp.json" > "$RECORD" 2>/dev/null
            if [ -s "$RECORD" ]; then
                echo "$ME: the trim's own output (the termination message of $pod; its log could not be read: $(words "$WORK/logs.err")):"
            else
                echo "$ME: $pod left no output this run could read (log: $(words "$WORK/logs.err"); no termination message)"
            fi
        fi
        sed 's/^/    /' "$RECORD"
    fi
fi

# --- the verdict -----------------------------------------------------------------
case "$OUTCOME" in
    dark)
        if remove_job; then gone_line="Job $NAME was removed (read back: NotFound)"; else gone_line="Job $NAME was NOT proven removed"; fi
        unread "Job $NAME was created, and then the cluster could not be read $UNREAD_MAX times running ($(words "$WORK/dark.err")) — whether the trim ran is UNKNOWN, neither failed nor ok. $gone_line."
        ;;
    gone)
        REMOVED=1
        fail "Job $NAME (uid $UID_MADE) was removed by something else while this run waited on it — its outcome is unknown, and there is nothing left for this run to remove."
        ;;
esac

exit_code="$(jq -r '[.items[] | .status.containerStatuses[]? | .state.terminated.exitCode | select(. != null)] | last // empty' "$WORK/jp.json" 2>/dev/null)"
if [ "$OUTCOME" = complete ]; then
    missing=""
    for m in $MOUNTS; do
        grep -Eq "^node-trim: $m: .*trimmed" "$RECORD" || missing="$missing $m"
    done
    if [ "${exit_code:-unread}" != 0 ]; then
        OUTCOME=failed
        WHY="the Job says Complete but its pod's exit code reads '${exit_code:-unread}', so the trim is not proven"
    elif [ -n "$missing" ] || ! grep -qx 'node-trim: OK' "$RECORD"; then
        OUTCOME=failed
        WHY="the Job completed and its pod exited 0, but the record this run could read does not show the trim: no 'trimmed' line for${missing:- (none missing)}, closing OK line $(grep -qx 'node-trim: OK' "$RECORD" && echo present || echo absent)"
    fi
fi

if remove_job; then
    removed_line="Job $NAME removed (read back: NotFound)"
    removed=1
else
    removed_line="Job $NAME was NOT proven removed — read it: kubectl -n $NS get job $NAME"
    removed=0
fi

if [ "$OUTCOME" != complete ]; then
    fail "the trim did not succeed: $WHY${exit_code:+ (pod exit code $exit_code)}. $removed_line."
fi
before="$(grep -c '^node-trim: before ' "$RECORD")"
after="$(grep -c '^node-trim: after ' "$RECORD")"
if [ "$removed" != 1 ]; then
    fail "the trim itself SUCCEEDED (Job $NAME completed, pod exit 0, both mounts trimmed, closing OK) — but $removed_line"
fi
echo "$ME: OK — Job $NAME completed and its pod exited 0; its record names $MOUNTS trimmed and closes OK, with $before discard-counter line(s) before and $after after; Job $NAME removed (read back: NotFound)"
