#!/usr/bin/env bash
# Read-only acceptance proof: protected generation and the six adopted callers.
# Never reads a credential or prints a command's arguments. Absence is unknown.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
TREE=${BOSS_GCP_ROOT_TREE:-/var/lib/boss/gcp-tree}
PREP=${BOSS_GCP_PREPARED_DIR:-/var/lib/boss/gcp-root-prepared}
SYSTEMCTL=${BOSS_GCP_STATUS_SYSTEMCTL:-systemctl}
unknown() { echo "gcp-root-status: UNVERIFIED: $*" >&2; exit 78; }
[ "$#" = 0 ] || unknown arguments
. "$HERE/../lib/host-check.sh"
host_seam BOSS_GCP_ROOT_TREE /var/lib/boss/gcp-tree
host_seam BOSS_GCP_PREPARED_DIR /var/lib/boss/gcp-root-prepared
host_seam BOSS_GCP_STATUS_SYSTEMCTL systemctl
host_check boss-gcp gcp-root-status
helper="$PREP/bootstrap/infra/gcp/root-tree.sh"
[ -f "$helper" ] && [ ! -L "$helper" ] || unknown protected-bootstrap-unavailable
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
# Validate the entire path before executing the bootstrap helper.
path=$helper; uid=$(id -u); child=
while :; do
    [ -e "$path" ] && [ ! -L "$path" ] && [ "$(kernel_path m "$path")" = "$path" ] || unknown linked-or-missing-bootstrap
    owner=$(stat -c %u "$path"); bits=$(stat -c %a "$path")
    [ "$owner" = 0 ] || [ "$owner" = "$uid" ] || unknown foreign-bootstrap-owner
    if [ "$((8#$bits & 0022))" != 0 ]; then
        [ "$owner" = 0 ] && [ "$((8#$bits & 01000))" != 0 ] && [ -n "$child" ] && [ -d "$child" ] || unknown writable-bootstrap
    fi
    [ "$path" != / ] || break
    child=$path; path=$(dirname -- "$path")
done
[ -f "$TREE/.lock" ] && [ ! -L "$TREE/.lock" ] || unknown accepted-tree-lock-unavailable
exec 9<"$TREE/.lock"
flock -s -w 3 9 || unknown accepted-tree-being-refreshed
head=$(BOSS_GCP_ROOT_TREE="$TREE" bash "$helper" head) || unknown accepted-generation-unavailable
[[ "$head" =~ ^[0-9a-f]{40}$ ]] || unknown invalid-generation
accepted=$(env -i PATH=/usr/bin:/bin HOME=/nonexistent LC_ALL=C GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    git -c core.hooksPath=/dev/null --git-dir="$TREE/repo.git" rev-parse --verify refs/boss/accepted) || unknown accepted-ref-unavailable
[ "$head" = "$accepted" ] || unknown generation-and-accepted-ref-differ
printf 'generation=%s accepted=%s tree_uid=%s tree_mode=%s\n' "$head" "$accepted" "$(stat -c %u "$TREE")" "$(stat -c %a "$TREE")"
verdict=0
for unit in boss-gcp-converge boss-ops-runner boss-codebase-metrics boss-protocol-drift boss-surface-usage boss-ml-inference-batch; do
    properties=$(timeout -k 1 3 "$SYSTEMCTL" --system show "$unit.service" --all --no-pager --property=User,DynamicUser,ExecStart,LoadState,ActiveState,FragmentPath) || unknown "unit-read-$unit"
    # systemd.exec: an unset User is root for system services, unless
    # DynamicUser allocates an identity. --all retains the empty property;
    # an absent or repeated field is an unreadable answer, never a default.
    user_count=$(awk '/^User=/ { n++ } END { print n+0 }' <<< "$properties")
    [ "$user_count" = 1 ] || unknown "unreadable-user-$unit"
    user=$(sed -n 's/^User=//p' <<< "$properties")
    dynamic=$(sed -n 's/^DynamicUser=//p' <<< "$properties")
    [ "$dynamic" = no ] || unknown "unverified-dynamic-user-$unit"
    user_source=explicit
    if [ -z "$user" ]; then user=root; user_source=system-default; fi
    loaded=$(sed -n 's/^LoadState=//p' <<< "$properties")
    active=$(sed -n 's/^ActiveState=//p' <<< "$properties")
    # Print only the executable, never argv or environment values.
    executable=$(sed -n 's/^ExecStart={[[:space:]]*path=\([^ ;]*\).*/\1/p' <<< "$properties")
    [[ "$user" =~ ^[a-z_][a-z0-9_-]*$ && "$loaded" =~ ^[a-z-]+$ && "$active" =~ ^[a-z-]+$ && "$executable" =~ ^/[A-Za-z0-9_./-]+$ ]] || unknown "unreadable-unit-$unit"
    case "$unit" in
        boss-gcp-converge|boss-ops-runner) expected="$PREP/bootstrap/infra/gcp/gcp-root-launch.sh" ;;
        *) expected="$TREE/current/infra/gcp/root-tree.sh" ;;
    esac
    result=adopted
    if [ "$user" != root ] || [ "$loaded" != loaded ] || [ "$executable" != "$expected" ]; then result=unadopted; verdict=1; fi
    printf 'unit=%s user=%s user_source=%s load=%s active=%s executable=%s verdict=%s\n' "$unit" "$user" "$user_source" "$loaded" "$active" "$executable" "$result"
done
# Inactive oneshot services are normal. This proves installed callers, not that
# an observer completed successfully; its native maintenance packet is separate.
exit "$verdict"
