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

# WHAT HOLDS THE NODE (backlog 2b7c78f8). On 2026-11-21 builders and a
# gate were refused `Insufficient cpu` on w-1 while the capacity record
# said it had room, and nothing the dev pod could reach said what held
# the box: this verb listed pods without their requests, and `kubectl
# describe node`'s "Allocated resources" block is a read of the Node the
# dev pod is forbidden. So the verb prints that block itself, from the
# two reads it already made:
#
#   requests <namespace>/<name> <phase> cpu <cores> memory <GiB>Gi
#   allocated cpu <sum> of <allocatable> allocatable (<pct>%) on <n> pod(s)
#   allocated memory <sum>Gi of <allocatable>Gi allocatable (<pct>%)
#
# one `requests` line per pod the scheduler still counts (every phase but
# Succeeded and Failed, as describe node counts them), largest CPU first.
# A pod's request is the SCHEDULER's, not a sum of its containers: a
# native sidecar (an init container with restartPolicy Always) runs
# beside everything after it, so it adds; an ordinary init container
# runs alone, so it is a max against what runs with it; the pod's
# overhead adds on top (KEP-753). A gate pod is the case that matters:
# its postgres sidecar's 250m and 3Gi beside the gate's 4 and 8Gi make
# 4.25 and 11Gi, which the record charged as 4 until this car.
#
# NO EVIDENCE IS NOT A PASS: a Node that reports no allocatable, or a
# quantity this reader cannot parse, is CANNOT ANSWER and exit 1 — never
# a sum that counted the unread as nothing.
# shellcheck disable=SC2016 # jq's $vars, not the shell's
REQ_JQ='
def qty:
  if . == null then 0
  else (tostring) as $t
  | ($t | capture("^(?<n>[0-9]+(\\.[0-9]+)?([eE][-+]?[0-9]+)?)(?<s>Ki|Mi|Gi|Ti|Pi|Ei|m|k|M|G|T|P|E)?$") // null) as $c
  | if $c == null then error("the quantity \($t) is not one this reader parses")
    else ($c.n | tonumber) * ({"": 1, "m": 0.001, "k": 1e3, "M": 1e6, "G": 1e9, "T": 1e12, "P": 1e15, "E": 1e18,
          "Ki": 1024, "Mi": 1048576, "Gi": 1073741824, "Ti": 1099511627776,
          "Pi": 1125899906842624, "Ei": 1152921504606846976}[$c.s // ""])
    end
  end;
def req($r): (.resources.requests // {})[$r] | qty;
def effective($r):
  (reduce (.spec.initContainers // [])[] as $c ({side: 0, init: 0};
      ($c | req($r)) as $q
      | if $c.restartPolicy == "Always" then .side += $q
        else .init = ([.init, $q + .side] | max) end)) as $i
  | ([(.spec.containers // [])[] | req($r)] | add // 0) as $main
  | ([$i.init, $main + $i.side] | max) + ((.spec.overhead // {})[$r] | qty);
def r2: (. * 100 | round) / 100;
def gib: . / 1073741824 | r2;
($node[0].status.allocatable // {}) as $a
| if ($a.cpu == null or $a.memory == null) then error("the Node \($name) reports no allocatable cpu and memory") else . end
| [.items[]
   | select((.status.phase // "Unknown") as $p | $p != "Succeeded" and $p != "Failed")
   | {ref: "\(.metadata.namespace)/\(.metadata.name)", phase: (.status.phase // "Unknown"),
      cpu: effective("cpu"), mem: effective("memory")}]
| sort_by([-.cpu, .ref]) as $pods
| ($pods | map(.cpu) | add // 0) as $cpu
| ($pods | map(.mem) | add // 0) as $mem
| ($a.cpu | qty) as $acpu | ($a.memory | qty) as $amem
| ($pods[] | "requests \(.ref) \(.phase) cpu \(.cpu | r2) memory \(.mem | gib)Gi"),
  "allocated cpu \($cpu | r2) of \($acpu | r2) allocatable (\(if $acpu > 0 then ($cpu * 100 / $acpu | floor) else "n/a" end)%) on \($pods | length) pod(s)",
  "allocated memory \($mem | gib)Gi of \($amem | gib)Gi allocatable (\(if $amem > 0 then ($mem * 100 / $amem | floor) else "n/a" end)%)"
'
if ! jq -r --slurpfile node "$WORK/node.json" --arg name "$NODE" "$REQ_JQ" \
        "$WORK/pods.json" > "$WORK/requests" 2> "$WORK/requests.jq"; then
    echo "CANNOT ANSWER — $(sed 's/^jq: error ([^)]*): //' "$WORK/requests.jq" | tr '\n' ' ')"
    node_fail "what holds $NODE could not be read, and an unread request is not a zero one"
fi
echo "== requests on $NODE (the pods the scheduler counts; largest CPU first) =="
cat "$WORK/requests"
echo
echo "node-pods: $NODE — $n pod(s): ${summary:-none}"
