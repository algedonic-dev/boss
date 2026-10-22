#!/usr/bin/env bash
#
# ops-runner-wake — start this host's ops-request runner the moment a
# request for this host is filed, instead of at the timer's next tick.
#
# WHY IT EXISTS (backlog ca6f40c2)
# --------------------------------
# The runner learned of a filed ops-request only from
# boss-ops-runner.timer, once a minute. Across the 199 requests the forge
# answered 2026-10-18T17:06Z..2026-10-19T08:30Z its `queued_s` read
# median 33 s, p90 67 s, max 107 s: the timer's phase, not the work —
# about 1.8 h a day of waiting in aggregate, ten minutes a day of track
# time on converge requests alone. Every filing already lands
# `jobs.job.created` on the record; nothing on the host listened.
#
# WHAT IT DOES, AND ALL IT CAN DO
# -------------------------------
# It holds one read open — the events service's SSE stream of the audit
# log, narrowed server-side to source=jobs and kind=jobs.job.created, on
# the system of record's own host — and when a frame names an
# ops-request whose metadata.host is THIS host, it runs
# `systemctl start boss-ops-runner.service`. That is the whole of its
# authority, and each part of it is fixed here:
#
#   * THE UNIT IS A LITERAL, never read from a frame. The frame is read
#     for one yes/no (jq, never a shell), plus the request id for the
#     journal line, which jq returns only when it is shaped like an id.
#     Nothing a frame carries reaches an argv, an environment, or a file
#     the runner reads: the runner starts exactly as the timer starts it,
#     and reads its queue from the jobs API itself, as it always has.
#     A forged or mistaken frame can at most start one pass early — the
#     act the timer performs every minute anyway.
#   * THE TIMER STAYS THE NET, and owes this nothing. The waker never
#     replaces a tick: when it is down, dark, or wrong, requests wait for
#     the minute they waited before. And it has no authority the timer
#     does not: it wakes the runner only while boss-ops-runner.timer is
#     active, so a runner retired (retire-ops-runner stops exactly the
#     timer) or stopped by a hand is never started by a wake. The unit is
#     PartOf= the timer as well, so the retire's one act stops it.
#   * A PASS THAT BEGINS AFTER THE FILING. The runner is a oneshot;
#     systemd merges a start into the job of a pass already running, so
#     a bare start during a pass would return with that pass — which read
#     its queue before this request existed. So: if a pass is running,
#     the first start waits it out and a second starts a fresh one; if
#     none is, one start is a fresh one. A frame reaches this stream only
#     from the audit log, which the relay writes after the filing's
#     transaction committed, so a pass begun after the frame sees it.
#   * A BURST IS ONE PASS. A train files its car probes back to back.
#     Frames already waiting when one matches are read off unparsed: each
#     was committed before the pass about to start, which answers them
#     all.
#
# WHAT IT OWES NOTHING TO, AND WHAT IT LOSES. A dropped or refused
# stream is a reconnect after a backoff (5 s, doubling to 5 min), said on
# one line with curl's own words. The stream anchors at the log's head
# when it connects, so a filing during a gap is the timer's — never
# replayed, never guessed. It exits 0 after OPS_WAKE_MAX_S (an hour) at a
# quiet moment and systemd starts it again: the forge runs units from
# root's tree by its `current` link, so a long-lived process would keep
# running a generation the next refresh may prune.
#
# NOT UNDER A TREE HELPER. boss-gcp starts its runner through
# gcp-root-launch.sh, whose `run` holds the root tree's SHARED lock until
# the interpreter exits, and the tree's refresh takes it EXCLUSIVE with a
# 120 s wait. A process that never exits would hold off every refresh,
# so install-ops-runner.sh installs this only where the runner runs from
# its tree directly, and says so where it does not.
#
# EXIT
#   0   the lifetime ran out (the unit restarts it)
#   78  no usable HOST_ID or system of record — a configuration fault the
#       unit does not restart (RestartPreventExitStatus=78), so it stays
#       red where somebody looks
#
# ENV
#   HOST_ID            the estate node id, from the installer's drop-in
#   BOSS_JOBS_URL      the system of record (/etc/boss/sor.env)
#   OPS_WAKE_MAX_S     the lifetime, seconds (default 3600; the tests use
#                      0 to end after one connection)
#   INSTALL_SYSTEMCTL  systemctl (the installer's own test seam)
#   BOSS_MACHINE_TOKEN_DIR  see infra/lib/secret-header.sh
# Nothing here is read from a frame, and the unit passes no environment
# a request can set.

set -uo pipefail

ME="ops-runner-wake"
say() { echo "$ME: $*" >&2; }

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"
RUNNER="boss-ops-runner.service"
TIMER="boss-ops-runner.timer"
MAX_S="${OPS_WAKE_MAX_S:-3600}"
case "$MAX_S" in '' | *[!0-9]*) MAX_S=3600 ;; esac
# A healthy stream sends a keep-alive every 15 s; a minute of nothing is
# a connection that died without closing.
IDLE_S=60

HOST="${HOST_ID:-}"
[[ "$HOST" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] || {
    say "REFUSED — HOST_ID is '${HOST}', not an estate node id. The installer's drop-in names the host; with no host this would wake on another host's requests. Exit 78."
    exit 78
}

# shellcheck source=infra/lib/sor.sh
. "$REPO/infra/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || {
    say "REFUSED — BOSS_JOBS_URL is not set (${BOSS_SOR_ENV:-/etc/boss/sor.env} carries none), so there is no record to listen to. Exit 78."
    exit 78
}

# The events service is the record's own host on the port table's
# `events` row (infra/forge/sor-ports.env, pinned to crates/core/boss-ports
# — read as data, never sourced; the way infra/forge/alert-lib.sh finds
# the people port). No row, no guessed port.
events_port=$(grep -Ev '^[[:space:]]*(#|$)' "$REPO/infra/forge/sor-ports.env" 2>/dev/null \
    | sed -n 's/^events=\([0-9][0-9]*\)[[:space:]]*$/\1/p' | sed -n 1p)
[ -n "$events_port" ] || {
    say "REFUSED — $REPO/infra/forge/sor-ports.env names no events port, so the stream's address is unknown. Exit 78."
    exit 78
}
scheme="${BOSS_JOBS_URL%%://*}"
hostport="${BOSS_JOBS_URL#*://}"
hostport="${hostport%%/*}"
STREAM="$scheme://${hostport%%:*}:$events_port/api/events/stream?source=jobs&kind=jobs.job.created"

# The machine token, PRESENTED and never required (design 6805c764).
MT_HDR=""
if [ -r "$REPO/infra/lib/secret-header.sh" ]; then
    # shellcheck source=infra/lib/secret-header.sh
    . "$REPO/infra/lib/secret-header.sh"
fi

# The one yes/no asked of a frame, and the id for the journal line. jq
# prints the id only when the frame is a job-created event for an
# ops-request filed for this host AND the id is shaped like one; it
# prints nothing for anything else, malformed input included.
matches() { # <frame body>
    printf '%s' "$1" | jq -nr --arg h "$HOST" '
        (try input catch null) as $e
        | select(($e | type) == "object"
            and $e.source == "jobs" and $e.kind == "jobs.job.created"
            and ($e.payload | type) == "object"
            and $e.payload.kind == "ops-request"
            and (($e.payload.metadata // {}) | type) == "object"
            and $e.payload.metadata.host == $h)
        | $e.payload.id
        | select(type == "string" and test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"))' \
        2>/dev/null
}

word() { # <is-active output> — the first line, read by the WORD
    printf '%s' "${1%%$'\n'*}"
}

# The one act, and its one condition.
wake() { # <request id>
    local t s
    t=$(word "$("$SYSTEMCTL" is-active -- "$TIMER" 2>/dev/null)")
    if [ "$t" != active ]; then
        say "request ${1:0:8} filed for $HOST; $TIMER is ${t:-unreadable}, so not waking — the runner's own timer decides whether it runs"
        return 0
    fi
    s=$(word "$("$SYSTEMCTL" is-active -- "$RUNNER" 2>/dev/null)")
    case "$s" in
        active | activating | reloading | deactivating)
            # Merges into the running pass's job and returns with it.
            "$SYSTEMCTL" start -- "$RUNNER" >/dev/null 2>&1 || true
            say "request ${1:0:8} filed for $HOST while a pass was $s; waited it out, starting a fresh pass"
            ;;
        *)
            say "request ${1:0:8} filed for $HOST; starting a pass"
            ;;
    esac
    "$SYSTEMCTL" start -- "$RUNNER" >/dev/null 2>&1 \
        || say "the pass for ${1:0:8} ended non-zero; $RUNNER's own journal and packet carry why"
}

backoff=5
while :; do
    machine_token_header MT_HDR "$STREAM" || MT_HDR=""
    errf=$(mktemp) || { say "FAILED — no temporary file for curl's errors"; errf=/dev/null; }
    started=$SECONDS
    # The identity and the URL stay on consecutive lines (the pair
    # every_estate_read_is_signed.rs reads as this door's evidence).
    exec {S}< <(exec curl -sS -N --fail --connect-timeout 10 ${MT_HDR:+-H "$MT_HDR"} \
        -H "x-boss-user: $(sor_reader_header automation:ops-runner-wake)" \
        "$STREAM" 2>"$errf")
    cpid=$!
    why="the stream ended"
    while :; do
        IFS= read -r -t "$IDLE_S" -u "$S" line
        rc=$?
        if [ "$rc" -ne 0 ]; then
            [ "$rc" -le 128 ] || why="no keep-alive for ${IDLE_S}s"
            break
        fi
        case "$line" in
            data:*)
                id=$(matches "${line#data:}")
                [ -n "$id" ] || continue
                # Read off the burst behind it; the pass below answers it.
                while IFS= read -r -t 0.2 -u "$S" _; do :; done
                wake "$id"
                ;;
            event:*failed*)
                why="the server reported a failed read"
                ;;
            :*)
                # A keep-alive: the stream is quiet. The only place the
                # lifetime is checked mid-stream, so a burst is never cut.
                if [ "$SECONDS" -ge "$MAX_S" ]; then
                    why="lifetime"
                    break
                fi
                ;;
        esac
    done
    kill "$cpid" 2>/dev/null
    exec {S}<&-
    [ "$why" = lifetime ] \
        || say "$why ($STREAM): $(head -c 300 "$errf" 2>/dev/null | tr '\n' ' ')— until it is back the timer is the only wake"
    [ "$errf" = /dev/null ] || rm -f "$errf"
    if [ "$SECONDS" -ge "$MAX_S" ]; then
        say "lifetime of ${MAX_S}s reached; exiting for a fresh start from the current tree"
        exit 0
    fi
    [ $((SECONDS - started)) -lt 60 ] || backoff=5
    say "reconnecting in ${backoff}s"
    sleep "$backoff"
    backoff=$((backoff * 2))
    [ "$backoff" -le 300 ] || backoff=300
done
