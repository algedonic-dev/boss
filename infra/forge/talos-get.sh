#!/usr/bin/env bash
# talos-get.sh — READ-ONLY: one Talos resource of one cluster node, as
# the Talos API states it. The ops verb `talos-get`.
#
#   talos-get.sh <node> machinestatus|disks|volumestatus [<id>]
#
# WHY IT EXISTS (backlog f0aaa72f; design 8457c07b §4). The morning after
# w-1's second NVMe is fitted is read, not assumed: `machinestatus` says
# the node finished booting (stage running, not booting — a node that
# never finishes looks Ready for about 70 minutes), `disks` shows both
# NVMe and which is the system disk whatever its kernel number, and
# `volumestatus u-gate` says the new volume is ready, xfs, at its mount.
# Until this verb every one of those was `talosctl get` typed by hand
# with the admin talosconfig.
#
# THE BOUND. The resource is one of three literal words, checked here and
# fixed by the verb file's one_of, never a free talosctl argument — in
# particular never `machineconfig`, whose bytes carry the cluster's CA
# keys and join tokens, and never `secrets`. The node is one DNS label
# the estate registry declares, not retired, with a `talos-*` role (a
# control plane may be READ); its address is the registry's, one IPv4
# literal. The optional id is one word. It runs `talosctl -n <address>
# get <resource> [<id>] -o yaml` through the ops_talosctl door and prints
# the answer; nothing is written.
#
# Exit 78 is a refusal (the request was wrong); exit 1 a read that could
# not look, with talosctl's own words.
set -uo pipefail

ME=talos-get
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

RESOURCES="machinestatus disks volumestatus"
[ $# -ge 2 ] && [ $# -le 3 ] || node_refuse "usage: talos-get.sh <node> <$(echo "$RESOURCES" | tr ' ' '|')> [<id>]"
NODE="$1"
RESOURCE="$2"
ID="${3:-}"
node_check_id "$NODE"
case " $RESOURCES " in
    *" $RESOURCE "*) ;;
    *) node_refuse "the resource must be one of: $RESOURCES — got '$RESOURCE' (machineconfig and secrets are never read through this door: their bytes carry the cluster's keys)" ;;
esac
if [ -n "$ID" ]; then
    [[ "$ID" =~ ^[a-z0-9][a-z0-9._-]{0,62}$ ]] || node_refuse "the resource id must be one lowercase word (e.g. u-gate, nvme0n1), got '$ID'"
fi
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read"

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

node_read_registry "$NODE"
case "$NODE_ROLE" in
    talos-*) ;;
    *) node_refuse "the estate registry says $NODE is a ${NODE_ROLE:-node with no role}, not a Talos node — there is no Talos API to read" ;;
esac

# The image both doors run in, present and pinned (a bounded pull, named
# when it fails) before talosctl is asked.
node_door
T="$(ops_talosctl)"
echo "talos-get: $RESOURCE${ID:+ $ID} on $NODE ($NODE_ADDRESS, $NODE_ROLE)"
# shellcheck disable=SC2086 # T is one line the door prints to be word-split
if ! $T -n "$NODE_ADDRESS" get "$RESOURCE" ${ID:+"$ID"} -o yaml < /dev/null > "$WORK/out" 2> "$WORK/err"; then
    cat "$WORK/out"
    sed 's/^/    /' "$WORK/err" >&2
    node_fail "talosctl -n $NODE_ADDRESS get $RESOURCE${ID:+ $ID} could not read (its words above)"
fi
cat "$WORK/out"
