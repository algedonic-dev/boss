#!/usr/bin/env bash
# One fixed database target; the runner requires David's passkey on the plan.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "$#" -ne 1 ]; then
    echo 'orphan-backends: REFUSED — fixed plan or approved hash required' >&2
    exit 78
fi
case "$1" in
    --plan) ;;
    *) if [[ ! "$1" =~ ^[0-9a-f]{64}$ ]]; then
           echo 'orphan-backends: REFUSED — approved plan hash required' >&2
           exit 78
       fi ;;
esac
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2 || exit 1
ORPHAN_BACKENDS_KUBECTL="$(ops_kubectl)" || exit 1
export ORPHAN_BACKENDS_KUBECTL
python3 -B "$HERE/orphan-backends.py" "$1"
if [ "$1" != --plan ]; then
    echo 'orphan-backends: approved original client sessions absent — effect read back'
fi
