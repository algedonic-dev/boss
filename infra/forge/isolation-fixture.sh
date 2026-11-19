#!/usr/bin/env bash
# a21 R1's fixed purpose on the existing forge ops executor. Source
# publication grants no rights and never activates worker isolation.
set -euo pipefail
umask 077
fixture_effect() {
    case "$1" in
        prepare) printf 'isolation-fixture: READY original-request %s native UID binding and synthetic counter readback verified\n' "$2" ;;
        retire) printf 'isolation-fixture: TEARDOWN original-request %s original UID absence and evidence readback verified\n' "$3" ;;
    esac
}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
[ "$#" -ge 1 ] || { echo 'isolation-fixture: REFUSED missing fixed phase' >&2; exit 78; }
PHASE="$1"
shift
# Pure syntax validation precedes host authority and all private readers.
python3 -I -B "$HERE/isolation-fixture-host.py" validate "$PHASE" "${OPS_REQUEST_ID:-}" "$@"
# These operations have no production-authority test override. Owning tests
# construct explicit native ports and temporary journals instead.
for name in BOSS_FIXTURE_STATE BOSS_FIXTURE_CLI BOSS_FIXTURE_INVOCATIONS; do
    [ -z "${!name+x}" ] || { echo 'isolation-fixture: REFUSED authority path override' >&2; exit 78; }
done
# shellcheck source=infra/lib/host-check.sh
. "$HERE/../lib/host-check.sh"
host_seam BOSS_OPS_DIR /etc/boss-ops
host_seam BOSS_FIXTURE_STATE /var/lib/boss/isolation-fixture
host_seam BOSS_FIXTURE_CLI /opt/boss-cli
host_seam BOSS_FIXTURE_INVOCATIONS /var/lib/boss/isolation-fixture-invocations
host_check forge isolation-fixture
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
sor_require BOSS_JOBS_URL
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# shellcheck source=infra/forge/forge-defaults.sh
. "$HERE/forge-defaults.sh"
forge_need REGISTRY
# shellcheck source=infra/lib/secret-header.sh
. "$HERE/../lib/secret-header.sh"
machine_token_header FIXTURE_MT_HDR "$BOSS_JOBS_URL"
[ -n "$FIXTURE_MT_HDR" ] || { echo 'isolation-fixture: REFUSED missing existing machine read authority' >&2; exit 78; }
SOURCE="$(cd "$HERE/../.." && pwd -P)"
# A command substitution yields only the existing fixed Docker/kubectl
# invocation, never a token value or caller-supplied command.
K="$(ops_kubectl)"
KI="$(ops_kubectl --stdin)"
python3 -I -B "$HERE/isolation-fixture-host.py" "$PHASE" "$SOURCE" "$K" "$KI" \
    "$FIXTURE_MT_HDR" "$BOSS_JOBS_URL" "$REGISTRY" "$OPS_REQUEST_ID" "$@"
# set -e prevents any effect line when the complete engine readback refuses.
fixture_effect "$PHASE" "$OPS_REQUEST_ID" "${1:-}"
