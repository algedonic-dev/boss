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
# Validate the entire path before executing the bootstrap helper.
path=$helper; uid=$(id -u); child=
while :; do
    [ -e "$path" ] && [ ! -L "$path" ] && [ "$(realpath -m -- "$path")" = "$path" ] || unknown linked-or-missing-bootstrap
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
    properties=$(timeout -k 1 3 "$SYSTEMCTL" show "$unit.service" --no-pager --property=User,ExecStart,LoadState,ActiveState,FragmentPath) || unknown "unit-read-$unit"
    user=$(sed -n 's/^User=//p' <<< "$properties")
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
    printf 'unit=%s user=%s load=%s active=%s executable=%s verdict=%s\n' "$unit" "$user" "$loaded" "$active" "$executable" "$result"
done
# Inactive oneshot services are normal. This proves installed callers, not that
# an observer completed successfully; its native maintenance packet is separate.
exit "$verdict"
