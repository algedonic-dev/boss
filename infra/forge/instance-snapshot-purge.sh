#!/usr/bin/env bash
# A fixed Longhorn action, never an operator-supplied URL or command.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
if ! instances="$("$REPO/infra/cluster/render-instance.sh" --instances)"; then
    echo 'instance-snapshot-purge: CANNOT ANSWER — instance registry unread' >&2
    exit 1
fi
INSTANCE_PURGE_NAMESPACES="$(printf '%s\n' "$instances" | cut -f2)"
export INSTANCE_PURGE_NAMESPACES
ops_image_ready >&2 || exit 1
INSTANCE_PURGE_KUBECTL="$(ops_kubectl)" || exit 1
export INSTANCE_PURGE_KUBECTL
python3 -B "$HERE/instance-snapshot-purge.py" "$@" || exit $?
if [ "${1-}" != --plan ]; then
    echo 'instance-snapshot-purge: per-replica purge complete; capacity recovery not yet proven'
fi
