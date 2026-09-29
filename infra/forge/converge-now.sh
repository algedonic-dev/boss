#!/usr/bin/env bash
# converge-now — the `converge` ops verb: start ONE cluster converge
# now, and leave the runner a note saying which packet asked.
#
# Until 2026-09-08 the verb was the bare `systemctl start --no-block
# cluster-deploy-runner.service`. --no-block is right (the ops-runner
# polls every minute and must not sit on a build), but it returns 0 the
# instant systemd ACCEPTS the job — so the ops-request closed `answered`
# even when the unit then died, as it did twice on 2026-09-07 22:01
# (backlog d66f92b2). The packet said answered; the converge had not
# happened; the conductor's 30-minute alarm was the only signal.
#
# So the verb now writes the requesting packet's id (OPS_REQUEST_ID,
# which ops-runner.sh puts in the verb's environment) to an inbox in
# the checkout, `.git/boss-converge-requests`, and the runner takes that
# inbox when it starts and PATCHes each request with how the run ended
# (`converged: <sha>`, `converge_held: <reason>`, or `converge_failed:
# <stage> (exit N)`) — see cluster-deploy-lib.sh answer_converge_requests.
# A request that started a run which then failed reads as failed on the
# packet that asked, with the reason, not as a clean `answered`.
#
# Runs as root under the ops-runner (no HOME); the inbox is in the
# checkout owner's .git, world-readable, so the runner (the owner) can
# read and remove it. A request that arrives while a run is already
# active is taken by the NEXT run, which then answers it honestly
# (usually `converged: <sha>` — main unchanged, the cluster is on it).
set -euo pipefail

REPO="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
INBOX="$REPO/.git/boss-converge-requests"

if [ -n "${OPS_REQUEST_ID:-}" ]; then
    case "$OPS_REQUEST_ID" in
        *[!0-9a-fA-F-]* | "")
            echo "converge-now: OPS_REQUEST_ID '$OPS_REQUEST_ID' is not a job id — starting the converge without a note" >&2 ;;
        *)
            if ( umask 022; printf '%s\n' "$OPS_REQUEST_ID" >> "$INBOX" ) 2>/dev/null; then
                chmod 0644 "$INBOX" 2>/dev/null || true
                echo "converge-now: noted request ${OPS_REQUEST_ID:0:8} in $INBOX — the runner answers it when the run ends"
            else
                echo "converge-now: could not write $INBOX — the run's outcome will reach only its maintenance packet" >&2
            fi ;;
    esac
else
    echo "converge-now: no OPS_REQUEST_ID in the environment — starting the converge without a note"
fi

# THE READ-BACK (backlog 1058e686, car D). `start --no-block` returning
# 0 says systemd ACCEPTED a job, not that a run began — the only proof
# this verb had until 2026-09-29. systemd names every run of a unit
# with a fresh InvocationID, so the unit itself answers "did a new run
# begin": read InvocationID/ActiveState/Result before the start and
# after it. The effect is a NEW invocation, activating or active — or
# one that has already finished, said with its Result, since a converge
# with nothing to do can end before the first read. The SAME invocation
# is a run already in progress that merged the start: the note above
# waits for the next run, so that is a not-yet (75), not an effect. A
# read that cannot answer is CANNOT ANSWER, exit 1 — after the start,
# which runs regardless: this verb only accelerates the timer, and a
# missing proof must not become a missing converge.
UNIT=cluster-deploy-runner.service
SYSTEMCTL="${BOSS_SYSTEMCTL:-systemctl}"
READBACK_SECONDS="${BOSS_CONVERGE_READBACK_SECONDS:-20}"
case "$READBACK_SECONDS" in ''|*[!0-9]*) READBACK_SECONDS=20 ;; esac

# read_unit — sets INV, STATE, RESULT from `systemctl show`, or returns 1
# with the error in READ_ERR.
read_unit() {
    local out line
    INV=""; STATE=""; RESULT=""
    if ! out=$("$SYSTEMCTL" show -p InvocationID -p ActiveState -p Result "$UNIT" 2>&1); then
        READ_ERR="$out"
        return 1
    fi
    while IFS= read -r line; do
        case "$line" in
            InvocationID=*) INV="${line#InvocationID=}" ;;
            ActiveState=*) STATE="${line#ActiveState=}" ;;
            Result=*) RESULT="${line#Result=}" ;;
        esac
    done <<< "$out"
    if [ -z "$STATE" ]; then
        READ_ERR="systemctl show answered no ActiveState: $out"
        return 1
    fi
}

BEFORE_OK=1
if read_unit; then
    BEFORE_INV="$INV"
    echo "converge-now: before the start, $UNIT is ${STATE} (invocation ${BEFORE_INV:-none})"
else
    BEFORE_OK=0
    BEFORE_ERR="$READ_ERR"
fi

"$SYSTEMCTL" start --no-block "$UNIT"
echo "converge-now: $UNIT start accepted (--no-block) — reading the new invocation back"

if [ "$BEFORE_OK" = 0 ]; then
    echo "converge-now: CANNOT ANSWER — the start was accepted, but $UNIT could not be read BEFORE it, so no new invocation can be told from the old: $BEFORE_ERR" >&2
    exit 1
fi

# The bound is WALL-CLOCK, off bash's $SECONDS, never a poll count: a
# `systemctl show` slowed by PID 1 or dbus under a build's load makes
# twenty polls outlast the verb's declared timeout, and the runner's kill
# (124) would then stand in for this script's own verdict (review of car
# D, run 7fe34bd7).
started=$SECONDS
waited=0
while :; do
    if ! read_unit; then
        echo "converge-now: CANNOT ANSWER — the start was accepted, but $UNIT could not be read back: $READ_ERR" >&2
        exit 1
    fi
    waited=$((SECONDS - started))
    # A new run caught mid-exit (deactivating) has no final Result yet:
    # read again rather than judge it half-way.
    if [ -n "$INV" ] && [ "$INV" != "$BEFORE_INV" ] && [ "$STATE" != deactivating ]; then
        break
    fi
    if [ "$waited" -ge "$READBACK_SECONDS" ] && [ -n "$INV" ] && [ "$INV" != "$BEFORE_INV" ]; then
        echo "converge-now: CANNOT ANSWER — $UNIT began invocation $INV, but it was still deactivating ${waited}s after the start, with no final Result" >&2
        exit 1
    fi
    if [ "$waited" -ge "$READBACK_SECONDS" ]; then
        case "$STATE" in
            activating|active|deactivating)
                echo "converge-now: not yet: $UNIT is still invocation ${BEFORE_INV:-none} (${STATE}) ${waited}s after the start — a converge already running took this start into itself, so no new run began; this request is noted, and the next run answers it" ;;
            *)
                echo "converge-now: not yet: $UNIT is still invocation ${BEFORE_INV:-none} (${STATE}) ${waited}s after the start — no new run began; the start job is queued or was dropped, this request is noted, and the next run answers it" ;;
        esac
        exit 75
    fi
    sleep 1
done

case "$STATE" in
    activating|active)
        # The unit is spelled out, not $UNIT: this line is the verb's
        # declared `effect`, and the pin reads its literal text here.
        echo "converge-now: started cluster-deploy-runner.service — read back: invocation $INV ($STATE), was ${BEFORE_INV:-none}; its verdict lands on the maintenance-cluster-converge packet and on this request"
        ;;
    inactive)
        if [ "$RESULT" = success ]; then
            echo "converge-now: started cluster-deploy-runner.service — read back: invocation $INV (inactive, already finished: Result=success), was ${BEFORE_INV:-none}; its verdict lands on the maintenance-cluster-converge packet and on this request"
        else
            echo "converge-now: FAILED — the run this start began (invocation $INV) has already ended: ActiveState=inactive, Result=${RESULT:-unknown}" >&2
            exit 1
        fi
        ;;
    failed)
        echo "converge-now: FAILED — the run this start began (invocation $INV) has already failed: Result=${RESULT:-unknown}; its stage and exit land on the maintenance-cluster-converge packet" >&2
        exit 1
        ;;
    *)
        echo "converge-now: CANNOT ANSWER — $UNIT began invocation $INV, but reads ActiveState=${STATE}, which is neither running nor finished" >&2
        exit 1
        ;;
esac
