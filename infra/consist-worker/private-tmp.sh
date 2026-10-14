#!/usr/bin/env bash
# Fixed trusted bootstrap library. A worker mounts the child itself:
# the kubelet-owned, group-writable emptyDir root is not its /tmp.
# No root, chmod of shared ancestors, or candidate code is required.
consist_tmp_refuse() {
    printf 'consist-private-tmp: refused: %s\n' "$1" >&2
    return 78
}
prepare_consist_tmp() {
    [ "$#" -eq 2 ] || { consist_tmp_refuse 'expected volume and child'; return 78; }
    local volume="$1" child="$2" path uid
    case "$child" in ''|.|..|*[!a-zA-Z0-9_-]*) consist_tmp_refuse 'invalid child name'; return 78 ;; esac
    [ "${#child}" -le 64 ] || { consist_tmp_refuse 'child name too long'; return 78; }
    [ -d "$volume" ] && [ ! -L "$volume" ] || { consist_tmp_refuse 'volume is not an unlinked directory'; return 78; }
    path="$volume/$child"
    [ ! -e "$path" ] && [ ! -L "$path" ] || { consist_tmp_refuse 'child already exists'; return 78; }
    uid=$(id -u) || { consist_tmp_refuse 'current uid unreadable'; return 78; }
    (umask 077; mkdir -m 0700 -- "$path") || { consist_tmp_refuse 'child creation failed'; return 78; }
    [ -d "$path" ] && [ ! -L "$path" ] || { consist_tmp_refuse 'created child is not an unlinked directory'; return 78; }
    [ "$(stat -c %u -- "$path")" = "$uid" ] || { consist_tmp_refuse 'created child owner differs'; return 78; }
    # Kubernetes fsGroup roots carry SGID, which mkdir inherits despite
    # -m0700 (138676ab). GNU chmod also preserves directory SGID with
    # four digits; the explicit fifth digit clears special bits only on
    # this new verified owned child, never its volume ancestor.
    chmod 00700 -- "$path" || { consist_tmp_refuse 'child mode normalization failed'; return 78; }
    [ -d "$path" ] && [ ! -L "$path" ] || { consist_tmp_refuse 'normalized child is not an unlinked directory'; return 78; }
    [ "$(stat -c %u -- "$path")" = "$uid" ] || { consist_tmp_refuse 'normalized child owner differs'; return 78; }
    [ "$(stat -c %a -- "$path")" = 700 ] || { consist_tmp_refuse 'child mode is not private'; return 78; }
}
