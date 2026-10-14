#!/usr/bin/env bash
# Source-only boss-gcp runner deposit receiver. The forced-command enrollment
# is a separate obligation; this file neither installs nor enrolls itself.
# Trusted host configuration supplies destination and system-of-record URL.
# The transport supplies only a bounded value and its exact rotation binding.
set -euo pipefail
export LC_ALL=C
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
. "$INFRA/lib/jq.sh"
DEST="${BOSS_RUNNER_CREDENTIAL_FILE:-/etc/boss/ops-runner.credential}"
HOST=boss-gcp
CRED=ops-runner-credential-boss-gcp
# EVERY REFUSAL NAMES ITS CHECK (backlog b675ad09). This line was one
# bare sentence for every check below — argv, size, document, duplicate
# key, shape, address, door, packet, destination — so the first boss-gcp
# delivery (forge converge ec127c89, 2026-10-10T19:45Z) recorded THAT it
# was refused and not WHICH check refused, and finding it took the jobs
# API's actor-role report (CLAUDE.md: a verdict must name what failed).
# REASON is a fixed word from this file, plus an HTTP status where the
# check is an answer — never a byte of the envelope, which jq diagnostics
# and an echoed field could carry, so no caller passes one through.
refuse() { echo "runner-credential-recv: refused ($1); installed credential unchanged" >&2; exit 65; }
fail() { echo "runner-credential-recv: failed${1:+ ($1)}; delivery not acknowledged" >&2; exit 1; }
# http_status — CODE as the digits curl wrote, or `none` when no answer
# came, for a reason word.
http_status() { local c="${CODE//[!0-9]/}"; printf '%s' "${c:-none}"; }
# THE RECEIVER SPEAKS FIRST (backlog dea2236f; backlog 1e50e66b, the
# boss-gcp delivery). Its first line, before it reads one byte, is this
# one, word for word: the sender (infra/gcp/runner-credential-send.sh)
# writes the envelope only after reading it, so a deposit key whose line
# was placed without its forced command, and so reaches a shell, is
# handed no credential byte. runner_credential_recv_sh.rs holds the two
# copies of the line equal.
GREETING='runner-credential-recv: boss-gcp: ready for one envelope'
# An EMPTY stream is refused in words of its own: it is what the sender
# hands this receiver when it only proves the enrolled key reaches it
# (the verify step of the runner deposit key's prepare-a-deposit-key
# packet), so that refusal is evidence and must not read like any other.
refuse_empty() { echo 'runner-credential-recv: refused an empty envelope; installed credential unchanged' >&2; exit 65; }
[ "$#" -eq 0 ] || refuse arguments
umask 077
WORK="$(mktemp -d)" || fail
TMP=""
cleanup() { rm -rf "$WORK"; [ -z "$TMP" ] || rm -f "$TMP"; }
trap cleanup EXIT
printf '%s\n' "$GREETING" || fail
head -c 8193 > "$WORK/input" || refuse unread
BYTES="$(stat -c %s "$WORK/input")" || refuse unread
[ "$BYTES" -gt 0 ] || refuse_empty
[ "$BYTES" -le 8192 ] || refuse oversize
# One document and one occurrence of every scalar path. Ordinary jq object
# parsing silently accepts duplicate keys by keeping the last occurrence.
jq -s -e 'length == 1' "$WORK/input" >/dev/null 2>&1 || refuse not-one-document
jq -n --stream -e 'reduce inputs as $row ([]; if ($row | length) == 2 then . + [$row[0]] else . end) | group_by(.) | all(length == 1)' "$WORK/input" >/dev/null 2>&1 || refuse duplicate-key
# jq diagnostics can quote the credential, so all parsing failures are silent.
jq_doc_file "$WORK/input" || refuse not-one-document
jq -e '
  def uuid: type == "string" and test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$");
  type == "object" and (keys == ["attempt","credential_id","host","job_id","secret_uid","value","version"])
  and .version == 1 and .host == "boss-gcp"
  and .credential_id == "ops-runner-credential-boss-gcp"
  and (.job_id | uuid) and (.attempt | uuid)
  and (.secret_uid | uuid)
  and (.value | type == "string" and length >= 32 and length <= 4096 and test("^[A-Za-z0-9_-]+$"))
' "$WORK/input" >/dev/null 2>&1 || refuse envelope-shape
CUR="$(jq -r .value "$WORK/input" 2>/dev/null)" || refuse envelope-unread
NEXT_FOR="$(jq -r .job_id "$WORK/input" 2>/dev/null)" || refuse envelope-unread
BOUND="$(jq -c 'del(.value,.host,.version)' "$WORK/input" 2>/dev/null)" || refuse envelope-unread
. "$INFRA/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || refuse no-jobs-url
. "$INFRA/lib/secret-header.sh"
. "$INFRA/lib/runner-delivery-ack.sh"
# EVERY FUNCTION THIS FILE CALLS FROM THEM, OR A FAILURE THAT NAMES IT
# (backlog 50708d76, a low of review run ce15f459 on car 578fc323). This
# runs a COPY whose libraries sit beside it, and a secret-header.sh from
# before machine_token_admits existed answered that call with bash's 127,
# which present() reads as a host the rule refused: every delivery was
# refused naming BOSS_MACHINE_TOKEN_HOSTS, a list that was right. A set
# from two trees is this host's defect, not the transport's, so it fails
# and is not acknowledged; nothing has been written yet.
for need in secret-header.sh:secret_header secret-header.sh:machine_token_header \
    secret-header.sh:machine_token_admits secret-header.sh:machine_token_proxy \
    runner-delivery-ack.sh:runner_delivery_ack; do
    if ! declare -F "${need#*:}" >/dev/null; then
        echo "runner-credential-recv: FAILED — $INFRA/lib/${need%%:*} defines no ${need#*:}, so the file set beside this receiver is not from one tree; reinstall it with infra/gcp/install-runner-credential-receiver.sh; installed credential unchanged, delivery not acknowledged" >&2
        exit 1
    fi
done
API_CURL="$INFRA/boss-api-curl.sh"
BODY="$WORK/body"
# WHO EVERY REQUEST SIGNS AS (backlog b675ad09). The credential door
# binds the ACTOR to automation:ops-runner and keeps the ROLE the request
# asserts — "the credential supplies identity, never a new role"
# (crates/core/boss-jobs/src/runner_credential.rs) — so a request with no
# x-boss-user is the guest role, which holds no job read. This receiver
# sent none, and the rotation packet read below answered 403 to both
# boss-gcp deliveries: the jobs API's actor-role report recorded
# automation:ops-runner asserting `guest`, asserted_allowed false on
# `job` `read`, count 2, first 2026-10-10T19:45:54Z, last 19:56:24Z —
# forge converges ec127c89 and f02e5a33. The forge's deposit
# (infra/forge/runner-credential-deposit.sh) always signed, and its
# rotation (c896cb12) closed. Same row, same role: its
# infra/platform/automations/runner-credential-deposit.toml is the runner
# deposit's attribution on its consuming host, and platform-admin is the
# role automation:ops-runner's own row records, so an enforcing role
# guard reads no mismatch.
BOSS_USER='{"id":"automation:runner-credential-deposit","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
HDRS=(-H "x-boss-user: $BOSS_USER")
machine_token_header MT_HDR "$BOSS_JOBS_URL" || fail machine-token-header
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
# present VALUE — the credential's header, made ONLY for a host the machine
# token's rule admits (machine_token_admits: loopback or
# BOSS_MACHINE_TOKEN_HOSTS). Both presentations below were plain
# secret_header, which judges no host, so a BOSS_JOBS_URL pointed off the
# estate kept the machine token home and sent this (backlog 50708d76, F2
# of review 927f8602 — the shape car 025640e3 repaired in the ops runner).
# The line names the rule and nothing of the URL. The rule also withholds
# a request curl would send through a proxy (backlog 50708d76, F4), and
# the line says which of the two refused.
present() {
    local rule=0
    machine_token_admits "$BOSS_JOBS_URL" || rule=$?
    if [ "$rule" -eq 2 ]; then
        echo "runner-credential-recv: the runner credential is NOT presented — curl would send it through the proxy in \$$(machine_token_proxy "$BOSS_JOBS_URL"), which would read it" >&2
        return 1
    elif [ "$rule" -ne 0 ]; then
        echo 'runner-credential-recv: the runner credential is NOT presented — the host of BOSS_JOBS_URL is not loopback and not in BOSS_MACHINE_TOKEN_HOSTS' >&2
        return 1
    fi
    secret_header RC_HDR "x-boss-runner-credential: $1" || fail credential-header-file
}
present "$CUR" || refuse not-presented
CODE=""
api_get() {
    local path="$1"; shift
    CODE=""
    if ! CODE="$("$API_CURL" -sS -o "$BODY" -w '%{http_code}' "${HDRS[@]}" "$@" "$BOSS_JOBS_URL$path" 2>/dev/null)"; then CODE=""; fi
}
# resolve_bound — 0 when the door resolves the presented value to this
# delivery; else 1 with DOOR naming which of its three checks did not hold.
DOOR=""
resolve_bound() {
    DOOR=""
    api_get /api/jobs/runner-credential -H "$RC_HDR"
    [ "$CODE" = 200 ] || { DOOR="door-http-$(http_status)"; return 1; }
    jq_doc_file "$BODY" || { DOOR=door-not-one-document; return 1; }
    jq -e --argjson bound "$BOUND" '
      .resolved == true and .host == "boss-gcp" and .slot == "next"
      and .delivery == $bound
    ' "$BODY" >/dev/null 2>&1 || { DOOR=door-not-this-delivery; return 1; }
    DELIVERY_CONTEXT="$BOUND"
    SEEN=boss-gcp/next
}
resolve_bound || refuse "$DOOR"
api_get "/api/jobs/$NEXT_FOR" -H "$RC_HDR"
[ "$CODE" = 200 ] || refuse "rotation-packet-http-$(http_status)"
jq_doc_file "$BODY" || refuse rotation-packet-not-one-document
PKT_STEP="$(jq -er --arg c "$CRED" '
  select(.status == "open" and (.subject.id // .subject_id) == $c)
  | [.steps[] | select(.spec_slug == "delivered" and (.status == "ready" or .status == "active" or .status == "completed"))]
  | select(length == 1) | .[0].id
' "$BODY" 2>/dev/null)" || refuse rotation-packet-awaits-no-delivery
[[ $PKT_STEP =~ ^[0-9a-f-]{36}$ ]] || refuse delivered-step-id
[ ! -L "$DEST" ] || refuse destination-symlink
DIR="$(dirname "$DEST")"
[ -d "$DIR" ] || refuse destination-directory-absent
TMP="$(mktemp "$DIR/.runner-recv.XXXXXX")" || fail
printf '%s' "$CUR" > "$TMP" || fail
chmod 600 "$TMP" || fail
if [ "$(id -u)" -eq 0 ]; then chown 0:0 "$TMP" || fail; fi
sync -- "$TMP" || fail
mv -fT "$TMP" "$DEST" || fail
TMP=""
sync -- "$DIR" || fail
# Acknowledgment proves the installed bytes, not merely the transport input.
CUR="$(cat "$DEST" 2>/dev/null)" || fail
present "$CUR" || fail installed-not-presented
resolve_bound || fail "installed-$DOOR"
last8() { printf '%s' "${1: -8}"; }
rc=0
DELIVERY_STATE="not recorded"
runner_delivery_ack "$CRED" "$HOST" "$NEXT_FOR" "$CUR" "$DEST" "$DELIVERY_CONTEXT" "$PKT_STEP"
# DELIVERY_STATE is the ack's own value-free sentence (fixed words and an
# HTTP status; runner-delivery-ack.sh), the one that says which owner
# step refused — dropped here until b675ad09.
[ "$rc" -eq 0 ] || fail "$DELIVERY_STATE"
# The sender retains the owner's original record, not an English success
# sentence. The presented value is not a field of this value-free response.
[[ $DELIVERY_RECEIPT != *"$CUR"* ]] || fail
jq -nc --argjson binding "$BOUND" --argjson receipt "$DELIVERY_RECEIPT" \
    '$binding + {version:1,host:"boss-gcp",recorded:true,original_delivery_receipt:$receipt}'
