#!/usr/bin/env bash
# Read-only API inventory; packet arguments cannot become kubectl arguments.
set -uo pipefail
if [ "$#" -ne 0 ]; then
    echo 'read-volume-snapshots: REFUSED — no arguments accepted' >&2
    exit 78
fi
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2 || exit 1
VOLUME_SNAPSHOTS_KUBECTL="$(ops_kubectl)" || exit 1
export VOLUME_SNAPSHOTS_KUBECTL
exec python3 -B "$HERE/volume-snapshots.py"
