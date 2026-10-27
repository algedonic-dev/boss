#!/usr/bin/env bash
# Fixed dev-work purpose; the instances-only expander remains unchanged.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../lib/sor.sh"
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2
BOSS_DEV_WORK_KUBECTL=$(ops_kubectl)
export BOSS_DEV_WORK_KUBECTL
python3 -B "$HERE/dev-work-volume.py" "$@"
if [[ $# == 2 && $1 != --plan ]]; then
    echo 'expand-dev-work-volume: mounted capacity and session continuity observed; record conservation remains unproven'
fi
