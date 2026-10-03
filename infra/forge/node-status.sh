#!/usr/bin/env bash
# node-status.sh — READ-ONLY: what would hold a drain of one cluster
# node, read before anyone drains it. The ops verb `node-status`.
#
#   node-status.sh <node>
#
# WHY IT EXISTS (backlog 9fa9f835; review bc33ff50 M1; design 8457c07b
# §1.5). The w-1 second-NVMe window drains w-1 through a graceful Talos
# shutdown, and nothing read whether that drain would be HELD. The dev
# /work volume (longhorn-dev-disposable, one replica) lives on w-1, and
# the tree never sets Longhorn's node-drain-policy, whose default
# (block-if-contains-last-replica) keeps the instance manager's
# PodDisruptionBudget at zero while the node holds a volume's last
# healthy replica. A budget at zero makes every eviction retry, so the
# drain waits, and shutdown-node reads "NOT proven down" after 720 s
# with the node's shutdown sequence still running. Each of those facts
# was an unread assumption; this verb reads them.
#
# WHAT IT PRINTS (stdout, sorted, no clock):
#   * the node: its estate role and address, the Node's InternalIP, its
#     cordon (spec.unschedulable) and every condition the Node reports;
#   * Longhorn's LIVE node-drain-policy setting — read, never assumed,
#     and a setting that does not exist says NOT FOUND, never the default;
#   * the Longhorn volumes whose only healthy replica is on the node, with
#     each volume's state and claim. Healthy is Longhorn's own test for a
#     replica a budget protects: spec.healthyAt set and spec.failedAt
#     empty. The state is printed because whether Longhorn keeps the
#     budget for a DETACHED volume's replica is its own logic, not read
#     here — the budgets section below is the reading of what holds;
#   * the PodDisruptionBudgets at 0 that select a pod the drain would
#     evict (cluster-node-lib.sh's node_held_by_budgets — the same reading
#     the shutdown plan signs).
#
# THE BOUND. The node is one DNS label the estate registry declares, not
# retired, with a talos-* role — a control plane may be READ, as
# talos-get reads one. Every call is a `kubectl get` through the forge's
# ops_kubectl door; nothing is written.
#
# NO EVIDENCE IS NOT A PASS. A section whose read could not look prints
# CANNOT ANSWER with kubectl's words, the other sections still print, and
# the run exits 1 — a Longhorn or budget read that failed must never
# render as a node with nothing on it. Exit 78 is a refusal (the request
# was wrong); exit 0 means every section was read.
set -uo pipefail

ME=node-status
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

[ $# -eq 1 ] || node_refuse "usage: node-status.sh <node> — the node and nothing else"
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

# --- the node -----------------------------------------------------------------
ip="$(jq -r '[(.status.addresses // [])[] | select(.type == "InternalIP") | .address] | join(",")' "$WORK/node.json")"
if [ "$ip" = "$NODE_ADDRESS" ]; then agree="agrees with the estate registry"; else agree="the estate registry says $NODE_ADDRESS — they DISAGREE"; fi
if [ "$(node_unschedulable)" = true ]; then cordon="yes (spec.unschedulable=true)"; else cordon="no (schedulable)"; fi
echo "node-status: $NODE — READ-ONLY; nothing is written"
echo "node: $NODE"
echo "estate role: $NODE_ROLE"
echo "address: $NODE_ADDRESS (the Node's InternalIP ${ip:-none}: $agree)"
echo "cordoned: $cordon"
jq -r '(.status.conditions // [])[] | "condition \(.type): \(.status)\(if (.reason // "") != "" then " (\(.reason))" else "" end)"' "$WORK/node.json"
ready="$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status] | .[0] // "none"' "$WORK/node.json")"

# --- Longhorn -----------------------------------------------------------------
echo
echo "== longhorn =="
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if $K get settings.longhorn.io node-drain-policy -n "$LH" -o json --request-timeout=20s > "$WORK/policy.json" 2> "$WORK/policy.err"; then
    policy="$(jq -r '.value // "no value"' "$WORK/policy.json" 2>&1)"
    echo "node-drain-policy: $policy (the live setting)"
elif grep -q NotFound "$WORK/policy.err"; then
    policy="NOT FOUND"
    echo "node-drain-policy: NOT FOUND — this Longhorn holds no such setting; its documented default is block-if-contains-last-replica, but nothing here says this cluster runs it"
else
    policy="unread"
    unread "settings.longhorn.io node-drain-policy: $(tr '\n' ' ' < "$WORK/policy.err")"
fi

echo
# The healthy test is Longhorn's own for a replica a budget protects.
nlast="unread"
# shellcheck disable=SC2086
if ! $K get replicas.longhorn.io -n "$LH" -o json --request-timeout=30s > "$WORK/replicas.json" 2> "$WORK/replicas.err"; then
    echo "== longhorn volumes whose only healthy replica is on $NODE (unread) =="
    unread "replicas.longhorn.io: $(tr '\n' ' ' < "$WORK/replicas.err")"
# shellcheck disable=SC2086
elif ! $K get volumes.longhorn.io -n "$LH" -o json --request-timeout=30s > "$WORK/volumes.json" 2> "$WORK/volumes.err"; then
    echo "== longhorn volumes whose only healthy replica is on $NODE (unread) =="
    unread "volumes.longhorn.io: $(tr '\n' ' ' < "$WORK/volumes.err")"
elif ! jq -r --arg n "$NODE" --slurpfile vols "$WORK/volumes.json" '
        [.items[] | select((.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "")] as $h
        | [$h[] | select(.spec.nodeID == $n) | .spec.volumeName] | unique[] as $v
        | select([$h[] | select(.spec.volumeName == $v and .spec.nodeID != $n)] | length == 0)
        | ([$vols[0].items[]? | select(.metadata.name == $v)] | .[0]) as $vo
        | ($vo.status.kubernetesStatus // {}) as $ks
        | (if ($ks.pvcName // "") != "" then "pvc \($ks.namespace)/\($ks.pvcName), " else "" end) as $pvc
        | "volume \($v) (\($pvc)\($vo.status.state // "state unread"))"' \
        "$WORK/replicas.json" > "$WORK/last" 2> "$WORK/last.err"; then
    echo "== longhorn volumes whose only healthy replica is on $NODE (unread) =="
    unread "replicas.longhorn.io would not parse: $(tr '\n' ' ' < "$WORK/last.err")"
else
    nlast="$(grep -c '^volume ' "$WORK/last")"
    echo "== longhorn volumes whose only healthy replica is on $NODE ($nlast) =="
    if [ "$nlast" -eq 0 ]; then echo "none"; else cat "$WORK/last"; fi
fi

# --- disruption budgets -------------------------------------------------------
echo
nheld="unread"
if ! node_pods "$NODE"; then
    echo "== disruption budgets at 0 that select a pod the drain would evict on $NODE (unread) =="
    unread "$NODE_ERR"
elif ! node_held_by_budgets; then
    echo "== disruption budgets at 0 that select a pod the drain would evict on $NODE (unread) =="
    unread "$NODE_ERR"
else
    nheld="$(grep -c '^pdb ' "$WORK/budgets.held")"
    echo "== disruption budgets at 0 that select a pod the drain would evict on $NODE ($nheld) =="
    if [ "$nheld" -eq 0 ]; then echo "none"; else cat "$WORK/budgets.held"; fi
fi

echo
echo "node-status: $NODE — cordoned ${cordon%% *}, Ready $ready; node-drain-policy $policy; $nlast volume(s) hold their only healthy replica here; $nheld budget(s) at 0 would hold a drain"
[ -z "$UNREAD" ] || node_fail "some of $NODE could not be read, and an unread section is not an empty one: $UNREAD"
