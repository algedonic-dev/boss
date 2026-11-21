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
# kernel_path e|m <path> — where the KERNEL reaches <path>, one component
# at a time: each name is asked of the kernel (`[ -L ]` is lstat(2)) on a
# prefix already resolved physically, a symlink's target from readlink is
# put back in front of what remains, and `..` climbs from the resolved
# prefix, never from the spelling — never `realpath`, whose -e/-m is
# GNU's and which a host may ship as one that collapses `..` as text
# first (backlog 9b74c84b, after restore-shared's 920f4fd5). `e` refuses
# a missing component, or a non-directory with more path after it; `m`
# spells a missing name as written and keeps walking, so a link AFTER a
# cancelled missing prefix (`gone/../link`) is still followed, as GNU -m
# follows it — the first text folded the missing prefix and appended the
# rest unresolved (review 5e3eb6f3). More than 40 links is a refusal in
# both modes: no answer exists, and a spelling would pass a guard. One
# text in every root verb that carries it, held to GNU's answers by
# gcp_root_paths_resolve_through_the_kernel.rs.
kernel_path() {
    local mode=$1 rest=$2 r c t more links=0
    [ -n "$rest" ] || return 1
    case $rest in
        /*) r= ;;
        *) r=$(pwd -P) || return 1; r=${r%/} ;;
    esac
    while [ -n "$rest" ]; do
        case $rest in
            */*) c=${rest%%/*}; rest=${rest#*/}; more=1 ;;
            *) c=$rest; rest=; more= ;;
        esac
        case $c in
            '' | .) continue ;;
            ..) r=${r%/*}; continue ;;
        esac
        if [ -L "$r/$c" ]; then
            links=$((links + 1))
            [ "$links" -le 40 ] || return 1
            t=$(readlink -- "$r/$c" && printf .) || return 1
            t=${t%.}; t=${t%$'\n'}
            [ -n "$t" ] || return 1
            case $t in /*) r= ;; esac
            rest=$t${more:+/$rest}
        elif [ "$mode" = m ]; then
            r=$r/$c
        elif [ -e "$r/$c" ] && { [ -z "$more" ] || [ -d "$r/$c" ]; }; then
            r=$r/$c
        else
            return 1
        fi
    done
    printf '%s\n' "${r:-/}"
}
safe() {
    local path=$1 owner bits child=
    [[ "$path" = /* ]] && [ "$(kernel_path m "$path")" = "$path" ] || refuse ambiguous-path
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
