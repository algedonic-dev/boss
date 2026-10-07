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
refuse() { echo 'runner-credential-recv: refused; installed credential unchanged' >&2; exit 65; }
fail() { echo 'runner-credential-recv: failed; delivery not acknowledged' >&2; exit 1; }
[ "$#" -eq 0 ] || refuse
umask 077
WORK="$(mktemp -d)" || fail
TMP=""
cleanup() { rm -rf "$WORK"; [ -z "$TMP" ] || rm -f "$TMP"; }
trap cleanup EXIT
head -c 8193 > "$WORK/input" || refuse
BYTES="$(stat -c %s "$WORK/input")" || refuse
[ "$BYTES" -gt 0 ] && [ "$BYTES" -le 8192 ] || refuse
# One document and one occurrence of every scalar path. Ordinary jq object
# parsing silently accepts duplicate keys by keeping the last occurrence.
jq -s -e 'length == 1' "$WORK/input" >/dev/null 2>&1 || refuse
jq -n --stream -e 'reduce inputs as $row ([]; if ($row | length) == 2 then . + [$row[0]] else . end) | group_by(.) | all(length == 1)' "$WORK/input" >/dev/null 2>&1 || refuse
# jq diagnostics can quote the credential, so all parsing failures are silent.
jq_doc_file "$WORK/input" || refuse
jq -e '
  def uuid: type == "string" and test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$");
  type == "object" and (keys == ["attempt","credential_id","host","job_id","secret_uid","value","version"])
  and .version == 1 and .host == "boss-gcp"
  and .credential_id == "ops-runner-credential-boss-gcp"
  and (.job_id | uuid) and (.attempt | uuid)
  and (.secret_uid | uuid)
  and (.value | type == "string" and length >= 32 and length <= 4096 and test("^[A-Za-z0-9_-]+$"))
' "$WORK/input" >/dev/null 2>&1 || refuse
CUR="$(jq -r .value "$WORK/input" 2>/dev/null)" || refuse
NEXT_FOR="$(jq -r .job_id "$WORK/input" 2>/dev/null)" || refuse
BOUND="$(jq -c 'del(.value,.host,.version)' "$WORK/input" 2>/dev/null)" || refuse
. "$INFRA/lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || refuse
. "$INFRA/lib/secret-header.sh"
. "$INFRA/lib/runner-delivery-ack.sh"
API_CURL="$INFRA/boss-api-curl.sh"
BODY="$WORK/body"
HDRS=()
machine_token_header MT_HDR "$BOSS_JOBS_URL" || fail
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
secret_header RC_HDR "x-boss-runner-credential: $CUR" || fail
CODE=""
api_get() {
    local path="$1"; shift
    CODE=""
    if ! CODE="$("$API_CURL" -sS -o "$BODY" -w '%{http_code}' "${HDRS[@]}" "$@" "$BOSS_JOBS_URL$path" 2>/dev/null)"; then CODE=""; fi
}
resolve_bound() {
    api_get /api/jobs/runner-credential -H "$RC_HDR"
    [ "$CODE" = 200 ] || return 1
    jq_doc_file "$BODY" || return 1
    jq -e --argjson bound "$BOUND" '
      .resolved == true and .host == "boss-gcp" and .slot == "next"
      and .delivery == $bound
    ' "$BODY" >/dev/null 2>&1 || return 1
    DELIVERY_CONTEXT="$BOUND"
    SEEN=boss-gcp/next
}
resolve_bound || refuse
api_get "/api/jobs/$NEXT_FOR" -H "$RC_HDR"
[ "$CODE" = 200 ] || refuse
jq_doc_file "$BODY" || refuse
PKT_STEP="$(jq -er --arg c "$CRED" '
  select(.status == "open" and (.subject.id // .subject_id) == $c)
  | [.steps[] | select(.spec_slug == "delivered" and (.status == "ready" or .status == "active" or .status == "completed"))]
  | select(length == 1) | .[0].id
' "$BODY" 2>/dev/null)" || refuse
[[ $PKT_STEP =~ ^[0-9a-f-]{36}$ ]] || refuse
[ ! -L "$DEST" ] || refuse
DIR="$(dirname "$DEST")"
[ -d "$DIR" ] || refuse
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
secret_header RC_HDR "x-boss-runner-credential: $CUR" || fail
resolve_bound || fail
last8() { printf '%s' "${1: -8}"; }
rc=0
DELIVERY_STATE="not recorded"
runner_delivery_ack "$CRED" "$HOST" "$NEXT_FOR" "$CUR" "$DEST" "$DELIVERY_CONTEXT" "$PKT_STEP"
[ "$rc" -eq 0 ] || fail
# The sender retains the owner's original record, not an English success
# sentence. The presented value is not a field of this value-free response.
[[ $DELIVERY_RECEIPT != *"$CUR"* ]] || fail
jq -nc --argjson binding "$BOUND" --argjson receipt "$DELIVERY_RECEIPT" \
    '$binding + {version:1,host:"boss-gcp",recorded:true,original_delivery_receipt:$receipt}'
