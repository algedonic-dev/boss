#!/usr/bin/env bash
# Inert source delivery door: a separately completed exact-purpose enrollment
# is required before reading the staged value or attempting SSH. No key mint,
# receiver enrollment, host-key discovery or runtime installation happens here.
set -euo pipefail
export LC_ALL=C
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
refuse() { echo "runner-credential-send: refused: $1" >&2; exit 78; }
fail() { echo 'runner-credential-send: failed; remote delivery not proved' >&2; exit 1; }
ENROLLMENT="${BOSS_RUNNER_DEPOSIT_ENROLLMENT:-}"
[[ $ENROLLMENT =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || refuse 'recorded enrollment packet absent'
[ "$#" -eq 0 ] || refuse 'no transport-selected arguments are accepted'
. "$INFRA/lib/sor.sh"
. "$INFRA/lib/jq.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || refuse 'system-of-record configuration absent'
KEY="${BOSS_RUNNER_DEPOSIT_KEY:-/etc/boss/runner-deposit-key/private_key}"
KNOWN="${BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS:-/etc/boss/runner-deposit-key/known_hosts}"
[ -f "$KEY" ] && [ -f "$KNOWN" ] || refuse 'enrollment transport or pinned host key absent'
umask 077
WORK="$(mktemp -d)" || fail
trap 'rm -rf "$WORK"' EXIT
# A Secret volume's read-only mode is not the host SSH client's private-key
# mode. The private copy is scoped to this run and removed on every exit.
cat "$KEY" > "$WORK/key" || refuse 'enrollment transport key unreadable'
chmod 600 "$WORK/key" || fail
. "$INFRA/lib/secret-header.sh"
machine_token_header MT_HDR "$BOSS_JOBS_URL" || fail
HDRS=(-H "x-boss-user: $(sor_reader_header automation:runner-credential-deposit)")
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
API_CURL="$INFRA/boss-api-curl.sh"
CODE="$("$API_CURL" -sS "${HDRS[@]}" -o "$WORK/enrollment" -w '%{http_code}' "$BOSS_JOBS_URL/api/jobs/$ENROLLMENT" 2>/dev/null)" || fail
[ "$CODE" = 200 ] || refuse 'enrollment packet unreadable'
PUBLIC="$(ssh-keygen -y -f "$WORK/key" 2>/dev/null)" || refuse 'enrollment key unreadable'
PUBLIC="$(printf '%s\n' "$PUBLIC" | awk 'NF >= 2 {print $1 " " $2}')" || refuse 'enrollment key unreadable'
jq_doc_file "$WORK/enrollment" || refuse 'enrollment packet contains no document'
jq -e --arg public "$PUBLIC" --arg enrollment "$ENROLLMENT" '
  .id == $enrollment and .kind == "prepare-a-deposit-key" and (.subject.id // .subject_id) == "runner-deposit-key"
  and ([.steps[] | select(.spec_slug == "enroll" and .status == "completed"
    and .metadata.decision == "approved" and .metadata.receiver_host == "boss-gcp"
    and .metadata.purpose == "ops-runner credential deposit"
    and (.metadata.enrollment_record | type == "string" and length > 0)
    and (.metadata.public_key | split(" ") | .[0:2] | join(" ")) == $public)] | length == 1)
' "$WORK/enrollment" >/dev/null 2>&1 || refuse 'exact receiver enrollment absent or mismatched'
ADDRESS="$(awk '
  /^\[\[node\]\]$/ {selected=0}
  /^id = "boss-gcp"$/ {selected=1}
  selected && /^address = / {gsub(/"/, "", $3); print $3}
' "$INFRA/estate/estate.toml")" || refuse 'enrolled estate host unavailable'
TARGET="${BOSS_RUNNER_DEPOSIT_TARGET:-}"
[[ $TARGET =~ ^[a-z_][a-z0-9_-]*@[^@[:space:]]+$ ]] || refuse 'enrolled receiver account configuration absent'
[ "${TARGET#*@}" = "$ADDRESS" ] || refuse 'receiver target differs from the declared estate host'
# Only the existing broker-owned runner Secret; a recovery transport is never
# read. kubectl uses the deployment's already authorized mounted identity.
kubectl -n boss get secret ops-runner-credential -o json > "$WORK/secret" 2>/dev/null || fail
jq_doc_file "$WORK/secret" || fail
jq -er '.data["boss-gcp.next"]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/value" || fail
jq -er '.data["boss-gcp.next.minted-for"]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/job" || fail
JOB="$(cat "$WORK/job")"
[[ $JOB =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || fail
jq_doc_file "$WORK/secret" || fail
jq -er --arg key "boss-gcp.recovery.$JOB.witness" '.data[$key]' "$WORK/secret" 2>/dev/null | base64 -d > "$WORK/witness" || fail
jq_doc_file "$WORK/witness" || fail
jq -en --slurpfile witness "$WORK/witness" --slurpfile secret "$WORK/secret" \
  --rawfile value "$WORK/value" --arg job "$JOB" '
  $witness[0] as $w | select($w.version == 1 and $w.host == "boss-gcp"
    and $w.credential_id == "ops-runner-credential-boss-gcp"
    and $w.job_id == $job and $w.uid == $secret[0].metadata.uid and $w.promoted == false)
  | {version:1,host:$w.host,credential_id:$w.credential_id,job_id:$w.job_id,
      attempt:$w.attempt,secret_uid:$w.uid,value:$value}
' > "$WORK/envelope" 2>/dev/null || fail
[ "$(stat -c %s "$WORK/envelope")" -le 8192 ] || fail
# The receiver forced command ignores the client command. No accept-new, agent,
# proxy, password fallback or dynamically discovered host key is permitted.
ssh -F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none \
    -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN" \
    -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=10 \
    -i "$WORK/key" "$TARGET" < "$WORK/envelope" > "$WORK/receipt" 2> "$WORK/ssh-error" || fail
jq_doc_file "$WORK/receipt" || fail
jq -e --slurpfile sent "$WORK/envelope" '
  $sent[0] as $s | .version == 1 and .recorded == true and .host == $s.host
  and .credential_id == $s.credential_id and .job_id == $s.job_id
  and .attempt == $s.attempt and .secret_uid == $s.secret_uid
  and (.original_delivery_receipt as $r | $r.version == 1
    and $r.credential_id == $s.credential_id and $r.phase == "verified"
    and $r.actor == "automation:ops-runner" and $r.observation_id == ($s.attempt + ":delivered")
    and ($r.event_id | type == "string") and ($r.timestamp | type == "string")
    and ($r.evidence_json | fromjson | .purpose == "authenticated-runner-delivery"
      and .job_id == $s.job_id and .host == $s.host and .attempt == $s.attempt))
' "$WORK/receipt" >/dev/null 2>&1 || fail
# Preserve the receiver's complete original owner record for the caller.
# Credential input remains private; it cannot ride a returned field.
jq -s -e 'length == 1' "$WORK/receipt" >/dev/null 2>&1 || fail
jq_doc_file "$WORK/receipt" || fail
jq -e --rawfile value "$WORK/value" 'tostring | contains($value) | not' "$WORK/receipt" >/dev/null 2>&1 || fail
cat "$WORK/receipt"
