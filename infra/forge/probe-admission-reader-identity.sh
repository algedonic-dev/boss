#!/usr/bin/env bash
# Fixed READ: installed client/image/method facts, no credentials or server.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
unavailable() {
    printf '{"schema":"boss.admission-reader-identity.v1","scope":"installed_reader_identity","state":"unavailable","history_verdict":"unavailable","reason":"%s"}\n' "$1"
    exit 4
}
[ "$#" -eq 0 ] || unavailable unexpected_arguments
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh" 2>/dev/null || unavailable operator_configuration_unavailable
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh" 2>/dev/null || unavailable operator_configuration_unavailable
BOSS_READER_IMAGE="$(ops_image 2>/dev/null)" || unavailable operator_image_unavailable
export BOSS_READER_IMAGE
# Do not call ops_image_ready: this read never pulls, creates a pin or prunes.
report="$(timeout -k 2 40 python3 -B "$HERE/admission-identity-bootstrap.py" 2>/dev/null)"
status=$?
case "$status" in
    0 | 4) [ -n "$report" ] || unavailable identity_runtime_unavailable ;;
    124 | 137) unavailable identity_deadline ;;
    *) unavailable identity_runtime_unavailable ;;
esac
printf '%s\n' "$report"
exit "$status"
