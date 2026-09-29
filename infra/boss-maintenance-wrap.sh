#!/usr/bin/env bash
# boss-maintenance-wrap — ensure the open maintenance Job a timer run
# will complete (internal-forge.md Q6; the maintenance family).
#
#   ./infra/boss-maintenance-wrap.sh <kind> <chore-label>
#   e.g. ./infra/boss-maintenance-wrap.sh maintenance-backup "Nightly backup"
#
# Called from the timer service's ExecStartPre. If an open Job of the
# kind exists (yesterday's run FAILED and nobody closed it), reuse it:
# today's successful run completing it is the recovery, and the pile
# staying at one open Job per chore keeps boss-step's single-open
# contract intact. Otherwise spawn today's Job.
#
# The visibility contract, stated once: the timer is the EXECUTOR,
# the Job is the VISIBILITY. ExecStopPost → boss-step.sh records the
# verdict from systemd's $SERVICE_RESULT: success completes `run` as
# `ok` and the packet closes "Maintenance completed"; a run that died
# completes it with how (exit-code, timeout, signal) and the packet
# closes "Maintenance failed". A Job still OPEN at `run` is therefore a
# run in progress or a host that rebooted mid-run — which is what the
# recovery above is for. (Until 2026-09-05 the completion sat on
# ExecStartPost, which systemd skips on failure, so a failed run left
# its packet open looking exactly like one in progress.)
#
# Deliberately NOT the dispatcher's schedule runner: that fires on
# SIM-day boundaries, and at warp a daily rule fires every couple of
# wall-minutes. Maintenance is wall-clock work.

set -euo pipefail

KIND="${1:?usage: boss-maintenance-wrap.sh <kind> <chore-label>}"
LABEL="${2:?chore label required}"

# WHICH HOST THIS RUN IS — and so which packet is its own (backlog
# 79f7678b, 2026-09-26). One kind can run on more than one host:
# maintenance-estate-observe-host runs on the forge every 15 minutes and
# on boss-gcp daily. This looked the open packet up by KIND alone and
# stamped no host, so boss-gcp's run found an open forge packet, took it
# as "recovery", and its ExecStopPost completed the forge's run step;
# and on /it/estate, which reads `metadata.host`, one host's observer
# could hide the other's silence. So the host is stamped on the packet
# and the lookup is kind + host.
#
# The host is the unit's HOST_ID, else the BOSS_NODE_ID a converge unit
# declares, else the machine's own name. In a Kubernetes pod with
# neither, NO host: a pod's name is new on every CronJob run, and keying
# on it would orphan the failed run's packet the next run is meant to
# recover, so those chores keep the kind-only key they had. A packet
# with no host — every packet filed before this — still matches, so an
# open one is recovered rather than left open forever under a key it
# never carried. boss-step.sh derives the host the same way, pinned by
# crates/core/boss-testing/tests/a_loop_packet_names_its_host.rs.
HOST="${HOST_ID:-${BOSS_NODE_ID:-}}"
if [ -z "$HOST" ] && [ -z "${KUBERNETES_SERVICE_HOST:-}" ]; then
    HOST=$(uname -n)
fi


# THE EXECUTOR NEVER WAITS ON ITS VISIBILITY — whatever the system of
# record does. On 2026-09-05 the SoR was dark for four hours because the
# loop that could have restored it, cluster-deploy-runner, has this
# script as its ExecStartPre: the wrap failed, systemd never started the
# converge, and the fix sat unbuilt until a human ran it by hand. That
# fix covered only a DARK API; one that ANSWERED an error still aborted
# the run — and the open-packet read below asks policy, which answers
# 503 while it is down, so a policy outage stopped the converge that
# would deploy the policy fix (backlog 9fd7f51e, 2026-09-27). So: any
# failure to reach the SoR or to be answered by it — refused, 5xx, 4xx,
# a timeout, a reply of the wrong shape — costs this run its packet,
# loudly, and never its work; every such path exits 0. The script exits
# nonzero only when no SoR is named (78, below) or on a defect `set -e`
# stops on; every unit runs it from `ExecStartPre=-`, so systemd logs
# either and starts the chore anyway, and the EXIT trap cannot change
# the status (it is `|| true` throughout).
#
# LOUD, AND KEPT. A journal line alone is a check nobody reads, and an
# alarm that reports through its subject dies with it (CLAUDE.md
# §Diagnosis). So each miss is also kept as one small JSON row (id, when,
# the wrap's phrase, curl's FINAL line and its attempt count — the whole
# retry log goes to the journal) in a ledger in the running user's home.
# The next packet this host spawns for the kind carries the newest
# UNRECORDED_CAP rows as `unrecorded_runs` and counts the rest; the
# ledger holds at most LEDGER_MAX rows, and a drop past that is counted
# on the next packet too. A pod keeps no ledger (its filesystem ends with
# the run), nor does a user with no home ($HOME, else the passwd entry,
# where a root unit with no User= keeps its own); the line says so.
UNRECORDED_CAP=50
LEDGER_MAX=1000
LEDGER="" NOT_KEPT=""
if [ -n "${KUBERNETES_SERVICE_HOST:-}" ]; then
    NOT_KEPT="this is a pod, and its filesystem ends with the run"
else
    ledger_home="${HOME:-}"
    if [ -z "$ledger_home" ]; then
        ledger_home=$(getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6) || ledger_home=""
    fi
    if [ -n "$ledger_home" ]; then
        LEDGER="$ledger_home/.boss-maintenance-unrecorded/$KIND${HOST:+@$HOST}.jsonl"
    else
        NOT_KEPT="uid $(id -u) has no HOME and no home directory"
    fi
fi
# curl's stderr (so a miss can name the status) and today's packet body.
work=$(mktemp -d "${TMPDIR:-/tmp}/boss-maintenance-wrap.XXXXXX" 2>/dev/null) || work=""
curl_err="${work:+$work/curl.err}"

# ONE RUN AT A TIME TOUCHES THE LEDGER. `flock` on `<ledger>.lock` (never
# deleted, or a second run could lock another inode) is taken at the
# first touch and held to exit, so a hand run beside a timer run cannot
# sweep a live claim or append into a ledger being claimed. A run that
# waits BOSS_WRAP_LOCK_WAIT (10) seconds in vain files no packet, and a
# miss it keeps is written as `.writing-*` and only then renamed
# `.stray-*` — so the claiming run's sweep never reads it half-written.
# With no flock the ledger is used unlocked, and no claim is swept: one
# might belong to a run still in flight.
LOCK_STATE=""
lock_ledger() {
    [ -z "$LOCK_STATE" ] || return 0
    LOCK_STATE=none
    [ -n "$LEDGER" ] && mkdir -p "$(dirname "$LEDGER")" 2>/dev/null || return 0
    if ! command -v flock >/dev/null 2>&1; then
        echo "boss-maintenance-wrap: no flock on this host — the ledger $LEDGER is used unlocked" >&2
        LOCK_STATE=unlocked
    elif { exec 9>>"$LEDGER.lock"; } 2>/dev/null; then
        LOCK_STATE=busy
        ! flock -w "${BOSS_WRAP_LOCK_WAIT:-10}" 9 || LOCK_STATE=held
    fi
}
ledger_is_ours() { [ "$LOCK_STATE" = held ] || [ "$LOCK_STATE" = unlocked ]; }

# The oldest rows past LEDGER_MAX go. The count is written BEFORE the
# rows are dropped, so a run killed between the two over-counts rather
# than losing the count.
trim_ledger() {
    local n t prev
    [ -f "$LEDGER" ] && n=$(wc -l 2>/dev/null <"$LEDGER") && [ "$n" -gt "$LEDGER_MAX" ] || return 0
    t=$(mktemp "$LEDGER.tmp-XXXXXX" 2>/dev/null) || return 0
    prev=$(cat "$LEDGER.dropped" 2>/dev/null) || prev=0
    case "${prev:-x}" in *[!0-9]*) prev=0 ;; esac
    if tail -n "$LEDGER_MAX" "$LEDGER" >"$t" 2>/dev/null \
        && echo "$((prev + n - LEDGER_MAX))" >"$LEDGER.dropped" 2>/dev/null \
        && mv -f "$t" "$LEDGER" 2>/dev/null; then
        echo "    the ledger passed $LEDGER_MAX rows; the oldest $((n - LEDGER_MAX)) were dropped, and the next packet counts them" >&2
    else
        rm -f "$t" 2>/dev/null || true
    fi
}

keep_miss() {  # $1 = what went wrong, as a phrase
    local why="$1" last="" attempts=0 id row w
    if [ -n "$curl_err" ] && [ -s "$curl_err" ]; then
        cat "$curl_err" >&2
        last=$(grep -v '^[[:space:]]*$' "$curl_err" | tail -n 1) || last=""
        attempts=$(grep -c '^curl: (' "$curl_err") || attempts=0
    fi
    echo "boss-maintenance-wrap: UNRECORDED RUN — $KIND${HOST:+ on $HOST} runs WITHOUT its packet: $why${last:+ — $last}. The chore goes on; only this run's visibility is lost." >&2
    if [ -z "$LEDGER" ]; then
        echo "    NOT kept for replay: $NOT_KEPT — this line is the only record of the miss" >&2
        return 0
    fi
    id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null) || id="$(date -u +%s)-$$-$RANDOM"
    row=$(jq -nc --arg id "$id" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --arg why "$why" --arg last "$last" --argjson attempts "$attempts" \
        '{id: $id, at: $at, why: $why[:500]}
         + (if $last == "" then {} else {curl: $last[:500]} end)
         + (if $attempts > 0 then {attempts: $attempts} else {} end)' 2>/dev/null) || row=""
    lock_ledger
    if [ -n "$row" ] && ledger_is_ours && printf '%s\n' "$row" >>"$LEDGER" 2>/dev/null; then
        trim_ledger
        echo "    kept in $LEDGER — the next $KIND packet spawned here carries it as unrecorded_runs" >&2
    elif [ -n "$row" ] && [ "$LOCK_STATE" = busy ] \
        && w=$(mktemp "$LEDGER.writing-XXXXXX" 2>/dev/null) \
        && printf '%s\n' "$row" >"$w" 2>/dev/null && mv -f "$w" "${w/.writing-/.stray-}" 2>/dev/null; then
        echo "    kept in ${w/.writing-/.stray-} (another run holds the ledger lock) — the next claiming run carries it" >&2
    else
        echo "    NOT kept for replay: $LEDGER could not be locked or written — this line is the only record of the miss" >&2
    fi
}

# THE CLAIM: one file the rows this run carries are gathered into (TODAY'S
# PACKET, below). Until the packet lands it is their only copy, so every
# way out that is not a landed packet folds it back first — the claimed
# rows ahead of any kept since, so the disk cap keeps dropping the oldest.
# While a POST carrying them is in flight, POSTING is set: a run that
# leaves then through a trappable signal (TERM, HUP, INT) or an exit
# stamps each row `possibly_recorded_at`, because the SoR may already
# hold it. A SIGKILL, an OOM kill or a crash runs no trap and leaves the
# claim unstamped on disk; the next locked sweep stamps it (THE CLAIM IS
# GATHERED, below). A claim that cannot be folded back stays on disk too.
CLAIM="" POSTING=""
stamped() {  # $1 = possibly_recorded_at, $2 = a file of rows; the rows, stamped, on stdout
    jq -rR --arg now "$1" '
        (fromjson? // null) as $j
        | if ($j | type) == "object"
          then $j + {possibly_recorded_at: ($j.possibly_recorded_at // $now)} | tojson
          else . end' "$2"
}
fold_back() {  # $1 = possibly_recorded_at to stamp ('' for none)
    local t=""
    [ -n "$CLAIM" ] || return 0
    if [ -n "${1:-}" ]; then
        { t=$(mktemp "$LEDGER.tmp-XXXXXX") && stamped "$1" "$CLAIM" >"$t" && mv -f "$t" "$CLAIM"; } 2>/dev/null \
            || { rm -f "$t" 2>/dev/null; echo "    the claimed rows could not be marked possibly_recorded_at" >&2; }
    fi
    if [ "$LOCK_STATE" = held ]; then
        { [ ! -e "$LEDGER" ] || cat "$LEDGER" >>"$CLAIM"; } 2>/dev/null && mv -f "$CLAIM" "$LEDGER" 2>/dev/null && CLAIM=""
    else
        cat "$CLAIM" >>"$LEDGER" 2>/dev/null && rm -f "$CLAIM" && CLAIM=""
    fi
    [ -z "$CLAIM" ] || echo "    the claimed rows in $CLAIM could not be folded back — left on disk for the next run" >&2
    CLAIM=""
    trim_ledger
}
on_exit() {
    local stamp=""
    [ -z "$POSTING" ] || stamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    fold_back "$stamp" || true
    [ -z "$work" ] || rm -rf "$work" || true
}
trap 'on_exit || true' EXIT
trap 'exit 143' TERM
trap 'exit 129' HUP
trap 'exit 130' INT
run_unrecorded() {  # $1 = what the SoR did, as a phrase; $2 = possibly_recorded_at for the claim
    fold_back "${2:-}" || true
    keep_miss "$1"
    exit 0
}

# WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION.
#
# This read `${BOSS_JOBS_URL:-http://127.0.0.1:7900}` until 2026-08-17.
# On a box whose local instance is not the system of record that
# fallback is a silent redirect, and it ran for weeks: the backup,
# audit-integrity and ledger-replay timers each left 7 packets on
# boss-gcp's demo instance and ZERO on the cluster SoR, while firing
# exactly on schedule. Every check in `timers-leave-a-packet` passed
# the whole time, because none of them can see WHERE a packet lands.
#
# The estate model already knows: `service_instances.authoritative` is
# true for boss-cluster and false for boss-gcp-local. It was inert
# metadata — nothing consulted it, and a plausible default beat it.
#
# So: no default. A maintenance tool with no system of record named
# files no packet anywhere and exits 78. Since backlog 9fd7f51e every
# unit runs this with `-`, so systemd logs the 78 and starts the chore
# anyway — the unit is NOT marked failed — which is why the miss is also
# kept in the ledger like any other, to ride the first packet this host
# files once the address is back. Every unit reads BOSS_JOBS_URL from
# the host's /etc/boss/sor.env (infra/estate/estate.toml, rendered by
# the converge).
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$(basename "$0"): BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    Defaulting to 127.0.0.1 is how nightly maintenance packets spent weeks" >&2
    echo "    landing on a non-authoritative instance (2026-08-17). Name the system of" >&2
    echo "    record explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://<jobs-api-host>:<port> $(basename "$0") ..." >&2
    echo "    A unit reads it from /etc/boss/sor.env with EnvironmentFile= (timers-leave-a-packet check 7)." >&2
    keep_miss "BOSS_JOBS_URL is not set, so no system of record was named and no packet was filed anywhere"
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="${BOSS_JOBS_URL}"
BOSS_USER='{"id":"automation:maintenance-timer","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# Transport-retry curl (25e518c0): a deploy roll takes the API away
# for ~45s and that must not kill the chore. Resolved next-to-self so
# the same line works from the repo checkout (boss-gcp timers) and
# /usr/local/bin (the image).
API_CURL="$(dirname "$0")/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
# EVERY REQUEST HAS A DEADLINE. A SoR that accepts the connection and
# never answers held this ExecStartPre until the unit's own
# TimeoutStartSec — 90 minutes on the converge, whose ExecStart then
# never ran: an arm that needs the patient (re-review of 324597eb,
# 2026-09-27). curl's own 28 on the deadline is a miss like any other.
# BOSS_WRAP_MAX_TIME exists so a test need not wait the 30 s.
MAX_TIME="${BOSS_WRAP_MAX_TIME:-30}"

# The machine token rides to curl in a 0600 file, never in its argv,
# where every local user reads it in ps (backlog 5f3ad356). lib/ next to
# this file, like the curl helper: infra/lib in a checkout,
# /usr/local/bin/lib in the image. Made after the EXIT trap above, so
# the lib's cleanup is chained in front of on_exit rather than lost.
SECRET_LIB="$(dirname "$0")/lib/secret-header.sh"
[ -r "$SECRET_LIB" ] \
    || run_unrecorded "$SECRET_LIB is missing, so the machine token could only have been sent in curl's command line and nothing was sent"
# shellcheck source=infra/lib/secret-header.sh
. "$SECRET_LIB"
secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    || run_unrecorded "the machine token's header file could not be written, so nothing was sent"

reply=""; rc=0
reply=$("$API_CURL" -fsS --max-time "$MAX_TIME" -H "x-boss-user: $BOSS_USER" \
    "$BASE/api/jobs?kind=$KIND&status=open&limit=50" 2>"${curl_err:-/dev/stderr}") || rc=$?
if [ "$rc" -ne 0 ]; then
    run_unrecorded "the jobs API at $BASE did not answer the open-packet read (curl exit $rc)"
fi
# `.data` missing from the reply means the jobs API changed shape. Read
# as zero open Jobs it would spawn a duplicate, so it spawns nothing —
# and, like every other bad answer, it does not stop the chore. Only
# this host's packets, and hostless ones, are this run's (see WHICH
# HOST above).
open_count=$(printf '%s' "$reply" | jq --arg host "$HOST" '
    .data | if . == null then error("jobs reply has no .data") else
        map(select((.metadata.host // "") as $h | $h == $host or $h == "")) | length
    end' 2>"${curl_err:-/dev/stderr}") \
    || run_unrecorded "the jobs API at $BASE answered the open-packet read in a shape this script cannot read, so no packet is spawned (it could be a duplicate)"

if [ "$open_count" != "0" ]; then
    echo "boss-maintenance-wrap: open $KIND Job exists${HOST:+ for $HOST} — this run will complete it (recovery)"
    exit 0
fi

# TODAY'S PACKET IS BUILT IN A FILE AND SENT FROM IT. Nothing of variable
# length ever rides as an argument: until 2026-09-27 the ledger rode as
# one argv string (jq --argjson, curl -d), and past Linux's 128 KiB
# MAX_ARG_STRLEN curl POSTed an EMPTY body and the ledger never emptied
# again. jq reads the claim file itself, curl sends --data-binary @file
# (which a retry re-reads), and a body that fails to build is a loud miss
# with nothing sent.
#
# Under the lock, the claim is ONE fresh `mktemp` file (never a
# pid-derived name, which a later run with the same pid overwrote) into
# which any claim or stray a killed or locked-out run left, then the
# ledger, are gathered; each source goes only once it is copied. A
# ledger jq cannot even read (a probe that writes nothing, so a full
# TMPDIR cannot trip it) is set aside as `.unreadable-*` and COUNTED on
# every packet until one counting it lands — a count, never a list of
# names, which is what wedged every packet in the fourth review of
# fe1309fe.
[ -n "$work" ] || run_unrecorded "there is no temporary directory (TMPDIR ${TMPDIR:-/tmp}) to build today's packet in, so nothing was sent"
payload="$work/packet.json"
unreadable=0 unmovable="" dropped_on_disk=0
if [ -n "$LEDGER" ]; then
    lock_ledger
    [ "$LOCK_STATE" != busy ] \
        || run_unrecorded "another run of $KIND holds $LEDGER.lock past ${BOSS_WRAP_LOCK_WAIT:-10}s, so this run files no packet rather than race it for the same rows"
fi
if [ -n "$LEDGER" ] && ledger_is_ours; then
    find "$(dirname "$LEDGER")" -maxdepth 1 -type f -name "$(basename "$LEDGER").*.reported" \
        -mtime +30 -delete 2>/dev/null || true
    # Under the lock no one else is mid-write, so a `.tmp-*` is an orphan
    # of a run killed mid-trim or mid-stamp: a copy, never the only one.
    [ "$LOCK_STATE" != held ] || rm -f "$LEDGER".tmp-* 2>/dev/null || true
    if [ -e "$LEDGER" ] && ! jq -n --rawfile l "$LEDGER" empty 2>/dev/null; then
        aside="$LEDGER.unreadable-$(date -u +%Y%m%dT%H%M%SZ)"
        mv -f "$LEDGER" "$aside" 2>/dev/null || { unmovable="$LEDGER"; aside="nowhere: it could not be moved"; }
        echo "boss-maintenance-wrap: the ledger $LEDGER cannot be read — set aside as $aside; every packet says so until one lands" >&2
    fi
    for f in "$LEDGER".unreadable-*; do
        case "$f" in *.reported) ;; *) [ ! -e "$f" ] || unreadable=$((unreadable + 1)) ;; esac
    done
    sources=()
    if [ "$LOCK_STATE" = held ]; then
        for f in "$LEDGER".claim-* "$LEDGER".stray-*; do [ ! -e "$f" ] || sources+=("$f"); done
    fi
    [ -n "$unmovable" ] || [ ! -e "$LEDGER" ] || sources+=("$LEDGER")
    # THE CLAIM IS GATHERED one source at a time. A `.claim-*` left on
    # disk exists only after an unclean finish — possibly mid-POST — so
    # its rows are stamped possibly_recorded_at as they are gathered; a
    # stray and the ledger were never in a POST. A copy cut short (ENOSPC)
    # would leave rows in the claim AND the source, sent now and again
    # later, so the claim is cut back to its size before that source and
    # gathering stops; the source keeps its rows for a later run.
    if [ "${#sources[@]}" -gt 0 ] && CLAIM=$(mktemp "$LEDGER.claim-XXXXXX" 2>/dev/null); then
        now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        for f in "${sources[@]}"; do
            size=$(wc -c 2>/dev/null <"$CLAIM") || size=""
            case "$f" in
                *.claim-*) { stamped "$now" "$f" >>"$CLAIM" && rm -f "$f"; } 2>/dev/null ;;
                *) { cat "$f" >>"$CLAIM" && rm -f "$f"; } 2>/dev/null ;;
            esac && continue
            if [ -n "$size" ] && truncate -s "$size" "$CLAIM" 2>/dev/null; then
                echo "boss-maintenance-wrap: could not claim $f — its rows wait for a later run, still in it" >&2
            else
                echo "boss-maintenance-wrap: could not claim $f, nor undo the partial copy — nothing is carried today; the rows go back to the ledger, where a row held twice is deduped by id" >&2
                fold_back ""
            fi
            break
        done
        [ -z "$CLAIM" ] || [ -s "$CLAIM" ] || { rm -f "$CLAIM"; CLAIM=""; }
    fi
    dropped_on_disk=$(cat "$LEDGER.dropped" 2>/dev/null) || dropped_on_disk=0
    case "${dropped_on_disk:-x}" in *[!0-9]*) dropped_on_disk=0 ;; esac
fi

build_packet() {  # $1 = carry | hold, $2 = why the rows are held back
    jq -nR --arg kind "$KIND" --arg host "$HOST" --arg title "$LABEL — $(date +%F)" \
        --arg mode "$1" --arg because "${2:-}" --arg unmovable "$unmovable" \
        --argjson disk "$dropped_on_disk" --argjson unreadable "$unreadable" \
        --argjson cap "$UNRECORDED_CAP" '
        # A line that is not a JSON object, is too long to be a row the
        # wrap wrote, or holds a NUL (a byte or \u0000) rides as its raw
        # text, capped at 500 characters with NULs removed.
        [inputs | select(length > 0)
            | (fromjson? // null) as $j
            | if ($j | type) == "object" and (($j | tojson | length) <= 2000)
                 and (explode | any(. == 0) | not) and (contains("\\u0000") | not)
              then $j
              else {raw: (.[:2000] | explode | map(select(. != 0)) | implode | .[:500])} end]
        # One row per id (a fold-back cut short can leave a copy; copies
        # merge, so a possibly_recorded_at survives), oldest first, raw last.
        | (map(select((.id? | type) == "string")) | group_by(.id) | map(add))
          + map(select((.id? | type) != "string"))
        | sort_by(.at // "~") as $rows
        | ($rows | length) as $n
        | (if $n > $cap then $rows[($n - $cap):] else $rows end) as $carried
        | ([$carried[] | select(.possibly_recorded_at? != null)] | length) as $maybe
        | {
            kind: $kind,
            subject: {subject_kind: "custom", id: ("infra/" + $kind)},
            title: $title,
            owner_id: "emp-bootstrap-admin",
            priority: "standard",
            status: "open",
            metadata: ({chore: $kind}
                + (if $host == "" then {} else {host: $host} end)
                + if $mode == "hold" then {unrecorded_runs_held_back: {rows: $n, because: $because[:500]}}
                  elif $n == 0 then {}
                  else {unrecorded_runs: $carried}
                    + (if $maybe == 0 then {} else {unrecorded_runs_possibly_repeated: $maybe} end)
                    + (if $n == ($carried | length) then {} else {unrecorded_runs_dropped: {
                        count: ($n - ($carried | length)),
                        first_at: (($rows[0] | objects | .at) // null)}} end) end
                + (if $disk == 0 then {} else {unrecorded_runs_dropped_on_disk: $disk} end)
                + (if $unreadable == 0 then {} else {unrecorded_ledgers_unreadable: $unreadable} end)
                + (if $unmovable == "" then {} else {unrecorded_ledger_unmovable: $unmovable} end)),
            tags: ["maintenance"]
        }' "${CLAIM:-/dev/null}" >"$payload" 2>"$curl_err"
}
post_packet() {  # $1 = 1 when the body carries the claimed rows
    local rc=0
    POSTING="${1:-}"
    "$API_CURL" -fsS --max-time "$MAX_TIME" -X POST "$BASE/api/jobs" \
        -H "x-boss-user: $BOSS_USER" -H "content-type: application/json" \
        ${MT_HDR:+-H "$MT_HDR"} \
        --data-binary "@$payload" >/dev/null 2>"$curl_err" || rc=$?
    POSTING=""
    return "$rc"
}
answered() { grep -qE 'returned error: [0-9]{3}' "$curl_err"; }

build_packet carry \
    || run_unrecorded "today's packet could not be built, so nothing was sent and every kept row stays in the ledger"
[ -s "$payload" ] || run_unrecorded "today's packet was built EMPTY, so nothing was sent and every kept row stays in the ledger"
carrying="${CLAIM:+1}"
rc=0
post_packet "$carrying" || rc=$?
# A 4xx while carrying rows may be a row the API never accepts. The
# packet goes once more WITHOUT them and says it held them back; the rows
# stay in the ledger unchanged either way, bounded by LEDGER_MAX alone.
if [ "$rc" -ne 0 ] && [ -n "$carrying" ] && grep -qE 'returned error: 4[0-9]{2}' "$curl_err"; then
    refused=$(grep -v '^[[:space:]]*$' "$curl_err" | tail -n 1) || refused="curl exit $rc"
    cat "$curl_err" >&2
    echo "boss-maintenance-wrap: the jobs API refused today's packet while it carried the kept rows ($refused) — sending it once more without them; they stay in the ledger" >&2
    build_packet hold "$refused" \
        || run_unrecorded "today's packet could not be rebuilt without the kept rows, so nothing was sent"
    carrying="" rc=0
    post_packet "" || rc=$?
fi
if [ "$rc" -ne 0 ]; then
    # Only curl 6 and 7 (nothing sent) and an HTTP status line (a definite
    # answer) say the packet did not land. Any other exit may follow a
    # POST the SoR committed. POST /api/jobs does answer a create whose
    # caller-chosen id names an identical packet with 200
    # already_admitted (558396ff), but the wrap sends no id today: the
    # next run's packet is not the same body (a new title date, and the
    # rows it carries change with every miss), so a key would need the id
    # and the body kept with the claim across runs. Until then the rows
    # go back stamped possibly_recorded_at, and the next packet counts them.
    if [ -n "$carrying" ] && [ "$rc" != 6 ] && [ "$rc" != 7 ] && ! answered; then
        run_unrecorded "the jobs API at $BASE may or may not have taken today's packet (curl exit $rc); its rows go back marked possibly_recorded_at" \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    fi
    run_unrecorded "the jobs API at $BASE did not accept today's packet (curl exit $rc)"
fi

# LANDED. Only now are the carried rows gone, the disk-cap count spent
# (only what this packet counted: a trim since may have added to it), and
# the set-aside ledgers reported. Held-back rows go back to the ledger.
if [ -n "$carrying" ]; then
    rm -f "$CLAIM" 2>/dev/null || true
    CLAIM=""
    echo "boss-maintenance-wrap: the packet carries the runs the jobs API could not record — $(jq -c '.metadata | {carried: (.unrecorded_runs | length), dropped: (.unrecorded_runs_dropped.count // 0)}' "$payload" 2>/dev/null || echo 'count unreadable')"
fi
if [ "$dropped_on_disk" -gt 0 ]; then
    left=$(cat "$LEDGER.dropped" 2>/dev/null) || left=0
    case "${left:-x}" in *[!0-9]*) left=0 ;; esac
    left=$((left - dropped_on_disk))
    if [ "$left" -gt 0 ]; then echo "$left" >"$LEDGER.dropped"; else rm -f "$LEDGER.dropped"; fi 2>/dev/null || true
fi
fold_back ""
for f in ${LEDGER:+"$LEDGER".unreadable-*}; do
    case "$f" in *.reported) ;; *) [ ! -e "$f" ] || { mv -f "$f" "$f.reported" && touch "$f.reported"; } 2>/dev/null || true ;; esac
done
echo "boss-maintenance-wrap: spawned today's $KIND Job${HOST:+ for $HOST}"
