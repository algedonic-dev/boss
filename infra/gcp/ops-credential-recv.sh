#!/usr/bin/env bash
#
# ops-credential-recv — boss-gcp's half of the break-glass kubeconfig
# delivery (backlog 7336cb5f; transport decided on design 835c0c9c,
# the uid question on design b08725c2 Q3). The cluster's
# boss-break-glass-deposit CronJob assembles the kubeconfig, proves it
# by effect in both directions, and pipes it here over ssh; this takes
# the bytes on stdin and makes them /etc/boss-ops/kubeconfig, or
# refuses and leaves the file that is there alone.
#
#   ops-credential-recv kubeconfig < <bytes>
#
# HOW IT IS REACHED. Only as a forced command, through sudo, with its
# argument fixed twice: by the authorized_keys line David places for the
# deposit key (`command="exec sudo -n <this> kubeconfig",restrict`) and
# by the sudoers rule install-ops-credential-receiver.sh writes, which
# names the argument too. The caller's own command
# ($SSH_ORIGINAL_COMMAND) is never read. sudo's env_reset strips the
# environment, so the two knobs below are a test's and never the
# host's.
#
# WHAT IS REFUSED, AND WHY EACH: a deposit that can only have come from
# a broken or hostile transfer must never replace a working credential,
# because the file it would replace is the road back when the forge is
# down.
#   - EMPTY — a transfer that carried nothing (an ssh that died before
#     writing, a Job that piped an empty file).
#   - LARGER THAN 64 KiB — a kubeconfig for one ServiceAccount is under
#     4 KiB; anything larger is not one.
#   - NOT A KUBECONFIG — no `apiVersion: v1` / `kind: Config`, no https
#     `server:`, no embedded CA.
#   - TOKENLESS — a kubeconfig with no bearer token authenticates as
#     anonymous and fails later, somewhere less obvious.
#   - ANY KEY OUTSIDE THE ONE SHAPE the deposit Job writes. A kubeconfig
#     can carry an `exec:` credential plugin or an `auth-provider:`, and
#     either one RUNS A COMMAND as whoever next uses the file — root, on
#     this host (the watchdog, rollback-to). So the deposit is judged
#     against an allow-list of keys and plain scalar values, never a
#     deny-list: whoever holds the deposit key can write this file, and
#     the allow-list is what keeps that from being a root shell.
#
# WHAT IS PRINTED: byte counts, the path and its mode — never a value.
# The deposit Job compares the byte count with what it sent, so the
# receipt is read back rather than assumed.
#
# Exit: 0 installed, or unchanged; 65 the deposit was refused and the
# file left as it was; 78 a refusal of how it was invoked; 1 the write
# itself failed.
# Tracing off, first: an exported SHELLOPTS=xtrace would print every
# expansion, a deposited slot among them (review 9a1e289b, F8). sudo's
# env_reset already strips it on the host.
set +x
set -euo pipefail
# THE LOCALE IS THIS SCRIPT'S, NOT THE DEPOSITOR'S (review b6d2a716 of
# 7336cb5f, N6). sudo keeps LC_* and Debian's sshd accepts them from
# the client, so without this the character classes below judged in
# whatever locale the ssh client sent: under C.UTF-8 a line of only
# U+2028 counted as blank and was accepted (measured). Pinned to C, a
# byte is a byte.
export LC_ALL=C

ME=ops-credential-recv
# Knobs for the test harness only — sudo's env_reset removes both on
# the host, where the directory is /etc/boss-ops and the owner root.
DIR="${BOSS_OPS_DIR:-/etc/boss-ops}"
OWNER="${BOSS_RECV_OWNER-root:root}"
MAX_BYTES=65536

usage() { echo "$ME: REFUSED — $*" >&2; exit 78; }
refuse() { echo "$ME: REFUSED — $*; $DIR/$NAME left as it was" >&2; exit 65; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

# ---------------------------------------------------------------------
# THE SECOND PURPOSE: the estate machine token (design-doc bdc60b65,
# question gcp-push, David 2026-10-06; backlog 88379df3).
#
#   ops-credential-recv machine-token < three lines
#
# boss-gcp's shell callers already ask infra/lib/secret-header.sh for the
# token and find no file: this host can read no Secret, so the cluster's
# boss-machine-token-push CronJob pushes the three slots here every ten
# minutes, over ITS OWN key (machine-token-deposit-key, prepared by the
# broker and enrolled by a presence sign-off) forced to exactly this
# command. The break-glass key above cannot reach this branch and this
# key cannot reach that one: each authorized_keys line and each sudoers
# line names its argument.
#
# WHAT IS TAKEN FROM THE SENDER: three values, and nothing else. Not a
# path, not a slot name it may choose, not a mode. The deposit is exactly
#
#     current=<value>
#     next=<value or nothing>
#     previous=<value or nothing>
#
# in that order, each line ended by LF. A value is one base64url word of
# at most MAX_SLOT_BYTES — boss-core's bound and shape for a slot
# (machine_token.rs; machine_token_push_sh.rs holds the number equal) —
# and `current` may not be blank: a host whose callers read `current`
# gains nothing from a deposit without one. Everything is judged BEFORE
# anything is written, so a refused deposit changes no slot.
#
# WHERE IT GOES: /etc/boss/machine-token, the directory
# machine_token_header reads — root:root 0700, each slot 0600, each
# written to a temp file beside it and renamed, a blank slot removed.
# ROOT-ONLY, as on the forge: this host's units that run as another
# account do not read it and send unstamped, as they did before.
#
# WHAT IS PRINTED: first, before a byte is read, one fixed line saying
# this is the receiver and it is ready (below, where it is printed);
# then each slot's byte count and what was done. Never a value, never a
# suffix of one. The sender compares the three counts with what it sent.
recv_machine_token() {
    local dir owner max tmp bytes n line slot value want
    local -a slots=(current next previous) values=("" "" "")
    local wrote="" removed="" kept="" f staged stage
    # Knobs for the test harness only — sudo's env_reset removes them.
    dir="${BOSS_RECV_TOKEN_DIR:-/etc/boss/machine-token}"
    owner="${BOSS_RECV_OWNER-root:root}"
    # Three slots at the bound, their names and separators.
    max=$((3 * MAX_SLOT_BYTES + 64))
    refuse() { echo "$ME: REFUSED — $*; $dir left as it was" >&2; exit 65; }

    umask 077
    # STAGED ON TMPFS, AND A KILLED RUN'S LEFTOVER IS SWEPT (review
    # a92c0b94 F1: a transfer killed mid-way must not leave the token on
    # a disk). /run is root's and memory-backed; the staged bytes are
    # removed before any slot is written, a reboot clears what a SIGKILL
    # left, and the next deposit — at most ten minutes on — removes it
    # first. Never the host's shared /tmp.
    stage="${BOSS_RECV_STAGE_DIR:-/run}"
    rm -f -- "$stage"/.machine-token.recv.?????? 2>/dev/null || true
    tmp="$(mktemp "$stage/.machine-token.recv.XXXXXX")" || fail "cannot stage the deposit in $stage"
    # shellcheck disable=SC2064  # the path, expanded now
    trap "rm -f '$tmp'" EXIT
    # THIS SPEAKS FIRST (backlog dea2236f; packet 96a0f7bb, 2026-10-07).
    # On the first live enrolment the key's line was placed without its
    # command= prefix, so ssh ran the account's shell where this was
    # expected, and the push — which wrote before it read — put three
    # name=value lines on that shell's stdin. The sender now writes
    # nothing until it has read this exact line, which only this branch
    # prints and only once it is ready to take the deposit: a shell, a
    # banner, or silence is handed no byte. machine_token_push_sh.rs
    # holds the literal equal to the sender's, so it is spelled whole.
    echo 'ops-credential-recv: machine-token: ready for three slots'
    head -c "$((max + 1))" > "$tmp" || fail "could not read the deposit from stdin"
    bytes="$(stat -c %s "$tmp")"
    [ "$bytes" -gt 0 ] || refuse "an empty deposit — the transfer carried nothing"
    [ "$bytes" -le "$max" ] || refuse "a deposit over $max bytes is not three slots"
    if LC_ALL=C grep -q $'\r' "$tmp" || [ "$(LC_ALL=C tr -d '\000' < "$tmp" | wc -c)" -ne "$bytes" ]; then
        refuse "the deposit carries NUL or CR bytes — not the three plain lines the push writes"
    fi
    [ "$(tail -c 1 "$tmp" | od -An -c | tr -d ' ')" = '\n' ] \
        || refuse "the deposit does not end in a newline — a transfer cut short"
    n=0
    while IFS= read -r line; do
        n=$((n + 1))
        [ "$n" -le 3 ] || refuse "the deposit has more than three lines — one per slot, and no fourth"
        want="${slots[$((n - 1))]}"
        slot="${line%%=*}"
        [ "$line" != "$slot" ] && [ "$slot" = "$want" ] \
            || refuse "line $n is not '$want=<value>' — the slots arrive in one fixed order and the sender names no other"
        value="${line#*=}"
        if [ -n "$value" ]; then
            [ "${#value}" -le "$MAX_SLOT_BYTES" ] \
                || refuse "slot $want is ${#value} bytes, past the $MAX_SLOT_BYTES a slot may hold"
            [[ $value =~ ^[A-Za-z0-9_-]+$ ]] \
                || refuse "slot $want is not one base64url word (${#value} bytes)"
        fi
        values[n - 1]="$value"
    done < "$tmp"
    [ "$n" -eq 3 ] || refuse "the deposit has $n line(s), where it carries exactly three: current, next, previous"
    [ -n "${values[0]}" ] || refuse "the current slot is blank — a host's callers read current, and a deposit without one is not a token"
    rm -f "$tmp"
    trap - EXIT

    # Judged whole; only now is the host touched.
    if [ -L "$dir" ]; then
        refuse "$dir is a symlink, not the token's own directory"
    fi
    if [ -e "$dir" ] && [ ! -d "$dir" ]; then
        refuse "$dir exists and is not a directory"
    fi
    if [ ! -d "$dir" ]; then
        mkdir -p -m 0700 "$dir" || fail "cannot create $dir"
    fi
    chmod 0700 "$dir" || fail "cannot make $dir 0700"
    if [ -n "$owner" ]; then chown "$owner" "$dir" || fail "cannot make $dir $owner"; fi
    rm -f -- "$dir"/.current.?????? "$dir"/.next.?????? "$dir"/.previous.?????? 2>/dev/null || true
    for n in 0 1 2; do
        slot="${slots[n]}" value="${values[n]}" f="$dir/${slots[n]}"
        if [ -z "$value" ]; then
            if [ -e "$f" ] || [ -L "$f" ]; then
                rm -f -- "$f" || fail "cannot remove the blank slot $f"
                removed="$removed $slot"
            fi
        elif [ -f "$f" ] && [ ! -L "$f" ] && [ "$(cat -- "$f" 2>/dev/null)" = "$value" ]; then
            chmod 0600 "$f" || fail "cannot hold $f at 0600"
            if [ -n "$owner" ]; then chown "$owner" "$f" || fail "cannot hold $f at $owner"; fi
            kept="$kept $slot"
        else
            staged="$(mktemp "$dir/.$slot.XXXXXX")" || fail "cannot stage a file in $dir"
            printf '%s' "$value" > "$staged" || { rm -f "$staged"; fail "cannot write the staged $slot"; }
            sync -- "$staged" || { rm -f "$staged"; fail "cannot fsync the staged $slot"; }
            chmod 0600 "$staged" || { rm -f "$staged"; fail "cannot make the staged $slot 0600"; }
            if [ -n "$owner" ]; then
                chown "$owner" "$staged" || { rm -f "$staged"; fail "cannot make the staged $slot $owner"; }
            fi
            # -T: a link or directory planted at the slot is never written into.
            mv -fT "$staged" "$f" || { rm -f "$staged"; fail "cannot rename the deposit into $f"; }
            wrote="$wrote $slot"
        fi
    done
    sync -- "$dir" || fail "cannot fsync $dir"
    echo "$ME: machine-token: received current ${#values[0]} bytes, next ${#values[1]} bytes, previous ${#values[2]} bytes; $dir ${owner:-$(id -un):$(id -gn)} 0700, slots 0600 (wrote:${wrote:- none}; removed:${removed:- none}; unchanged:${kept:- none})"
    values=("" "" "")
    exit 0
}
# boss-core's MAX_SLOT_BYTES (crates/core/boss-core/src/machine_token.rs).
MAX_SLOT_BYTES=4096

[ "$#" -eq 1 ] || usage "takes exactly one argument, the credential's name (usage: $ME kubeconfig < bytes, or $ME machine-token < three lines)"
NAME="$1"
# TWO names, each its own key and its own sudoers line. boss-gcp's
# declared ops credential is the scoped kubeconfig alone
# (infra/estate/estate.toml [ops_credentials.boss-gcp]); a talosconfig
# must never reach the public edge, so there is no third name to take.
case "$NAME" in
    kubeconfig) ;;
    machine-token) recv_machine_token ;;
    *) usage "'$NAME' is not a credential this host receives — only kubeconfig, and machine-token over its own key" ;;
esac

umask 077
if [ ! -d "$DIR" ]; then
    mkdir -p -m 0700 "$DIR" || fail "cannot create $DIR"
fi
DEST="$DIR/$NAME"
TMP="$(mktemp "$DIR/.$NAME.recv.XXXXXX")" || fail "cannot stage a file in $DIR"
trap 'rm -f "$TMP"' EXIT

# One byte past the bound, so an oversize deposit is measurable without
# reading an unbounded stream into this host.
head -c "$((MAX_BYTES + 1))" > "$TMP" || fail "could not read the deposit from stdin"
BYTES="$(stat -c %s "$TMP")"

[ "$BYTES" -gt 0 ] || refuse "an empty deposit — the transfer carried nothing"
[ "$BYTES" -le "$MAX_BYTES" ] || refuse "a deposit over $MAX_BYTES bytes is not one ServiceAccount's kubeconfig"
# NUL or CR bytes are not in a file the deposit Job writes, and a line
# reader below would judge a different text than kubectl later parses.
if LC_ALL=C grep -q $'\r' "$TMP" || [ "$(LC_ALL=C tr -d '\000' < "$TMP" | wc -c)" -ne "$BYTES" ]; then
    refuse "the deposit carries NUL or CR bytes — not the plain text the deposit Job writes"
fi

# THE ALLOW-LIST. Every non-blank line is `<indent>[- ]<key>: [<value>]`
# where the key is one the deposit Job writes and the value, if any, is
# one plain scalar: no quotes, no flow collections, no anchors, aliases,
# tags or block scalars. Comments and a second document are refused with
# the rest — the Job writes neither.
ALLOWED=' apiVersion kind clusters cluster server certificate-authority-data name users user token contexts context namespace current-context '
line_re='^([ ]*)(- )?([a-z][a-zA-Z-]*):( (.*))?$'
value_re='^[A-Za-z0-9._:/=+@-]+$'
TOKENS=0 SERVERS=0 CAS=0
n=0
while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    # Blank is SPACES alone. A line of only a tab, \f or \v was blank
    # to [[:space:]] in any locale (review b6d2a716, N6); the Job never
    # writes a blank line at all, so nothing else gets the benefit.
    [ -n "${line// /}" ] || continue
    if ! [[ $line =~ $line_re ]]; then
        refuse "line $n is not one key and one plain value — the deposit Job writes nothing else"
    fi
    key="${BASH_REMATCH[3]}"
    value="${BASH_REMATCH[5]:-}"
    case "$ALLOWED" in
        *" $key "*) ;;
        *) refuse "line $n carries key '$key', which the deposit Job never writes (an exec: or auth-provider: would run a command as whoever uses this file)" ;;
    esac
    if [ -n "$value" ] && ! [[ $value =~ $value_re ]]; then
        refuse "line $n: the value of '$key' is not one plain scalar"
    fi
    case "$key" in
        token)
            [ "${#value}" -ge 20 ] || refuse "line $n: the token is empty or too short to be a ServiceAccount token"
            TOKENS=$((TOKENS + 1))
            ;;
        server)
            [[ $value == https://* ]] || refuse "line $n: the server is not an https:// address"
            SERVERS=$((SERVERS + 1))
            ;;
        certificate-authority-data)
            [ "${#value}" -ge 20 ] || refuse "line $n: the embedded CA is empty"
            CAS=$((CAS + 1))
            ;;
    esac
done < "$TMP"

grep -qx 'apiVersion: v1' "$TMP" || refuse "not a kubeconfig: no 'apiVersion: v1' line"
grep -qx 'kind: Config' "$TMP" || refuse "not a kubeconfig: no 'kind: Config' line"
[ "$CAS" -eq 1 ] || refuse "not a kubeconfig this host can trust: $CAS embedded CAs, where the deposit carries exactly one"
[ "$SERVERS" -eq 1 ] || refuse "a kubeconfig names $SERVERS servers; the deposit names exactly one"
[ "$TOKENS" -ge 1 ] || refuse "a TOKENLESS kubeconfig — it would authenticate as anonymous"
[ "$TOKENS" -eq 1 ] || refuse "a kubeconfig carrying $TOKENS tokens; the deposit carries exactly one"

if [ -f "$DEST" ] && [ ! -L "$DEST" ] && cmp -s "$TMP" "$DEST"; then
    echo "$ME: $NAME: received $BYTES bytes; unchanged — $DEST already holds them"
    exit 0
fi

WAS="absent"
if [ -L "$DEST" ]; then
    WAS="a symlink, replaced"
elif [ -f "$DEST" ]; then
    WAS="$(stat -c %s "$DEST") bytes, replaced"
fi
chmod 0600 "$TMP" || fail "cannot make the staged file 0600"
if [ -n "$OWNER" ]; then
    chown "$OWNER" "$TMP" || fail "cannot make the staged file $OWNER"
fi
# -T: a link planted at the destination is REPLACED, never followed or
# moved into. Same directory, so the rename is atomic: a reader sees the
# old file or the new one, never half of either.
mv -fT "$TMP" "$DEST" || fail "cannot rename the deposit into $DEST"
trap - EXIT
echo "$ME: $NAME: received $BYTES bytes; installed at $DEST, ${OWNER:-$(id -un):$(id -gn)} 0600 (was $WAS)"
