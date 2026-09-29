#!/usr/bin/env bash
# observe-nodefs — each cluster node's kubelet filesystem, total and free,
# read through the Talos API from OUTSIDE the cluster (backlog eeac3d56).
#
# WHY IT MOVED HERE. The free figure the estate's disk floor judges for a
# cluster node came from the in-cluster observer (boss-estate-observe.yaml)
# reading the kubelet's /stats/summary through the API server — authorized
# as `get nodes/proxy`. That grant is not the read-only thing its header
# said: the kubelet authorizes a WebSocket exec, attach or port-forward
# opened as a GET as `get nodes/proxy` too, so the observer's token could
# exec into ANY pod on ANY node, the boss pod holding the broker's root
# tokens included (review of cbb56130, 2026-09-29). The narrower in-cluster
# read, `nodes/stats` straight to the kubelet on :10250, needs a TLS
# decision this cluster cannot make well today: Talos kubelet serving
# certs are self-signed per node, so it would be skip-verify with a bearer
# token. The forge already holds the admin talosconfig (design 835c0c9c),
# and the Talos API states the same statfs the kubelet does, so the
# reading moves here and the grant goes away entirely.
#
# WHICH FILESYSTEM. The kubelet's root directory on Talos is
# /var/lib/kubelet, on the EPHEMERAL partition mounted at /var — the
# filesystem `status.capacity.ephemeral-storage` describes and the one
# `.node.fs` of /stats/summary reported. `talosctl mounts` lists every
# mount with its statfs size and available bytes (printed in GB, 1e9, two
# decimals); this takes the row mounted on exactly $NODEFS_MOUNT (/var).
# That it IS the kubelet's filesystem is not assumed downstream: the
# in-cluster observer compares this total with the node's
# ephemeral-storage capacity and refuses a free figure whose total does
# not match, so a wrong row reads as unmeasured, never as headroom.
#
# WHAT IT POSTS. One `talos-nodefs` observation per run: every node
# `kubectl get nodes` lists (through the admin kubeconfig, so a node
# nobody declared is measured too — the in-cluster set, not the
# registry's), each with `disk_gb` and `disk_free_gb` in GiB (nearest,
# ties up — THE one rounding rule), or both null and `unread` naming why.
# One node's failed read costs that node's figure and nothing else. The
# in-cluster observer merges the newest reading into its
# kubernetes-nodes observation, where estate.compare's floor and the
# estate page already read it, and a reading too old to trust is
# `disk_unmeasured` there — so this loop going quiet raises the
# existing blind-class alarm rather than nothing.
#
# NOT RETAINED. The host observer spools a reading the record will not
# take; this one does not. Its only reader wants the NEWEST reading and
# judges its age, so a replayed old one is worth nothing, and a spool
# would only put stale rows at the head of the series.
#
# Env: JOBS_API (required to post; from /etc/boss/sor.env),
#      BOSS_OPS_DIR / BOSS_TALOSCTL / BOSS_TALOS_TIMEOUT_S (ops-credentials.sh),
#      NODEFS_MOUNT (default /var), BOSS_OBSERVE_WORK (scratch; default a
#      mktemp -d of its own).
#
# `--print` reads and prints the observation instead of posting it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$here/observe-lib.sh"
# shellcheck source=/dev/null
. "$here/ops-credentials.sh"

mode=post
case "${1:-}" in
    --print) mode=print ;;
    '') ;;
    *) echo "usage: observe-nodefs.sh [--print]" >&2; exit 2 ;;
esac
[[ "$mode" == print ]] || : "${JOBS_API:?JOBS_API is required to post (it comes from /etc/boss/sor.env)}"

MOUNT="${NODEFS_MOUNT:-/var}"
if [[ -n "${BOSS_OBSERVE_WORK:-}" ]]; then
    WORK="$BOSS_OBSERVE_WORK"; mkdir -p "$WORK"
else
    WORK="$(mktemp -d)" || exit 1
    trap 'rm -rf "$WORK"' EXIT
fi

# The node list, as the cluster states it. No list is no observation:
# the door refuses one that saw nothing, and the in-cluster observer then
# ages this series out into `disk_unmeasured` — loud either way.
# Bounded: this runs inside the forge's host observer unit, and a list
# that never answers would hold that unit activating — and the forge's
# own host reading with it (review of 4327d1a3, finding 3; the unit's
# TimeoutStartSec is the outer bound for a stalled docker daemon).
if ! $(ops_kubectl) get nodes -o json --request-timeout=20s >"$WORK/nodes.json" 2>"$WORK/nodes.err" \
    || [[ "$(jq -r '.items | type' "$WORK/nodes.json" 2>/dev/null)" != array ]]; then
    echo "observe-nodefs: REFUSED — could not list the cluster's nodes through $(ops_kubeconfig): $(tr '\n' ' ' <"$WORK/nodes.err" | cut -c1-400)" >&2
    exit 1
fi

: >"$WORK/rows.jsonl"
while IFS="$(printf '\t')" read -r id address; do
    [[ -n "$id" ]] || continue
    unread=""
    # ONE address literal, or talosctl is never called (review of
    # 4327d1a3, finding 2). `-n` is a string slice, so a comma list would
    # aim the admin talosconfig at several targets, and a hostname would
    # have the endpoint's apid proxy to whatever it resolves to. Writing
    # a node's status.addresses already takes node or admin credentials;
    # refusing here costs nothing and is loud.
    if [[ "$address" == "-" ]]; then
        unread="the node lists no InternalIP to reach its Talos API at"
    elif ! [[ "$address" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ || "$address" =~ ^[0-9A-Fa-f]{0,4}(:[0-9A-Fa-f]{0,4}){2,7}$ ]]; then
        unread="its InternalIP ($(printf '%s' "$address" | cut -c1-80)) is not one IPv4 or IPv6 address literal, so the Talos API was not asked"
    elif ! $(ops_talosctl) -n "$address" mounts </dev/null >"$WORK/$id.mounts" 2>"$WORK/$id.err"; then
        unread="talosctl -n $address mounts failed: $(tr '\n' ' ' <"$WORK/$id.err" | cut -c1-300)"
    fi
    if [[ -z "$unread" ]]; then
        # Columns, counted from the END because the NODE column is
        # present only when the answer carries node metadata:
        # ... SIZE(GB) USED(GB) AVAILABLE(GB) PERCENT MOUNTED-ON.
        pairs=$(awk -v m="$MOUNT" '$NF == m && NF >= 6 {
                    printf "%d %d\n", int(($(NF-4) * 1e9 + 536870912) / 1073741824),
                                      int(($(NF-2) * 1e9 + 536870912) / 1073741824) }' \
                    "$WORK/$id.mounts" | sort -u)
        case "$(printf '%s' "$pairs" | grep -c .)" in
            1) read -r total free <<<"$pairs" ;;
            0) unread="talosctl -n $address mounts listed no filesystem mounted on $MOUNT" ;;
            *) unread="talosctl -n $address mounts listed $MOUNT with different sizes: $(printf '%s' "$pairs" | tr '\n' ';')" ;;
        esac
    fi
    if [[ -n "$unread" ]]; then
        echo "no nodefs reading for $id: $unread"
        jq -cn --arg id "$id" --arg address "$address" --arg unread "$unread" \
            '{ id: $id, address: (if $address == "-" then null else $address end),
               disk_gb: null, disk_free_gb: null, unread: $unread }' >>"$WORK/rows.jsonl"
    else
        echo "  $id ($address): $MOUNT disk=${total}G free=${free}G"
        jq -cn --arg id "$id" --arg address "$address" --argjson total "$total" --argjson free "$free" \
            '{ id: $id, address: $address, disk_gb: $total, disk_free_gb: $free }' >>"$WORK/rows.jsonl"
    fi
done < <(jq -r '(.items // [])[]
                | [ .metadata.name,
                    ([(.status.addresses // [])[] | select(.type == "InternalIP") | .address] | .[0] // "-") ]
                | @tsv' "$WORK/nodes.json")

observation=$(jq -cs --arg mount "$MOUNT" '{
    observed_at: (now | todate),
    observer: "boss-estate-observe-nodefs",
    scope: "talos-nodefs",
    mount: $mount,
    nodes: .
}' "$WORK/rows.jsonl")

if [[ "$mode" == print ]]; then
    printf '%s\n' "$observation"
    exit 0
fi
post_observation "$observation"
