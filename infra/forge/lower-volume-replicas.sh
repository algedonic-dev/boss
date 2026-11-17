#!/usr/bin/env bash
# 723c0828: use the existing credential door and retirement engine. This
# wrapper grants no authority; the registered write requires a signed plan.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Begin the one absolute budget before image preparation, not at ticket admission.
STARTED="$(python3 -c 'import time; print(time.monotonic())')" || exit 1
. "$HERE/../lib/sor.sh"
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2 || exit 1
read -r -a K <<< "$(ops_kubectl --stdin)"
RESULT_ARGS=()
if [ "${1-}" = --effect-result ]; then
    RESULT_ARGS=(--effect-result "${2-}")
    shift 2
fi
exec python3 "$HERE/lower-volume-replicas.py" --started-at "$STARTED" "${RESULT_ARGS[@]}" --kubectl "${K[@]}" -- "$@"
