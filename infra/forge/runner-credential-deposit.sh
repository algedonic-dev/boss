#!/usr/bin/env bash
#
# runner-credential-deposit — the forge takes ITS ops runner credential
# out of the broker's Secret and into the root-only file its runner
# presents, proves it by effect through the jobs API's credential door,
# and records delivery so the broker may promote it and drop the old one.
#
#   runner-credential-deposit.sh --rule <broker rule .toml> --dest <file>
#
# WHY IT EXISTS (design f623e425 Q1, option A, David 2026-09-25; backlog
# 1e50e66b). infra/ops/ops-runner.sh presents /etc/boss/ops-runner.credential
# in x-boss-runner-credential on every request, and the jobs API resolves
# it to runner:ops for this host against the slots of Secret
# boss/ops-runner-credential. The broker (`credential.rotate.ops-runner`)
# mints a host's value into `<host>.next`, leaving `<host>.current` valid;
# this is the host's half of the rotation, the checkout token's deposit
# shape (credential-deposit.sh, design 1c90d183) with the jobs API's own
# door as the proof instead of git.
#
# ONE PASS:
#   1. Read the rule's declaration — secret_namespace, secret_name, host,
#      and the credential id off its `when` (the rule is the one
#      declaration; nothing here repeats it, CLAUDE.md §9a) — and refuse a
#      rule for another host than this one (BOSS_NODE_ID).
#   2. Read the host's `next`, the packet it was minted for
#      (`next.minted-for`) and `current` from the Secret through the
#      forge's admin kubeconfig. The value to hold is `next` when the
#      broker has staged one, else `current`.
#   3. A value that differs from the file's is PROVED first — presented to
#      GET /api/jobs/runner-credential, it must resolve to THIS host — and
#      only then written: umask 077 into a temp file beside the
#      destination, fsynced, root:root, 0600, renamed into place, the
#      directory fsynced. A value the door does not resolve changes
#      nothing. That is "not yet", green, while the door is only BEHIND —
#      the held value resolves AND the named install is younger than two
#      hours, or the first staged value is younger than kubelet's refresh
#      bound — and a red otherwise, because
#      then the mount is missing or broken (review 3930a3eb, N1). A jobs
#      API that answers 404 has no door at all, and says so, red (N5).
#   4. When the file holds the staged `next` and the door resolves it to
#      this host, the packet `next.minted-for` names — and only that one
#      (N3) — has its `delivered` step completed with the last eight: the
#      record the broker's promotion waits for, and believes only from
#      this script's actor. The Secret is read again first: a value that
#      moved under the pass is recorded by the next pass, which installs
#      it first.
#   5. The file is held at 0600 (and root:root when this runs as root) on
#      every pass, whatever changed it (review of 8e5de104, finding 5).
#
# EVERY END NAMES ITS CAUSE on the converge packet: a destination that
# cannot be read or written ends the pass through `finish`, which writes
# the run summary first — never through `set -e`, which would leave the
# packet red with no reason (N4).
#
# IT OWES NOTHING TO THE RUNNER, AND THE RUNNER OWES NOTHING TO IT. No
# file is the runner's state on every host until the first rotation, and
# the runner answers without a credential (DR rule 62dac114). A pass that
# cannot prove a value leaves the held file alone.
#
# NO VALUE EVER LEAVES A VARIABLE except into the file and a 0600 header
# file curl reads (infra/lib/secret-header.sh): never an argv, a message,
# a summary field or a request body — the last eight, the host and the
# slot only.
#
# Exit: 0 done (including "nothing to do", "not ready" and "not yet");
# 1 a step failed and says so on the converge packet; 78 a refusal of how
# it was invoked.
set -euo pipefail

ME=runner-credential-deposit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

# How long a staged value may go unseen by the door before, with no held
# value to tell a lagging mount from a broken one, its absence is a red:
# kubelet's Secret refresh (60-90 s) with room to spare.
STAGED_GRACE_S=300
# A working held credential is continuity, not evidence that an indefinitely
# lagging mount is healthy (review3121561f R3). The named install bounds it.
HELD_GRACE_S=7200

RULE="" DEST=""
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || refuse "$1 needs a value"
    case "$1" in
        --rule) RULE="$2" ;;
        --dest) DEST="$2" ;;
        *) refuse "unknown argument $1 (usage: $ME --rule FILE --dest FILE)" ;;
    esac
    shift 2
done
[ -n "$RULE" ] && [ -n "$DEST" ] || refuse "--rule and --dest are both required"

# --- 1. the declaration: the broker rule -------------------------------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
HANDLER_LINE="$(grep -m1 '^handler = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
[ "$HANDLER_LINE" = 'handler = "credential.rotate.ops-runner"' ] \
    || refuse "$RULE is not an ops runner credential rule ($HANDLER_LINE)"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
WHEN_LINE="$(grep -m1 '^when = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
HOST="$(rule_arg host)"
CRED=""
when_re='subject_id = "([^"]+)"'
if [[ $WHEN_LINE =~ $when_re ]]; then CRED="${BASH_REMATCH[1]}"; fi
[ -n "$NS" ] && [ -n "$NAME" ] && [ -n "$HOST" ] && [ -n "$CRED" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name / host, or no subject_id in its when — cannot know what to deposit"
[[ $HOST =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]] || refuse "$RULE names host '$HOST', which is not an estate host id"
NODE="${BOSS_NODE_ID:-}"
[ -n "$NODE" ] || refuse "BOSS_NODE_ID is unset: the deposit renders THIS host's credential, and the unit declares which host this is"
[ "$NODE" = "$HOST" ] \
    || refuse "$RULE declares host $HOST's credential, and this is $NODE — a host holds its own credential only"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"
# shellcheck source=infra/lib/jq.sh
. "$INFRA/lib/jq.sh"
# The remote receiver and local deposit share the exact owner/CAS contract.
. "$INFRA/lib/runner-delivery-ack.sh"

KERR="$(mktemp)"
BODY="$(mktemp)"
TMP=""
cleanup() {
    rm -f "$KERR" "$BODY"
    if [ -n "$TMP" ]; then rm -f "$TMP"; fi
}
# Set BEFORE the first secret_header call, which chains its own cleanup in
# front of this one (infra/lib/secret-header.sh, ORDER).
trap cleanup EXIT

last8() { if [ "${#1}" -le 8 ]; then printf '%s' "$1"; else printf '%s' "${1: -8}"; fi; }

rc=0
CUR="" WANT="" NEXT="" NEXT_FOR="" CURRENT=""
SECRET_STATE="" ACTION="unchanged" DELIVERY_STATE="nothing to record"

# finish — the one way out after the declaration: the run summary names
# what this pass found and did, then the exit status. Never skipped by an
# early failure (N4).
finish() {
    local held="none"
    if [ -n "$CUR" ]; then held="$(last8 "$CUR")"; fi
    CUR="" WANT="" NEXT="" CURRENT=""
    run_summary_field runner_credential_last_eight "$held"
    run_summary_field runner_credential_secret "$SECRET_STATE"
    run_summary_field runner_credential_action "$ACTION"
    run_summary_field runner_credential_delivery "$DELIVERY_STATE"
    say "file $DEST: $ACTION"
    say "delivery: $DELIVERY_STATE"
    say "the runner credential ends in $held"
    exit "$rc"
}
# fail_file WHY — the destination could not be read or written: red, named.
fail_file() {
    rc=1
    ACTION="$1"
    if [ -n "$TMP" ]; then rm -f "$TMP"; TMP=""; fi
    finish
}

# --- 2. the Secret -----------------------------------------------------
KC_STATE=""
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig through the kubectl container every
    # forge script uses (kubectl is not on the host). ABSENT is not ready
    # (root material David places, backlog 714bc71f), never a red.
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    KUBECTL=(docker run --rm --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc)
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c)"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable"
    fi
fi

# read_secret — the host's `next`, its packet and `current` into NEXT,
# NEXT_FOR and CURRENT (each empty when absent or blank), and
# SECRET_STATE naming what was read.
read_secret() {
    local read_out next_b64 for_b64 cur_b64 v slot
    NEXT="" NEXT_FOR="" CURRENT="" SECRET_STATE="$KC_STATE"
    [ -z "$SECRET_STATE" ] || return 0
    if ! read_out="$("${KUBECTL[@]}" -n "$NS" get secret "$NAME" \
        -o "jsonpath={.data.$HOST\\.next}|{.data.$HOST\\.next\\.minted-for}|{.data.$HOST\\.current}" 2>"$KERR")"; then
        if grep -q 'NotFound' "$KERR"; then
            SECRET_STATE="absent: Secret $NS/$NAME does not exist (the cluster converge creates it from the rule)"
        else
            SECRET_STATE="unreadable: $(head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300)"
        fi
        return 0
    fi
    IFS='|' read -r next_b64 for_b64 cur_b64 <<< "$read_out"
    for slot in next next.minted-for current; do
        case "$slot" in
            next) v="$next_b64" ;;
            next.minted-for) v="$for_b64" ;;
            *) v="$cur_b64" ;;
        esac
        [ -n "$v" ] || continue
        if ! v="$(printf '%s' "$v" | base64 -d 2>/dev/null)"; then
            SECRET_STATE="unreadable: key $HOST.$slot of $NS/$NAME is not base64"
            NEXT="" NEXT_FOR="" CURRENT=""
            return 0
        fi
        v="${v//[[:space:]]/}"
        [ -n "$v" ] || continue
        case "$slot" in
            next.minted-for)
                if [[ ! $v =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
                    SECRET_STATE="unreadable: key $HOST.next.minted-for of $NS/$NAME is not a packet id"
                    NEXT="" NEXT_FOR="" CURRENT=""
                    return 0
                fi
                NEXT_FOR="$v"
                ;;
            *)
                if [[ ! $v =~ ^[A-Za-z0-9_-]+$ ]]; then
                    SECRET_STATE="unreadable: key $HOST.$slot of $NS/$NAME is not one credential"
                    NEXT="" NEXT_FOR="" CURRENT=""
                    return 0
                fi
                if [ "$slot" = next ]; then NEXT="$v"; else CURRENT="$v"; fi
                ;;
        esac
    done
    if [ -n "$NEXT" ]; then
        SECRET_STATE="staged: $HOST.next …$(last8 "$NEXT") for ${NEXT_FOR:0:8}${CURRENT:+, $HOST.current …$(last8 "$CURRENT")}"
    elif [ -n "$CURRENT" ]; then
        SECRET_STATE="current: $HOST.current …$(last8 "$CURRENT")"
    else
        SECRET_STATE="empty: the broker has not minted $HOST's credential"
    fi
}
read_secret
case "$SECRET_STATE" in unreadable*) rc=1 ;; esac
say "secret $NS/$NAME: $SECRET_STATE"
WANT="${NEXT:-$CURRENT}"

# --- the jobs API ------------------------------------------------------
BOSS_USER="{\"id\":\"automation:runner-credential-deposit\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"
API_CURL="$INFRA/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
HDRS=(-H "x-boss-user: $BOSS_USER")
# The machine token: the mounted Secret's `current` slot, stamped only on
# an estate host (infra/lib/secret-header.sh machine_token_header; design
# 6805c764 car 4).
machine_token_header MT_HDR "${BOSS_JOBS_URL:-}" \
    || refuse "the machine token's header file could not be written"
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
NO_DOOR="the jobs API at ${BOSS_JOBS_URL:-(unset)} has no credential door (GET /api/jobs/runner-credential answered 404) — a build without it; no value can be proved until it is back"

# api_get PATH [CURL ARGS…] — GET into $BODY; CODE is the HTTP status, or
# empty when no answer came. Never -f: a 404 is an answer (N5).
CODE=""
api_get() {
    local path="$1"
    shift
    CODE=""
    if ! CODE="$("$API_CURL" -sS -o "$BODY" -w '%{http_code}' "${HDRS[@]}" "$@" \
        "$BOSS_JOBS_URL$path" 2>/dev/null)"; then
        CODE=""
    fi
}

# whoami VALUE — what the credential door makes of VALUE, into SEEN
# ("<host>/<slot>", or "unresolved"). 0 answered; 1 no answer (a
# transient); 3 no door; 4 an actual HTTP refusal, with CODE retained. The value
# rides in a 0600 header file, never curl's argv.
SEEN="" DELIVERY_CONTEXT=""
whoami() {
    SEEN="" DELIVERY_CONTEXT=""
    [ -n "${BOSS_JOBS_URL:-}" ] || return 1
    secret_header RC_HDR "x-boss-runner-credential: $1" || return 1
    api_get /api/jobs/runner-credential -H "$RC_HDR"
    case "$CODE" in
        200) ;;
        404) return 3 ;;
        '') return 1 ;;
        *) return 4 ;;
    esac
    if ! SEEN="$(jq -r 'if .resolved == true then "\(.host)/\(.slot)"
        elif .resolved == false then "unresolved" else error("no resolved") end' "$BODY" 2>/dev/null)"; then
        SEEN=""
        return 1
    fi
    if ! DELIVERY_CONTEXT="$(jq -c '.delivery // null' "$BODY" 2>/dev/null)"; then
        return 1
    fi
}

# packet ID — the rotation packet the staged value names, into PKT_STATE
# (`awaiting`, `recorded`, or why neither), PKT_STEP (its delivered
# step's id) and PKT_INSTALLED (its install's epoch, or empty).
PKT_STATE="" PKT_STEP="" PKT_INSTALLED="" PKT_INSTALL_STATE="" PKT_PREPARED=""
packet() {
    local at
    PKT_STATE="" PKT_STEP="" PKT_INSTALLED="" PKT_INSTALL_STATE="" PKT_PREPARED=""
    api_get "/api/jobs/$1"
    case "$CODE" in
        200) ;;
        404) PKT_STATE="unknown: the jobs API has no packet ${1:0:8}"; return 0 ;;
        '') PKT_STATE="unanswered"; return 0 ;;
        *) PKT_STATE="refused: rotation packet HTTP $CODE"; return 0 ;;
    esac
    if ! PKT_STATE="$(jq -r --arg c "$CRED" '
        ([.steps[]? | select(.spec_slug == "delivered")][0]) as $d
        | if (.subject.id // .subject_id) != $c then "a packet of another credential"
          elif $d == null then "no delivered step"
          elif $d.status == "completed" then "recorded"
          elif .status != "open" then "closed (\(.status))"
          elif $d.status == "ready" or $d.status == "active" then "awaiting"
          else "delivered step \($d.status)" end' "$BODY" 2>/dev/null)"; then
        PKT_STATE="unreadable: packet ${1:0:8} did not parse"
        return 0
    fi
    if ! PKT_STEP="$(jq -r '[.steps[]? | select(.spec_slug == "delivered")][0].id // empty' "$BODY")" \
        || ! at="$(jq -r '[.steps[]? | select(.spec_slug == "install")][0].completed_at // empty' "$BODY")" \
        || ! PKT_INSTALL_STATE="$(jq -r '[.steps[]? | select(.spec_slug == "install")][0].status // empty' "$BODY")" \
        || ! PKT_PREPARED="$(jq -r '([.steps[]? | select(.spec_slug == "issue" and .status == "completed")] + [.steps[]? | select(.spec_slug == "scope" and .status == "completed")])[0].completed_at // empty' "$BODY")"; then
        PKT_STATE="unreadable: packet ${1:0:8} did not parse"
        return 0
    fi
    # An install time that does not parse is no age at all: the staged
    # value then gets no grace, and its absence reads as a broken mount.
    if [ -n "$at" ] && ! PKT_INSTALLED="$(date -u -d "$at" +%s 2>/dev/null)"; then
        PKT_INSTALLED=""
    fi
}

# Unknown/future times establish no grace. A relative or empty date must not
# silently become today's midnight or a continuously renewed deadline.
within_bound() {
    local now
    case "$1" in ''|*[!0-9]*) return 1;; esac
    now="$(date -u +%s)" || return 1
    [ "$1" -le "$now" ] && [ $((now - $1)) -lt "$2" ]
}

pending_install() {
    local prepared_epoch
    case "$PKT_INSTALL_STATE" in active|ready|pending) ;;
        *) return 1;; esac
    # Admission can wait days for a human. The issue completion, or the
    # completed human scope before issue, bounds this preparation interval.
    [ -n "$PKT_PREPARED" ] || return 1
    prepared_epoch="$(date -u -d "$PKT_PREPARED" +%s 2>/dev/null)" || return 1
    within_bound "$prepared_epoch" "$HELD_GRACE_S"
}

# held_mode FILE — 0600, and root:root when this runs as root.
held_mode() {
    chmod 600 "$1" || return 1
    if [ "$(id -u)" -eq 0 ]; then chown 0:0 "$1" || return 1; fi
}

# --- 3. the file -------------------------------------------------------
if [ -L "$DEST" ]; then
    rc=1
    ACTION="refused: $DEST is a symlink, not the deposit's own file; it was not read or replaced"
    finish
fi
if [ -e "$DEST" ]; then
    if ! CUR="$(cat -- "$DEST" 2>"$KERR")"; then
        CUR=""
        fail_file "refused: $DEST cannot read ($(head -n 1 "$KERR" | cut -c1-200)); nothing was replaced"
    fi
fi
if [ -n "$NEXT" ] && [ -n "$NEXT_FOR" ]; then
    packet "$NEXT_FOR"
    if pending_install && [ "$PKT_STATE" = 'delivered step pending' ]; then
        ACTION="not yet: named packet install is not completed; inside the bounded preparation window; the held file is untouched"
        if [ -n "$CUR" ]; then held_mode "$DEST" || fail_file "held file mode could not be repaired"; fi
        finish
    fi
fi
if [ -n "$WANT" ] && [ "$WANT" != "$CUR" ]; then
    SLOT=next
    [ -n "$NEXT" ] || SLOT=current
    wrc=0
    whoami "$WANT" || wrc=$?
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        ACTION="not installed: BOSS_JOBS_URL is unset (/etc/boss/sor.env), so $HOST.$SLOT (…$(last8 "$WANT")) cannot be proved; the next pass retries"
    elif [ "$wrc" -eq 3 ]; then
        rc=1
        ACTION="not installed: $NO_DOOR"
    elif [ "$wrc" -eq 4 ]; then
        rc=1
        ACTION="not installed: GET /api/jobs/runner-credential refused: HTTP $CODE; the held file is untouched"
    elif [ "$wrc" -ne 0 ]; then
        ACTION="not installed: the jobs API did not answer GET /api/jobs/runner-credential; the next pass retries"
    elif [ "$SEEN" = unresolved ]; then
        # BEHIND OR BROKEN (N1). The held value resolving says the mount
        # is live and only behind; with nothing held, the staged value's
        # age says it.
        why="the jobs API does not resolve $HOST.$SLOT (…$(last8 "$WANT"))"
        behind=""
        if [ -n "$CUR" ]; then
            hrc=0
            whoami "$CUR" || hrc=$?
            if [ "$hrc" -eq 0 ] && [ "${SEEN%%/*}" = "$HOST" ]; then
                packet "$NEXT_FOR"
                if [ "$PKT_INSTALL_STATE" = completed ] && within_bound "$PKT_INSTALLED" "$HELD_GRACE_S"; then
                    behind="the held value still resolves; the named install is inside the $HELD_GRACE_S-second lag bound"
                else
                    why="$why; named install exceeds the lag bound or its past time is unknown ($PKT_STATE)"
                fi
            elif [ "$hrc" -eq 4 ]; then
                why="$why; held credential HTTP $CODE refusal"
            else
                why="$why, and the value the runner holds (…$(last8 "$CUR")) does not resolve to $HOST either — its mount of $NS/$NAME is missing or broken"
            fi
        elif [ "$SLOT" = next ] && [ -n "$NEXT_FOR" ]; then
            packet "$NEXT_FOR"
            if [ "$PKT_INSTALL_STATE" = completed ] && within_bound "$PKT_INSTALLED" "$STAGED_GRACE_S"; then
                behind="the named install is inside kubelet's $STAGED_GRACE_S-second refresh bound"
            else
                why="$why, named install is past kubelet's refresh or its time is unknown ($PKT_STATE) — its mount of $NS/$NAME is missing or broken"
            fi
        fi
        if [ -n "$behind" ]; then
            ACTION="not yet: $why; $behind; the held file is untouched"
        else
            rc=1
            ACTION="not installed: $why; the held file is untouched"
        fi
    elif [ "${SEEN%%/*}" != "$HOST" ]; then
        rc=1
        ACTION="not installed: the jobs API resolves $HOST.$SLOT (…$(last8 "$WANT")) to $SEEN, not $HOST"
    else
        DIR="$(dirname "$DEST")"
        cant="could not be written — nothing was replaced"
        mkdir -p -m 755 "$DIR" 2>"$KERR" \
            || fail_file "not installed: $DIR $cant ($(head -n 1 "$KERR" | cut -c1-200))"
        TMP="$(mktemp "$DIR/.$(basename "$DEST").XXXXXX" 2>"$KERR")" \
            || fail_file "not installed: a temp file in $DIR $cant ($(head -n 1 "$KERR" | cut -c1-200))"
        (umask 077 && printf '%s' "$WANT" > "$TMP") 2>"$KERR" \
            || fail_file "not installed: $TMP $cant ($(head -n 1 "$KERR" | cut -c1-200))"
        # Durable before it is visible: a power loss after the rename must
        # not leave an empty file where the runner reads its credential.
        sync -- "$TMP" 2>"$KERR" \
            || fail_file "not installed: $TMP $cant — fsync failed ($(head -n 1 "$KERR" | cut -c1-200))"
        held_mode "$TMP" 2>"$KERR" \
            || fail_file "not installed: $TMP's owner and mode $cant ($(head -n 1 "$KERR" | cut -c1-200))"
        # -T: a link planted at the path is replaced, never written through.
        mv -fT "$TMP" "$DEST" 2>"$KERR" \
            || fail_file "not installed: $DEST $cant ($(head -n 1 "$KERR" | cut -c1-200))"
        TMP=""
        sync -- "$DIR" 2>"$KERR" \
            || fail_file "installed, but $DIR could not be fsynced ($(head -n 1 "$KERR" | cut -c1-200))"
        CUR="$WANT"
        ACTION="installed $HOST.$SLOT (…$(last8 "$WANT")), which the jobs API resolves to $SEEN"
    fi
fi
if [ -n "$CUR" ]; then
    held_mode "$DEST" 2>"$KERR" \
        || fail_file "refused: $DEST's owner and mode could not be held at root:root 0600 ($(head -n 1 "$KERR" | cut -c1-200))"
fi

# --- 4. the delivery record --------------------------------------------
record_delivery() {
    local body l8 moved moved_for wrc identity expected receipt attempt
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        DELIVERY_STATE="not recorded: BOSS_JOBS_URL is unset (/etc/boss/sor.env)"
        return 0
    fi
    if [ -z "$NEXT_FOR" ]; then
        DELIVERY_STATE="not recorded: $HOST.next names no packet ($HOST.next.minted-for is empty), so there is no rotation to record it on"
        rc=1
        return 0
    fi
    # THE PACKET THE VALUE NAMES, and only it (N3).
    packet "$NEXT_FOR"
    case "$PKT_STATE" in
        awaiting) ;;
        recorded)
            DELIVERY_STATE="already recorded on ${NEXT_FOR:0:8}: promotion not observed; the staged value remains"
            return 0
            ;;
        unanswered)
            DELIVERY_STATE="not recorded: the jobs API did not answer for ${NEXT_FOR:0:8}; the next pass retries"
            return 0
            ;;
        *)
            DELIVERY_STATE="not recorded: packet ${NEXT_FOR:0:8}, which $HOST.next.minted-for names, does not await delivery ($PKT_STATE)"
            rc=1
            return 0
            ;;
    esac
    # Present the installed local value after installation, including the
    # next-slot bootstrap. The earlier pre-install whoami is not this act.
    if [ -n "$CUR" ]; then
        wrc=0
        whoami "$CUR" || wrc=$?
        if [ "$wrc" -eq 3 ]; then
            DELIVERY_STATE="not recorded: $NO_DOOR"
            rc=1
            return 0
        elif [ "$wrc" -ne 0 ]; then
            if [ "$wrc" -eq 4 ]; then
                DELIVERY_STATE="not recorded: GET /api/jobs/runner-credential refused: HTTP $CODE"
                rc=1
                return 0
            fi
            DELIVERY_STATE="not recorded: the jobs API did not answer GET /api/jobs/runner-credential; the next pass retries"
            return 0
        fi
        if [ "${SEEN%%/*}" != "$HOST" ]; then
            DELIVERY_STATE="not recorded: the held value (…$(last8 "$CUR")) answers $SEEN at the jobs API, not $HOST"
            rc=1
            return 0
        fi
    fi
    # THE SECRET AGAIN, AFTER THE PACKET WAS READ (the checkout deposit's
    # review F3): the broker may have staged a newer value since this pass
    # read it, and recording the older one's last eight would be refused,
    # and a completed step cannot be recorded again.
    moved="$NEXT" moved_for="$NEXT_FOR"
    read_secret
    if [ "$NEXT" != "$moved" ] || [ "$NEXT_FOR" != "$moved_for" ]; then
        case "$SECRET_STATE" in
            unreadable*)
                DELIVERY_STATE="not recorded: the Secret could not be read again before the record — $SECRET_STATE"
                rc=1
                ;;
            *)
                DELIVERY_STATE="not recorded: $HOST.next moved during this pass (…$(last8 "$moved") when read, $SECRET_STATE now); the next pass installs it and records that"
                ;;
        esac
        return 0
    fi
    runner_delivery_ack "$CRED" "$HOST" "$NEXT_FOR" "$CUR" "$DEST" "$DELIVERY_CONTEXT" "$PKT_STEP"
}
if [ -n "$NEXT" ] && [ "$CUR" = "$NEXT" ]; then
    record_delivery
fi

finish
