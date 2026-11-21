#!/usr/bin/env bash
# Fixed forge GETs only; no packet-selected resource or executable.
set -uo pipefail
if [ "$#" -ne 0 ]; then
    echo 'read-stack-memory: REFUSED — no arguments accepted' >&2
    exit 78
fi
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
if ! ops_image_ready >&2; then
    echo 'read-stack-memory: CANNOT ANSWER — existing ops image unavailable' >&2
    exit 1
fi
STACK_MEMORY_KUBECTL="$(ops_kubectl)" || exit 1
export STACK_MEMORY_KUBECTL
exec python3 -B "$HERE/stack-memory.py"
