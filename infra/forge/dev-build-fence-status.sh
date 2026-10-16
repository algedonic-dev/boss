#!/usr/bin/env bash
# READ-ONLY fixed worker-fence observation. Client serialization creates
# no server object; installed spec/readiness does not prove packet effects.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
[ "$#" -eq 0 ] || { echo 'dev-build-fence-status: REFUSED arguments' >&2; exit 78; }
# shellcheck source=infra/lib/host-check.sh
. "$HERE/../lib/host-check.sh"
host_seam BOSS_OPS_DIR /etc/boss-ops
host_seam BOSS_FENCE_SOURCE
host_check forge dev-build-fence-status
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
SOURCE="${BOSS_FENCE_SOURCE:-$(cd "$HERE/../.." && pwd -P)}"
python3 "$HERE/dev-build-fence-status.py" guard "$SOURCE" "$(ops_kubeconfig)" "${BOSS_FENCE_SOURCE:+fixture}"
WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
python3 "$SOURCE/infra/cluster/render-dev-build-policy.py" \
    "$SOURCE/infra/cluster/manifests/boss-dev-build-egress.yaml" \
    "$SOURCE/infra/estate/estate.toml" "$WORK/expected.yaml"
K="$(ops_kubectl)"
failed=0
read_record() {
    local label="$1"; shift
    local rc=0
    local reader="$K"
    [ "$label" != expected ] || reader="$(ops_kubectl --stdin)"
    # K is the existing fixed ops door's deliberately word-split command.
    # shellcheck disable=SC2086
    $reader "$@" > "$WORK/$label.json" 2> "$WORK/$label.stderr" || rc=$?
    echo "=== $label raw stdout (exit $rc) ==="
    cat "$WORK/$label.json"
    echo
    echo "=== $label raw stderr ==="
    cat "$WORK/$label.stderr"
    sha256sum "$WORK/$label.json" "$WORK/$label.stderr"
    [ "$rc" -eq 0 ] || failed=1
}
echo '=== trusted estate-rendered expected YAML ==='
cat "$WORK/expected.yaml"
sha256sum "$WORK/expected.yaml"
# This is LOCAL serialization, fixed client dry-run and validation OFF.
# Discovery may still refuse; that refusal is retained and fails closed.
read_record expected create --dry-run=client --validate=false -f - -o json --request-timeout=20s < "$WORK/expected.yaml"
read_record actual get ciliumnetworkpolicy.cilium.io boss-dev-build-egress -n boss-dev -o json --request-timeout=20s
read_record cilium get daemonset cilium -n kube-system -o json --request-timeout=20s
[ "$failed" -eq 0 ] || { echo 'dev-build-fence-status: UNVERIFIED unreadable/discovery refusal'; exit 1; }
python3 "$HERE/dev-build-fence-status.py" judge "$WORK"
