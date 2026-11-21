#!/usr/bin/env bash
# Scoped credential helper for the boss-gcp stage of a604a35b / c98c79aa.
# The broker declaration (db018094) reserves a read-only token for root's
# own fetch. The receiver installs its protected delivery copy; adoption uses
# it after the transport and credential lifecycles complete on the record.
# Git must use credential.useHttpPath=true and clear other helpers before
# configuring this one. It answers one repository, not every URL on the
# forge; store/erase neither read nor change the broker's file.
set -euo pipefail
[ "${1:-}" = get ] || exit 0
# shellcheck source=infra/lib/sor.sh
. "$(dirname "$0")/../lib/sor.sh"
sor_require BOSS_FORGE_URL
export LC_ALL=C
# Bash's ordinary read/mapfile silently erase NUL bytes (review 6685ed5f).
# Read once with NUL as the delimiter: success means NUL or the byte
# bound was reached, both refusals. Only EOF accepts the bounded bytes;
# the timeout refuses a client that never closes its request. Validate
# the SAME credential bytes, rather than checking and then rereading.
read_raw() { # <variable name>, stdin is the request or private file
    local status
    if IFS= read -r -d '' -n 4097 -t 5 "$1"; then
        echo 'root-tree-credential: NUL or oversized raw input refused' >&2; return 78
    else
        status=$?
        [ "$status" = 1 ] || { echo 'root-tree-credential: raw input read did not reach EOF' >&2; return 78; }
    fi
}
request=
read_raw request
case "$BOSS_FORGE_URL" in
    http://*) scheme=http; authority=${BOSS_FORGE_URL#http://} ;;
    https://*) scheme=https; authority=${BOSS_FORGE_URL#https://} ;;
    *) echo 'root-tree-credential: unsupported forge URL' >&2; exit 78 ;;
esac
[[ "$authority" =~ ^[a-zA-Z0-9.-]+(:[0-9]+)?$ ]] \
    || { echo 'root-tree-credential: forge URL is not an origin' >&2; exit 78; }
p= h= pa= user=
declare -A seen=()
while IFS= read -r line && [ -n "$line" ]; do
    key=${line%%=*}
    case "$key" in
        protocol|host|path|username)
            [ -z "${seen[$key]:-}" ] || exit 0
            seen[$key]=1
            case "$key" in
                protocol) p=${line#*=} ;; host) h=${line#*=} ;;
                path) pa=${line#*=} ;; username) user=${line#*=} ;;
            esac ;;
    esac
done <<< "$request"
[ "$p" = "$scheme" ] && [ "$h" = "$authority" ] \
    && [ "$pa" = david/boss.git ] && { [ -z "$user" ] || [ "$user" = david ]; } || exit 0

token=${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}
uid=$(id -u)
# Only the private fixture path substitutes the executing uid in tests;
# the host's reserved path is never read by a non-root process.
if [ "$token" = /etc/boss/root-tree/forge.token ] && [ "$uid" != 0 ]; then
    echo 'root-tree-credential: reserved token requires root' >&2; exit 78
fi
parent=$(dirname -- "$token")
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
refuse() { echo 'root-tree-credential: token must be an owned private regular file in an owned private directory, with one bounded credential line' >&2; exit 78; }
[[ "$token" = /* ]] && [ ! -L "$token" ] && [ -f "$token" ] \
    && [ "$(kernel_path e "$parent")" = "$parent" ] \
    && [ "$(stat -c %u -- "$parent")" = "$uid" ] \
    && [ "$(stat -c %a -- "$parent")" = 700 ] \
    && [ "$(stat -c %u -- "$token")" = "$uid" ] \
    && [ "$(stat -c %a -- "$token")" = 600 ] || refuse
size=$(stat -c %s -- "$token")
[[ "$size" =~ ^[0-9]+$ ]] && [ "$size" -gt 0 ] && [ "$size" -le 4096 ] || refuse
value=
read_raw value < "$token"
value=${value%$'\n'}
[[ "$value" =~ ^[a-zA-Z0-9_-]+$ ]] || refuse
printf 'username=david\npassword=%s\n\n' "$value"
