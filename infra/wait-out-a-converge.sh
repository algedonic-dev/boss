#!/usr/bin/env bash
#
# wait-out-a-converge.sh — a check that reads a live instance does not
# start while the cluster converge is rolling it (backlog c8c6b9a8).
#
# MEASURED 2026-09-28. The nightly playground crawl (55bcf7fb) ran
# 04:45:18-04:45:59Z. Converge a10cbaa4 opened 04:41:01, recorded
# `build_head: 4f9a588` and `roll_boss-playground: 4f9a588` on its run
# step, and closed 04:47:18 — the crawl sat inside the roll, all 43
# routes read `no-shell`, and the judge filed 43 backlog-items that
# were closed stale by hand. The roll window was on the record the whole
# time; nothing read it before the browser started.
#
# THE RECORD IT READS, not a clock guess. cluster-deploy-runner.service
# opens a `maintenance-cluster-converge` packet in ExecStartPre (the
# wrap) and closes it in ExecStopPost (boss-step.sh) with what the run
# did on its `run` step — `build_head` when it built and rolled a head,
# `observed_on: unchanged` on a no-op tick. So, off the newest packets:
#
#   * a converge packet still OPEN is a run in progress — unless it was
#     opened longer ago than the unit's TimeoutStartSec (90 min), past
#     which systemd has killed the run and a packet still open is one
#     whose close never landed, not a roll;
#   * a closed one whose run BUILT (`build_head`) and completed less
#     than CONVERGE_SETTLE_MIN ago (10) is a head still settling —
#     roll_deployment proves Ready before the run closes, and the margin
#     is for what Ready does not cover (a cold cache, the tunnel);
#   * anything else is clear: an `unchanged` tick rolls nothing.
#
# DECLINE AND RESCHEDULE. Not clear, it says why once, re-reads every
# CONVERGE_POLL_S (60) and returns the moment the record is clear. The
# crawl fires once a day, so rescheduling to "tomorrow" would lose the
# night; the wait is bounded by CONVERGE_WAIT_MAX_MIN (45 — inside the
# CronJob's one-hour activeDeadlineSeconds with the crawl's few minutes
# to spare). Past the bound it says NOT CLEAR and returns anyway: a
# crawl that never runs is silence, and the judge
# (maintenance.chore.file_reds) files a roll-shaped red — every route
# no-shell — as ONE shared-cause item, not one per route.
#
# IT NEVER FAILS THE CHECK. Exit 0 on every path: a dark system of
# record is a decline that is re-read (a roll takes the jobs API away
# too), an unnamed one is said and not waited on. Every line goes to
# stdout, which boss-chore.sh carries onto the crawl's run step, so the
# wait — and what it waited for — is on the packet.
#
# Env: BOSS_JOBS_URL (the system of record), BOSS_API_CURL (the door;
# boss-api-curl.sh, the roll-posture helper every chore shares),
# CONVERGE_SETTLE_MIN, CONVERGE_WAIT_MAX_MIN, CONVERGE_POLL_S.
# Pinned by crates/core/boss-testing/tests/wait_out_a_converge_sh.rs.
set -uo pipefail

ME="wait-out-a-converge"
KIND="maintenance-cluster-converge"
say() { echo "$ME: $*"; }

# A knob that is not a positive integer is the default, said once.
num() { # NAME DEFAULT
    local v="${!1:-}"
    case "$v" in
        '') echo "$2" ;;
        *[!0-9]* | 0) say "$1=$v is not a positive whole number; using $2" >&2; echo "$2" ;;
        *) echo "$v" ;;
    esac
}
SETTLE_S=$(( $(num CONVERGE_SETTLE_MIN 10) * 60 ))
WAIT_MAX_S=$(( $(num CONVERGE_WAIT_MAX_MIN 45) * 60 ))
POLL_S=$(num CONVERGE_POLL_S 60)
# cluster-deploy-runner.service: TimeoutStartSec=90min.
OPEN_MAX_S=5400
API_CURL="${BOSS_API_CURL:-boss-api-curl.sh}"
# Signed as the chore wrapper signs (boss-maintenance-wrap.sh): an
# unidentified read answers `total: 0`, a denied scope that would read
# as "no converge" — clear, on the one night it matters.
BOSS_USER='{"id":"automation:maintenance-timer","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

if [ -z "${BOSS_JOBS_URL:-}" ]; then
    say "BOSS_JOBS_URL is not set — no converge record to read, so not waiting; the check runs now"
    exit 0
fi

# The machine token, PRESENTED and never required (design 6805c764;
# backlog 44b2087e): the converge read went out with x-boss-user alone.
# Made here, in the script's own shell, because judge runs inside $(…).
# A host or pod with no token mounted — the playground crawl's, today —
# gets an empty MT_HDR and sends exactly the read it sent before; so
# does a copy of this script with no lib/ beside it, and a reader that
# could not write its file. Nothing here can stop the wait: this script
# exits 0 on every path.
MT_HDR=""
SECRET_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/secret-header.sh"
if [ -r "$SECRET_LIB" ]; then
    # shellcheck source=infra/lib/secret-header.sh
    . "$SECRET_LIB"
    machine_token_header MT_HDR "$BOSS_JOBS_URL" || MT_HDR=""
fi

# judge — one line: `clear`, or `wait <why>`.
#
# The newest 20 packets: the list is newest first (opened_on DESC,
# created_at DESC), the converge ticks every ten minutes, so 20 rows
# reach back ~3 hours — past both the 90-minute open bound and the
# settle window. `full=true` carries the run step's metadata.
judge() {
    local body rc=0 verdict
    body=$("$API_CURL" -fsS --max-time 20 -H "x-boss-user: $BOSS_USER" \
        ${MT_HDR:+-H "$MT_HDR"} \
        "$BOSS_JOBS_URL/api/jobs?kind=$KIND&limit=20&full=true" 2>/dev/null) || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "wait the system of record did not answer the converge read (exit $rc)"
        return
    fi
    verdict=$(printf '%s' "$body" | jq -r \
        --argjson now "$(date -u +%s)" --argjson settle "$SETTLE_S" --argjson open_max "$OPEN_MAX_S" '
        def epoch: sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z") | fromdateiso8601;
        (if type == "object" and has("data") then .data else . end) as $rows
        | if ($rows | type) != "array" then "wait the converge read answered no rows"
          else
            ([$rows[] | select(.status == "open" and (.opened_at // "") != "")
                      | select(($now - (.opened_at | epoch)) < $open_max)] | first) as $open
            | ([$rows[] | . as $j
                | ((.steps // []) | map(select(.spec_slug == "run")) | .[0]) as $run
                | select($run != null and ($run.metadata.build_head // "") != ""
                         and ($run.completed_at // "") != "")
                | ($run.completed_at | epoch) as $done
                | select(($now - $done) < $settle)
                | {id: $j.id, head: $run.metadata.build_head, at: $run.completed_at,
                   until: (($done + $settle) | todate)}]
               | first) as $roll
            | if $open != null then
                "wait converge \($open.id[0:8]) is in progress (opened \($open.opened_at))"
              elif $roll != null then
                "wait converge \($roll.id[0:8]) rolled \($roll.head) at \($roll.at) — settling until \($roll.until)"
              else "clear" end
          end' 2>/dev/null) || verdict="wait the converge read did not parse"
    echo "${verdict:-wait the converge read printed nothing}"
}

waited=0
said=""
while :; do
    verdict=$(judge)
    if [ "$verdict" = "clear" ]; then
        if [ "$waited" -gt 0 ]; then
            say "clear after ${waited}s — no converge open, none rolled in the last $((SETTLE_S / 60)) min; the check runs now"
        else
            say "clear — no converge open, none rolled in the last $((SETTLE_S / 60)) min"
        fi
        exit 0
    fi
    why="${verdict#wait }"
    if [ "$waited" -ge "$WAIT_MAX_S" ]; then
        say "NOT CLEAR after ${waited}s — $why — running anyway: a red that is every route no-shell is judged as ONE shared-cause item"
        exit 0
    fi
    if [ "$why" != "$said" ]; then
        say "declining for now: $why — re-reading every ${POLL_S}s, for up to $((WAIT_MAX_S / 60)) min"
        said="$why"
    fi
    sleep "$POLL_S"
    waited=$((waited + POLL_S))
done
