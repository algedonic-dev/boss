#!/usr/bin/env bash
#
# probe-reader-deposit — the forge host takes the PROBE-READER
# credential's `current` slot, and no other, out of the broker's Secret
# and into the root-only file `boss prove` opens its reader door with,
# on every forge-converge tick — and only after every gated port has
# named that value `reader.current`.
#
#   probe-reader-deposit.sh --rule <reader rule .toml> --ports <port table> --dest <file>
#
# WHY IT EXISTS (design b35c22b4; backlog d26515c5, unit 7 of its chain).
# A recorded probe runs on this host as the unprivileged account
# boss-probe and sends no machine token, so once the machine gates
# enforce (2710c8fc row C) every probe read is refused and no landed car
# can be proven. The answer decided on 2026-10-01 is a reader credential
# of its own — GET and HEAD only, answered as
# automation:run-car-probe-reader — held by `boss prove` (root) for the
# life of one probe behind a private socket (boss-cli probe_reader.rs).
# The gates mount it (unit 2), the broker rotates it (unit 3), the door's
# hand-over was proved on this host (unit 6). This is the last piece: the
# client file, /etc/boss/probe-reader.credential, which nothing delivered.
# It follows the estate token's own deposit (machine-token-deposit.sh,
# backlog 88379df3): this host already holds the admin kubeconfig, so its
# converge reads the Secret with no new grant.
#
# FOUR RULES FROM THE REVIEW THAT ASKED FOR THIS FILE (run f09db7f0, F4),
# each held by a test in probe_reader_deposit_sh.rs:
#
#   1. `current` ONLY. The API server is asked for `{.data.current}` and
#      nothing else: `next` and `previous` are never read, so they cannot
#      be deposited, logged, or fallen back to. A staged `next` is a value
#      no gate has agreed to call current.
#   2. EVERY GATE SAYS `reader.current` FIRST. Before a byte is written
#      the value is presented ALONE — one x-boss-machine-token header, no
#      asserted identity, exactly as the door will present it — to
#      /api/machine-gate/accepts on every gated port of the table, and
#      the deposit goes on only if every one answers `reader.current`.
#   3. A BARE SLOT NAME IS A COLLISION, REFUSED OUTRIGHT. A gate names an
#      ESTATE token `current`, `next` or `previous`, and estate wins a
#      tie: so a gate that names THIS value by a bare word is saying the
#      reader value equals an estate machine token — which would hand
#      probe text every method under any identity (review 177b4976, N2).
#      Never deposited; a fault in capitals on stderr and the packet.
#   4. NO FALLBACK, AND NOTHING IS EVER REMOVED. An empty or absent
#      `current`, a gate that is behind, dark or says no: the file this
#      host holds is left byte for byte as it was, and where there was
#      none there is none. This script has no line that deletes the
#      credential file. (What it does delete, on every pass, is a TEMP
#      name an earlier, killed pass left beside it.) A kept file whose
#      value has left every slot stops every probe at the door; the
#      packet line says so and names the remedy, a fresh rotation.
#
# WHICH PORTS ARE "EVERY GATE". The table handed in as --ports is the
# list of ports the reader door forwards a probe's reads to, so it is
# exactly the set this credential must work at; plus the port of
# BOSS_JOBS_URL itself, as the door adds it. Minus the services boss-core
# says mount no gate (machine_gate.rs UNGATED — the gateway, which strips
# every x-boss-* header at the edge): a port the table gives one of them
# is never sent the value, under any name and not as the base port
# either (review 4d39f4dc, F3). The roster below is a copy, pinned equal
# to boss-core's by the test (CLAUDE.md §9a).
#
# "EVERY GATE" IS AS STRONG AS THE TABLE, so the table must be this
# account's alone to write (review 4d39f4dc, N6): a table shortened to
# one gate deposited at 1 of 1 and hid a gate that said no. The converge
# hands in the ROOT-OWNED probe view's copy (/var/lib/boss/probe-view,
# infra/forge/probe-account.sh) — the very file the door reads its ports
# from, under the door's own rule (review 991bb439, N3) — never the
# checkout owner's. A table that is another account's, or that group or
# other may write, lists nothing and nothing is sent. The effect line
# counts the gates it asked, N of N, and names the table, so a short one
# is on the packet; the car's recorded probe refuses an N smaller than
# the converged tree's own table implies.
#
# WHERE THE VALUE MAY GO. Only to the host of BOSS_JOBS_URL, and only
# when that host is loopback or named in BOSS_MACHINE_TOKEN_HOSTS (env,
# else /etc/boss/sor.env) — the estate machine token's own rule, by the
# same function (infra/lib/secret-header.sh): a hand run with the public
# edge in BOSS_JOBS_URL sends nothing. Never through a proxy, never
# following a redirect, never with ~/.curlrc.
#
# THE FILE. root:root 0600 at --dest, whose DIRECTORY must already exist,
# be this account's own (root's, on the host) and closed to group and
# other for writing — else nothing is read or written, because a name
# someone else can plant in that directory is a name root would write
# through. A symlink or a directory at --dest is refused the same way.
# The value is written to a temp name in that same directory under
# noclobber (bash opens a name that is not there with O_EXCL and refuses
# one that is), fsynced, held at 0600 root:root, and renamed into place
# (`mv -T`: whatever stands at the path is replaced, never written into).
#
# NO VALUE EVER LEAVES A VARIABLE except into that file and the 0600
# header file curl reads (`-H @file`, in the unit's RuntimeDirectory):
# never an argv, a message, a summary field, an exported variable — not
# even a suffix. Every line names a slot, a port, a path and a LENGTH.
# What a value still passes through, stated and not closed, as the
# sibling deposits': kubectl's stdout carries the base64 slot through
# docker's json-file log for the life of the `--rm` container, root-only.
#
# WHAT THIS HOST'S MODES DO NOT ISOLATE, stated as the estate deposit
# states it: forge-converge.service runs this script AS ROOT out of
# /home/david/boss, and david holds passwordless sudo on this host
# (measured 2026-10-07, ops-request 2ba87619). The 0600 file keeps the
# reader credential from boss-probe — the account a probe's text runs
# as, which holds no sudo and cannot read david's checkout — and from an
# accident. It keeps nothing from david, who is root here by two roads
# older than this file (design c98c79aa is that question).
#
# NOTHING HERE IS EVER REQUIRED. forge-converge.sh carries this script's
# exit onto its packet and never into its own, and bounds its time. A
# host with no reader file is a host whose probes run without the door,
# exactly as before this file — which a gate in `report` admits.
#
# ROTATION. The broker promotes `next` to `current` only after every
# gate names it `reader.next`; the old value stays accepted as
# `reader.previous` for the drain (1440 minutes, the rule's line). This
# runs every ten minutes, so the file follows a promotion within a tick
# or two (a gate whose mount is a refresh behind answers `reader.next`
# for the new current: a not-yet, and the old file — still accepted —
# stands until the next tick).
#
# Exit: 0 the file holds the value every gate names reader.current, or
# there is nothing to deposit yet (not minted, not ready, a gate behind
# or dark); 1 a fault this pass names — a collision first among them;
# 78 a refusal of how it was invoked.

# TRACING OFF, FIRST: an exported SHELLOPTS=xtrace starts this shell
# printing every expansion, the value among them (review 9a1e289b, F8).
set +x
set -euo pipefail

ME=probe-reader-deposit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

# boss-core's MAX_SLOT_BYTES: past this a slot is not a token, to the
# gate and to every reader.
MAX_SLOT_BYTES=4096
# The services that mount no machine gate — boss-core machine_gate.rs
# UNGATED, held equal by probe_reader_deposit_sh.rs.
UNGATED="gateway"

RULE="" DEST="" PORTS=""
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || refuse "$1 needs a value"
    case "$1" in
        --rule) RULE="$2" ;;
        --dest) DEST="$2" ;;
        --ports) PORTS="$2" ;;
        *) refuse "unknown argument $1 (usage: $ME --rule FILE --ports FILE --dest FILE)" ;;
    esac
    shift 2
done
[ -n "$RULE" ] && [ -n "$DEST" ] && [ -n "$PORTS" ] || refuse "--rule, --ports and --dest are all required"
case "$DEST" in
    /*) ;;
    *) refuse "--dest '$DEST' is not an absolute path" ;;
esac
case "$DEST" in
    */) refuse "--dest '$DEST' names a directory, not the credential's file" ;;
esac

# WHICH MACHINE THIS IS, BEFORE ANYTHING IS READ OR WRITTEN (backlog
# 62b09c57, N7; infra/lib/host-check.sh carries the incident and the
# rule). Aimed at the one file the probe door reads, this deposits the
# reader's credential: the machine must hold the address the estate
# declares for the forge, or the exit is 78 and nothing is read from the
# cluster or written here. Aimed anywhere else it is a test's file and
# no identity is asked. Before the summary library is sourced, so a
# refusal is this script's own line and never a field on its caller's
# packet.
# shellcheck source=infra/lib/host-check.sh
. "$INFRA/lib/host-check.sh"
host_path --dest "$DEST" /etc/boss/probe-reader.credential
host_check forge "$ME"

# --- 1. the declaration: the READER's broker rule, and no other --------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
HANDLER_LINE="$(grep -m1 '^handler = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
[ "$HANDLER_LINE" = 'handler = "credential.rotate.self-issued"' ] \
    || refuse "$RULE is not a self-issued credential's rule ($HANDLER_LINE)"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
GATE_SLOTS="$(rule_arg gate_slots)"
# The estate machine token's rule is this handler too, one argument
# apart. Its Secret must never be read into the reader's file.
[ "$GATE_SLOTS" = reader ] \
    || refuse "$RULE does not declare gate_slots = \"reader\" (it says '${GATE_SLOTS:-nothing}'): only the probe reader's own rule names what this file may hold"
[ -n "$NS" ] && [ -n "$NAME" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name — cannot know what to deposit"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"

PARENT="$(dirname "$DEST")"
BASE="$(basename "$DEST")"

KERR="$(mktemp)"
BODY="$(mktemp)"
TMP=""
cleanup() {
    rm -f "$KERR" "$BODY"
    if [ -n "$TMP" ]; then rm -f -- "$TMP"; fi
}
# Set BEFORE the first secret_header call, which chains its own cleanup
# in front of this one (infra/lib/secret-header.sh, ORDER).
trap cleanup EXIT

rc=0
SECRET_STATE="" ACTION="untouched" EFFECT="not asked"
V="" SWEPT=0

# finish — the one way out after the declaration: what this pass found,
# did and proved, on the converge packet and in the journal, then the
# exit status. A fault is said on stderr too.
finish() {
    V=""
    if [ "$SWEPT" -gt 0 ]; then
        ACTION="$ACTION; $SWEPT leftover temp file(s) of an earlier pass removed"
    fi
    run_summary_field probe_reader_secret "$SECRET_STATE"
    run_summary_field probe_reader_action "$ACTION"
    run_summary_field probe_reader_effect "$EFFECT"
    say "secret $NS/$NAME: $SECRET_STATE"
    say "file $DEST: $ACTION"
    say "effect: $EFFECT"
    if [ "$rc" -ne 0 ]; then
        echo "$ME: FAULT (exit $rc) — the lines above name it; no probe and no caller on this host is stopped by it, and the file it found is as it found it unless a line above says it was written" >&2
    fi
    exit "$rc"
}

kerr() { head -n 1 "$KERR" | cut -c1-200; }

# --- the destination, before anything is read --------------------------
# A name in a directory someone else can write is a name they can plant:
# root would then read a held value through it, or rename onto it.
#
# not_alone PATH NAME — ONE RULE, for the file's directory and for the
# port table: PATH is this account's own (root's, on the host) and
# carries no group or other write bit. Prints why it is not, naming it
# NAME; prints nothing when it is. The reader door's rule for its port
# list is the same two facts with uid 0 spelled out (boss-cli
# probe_reader.rs ports_list_refusal; held beside this line by
# probe_reader_deposit_sh.rs). A link is followed: the caller refuses a
# linked directory first, and hands the table as its OPEN descriptor.
not_alone() {
    local owner_mode owner bits me
    if ! owner_mode="$(stat -L -c '%u %a' -- "$1")"; then
        echo "$2 could not be examined"
        return 0
    fi
    owner="${owner_mode%% *}"
    bits="${owner_mode##* }"
    me="$(id -u)"
    case "$owner$bits" in
        '' | *[!0-9]*)
            echo "$2 could not be examined"
            return 0
            ;;
    esac
    if [ "$owner" != "$me" ] || [ $((8#$bits & 8#022)) -ne 0 ]; then
        echo "$2 is uid $owner's at mode $bits, not this account's alone to write (uid $me, no group or other write bit)"
    fi
    return 0
}
dest_refusal() {
    local why
    if [ -L "$PARENT" ] || [ ! -d "$PARENT" ]; then
        echo "$PARENT is not a directory of its own (absent, or a link); this script never makes it"
        return 0
    fi
    why="$(not_alone "$PARENT" "$PARENT")"
    if [ -n "$why" ]; then
        echo "$why: a name another account can plant there is a name this would write through"
        return 0
    fi
    if [ -L "$DEST" ]; then
        echo "$DEST is a symlink, not the deposit's own file"
        return 0
    fi
    if [ -e "$DEST" ] && [ ! -f "$DEST" ]; then
        echo "$DEST exists and is not a regular file"
        return 0
    fi
    return 0
}
WHY_NOT="$(dest_refusal)"
if [ -n "$WHY_NOT" ]; then
    rc=1
    SECRET_STATE="not read: the destination was refused first"
    ACTION="REFUSED: $WHY_NOT; nothing was read or written"
    EFFECT="not asked: the destination was refused, so no gate was sent anything"
    finish
fi
HELD=0
if [ -f "$DEST" ] && [ -s "$DEST" ]; then HELD=1; fi
as_it_was() {
    if [ "$HELD" -eq 1 ]; then
        printf 'the file this host holds is as it was, and was not asked about'
    else
        printf 'this host holds no reader file, so its probes run without the reader door'
    fi
}
# stale_cost — what a KEPT file costs once its value is no gate's, and
# what ends it (review 4d39f4dc, N7). Keeping it is right: removing it
# on one empty answer would return every probe to tokenless reads. But
# the packet line is where an operator learns that probes have stopped.
stale_cost() {
    if [ "$HELD" -eq 1 ]; then
        printf '. THE FILE KEPT MAY BE STALE: once the value it holds has left every reader slot, every probe on this host is refused at the door, and each attempt is tallied as a mismatch, until the file is replaced — by a fresh rotation of %s, which the next deposit follows, or by root removing the file' "$NAME"
    fi
}

# A TEMP NAME AN EARLIER PASS LEFT, swept on EVERY pass that accepts the
# destination — not only one that writes (review 4d39f4dc, F4: a pass
# KILLed between create and rename left a 0600 file holding a value
# beside a file that stayed current, and every later pass said
# `unchanged` without looking). A oneshot never runs beside itself, so
# any such name is a leftover. Removed by NAME: a link planted there
# loses the link, not its target.
for leftover in "$PARENT/.$BASE.tmp."*; do
    if [ -e "$leftover" ] || [ -L "$leftover" ]; then
        if rm -f -- "$leftover" 2>/dev/null; then SWEPT=$((SWEPT + 1)); fi
    fi
done

# --- 2. the Secret: `current`, and no other key ------------------------
# THE BOUNDS. A read asks the API server for one key of one object: 15 s
# is far past a healthy answer; the outer kill is past that plus a cold
# image start. Each gate is asked once, bounded; forge-converge.sh bounds
# the whole script again (probe_reader_deposit_sh.rs holds the sum).
REQUEST_TIMEOUT_S=15
READ_BOUND_S="${BOSS_DEPOSIT_READ_BOUND_S:-25}"
GATE_BOUND_S="${BOSS_READER_GATE_BOUND_S:-3}"
case "$READ_BOUND_S$GATE_BOUND_S" in
    '' | *[!0-9]*) refuse "BOSS_DEPOSIT_READ_BOUND_S / BOSS_READER_GATE_BOUND_S must be whole seconds" ;;
esac
KC_STATE=""
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig through the kubectl container every
    # forge script uses. ABSENT is not ready (root material David places,
    # design 835c0c9c), never a red.
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    KUBECTL=(docker run --rm --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc "--request-timeout=${REQUEST_TIMEOUT_S}s")
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c)"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable"
    fi
fi

# read_current — the `current` slot into V (empty when absent or blank).
# Returns 0 with SECRET_STATE naming its length; 1 with SECRET_STATE
# naming why nothing was read, and V empty.
read_current() {
    local out b read_rc=0
    V=""
    if [ -n "$KC_STATE" ]; then
        SECRET_STATE="$KC_STATE"
        return 1
    fi
    out="$(timeout -k 5 "$READ_BOUND_S" "${KUBECTL[@]}" -n "$NS" get secret "$NAME" \
        -o "jsonpath={.data.current}" 2>"$KERR")" || read_rc=$?
    if [ "$read_rc" -ne 0 ]; then
        out=""
        if [ "$read_rc" -eq 124 ] || [ "$read_rc" -eq 137 ]; then
            SECRET_STATE="unreadable: the read of $NS/$NAME did not answer inside $READ_BOUND_S seconds and was stopped (a stalled cluster API or docker daemon)"
        elif grep -q 'NotFound' "$KERR"; then
            SECRET_STATE="absent: Secret $NS/$NAME does not exist (the cluster converge creates it from the rule)"
        else
            SECRET_STATE="unreadable: $(head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300)"
        fi
        return 1
    fi
    b="$out"
    out=""
    if [ -z "$b" ]; then
        SECRET_STATE="empty: $NS/$NAME holds no current slot (the broker has not promoted a probe-reader value)"
        return 0
    fi
    if ! V="$(printf '%s' "$b" | base64 -d 2>/dev/null)"; then
        V=""
        SECRET_STATE="malformed: key current of $NS/$NAME is not base64"
        return 1
    fi
    b=""
    # Leading and trailing whitespace, as boss-core's read trims it.
    V="${V#"${V%%[![:space:]]*}"}"
    V="${V%"${V##*[![:space:]]}"}"
    if [ -z "$V" ]; then
        SECRET_STATE="empty: $NS/$NAME holds a blank current slot (the broker has not promoted a probe-reader value)"
        return 0
    fi
    if [ "${#V}" -gt "$MAX_SLOT_BYTES" ]; then
        SECRET_STATE="malformed: key current of $NS/$NAME is ${#V} bytes, past the $MAX_SLOT_BYTES a slot may hold"
        V=""
        return 1
    fi
    if [[ ! $V =~ ^[A-Za-z0-9_-]+$ ]]; then
        SECRET_STATE="malformed: key current of $NS/$NAME is not one base64url word (${#V} bytes)"
        V=""
        return 1
    fi
    SECRET_STATE="read: current ${#V} bytes"
    return 0
}

# --- 3. every gate, asked with the value alone -------------------------
# gates — GATES becomes `name:port` for every gated port of the table,
# plus the system of record's own. Returns 1 with EFFECT naming why no
# gate may be asked; nothing has been sent then.
GATES=() SCHEME="" HOST=""
gates() {
    local line name port base_port="" url="${BOSS_JOBS_URL:-}" seen=" " ungated=" " why entry fd
    GATES=()
    if [ -z "$url" ]; then
        EFFECT="UNVERIFIED: BOSS_JOBS_URL is unset (/etc/boss/sor.env), so no gate could be asked; nothing was deposited"
        return 1
    fi
    # A plain origin and nothing else: userinfo, a path or a query would
    # make the host asked a different one from the host judged.
    if [[ ! $url =~ ^(https?)://([A-Za-z0-9.-]+)(:([0-9]{1,5}))?/?$ ]]; then
        rc=1
        EFFECT="UNVERIFIED: BOSS_JOBS_URL is not a plain http(s) origin (scheme, host, optional port), so no gate was asked; nothing was deposited"
        return 1
    fi
    SCHEME="${BASH_REMATCH[1]}"
    HOST="${BASH_REMATCH[2],,}"
    base_port="${BASH_REMATCH[4]}"
    if [ -z "$base_port" ]; then
        if [ "$SCHEME" = https ]; then base_port=443; else base_port=80; fi
    fi
    if [ ! -f "$PORTS" ] || [ ! -r "$PORTS" ]; then
        rc=1
        EFFECT="UNVERIFIED: the port table $PORTS is not a file this account can read (on the forge it is the root-owned probe view's, which the converge makes), so the gates could not be listed; nothing was sent or deposited"
        return 1
    fi
    # Opened ONCE, and judged on the open file: the lines read below are
    # the lines of the file whose owner and mode were judged.
    if ! { exec {fd}< "$PORTS"; } 2>/dev/null; then
        rc=1
        EFFECT="UNVERIFIED: the port table $PORTS could not be opened, so the gates could not be listed; nothing was sent or deposited"
        return 1
    fi
    why="$(not_alone "/dev/fd/$fd" "the port table $PORTS")"
    if [ -n "$why" ]; then
        exec {fd}<&-
        rc=1
        EFFECT="UNVERIFIED: $why — whoever can write the table can shorten it, and a gate it does not list is a gate that is never asked — so no gate was listed; nothing was sent or deposited"
        return 1
    fi
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%%#*}"
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        [ -n "$line" ] || continue
        if [[ ! $line =~ ^([a-z][a-z0-9-]*)=([0-9]{1,5})$ ]]; then
            exec {fd}<&-
            rc=1
            EFFECT="UNVERIFIED: the port table $PORTS holds a line that is not name=port, so the gates could not be listed; nothing was sent or deposited"
            return 1
        fi
        name="${BASH_REMATCH[1]}"
        port="$((10#${BASH_REMATCH[2]}))"
        if [ "$port" -lt 1 ] || [ "$port" -gt 65535 ]; then
            exec {fd}<&-
            rc=1
            EFFECT="UNVERIFIED: the port table $PORTS names port $port for $name, which is no port; nothing was sent or deposited"
            return 1
        fi
        # An ungated service's PORT is remembered, not only skipped: it
        # must never come back as the base port or under another name
        # (review 4d39f4dc, F3).
        case " $UNGATED " in
            *" $name "*)
                ungated="$ungated$port=$name "
                continue
                ;;
        esac
        case "$seen" in *" $port "*) continue ;; esac
        seen="$seen$port "
        GATES+=("$name:$port")
    done <&"$fd"
    exec {fd}<&-
    base_port="$((10#$base_port))"
    case "$seen" in
        *" $base_port "*) ;;
        *)
            seen="$seen$base_port "
            GATES+=("sor:$base_port")
            ;;
    esac
    # NEVER TO A PORT THE TABLE GIVES AN UNGATED SERVICE — as the system
    # of record's own port, or under a second, gated name.
    for entry in "${GATES[@]}"; do
        port="${entry##*:}"
        if [[ $ungated =~ \ $port=([a-z0-9-]+)\  ]]; then
            GATES=()
            rc=1
            EFFECT="UNVERIFIED: port $port is the ungated ${BASH_REMATCH[1]}'s in $PORTS, and ${entry%%:*} names it too (BOSS_JOBS_URL's own port is listed as \`sor\`); a service that mounts no gate is never sent the value, so nothing was sent or deposited"
            return 1
        fi
    done
    return 0
}

# ask — every gate in GATES, once, with the value alone. Sets N_OK and
# the four lists of gates that did not say `reader.current`.
N_OK=0 COLLIDE="" REFUSE="" BROKEN="" DARK="" BEHIND=""
READER_HDR=""
ask() {
    local entry name port code word
    N_OK=0 COLLIDE="" REFUSE="" BROKEN="" DARK="" BEHIND=""
    for entry in "${GATES[@]}"; do
        name="${entry%%:*}"
        port="${entry##*:}"
        : > "$BODY"
        code=""
        # -q: no ~/.curlrc. No proxy, no redirect followed, one bounded try.
        if ! code="$(curl -q -sS --noproxy '*' --connect-timeout "$GATE_BOUND_S" --max-time "$GATE_BOUND_S" \
            -o "$BODY" -w '%{http_code}' -H "$READER_HDR" \
            "$SCHEME://$HOST:$port/api/machine-gate/accepts" 2>/dev/null)"; then
            code=""
        fi
        case "$code" in
            200) ;;
            '' | 000)
                DARK="$DARK $name:$port"
                continue
                ;;
            *)
                BROKEN="$BROKEN $name:$port HTTP $code;"
                continue
                ;;
        esac
        # A body that does not parse is its own answer, never a match.
        # THE WORD IS COMPARED AS JSON, QUOTES AND ALL (review 4d39f4dc,
        # N8): `jq -r` into a command substitution drops trailing line
        # breaks, so a `matched` of the word and then a newline read as
        # the word. Encoded, a newline is the two characters `\n` inside
        # the quotes, and only the exact string is the slot's name.
        if ! word="$(jq -c 'if (.matched | type) == "string" then .matched else null end' "$BODY" 2>/dev/null)"; then
            BROKEN="$BROKEN $name:$port answered 200 with something that is not a gate's JSON;"
            continue
        fi
        case "$word" in
            '"reader.current"') N_OK=$((N_OK + 1)) ;;
            '"current"' | '"next"' | '"previous"')
                word="${word//\"/}"
                COLLIDE="$COLLIDE $name:$port \`$word\`;"
                ;;
            '"reader.next"' | '"reader.previous"')
                word="${word//\"/}"
                BEHIND="$BEHIND $name:$port \`$word\`;"
                ;;
            '"none"') REFUSE="$REFUSE $name:$port \`none\`;" ;;
            *) BROKEN="$BROKEN $name:$port answered 200 with no slot name a gate gives;" ;;
        esac
    done
}

held_mode() {
    chmod "$2" "$1" || return 1
    if [ "$(id -u)" -eq 0 ]; then chown 0:0 "$1" || return 1; fi
}

# put — V into $DEST, durable before it is visible. Returns 1 with WHY
# set; the file this host held is then still whole.
WHY=""
put() {
    WHY=""
    TMP="$PARENT/.$BASE.tmp.$$.$RANDOM$RANDOM"
    # EXCLUSIVE, and held by effect: the_create_refuses_a_name_that_is_
    # already_there lifts this one line out and runs it against a planted
    # link. `>` under noclobber opens a name that is not there with O_EXCL
    # and refuses one that is; `>>` is not guarded by noclobber at all.
    (umask 077 && set -o noclobber && printf '%s' "$V" > "$TMP") 2>"$KERR" \
        || { WHY="the temp file beside $DEST could not be created exclusively ($(kerr))"; return 1; }
    sync -- "$TMP" 2>"$KERR" || { WHY="the temp file could not be fsynced ($(kerr))"; return 1; }
    held_mode "$TMP" 600 2>"$KERR" || { WHY="the temp file's owner and mode could not be set ($(kerr))"; return 1; }
    # -T: a link or a directory planted at the path is never written into.
    mv -fT "$TMP" "$DEST" 2>"$KERR" || { WHY="$DEST could not be replaced ($(kerr))"; return 1; }
    TMP=""
    sync -- "$PARENT" 2>"$KERR" || { WHY="$PARENT could not be fsynced after the rename ($(kerr))"; return 1; }
}

if ! read_current; then
    case "$SECRET_STATE" in
        "not ready"*)
            EFFECT="not yet: the Secret could not be read on this host yet; $(as_it_was)"
            ;;
        *)
            rc=1
            EFFECT="UNVERIFIED: the Secret was not read, so no gate was asked; $(as_it_was)"
            ;;
    esac
    ACTION="untouched: the Secret was not read, so the file is as it was"
    finish
fi
if [ -z "$V" ]; then
    if [ "$HELD" -eq 1 ]; then
        # A Secret that held a current and now holds none is not a state
        # a rotation passes through. The host keeps what it has, and
        # nothing here vouches for it.
        rc=1
        ACTION="untouched: the Secret holds NO current slot while this host holds a reader file — kept, not removed"
        EFFECT="UNVERIFIED: there is no current slot to ask the gates about, so the file this host holds was not vouched for this pass$(stale_cost)"
    else
        ACTION="untouched: nothing to deposit yet"
        EFFECT="not yet: the probe reader has no current slot — its first rotation has not been promoted — so nothing was deposited and no gate was asked; $(as_it_was)"
    fi
    finish
fi

if ! gates; then
    ACTION="untouched: no gate was asked, so nothing was written"
    EFFECT="$EFFECT; $(as_it_was)"
    finish
fi
# The header, by the estate token's own writer and under its own host
# rule (infra/lib/secret-header.sh): the value is handed to a shell
# function, never an argv, and to no host that rule does not name.
if ! machine_gate_value_header READER_HDR "$SCHEME://$HOST" "$V" 2>"$KERR"; then
    rc=1
    ACTION="untouched: no gate was asked, so nothing was written"
    EFFECT="UNVERIFIED: the header file could not be written ($(kerr)), so no gate was asked; $(as_it_was)"
    finish
fi
if [ -z "$READER_HDR" ]; then
    rc=1
    ACTION="untouched: no gate was asked, so nothing was written"
    EFFECT="UNVERIFIED: the value was WITHHELD from $SCHEME://$HOST — not loopback and not in BOSS_MACHINE_TOKEN_HOSTS — so no gate was asked; $(as_it_was)"
    finish
fi
ask
# The header file is gone before anything is written or said.
secret_header_close
READER_HDR=""

TOTAL="${#GATES[@]}"
if [ -n "$COLLIDE" ]; then
    rc=1
    ACTION="untouched: REFUSED on a collision, so nothing was written"
    EFFECT="REFUSED: COLLISION — gate(s)$COLLIDE name the probe reader's current slot by a BARE estate slot name, which says this value EQUALS an estate machine token; it is never deposited. Abandon the rotation and file a new one (review f09db7f0, F4); $(as_it_was)$(stale_cost)"
    echo "$ME: COLLISION — the probe reader's current slot is named as an ESTATE machine token by gate(s)$COLLIDE NOT DEPOSITED" >&2
elif [ -n "$REFUSE" ]; then
    rc=1
    ACTION="untouched: a gate does not know the value, so nothing was written"
    EFFECT="NOT ACCEPTED: gate(s)$REFUSE match the Secret's current slot to NO reader slot ($N_OK of $TOTAL gates said reader.current) — a gate that does not mount the reader directory, or a Secret written by a hand; $(as_it_was)$(stale_cost)"
elif [ -n "$BROKEN" ]; then
    rc=1
    ACTION="untouched: a gate did not answer as a gate, so nothing was written"
    EFFECT="UNVERIFIED: gate(s)$BROKEN ($N_OK of $TOTAL gates said reader.current); $(as_it_was)"
elif [ -n "$DARK" ]; then
    ACTION="untouched: a gate did not answer, so nothing was written"
    EFFECT="UNVERIFIED: no answer inside $GATE_BOUND_S seconds from gate(s)$DARK ($N_OK of $TOTAL gates said reader.current); the next tick asks again; $(as_it_was)"
elif [ -n "$BEHIND" ]; then
    ACTION="untouched: a gate is a refresh away from the Secret, so nothing was written"
    EFFECT="not yet: gate(s)$BEHIND name the Secret's current slot as another reader slot — a mount one refresh behind a promotion, or a Secret that moved after this pass read it ($N_OK of $TOTAL gates said reader.current); the next tick asks again; $(as_it_was)"
elif [ "$N_OK" -ne "$TOTAL" ] || [ "$TOTAL" -lt 1 ]; then
    rc=1
    ACTION="untouched: not every gate was counted, so nothing was written"
    EFFECT="UNVERIFIED: $N_OK of $TOTAL gates said reader.current and no other answer was recorded; $(as_it_was)"
else
    # --- 4. the file ---------------------------------------------------
    if [ -f "$DEST" ] && [ ! -L "$DEST" ] && [ "$(cat -- "$DEST" 2>/dev/null)" = "$V" ]; then
        if held_mode "$DEST" 600 2>"$KERR"; then
            ACTION="unchanged: the file already holds the Secret's current slot (${#V} bytes), held at 0600"
        else
            rc=1
            ACTION="NOT HELD: $DEST's owner and mode could not be held at root:root 0600 ($(kerr))"
        fi
    elif put; then
        ACTION="wrote the Secret's current slot (${#V} bytes) at 0600"
    else
        rc=1
        ACTION="NOT WRITTEN: $WHY; the file this host held is as it was"
    fi
    if [ "$rc" -eq 0 ] && [ -f "$DEST" ] && [ ! -L "$DEST" ] && [ "$(cat -- "$DEST" 2>/dev/null)" = "$V" ]; then
        EFFECT="reader.current at $N_OK of $TOTAL gates (every gated port of $PORTS, and the system of record's own): $DEST holds the value every gated port names reader.current (${#V} bytes), so a recorded probe on this host reads through the reader door"
    else
        rc=1
        EFFECT="NOT DEPOSITED: every gate ($N_OK of $TOTAL) names the Secret's current slot reader.current, but $DEST does not hold it — see the file line"
    fi
fi
finish
