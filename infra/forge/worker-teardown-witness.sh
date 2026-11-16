#!/usr/bin/env bash
# Fixed read-only Kubernetes inventory; never a cleanup command.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=infra/lib/host-check.sh
. "$HERE/../lib/host-check.sh"
host_seam BOSS_OPS_DIR /etc/boss-ops
host_check forge worker-teardown-witness
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
ops_image_ready >&2 || exit 1
WORKER_TEARDOWN_KUBECTL="$(ops_kubectl)"
export WORKER_TEARDOWN_KUBECTL
exec python3 -B "$HERE/worker-teardown-witness.py" "$@"
