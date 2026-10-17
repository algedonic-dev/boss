#!/usr/bin/env bash
# Internal fixed registry/Node check; not itself a registered ops verb.
set -uo pipefail
[ "$#" -eq 1 ] || exit 78
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ME=read-gate-storage
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
node_check_id "$1"
WORK="$(mktemp -d)" || exit 1
trap 'rm -rf "$WORK"' EXIT
node_read_registry "$1"
node_require_worker "$1"
node_door
node_read_cluster "$1"
jq -cn --arg address "$NODE_ADDRESS" --arg role "$NODE_ROLE" --slurpfile n "$WORK/node.json" \
    '{address:$address,role:$role,node: {metadata: {name:$n[0].metadata.name,uid:$n[0].metadata.uid,labels:$n[0].metadata.labels},status:{addresses:$n[0].status.addresses}}}'
