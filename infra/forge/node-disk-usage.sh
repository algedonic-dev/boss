#!/usr/bin/env bash
# node-disk-usage.sh — READ-ONLY: what occupies one cluster node's disk,
# named. The ops verb `node-disk-usage`.
#
#   node-disk-usage.sh <node>
#
# WHY IT EXISTS (backlog ebcddea5, 2026-10-09). The estate alarm
# disk_tight:w-2 fired for the second time in three days: w-2's /var fell
# from 73 GiB free to 31 GiB in 59 hours, against a 39 GiB floor. The
# measure step (agent-run 34723794) could read THAT the disk was filling —
# the talos-nodefs series, Longhorn's disk sums — and not WHAT filled it,
# because no door named what occupies a node's disk: node-status lists
# only volumes whose ONLY replica is on the node, plan-a-node-shutdown
# refuses an uncordoned node, the replica plan verbs need a volume name
# already known, and disk-report serves the forge and boss-gcp, not
# cluster nodes. Both alarms on w-2 were answered by moving a replica
# without knowing the grower. This verb is the read that names it, so a
# remedy can be chosen from a measurement.
#
# WHAT IT PRINTS (stdout, sorted, no clock — two reads of one state are
# byte-identical):
#   * the node: its estate role and address;
#   * the kubelet's own filesystem figures, from the node's /stats/summary:
#     nodefs (the kubelet's /var), imagefs, and containerfs when the
#     runtime reports a separate one;
#   * every pod on the node by ephemeral storage, largest first, split into
#     container writable layers, logs and local volumes (a volume with a
#     claim is not ephemeral storage and is not counted);
#   * the node's largest images, as the Node object lists them;
#   * every Longhorn replica whose spec.nodeID is the node, largest actual
#     size first, with its volume, claim, the volume's size and actual
#     size (status.actualSize, Longhorn's figure for one replica's data,
#     snapshots included), disk and health — and the node's Longhorn disks
#     with their maximum, reserved, scheduled and available bytes.
#
# THE BOUND. The node is one DNS label the estate registry declares, not
# retired, with a talos-* role — a control plane may be READ, as
# node-status reads one. Every call is a `kubectl get` through the forge's
# ops_kubectl door (the admin kubeconfig the forge already holds); nothing
# is written. The summary is `get --raw` of the node's proxy path, a GET:
# no grant is added to any in-cluster token (the observer's nodes/proxy
# grant was removed by backlog eeac3d56 and stays removed).
#
# NO EVIDENCE IS NOT A PASS. A section whose read could not look prints
# CANNOT ANSWER with kubectl's words, the other sections still print, and
# the run exits 1 — a summary or Longhorn read that failed must never
# render as a node with nothing on it. Exit 78 is a refusal (the request
# was wrong); exit 0 means every section was read.
set -uo pipefail

ME=node-disk-usage
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

[ $# -eq 1 ] || node_refuse "usage: node-disk-usage.sh <node> — the node and nothing else"
NODE="$1"
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

node_read_registry "$NODE"
case "$NODE_ROLE" in
    talos-*) ;;
    *) node_refuse "the estate registry says $NODE is a ${NODE_ROLE:-node with no role}, not a Talos node — it is not a node of the cluster" ;;
esac
node_door
node_get "$NODE"

UNREAD=""
unread() {
    echo "CANNOT ANSWER — $*"
    UNREAD="${UNREAD:+$UNREAD; }$*"
}
LH=longhorn-system
# GiB to one decimal, as a string; a figure the source did not report says so.
GIB='def gib: if . == null then "unreported" else ((. * 10 / 1073741824 + 0.5) | floor) as $t | "\($t / 10 | floor).\($t % 10)" end;'

echo "node-disk-usage: $NODE — READ-ONLY; nothing is written"
echo "node: $NODE"
echo "estate role: $NODE_ROLE"
echo "address: $NODE_ADDRESS"

# --- the kubelet's figures ------------------------------------------------------
echo
fs_line="nodefs unread"
eph_line="pods unread"
SUMMARY="/api/v1/nodes/$NODE/proxy/stats/summary"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K get --raw "$SUMMARY" --request-timeout=30s > "$WORK/summary.json" 2> "$WORK/summary.err"; then
    echo "== filesystems (the kubelet's /stats/summary) (unread) =="
    unread "$SUMMARY: $(tr '\n' ' ' < "$WORK/summary.err")"
elif ! jq_doc_file "$WORK/summary.json" \
        || ! jq -e --arg n "$NODE" '.node.nodeName == $n and (.pods | type == "array")' "$WORK/summary.json" >/dev/null 2>&1; then
    echo "== filesystems (the kubelet's /stats/summary) (unread) =="
    unread "$SUMMARY did not read back as $NODE's summary (its first 200 bytes: '$(head -c 200 "$WORK/summary.json" | tr '\n' ' ')')"
else
    echo "== filesystems (the kubelet's /stats/summary) =="
    jq -r "$GIB"'
        def fs($name): if . == null then empty
            else "\($name): capacity \(.capacityBytes | gib) GiB, used \(.usedBytes | gib) GiB, available \(.availableBytes | gib) GiB" end;
        (.node.fs | fs("nodefs")), (.node.runtime.imageFs | fs("imagefs")), (.node.runtime.containerFs | fs("containerfs"))' \
        "$WORK/summary.json"
    fs_line="$(jq -r "$GIB"'.node.fs as $f | .node.runtime.imageFs as $i
        | "nodefs used \($f.usedBytes | gib) of \($f.capacityBytes | gib) GiB (\($f.availableBytes | gib) available); imagefs used \($i.usedBytes | gib) GiB"' \
        "$WORK/summary.json")"

    echo
    # Ephemeral storage is the kubelet's own sum; the split beside it is
    # read from the same answer — a volume with a pvcRef is a claim, not
    # local storage.
    jq -r --arg n "$NODE" "$GIB"'
        [.pods[]
         | { ref: "\(.podRef.namespace)/\(.podRef.name)",
             eph: (.["ephemeral-storage"].usedBytes // null),
             rootfs: ([.containers[]?.rootfs.usedBytes // 0] | add // 0),
             logs: ([.containers[]?.logs.usedBytes // 0] | add // 0),
             local: ([.volume[]? | select(.pvcRef == null) | .usedBytes // 0] | add // 0) }]
        | sort_by([-(.eph // 0), .ref])
        | "== pods on \($n) by ephemeral storage (\(length)) ==",
          (if length == 0 then "none" else .[]
           | "pod \(.ref): ephemeral \(.eph | gib) GiB (container rootfs \(.rootfs | gib) GiB, logs \(.logs | gib) GiB, local volumes \(.local | gib) GiB)" end)' \
        "$WORK/summary.json"
    eph_line="$(jq -r "$GIB"'[.pods[] | .["ephemeral-storage"].usedBytes // 0] | "\(length) pod(s) use \(add // 0 | gib) GiB ephemeral"' "$WORK/summary.json")"
fi

# --- images -------------------------------------------------------------------
echo
# The Node lists its images largest first (the kubelet caps the list, 50
# by default); the ten largest are named, by a tag where one is listed.
jq -r --arg n "$NODE" "$GIB"'
    [(.status.images // [])[]
     | { name: (([.names[]? | select(contains("@") | not)] | .[0]) // (.names[0]? // "unnamed")),
         size: (.sizeBytes // 0) }]
    | sort_by([-.size, .name]) as $all
    | "== images on \($n) by size (the \([$all | length, 10] | min) largest of \($all | length) the Node lists) ==",
      (if $all == [] then "none" else $all[:10][] | "image \(.name): \(.size | gib) GiB" end)' \
    "$WORK/node.json"

# --- Longhorn -----------------------------------------------------------------
echo
rep_line="longhorn replicas unread"
# shellcheck disable=SC2086
if ! $K get replicas.longhorn.io -n "$LH" -o json --request-timeout=30s > "$WORK/replicas.json" 2> "$WORK/replicas.err"; then
    echo "== longhorn replicas on $NODE (unread) =="
    unread "replicas.longhorn.io: $(tr '\n' ' ' < "$WORK/replicas.err")"
# shellcheck disable=SC2086
elif ! $K get volumes.longhorn.io -n "$LH" -o json --request-timeout=30s > "$WORK/volumes.json" 2> "$WORK/volumes.err"; then
    echo "== longhorn replicas on $NODE (unread) =="
    unread "volumes.longhorn.io: $(tr '\n' ' ' < "$WORK/volumes.err")"
elif ! { jq_doc_file "$WORK/replicas.json" && jq_doc_file "$WORK/volumes.json" \
        && jq -e '.items | type == "array"' "$WORK/replicas.json" >/dev/null 2>&1 \
        && jq -e '.items | type == "array"' "$WORK/volumes.json" >/dev/null 2>&1; }; then
    echo "== longhorn replicas on $NODE (unread) =="
    unread "replicas.longhorn.io or volumes.longhorn.io did not read back as a list"
else
    jq -r --arg n "$NODE" --slurpfile vols "$WORK/volumes.json" "$GIB"'
        def num: if . == null then null else (tostring | tonumber? // null) end;
        [.items[] | select(.spec.nodeID == $n)
         | .spec.volumeName as $v
         | ([$vols[0].items[] | select(.metadata.name == $v)] | .[0]) as $vo
         | ($vo.status.kubernetesStatus // {}) as $ks
         | { name: .metadata.name, vol: $v, found: ($vo != null),
             claim: (if ($ks.pvcName // "") != "" then "pvc \($ks.namespace)/\($ks.pvcName)" else "no claim" end),
             size: ($vo.spec.size | num), actual: ($vo.status.actualSize | num),
             disk: (.spec.diskID // "none"),
             health: (if (.spec.failedAt // "") != "" then "failed"
                      elif (.spec.healthyAt // "") != "" then "healthy"
                      else "not yet healthy (rebuilding, or never finished)" end) }]
        | sort_by([-(.actual // 0), .name])
        | "== longhorn replicas on \($n) (\(length)) ==",
          (if length == 0 then "none" else .[]
           | if .found then "replica \(.name): volume \(.vol) (\(.claim)), size \(.size | gib) GiB, actual \(.actual | gib) GiB, disk \(.disk), \(.health)"
             else "replica \(.name): volume \(.vol) is not in volumes.longhorn.io, disk \(.disk), \(.health)" end end)' \
        "$WORK/replicas.json"
    rep_line="$(jq -r --arg n "$NODE" --slurpfile vols "$WORK/volumes.json" "$GIB"'
        [.items[] | select(.spec.nodeID == $n) | .spec.volumeName as $v
         | [$vols[0].items[] | select(.metadata.name == $v) | (.status.actualSize // 0 | tostring | tonumber? // 0)] | .[0] // 0]
        | "\(length) longhorn replica(s) hold \(add // 0 | gib) GiB actual"' "$WORK/replicas.json")"
fi

echo
echo "== longhorn disks on $NODE =="
# shellcheck disable=SC2086
if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/lhnode.json" 2> "$WORK/lhnode.err"; then
    if jq_doc_file "$WORK/lhnode.json" && jq -e --arg n "$NODE" '.metadata.name == $n' "$WORK/lhnode.json" >/dev/null 2>&1; then
        jq -r "$GIB"'
            . as $N | [(.spec.disks // {}) | to_entries[] | .key as $d | .value as $ds
             | ($N.status.diskStatus // {})[$d] as $st
             | "longhorn disk \($d) (\($ds.path // "no path")): maximum \($st.storageMaximum | gib) GiB, reserved \($ds.storageReserved | gib) GiB, scheduled \($st.storageScheduled | gib) GiB, available \($st.storageAvailable | gib) GiB"]
            | if . == [] then "longhorn node: declares no disk" else sort[] end' "$WORK/lhnode.json"
    else
        unread "nodes.longhorn.io $NODE did not read back as that node"
    fi
elif grep -q NotFound "$WORK/lhnode.err"; then
    echo "longhorn node: NOT FOUND — nodes.longhorn.io holds no $NODE, so Longhorn keeps no disk there"
else
    unread "nodes.longhorn.io $NODE: $(tr '\n' ' ' < "$WORK/lhnode.err")"
fi

echo
echo "node-disk-usage: $NODE — $fs_line; $eph_line; $rep_line"
[ -z "$UNREAD" ] || node_fail "some of $NODE's disk could not be read, and an unread section is not an empty one: $UNREAD"
