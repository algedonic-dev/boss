#!/usr/bin/env bash
# converge-hold hold <reason> | release | prepare — an operator's hand on
# the converge, as a file the runner reads before it rolls anything.
#
# While a hold stands, cluster-deploy-runner builds nothing and rolls
# nothing; it says so on every tick and exits 0 (a hold is a decision,
# not a failure). The ops verbs `hold-converge` / `release-converge`
# call this; the reason rides on the packet and in the file. `prepare`
# is install.sh's, run as root on every forge converge.
#
# EVERY VERDICT IS READ BACK, NEVER ASSUMED (backlog d94d287e,
# 2026-09-28). `hold` used to print HELD after a write whose status it
# never looked at, so an operator — or move-forgejo-data, which holds
# the converge around a data move — was told the converge was stopped
# when nothing had been written. HELD is printed only once the file
# reads back as the reason; `release` says released only once nothing
# stands at the path. Either failure is NOT HELD / NOT released, on
# stderr, exit 1.
set -uo pipefail
# HOLD_FILE — one definition for this verb, the converge and the
# database switch: infra/forge/forge-defaults.sh (which says why it is a
# fixed path, not $HOME, and why it is under /var/lib/boss).
. "$(dirname "$0")/forge-defaults.sh"
HOLD_DIR="$(dirname -- "$HOLD_FILE")"

# THE OLD PATH, named here and nowhere else in the tree: the hold lived
# in world-writable /var/tmp until d94d287e, and `prepare` carries a hold
# standing there across ONCE. The marker in the new directory records
# that the carry ran; after it nothing reads the old path, so a file any
# account plants there later holds nothing (converge_hold_sh.rs).
LEGACY_HOLD="${BOSS_CONVERGE_HOLD_LEGACY:-/var/tmp/boss-converge-hold}"
CARRIED_MARKER="$HOLD_DIR/.converge-hold.carried"

# hold_dir_problem — the reason HOLD_DIR cannot carry a hold, or nothing.
# A symlinked directory is somebody else's to repoint; a directory any
# account can write is the /var/tmp shape this file left.
hold_dir_problem() {
    if [ -L "$HOLD_DIR" ]; then
        echo "$HOLD_DIR is a symlink — the hold's directory must be a directory of its own"
    elif [ ! -d "$HOLD_DIR" ]; then
        echo "$HOLD_DIR does not exist (install.sh makes it, via converge-hold.sh prepare, as root)"
    elif [ -n "$(find "$HOLD_DIR" -maxdepth 0 -perm -0002 2>/dev/null)" ]; then
        echo "$HOLD_DIR is writable by any account ($(stat -c %A -- "$HOLD_DIR")) — anyone could place or replace the hold"
    fi
}

# carry_staged — make the thing staged at $STAGED the hold, and remove it.
# 0: carried, or dropped for a hold already standing; 1: not a regular
# file — removed, never followed, loud; 2: failed, left staged for the
# next tick to finish (review F2 of d94d287e).
#
# READ IN PLACE ONLY WHAT CANNOT BE SWAPPED (review F1 of d94d287e,
# reproduced as uid 0). A rename moves a HARDLINK as it stands, so a
# "hold" planted in /var/tmp as a hardlink to a secret used to be moved
# here, read — 300 bytes into the install output, the journal and the
# run summary — then chowned and made 0644: the secret world-readable.
# Only a file owned by the uid running prepare (root on the forge, which
# is who the ops runner wrote the hold as) with ONE link is this verb's
# own; its words are carried as they stand. Anything else is never read,
# chmodded or chowned: the hold is a FRESH root file saying so, because
# a stop someone placed is never dropped, and `rm -f` then removes only
# the planted link.
STAGED="$HOLD_DIR/.converge-hold.carry"
carry_staged() {
    local kind owner links was fresh err
    if [ -L "$STAGED" ] || [ ! -f "$STAGED" ]; then
        kind="$( [ -L "$STAGED" ] && echo "a symlink" || stat -c %F -- "$STAGED" 2>/dev/null)"
        rm -rf -- "$STAGED"
        echo "converge-hold: $LEGACY_HOLD was ${kind:-not a regular file}, not a hold this verb wrote — not followed, not carried, removed" >&2
        return 1
    fi
    owner="$(stat -c %u -- "$STAGED")"
    links="$(stat -c %h -- "$STAGED")"
    if [ "$owner" = "$(id -u)" ] && [ "$links" = 1 ]; then
        was="$(head -c 300 -- "$STAGED")"
        if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
            rm -f -- "$STAGED"
            echo "converge-hold: a hold already stands at $HOLD_FILE ($(head -c 300 -- "$HOLD_FILE" 2>/dev/null)); the one at $LEGACY_HOLD ($was) was dropped in its favour"
            return 0
        fi
        if ! err="$(chmod 0644 -- "$STAGED" 2>&1 && mv -T -- "$STAGED" "$HOLD_FILE" 2>&1)"; then
            echo "converge-hold: prepare REFUSED — could not carry the hold to $HOLD_FILE: $err (it stays at $STAGED; the next prepare finishes it)" >&2
            return 2
        fi
        echo "converge-hold: carried the hold standing at $LEGACY_HOLD (owned by uid $owner) to $HOLD_FILE — $was"
        return 0
    fi
    if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
        rm -f -- "$STAGED"
        echo "converge-hold: a hold already stands at $HOLD_FILE; the file at $LEGACY_HOLD (owned by uid $owner, $links link(s), contents not read) was dropped in its favour"
        return 0
    fi
    fresh="carried from $LEGACY_HOLD (owned by uid $owner, $links link(s)), contents not read — release-converge lifts it"
    if ! err="$( { printf '%s\n' "$fresh" > "$HOLD_FILE.new" && chmod 0644 -- "$HOLD_FILE.new" && mv -T -- "$HOLD_FILE.new" "$HOLD_FILE"; } 2>&1 )"; then
        rm -f -- "$HOLD_FILE.new"
        echo "converge-hold: prepare REFUSED — could not write a fresh hold at $HOLD_FILE: $err (the planted file stays at $STAGED; the next prepare finishes it)" >&2
        return 2
    fi
    rm -f -- "$STAGED"
    echo "converge-hold: HELD — $fresh ($HOLD_FILE)"
    return 0
}

case "${1:-}" in
    hold)
        reason="${2:?hold needs a reason}"
        problem="$(hold_dir_problem)"
        if [ -n "$problem" ]; then
            echo "converge-hold: NOT HELD — $HOLD_FILE cannot be written: $problem" >&2
            exit 1
        fi
        # Written 0644 whatever the caller's umask: the converge reads it
        # as david, and a hold it cannot read would once have passed as
        # no hold at all.
        if ! err="$( { printf '%s\n' "$reason" > "$HOLD_FILE" && chmod 0644 -- "$HOLD_FILE"; } 2>&1 )"; then
            echo "converge-hold: NOT HELD — could not write $HOLD_FILE: $err" >&2
            exit 1
        fi
        if [ "$(cat -- "$HOLD_FILE" 2>/dev/null)" != "$reason" ]; then
            echo "converge-hold: NOT HELD — $HOLD_FILE does not read back as '$reason'" >&2
            exit 1
        fi
        echo "converge-hold: HELD — $reason ($HOLD_FILE)"
        ;;
    release)
        if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
            was="$(cat -- "$HOLD_FILE" 2>/dev/null || echo "unreadable")"
            err="$(rm -f -- "$HOLD_FILE" 2>&1)"
            if [ -e "$HOLD_FILE" ] || [ -L "$HOLD_FILE" ]; then
                echo "converge-hold: NOT released — $HOLD_FILE still stands (was: $was)${err:+: $err}" >&2
                exit 1
            fi
            echo "converge-hold: released (was: $was)"
        else
            echo "converge-hold: no hold was standing"
        fi
        ;;
    prepare)
        # THE DIRECTORY, ROOT'S. Made 0755 (david reads the hold) and, as
        # root, root:root — on every converge, so a mode someone changed
        # by hand is put back on the next tick. Then verified, not
        # assumed: a symlink or a directory another account can write is
        # a refusal that names itself on the converge's packet.
        if [ -L "$HOLD_DIR" ]; then
            echo "converge-hold: prepare REFUSED — $HOLD_DIR is a symlink; the hold's directory must be a directory of its own" >&2
            exit 1
        fi
        if ! err="$(install -d -m 0755 -- "$HOLD_DIR" 2>&1 && chmod 0755 -- "$HOLD_DIR" 2>&1)"; then
            echo "converge-hold: prepare REFUSED — could not make $HOLD_DIR: $err" >&2
            exit 1
        fi
        if [ "$(id -u)" -eq 0 ] && ! err="$(chown root:root -- "$HOLD_DIR" 2>&1)"; then
            echo "converge-hold: prepare REFUSED — could not make $HOLD_DIR root's: $err" >&2
            exit 1
        fi
        owner="$(stat -c %u -- "$HOLD_DIR")"
        if [ "$owner" != "$(id -u)" ] || [ -n "$(find "$HOLD_DIR" -maxdepth 0 -perm /0022 2>/dev/null)" ]; then
            echo "converge-hold: prepare REFUSED — $HOLD_DIR is owned by uid $owner at $(stat -c %A -- "$HOLD_DIR"); it must be this uid's ($(id -u)) and writable by no one else" >&2
            exit 1
        fi

        # THE CARRY, ONCE. A hold standing at the old path is a human's
        # stop, and dropping it would let the converge roll under a data
        # move; carrying it on every tick would keep the old path a live
        # input any account can write. So: once, then the marker.
        if [ -e "$CARRIED_MARKER" ]; then
            if [ -e "$LEGACY_HOLD" ] || [ -L "$LEGACY_HOLD" ]; then
                echo "converge-hold: $LEGACY_HOLD exists but is not carried — the hold moved to $HOLD_FILE and the one carry ran ($(cat -- "$CARRIED_MARKER" 2>/dev/null)); nothing reads the old path"
            fi
            exit 0
        fi
        rc=0
        # A carry a failed tick left staged is finished FIRST: the old
        # path is empty by now, so without this the marker below would be
        # written and the staged hold would never become the hold.
        if [ -e "$STAGED" ] || [ -L "$STAGED" ]; then
            carry_staged || rc=$?
            [ "$rc" -ne 2 ] || exit 1
        fi
        if [ -e "$LEGACY_HOLD" ] || [ -L "$LEGACY_HOLD" ]; then
            # MOVED, never read where it lies: a rename does not follow a
            # symlink, and once the thing is inside a directory only this
            # uid can write, nobody can swap it before it is judged.
            if ! err="$(mv -T -- "$LEGACY_HOLD" "$STAGED" 2>&1)"; then
                echo "converge-hold: prepare REFUSED — could not take $LEGACY_HOLD: $err" >&2
                exit 1
            fi
            one=0
            carry_staged || one=$?
            [ "$one" -ne 2 ] || exit 1
            [ "$one" -eq 0 ] || rc="$one"
        fi
        date -u +%Y-%m-%dT%H:%M:%SZ > "$CARRIED_MARKER" || {
            echo "converge-hold: prepare could not record the carry at $CARRIED_MARKER — it will run again next tick" >&2
            exit 1
        }
        exit "$rc"
        ;;
    *) echo "usage: converge-hold.sh hold <reason> | release | prepare" >&2; exit 2 ;;
esac
