#!/usr/bin/env bash
#
# machine-token-push — the cluster's half of delivering the estate
# machine token to boss-gcp (design-doc bdc60b65, question gcp-push,
# David 2026-10-06; backlog 88379df3, part of urgent 2710c8fc). Runs as
# the boss-machine-token-push CronJob (infra/cluster/manifests/
# boss-machine-token-push.yaml), in the boss image, as uid 1500, every
# ten minutes.
#
# WHY A PUSH, AND WHY ITS OWN JOB. boss-gcp is the one estate host that
# can read no Secret: its only cluster credential is the break-glass
# kubeconfig, whose deposit proves "cannot get secrets" before it ships.
# So the token is pushed, as that kubeconfig is. But NOT by that Job and
# NOT on its key: the break-glass deposit runs once a day, which equals
# the token's 1440-minute drain and leaves a rotation no margin; and its
# key is forced to `ops-credential-recv kubeconfig`, which this tree
# refuses to widen. This Job holds a key of its own, forced to the
# receiver's `machine-token` purpose and to nothing else.
#
# ONE PASS:
#   1. THE KEY, mounted from Secret machine-token-deposit-key at
#      $BOSS_PUSH_KEY_DIR. The credential broker prepares it
#      (rule broker-prepares-the-machine-token-deposit-key, protocol
#      prepare-the-machine-token-deposit-key) and this Job never mints one: a key nobody
#      authorized is a key nobody can enroll. NO KEY is `not yet`, and
#      the line says which packet to file.
#   2. THE TOKEN, the three slots of Secret boss-machine-token as the
#      kubelet mounts them at $BOSS_MACHINE_TOKEN_DIR — the same mount
#      this Job's own chore pair stamps its packet writes from. Each slot
#      is judged as boss-core judges one (one base64url word, at most
#      MAX_SLOT_BYTES). No `current` is `not yet`: the broker has not
#      minted. A malformed slot is a refusal, and nothing is sent.
#   3. SHIP, IN TWO STEPS: connect to the forced-command receiver
#      (infra/gcp/ops-credential-recv.sh, purpose machine-token), host
#      key pinned from $BOSS_PUSH_KNOWN_HOSTS, and READ ITS FIRST LINE.
#      Only when that line is the receiver's own are the three lines
#      written to ssh's stdin; the whole transfer is under a time bound,
#      and the receiver's three byte counts are READ BACK against the
#      three sent.
#
# THE RECEIVER SPEAKS FIRST, OR NOTHING IS SENT (backlog dea2236f). On the
# first live enrolment (packet 96a0f7bb, 2026-10-07) the key's line was
# placed on boss-gcp as the bare public key, without its command=
# prefix. sshd accepted the key and ran the account's SHELL; this script
# wrote the three slots before it had read anything, so they went to
# that shell's stdin, and the pass closed on `no slot counts` — true, and
# silent about a key minted for one command having opened an account,
# for the ten to fifteen minutes until a person noticed. So the far side
# now takes the first step, and what it says decides everything:
#   - the receiver's line: the slots are sent;
#   - any other text — a login banner above all: `this key reaches a
#     shell`, RED on a route of its own, naming the line to remove;
#   - nothing, then a clean end on an empty stream: the same;
#   - nothing, then the OLDER receiver's refusal of an empty deposit:
#     not yet, naming boss-gcp's converge (the two halves land apart).
# In every case but the first, no byte of a slot leaves this pod.
# WHAT THIS DOES NOT SEE: a line that forces the receiver and lacks
# `restrict`. That key is confined to the command and can still forward
# a port; only reading authorized_keys on boss-gcp shows it.
#
# NOT YET IS NOT A FAILURE, AND SAYS WHICH ENROLLMENT IS MISSING. Until
# David has scoped the key's packet there is no key; until he has placed
# its line on boss-gcp, ssh answers `Permission denied`. Both exit 75,
# which this Job's manifest tells boss-chore.sh to record as `not-yet`
# (BOSS_CHORE_NOT_YET_ON_75): a packet every ten minutes that says what
# is owed and files nothing. Everything else that stops a push — a
# changed host key, a receiver that refuses, counts that disagree, a key
# that reaches anything but the receiver — is a
# RED line and a failed Job, which
# file-backlog-items-on-machine-token-push-red files once per route.
#
# THE TOKEN IS NEVER REQUIRED. boss-gcp without a token is a host whose
# callers send without one, which a gate in `report` admits and tallies;
# this Job failing stops no caller there.
#
# ROTATION. The broker stages `next`, promotes it to `current` (the old
# value moves to `previous`, still accepted) and blanks `previous` only
# once no gate has seen it presented for 1440 minutes. The kubelet
# refreshes this pod's mount within about two minutes and this runs
# every ten, so after a promotion boss-gcp presents the old value for at
# most one run plus one refresh — 144 runs inside one drain — and a
# caller still presenting `previous` HOLDS the revoke open, named on it.
# What is left: a boss-gcp unreachable for longer than the drain presents
# a revoked value from when it returns until the next push, at most ten
# minutes.
#
# NOTHING SECRET IS PRINTED OR PASSED IN AN ARGV: the slots reach ssh's
# stdin through printf (a builtin), the key reaches a 0600 file in this
# pod's memory-backed scratch, and every line names slots and lengths.
#
# Exit: 0 pushed (or boss-gcp already held all three); 75 not yet —
# nothing to push, or a key boss-gcp does not accept yet; 1 refused or
# failed, named on a RED line; 78 a refusal of how it was invoked.
# Tracing off, first: an exported SHELLOPTS=xtrace would print every
# expansion, slot values among them (review 9a1e289b, F8).
set +x
set -euo pipefail

ME=machine-token-push
ROUTE=machine-token-push
KEY_SECRET="${BOSS_PUSH_KEY_SECRET:-machine-token-deposit-key}"
KEY_DIR="${BOSS_PUSH_KEY_DIR:-/keys}"
KNOWN_HOSTS="${BOSS_PUSH_KNOWN_HOSTS:-/known/known_hosts}"
TARGET="${BOSS_PUSH_TARGET:-}"
TOKEN_DIR="${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}"
# The whole transfer: connect, the receiver's work, the answer.
SHIP_TIMEOUT_S="${BOSS_PUSH_TIMEOUT_S:-60}"
# boss-core's MAX_SLOT_BYTES (crates/core/boss-core/src/machine_token.rs).
MAX_SLOT_BYTES=4096
# The one command the key may run on boss-gcp, as the broker's rule
# declares it and the receiver's installer prints it —
# machine_token_push_sh.rs holds the three equal.
FORCED_COMMAND='exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token'
PACKET="the prepare-the-machine-token-deposit-key packet for $KEY_SECRET"
# The receiver's first line, word for word (infra/gcp/ops-credential-recv.sh
# prints it; machine_token_push_sh.rs holds the two equal), and how long
# this pass waits for it: connect, sudo, and one echo.
GREETING='ops-credential-recv: machine-token: ready for three slots'
GREETING_TIMEOUT_S="${BOSS_PUSH_GREETING_TIMEOUT_S:-30}"
# A KEY THAT IS NOT CONFINED TO THE RECEIVER IS ITS OWN ROUTE. The red
# rule files one backlog-item per route and files nothing for a route
# whose item is open — so under this Job's one route, an item already
# open for a slow host would have swallowed "this key opens a shell".
KEY_ROUTE=machine-token-deposit-key-unconfined

red() { echo "RED $1 $2: $3" >&2; }
usage() { red "$ROUTE" misconfigured "$*"; exit 78; }
refuse() { red "$ROUTE" refused "$*"; exit 1; }
unconfined() { red "$KEY_ROUTE" refused "$*"; exit 1; }
not_yet() { echo "$ME: not yet — $*"; exit 75; }
say() { echo "$ME: $*"; }

[[ $TARGET =~ ^[a-z_][a-z0-9_-]*@[A-Za-z0-9.:-]+$ ]] \
    || usage "BOSS_PUSH_TARGET='$TARGET' is not <account>@<host> (the manifest reads it from the deposit's ConfigMap)"
[[ $SHIP_TIMEOUT_S =~ ^[1-9][0-9]*$ ]] || usage "BOSS_PUSH_TIMEOUT_S='$SHIP_TIMEOUT_S' is not a number of seconds"
[[ $GREETING_TIMEOUT_S =~ ^[1-9][0-9]*$ ]] || usage "BOSS_PUSH_GREETING_TIMEOUT_S='$GREETING_TIMEOUT_S' is not a number of seconds"

umask 077
WORK="$(mktemp -d "${TMPDIR:-/tmp}/push.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
ERR="$WORK/err"
why() { tr '\n' ' ' < "$ERR" | cut -c1-300; }

# --- 1. the key ---------------------------------------------------------
KEY="$KEY_DIR/private_key"
if [ ! -s "$KEY" ]; then
    not_yet "no deposit key is prepared: Secret $KEY_SECRET holds none (or this pod's mount does not show it yet). MISSING: $PACKET has not been filed and scoped — file it (boss job file --kind prepare-the-machine-token-deposit-key --subject-id $KEY_SECRET) and complete its scope step; the broker then prepares the key and puts its authorized_keys_line on the enroll step"
fi
MINTED_FOR=""
if [ -s "$KEY_DIR/minted_for" ]; then
    MINTED_FOR="$(tr -d '[:space:]' < "$KEY_DIR/minted_for")"
    [[ $MINTED_FOR =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || MINTED_FOR=""
fi
[ -z "$MINTED_FOR" ] || PACKET="prepare-the-machine-token-deposit-key packet $MINTED_FOR"
# A copy this uid owns, 0600: OpenSSH refuses a key it owns that group
# or other can read, and the mount is root's, group-readable. The broker
# stores the PEM without its last newline; the parser wants it back.
{ cat -- "$KEY" && printf '\n'; } > "$WORK/key" 2>"$ERR" \
    || refuse "the deposit key at $KEY could not be read: $(why)"
chmod 600 "$WORK/key"

# --- 2. the token -------------------------------------------------------
# slot NAME — the slot's value into SLOT_VALUE (empty when absent or
# blank), judged as boss-core judges one. Returns 1 naming why in
# SLOT_WHY; never prints a value.
SLOT_VALUE="" SLOT_WHY=""
slot() {
    local f="$TOKEN_DIR/$1" size v
    SLOT_VALUE="" SLOT_WHY=""
    if [ ! -e "$f" ] && [ ! -L "$f" ]; then return 0; fi
    if [ ! -f "$f" ] || [ ! -r "$f" ]; then
        SLOT_WHY="slot $1 at $f is not a readable regular file"
        return 1
    fi
    size="$(wc -c < "$f" | tr -d ' ')" || size=""
    case "${size:-empty}" in
        empty | *[!0-9]*) SLOT_WHY="slot $1 at $f could not be measured"; return 1 ;;
    esac
    # Room for the whitespace a mount may carry around the bound itself.
    if [ "$size" -gt $((MAX_SLOT_BYTES + 64)) ]; then
        SLOT_WHY="slot $1 is $size bytes, past the $MAX_SLOT_BYTES a slot may hold"
        return 1
    fi
    v="$(cat -- "$f")" || { SLOT_WHY="slot $1 at $f could not be read"; return 1; }
    v="${v#"${v%%[![:space:]]*}"}"
    v="${v%"${v##*[![:space:]]}"}"
    [ -n "$v" ] || return 0
    if [ "${#v}" -gt "$MAX_SLOT_BYTES" ]; then
        SLOT_WHY="slot $1 is ${#v} bytes, past the $MAX_SLOT_BYTES a slot may hold"
        return 1
    fi
    if [[ ! $v =~ ^[A-Za-z0-9_-]+$ ]]; then
        SLOT_WHY="slot $1 is not one base64url word (${#v} bytes)"
        return 1
    fi
    SLOT_VALUE="$v"
}
slot current || refuse "the mounted machine token is not one this pass will send — $SLOT_WHY; nothing was sent"
CURRENT="$SLOT_VALUE"
slot next || refuse "the mounted machine token is not one this pass will send — $SLOT_WHY; nothing was sent"
NEXT="$SLOT_VALUE"
slot previous || refuse "the mounted machine token is not one this pass will send — $SLOT_WHY; nothing was sent"
PREVIOUS="$SLOT_VALUE"
SLOT_VALUE=""
if [ -z "$CURRENT" ]; then
    not_yet "there is no token to push: $TOKEN_DIR holds no current slot (the broker has not minted the machine token, or this pod's mount is empty). MISSING: nothing of boss-gcp's — the first rotate-a-credential packet for boss-machine-token"
fi
SENT="current ${#CURRENT} bytes, next ${#NEXT} bytes, previous ${#PREVIOUS} bytes"
say "read the mounted token: $SENT"

# --- 3. ship ------------------------------------------------------------
[ -s "$KNOWN_HOSTS" ] || refuse "no pinned host key at $KNOWN_HOSTS — the token is never shipped to a host this pass cannot authenticate"
# -T: no pty — the receiver reads bytes, and a pty would translate them.
SSH_OPTS=(-F /dev/null -T -i "$WORK/key" -o IdentitiesOnly=yes -o BatchMode=yes
    -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$KNOWN_HOSTS" -o GlobalKnownHostsFile=/dev/null
    -o ConnectTimeout=20 -o ServerAliveInterval=10 -o ServerAliveCountMax=3)
src=0
# TWO STEPS, AND THE FAR SIDE TAKES THE FIRST (backlog dea2236f; header,
# THE RECEIVER SPEAKS FIRST). ssh's stdin and stdout are two fifos in
# this pod's scratch, so the far side's first line can be read while
# nothing has been written to it.
#
# BOUNDED (review a92c0b94 F1): a transfer that hangs is killed, and the
# EXIT trap above removes this pod's scratch — which is memory, and the
# Job's own, either way. The wait for the first line has its own bound
# inside that one; the read of the rest ends when ssh does.
mkfifo "$WORK/to" "$WORK/from" 2>"$ERR" \
    || refuse "this pod's scratch could not hold the transfer's two pipes: $(why); nothing was sent"
timeout -k 5 "$SHIP_TIMEOUT_S" ssh "${SSH_OPTS[@]}" "$TARGET" < "$WORK/to" > "$WORK/from" 2>"$ERR" &
SSH_PID=$!
# After the fork, so ssh keeps its own handling: a far side that closes
# early must be an exit status read below, never this shell killed by a
# write to a closed pipe with a slot half out.
trap '' PIPE
exec {TO}> "$WORK/to" {FROM}< "$WORK/from"
# One line, at most 256 bytes of it: a far side that streams without a
# newline is not read without end.
FIRST="" heard=0
IFS= read -r -t "$GREETING_TIMEOUT_S" -n 256 -u "$FROM" FIRST || heard=$?
GREETED=0
if [ "$heard" -eq 0 ] && [ "$FIRST" = "$GREETING" ]; then
    GREETED=1
    # On the packet, so a pass that delivered says by which road: this
    # line is what a completed push carries that the one-step sender's
    # never did.
    say "$TARGET answered first as the receiver; sending the three slots"
    # A write the far side did not take shows as counts that disagree.
    printf 'current=%s\nnext=%s\nprevious=%s\n' "$CURRENT" "$NEXT" "$PREVIOUS" >&"$TO" 2>/dev/null || true
fi
CURRENT="" NEXT="" PREVIOUS=""
# Closing the stream with nothing written is itself the question put to
# a far side that said nothing: an older receiver answers an empty
# deposit in its own words, a shell ends without a word.
exec {TO}>&-
cat <&"$FROM" > "$WORK/recv.out" || true
exec {FROM}<&-
wait "$SSH_PID" || src=$?
sed "s/^/  $TARGET: /" "$WORK/recv.out"
if [ "$GREETED" -eq 0 ]; then
    # NOTHING WAS SENT. What follows only names why, in the order the
    # evidence is strong.
    HOST="${TARGET#*@}"
    FIX="The deposit key's line in ~${TARGET%@*}/.ssh/authorized_keys on $HOST was placed without its command= prefix (the bare public key, or a line that forces something else), so this key opens that account where it may run one receiver command. REMOVE THAT LINE NOW, then place the authorized_keys_line on the enroll step of $PACKET, whole — it forces: $FORCED_COMMAND. No token byte was sent."
    # 1. TEXT ON STDOUT THAT IS NOT THE GREETING IS NOT THE RECEIVER,
    # whatever happened next: the receiver prints nothing before that
    # line, and sshd shows no login message when a command is forced.
    # This is the first live enrolment's own shape (packet 96a0f7bb): a
    # login banner. It is quoted for the record — printable bytes, a
    # bounded length.
    if [ -n "$FIRST" ]; then
        SAID="$(printf '%s' "$FIRST" | LC_ALL=C tr -c '[:print:]' '?' | cut -c1-120)"
        unconfined "this key reaches a shell on $HOST: the far side accepted the key and answered with its own text ('$SAID') where the receiver's first line was expected. $FIX"
    fi
    # The same, late: nothing inside the bound, then text. ONE late line
    # is the receiver's — it was ready after the bound, and was handed
    # nothing — and that is a slow host, not a finding about the key.
    LATE=""
    IFS= read -r LATE < "$WORK/recv.out" || true
    if [ "$LATE" = "$GREETING" ]; then
        refuse "boss-gcp's receiver said it was ready only after the $GREETING_TIMEOUT_S seconds this pass waits for that line (sudo or the host is slow); nothing was sent, and the next pass asks again"
    fi
    if [ -s "$WORK/recv.out" ]; then
        unconfined "this key reaches a shell on $HOST: the far side accepted the key, said nothing for the $GREETING_TIMEOUT_S seconds the receiver's first line is waited for, and then answered with its own text (above). $FIX"
    fi
    # 2. NOT ENROLLED IS ONE EXACT ANSWER, AND ONLY IT AND THE OLDER
    # RECEIVER ARE QUIET (adversarial review ecd9263a, F3). ssh's stderr
    # carries the far side's stderr too, and the first draft took the
    # words `Permission denied` from anywhere in it, whatever the exit: a
    # receiver that could not stage its file — exit 1, AFTER the key was
    # accepted — closed not-yet, for ever, every ten minutes. Not-yet is
    # ssh's OWN failure (255) with sshd's own refusal of the key,
    # `<account>@<host>: Permission denied (publickey…`. Anything else is
    # a RED line.
    #
    # What this still cannot tell apart: a key never enrolled, and a key
    # that WAS accepted and no longer is (authorized_keys rewritten, the
    # key re-prepared). sshd answers both the same way, so both close
    # not-yet; the push's silence roster entry and boss-gcp's own
    # machine_token_effect are what notice the second.
    if [ "$src" -eq 255 ] && grep -Eq '^[^ ]+@[^ ]+: Permission denied \(publickey' "$ERR"; then
        not_yet "boss-gcp does not accept this deposit key (ssh exit $src). MISSING: the enrollment on boss-gcp — the authorized_keys_line on the enroll step of $PACKET is not in ~${TARGET%@*}/.ssh/authorized_keys there (this pass reads the placement, not the signature: the line is placed first, and that step is signed after a pass has closed completed). That line forces: $FORCED_COMMAND"
    fi
    if [ "$src" -eq 124 ] || [ "$src" -eq 137 ]; then
        refuse "the push to $TARGET did not finish inside $SHIP_TIMEOUT_S seconds and was stopped before the far side had said anything; nothing was sent, and boss-gcp holds whatever it held"
    fi
    # 3. THE OLDER RECEIVER, which reads its whole stdin before it says
    # anything (this sender and boss-gcp's receiver land by two
    # converges). Handed an empty stream it refuses in its own words —
    # proof of what is behind the key — so this is a converge that is
    # owed, not a finding, and it is the one the sender waits out.
    if [ "$src" -eq 65 ] && grep -Fq 'ops-credential-recv: REFUSED — an empty deposit' "$ERR"; then
        not_yet "boss-gcp's receiver does not speak first yet, so nothing was sent to it. MISSING: boss-gcp's converge has not installed this tree's ops-credential-recv (infra/gcp/install-ops-credential-receiver.sh, on its next tick); boss-gcp holds the token it held"
    fi
    # 4. Accepted, silent, and a clean end on an empty stream: the
    # receiver never ends cleanly on one. A shell does.
    if [ "$src" -eq 0 ]; then
        unconfined "this key reaches a shell on $HOST: the far side accepted the key, said nothing for the $GREETING_TIMEOUT_S seconds the receiver's first line is waited for, and ended cleanly when the stream closed with nothing in it, which the receiver never does. $FIX"
    fi
    refuse "the push to $TARGET failed (ssh exit $src) before the receiver said it was ready: $(why); nothing was sent"
fi
if [ "$src" -ne 0 ]; then
    # The receiver was ready, was sent the three lines, and did not end
    # cleanly: its own refusal, or the transfer's bound.
    if [ "$src" -eq 124 ] || [ "$src" -eq 137 ]; then
        refuse "the push to $TARGET did not finish inside $SHIP_TIMEOUT_S seconds and was stopped; boss-gcp holds whatever it held"
    fi
    refuse "the push to $TARGET failed (ssh exit $src): $(why)"
fi
got="$(sed -n 's/^ops-credential-recv: machine-token: received \(current [0-9][0-9]* bytes, next [0-9][0-9]* bytes, previous [0-9][0-9]* bytes\);.*/\1/p' "$WORK/recv.out")"
[ "$got" = "$SENT" ] \
    || refuse "the receiver reported '${got:-no slot counts}' where '$SENT' was sent — the push is not proven"
say "pushed: $TARGET received $SENT"
