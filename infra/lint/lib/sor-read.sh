# sor-read.sh — how a lint reads a registry off the system of record:
# one GET, the body to a file, the HTTP code on stdout, and a rollout
# waited out rather than refused.
#
# Source it, then read:
#
#   . infra/lint/lib/sor-read.sh || exit 3
#   code=$(lint_sor_read "$NAME" "the jobs API" "$URL" "$body" BOSS_JOBS_URL)
#   [ "$code" = "200" ] || skip "$URL answered HTTP $code"
#
# The last argument is the NAME of the env var the lint's header gives
# for its read surface: unset, the lint is reading its default, and a
# door on PATH answers it (A LINT RUN BY HAND, below).
#
# The code is curl's `%{http_code}`, and `000` when nothing answered —
# the word each lint's skip already read as "never got an answer", so a
# refusal reads exactly as it did before this helper existed.
#
# WHY (backlog 834ddb7c, measured 2026-09-23 ~09:05Z). Gate-run 44da8e5a
# was refused before any check ran: the-live-protocols-are-the-authored-
# protocols curled the jobs API during a stack rollout (train #576
# converging) and got HTTP 000, and the relaunch a minute later went
# green on the same diff. The door had learned to wait out that dark
# minute that morning (034002b3); the three lints that curl a registry
# themselves each carried their own bare `curl -sS -m 10` line and had
# not. The refusal was the right exit — 3, an infrastructure refusal,
# never a red (a26f92c4) — but it cost a relaunch the machine could have
# spent waiting. The wait is NOT defined here: it is the door's own
# `curl_through_a_roll`, sourced from infra/lib, so the lints and the
# door re-send exactly the same exits (CLAUDE.md §9a).
#
# THE WINDOW: 60 seconds, against the door's 120. A roll takes the API
# dark for about a minute (034002b3), and a lint that meets it has
# already lost however much of that minute passed before it launched,
# so 60s from the first refused connect covers a roll met anywhere in
# it. The door waits longer because an operator is waiting on a write;
# a lint's wait is a whole gate sitting on a dark registry, and the
# pre-flight runs three such lints in sequence, so a registry that is
# genuinely down — not rolling — costs at most three minutes before the
# gate refuses, the answer it would have given anyway. BOSS_SOR_WAIT_SECONDS
# overrides it (0 = not at all), the door's own knob: a box that cannot
# reach the cluster at all, a workstation off the LAN, sets it to 0.
#
# curl's own stderr is kept: the reason a connect failed ("No route to
# host", "Connection refused", "Could not resolve host") is the first
# thing the next reader of a refusal asks, and until this helper every
# one of these reads sent it to /dev/null.

# shellcheck source=infra/lib/curl-through-a-roll.sh
. "$(dirname "${BASH_SOURCE[0]}")/../../lib/curl-through-a-roll.sh" || exit 3

LINT_SOR_WAIT_SECONDS=60

# THE READ IS SIGNED, AS A READER (backlog e76582c1, measured
# 2026-09-27 ~12:15Z). Until then this helper sent no `x-boss-user`,
# and an anonymous caller is answered a smaller world, not an error:
# once the guest-reads car (5763e52e / 493cebf3, train #744) scoped the
# dispatcher's rule read, a headerless GET answered 200 `{rules: [],
# error: "…withheld…"}`, the-live-rules-are-the-authored-rules counted
# zero enforced rules, and every gate that ran it went red while a
# signed read of the same dispatcher answered 79. A lint judges the
# SYSTEM, so it reads what the system holds: role `audit-readonly`
# (core policy's Read at Scope::All on every shipped resource, and no
# other action anywhere) at the auditor tier — the shape a recorded
# probe reads with through boss-sor-read (`boss_core::roles::
# reader_header`). The id is the gate's own actor, automation:gate-runner,
# because every lint runs under infra/gate.sh; it signs here with the
# read role alone, so no lint can write through this helper.
# `a_lint_read_of_the_record_is_signed` holds every caller to it. The
# shape itself is infra/lib/sor-reader.sh's, the one shell spelling
# node-roles.sh signs with too (backlog e5f7b51e; it was a literal copy
# here until then).
# shellcheck source=infra/lib/sor-reader.sh
. "$(dirname "${BASH_SOURCE[0]}")/../../lib/sor-reader.sh" || exit 3
LINT_SOR_READER=$(sor_reader_header automation:gate-runner)

# A READ HANDED IN (backlog 44b2087e, 2026-10-09). A lint holds no
# machine token by rule, so a script that DOES hold one and runs a lint
# — the converge's infra/forge/registry-drift.sh, the daily
# infra/protocol-drift.sh — sent the lint's read with none: one tokenless
# GET /api/workflows and GET /api/dispatcher/rules per converge on the
# machine-gate window, signed automation:gate-runner from the forge. Such
# a script reads the URL itself, stamped, and hands the read in on the
# command that runs the lint:
#
#   BOSS_LINT_SOR_HANDED_URL   the URL the lint asks, exactly
#   BOSS_LINT_SOR_HANDED_CODE  the HTTP code it got (000: no answer)
#   BOSS_LINT_SOR_HANDED_BODY  a file holding the body
#
# and the lint answers from that, sending nothing. A read of ANOTHER URL,
# or a 200 whose body cannot be read, answers 000 — never a request in its
# place, which would be the tokenless read the hand-over exists to remove.
# Empty or unset, the lint reads for itself as it always did. Every
# stamping script that runs such a lint hands it in on that same command
# (every_shell_identity_sender_presents_the_machine_token.rs).
lint_sor_handed() { # WHO URL BODY_FILE — prints the HTTP code
    local who=$1 url=$2 body=$3 code=${BOSS_LINT_SOR_HANDED_CODE:-000}
    if [ "$BOSS_LINT_SOR_HANDED_URL" != "$url" ]; then
        echo "$who: this run's launcher handed in its read of $BOSS_LINT_SOR_HANDED_URL and this lint asks $url — no request is sent in its place, because the launcher holds the machine token and a lint does not" >&2
        printf '000'
        return 0
    fi
    case "$code" in [0-9][0-9][0-9]) ;; *) code=000 ;; esac
    if ! cp -- "${BOSS_LINT_SOR_HANDED_BODY:-}" "$body" 2>/dev/null && [ "$code" = "200" ]; then
        echo "$who: GET $url was handed in as HTTP 200 with no readable body (${BOSS_LINT_SOR_HANDED_BODY:-unset}) — read as no answer, and no request is sent in its place" >&2
        code=000
    fi
    echo "$who: GET $url was read by this run's launcher and handed in (HTTP $code) — no request from this lint" >&2
    printf '%s' "$code"
}

# A LINT RUN BY HAND READS THROUGH THE DOOR (backlog 83bff88b, from
# 0914f277). The hand-over above serves a launcher that holds the token;
# an agent or operator running the-live-protocols or the-live-rules by
# hand on the dev pod has none, so the read below went out with no token,
# signed automation:gate-runner — three would-refuse facts from the dev
# pod on the machine gate's window, 2026-10-09 17:17-20:01Z. That
# container holds the token for its doors (infra/dev/machine-token-dir),
# so the read goes through `boss-api`, which stamps it — the way the
# pod's boss-sor-read closed the same hole for a probe rehearsal
# (37742794 R3) and the pre-flight for the edit level (R2). Only when:
#   - nothing was handed in (that branch runs first and sends nothing);
#   - the run names no instance: neither the lint's own URL variable
#     (the fifth argument) nor BOSS_JOBS_URL, which is the door's base,
#     so a lint aimed at a named instance reads THAT instance, unstamped,
#     exactly as before;
#   - a `boss-api` is on PATH. The gate's runner has none, so the gate
#     reads exactly as it did.
# The door is asked for the URL's path with the lint's window, and the
# path decides the service: a BOSS_SOR_SERVICE the caller's shell carried
# is dropped, or a dispatcher question would go to the gateway. The door
# signs as the session's actor where one is named (the same world on
# every list as audit-readonly, 61085a9e), and as audit-readonly where
# none is. A door that cannot answer is NO ANSWER (000), with its own
# words on stderr — never a curl in its place, which would be the
# tokenless read this exists to remove (the rule the hand-over keeps).
# Pinned by a_hand_run_lint_reads_the_record_through_the_door.rs.
lint_sor_through_the_door() { # WHO URL BODY_FILE WINDOW — prints the HTTP code
    local who=$1 url=$2 body=$3 window=$4 rest path err code
    rest=${url#*://}
    case "$rest" in
        */*) path="/${rest#*/}" ;;
        *)
            echo "$who: $url names no path to ask the door (boss-api) for — no request is sent" >&2
            printf '000'
            return 0
            ;;
    esac
    err=$(mktemp) || { printf '000'; return 0; }
    (unset BOSS_SOR_SERVICE; BOSS_SOR_WAIT_SECONDS=$window boss-api GET "$path" >"$body" 2>"$err" </dev/null)
    code=$(sed -n 's/^HTTP:\([0-9][0-9][0-9]\)$/\1/p' "$err" | tail -n 1)
    grep -v '^HTTP:[0-9][0-9][0-9]$' "$err" >&2 || true
    rm -f "$err"
    code=${code:-000}
    echo "$who: GET $path was read through the door (boss-api, which presents this container's machine token) — HTTP $code, and no request from this lint" >&2
    printf '%s' "$code"
}

# lint_sor_read WHO SERVICE URL BODY_FILE URL_VAR — prints the HTTP code.
lint_sor_read() {
    local who=$1 service=$2 url=$3 body=$4 url_var=${5:-} code
    if [ -n "${BOSS_LINT_SOR_HANDED_URL:-}" ]; then
        lint_sor_handed "$who" "$url" "$body"
        return 0
    fi
    local window=${BOSS_SOR_WAIT_SECONDS:-$LINT_SOR_WAIT_SECONDS}
    case "$window" in
        *[!0-9]*)
            echo "$who: BOSS_SOR_WAIT_SECONDS='$window' is not a whole number of seconds — refusing to read $url rather than reading it as no wait or as forever" >&2
            printf '000'
            return 0
            ;;
    esac
    # A caller that cannot say which variable names its instance cannot
    # say whether one was named: refused, never guessed either way.
    case "$url_var" in
        '' | *[!A-Za-z0-9_]*)
            echo "$who: lint_sor_read was not told which variable names this lint's read surface (got '$url_var') — refusing to read $url" >&2
            printf '000'
            return 0
            ;;
    esac
    if [ -z "${!url_var:-}" ] && [ -z "${BOSS_JOBS_URL:-}" ] \
        && command -v boss-api >/dev/null 2>&1; then
        lint_sor_through_the_door "$who" "$url" "$body" "$window"
        return 0
    fi
    code=$(curl_through_a_roll "$window" "$who" "$service" "GET $url" \
        -sS -m 10 -H "x-boss-user: $LINT_SOR_READER" \
        -o "$body" -w '%{http_code}' "$url") || true
    printf '%s' "${code:-000}"
}
