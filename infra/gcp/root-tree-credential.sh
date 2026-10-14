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
refuse() { echo 'root-tree-credential: token must be an owned private regular file in an owned private directory, with one bounded credential line' >&2; exit 78; }
[[ "$token" = /* ]] && [ ! -L "$token" ] && [ -f "$token" ] \
    && [ "$(realpath -e -- "$parent")" = "$parent" ] \
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
