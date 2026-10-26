#!/usr/bin/env bash
# alert-lib — an alert is a PACKET in David's queue (David, 2026-09-05:
# "Let's use the ops-request packets for alerts for now"). Sourced by
# cluster-watchdog.sh; tested by infra/lint/alerts-are-packets.sh.
#
# WHY A SPOOL. The alert that matters most is "the system of record is
# dark", and a packet can only be filed through the system of record.
# So an alert that cannot be filed is kept (one file per alert under
# ALERT_SPOOL) and filed by the next run that reaches the API, with its
# original `raised_at` in the body — the same retain-and-replay shape as
# the estate observer's readings. The forge journal carries every alert
# the moment it is raised, whatever the API is doing.
#
# Shape: an urgent backlog-item owned by the PLATFORM OWNER, titled
# from the alert, with the facts in metadata. Not an ops-request: that
# kind asks a host to run a verb; this one asks a person to look.
#
# WHO THE OWNER IS (backlog 3c23662d — until 2026-09-18 this file wrote
# a named person into every alert). BOSS_PLATFORM_OWNER when the unit
# carries it; else the people registry's first active platform-admin
# hire, read the way this file reads the system of record (the people
# service is the same address on boss-ports' people port — sor-ports.env
# beside this file, the table the unattended prove door hands its reader); else
# NOBODY, an empty owner_id the jobs API resolves from the kind's
# owner_role or refuses by name. Never a literal. The read is
# best-effort with a short timeout on purpose: the alert that matters
# most is "the system of record is dark", and a dark registry must not
# stop the alert being kept.

ALERT_SPOOL="${ALERT_SPOOL:-/var/tmp/boss-alert-spool}"
# The system of record, from /etc/boss/sor.env (infra/lib/sor.sh) — no
# fallback address: an alert filed against a wrong instance is an alert
# nobody reads (backlog 5222163e).
# shellcheck source=infra/lib/sor.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
sor_require JOBS_API
ALERT_API="$JOBS_API"
# The machine token's one shell reader — sourced when it is there, and
# only defines functions; alert_machine_headers says what it is for.
# shellcheck source=infra/lib/secret-header.sh
if [ -r "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" ]; then . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh"; fi
# Who files: the watchdog unless the sourcing script names itself
# (the cluster converge signs its tenant-check refusals as
# automation:cluster-deploy-runner, backlog 1af5119d) — an alert that
# credits the wrong loop sends a reader to the wrong journal.
ALERT_ACTOR="${ALERT_ACTOR:-automation:cluster-watchdog}"
ALERT_USER='{"id":"'"$ALERT_ACTOR"'","role":"platform-admin","access_tier":"operator"}'
ALERT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The two header variables are this lib's, made only by
# alert_machine_headers — never inherited (backlog df38075a, F5 of review
# a92c0b94). Unset, a caller that posted through alert_post before any
# alert handed curl whatever `@<file>` its environment carried.
ALERT_MT_HDR=""
ALERT_PEOPLE_MT_HDR=""

# alert_people_api — the people door: ALERT_API's host on the people
# port from sor-ports.env (read as data, never sourced). Empty when the
# table is missing or names no people port: no guessed port, no read.
alert_people_api() {
    local table port scheme hostport
    [ -r "$ALERT_LIB_DIR/sor-ports.env" ] || return 0
    table=$(grep -Ev '^[[:space:]]*(#|$)' "$ALERT_LIB_DIR/sor-ports.env" | tr -s '[:space:]' ' ')
    port=$(printf ' %s ' "$table" | sed -n 's/.* people=\([0-9][0-9]*\) .*/\1/p')
    [ -n "$port" ] || return 0
    scheme="${ALERT_API%%://*}"; hostport="${ALERT_API#*://}"; hostport="${hostport%%/*}"
    printf '%s://%s:%s' "$scheme" "${hostport%%:*}" "$port"
}

# alert_machine_headers — the estate machine token for this lib's two
# requests, PRESENTED AND NEVER REQUIRED (design 6805c764; backlog
# 2710c8fc). Both go to the system of record and nowhere else: the
# alert's POST to the jobs port (ALERT_MT_HDR) and the owner read on the
# people port of the same host (ALERT_PEOPLE_MT_HDR). Each header is
# made for its own URL, and the reader stamps only a host on the
# estate's list, so an ALERT_API pointed anywhere else gets none.
#
# `alert` and `alert_replay` call this FIRST, in the caller's own shell:
# the owner read runs two `$(…)` deep, and the reader refuses a first
# call inside a subshell. The watchdog sets its `trap … EXIT` before its
# first alert, which is the order the reader's chained cleanup needs.
# Sourced inside `( … )` (the converge's tenant_check_alert), the call
# is in a subshell: it stamps when the converge opened the header
# directory first, and otherwise the reader says so and the alert goes
# out unstamped.
#
# It cannot stop an alert. No lib, no slot, a slot the reader refuses, a
# host off the list, a header file that cannot be made: both variables
# are empty and the alert is raised, filed or kept exactly as before —
# the alert that matters most is "the system of record is dark".
alert_machine_headers() {
    ALERT_MT_HDR="" ALERT_PEOPLE_MT_HDR=""
    declare -F machine_token_header >/dev/null || return 0
    machine_token_header ALERT_MT_HDR "$ALERT_API" || ALERT_MT_HDR=""
    if [ -z "${BOSS_PLATFORM_OWNER:-}" ]; then
        # alert_people_api always returns 0: no table or no port is an
        # empty answer, and an empty answer makes no header.
        local api; api=$(alert_people_api)
        if [ -n "$api" ]; then
            machine_token_header ALERT_PEOPLE_MT_HDR "$api" || ALERT_PEOPLE_MT_HDR=""
        fi
    fi
    return 0
}

# alert_owner — the owner_id an alert is filed with. Pure shell + awk
# (the forge units must not need jq or python): the roster is split at
# object boundaries, each object's id and hire_date are lifted off its
# quoted fields (a null hire_date sorts last, as `~`), and the earliest
# hire wins with the id as the tie-break — the same rule as
# boss_core::platform_owner::first_hire.
alert_owner() {
    if [ -n "${BOSS_PLATFORM_OWNER:-}" ]; then printf '%s' "$BOSS_PLATFORM_OWNER"; return; fi
    local api; api=$(alert_people_api)
    [ -n "$api" ] || return 0
    curl -s --max-time 5 ${ALERT_PEOPLE_MT_HDR:+-H "$ALERT_PEOPLE_MT_HDR"} -H "x-boss-user: $ALERT_USER" \
        "$api/api/people?role=platform-admin&status=active" 2>/dev/null \
        | tr '}' '\n' \
        | awk -F'"' '{ id=""; hd="~"
            for (i = 1; i < NF; i++) {
                if ($i == "id" && $(i+1) == ":") id = $(i+2)
                if ($i == "hire_date" && $(i+1) == ":") hd = $(i+2)
            }
            if (id != "") print hd "\t" id }' \
        | sort | awk -F'\t' 'NR == 1 { printf "%s", $2 }'
    # Best-effort by design: a dark registry is empty output (nobody),
    # never a failed alert — under a caller's `set -e` the alert must
    # still be raised and kept.
    return 0
}

# alert_body TITLE DETAIL — the packet JSON, raised_at now, owned by
# whoever alert_owner names (nobody when it names no one).
alert_body() {
    local title="$1" detail="$2" at owner
    at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    owner=$(alert_owner)
    python_free_json "$title" "$detail" "$at" "$owner"
}
# JSON without jq or python (the forge units must not need either):
# escape backslashes, quotes and newlines by hand.
_esc() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr '\n' ' '; }
python_free_json() {
    local title detail at owner
    title=$(_esc "$1"); detail=$(_esc "$2"); at="$3"; owner=$(_esc "${4:-}")
    printf '{"kind":"backlog-item","status":"open","owner_id":"%s","priority":"urgent","tags":["alert","cluster"],"subject":{"subject_kind":"custom","id":"boss-cluster"},"title":"%s","metadata":{"area":"alert","filed_by":"%s","raised_at":"%s","detail":"%s"}}' "$owner" "$title" "$ALERT_ACTOR" "$at" "$detail"
}

# alert_post JSON [ANSWER_FILE] — POST one alert packet. Returns 0 on
# 201. ALERT_POST_CODE is left holding the HTTP code the API gave (000
# or empty when it gave none), and the answer's body goes to ANSWER_FILE
# when one is named, so a replay can tell a refusal from silence and
# keep what the refusal said.
#
# ANY NON-ZERO CURL EXIT IS NO ANSWER (review 4d18be3a, finding 3), as
# the dev memory watch already reads it. curl can exit non-zero AFTER
# the server answered — 23 when the answer could not be written (a full
# spool disk), 28 on a timeout after the headers — and the code it
# printed then is not one this lib can act on: a 201 read as a refusal
# was re-posted (a duplicate urgent packet per back-off) and then moved
# aside although it had been filed. As 000 the replay stops and counts
# nothing; the worst case is one duplicate on the next tick.
ALERT_POST_CODE=""
alert_post() {
    local code
    ALERT_POST_CODE=""
    code=$(printf '%s' "$1" | curl -s -o "${2:-/dev/null}" -w '%{http_code}' --max-time 15 \
        -X POST -H 'content-type: application/json' ${ALERT_MT_HDR:+-H "$ALERT_MT_HDR"} -H "x-boss-user: $ALERT_USER" \
        --data-binary @- "$ALERT_API/api/jobs") || { ALERT_POST_CODE=000; return 1; }
    ALERT_POST_CODE="$code"
    [ "$code" = "201" ]
}

# alert_answer_judges_the_body CODE — 0 when CODE is the jobs API's
# judgement of THIS body, the only answer a replay may count against it;
# 1 when the answer says nothing about the body, so the next one would
# get the same and the replay should stop:
#   000, empty   no answer (dark, or curl failed — see alert_post);
#   5xx          the API is failing, not judging;
#   401, 403     the CALLER is refused (review 4d18be3a, finding 1): the
#                machine gate's answers to a missing, stale or revoked
#                token, and the create door's policy refusal, which is
#                asked of who signs and the kind it opens
#                (open_authority::may_open in boss-jobs http/jobs.rs) —
#                every alert this lib files is the same signer and the
#                same kind, so no body can pass where another did not;
#   407, 408     a proxy or the connection, not the API;
#   429          too many requests, whoever sent them.
# What remains (400, 409, 422 — what the create door answers for a body
# it reads and refuses, an owner it cannot resolve among them) is the
# body's own, and backs that body off.
alert_answer_judges_the_body() {
    case "${1:-000}" in
        000 | 5[0-9][0-9] | 401 | 403 | 407 | 408 | 429) return 1 ;;
    esac
    return 0
}

# alert_count — alerts waiting to be filed.
alert_count() { [ -d "$ALERT_SPOOL" ] || { echo 0; return; }; ls -1 "$ALERT_SPOOL" 2>/dev/null | grep -c '\.json$'; }

# alert_refused_count — alerts the jobs API refused ALERT_REFUSALS_MAX
# times, moved aside under $ALERT_SPOOL/refused/ (see alert_replay).
alert_refused_count() { [ -d "$ALERT_SPOOL/refused" ] || { echo 0; return; }; ls -1 "$ALERT_SPOOL/refused" 2>/dev/null | grep -c '\.json$'; }

# alert_spool_trusted — 0 when ALERT_SPOOL is a spool only this account
# could have written; otherwise 1, with a refusal naming the path.
#
# A REPLAY PRESENTS THE CALLER'S CREDENTIAL WITH EVERY BODY IT READS
# (backlog 46c18d11, from re-review 8c585a24 of car 1d368d6c). The
# watchdog runs as root and alert_replay POSTs each *.json with the
# estate machine token, while ALERT_SPOOL's default is a name under
# /var/tmp: a run started without its unit environment replayed
# whatever another local account had created there, as root. So the
# replay first refuses a spool that is a symbolic link, is not a
# directory, belongs to another uid, is writable by group or other, or
# sits in a parent another account can change (writable by group or
# other without the sticky bit, or owned by neither this uid nor root:
# such a parent lets its owner swap the spool between this check and
# the read). Nothing is read, filed, moved or removed on a refusal — a
# planted body and a real kept alert look alike from here, and either
# may be the one copy of what it says. The units keep theirs under a
# 0700 StateDirectory, which passes; /var/tmp is sticky, so a spool
# this account made there passes too, and one another account made
# first does not.
alert_spool_trusted() {
    local spool="$ALERT_SPOOL" parent euid st own mode why=""
    euid=$(id -u)
    parent=$(dirname -- "$spool")
    if [ -L "$spool" ]; then
        why="it is a symbolic link"
    elif [ ! -d "$spool" ]; then
        why="it is not a directory"
    elif ! st=$(stat -c '%u %a' -- "$spool" 2> /dev/null); then
        why="its owner and mode cannot be read"
    else
        own=${st%% *}; mode=$((8#${st#* }))
        if [ "$own" != "$euid" ]; then
            why="it belongs to uid $own, not uid $euid"
        elif [ $((mode & 8#022)) -ne 0 ]; then
            why="it is writable by group or other (mode $(printf '%o' "$mode"))"
        elif ! st=$(stat -L -c '%u %a' -- "$parent" 2> /dev/null); then
            why="its parent $parent cannot be read"
        else
            own=${st%% *}; mode=$((8#${st#* }))
            if [ "$own" != "$euid" ] && [ "$own" != 0 ]; then
                why="its parent $parent belongs to uid $own"
            elif [ $((mode & 8#022)) -ne 0 ] && [ $((mode & 8#1000)) -eq 0 ]; then
                why="its parent $parent is writable by group or other without the sticky bit (mode $(printf '%o' "$mode"))"
            fi
        fi
    fi
    [ -z "$why" ] && return 0
    echo "alert: REFUSED to replay $spool — $why. A replay presents this account's credential with every body there, so the spool must be a real directory owned by uid $euid that no other account can write or swap. Each unit sets ALERT_SPOOL under its own 0700 StateDirectory; a run without that environment falls back to /var/tmp/boss-alert-spool. Nothing was read, filed, moved or removed." >&2
    return 1
}

# alert TITLE DETAIL — raise an alert: journal it, file it, or keep it.
alert() {
    alert_machine_headers
    local body; body=$(alert_body "$1" "$2")
    echo "ALERT: $1 — $2" >&2
    if alert_post "$body"; then
        echo "alert: filed as a packet for the platform owner" >&2
    else
        # 0700 whatever the umask: a spool this lib makes must pass
        # alert_spool_trusted, or the alert it keeps is never filed.
        mkdir -p -m 0700 "$ALERT_SPOOL"
        # Unique per alert even within one second and one process
        # (two alerts in a burst must not overwrite each other).
        printf '%s' "$body" > "$ALERT_SPOOL/$(date -u +%Y-%m-%dT%H:%M:%SZ)-$$-$(( ALERT_SEQ=${ALERT_SEQ:-0}+1 ))-$RANDOM.json"
        echo "alert: the jobs API did not take it — kept ($(alert_count) waiting); filed on the next run that reaches the API" >&2
    fi
}

# alert_replay — file every kept alert, oldest first. Returns 0 when
# nothing is left waiting.
#
# A REFUSED BODY DOES NOT HOLD THE SPOOL (backlog 9ecd12ea, residue of
# car 7849553a's review, finding 3). Until 2026-10-10 the replay stopped
# at the first alert that was not filed, whatever the answer, so a body
# the jobs API judged bad (a 4xx — an owner it cannot resolve, a field
# it refuses) was retried every five minutes for ever, and every alert
# raised after it — "cluster DARK: hands needed" among them — waited
# behind it, kept and never filed. The dev memory watch already answered
# this for its own spool (dev-memory-watch.sh file_kept_alerts); this is
# the same rule for the forge's:
#   201              filed; the file goes.
#   no answer, 5xx,  the API is dark or failing, or refuses the CALLER,
#   401/403/407/     not judging the body (alert_answer_judges_the_body):
#   408/429          stop, nothing else will pass either, and nothing is
#                    counted against the alert.
#   any other code   REFUSED. The body backs off (2, 4, 8, 16 minutes,
#                    in <file>.tries) and the ones behind it are still
#                    tried; at the ALERT_REFUSALS_MAX-th refusal it is
#                    moved aside to $ALERT_SPOOL/refused/ with the answer
#                    it got beside it as <file>.why, and counted by
#                    alert_refused_count for the caller's record. It is
#                    never deleted here: it may be the one copy of what
#                    the alert said, and the journal has the line.
ALERT_REFUSALS_MAX="${ALERT_REFUSALS_MAX:-5}"
alert_replay() {
    local n f tries k next now
    n=$(alert_count); [ "$n" -gt 0 ] || return 0
    alert_spool_trusted || return 1
    alert_machine_headers
    echo "alert: filing $n kept alert(s), oldest first" >&2
    now=$(date -u +%s)
    for f in $(ls -1 "$ALERT_SPOOL" | grep '\.json$' | sort); do
        tries="$ALERT_SPOOL/$f.tries"; k=0; next=0
        if [ -r "$tries" ]; then read -r k next < "$tries" || true; fi
        case "${k:-empty}" in empty | *[!0-9]*) k=0 ;; esac
        case "${next:-empty}" in empty | *[!0-9]*) next=0 ;; esac
        [ "$now" -ge "$next" ] || continue
        if alert_post "$(cat "$ALERT_SPOOL/$f")" "$ALERT_SPOOL/.answer"; then
            rm -f "$ALERT_SPOOL/$f" "$tries"
            continue
        fi
        if ! alert_answer_judges_the_body "$ALERT_POST_CODE"; then
            echo "alert: replay stopped at $f (answer ${ALERT_POST_CODE:-none}, which judges the API or the caller, not this alert) — $(alert_count) still waiting, nothing counted against them" >&2
            rm -f "$ALERT_SPOOL/.answer"
            return 1
        fi
        k=$((k + 1))
        if [ "$k" -ge "$ALERT_REFUSALS_MAX" ]; then
            mkdir -p "$ALERT_SPOOL/refused"
            { echo "refused $k times, last answer $ALERT_POST_CODE:"; head -c 2000 "$ALERT_SPOOL/.answer" 2>/dev/null; } > "$ALERT_SPOOL/refused/$f.why"
            mv "$ALERT_SPOOL/$f" "$ALERT_SPOOL/refused/" && rm -f "$tries"
            echo "alert REFUSED $k times (last answer $ALERT_POST_CODE): $f moved aside to $ALERT_SPOOL/refused, so the alerts behind it pass — $(alert_refused_count) refused there, each with its answer as .why" >&2
        else
            printf '%s %s\n' "$k" "$((now + 60 * (1 << k)))" > "$tries"
            echo "alert refused (answer $ALERT_POST_CODE, refusal $k of $ALERT_REFUSALS_MAX): $f waits $((1 << k)) min; the ones behind it are still tried" >&2
        fi
    done
    rm -f "$ALERT_SPOOL/.answer"
    n=$(alert_count)
    if [ "$n" -gt 0 ]; then echo "alert: $n kept alert(s) still waiting on a refusal's back-off" >&2; return 1; fi
    echo "alert: all kept alerts filed" >&2
}
