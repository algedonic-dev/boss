# sor-read.sh — how a lint reads a registry off the system of record:
# one GET, the body to a file, the HTTP code on stdout, and a rollout
# waited out rather than refused.
#
# Source it, then read:
#
#   . infra/lint/lib/sor-read.sh || exit 3
#   code=$(lint_sor_read "$NAME" "the jobs API" "$URL" "$body")
#   [ "$code" = "200" ] || skip "$URL answered HTTP $code"
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
# probe reads with through boss-sor-read (boss-cli identity.rs
# `reader_header`). The id is the gate's own actor, automation:gate-runner,
# because every lint runs under infra/gate.sh; it signs here with the
# read role alone, so no lint can write through this helper.
# `a_lint_read_of_the_record_is_signed` holds every caller to it.
LINT_SOR_READER='{"id":"automation:gate-runner","role":"audit-readonly","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# lint_sor_read WHO SERVICE URL BODY_FILE — prints the HTTP code.
lint_sor_read() {
    local who=$1 service=$2 url=$3 body=$4 code
    local window=${BOSS_SOR_WAIT_SECONDS:-$LINT_SOR_WAIT_SECONDS}
    case "$window" in
        *[!0-9]*)
            echo "$who: BOSS_SOR_WAIT_SECONDS='$window' is not a whole number of seconds — refusing to read $url rather than reading it as no wait or as forever" >&2
            printf '000'
            return 0
            ;;
    esac
    code=$(curl_through_a_roll "$window" "$who" "$service" "GET $url" \
        -sS -m 10 -H "x-boss-user: $LINT_SOR_READER" \
        -o "$body" -w '%{http_code}' "$url") || true
    printf '%s' "${code:-000}"
}
