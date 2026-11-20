#!/usr/bin/env bash
# revoke-logged-calendar-tokens — count, and through a rendered plan
# revoke, the calendar feeds whose token ONE declared instance's audit
# log still holds in the clear, from inside that instance.
#
#   revoke-logged-calendar-tokens.sh --plan <namespace>
#   revoke-logged-calendar-tokens.sh <namespace> <plan-sha256>
#
# WHY IT EXISTS (backlog 77dc1579; design d33dfbe5, decided by David
# 2026-09-26). Car 4aaff4dc ("a calendar token rests as its hash",
# train #697) left one act owed on every instance: POST
# /api/scheduling/calendar-tokens/logged-raw/revoke, which revokes each
# feed minted before digests, one rotated event each, and answers
# `revoked: 0` when run again. The system of record measured
# logged_raw 0. The playground (namespace boss-playground) is where
# anonymous guests read the audit log at audit-readonly, so it is the
# instance the raw tokens were actually readable on — and nothing the
# dev pod holds reaches it: boss-api is pinned to the system of record,
# BOSS_SOR_SERVICE names a port and never a namespace, and a request
# hand-built at the playground's Service would forge an actor.
#
# REJECTED: an `--instance` on boss-api. It would give the dev pod a
# standing write path to a public, guest-readable instance, and a
# credential and a platform-admin identity there for every future call,
# to do one act. This is one named act and nothing more; a second such
# act gets its own pair, and only a third earns a shared shape (the
# design's decision 5).
#
# THE SHAPE is design 17835005's (David, 2026-09-21: "a rendered plan
# hash, signed with his passkey, single-use, verified before the argv is
# built"), as reap-terminated-pods and expand-instance-volume carry it:
# `plan-a-logged-raw-revoke` runs --plan; `revoke-logged-calendar-tokens`
# declares requires_approval with that verb as its plan_verb and
# emp-david as its approver, so the runner renders the plan onto the
# approve step and runs this only with the hash a passkey signed.
#
# THE BOUND IS THE NAMESPACE, AND IT MUST BE DECLARED. It is one DNS
# label; boss-dev is refused by name (the pipeline's namespace, never an
# instance); and it must be a namespace infra/cluster/instances.toml
# declares, read through render-instance.sh --instances as
# expand-instance-volume reads it. Prod (namespace boss) is one: its
# count is 0, its plan says so, and the runner closes that request as
# nothing to do — "once per instance" without a special case.
#
# THE ROUTE IS FROM INSIDE THE INSTANCE (the design's decision 4; Q1
# resolved 2026-09-26). The one kubectl call is an exec into the
# instance's own `deploy/boss`, container `boss` — every service of an
# instance runs in that container — through the forge's kubectl
# (undeclared-objects.sh --kubectl, the ops kubeconfig). The program it
# runs there is fixed text below, never a packet string: it calls the
# jobs port on loopback, the port read as data from sor-ports.env beside
# this script (the one table of the read surfaces). The machine token
# is THAT container's own, read from its own mount by the image's copy
# of the tree's one reader (machine_token_header,
# /usr/local/bin/lib/secret-header.sh, which the Dockerfile COPYs), and
# handed to curl as a 0600 header file: it is never read out, copied,
# printed, or delivered anywhere — not to the forge, not to the dev
# pod. No curl in the container is a refusal, never an install.
#
# WHO SIGNS (Q2 resolved 2026-09-26). The count is read as
# automation:ops-runner at audit-readonly — it is a count, and any
# caller may read it. The revoke alone asserts platform-admin, as
# automation:ops-runner: the playground's events name that actor and
# never a person, because its audit log is read by anonymous guests and
# open item abf0ce29 is about taking the operator's identity out of it.
# David's approval and passkey stamp stay on the ops-request in the
# system of record, and this run prints that request's id
# ($OPS_REQUEST_ID, which the runner sets) beside the act.
#
# THE PLAN BYTES name the instance, the namespace, the route and the
# count, and what the approval authorises. No clock: the time the count
# was read rides stderr, because a plan that carried it could never be
# re-rendered to the hash that was signed. The count IS in the bytes, so
# a count that moved since the approval refuses (exit 78, both hashes
# named), and a second run of an applied plan — whose count is now 0 —
# refuses too: at most once. Its sha256 goes on stderr as
# `plan-sha256: <hex>`.
#
# THE EFFECT IS READ BACK. After the one POST the count is read again
# through the same route, and only logged_raw 0 prints the effect line
# (with the `revoked` number the API answered and the count before). A
# count that does not read back 0 is NOT proven: exit 1, naming what it
# read. An approved plan whose count was 0 sends no POST and says so.
#
# ENV (test seams; the ops runner passes a verb no environment from the
# packet, only an argv built from the allowlist): BOSS_KUBECTL and
# BOSS_CLUSTER_TREE, read by undeclared-objects.sh and render-instance.sh;
# BOSS_LOGGED_RAW_POD_LIB, the directory holding secret-header.sh as the
# IN-POD program sees it (default /usr/local/bin/lib, the image's).
#
# Exit 78 is a refusal (the request was wrong, or is no longer true); 1
# is a read or a write that genuinely failed, including a read that could
# not look — no evidence is not a plan.
set -uo pipefail

ME=revoke-logged-calendar-tokens
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
PORTS="$HERE/sor-ports.env"
POD_LIB="${BOSS_LOGGED_RAW_POD_LIB:-/usr/local/bin/lib}"
COUNT_PATH=/api/scheduling/calendar-tokens/logged-raw
REVOKE_PATH=/api/scheduling/calendar-tokens/logged-raw/revoke
ACTOR=automation:ops-runner
LABEL='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
say() { echo "$ME: $*" >&2; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 1 ] || refuse "usage: $ME --plan <namespace> — the plan takes the instance's namespace and nothing else"
else
    [ $# -eq 2 ] || refuse "usage: $ME <namespace> <plan-sha256> — this revokes only under an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-logged-raw-revoke verb)"
    [[ "$2" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$2'"
    APPROVED="$2"
fi
NS="$1"
[[ "$NS" =~ $LABEL ]] || refuse "the namespace must be one DNS label, got '$NS'"
[ "$NS" != boss-dev ] || refuse "boss-dev is the pipeline's namespace, not an instance's — this verb acts only on an instance infra/cluster/instances.toml declares"

# --- the namespace, against instances.toml through the renderer ------------
INSTANCES="$("$REPO/infra/cluster/render-instance.sh" --instances)" \
    || fail "infra/cluster/instances.toml would not render (see above), so the namespace bound cannot be read"
INSTANCE=""
while IFS=$'\t' read -r name ns _; do
    [ -n "$ns" ] && [ "$ns" = "$NS" ] && INSTANCE="$name"
done <<<"$INSTANCES"
[ -n "$INSTANCE" ] || refuse "the namespace '$NS' is not an instance in infra/cluster/instances.toml; this verb revokes in $(printf '%s\n' "$INSTANCES" | cut -f2 | tr '\n' ' ' | sed 's/ $//; s/ /, /g')"

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the instance's answer cannot be read, and no evidence is not a plan"

# --- the jobs port, as data -------------------------------------------------
[ -r "$PORTS" ] || fail "$PORTS is missing — the jobs port is not guessed"
PORT=$(sed -n 's/^[[:space:]]*jobs=\([0-9][0-9]*\)[[:space:]]*$/\1/p' "$PORTS")
case "${PORT:-empty}" in
    empty | *[!0-9]*) fail "$PORTS names no jobs=<port> line — the jobs port is not guessed" ;;
esac

KUBECTL_LINE=$("$RESOLVE" --kubectl) || fail "no kubectl to reach the instance with (above)"
read -r -a KUBECTL <<<"$KUBECTL_LINE"

WORK=$(mktemp -d) || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the program that runs INSIDE the instance's boss container ------------
# POSIX sh: the image's /bin/sh is dash. $1 the jobs port, $2 `count` or
# `revoke`, $3 the directory holding the image's secret-header.sh. It
# prints the answer's body on stdout and exits 0 only on HTTP 200. The
# reader's EXIT trap is chained in front of this one (secret-header.sh,
# ORDER), so the 0600 header file is gone when the program exits.
IN_POD=$(cat <<'SH'
set -u
port=$1
mode=$2
lib=$3
case $mode in
    count)
        method=GET
        path=/api/scheduling/calendar-tokens/logged-raw
        user='{"id":"automation:ops-runner","role":"audit-readonly","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
        ;;
    revoke)
        method=POST
        path=/api/scheduling/calendar-tokens/logged-raw/revoke
        user='{"id":"automation:ops-runner","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
        ;;
    *) echo "in-pod: no such mode '$mode'" >&2; exit 2 ;;
esac
command -v curl >/dev/null 2>&1 || { echo "in-pod: this container has no curl, and this verb installs none" >&2; exit 3; }
[ -r "$lib/secret-header.sh" ] || { echo "in-pod: $lib/secret-header.sh is missing — the machine token has no reader here, so nothing was sent" >&2; exit 3; }
d=$(mktemp -d) || exit 1
trap 'rm -rf "$d"' EXIT
. "$lib/secret-header.sh"
url="http://127.0.0.1:$port$path"
machine_token_header MT_HDR "$url" || { echo "in-pod: the machine token's header file could not be written, so nothing was sent" >&2; exit 1; }
code=$(curl -sS --max-time 30 -o "$d/body" -w '%{http_code}' -X "$method" ${MT_HDR:+-H "$MT_HDR"} -H "x-boss-user: $user" "$url") || { echo "in-pod: curl could not complete $method $path on loopback" >&2; exit 4; }
cat "$d/body"
echo
[ "$code" = 200 ] || { echo "in-pod: $method $path answered HTTP $code" >&2; exit 5; }
SH
)

# in_pod <count|revoke> <out-file> — one exec; the instance's words kept.
in_pod() {
    "${KUBECTL[@]}" -n "$NS" exec deploy/boss -c boss -- \
        sh -c "$IN_POD" logged-raw "$PORT" "$1" "$POD_LIB" > "$2" 2> "$WORK/exec.err"
}

# read_count -> COUNT, or a failure naming what the instance said.
read_count() {
    if ! in_pod count "$WORK/count.json"; then
        sed 's/^/    /' "$WORK/exec.err" >&2
        sed 's/^/    /' "$WORK/count.json" >&2
        fail "GET $COUNT_PATH inside $NS (deploy/boss, container boss) did not answer (above) — an unread count is not a count"
    fi
    COUNT=$(jq -r '.logged_raw // empty' "$WORK/count.json" 2> /dev/null)
    case "${COUNT:-empty}" in
        empty | *[!0-9]*)
            sed 's/^/    /' "$WORK/count.json" >&2
            fail "GET $COUNT_PATH inside $NS answered no whole logged_raw (above)"
            ;;
    esac
    say "read logged_raw $COUNT in $NS at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
}

# --- the plan document -------------------------------------------------------
render() { # <count> — pure in its input: no clock, no cluster.
    local via="on the instance's jobs port $PORT, from inside deploy/boss container boss through kubectl exec"
    echo "plan: $ME"
    echo "instance: $INSTANCE"
    echo "namespace: $NS"
    echo "read: GET $COUNT_PATH $via, as $ACTOR at audit-readonly"
    echo "logged_raw: $1"
    echo
    echo "== the act this plan authorises =="
    if [ "$1" -eq 0 ]; then
        echo "none — nothing to revoke"
    else
        echo "POST $REVOKE_PATH $via, ONCE, as $ACTOR at platform-admin: it revokes every calendar feed whose token the instance's audit log holds in the clear ($1 now), one rotated event each. Then logged_raw is read back; only 0 is the verdict."
    fi
}

hash_of() { sha256sum "$1" | cut -d' ' -f1; }

read_count
BEFORE="$COUNT"
render "$BEFORE" > "$WORK/plan"
HASH=$(hash_of "$WORK/plan")

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the act -------------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "the plan as it stands (above) hashes to $HASH, not the approved $APPROVED — the count moved since it was approved, or the plan was already applied. Nothing was revoked; render and approve it again"
fi

# THE CAPTURE BEFORE THE ACT: the approved plan, on stdout, first.
cat "$WORK/plan"
echo
if [ "$BEFORE" -eq 0 ]; then
    echo "$ME: plan $APPROVED still holds and names no token — nothing to revoke in $NS"
    exit 0
fi
echo "$ME: plan $APPROVED still holds — revoking $BEFORE in $NS as $ACTOR (ops-request ${OPS_REQUEST_ID:-unnamed})"
say "ops-request ${OPS_REQUEST_ID:-unnamed} — the approval and its passkey stamp are on that packet in the system of record; the instance's events name $ACTOR"

if ! in_pod revoke "$WORK/revoke.json"; then
    sed 's/^/    /' "$WORK/exec.err" >&2
    sed 's/^/    /' "$WORK/revoke.json" >&2
    rc_note="the revoke did not answer 200 (above)"
    REVOKED=""
else
    REVOKED=$(jq -r '.revoked // empty' "$WORK/revoke.json" 2> /dev/null)
    rc_note=""
fi

# The answer is not the effect: read the count back, whatever it said.
read_count
AFTER="$COUNT"
[ -z "$rc_note" ] || fail "$rc_note; read back: logged_raw $AFTER (was $BEFORE)"
case "${REVOKED:-empty}" in
    empty | *[!0-9]*) fail "POST $REVOKE_PATH inside $NS answered no whole revoked count; read back: logged_raw $AFTER (was $BEFORE)" ;;
esac
[ "$AFTER" -eq 0 ] || fail "the revoke answered revoked $REVOKED, and read back: logged_raw $AFTER (was $BEFORE) — NOT proven; the request stands"
[ "$REVOKED" -eq "$BEFORE" ] || say "the revoke answered revoked $REVOKED against the plan's $BEFORE — a feed rotated by its own employee between the plan and the act is no longer logged; the read-back is the verdict"
echo "$ME: $NS revoked $REVOKED — read back: logged_raw 0 (was $BEFORE)"
