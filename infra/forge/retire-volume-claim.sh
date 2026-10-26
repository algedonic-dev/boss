#!/usr/bin/env bash
# retire-volume-claim — retire ONE PersistentVolumeClaim through a plan
# David signs with his passkey.
#
#   retire-volume-claim.sh --plan <namespace> <claim>
#   retire-volume-claim.sh <namespace> <claim> <plan-sha256>
#
# WHY IT EXISTS (backlog f36eb7ce, design b4b1c99d). Retiring two
# superseded claims on 2026-10-10 (gate-runner-disk, gate-seed; packet
# 1f13b894) needed David to run kubectl by hand with his admin
# kubeconfig: the dev session cannot delete a claim, and
# delete-orphan-object withholds PersistentVolumeClaim from its kind
# floor on purpose ("bytes on Longhorn ... stay a named human step").
# David, 2026-10-18, answering the design review on f36eb7ce: that step
# must NOT stay a human command line — manually running any ops command
# is a gap in BOSS's coverage. The human act is the approval, not the
# command. So a claim is retired the way expand-dev-work-volume grows
# one: the read-only `plan-a-volume-claim-retirement` renders this
# script's --plan onto the request's approve step, David's passkey signs
# those bytes, and the runner hands this script sha256 of the SIGNED
# plan (design 17835005).
#
# THE AUTHORITY IS DERIVED, NOT GRANTED — the delete-orphan-object
# design, kept for bytes. There is no "delete any claim by name" here.
# A claim is retirable only when every one of these holds, each refusing
# by name (exit 78) and checked again at the write:
#   * the namespace and name are DNS labels (re-checked here, not only in
#     the allowlist: a bound in one layer disappears the day something
#     else calls the script);
#   * the name is not a PROTECTED claim — pgdata-postgres-0 (the system
#     of record, in every instance namespace) and boss-dev-work (the dev
#     session's work volume) — in ANY namespace, before anything is read;
#   * the declaration set is COMMITTED: the manifest directories the
#     derivation reads are clean in git, read as the checkout's owner;
#   * infra/cluster/undeclared-objects.sh --check names the claim
#     undeclared — the one definition of "a manifest declares it"
#     (volumeClaimTemplate claims included), which also refuses a
#     namespace the tree does not own, an exempt object, a claim a
#     controller owns (the dev-build workers' claims are owned by their
#     Job, whose deletion releases them) and a claim that is not live;
#   * the claim is not already being deleted and carries no owner;
#   * its PersistentVolume, when bound, names THIS claim by uid, and its
#     reclaim is one this script can read back: Retain (the volume and
#     its bytes are kept, and said so), or Delete on a Longhorn volume
#     (the bytes go, and the Longhorn volume is read back gone);
#   * Longhorn reports that volume DETACHED — attached means a consumer
#     the namespace read did not show;
#   * no Pod (in ANY phase), Job, CronJob, Deployment, StatefulSet,
#     DaemonSet or ReplicaSet in the claim's namespace mounts it, and no
#     StatefulSet's volumeClaimTemplate would stamp it again. A claim can
#     only be mounted from its own namespace, so that read is complete;
#     a namespace that cannot be read is a failure, never an empty one.
#
# THE PLAN IS THE SIGNED BYTES. It names the claim (uid, size, class,
# labels, finalizers, creation), its volume (uid, reclaim policy, source),
# the Longhorn volume (size, state), the consumer verdict and the one
# DELETE it authorises. No clock, no resourceVersion, no tree sha: two
# renders of one true state are byte-identical, and a fact that moves
# moves the hash, which voids the approval (exit 78, both hashes named).
# A second run of an applied plan finds the claim gone and refuses, so
# one approval deletes at most once.
#
# CAPTURE BEFORE DELETE, ON THE RECORD. The PVC, the PV and the Longhorn
# volume are read as YAML immediately before the delete and ATTACHED to
# this ops-request through `boss attach` (the attach carrier the
# dev-work expansion uses), which reads the bytes back before it says
# so. Only a verified attachment lets the delete run: stdout reaches the
# packet only when this script ends, and a deleted object cannot be read
# again, so a capture that waited for the end could be lost with the
# run. The capture is printed on stdout as well.
#
# THE DELETE carries the API's own DeleteOptions.preconditions.uid, so a
# same-named claim created between the re-render and the delete is
# refused by the API server (409), not by a read-then-delete here. Then
# it reads back: the claim gone; for reclaim Delete, the PV and the
# Longhorn volume gone; for Retain, the PV kept. Only that complete
# read-back prints the effect line (infra/ops/verbs/retire-volume-claim
# .json `effect`); a claim held Terminating (a consumer appeared), a
# volume that outlives its Delete reclaim, or a read that cannot answer
# exits 1 naming what it saw.
#
# ENV (test seams; the ops runner passes a verb no environment from the
# packet, only an argv built from the allowlist): BOSS_CLUSTER_TREE and
# BOSS_KUBECTL, read by undeclared-objects.sh; BOSS_BIN, the boss CLI
# (default: `boss` on PATH, else /usr/local/bin/boss, as
# infra/gcp/publish-workflow.sh resolves it); BOSS_RETIRE_CLAIM_WAIT_S,
# how long each read-back waits (default 180). OPS_REQUEST_ID is set by
# the ops runner for the execute step; the write refuses without it,
# because the capture has nowhere to land.
#
# Exit 78 is a refusal (the request was wrong or no longer true); exit 1
# is a step that genuinely failed, including a read that could not look.
set -uo pipefail

ME=retire-volume-claim
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
DERIVE="$REPO/infra/cluster/undeclared-objects.sh"
TREE="${BOSS_CLUSTER_TREE:-${BOSS_OPS_CHECKOUT:-$REPO}}"
# The manifest directories undeclared-objects.sh reads its declaration
# set from ($DIR and OTHER_MANIFESTS there).
DECLARATION_DIRS=(infra/cluster/manifests infra/gate-runner)
# By NAME, in every namespace: the system of record's database claim
# (one per instance namespace) and the dev session's work volume. Both
# are declared today, so the derivation refuses them too; this list is
# the floor that holds if a manifest ever stops saying so.
PROTECTED=(pgdata-postgres-0 boss-dev-work)
CONSUMER_KINDS=pods,jobs,cronjobs,deployments,statefulsets,daemonsets,replicasets
LONGHORN_NS=longhorn-system
LONGHORN_DRIVER=driver.longhorn.io
# Three objects' YAML is a few KiB; a capture past this is not the shape
# this script expects, and boss attach's own bound is 2 MiB.
CAPTURE_MAX=1048576

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
say() { echo "$ME: $*" >&2; }

# --- the arguments ----------------------------------------------------------
PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 2 ] || refuse "usage: $ME --plan <namespace> <claim> — one claim, named in full"
else
    [ $# -eq 3 ] || refuse "usage: $ME <namespace> <claim> <plan-sha256> — this deletes only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-volume-claim-retirement verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
NS="$1"
CLAIM="$2"
[[ "$NS" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
    || refuse "the namespace '$NS' is not a DNS label"
[[ "$CLAIM" =~ ^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]] \
    || refuse "the claim '$CLAIM' is not a DNS subdomain name"
for p in "${PROTECTED[@]}"; do
    [ "$CLAIM" != "$p" ] || refuse "$CLAIM is a protected claim (${PROTECTED[*]}) in every namespace, so no plan retires it"
done
if [ "$PLAN" -eq 0 ]; then
    [[ "${OPS_REQUEST_ID:-}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "OPS_REQUEST_ID is '${OPS_REQUEST_ID:-}', not a request id — the capture of what is deleted has nowhere to land, so nothing is deleted (the ops runner sets it for the execute step)"
fi

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the claim cannot be read, and no evidence is not a plan"
# jq-1.6 passes `jq -e` over an input holding no document (backlog
# d96e38ab), so every identity check below asks jq_doc_file first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
[ -x "$DERIVE" ] || refuse "the derivation $DERIVE is missing or not executable, so there is no authority to act on"
WAIT_S="${BOSS_RETIRE_CLAIM_WAIT_S:-180}"
case "${WAIT_S:-empty}" in
    empty | *[!0-9]*) fail "BOSS_RETIRE_CLAIM_WAIT_S '$WAIT_S' is not whole seconds" ;;
esac

WORK=$(mktemp -d) || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- a committed declaration set, read as the checkout's owner --------------
# The same bound, and the same reason, as delete-orphan-object.sh's
# bound 2 and its as_owner: the runner executes verbs as root, git
# refuses a clone root does not own, and silencing that with
# safe.directory would make root-owned objects in the owner's clone one
# edit away. So every git read drops to the owner, read off the
# directory.
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so there is no declaration set to read"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}') to an account on this host, so there is no account to read its git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
if ! as_owner "git -C '$TREE' rev-parse --git-dir" >/dev/null 2> "$WORK/git.err"; then
    sed 's/^/    /' "$WORK/git.err" >&2
    refuse "cannot read $TREE as a git checkout (read as '$OWNER'), so the declaration set is not reviewable content (git's words above)"
fi
dirty=$(as_owner "git -C '$TREE' status --porcelain -- ${DECLARATION_DIRS[*]}" 2>&1)
if [ -n "$dirty" ]; then
    printf '    %s\n' "$dirty" >&2
    refuse "${DECLARATION_DIRS[*]} carry uncommitted changes (above), so the tree that would prove this claim undeclared is not one anybody reviewed"
fi
say "declaration set: ${DECLARATION_DIRS[*]} at $(as_owner "git -C '$TREE' rev-parse --short HEAD" 2>/dev/null || echo '?') (owner $OWNER)"

# --- the derivation: a manifest declares it, or not --------------------------
BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --check "PersistentVolumeClaim/$NS/$CLAIM" > "$WORK/check.out" 2> "$WORK/check.err"
rc=$?
# The derivation's own words, on every path.
cat "$WORK/check.err" >&2
case "$rc" in
    0) ;;
    3) refuse "undeclared-objects.sh does not name PersistentVolumeClaim/$NS/$CLAIM undeclared (its reason above)" ;;
    *) fail "undeclared-objects.sh could not decide (exit $rc, above) — 'could not look' is not 'undeclared'" ;;
esac
IFS=$'\t' read -r verdict kind cns cname < "$WORK/check.out"
[ "${verdict:-}/${kind:-}/${cns:-}/${cname:-}" = "undeclared/PersistentVolumeClaim/$NS/$CLAIM" ] \
    || fail "the derivation answered '$(cat "$WORK/check.out")', not about PersistentVolumeClaim/$NS/$CLAIM"

KUBECTL_LINE=$(BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --kubectl) || fail "no kubectl to read the claim with (above)"
read -r -a KUBECTL <<< "$KUBECTL_LINE"

# read_json <out> <resource> <name> [-n ns] — one object as JSON. 0 read,
# 2 NotFound (the server said so), 1 any other failure, words in <out>.err.
read_json() {
    local out="$1"; shift
    if "${KUBECTL[@]}" get "$@" -o json --request-timeout=30s > "$out" 2> "$out.err"; then return 0; fi
    grep -q 'NotFound' "$out.err" && return 2
    return 1
}

# --- the claim ----------------------------------------------------------------
read_json "$WORK/claim.json" PersistentVolumeClaim "$CLAIM" -n "$NS"
case $? in
    0) ;;
    2) refuse "there is no claim $NS/$CLAIM (NotFound) — a delete of it would be a no-op reported as work" ;;
    *) sed 's/^/    /' "$WORK/claim.json.err" >&2; fail "could not read the claim $NS/$CLAIM (above)" ;;
esac
jq_doc_file "$WORK/claim.json" || fail "the API server's answer for $NS/$CLAIM holds no JSON document — no answer is not a claim"
jq -e --arg n "$CLAIM" --arg ns "$NS" '.kind == "PersistentVolumeClaim" and .metadata.name == $n
    and ((.metadata.namespace // $ns) == $ns) and ((.metadata.uid // "") != "")' "$WORK/claim.json" > /dev/null \
    || fail "the API server's answer for $NS/$CLAIM is not that claim with a uid"
CUID=$(jq -r '.metadata.uid' "$WORK/claim.json")
[ "$(jq -r '.metadata.deletionTimestamp // ""' "$WORK/claim.json")" = "" ] \
    || refuse "$NS/$CLAIM is already being deleted (deletionTimestamp $(jq -r '.metadata.deletionTimestamp' "$WORK/claim.json"), finalizers $(jq -r '(.metadata.finalizers // []) | join(",")' "$WORK/claim.json")) — Kubernetes is holding it for a consumer; that is not this verb's to force"
[ "$(jq '(.metadata.ownerReferences // []) | length' "$WORK/claim.json")" = 0 ] \
    || refuse "$NS/$CLAIM carries an ownerReference ($(jq -c '.metadata.ownerReferences' "$WORK/claim.json")); deleting its owner releases it"

# --- its volume, and the bytes behind it ----------------------------------------
VOL=$(jq -r '.spec.volumeName // ""' "$WORK/claim.json")
POLICY=""; SOURCE=""; LH=""
if [ -n "$VOL" ]; then
    read_json "$WORK/pv.json" PersistentVolume "$VOL"
    case $? in
        0) ;;
        2) refuse "$NS/$CLAIM names PersistentVolume $VOL, which does not exist (NotFound) — a claim and volume that disagree are not a state this verb retires" ;;
        *) sed 's/^/    /' "$WORK/pv.json.err" >&2; fail "could not read PersistentVolume $VOL (above)" ;;
    esac
    jq_doc_file "$WORK/pv.json" || fail "the API server's answer for PersistentVolume $VOL holds no JSON document"
    jq -e --arg n "$CLAIM" --arg ns "$NS" --arg u "$CUID" \
        '.spec.claimRef.name == $n and .spec.claimRef.namespace == $ns and .spec.claimRef.uid == $u' \
        "$WORK/pv.json" > /dev/null \
        || refuse "PersistentVolume $VOL's claimRef is $(jq -c '.spec.claimRef // null' "$WORK/pv.json"), not $NS/$CLAIM uid $CUID — it is not this claim's volume"
    POLICY=$(jq -r '.spec.persistentVolumeReclaimPolicy // ""' "$WORK/pv.json")
    SOURCE=$(jq -r 'if .spec.csi then "csi \(.spec.csi.driver) \(.spec.csi.volumeHandle)"
        elif .spec.local then "local \(.spec.local.path)"
        elif .spec.hostPath then "hostPath \(.spec.hostPath.path)"
        else "other (\(.spec | keys | join(",")))" end' "$WORK/pv.json")
    case "$POLICY" in
        Retain) ;;
        Delete)
            [ "$(jq -r '.spec.csi.driver // ""' "$WORK/pv.json")" = "$LONGHORN_DRIVER" ] \
                || refuse "PersistentVolume $VOL reclaims by Delete through $SOURCE; this verb reads a Delete reclaim back only for Longhorn ($LONGHORN_DRIVER), so it cannot prove those bytes released"
            ;;
        *) refuse "PersistentVolume $VOL reclaims by '${POLICY:-none}'; this verb retires only Retain (kept) or Delete (read back gone)" ;;
    esac
    if [ "$(jq -r '.spec.csi.driver // ""' "$WORK/pv.json")" = "$LONGHORN_DRIVER" ]; then
        LH=$(jq -r '.spec.csi.volumeHandle // ""' "$WORK/pv.json")
        [[ "$LH" =~ ^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]] || fail "PersistentVolume $VOL's Longhorn volumeHandle '$LH' is not a name"
        read_json "$WORK/lh.json" volumes.longhorn.io "$LH" -n "$LONGHORN_NS"
        case $? in
            0) ;;
            2) refuse "PersistentVolume $VOL names Longhorn volume $LONGHORN_NS/$LH, which does not exist (NotFound)" ;;
            *) sed 's/^/    /' "$WORK/lh.json.err" >&2; fail "could not read Longhorn volume $LONGHORN_NS/$LH (above)" ;;
        esac
        state=$(jq -r '.status.state // ""' "$WORK/lh.json")
        [ "$state" = detached ] \
            || refuse "Longhorn reports volume $LH ${state:-in no state} on node '$(jq -r '.status.currentNodeID // ""' "$WORK/lh.json")' — attached means something mounts it that the namespace read did not show; it must be detached"
    fi
fi

# --- consumers -----------------------------------------------------------------
if ! "${KUBECTL[@]}" get "$CONSUMER_KINDS" -n "$NS" -o json --request-timeout=30s \
        > "$WORK/consumers.json" 2> "$WORK/consumers.err"; then
    sed 's/^/    /' "$WORK/consumers.err" >&2
    fail "could not read $CONSUMER_KINDS in $NS (above) — a namespace nobody read is not one with no consumer, so there is no plan"
fi
if ! jq -r --arg c "$CLAIM" --arg ns "$NS" '
    .items[] as $o | ($o.kind // "?") as $k
    | (if $k == "Pod" then $o.spec
       elif $k == "CronJob" then $o.spec.jobTemplate.spec.template.spec
       else $o.spec.template.spec end) as $ps
    | ([($ps.volumes // [])[] | select(.persistentVolumeClaim.claimName == $c)] | length > 0) as $mounts
    | (if $k == "StatefulSet" then
         [($o.spec.volumeClaimTemplates // [])[]
          | (.metadata.name // "") as $t | "\($t)-\($o.metadata.name)-" as $p
          | select($t != "" and ($c | startswith($p)) and ($c[($p | length):] | test("^[0-9]+$")))]
         | length > 0
       else false end) as $stamps
    | select($mounts or $stamps)
    | "\($k) \($ns)/\($o.metadata.name)"
      + (if $k == "Pod" then " (phase \($o.status.phase // "unknown"))" else "" end)
      + (if $stamps then " stamps it from a volumeClaimTemplate" else " mounts it" end)
    ' "$WORK/consumers.json" > "$WORK/users" 2> "$WORK/users.err"; then
    sed 's/^/    /' "$WORK/users.err" >&2
    fail "the consumer list for $NS would not parse (above)"
fi
if [ -s "$WORK/users" ]; then
    sed 's/^/    /' "$WORK/users" >&2
    refuse "$NS/$CLAIM is in use (above) — a claim something mounts, or would stamp again, is not retired"
fi

# --- the plan document ----------------------------------------------------------
render() {
    echo "plan: retire-volume-claim"
    jq -r --arg ns "$NS" '
        "claim: \($ns)/\(.metadata.name)",
        "  uid: \(.metadata.uid)",
        "  created: \(.metadata.creationTimestamp // "unknown")",
        "  phase: \(.status.phase // "unknown")",
        "  storage class: \(.spec.storageClassName // "none")",
        "  access modes: \((.spec.accessModes // []) | if length == 0 then "none" else join(", ") end)",
        "  requested: \(.spec.resources.requests.storage // "none")",
        "  capacity: \(.status.capacity.storage // "none")",
        "  labels: \((.metadata.labels // {}) | if length == 0 then "none" else (to_entries | sort_by(.key) | from_entries | tojson) end)",
        "  owner references: none",
        "  finalizers: \((.metadata.finalizers // []) | if length == 0 then "none" else join(", ") end)"
        ' "$WORK/claim.json"
    if [ -n "$VOL" ]; then
        jq -r --arg src "$SOURCE" '
            "volume: \(.metadata.name)",
            "  uid: \(.metadata.uid)",
            "  phase: \(.status.phase // "unknown")",
            "  reclaim policy: \(.spec.persistentVolumeReclaimPolicy)",
            "  capacity: \(.spec.capacity.storage // "none")",
            "  storage class: \(.spec.storageClassName // "none")",
            "  claim ref: \(.spec.claimRef.namespace)/\(.spec.claimRef.name) uid \(.spec.claimRef.uid)",
            "  source: \($src)"
            ' "$WORK/pv.json"
    else
        echo "volume: none — the claim is bound to no PersistentVolume, so no bytes are held for it"
    fi
    if [ -n "$LH" ]; then
        jq -r --arg ns "$LONGHORN_NS" '
            "longhorn volume: \($ns)/\(.metadata.name)",
            "  size: \(.spec.size // "unknown") bytes",
            "  state: \(.status.state)"
            ' "$WORK/lh.json"
    fi
    echo "consumers: none — no Pod (any phase), Job, CronJob, Deployment, StatefulSet, DaemonSet or ReplicaSet in $NS mounts this claim, and no StatefulSet would stamp it again"
    echo "authority: infra/cluster/undeclared-objects.sh --check names PersistentVolumeClaim/$NS/$CLAIM undeclared (a namespace the tree owns, no manifest declares it, no controller owns it), and it is none of the protected claims (${PROTECTED[*]})"
    echo
    echo "== the delete this plan authorises =="
    echo "DELETE /api/v1/namespaces/$NS/persistentvolumeclaims/$CLAIM with preconditions.uid=$CUID"
    case "$POLICY" in
        Delete) echo "then: reclaim policy Delete — Kubernetes deletes PersistentVolume $VOL and Longhorn deletes volume $LH with its replicas. The bytes are gone; the capture keeps the objects, not the data." ;;
        Retain) echo "then: reclaim policy Retain — PersistentVolume $VOL and its bytes are KEPT (phase Released); this verb deletes no PersistentVolume." ;;
        *) echo "then: nothing else — no PersistentVolume holds bytes for this claim." ;;
    esac
}

render > "$WORK/plan"
HASH=$(sha256sum "$WORK/plan" | cut -d' ' -f1)

if [ "$PLAN" -eq 1 ]; then cat "$WORK/plan"; echo "plan-sha256: $HASH" >&2; exit 0; fi

# --- the write ------------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "the plan as it stands (above) hashes to $HASH, not the approved $APPROVED: a fact the signed plan recorded has moved, or the claim is already gone. Nothing was deleted; render and approve it again"
fi

BOSS_BIN="${BOSS_BIN:-}"
if [ -z "$BOSS_BIN" ]; then
    if command -v boss >/dev/null 2>&1; then BOSS_BIN="$(command -v boss)"; else BOSS_BIN=/usr/local/bin/boss; fi
fi
[ -x "$BOSS_BIN" ] || fail "no boss CLI at $BOSS_BIN to attach the capture with — nothing was deleted"

cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — capturing $NS/$CLAIM onto request $OPS_REQUEST_ID before the delete"

# THE CAPTURE, as the API server serves each object, immediately before
# the delete.
CAP="$WORK/retire-volume-claim-$NS-$CLAIM-${CUID:0:8}.yaml"
{
    echo "# retire-volume-claim capture for request $OPS_REQUEST_ID, plan $APPROVED"
    echo "# every object as the API server served it immediately before the delete"
    echo "--- PersistentVolumeClaim $NS/$CLAIM ---"
} > "$CAP"
"${KUBECTL[@]}" get PersistentVolumeClaim "$CLAIM" -n "$NS" -o yaml --request-timeout=30s >> "$CAP" 2> "$WORK/cap.err" \
    || { sed 's/^/    /' "$WORK/cap.err" >&2; fail "could not capture the claim $NS/$CLAIM (above) — nothing was deleted: the capture is the only copy there will be"; }
if [ -n "$VOL" ]; then
    echo "--- PersistentVolume $VOL (cluster-scoped) ---" >> "$CAP"
    "${KUBECTL[@]}" get PersistentVolume "$VOL" -o yaml --request-timeout=30s >> "$CAP" 2> "$WORK/cap.err" \
        || { sed 's/^/    /' "$WORK/cap.err" >&2; fail "could not capture PersistentVolume $VOL (above) — nothing was deleted"; }
fi
if [ -n "$LH" ]; then
    echo "--- volumes.longhorn.io $LONGHORN_NS/$LH ---" >> "$CAP"
    "${KUBECTL[@]}" get volumes.longhorn.io "$LH" -n "$LONGHORN_NS" -o yaml --request-timeout=30s >> "$CAP" 2> "$WORK/cap.err" \
        || { sed 's/^/    /' "$WORK/cap.err" >&2; fail "could not capture Longhorn volume $LONGHORN_NS/$LH (above) — nothing was deleted"; }
fi
CAP_N=$(wc -c < "$CAP" | tr -d ' ')
[ "$CAP_N" -le "$CAPTURE_MAX" ] || fail "the capture is $CAP_N bytes, over this verb's $CAPTURE_MAX — not the three objects it expects; nothing was deleted"
CAP_SHA=$(sha256sum "$CAP" | cut -d' ' -f1)
if ! "$BOSS_BIN" attach "$OPS_REQUEST_ID" "$CAP" > "$WORK/attach.out" 2> "$WORK/attach.err"; then
    sed 's/^/    /' "$WORK/attach.out" "$WORK/attach.err" >&2
    fail "the capture did not land on request $OPS_REQUEST_ID (boss attach's words above) — nothing was deleted"
fi
FILE_ID=$(sed -n 's/.* as file \([0-9a-f-]\{36\}\),.*/\1/p' "$WORK/attach.out")
if [ "$(printf '%s\n' "$FILE_ID" | grep -c .)" -ne 1 ] || ! grep -qF "$CAP_SHA" "$WORK/attach.out" \
        || ! grep -qF 'byte-identical' "$WORK/attach.out"; then
    sed 's/^/    /' "$WORK/attach.out" >&2
    fail "boss attach exited 0 but its receipt (above) does not name one file id, the capture's sha256 $CAP_SHA and a byte-identical read-back — nothing was deleted"
fi
echo "--- the capture, as attached ---"
cat "$CAP"
echo "--- end of the capture ---"
echo "$ME: captured $NS/$CLAIM ($CAP_N bytes, sha256 $CAP_SHA) onto request $OPS_REQUEST_ID as file $FILE_ID, read back byte-identical"

# The delete reads its DeleteOptions body from stdin, and the docker form
# of the resolved kubectl runs without -i, which would drop the
# precondition SILENTLY (reap-terminated-pods.sh says the same).
if [ "${KUBECTL[0]}" = docker ] && [ "${KUBECTL[1]:-}" = run ]; then
    KDEL=(docker run -i "${KUBECTL[@]:2}")
else
    KDEL=("${KUBECTL[@]}")
fi
jq -n --arg uid "$CUID" '{kind: "DeleteOptions", apiVersion: "v1", preconditions: {uid: $uid}}' > "$WORK/body.json"
echo "$ME: deleting $NS/$CLAIM with preconditions.uid=$CUID"
if ! "${KDEL[@]}" delete --raw "/api/v1/namespaces/$NS/persistentvolumeclaims/$CLAIM" -f - --request-timeout=60s \
        < "$WORK/body.json" > "$WORK/del.out" 2> "$WORK/del.err"; then
    sed 's/^/    /' "$WORK/del.err" >&2
    fail "the delete of $NS/$CLAIM (uid $CUID) was refused or failed (above). A precondition refusal means a different claim now holds the name and it was left alone; any other failure leaves the outcome unread — read it with plan-a-volume-claim-retirement"
fi

# --- the read-back ----------------------------------------------------------------
# gone_or_wait <resource> <name> <uid> [-n ns] — wait up to WAIT_S for the
# object of that uid to be gone. Sets GONE to how it was read back; 0
# gone, 1 still there after the wait (STILL names how), 2 unreadable.
#
# Gone is only ever one of two READS: the server's NotFound, or a
# document naming a DIFFERENT non-empty uid. A get that exits 0 with no
# document, or with one carrying no uid, read nothing — it is
# unreadable, never gone (backlog 37338a58: jq-1.6 over an empty file
# exits 0 with no output, so an empty answer used to read as "a
# different object" and the effect line printed a NotFound no read had
# produced). GONE carries the read, and the effect line quotes it.
gone_or_wait() {
    local res="$1" name="$2" uid="$3"; shift 3
    local deadline=$((SECONDS + WAIT_S)) rc now
    if [ -z "$uid" ]; then
        STILL="unreadable: no uid was recorded for $res $name before the delete, so no read-back can tell it from another"
        return 2
    fi
    while :; do
        read_json "$WORK/back.json" "$res" "$name" "$@"; rc=$?
        if [ "$rc" -eq 2 ]; then GONE="NotFound"; return 0; fi
        if [ "$rc" -eq 0 ]; then
            if ! jq_doc_file "$WORK/back.json"; then
                rc=1
                echo "the get exited 0 with no JSON document" > "$WORK/back.json.err"
            else
                now=$(jq -r '.metadata.uid // ""' "$WORK/back.json")
                if [ -z "$now" ]; then
                    rc=1
                    echo "the get answered a document with no uid: $(head -c 200 "$WORK/back.json" | tr '\n' ' ')" > "$WORK/back.json.err"
                elif [ "$now" != "$uid" ]; then
                    GONE="its name is now held by uid $now, a different object"; return 0
                else
                    STILL=$(jq -r 'if .metadata.deletionTimestamp then "being deleted since \(.metadata.deletionTimestamp), held by finalizers \((.metadata.finalizers // []) | join(","))" else "present, phase \(.status.phase // "unknown")" end' "$WORK/back.json")
                fi
            fi
        fi
        [ "$SECONDS" -lt "$deadline" ] || break
        sleep 2
    done
    if [ "$rc" -ne 0 ]; then STILL="unreadable: $(tr '\n' ' ' < "$WORK/back.json.err")"; return 2; fi
    return 1
}

GONE=""; STILL=""
gone_or_wait PersistentVolumeClaim "$CLAIM" "$CUID" -n "$NS"
case $? in
    0) CLAIM_BACK="$GONE" ;;
    1) fail "the delete of $NS/$CLAIM was accepted and after ${WAIT_S}s uid $CUID is still there, $STILL — something mounted it after the plan was rendered, and Kubernetes holds it until that consumer goes. Nothing more was done; read it with plan-a-volume-claim-retirement" ;;
    *) fail "deleted, not proven gone: the delete of $NS/$CLAIM was accepted and the read-back is $STILL" ;;
esac
CLAIM_LINE="PersistentVolumeClaim $NS/$CLAIM uid $CUID is gone (read back: $CLAIM_BACK)"

case "$POLICY" in
    Delete)
        PV_UID=$(jq -r '.metadata.uid // ""' "$WORK/pv.json")
        gone_or_wait PersistentVolume "$VOL" "$PV_UID"
        case $? in
            0) PV_BACK="$GONE" ;;
            1) fail "$CLAIM_LINE, and PersistentVolume $VOL (reclaim policy Delete) is still there after ${WAIT_S}s, $STILL — its bytes are not proven released" ;;
            *) fail "$CLAIM_LINE, and PersistentVolume $VOL cannot be read back: $STILL" ;;
        esac
        LH_UID=$(jq -r '.metadata.uid // ""' "$WORK/lh.json")
        gone_or_wait volumes.longhorn.io "$LH" "$LH_UID" -n "$LONGHORN_NS"
        case $? in
            0) LH_BACK="$GONE" ;;
            1) fail "$CLAIM_LINE and PersistentVolume $VOL is gone, but Longhorn volume $LH is still there after ${WAIT_S}s, $STILL — its replicas are not proven released" ;;
            *) fail "$CLAIM_LINE and PersistentVolume $VOL is gone, but Longhorn volume $LH cannot be read back: $STILL" ;;
        esac
        echo "$ME: $CLAIM_LINE; PersistentVolume $VOL (reclaim policy Delete) is gone (read back: $PV_BACK); Longhorn volume $LH is gone (read back: $LH_BACK)"
        ;;
    Retain)
        # The volume controller moves a retained volume to Released on
        # its own clock; read until it leaves Bound, within the same bound.
        deadline=$((SECONDS + WAIT_S))
        while :; do
            read_json "$WORK/kept.json" PersistentVolume "$VOL" \
                || fail "$CLAIM_LINE, and PersistentVolume $VOL (reclaim policy Retain) cannot be read back: $(tr '\n' ' ' < "$WORK/kept.json.err")"
            [ "$(jq -r '.status.phase // ""' "$WORK/kept.json")" != Bound ] && break
            [ "$SECONDS" -lt "$deadline" ] || break
            sleep 2
        done
        echo "$ME: $CLAIM_LINE; PersistentVolume $VOL (reclaim policy Retain) is kept, phase $(jq -r '.status.phase // "unknown"' "$WORK/kept.json") — its bytes stand; this verb deletes no PersistentVolume"
        ;;
    *)
        echo "$ME: $CLAIM_LINE; it was bound to no PersistentVolume"
        ;;
esac
