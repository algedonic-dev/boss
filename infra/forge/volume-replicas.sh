#!/usr/bin/env bash
# volume-replicas.sh — raise ONE Longhorn volume's replica count, through
# a rendered plan a passkey signs. The ops verbs
# `plan-a-volume-replica-change` (--plan) and `set-volume-replicas`.
#
#   volume-replicas.sh --plan <volume> <replicas>
#   volume-replicas.sh <volume> <replicas> <plan-sha256>
#
# WHY IT EXISTS (backlog f5f182fc; backlog 52ea56ac, review 60b87bff N2).
# The dev pod's /work volume (PVC boss-dev/boss-dev-work, Longhorn volume
# pvc-dd7b5ac3-…, 40 GiB) was made by the StorageClass
# longhorn-dev-disposable with numberOfReplicas "1", and its one replica
# lives on w-1. w-1 drains on 2026-09-30 for its second NVMe, and under
# Longhorn's default node-drain-policy (block-if-contains-last-replica)
# that last replica holds the drain — the dev session is what gets
# evicted. A StorageClass parameter applies only when a volume is made;
# the live knob is volumes.longhorn.io spec.numberOfReplicas. The dev
# session cannot read or write longhorn.io (Forbidden), and editing
# boss-dev.yaml restarts the session, so the change is a forge verb over
# the ops_kubectl door (the admin kubeconfig David placed,
# infra/estate/ops-credentials.sh). David asked for it 2026-09-30 ~02:50Z.
#
# THE SHAPE is design 17835005's, as reclaim-gcp-root's and
# shutdown-node's: the verb file declares requires_approval and names
# plan-a-volume-replica-change as its plan_verb, so the runner renders
# this script's --plan onto the request's approve step, and hands the
# write sha256 of the SIGNED plan. The write re-renders and patches
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * the volume is one name of the shape Longhorn gives a provisioned
#     PV, pvc-<uuid> in lower case, in namespace longhorn-system — a name
#     Longhorn does not hold is a refusal;
#   * the count is 1, 2 or 3 AND greater than spec.numberOfReplicas now.
#     This verb ONLY RAISES, and never deletes a replica. A lowered count
#     is not a door it opens: Longhorn v1.11.3 trims the replicas a
#     lowered count leaves over ONLY when an eviction is requested,
#     best-effort data locality has a local replica, or replica
#     auto-balance is on (controller/volume_controller.go
#     cleanupExtraHealthyReplicas; review 091904d3) — under the defaults
#     they stand, and which one goes otherwise is Longhorn's choice, not
#     the plan's. (This line said Longhorn always deletes them until
#     backlog ab39a34e.) Moving a replica is move-volume-replica's;
#   * at least one replica is healthy — there is something to rebuild
#     from;
#   * enough Longhorn nodes can take a new replica to put every replica on
#     a DISTINCT node. A node can when it allows scheduling, is not being
#     evicted, reads Ready and Schedulable (Longhorn marks a cordoned node
#     unschedulable), holds no live replica of this volume, and has a disk
#     that allows scheduling with room for the volume's size. Room is
#     Longhorn's scheduling ledger — storageMaximum − storageReserved −
#     storageScheduled — read at 100% over-provisioning, so it can only
#     under-count what Longhorn would place: a refusal here is never a
#     change Longhorn would have made.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`, since a hash
# cannot be inside what it hashes) names the volume, its claim, size,
# state, robustness, the count before and after, the one patch, every
# replica of the volume with its node, disk and health, and every
# Longhorn node's verdict with its reason. No clock and no LIVE free
# space in the bytes: storageAvailable moves every second on a busy disk,
# and a plan that signed it could never be run — it rides stderr. The
# room figure is in whole GiB. The count before is IN the bytes, so a
# second run of an applied plan re-renders to a refusal: at most once.
#
# THE WRITE is one `kubectl patch volumes.longhorn.io --type=json` that
# TESTS spec.numberOfReplicas is still the planned value and replaces it —
# a compare-and-set, so a change made between the render and the patch
# fails the patch rather than being overwritten. Nothing else is written.
#
# THE EFFECT IS READ BACK. Every BOSS_REPLICAS_POLL_S (default 15) up to
# BOSS_REPLICAS_WAIT_S (default 1500, inside the verb's 1800 s timeout so
# the verdict is this script's, not the runner's kill): the volume's
# spec.numberOfReplicas and robustness, and its replicas. Only when the
# spec reads the new count AND that many replicas report healthy
# (spec.healthyAt set, spec.failedAt empty — Longhorn's own test, the one
# node-status reads) on that many DISTINCT nodes is the line the verb file
# declares as `effect` printed. Two replicas on one node survive nothing a
# drain does. A rebuild that outlasts the wait exits 1 saying the volume
# is NOT proven, naming the healthy nodes, the replicas not yet healthy
# and the failed ones; the patch stands and Longhorn goes on rebuilding.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan and never an empty cluster. Exit 78 is a
# refusal (the request was wrong, or is no longer true).
set -uo pipefail

ME=set-volume-replicas
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
VOL_RE='^pvc-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread volume has no plan, and nothing was written"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || refuse "usage: volume-replicas.sh --plan <volume> <replicas> — the plan takes the volume and the count and nothing else"
else
    [ $# -eq 3 ] || refuse "usage: volume-replicas.sh <volume> <replicas> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-volume-replica-change verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
VOL="$1"
WANT="$2"
[[ "$VOL" =~ $VOL_RE ]] || refuse "the volume must be one Longhorn volume name, pvc-<uuid> in lower case, got '$VOL'"
[[ "$WANT" =~ ^[1-3]$ ]] || refuse "the replica count must be 1, 2 or 3, got '$WANT'"
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not a plan"

WAIT_S="${BOSS_REPLICAS_WAIT_S:-1500}"
POLL_S="${BOSS_REPLICAS_POLL_S:-15}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_REPLICAS_WAIT_S and BOSS_REPLICAS_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# read_volume — the volume into $WORK/vol.json. A volume Longhorn does
# not hold is a refusal; any other failure cannot answer.
read_volume() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get volumes.longhorn.io "$VOL" -n "$LH" -o json --request-timeout=20s \
            > "$WORK/vol.json" 2> "$WORK/vol.err"; then
        if grep -q NotFound "$WORK/vol.err"; then
            refuse "Longhorn holds no volume $VOL in $LH ($(tr '\n' ' ' < "$WORK/vol.err"))"
        fi
        unread "volumes.longhorn.io $VOL: $(tr '\n' ' ' < "$WORK/vol.err")"
    fi
    [ "$(jq -r '.metadata.name // ""' "$WORK/vol.json" 2>/dev/null)" = "$VOL" ] \
        || unread "volumes.longhorn.io $VOL did not read back as that volume: $(head -c 200 "$WORK/vol.json" | tr '\n' ' ')"
}

# read_list <resource> <file> — a Longhorn list into $WORK/<file>.
read_list() {
    # shellcheck disable=SC2086
    if ! $K get "$1" -n "$LH" -o json --request-timeout=30s > "$WORK/$2" 2> "$WORK/$2.err"; then
        unread "$1: $(tr '\n' ' ' < "$WORK/$2.err")"
    fi
    jq_doc_file "$WORK/$2" && jq -e '.items | type == "array"' "$WORK/$2" >/dev/null 2>&1 \
        || unread "$1 did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/$2" | tr '\n' ' ')')"
}

# facts — one JSON object judged from the three reads: the volume's own
# fields, its replicas, and every Longhorn node's verdict.
facts() {
    jq -n --arg v "$VOL" \
        --slurpfile vol "$WORK/vol.json" \
        --slurpfile reps "$WORK/replicas.json" \
        --slurpfile nodes "$WORK/lhnodes.json" '
        # v1beta2 serves conditions as a list; v1beta1 served a map.
        def conds: (. // []) | if type == "object" then [.[]] else . end;
        def cond($t): [conds[] | select(.type == $t) | .status] | .[0] // "unreported";
        def healthy: (.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "";
        def failed: (.spec.failedAt // "") != "";
        def gib: (. / 1073741824 | floor);
        $vol[0] as $V
        | (($V.spec.size // "") | tostring | tonumber? // null) as $size
        | [$reps[0].items[] | select(.spec.volumeName == $v)] | sort_by(.metadata.name) as $mine
        | ([$mine[] | select(healthy) | .spec.nodeID] | unique) as $hn
        | ([$mine[] | select(failed | not) | .spec.nodeID // empty] | unique) as $occ
        | [$nodes[0].items[] | . as $N | .metadata.name as $n
            | ([(if .spec.allowScheduling != true then "spec.allowScheduling is false" else empty end),
                (if .spec.evictionRequested == true then "an eviction is requested" else empty end),
                ((.status.conditions | cond("Ready")) as $r | if $r != "True" then "Ready is \($r)" else empty end),
                ((.status.conditions | cond("Schedulable")) as $s | if $s != "True" then "Schedulable is \($s)" else empty end)]) as $why
            | [(.spec.disks // {}) | to_entries[] | .key as $d | .value as $ds
                | ($N.status.diskStatus // {})[$d] as $st
                | (if $st == null then null
                   else (($st.storageMaximum // 0) - ($ds.storageReserved // 0) - ($st.storageScheduled // 0)) end) as $room
                | {disk: $d, room: $room, available: ($st.storageAvailable // null),
                   why: [(if $ds.allowScheduling != true then "allowScheduling is false" else empty end),
                         (if $ds.evictionRequested == true then "an eviction is requested" else empty end),
                         (if $st == null then "no diskStatus reported"
                          else (($st.conditions | cond("Schedulable")) as $s | if $s != "True" then "Schedulable is \($s)" else empty end) end),
                         (if $room != null and $size != null and $room < $size
                          then "room \($room | gib) GiB is short of \($size | gib) GiB" else empty end)]}] as $disks
            | ([$disks[] | select(.why == [])] | .[0] // null) as $fit
            | {name: $n,
               verdict: (if ($occ | index($n)) != null then "holds"
                         elif $why != [] then "cannot"
                         elif $fit != null then "can"
                         else "cannot" end),
               line: (if ($occ | index($n)) != null then "node \($n): holds a replica of this volume"
                      elif $why != [] then "node \($n): cannot take a replica — \($why | join("; "))"
                      elif $fit != null then "node \($n): can take a replica — disk \($fit.disk): room \($fit.room | gib) GiB (maximum − reserved − scheduled) for a \($size | gib) GiB replica"
                      elif $disks == [] then "node \($n): cannot take a replica — it reports no disk"
                      else "node \($n): cannot take a replica — \([$disks[] | "disk \(.disk): \(.why | join(", "))"] | join("; "))" end),
               free: [$disks[] | "node \($n) disk \(.disk): storageAvailable \(if .available == null then "unreported" else "\(.available | gib) GiB" end)"]}]
            | sort_by(.name) as $verdicts
        | {size: $size,
           current: ($V.spec.numberOfReplicas // null),
           claim: (($V.status.kubernetesStatus // {}) as $k
                   | if ($k.pvcName // "") != "" then "\($k.namespace)/\($k.pvcName)" else "none" end),
           state: (($V.status.state // "unreported") + (if ($V.status.currentNodeID // "") != "" then " on \($V.status.currentNodeID)" else "" end)),
           robustness: ($V.status.robustness // "unreported"),
           locality: ($V.spec.dataLocality // "unreported"),
           healthy_nodes: $hn,
           occupied: $occ,
           replicas: [$mine[] | "replica \(.metadata.name) on \(.spec.nodeID // "no node") (disk \(.spec.diskID // "none")): \(if failed then "failed" elif healthy then "healthy" else "not yet healthy (rebuilding, or never finished)" end)"],
           candidates: [$verdicts[] | select(.verdict == "can") | .name],
           nodes: [$verdicts[] | .line],
           free: [$verdicts[] | .free[]]}' 2> "$WORK/facts.err"
}

# render — read Longhorn now, apply the bounds, and write the plan to
# $WORK/plan. Sets CUR (the count before). Nothing in the bytes is a
# clock or live free space.
render() {
    read_volume
    read_list replicas.longhorn.io replicas.json
    read_list nodes.longhorn.io lhnodes.json
    if ! facts > "$WORK/facts.json"; then
        unread "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/facts.err")"
    fi
    local f="$WORK/facts.json" size healthy nhealthy nocc need ncand
    size="$(jq -r '.size // "none"' "$f")"
    CUR="$(jq -r '.current // "none"' "$f")"
    case "$size" in '' | *[!0-9]*) unread "volume $VOL reports no size (spec.size '$size')" ;; esac
    case "$CUR" in '' | *[!0-9]*) unread "volume $VOL reports no spec.numberOfReplicas ('$CUR')" ;; esac
    healthy="$(jq -r '.healthy_nodes | join(", ")' "$f")"
    nhealthy="$(jq '.healthy_nodes | length' "$f")"
    if [ "$WANT" -le "$CUR" ]; then
        refuse "spec.numberOfReplicas of $VOL is already $CUR, and $WANT would not raise it — this verb only raises and never deletes a replica: what a lowered count leaves over is Longhorn's to trim or keep, by settings this plan does not read (move-volume-replica moves one by name). Replicas now: healthy on ${healthy:-no node}; $(jq -r '.replicas | join("; ")' "$f")"
    fi
    [ "$nhealthy" -gt 0 ] \
        || refuse "$VOL has no healthy replica (spec.healthyAt set, spec.failedAt empty), so there is nothing to rebuild a new one from: $(jq -r '.replicas | join("; ")' "$f")"
    nocc="$(jq '.occupied | length' "$f")"
    ncand="$(jq '.candidates | length' "$f")"
    need=$((WANT - nocc))
    if [ "$need" -gt "$ncand" ]; then
        refuse "$WANT replicas of $VOL on distinct nodes need $need more node(s) that can take one, and $ncand can: $(jq -r '.nodes | join("; ")' "$f")"
    fi
    {
        echo "plan: set-volume-replicas"
        echo "volume: $VOL"
        echo "claim: $(jq -r '.claim' "$f")"
        echo "size: $size bytes ($(jq -r '.size | if . % 1073741824 == 0 then "\(. / 1073741824) GiB" else "~\(. / 1073741824 | floor) GiB" end' "$f"))"
        echo "state: $(jq -r '.state' "$f")"
        echo "robustness: $(jq -r '.robustness' "$f")"
        echo "data locality: $(jq -r '.locality' "$f")"
        echo "numberOfReplicas: $CUR -> $WANT"
        echo "command: kubectl patch volumes.longhorn.io $VOL -n $LH --type=json — test that spec.numberOfReplicas is still $CUR, replace it with $WANT; the one field this writes. Longhorn schedules the new replica(s) and rebuilds them from a healthy one; nothing is deleted."
        echo "read-back: until spec.numberOfReplicas reads $WANT and $WANT replicas report healthy (spec.healthyAt set, spec.failedAt empty) on $WANT distinct nodes."
        echo
        echo "== replicas of $VOL ($(jq '.replicas | length' "$f")) =="
        jq -r '.replicas[]' "$f"
        echo
        echo "== longhorn nodes ($(jq '.nodes | length' "$f")): which can take a new replica =="
        jq -r '.nodes[]' "$f"
    } > "$WORK/plan"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
jq -r --arg me "$ME" '.free[] | "\($me): live free space, not signed (Longhorn checks it when it schedules): \(.)"' "$WORK/facts.json" >&2

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $VOL (above) hashes to $HASH, not the approved plan $APPROVED — its count, its replicas or the nodes that can take one moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — setting spec.numberOfReplicas of $VOL from $CUR to $WANT"

PATCH="[{\"op\":\"test\",\"path\":\"/spec/numberOfReplicas\",\"value\":$CUR},{\"op\":\"replace\",\"path\":\"/spec/numberOfReplicas\",\"value\":$WANT}]"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch volumes.longhorn.io "$VOL" -n "$LH" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch volumes.longhorn.io $VOL did not succeed (its words above) — this run cannot say the count changed; read the volume again with plan-a-volume-replica-change before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted spec.numberOfReplicas=$WANT for $VOL — reading it back (up to ${WAIT_S}s: the spec, and $WANT healthy replicas on $WANT distinct nodes)"

# --- the read-back ------------------------------------------------------------
# replicas_of <jq filter on one replica> — the matching replicas of $VOL
# in $WORK/reps.json, each `<name> on <node>`, sorted, or `none`.
replicas_of() {
    jq -r --arg v "$VOL" "[.items[] | select(.spec.volumeName == \$v) | select($1) | \"\\(.metadata.name) on \\(.spec.nodeID // \"no node yet\")\"] | sort | if . == [] then \"none\" else join(\", \") end" "$WORK/reps.json"
}
HEALTHY='(.spec.healthyAt // "") != "" and (.spec.failedAt // "") == ""'
start="$(date -u +%s)"
spec="not read yet"
rob="not read yet"
healthy=""
nhealthy=0
pending="not read yet"
failed="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $K get volumes.longhorn.io "$VOL" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        spec="$(jq -r '.spec.numberOfReplicas // "unreported"' "$WORK/back.json")"
        rob="$(jq -r '.status.robustness // "unreported"' "$WORK/back.json")"
    else
        spec="unread: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
        rob="unread"
    fi
    # shellcheck disable=SC2086
    if $K get replicas.longhorn.io -n "$LH" -o json --request-timeout=30s > "$WORK/reps.json" 2> "$WORK/reps.err" \
        && jq_doc_file "$WORK/reps.json" && jq -e '.items | type == "array"' "$WORK/reps.json" > /dev/null 2>&1; then
        healthy="$(jq -r --arg v "$VOL" "[.items[] | select(.spec.volumeName == \$v) | select($HEALTHY) | .spec.nodeID] | unique | join(\", \")" "$WORK/reps.json")"
        nhealthy="$(jq --arg v "$VOL" "[.items[] | select(.spec.volumeName == \$v) | select($HEALTHY) | .spec.nodeID] | unique | length" "$WORK/reps.json")"
        pending="$(replicas_of '(.spec.healthyAt // "") == "" and (.spec.failedAt // "") == ""')"
        failed="$(replicas_of '(.spec.failedAt // "") != ""')"
    else
        healthy=""
        nhealthy=0
        pending="unread: $(tr '\n' ' ' < "$WORK/reps.err" | cut -c1-200)"
        failed="unread"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$spec" = "$WANT" ] && [ "$nhealthy" -ge "$WANT" ]; then
        echo "$ME: $VOL has $nhealthy healthy replicas on $nhealthy distinct nodes ($healthy) — read back: spec.numberOfReplicas=$spec, robustness $rob (after ${elapsed}s)"
        exit 0
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        fail "Longhorn accepted spec.numberOfReplicas=$WANT for $VOL, and after ${elapsed}s it is NOT proven: spec.numberOfReplicas=$spec, robustness $rob, healthy on $nhealthy distinct node(s) (${healthy:-none}); not yet healthy: $pending; failed: $failed. The patch stands and Longhorn goes on rebuilding — read it again with plan-a-volume-replica-change, whose refusal names the replicas"
    fi
    sleep "$POLL_S"
done
