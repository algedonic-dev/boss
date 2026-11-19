#!/usr/bin/env bash
# node-labels.sh — READ-ONLY: a cluster node's labels and taints, and
# whether the purpose label the estate registry declares for it is the
# one it carries. The ops verb `node-labels`.
#
#   node-labels.sh <node>
#
# WHY IT EXISTS (backlog 01927489). join-a-node's `verify-role` step asks
# for `labels` and `declared_in`. w-2's join (829f75a8) answered it with
# `kubectl get node w-2 --show-labels`, run by hand by David, and w-3's
# join (aca68557) had no door at all: node-status prints conditions, not
# labels. A CLI line handed to David is a coverage gap.
#
# WHAT IT PRINTS (stdout, sorted, no clock):
#   * the node: its estate role and address against the Node's InternalIP,
#     the kubelet, OS, kernel and runtime the Node reports, its cordon and
#     its Ready condition;
#   * every label as `label <key>=<value>`, every taint as
#     `taint <key>=<value>:<effect>`;
#   * the purpose: the `boss.dev/purpose=<value>` the estate registry's
#     row declares (in its notes, where w-2's and w-3's live today) beside
#     the value the Node carries, judged AGREES, DISAGREES, UNDECLARED
#     (neither names one) or AMBIGUOUS (the row names two). A judgement is
#     a reading, not a failure: the run exits 0 whatever it says, and the
#     closing line carries the word for the step that quotes it.
#
# THE BOUND is node-status's: one DNS label the estate registry declares
# once, not retired, with a talos-* role — a control plane may be READ.
# Every call is a `kubectl get` through the forge's ops_kubectl door;
# nothing is written. Exit 78 is a refusal (the request was wrong), exit 1
# a read that could not look, exit 0 a node read in full.
set -uo pipefail

ME=node-labels
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

PURPOSE_KEY=boss.dev/purpose

[ $# -eq 1 ] || node_refuse "usage: node-labels.sh <node> — the node and nothing else"
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

if ! jq_doc_file "$WORK/node.json" || [ "$(jq -r '.kind // empty' "$WORK/node.json" 2>/dev/null)" != Node ]; then
    node_fail "node $NODE did not read back as a Node object: $(head -c 300 "$WORK/node.json" | tr '\n' ' ')"
fi

# --- the node -----------------------------------------------------------------
ip="$(jq -r '[(.status.addresses // [])[] | select(.type == "InternalIP") | .address] | join(",")' "$WORK/node.json")"
if [ "$ip" = "$NODE_ADDRESS" ]; then agree="agrees with the estate registry"; else agree="the estate registry says $NODE_ADDRESS — they DISAGREE"; fi
if [ "$(node_unschedulable)" = true ]; then cordon="yes (spec.unschedulable=true)"; else cordon="no (schedulable)"; fi
echo "node-labels: $NODE — READ-ONLY; nothing is written"
echo "node: $NODE"
echo "estate role: $NODE_ROLE"
echo "address: $NODE_ADDRESS (the Node's InternalIP ${ip:-none}: $agree)"
jq -r '(.status.nodeInfo // {}) as $i
    | "runs: kubelet \($i.kubeletVersion // "unread"); os \($i.osImage // "unread"); kernel \($i.kernelVersion // "unread"); runtime \($i.containerRuntimeVersion // "unread")"' "$WORK/node.json"
echo "cordoned: $cordon"
jq -r '(.status.conditions // [])[] | select(.type == "Ready") | "condition Ready: \(.status)\(if (.reason // "") != "" then " (\(.reason))" else "" end)"' "$WORK/node.json"

# --- labels and taints ----------------------------------------------------------
jq -r '(.metadata.labels // {}) | to_entries[] | "label \(.key)=\(.value)"' "$WORK/node.json" | LC_ALL=C sort > "$WORK/labels"
jq -r '(.spec.taints // [])[] | "taint \(.key)\(if (.value // "") != "" then "=" + .value else "" end):\(.effect // "")"' "$WORK/node.json" | LC_ALL=C sort > "$WORK/taints"
nlabels="$(grep -c . "$WORK/labels")"
ntaints="$(grep -c . "$WORK/taints")"
echo
echo "== labels ($nlabels) =="
if [ "$nlabels" -eq 0 ]; then echo "none"; else cat "$WORK/labels"; fi
echo
echo "== taints ($ntaints) =="
if [ "$ntaints" -eq 0 ]; then echo "none"; else cat "$WORK/taints"; fi

# --- the purpose ----------------------------------------------------------------
# What the row declares: every `boss.dev/purpose=<value>` its notes name,
# distinct. What the Node carries: that label's value, if any.
declared="$(printf '%s\n' "${NODE_NOTES:-}" | grep -oE 'boss\.dev/purpose=[a-z0-9]([a-z0-9._-]*[a-z0-9])?' | sed 's|^boss\.dev/purpose=||' | LC_ALL=C sort -u | paste -sd, -)"
carried="$(jq -r --arg k "$PURPOSE_KEY" '(.metadata.labels // {})[$k] // ""' "$WORK/node.json")"
echo
echo "== purpose =="
if [ -z "$declared" ]; then
    echo "declared: none — the estate registry's row for $NODE names no $PURPOSE_KEY"
else
    echo "declared: $PURPOSE_KEY=$declared (the estate registry's row for $NODE, its notes)"
fi
if [ -z "$carried" ]; then
    echo "carried: none — the Node carries no $PURPOSE_KEY label"
else
    echo "carried: $PURPOSE_KEY=$carried (the Node's label)"
fi
case "$declared" in
    *,*) verdict=AMBIGUOUS ;;
    "") if [ -z "$carried" ]; then verdict=UNDECLARED; else verdict=DISAGREES; fi ;;
    *) if [ "$declared" = "$carried" ]; then verdict=AGREES; else verdict=DISAGREES; fi ;;
esac

echo
echo "node-labels: $NODE — $nlabels label(s), $ntaints taint(s); $PURPOSE_KEY declared ${declared:-none}, carried ${carried:-none}: $verdict"
