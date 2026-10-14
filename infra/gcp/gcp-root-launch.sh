#!/usr/bin/env bash
# Root-owned recovery entrypoint, outside generations. Refresh can bring in
# a fix even when the current generation's converge cannot run. No legacy arm.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
PREP=${BOSS_GCP_PREPARED_DIR:-/var/lib/boss/gcp-root-prepared}
TREE=${BOSS_GCP_ROOT_TREE:-/var/lib/boss/gcp-tree}
case "${1:-}" in converge|run) mode=$1; shift ;; *) exit 64 ;; esac
# This recovery arm runs before a generation's summary library is available.
# Match run_summary_reset's best-effort removal before ANY converge refusal;
# the owning control holds the two entrypoints to the same behavior. The run
# arm must preserve the summary for ExecStopPost to consume.
if [ "$mode" = converge ] && [ -n "${BOSS_RUN_SUMMARY_FILE:-}" ]; then
    rm -f -- "$BOSS_RUN_SUMMARY_FILE" 2>/dev/null || true
fi
refuse() { echo "gcp-root-launch: REFUSED: $*; no checkout fallback" >&2; exit 78; }
uid=$(id -u)
safe() {
    local path=$1 owner bits child=
    [[ "$path" = /* ]] && [ "$(realpath -m -- "$path")" = "$path" ] || refuse ambiguous-path
    while :; do
        [ -e "$path" ] && [ ! -L "$path" ] || refuse missing-or-linked-path
        owner=$(stat -c %u "$path"); bits=$(stat -c %a "$path")
        [ "$owner" = 0 ] || [ "$owner" = "$uid" ] || refuse foreign-owner
        if [ "$((8#$bits & 0022))" != 0 ]; then
            [ "$owner" = 0 ] && [ "$((8#$bits & 01000))" != 0 ] && [ -n "$child" ] || refuse writable-path
        fi
        [ "$path" != / ] || break
        child=$path; path=$(dirname -- "$path")
    done
}
helper="$PREP/bootstrap/infra/gcp/root-tree.sh"
safe "$helper"
[ -f "$helper" ] && [ "$(stat -c %u "$helper")" = "$uid" ] || refuse untrusted-helper
export BOSS_GCP_ROOT_TREE="$TREE"
if [ "$mode" = converge ]; then
    [ "$#" = 0 ] || refuse arguments
    bash "$helper" refresh
    safe "$PREP/bootstrap/deposit-user"
    deposit_user=$(cat "$PREP/bootstrap/deposit-user")
    [[ "$deposit_user" =~ ^[a-z_][a-z0-9_-]*$ ]] && [ "$deposit_user" != root ] || refuse deposit-account
    export BOSS_GCP_DEPOSIT_USER="$deposit_user"
    exec bash "$helper" run infra/gcp/boss-gcp-converge.sh --root-generation
fi
# The tree door validates the relative script and holds its generation lock.
exec bash "$helper" run "$@"
