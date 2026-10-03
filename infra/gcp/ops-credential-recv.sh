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

[ "$#" -eq 1 ] || usage "takes exactly one argument, the credential's name (usage: $ME kubeconfig < bytes)"
NAME="$1"
# ONE name. boss-gcp's declared set is the scoped kubeconfig alone
# (infra/estate/estate.toml [ops_credentials.boss-gcp]); a talosconfig
# must never reach the public edge, so there is no second name to take.
[ "$NAME" = kubeconfig ] || usage "'$NAME' is not a credential this host receives — only kubeconfig"

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
