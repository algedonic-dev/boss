#!/usr/bin/env bash
# Fixed read-only Longhorn history; trim is a separate approved operation.
set -uo pipefail
if [ "$#" -ne 0 ]; then
    echo 'read-volume-replica-history: REFUSED — no arguments accepted' >&2
    exit 78
fi
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2 || exit 1
VOLUME_REPLICA_HISTORY_KUBECTL="$(ops_kubectl)" || exit 1
export VOLUME_REPLICA_HISTORY_KUBECTL
exec python3 -B "$HERE/volume-replica-history.py"
