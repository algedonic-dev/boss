#!/usr/bin/env bash
#
# machine-token-effect — does THIS HOST's own stamped call reach the
# jobs API as an accepted machine token? Asked of the gate, recorded on
# the calling converge's packet, and never a reason to stop anything.
#
#   machine-token-effect.sh
#
# WHY IT EXISTS (design-doc bdc60b65, question gcp-push; backlog
# 88379df3). boss-gcp's token is PUSHED to it by the cluster's
# boss-machine-token-push CronJob, and the push proves only that the
# receiver took three slots of the lengths sent. Whether a caller on this
# host is then ADMITTED is a different fact, and it is this host's to
# state: a file being there is not it (CLAUDE.md, "mostly sure vs.
# absolutely sure"). So the host's converge asks with the SAME reader
# every caller uses — infra/lib/secret-header.sh machine_token_header,
# reading the directory it reads by default — and records the gate's own
# word for the value presented.
#
# WHAT IT RECORDS, as `machine_token_effect` on the run summary and one
# line on stdout:
#   matched current    the gate admits this host's stamped read;
#   matched next / previous   accepted, with which side is behind;
#   NOT ACCEPTED       the gate matches the value to NO slot — this
#                      host's callers are tallied as misses;
#   NOT STAMPED        the reader sent no token though a slot is held
#                      (the jobs API's host is not in
#                      BOSS_MACHINE_TOKEN_HOSTS, or the slot is unusable);
#   nothing to present this host holds no current slot — not pushed yet,
#                      or the key is not enrolled;
#   UNVERIFIED         no answer, or no address: said, never read as a
#                      pass.
#
# THE SEND IS BOUNDED AND ITS HEADER FILE DOES NOT OUTLIVE THE UNIT
# (review a92c0b94 F1). The read waits twenty seconds at most, and the
# 0600 header file the reader makes lands in $RUNTIME_DIRECTORY when the
# unit declares one — boss-gcp-converge.service does: tmpfs under /run,
# removed by systemd whatever the exit — so a run killed mid-request
# leaves no token header on the host's disk.
#
# NO VALUE IS PRINTED, not even a suffix. Exit 0 always: this is a
# reading, and the converge that takes it owes it nothing.
# Tracing off, first: an exported SHELLOPTS=xtrace would print the header
# line, token and all (review 9a1e289b, F8).
set +x
set -uo pipefail

ME=machine-token-effect
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"

DIR="${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}"
# The one wait this makes, bounded (review 9a1e289b, B1: a reading taken
# inside a converge spends the converge's time). The caller bounds the
# whole script again.
BOUND_S="${BOSS_EFFECT_BOUND_S:-15}"
case "$BOUND_S" in '' | *[!0-9]*) BOUND_S=15 ;; esac
ERR="$(mktemp)" || ERR=/dev/null
BODY="$(mktemp)" || BODY=/dev/null
cleanup() {
    [ "$ERR" = /dev/null ] || rm -f "$ERR"
    [ "$BODY" = /dev/null ] || rm -f "$BODY"
}
# Set BEFORE machine_token_header, which chains its own cleanup in front.
trap cleanup EXIT

EFFECT=""
done_with() {
    EFFECT="$1"
    run_summary_field machine_token_effect "$EFFECT"
    echo "$ME: $EFFECT"
    exit 0
}
why() { head -n 1 "$ERR" 2>/dev/null | cut -c1-200; }

if [ ! -f "$DIR/current" ] || [ ! -s "$DIR/current" ]; then
    done_with "nothing to present: this host holds no current slot at $DIR, so its callers send without the token (the push has not delivered one, or its key is not enrolled yet)"
fi
[ -n "${BOSS_JOBS_URL:-}" ] \
    || done_with "UNVERIFIED: BOSS_JOBS_URL is unset (/etc/boss/sor.env), so no stamped read could be made"

MT_HDR=""
if ! machine_token_header MT_HDR "$BOSS_JOBS_URL" 2>"$ERR"; then
    done_with "UNVERIFIED: the header file could not be written ($(why))"
fi
if [ -z "$MT_HDR" ]; then
    said="$(why)"
    done_with "NOT STAMPED: machine_token_header, reading $DIR, sent no token to the jobs API${said:+ — $said} (is the jobs API's host in BOSS_MACHINE_TOKEN_HOSTS of /etc/boss/sor.env?)"
fi

BOSS_USER='{"id":"automation:machine-token-effect","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
API_CURL="$INFRA/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
code=""
if ! code="$(BOSS_API_RETRY_DEADLINE="${BOSS_EFFECT_WAIT:-$BOUND_S}" "$API_CURL" -sS --max-time "$BOUND_S" \
    -o "$BODY" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" -H "$MT_HDR" \
    "$BOSS_JOBS_URL/api/machine-gate/accepts" 2>/dev/null)"; then
    code=""
fi
case "$code" in
    200) ;;
    '' | 000) done_with "UNVERIFIED: the jobs API did not answer GET /api/machine-gate/accepts inside its bound; the next tick asks again" ;;
    *) done_with "UNVERIFIED: GET /api/machine-gate/accepts answered HTTP $code" ;;
esac
# A body that does not parse is its own answer, never an empty match.
if ! answer="$(jq -r '[(.matched // "" | tostring), (.mode // "unknown" | tostring), (.degraded // false | tostring)] | join("|")' "$BODY" 2>/dev/null)"; then
    done_with "UNVERIFIED: GET /api/machine-gate/accepts answered 200 with a body that is not the gate's JSON"
fi
IFS='|' read -r matched mode degraded <<< "$answer"
tail=""
[ "$degraded" != true ] || tail="; the gate reports itself DEGRADED (it holds no readable slot of its own)"
case "$matched" in
    current) done_with "matched current: the jobs API's gate (mode $mode) admits this host's stamped read as the current slot$tail" ;;
    next) done_with "matched next: the gate (mode $mode) knows this host's current slot as its next — its own mount is one refresh behind the promotion; accepted$tail" ;;
    previous) done_with "matched previous: the gate (mode $mode) has promoted past the slot this host holds — the next push follows; accepted, and a rotation's revoke waits on it$tail" ;;
    none) done_with "NOT ACCEPTED: the gate (mode $mode) matches the value this host presents to NO slot — this host's callers are tallied as misses$tail" ;;
    *) done_with "UNVERIFIED: GET /api/machine-gate/accepts answered 200 with no matched field" ;;
esac
