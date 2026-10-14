#!/usr/bin/env bash
# Protected GCP generations, adopted after the recorded credential lifecycles.
# Unlike the forge's exported tree, these observers need native Git history.
# Each generation has its OWN HEAD/index and borrows only root's objects;
# advancing current never changes a reader's commit or working directory.
set -euo pipefail
umask 022
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
TREE=${BOSS_GCP_ROOT_TREE:-/var/lib/boss/gcp-tree}
UID_NOW=$(id -u)
stage_cleanup=
trap 'if [ -n "$stage_cleanup" ]; then rm -rf -- "$stage_cleanup"; fi' EXIT
refuse() { echo "gcp-root-tree: REFUSED: $*" >&2; exit 78; }
case "$TREE" in /*) ;; *) refuse 'tree must be absolute' ;; esac
case "${1:-}" in refresh|path|head|run) mode=$1; shift ;; *) echo 'usage: root-tree.sh refresh|path|head|run <infra/script.sh> [arguments]' >&2; exit 2 ;; esac
# All Git configuration and repository selection starts empty. In particular,
# an inherited insteadOf, askpass, object directory or GIT_CONFIG_COUNT must
# not replace the source root was authorised to read.
clean_git() {
    env -i PATH=/usr/bin:/bin LC_ALL=C HOME=/nonexistent \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 \
        BOSS_FORGE_URL="${BOSS_FORGE_URL:-}" BOSS_SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}" \
        BOSS_GCP_ROOT_TREE_TOKEN="${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}" \
        git -c core.hooksPath=/dev/null "$@"
}
owned_directory() {
    [ -d "$1" ] && [ ! -L "$1" ] && [ "$(realpath -e -- "$1")" = "$1" ] \
        && [ "$(stat -c %u -- "$1")" = "$UID_NOW" ] \
        && [ -z "$(find "$1" -maxdepth 0 -perm /022 -print)" ] \
        || refuse "not a private owned directory: $1"
}
check_tree() {
    owned_directory "$TREE"
    owned_directory "$TREE/gen"
    owned_directory "$TREE/repo.git"
    # Existing metadata and generations are not accepted by name alone.
    local bad
    bad=$(find "$TREE" ! -type l \( ! -uid "$UID_NOW" -o -perm /022 \) -print)
    [ -z "$bad" ] || refuse "tree contains writable or foreign-owned objects: $bad"
}
current_path() {
    local target sha path
    check_tree
    [ -L "$TREE/current" ] || refuse 'no accepted current generation'
    target=$(readlink -- "$TREE/current")
    sha=${target#gen/}
    [[ "$target" = gen/* && "$sha" =~ ^[0-9a-f]{40,64}$ ]] || refuse 'invalid current link'
    path="$TREE/gen/$sha"
    owned_directory "$path"
    [ "$(cat -- "$path/.boss-tree-sha")" = "$sha" ] || refuse 'generation marker differs'
    [ "$(clean_git -C "$path" rev-parse --verify HEAD)" = "$sha" ] || refuse 'generation HEAD differs'
    printf '%s\n' "$path"
}
lock_tree() {
    [ -f "$TREE/.lock" ] && [ ! -L "$TREE/.lock" ] || refuse 'lock is not an owned regular file'
    exec 9>>"$TREE/.lock"
    flock "$1" -w 120 9 || refuse 'tree is still in use; nothing replaced'
}
refresh() {
    if [ "$TREE" = /var/lib/boss/gcp-tree ]; then
        [ "$UID_NOW" = 0 ] || refuse 'reserved tree requires root'
    fi
    # shellcheck source=infra/lib/host-check.sh
    . "$HERE/../lib/host-check.sh"
    host_seam BOSS_GCP_ROOT_TREE /var/lib/boss/gcp-tree
    host_check boss-gcp gcp-root-tree
    # shellcheck source=infra/lib/sor.sh
    . "$HERE/../lib/sor.sh"
    sor_require BOSS_FORGE_URL
    [[ "$BOSS_FORGE_URL" =~ ^https?://[a-zA-Z0-9.-]+(:[0-9]+)?$ ]] || refuse 'forge URL must be an HTTP origin'
    local scheme authority auth have= got stage path entry old
    scheme=${BOSS_FORGE_URL%%:*}; authority=${BOSS_FORGE_URL#*://}
    # Require a valid scoped credential even if a server permits anonymous
    # reads. Its value is never printed, placed in argv, or persisted here.
    auth=$(printf 'protocol=%s\nhost=%s\npath=david/boss.git\n\n' "$scheme" "$authority" \
        | bash "$HERE/root-tree-credential.sh" get) || refuse 'scoped credential unavailable; no fetch'
    [ -n "$auth" ] || refuse 'scoped credential refused the exact repository'
    unset auth
    if [ ! -e "$TREE" ]; then
        owned_directory "$(dirname -- "$TREE")"
        install -d -m 0700 -- "$TREE" "$TREE/gen"
        clean_git init --bare "$TREE/repo.git"
        : >"$TREE/.lock"
    fi
    check_tree
    lock_tree -x
    if [ -e "$TREE/current" ] || [ -L "$TREE/current" ]; then
        path=$(current_path); have=$(basename -- "$path")
    fi
    # Only protected main over the declared origin. Clear all helper state;
    # the helper itself refuses requests for any other repository.
    timeout -k 5 300 env -i PATH=/usr/bin:/bin LC_ALL=C HOME=/nonexistent \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 \
        BOSS_FORGE_URL="$BOSS_FORGE_URL" BOSS_SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}" \
        BOSS_GCP_ROOT_TREE_TOKEN="${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}" \
        git -c core.hooksPath=/dev/null -c credential.helper= \
        -c "credential.helper=$HERE/root-tree-credential.sh" -c credential.useHttpPath=true \
        -c http.followRedirects=false -c fetch.fsckObjects=true -c transfer.fsckObjects=true \
        --git-dir="$TREE/repo.git" fetch --no-tags -- "$BOSS_FORGE_URL/david/boss.git" refs/heads/main \
        || { echo 'gcp-root-tree: fetch failed; accepted generation unchanged' >&2; return 1; }
    got=$(clean_git --git-dir="$TREE/repo.git" rev-parse --verify 'FETCH_HEAD^{commit}')
    [[ "$got" =~ ^[0-9a-f]{40,64}$ ]] || refuse 'fetch did not name a commit'
    if [ -n "$have" ] && [ "$have" != "$got" ]; then
        if clean_git --git-dir="$TREE/repo.git" merge-base --is-ancestor "$have" "$got"; then :; else
            local status=$?
            [ "$status" = 1 ] || { echo 'gcp-root-tree: ancestry unreadable; accepted generation unchanged' >&2; return 1; }
            echo 'gcp-root-tree: REFUSED: main rewound; accepted generation unchanged' >&2; return 3
        fi
    fi
    [ "$have" != "$got" ] || { printf 'gcp-root-tree: already at %s\n' "$got"; return; }
    stage=$(mktemp -d "$TREE/gen/.staging.XXXXXXXX")
    stage_cleanup=$stage
    # Stage failures retain their diagnostics but never enter current.
    clean_git init "$stage"
    printf '%s\n' "$TREE/repo.git/objects" >"$stage/.git/objects/info/alternates"
    clean_git -C "$stage" checkout --detach "$got"
    # The four dormant units invoke this helper inside current too (135cd8da F1).
    # A generation without it must leave the last accepted entrypoint intact.
    for entry in infra/gcp/root-tree.sh infra/gcp/boss-gcp-converge.sh infra/gcp/adopt-root-tree.sh \
        infra/gcp/prepare-root-tree.sh infra/gcp/install-units.sh infra/gcp/root-tree-status.sh infra/ops/ops-runner.sh \
        infra/codebase-metrics.sh infra/protocol-drift.sh infra/surface-usage.sh \
        infra/ml/run-inference-batch.sh infra/boss-maintenance-wrap.sh infra/boss-step.sh; do
        [ -f "$stage/$entry" ] && [ -x "$stage/$entry" ] \
            && [ "$(realpath -e -- "$stage/$entry")" = "$stage/$entry" ] \
            || refuse "generation lacks a regular executable $entry; accepted tree unchanged"
    done
    [ ! -e "$stage/.boss-tree-sha" ] && [ ! -L "$stage/.boss-tree-sha" ] \
        || refuse 'tracked generation marker refused; helper metadata must be new'
    printf '%s\n' "$got" >"$stage/.boss-tree-sha"
    path="$TREE/gen/$got"
    [ ! -e "$path" ] && [ ! -L "$path" ] || refuse 'unaccepted generation already exists; operator investigation required'
    mv -T -- "$stage" "$path"
    stage_cleanup=
    if [ -n "$have" ]; then
        ln -s -- "gen/$have" "$TREE/.previous.$$"
        mv -Tf -- "$TREE/.previous.$$" "$TREE/previous"
    fi
    ln -s -- "gen/$got" "$TREE/.current.$$"
    mv -Tf -- "$TREE/.current.$$" "$TREE/current"
    clean_git --git-dir="$TREE/repo.git" update-ref refs/boss/accepted "$got"
    # run holds a shared lock until its interpreter exits. Under this
    # exclusive lock no reader can still be using a discarded generation.
    for old in "$TREE/gen"/*; do
        [ -d "$old" ] || continue
        case "$(basename -- "$old")" in "$got"|"$have") continue ;; esac
        rm -rf -- "$old"
    done
    printf 'gcp-root-tree: current is %s\n' "$got"
}
case "$mode" in
    refresh) [ "$#" = 0 ] || refuse 'refresh accepts no source override'; refresh ;;
    path) current_path ;;
    head) path=$(current_path); basename -- "$path" ;;
    run)
        check_tree
        lock_tree -s
        path=$(current_path)
        script=${1:-}; [ "$#" -gt 0 ] && shift
        [[ "$script" =~ ^infra/[A-Za-z0-9_./-]+\.sh$ ]] || refuse 'run needs a relative infra shell script'
        case "/$script/" in */../*|*/./*) refuse 'relative traversal refused' ;; esac
        [ -f "$path/$script" ] && [ -x "$path/$script" ] \
            && [ "$(realpath -e -- "$path/$script")" = "$path/$script" ] || refuse 'script is not a regular generation executable'
        cd "$path"
        # The owning observer's Git reads must use the same generation,
        # rather than any repository/configuration inherited by the caller.
        while IFS= read -r name; do
            case "$name" in GIT_*) unset "$name" ;; esac
        done < <(compgen -e)
        export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
        exec bash -- "$path/$script" "$@"
        ;;
esac
