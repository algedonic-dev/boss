#!/usr/bin/env bash
# Protected bootstrap and unit preparation, after db018094/c98c79aa/fe6d1124.
# It writes only private prepared files. Its adoption caller separately fetches
# authenticated bytes and installs units. Never fetch or read a token here.
set -euo pipefail
umask 077
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
SOURCE=${BOSS_GCP_PREP_SOURCE:-$(cd "$HERE/.." && pwd -P)}
DEST=${BOSS_GCP_PREPARED_DIR:-/var/lib/boss/gcp-root-prepared}
TREE=${BOSS_GCP_ROOT_TREE:-/var/lib/boss/gcp-tree}
UID_NOW=$(id -u)
refuse() { echo "gcp-root-preparation: REFUSED: $*; nothing adopted" >&2; exit 78; }
case "${1:-}" in bootstrap|units) MODE=$1; shift ;; *) refuse 'expected bootstrap or units <accepted sha>' ;; esac
if [ "$MODE" = bootstrap ]; then [ "$#" = 0 ] || refuse arguments
else [ "$#" = 1 ] && [[ "$1" =~ ^[0-9a-f]{40}$ ]] || refuse 'units need an exact accepted sha'; fi
[[ "$DEST" = /* && "$SOURCE" = /* && "$TREE" = /* ]] || refuse 'paths must be absolute'
[ "$DEST" != /var/lib/boss/gcp-root-prepared ] || [ "$UID_NOW" = 0 ] || refuse 'reserved destination requires root'

# A private leaf below a replaceable parent does not establish provenance.
# The sticky root-owned temp exception exists only for owned fixture children.
trusted_path() {
    local path=$1 child= owner bits
    [[ "$path" = /* ]] && [ "$(realpath -m -- "$path")" = "$path" ] || refuse "ambiguous path: $path"
    while :; do
        [ ! -L "$path" ] || refuse "linked path: $path"
        if [ -e "$path" ]; then
            [ -d "$path" ] || refuse "not a directory: $path"
            owner=$(stat -c %u -- "$path") || refuse "cannot stat $path"
            bits=$(stat -c %a -- "$path") || refuse "cannot stat $path"
            [[ "$owner:$bits" =~ ^[0-9]+:[0-7]+$ ]] || refuse "invalid stat: $path"
            [ "$owner" = 0 ] || [ "$owner" = "$UID_NOW" ] || refuse "foreign owner: $path"
            if [ "$((8#$bits & 0022))" != 0 ]; then
                [ "$owner" = 0 ] && [ "$((8#$bits & 01000))" != 0 ] \
                    && [ -n "$child" ] && [ -d "$child" ] && [ ! -L "$child" ] \
                    || refuse "replaceable ancestor: $path"
            fi
        fi
        [ "$path" != / ] || break
        child=$path; path=$(dirname -- "$path")
    done
}
trusted_file() {
    local file=$1 owner bits links
    trusted_path "$(dirname -- "$file")"
    [ -f "$file" ] && [ ! -L "$file" ] || refuse "not a regular file: $file"
    owner=$(stat -c %u -- "$file") && bits=$(stat -c %a -- "$file") \
        && links=$(stat -c %h -- "$file") || refuse "cannot stat $file"
    [[ "$owner:$bits:$links" =~ ^[0-9]+:[0-7]+:[0-9]+$ ]] || refuse "invalid stat: $file"
    [ "$owner" = "$UID_NOW" ] && [ "$((8#$bits & 0022))" = 0 ] && [ "$links" = 1 ] \
        || refuse "untrusted file: $file"
}
lock_preparation() {
    install -d -m 0700 -- "$DEST"
    if [ -e "$DEST/.lock" ] || [ -L "$DEST/.lock" ]; then trusted_file "$DEST/.lock"; fi
    exec 9>>"$DEST/.lock"
    flock -x -w 30 9 || refuse 'preparation is still in use'
}

# Validate before copying, and again at the copied closure. No private file
# or operator checkout is an implicit source of root's bootstrap bytes.
trusted_path "$DEST"
trusted_path "$SOURCE"
# shellcheck source=infra/lib/host-check.sh
. "$HERE/../lib/host-check.sh"
host_seam BOSS_GCP_PREPARED_DIR /var/lib/boss/gcp-root-prepared
host_check boss-gcp gcp-root-preparation
FILES=(gcp/prepare-root-tree.sh gcp/root-tree.sh gcp/root-tree-credential.sh gcp/gcp-root-launch.sh lib/host-check.sh lib/sor.sh lib/sor-reader.sh estate/estate.toml)
if [ "$MODE" = bootstrap ]; then
    for file in "${FILES[@]}"; do trusted_file "$SOURCE/$file"; done
    templates=("$SOURCE/gcp/root-tree-units/"*.service.in)
    [ "${#templates[@]}" = 4 ] || refuse 'expected the four dormant unit templates'
    for file in "${templates[@]}"; do trusted_file "$file"; done
    lock_preparation
else
    [ -d "$DEST/bootstrap" ] || refuse 'bootstrap preparation is unavailable'
    lock_preparation
    for file in "${FILES[@]}"; do trusted_file "$DEST/bootstrap/infra/$file"; done
    trusted_path "$TREE"
    trusted_file "$TREE/.lock"
    exec 8<"$TREE/.lock"
    flock -s -w 30 8 || refuse 'accepted tree is being refreshed'
    # The helper proves marker/HEAD equality and refuses writable tree state.
    head=$(BOSS_GCP_ROOT_TREE="$TREE" bash "$DEST/bootstrap/infra/gcp/root-tree.sh" head) \
        || refuse 'accepted tree is unavailable'
    [ "$head" = "$1" ] || refuse 'accepted generation differs from requested sha'
    accepted=$(env -i PATH=/usr/bin:/bin HOME=/nonexistent LC_ALL=C \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 \
        git -c core.hooksPath=/dev/null --git-dir="$TREE/repo.git" rev-parse --verify refs/boss/accepted) \
        || refuse 'authenticated fetch acceptance is unavailable'
    [ "$accepted" = "$head" ] || refuse 'accepted fetch differs from current generation'
    templates=("$DEST/bootstrap/infra/gcp/root-tree-units/"*.service.in)
    [ "${#templates[@]}" = 4 ] || refuse 'prepared templates are unavailable'
    for file in "${templates[@]}"; do
        trusted_file "$file"
        generation_template="$TREE/gen/$head/infra/gcp/root-tree-units/$(basename -- "$file")"
        trusted_file "$generation_template"
        cmp -s -- "$file" "$generation_template" || refuse 'prepared template differs from accepted source'
    done
fi
stage=$(mktemp -d "$DEST/.staging.XXXXXXXX")
old=
cleanup() { [ -z "$stage" ] || rm -rf -- "$stage"; }
trap cleanup EXIT
if [ "$MODE" = bootstrap ]; then
    for file in "${FILES[@]}"; do
        install -d -m 0700 "$stage/infra/$(dirname -- "$file")"
        mode=0644; case "$file" in gcp/*.sh) mode=0755 ;; esac
        install -m "$mode" -- "$SOURCE/$file" "$stage/infra/$file"
    done
    install -d -m 0700 "$stage/infra/gcp/root-tree-units"
    for file in "${templates[@]}"; do install -m 0644 -- "$file" "$stage/infra/gcp/root-tree-units/$(basename -- "$file")"; done
else
    for file in "${templates[@]}"; do
        grep -x 'User=root' "$file" >/dev/null || refuse 'template does not run as root'
        grep -x 'WorkingDirectory=/var/lib/boss/gcp-tree/current' "$file" >/dev/null || refuse 'template does not name the owned tree'
        install -m 0644 -- "$file" "$stage/$(basename -- "${file%.in}")"
    done
    printf '%s\n' "$head" > "$stage/accepted-sha"
fi
target="$DEST/$MODE"
if [ -e "$target" ] || [ -L "$target" ]; then
    trusted_path "$target"
    old="$DEST/.previous.$MODE.$$"
    [ ! -e "$old" ] && [ ! -L "$old" ] || refuse 'previous preparation already exists'
    mv -T -- "$target" "$old"
fi
if ! mv -T -- "$stage" "$target"; then
    [ -z "$old" ] || mv -T -- "$old" "$target"
    refuse 'prepared publication failed'
fi
stage=
[ -z "$old" ] || rm -rf -- "$old"
printf 'gcp-root-preparation: %s prepared; no credentials, grants, current callers or live units changed\n' "$MODE"
