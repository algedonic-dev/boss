#!/usr/bin/env bash
# node-pods.sh — READ-ONLY: every pod the cluster has placed on one node,
# with its namespace, name, phase, images and owner. The ops verb
# `node-pods`.
#
#   node-pods.sh <node>
#
# WHY IT EXISTS (backlog 01927489). join-a-node's `prove` step asks for
# the `workload` a new node ran and the `evidence` that it ran it. w-2's
# join (829f75a8) answered with the DaemonSet pods the cluster had placed
# there, read by hand; no verb listed the pods on a node (node-status
# reads them only to judge a drain, and prints none), so w-3's join
# (aca68557) had no door to that answer.
#
# WHAT IT PRINTS (stdout, sorted by namespace/name, no clock): one line
# per pod in every phase — a Succeeded probe is evidence too —
#
#   pod <namespace>/<name> <phase> image <image>[,<image>…] [init <image>[,…]] owner <kind name>
#
# and a closing line counting them by phase. The list is the API server's
# own filter (`--field-selector spec.nodeName=<node>`) and never a scan.
#
# A WRONG TARGET ANSWERS INSTEAD OF ERRORING (CLAUDE.md §Doors): a field
# selector on a node the cluster does not hold answers an empty list. So
# the Node itself is read first, and a node that is not there fails
# (exit 1) rather than reading as a node with nothing on it; a list that
# could not be read prints CANNOT ANSWER and fails the same way.
#
# THE BOUND is node-status's: one DNS label the estate registry declares
# once, not retired, with a talos-* role — a control plane may be READ.
# Every call is a `kubectl get` through the forge's ops_kubectl door;
# nothing is written. Exit 78 is a refusal, exit 1 an unread answer.
set -uo pipefail

ME=node-pods
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

[ $# -eq 1 ] || node_refuse "usage: node-pods.sh <node> — the node and nothing else"
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
# The Node first: an absent one fails here, never as an empty list below.
node_get "$NODE"

echo "node-pods: $NODE — READ-ONLY; nothing is written"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K get pods -A --field-selector "spec.nodeName=$NODE" -o json --request-timeout=30s \
        > "$WORK/pods.json" 2> "$WORK/pods.err"; then
    echo "CANNOT ANSWER — kubectl get pods on $NODE: $(tr '\n' ' ' < "$WORK/pods.err")"
    node_fail "the pods on $NODE could not be read, and an unread node is not an empty one"
fi
if ! jq -r '[.items[]
        | ([.metadata.ownerReferences[]? | select(.controller != false)] | .[0] // null) as $o
        | { ref: "\(.metadata.namespace)/\(.metadata.name)",
            phase: (.status.phase // "Unknown"),
            images: ([.spec.containers[]?.image] | join(",")),
            init: ([.spec.initContainers[]?.image] | join(",")),
            owner: (if (.metadata.annotations // {})["kubernetes.io/config.mirror"] != null then "static mirror pod"
                    elif $o == null then "no owner"
                    else "\($o.kind) \($o.name)" end) }]
        | sort_by(.ref)[]
        | "pod \(.ref) \(.phase) image \(if .images == "" then "none" else .images end)\(if .init != "" then " init " + .init else "" end) owner \(.owner)"' \
        "$WORK/pods.json" > "$WORK/lines" 2> "$WORK/pods.jq"; then
    echo "CANNOT ANSWER — the pod list for $NODE would not parse: $(tr '\n' ' ' < "$WORK/pods.jq")"
    node_fail "the pods on $NODE could not be read, and an unread node is not an empty one"
fi
summary="$(jq -r '[.items[] | .status.phase // "Unknown"] | group_by(.) | map("\(length) \(.[0])") | join(", ")' "$WORK/pods.json")"
n="$(grep -c '^pod ' "$WORK/lines")"

echo "== pods on $NODE ($n) =="
if [ "$n" -eq 0 ]; then echo "none"; else cat "$WORK/lines"; fi
echo
echo "node-pods: $NODE — $n pod(s): ${summary:-none}"
