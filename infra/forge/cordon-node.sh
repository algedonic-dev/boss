#!/usr/bin/env bash
# cordon-node.sh — cordon or uncordon ONE worker node, and read the cordon
# back. The ops verbs `cordon-node` and `uncordon-node`.
#
#   cordon-node.sh cordon <node>
#   cordon-node.sh uncordon <node>
#
# WHY IT EXISTS (backlog f0aaa72f; design 8457c07b). The w-1 second-NVMe
# window of 2026-09-30 cordons w-1 about 45 minutes before the shutdown,
# so the running gates finish and new gate Jobs wait Pending, and
# uncordons it after boot if Talos has not. Until this verb, that was
# `kubectl cordon` typed by hand with the admin kubeconfig — the hand act
# the ops door exists to replace (design Q2 names the hand fallback, and
# this is what makes it the last time).
#
# THE BOUND is cluster-node-lib.sh's, never a parameter: the node must be
# a `talos-worker` in the estate registry, and the cluster must agree (no
# control-plane label, the registry's address as its InternalIP). The
# action is one of two literal words the verb file fixes; the node is one
# DNS label. A control plane is refused before any door is opened.
#
# THE EFFECT IS READ BACK (backlog fdbb447e): after the write, a separate
# read of the Node's spec.unschedulable must say what the write was for —
# `true` after a cordon, unset or `false` after an uncordon — and only then
# is the line the verb file declares as `effect` printed. A write that
# answered and changed nothing exits 1, saying so.
#
# Exit 78 is a refusal (the request was wrong); exit 1 a step that failed,
# including a read that could not look.
set -uo pipefail

case "${1:-}" in
    cordon) ME=cordon-node ;;
    uncordon) ME=uncordon-node ;;
    *) echo "cordon-node: REFUSED — usage: cordon-node.sh cordon|uncordon <node>, got '${1:-}'" >&2; exit 78 ;;
esac
ACTION="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

[ $# -eq 2 ] || node_refuse "usage: cordon-node.sh $ACTION <node> — one node, nothing else"
NODE="$2"
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read, and no evidence is not a worker"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
node_read_cluster "$NODE"
before="$(node_unschedulable)"
echo "$ME: $NODE is a talos-worker at $NODE_ADDRESS (the estate registry and the cluster agree); spec.unschedulable=$before before the $ACTION"

# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K "$ACTION" "$NODE" --request-timeout=30s > "$WORK/write.out" 2> "$WORK/write.err"; then
    sed 's/^/    /' "$WORK/write.err" >&2
    node_fail "kubectl $ACTION $NODE failed (its words above)"
fi
sed 's/^/    /' "$WORK/write.out"

# A forge answer is not a forge effect: read the cordon back, separately.
# shellcheck disable=SC2086
if ! after="$($K get node "$NODE" -o 'jsonpath={.spec.unschedulable}' --request-timeout=20s 2> "$WORK/back.err")"; then
    node_fail "CANNOT ANSWER — kubectl $ACTION $NODE answered, but the node could not be read back: $(tr '\n' ' ' < "$WORK/back.err")"
fi
if [ "$ACTION" = cordon ]; then
    [ "$after" = true ] \
        || node_fail "kubectl cordon $NODE answered, and spec.unschedulable reads back '${after:-unset}' — the node is NOT cordoned"
    echo "cordon-node: $NODE is cordoned — read back spec.unschedulable=true (was $before)"
else
    case "$after" in
        '' | false) ;;
        *) node_fail "kubectl uncordon $NODE answered, and spec.unschedulable reads back '$after' — the node is NOT schedulable" ;;
    esac
    echo "uncordon-node: $NODE is schedulable — read back spec.unschedulable=${after:-unset} (was $before)"
fi
