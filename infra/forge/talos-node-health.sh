#!/usr/bin/env bash
# READ-ONLY fixed Talos health readings (ff99932e). No path or action is
# supplied to talosctl by a request. The sibling get door stays unchanged.
set -uo pipefail
ME=talos-node-health
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/cluster-node-lib.sh"
[ $# -ge 2 ] && [ $# -le 3 ] || node_refuse 'expected node, reading and optional fixed service'
NODE="$1"
node_check_id "$NODE"
# Validate BEFORE registry/image access; Python owns the one argv mapping.
REQUIRED_ROLE="$(python3 "$HERE/talos-node-health.py" --validate "$2" "${3:-}")" || exit $?
WORK="$(mktemp -d)" || node_fail 'cannot make scratch directory'
trap 'rm -rf "$WORK"' EXIT
node_read_registry "$NODE"
node_require_talos "$NODE"
if [ "$REQUIRED_ROLE" != talos ]; then
    [ "$NODE_ROLE" = "$REQUIRED_ROLE" ] || node_refuse 'this fixed reading requires a control-plane node'
fi
node_door
if [ "$REQUIRED_ROLE" != talos ]; then
    node_agree_cluster "$NODE"
fi
T="$(ops_talosctl)"
# T is the existing fixed container door, not command text from a packet.
# shellcheck disable=SC2086
python3 "$HERE/talos-node-health.py" "$2" "${3:-}" -- $T -n "$NODE_ADDRESS"
exit $?
