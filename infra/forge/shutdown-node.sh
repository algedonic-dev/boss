#!/usr/bin/env bash
# shutdown-node.sh — a graceful Talos shutdown of ONE cordoned worker,
# through a rendered plan a passkey signs. The ops verbs
# `plan-a-node-shutdown` (--plan) and `shutdown-node` (the write).
#
#   shutdown-node.sh --plan <node>
#   shutdown-node.sh <node> <plan-sha256>
#
# WHY IT EXISTS (backlog f0aaa72f; design 8457c07b §2). Fitting w-1's
# second NVMe needs w-1 powered off, and the design decided a graceful
# Talos shutdown through the ops door over the power button: `talosctl
# shutdown` cordons and drains the node before it powers off, so pods
# leave through the eviction API and Longhorn detaches cleanly, where a
# hard cut leaves the node NotReady for five minutes before the taint
# manager evicts anything — the Longhorn Multi-Attach shape of
# 2026-09-11. Until this verb it was `talosctl -n <address> shutdown`
# typed by hand with the admin talosconfig.
#
# THE SHAPE is design 17835005's (David, 2026-09-21: a rendered plan
# hash, signed with his passkey, single-use, verified before the argv is
# built) — reclaim-gcp-root's and reap-terminated-pods': the verb file
# declares requires_approval and names plan-a-node-shutdown as its
# plan_verb, so the runner renders this script's --plan onto the
# request's approve step and hands the write sha256 of the SIGNED plan.
# It takes a node down, so it is authorised per run (design 8457c07b Q1).
#
# THE BOUND is cluster-node-lib.sh's, plus one of its own, and never a
# parameter:
#   * a `talos-worker` in the estate registry, and the cluster agrees (no
#     control-plane label, the registry's address as its InternalIP);
#   * ALREADY CORDONED (spec.unschedulable=true) — cordon-node first, as
#     the design's window does. On a cordoned node the set of pods the
#     drain will evict stops changing, so the list the passkey signs is
#     the list the drain acts on; on a schedulable one it would be a
#     guess that drifts under every scheduled gate.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`, since a hash
# cannot be inside what it hashes) names the node, its registry role and
# address, the Node object's uid, its cordon and its Ready condition, the
# one command the write runs, and the pods: those the drain will evict
# (every pod on the node that is still Pending or Running and is neither
# a DaemonSet's nor a static mirror pod) and those that stay until power
# off (the DaemonSet and mirror pods), and the PodDisruptionBudgets at 0
# that select a pod the drain will evict — each one holds the drain
# (backlog 9fa9f835, review bc33ff50 M1). No clock, no age, no restart
# count, pods sorted — two renders of one true state are byte-identical.
# Ready is IN the bytes on purpose: after the shutdown the node reads
# NotReady, so a second run of an applied plan re-renders to other bytes
# and is refused — at most once, so no approval is spent twice.
#
# THE WRITE re-renders, refuses (exit 78, naming both hashes, nothing
# done) unless today's plan hashes to the approved one, prints the plan
# (the capture before the act), and runs `talosctl -n <address> shutdown
# --wait=false` through the ops_talosctl door — never --force, which
# skips the cordon and drain. `--wait=false` because the door bounds
# talosctl with a short timeout inside its container, and this script
# does its own waiting, reading the effect rather than a progress stream.
#
# THE EFFECT IS READ BACK (backlog fdbb447e). Every BOSS_SHUTDOWN_POLL_S
# (default 10) up to BOSS_SHUTDOWN_WAIT_S (default 720, inside the verb's
# 900 s timeout so the verdict is this script's, not the runner's kill),
# two independent reads: Kubernetes' Ready condition for the Node, and
# `talosctl -n <address> version` through the door. Only when the Node
# reads Ready=False or Unknown AND the Talos API no longer answers is the
# line the verb file declares as `effect` printed. A drain that outlasts
# the wait, or a shutdown accepted and never taken, exits 1 saying the
# node is NOT proven down — with the last thing each read said.
#
# Powering the machine back on is a hand at the machine; nothing here
# can, and the plan says so.
#
# Exit 78 is a refusal (the request was wrong or no longer true); exit 1
# a step that failed, including a read that could not look.
set -uo pipefail

ME=shutdown-node
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 1 ] || node_refuse "usage: shutdown-node.sh --plan <node> — the plan takes the node and nothing else"
    NODE="$1"
else
    [ $# -eq 2 ] || node_refuse "usage: shutdown-node.sh <node> <plan-sha256> — this shuts down only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-node-shutdown verb)"
    NODE="$1"
    [[ "$2" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$2'"
    APPROVED="$2"
fi
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read, and no evidence is not a worker"

WAIT_S="${BOSS_SHUTDOWN_WAIT_S:-720}"
POLL_S="${BOSS_SHUTDOWN_POLL_S:-10}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_SHUTDOWN_WAIT_S and BOSS_SHUTDOWN_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the plan ---------------------------------------------------------------
# render — the plan for the node as it stands now (after the registry
# read below). Reads the cluster; nothing in the bytes is a clock.
render() {
    node_read_cluster "$NODE"
    [ "$(node_unschedulable)" = true ] \
        || node_refuse "$NODE is not cordoned (spec.unschedulable is not true) — run cordon-node $NODE first and let its running pods finish; on a schedulable node the pods a passkey signs for eviction are not the pods the drain would find"
    node_pods "$NODE" \
        || node_fail "could not read the pods on $NODE ($NODE_ERR) — a node nobody read has no known evictions, so there is no plan"
    node_held_by_budgets \
        || node_fail "could not read the disruption budgets ($NODE_ERR) — a drain whose holds nobody read is a guess, so there is no plan"
    local uid ready nev nstay nheld
    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    ready="$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status] | .[0] // "none"' "$WORK/node.json")"
    nev="$(jq '[.[] | select(.stays | not)] | length' "$WORK/pods.sel")"
    nstay="$(jq '[.[] | select(.stays)] | length' "$WORK/pods.sel")"
    nheld="$(grep -c '^pdb ' "$WORK/budgets.held")"
    echo "plan: shutdown-node"
    echo "node: $NODE"
    echo "address: $NODE_ADDRESS"
    echo "estate role: $NODE_ROLE"
    echo "kubernetes node uid: $uid"
    echo "cordoned: yes (spec.unschedulable=true)"
    echo "kubernetes Ready: $ready"
    echo "command: talosctl -n $NODE_ADDRESS shutdown --wait=false — a graceful Talos shutdown: Talos cordons and drains the node through the eviction API, then powers it off. Powering it back on is a hand at the machine."
    echo "selects: every pod on $NODE whose phase is not Succeeded or Failed; a DaemonSet's pod or a static mirror pod stays until power off, every other one is evicted by the drain."
    echo
    echo "== pods the drain will evict ($nev) =="
    if [ "$nev" -eq 0 ]; then echo "none"; else
        jq -r '.[] | select(.stays | not) | "pod \(.ref) (\(.owner))"' "$WORK/pods.sel"
    fi
    echo
    echo "== pods that stay until power off ($nstay) =="
    if [ "$nstay" -eq 0 ]; then echo "none"; else
        jq -r '.[] | select(.stays) | "pod \(.ref) (\(.owner))"' "$WORK/pods.sel"
    fi
    echo
    # Review bc33ff50 M1 (backlog 9fa9f835): a budget at zero holds the
    # drain, and the write would then read NOT proven down after 720 s.
    echo "== disruption budgets at 0 that select a pod the drain will evict ($nheld) =="
    if [ "$nheld" -eq 0 ]; then echo "none"; else
        cat "$WORK/budgets.held"
        echo "each budget above holds Talos's drain until it allows a disruption; node-status $NODE names the Longhorn volumes whose only healthy replica is here and the live node-drain-policy"
    fi
}

# The registry first, so a control plane is refused before any door opens.
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
render > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    node_refuse "today's plan for $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the node, its cordon, its Ready condition or the pods on it moved since the plan was signed (or this plan already ran). Nothing was shut down; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "shutdown-node: plan $APPROVED still holds — shutting down $NODE ($NODE_ADDRESS)"

T="$(ops_talosctl)"
# shellcheck disable=SC2086 # T is one line the door prints to be word-split
if ! $T -n "$NODE_ADDRESS" shutdown --wait=false < /dev/null > "$WORK/shutdown.out" 2> "$WORK/shutdown.err"; then
    sed 's/^/    /' "$WORK/shutdown.err" >&2
    node_fail "talosctl -n $NODE_ADDRESS shutdown failed (its words above) — $NODE was NOT shut down by this run"
fi
sed 's/^/    /' "$WORK/shutdown.out"
echo "shutdown-node: talosctl accepted the shutdown of $NODE — reading it back (up to ${WAIT_S}s: Kubernetes' Ready condition, and whether the Talos API still answers)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
k8s="not read yet"
talos="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $K get node "$NODE" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err"; then
        k8s="$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status] | .[0] // "none"' "$WORK/back.json")"
    else
        k8s="unread: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    # shellcheck disable=SC2086
    if $T -n "$NODE_ADDRESS" version < /dev/null > "$WORK/version.out" 2> "$WORK/version.err"; then
        talos="answers"
    else
        talos="silent: $(tr '\n' ' ' < "$WORK/version.err" | cut -c1-200)"
    fi
    elapsed=$(( $(date -u +%s) - start ))
    case "$k8s|$talos" in
        False\|silent:* | Unknown\|silent:*)
            echo "shutdown-node: $NODE is down — read back: Kubernetes reports Ready=$k8s and the Talos API at $NODE_ADDRESS no longer answers (after ${elapsed}s; talosctl: ${talos#silent: })"
            exit 0 ;;
    esac
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "talosctl accepted the shutdown of $NODE, and after ${elapsed}s it is NOT proven down: Kubernetes Ready=$k8s, Talos API $talos. The drain may still be running, or the shutdown was never taken — read it again with talos-get $NODE machinestatus"
    fi
    sleep "$POLL_S"
done
