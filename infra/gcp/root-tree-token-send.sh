#!/usr/bin/env bash
# Forge-side deposit. Publication, key preparation and signed enrollment
# remain separate recorded acts; this script never mints or enrolls a key.
set +x
set -euo pipefail
export LC_ALL=C
ME=root-tree-token-send
refuse() { echo "$ME: refused ($1); nothing acknowledged" >&2; exit 78; }
fail() { echo "$ME: failed ($1); nothing acknowledged" >&2; exit 1; }
not_yet() { echo "$ME: not yet ($1); nothing sent"; exit 75; }
ENROLLMENT="${BOSS_ROOT_TREE_DEPOSIT_ENROLLMENT:-}"
[[ "$ENROLLMENT" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || refuse enrollment
[ "$#" -eq 0 ] || refuse arguments
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
. "$INFRA/lib/sor.sh"
. "$INFRA/lib/jq.sh"
KEY="${BOSS_ROOT_TREE_DEPOSIT_KEY:-}"
KNOWN="${BOSS_ROOT_TREE_DEPOSIT_KNOWN_HOSTS:-}"
TARGET="${BOSS_ROOT_TREE_DEPOSIT_TARGET:-}"
[ -f "$KEY" ] && [ -s "$KNOWN" ] || refuse enrollment-transport
[[ "$TARGET" =~ ^[a-z_][a-z0-9_-]*@[^@[:space:]]+$ ]] || refuse enrollment-target
ADDRESS="$(awk '/^\[\[node\]\]$/ {selected=0} /^id = "boss-gcp"$/ {selected=1} selected && /^address = / {gsub(/"/, "", $3); print $3}' "$INFRA/estate/estate.toml")"
[ -n "$ADDRESS" ] && [ "${TARGET#*@}" = "$ADDRESS" ] || refuse enrollment-host
umask 077
WORK="$(mktemp -d)" || fail staging
trap 'rm -rf "$WORK"' EXIT
{ cat "$KEY"; printf '\n'; } > "$WORK/key" || refuse enrollment-key
chmod 0600 "$WORK/key"
PUBLIC="$(ssh-keygen -y -f "$WORK/key" 2>/dev/null)" || refuse enrollment-key
PUBLIC="$(printf '%s\n' "$PUBLIC" | awk 'NF >= 2 {print $1 " " $2}')"
. "$INFRA/lib/secret-header.sh"
machine_token_header MT_HDR "$BOSS_JOBS_URL" || fail machine-header
HDRS=(-H "x-boss-user: $(sor_reader_header automation:credential-deposit)")
[ -z "$MT_HDR" ] || HDRS+=(-H "$MT_HDR")
API_CURL="$INFRA/boss-api-curl.sh"
"$API_CURL" -fsS "${HDRS[@]}" "$BOSS_JOBS_URL/api/jobs/$ENROLLMENT" > "$WORK/enrollment" 2> "$WORK/api-error" || fail enrollment-read
jq_doc_file "$WORK/enrollment" || refuse enrollment-document
if jq -e '[.steps[]|select(.spec_slug == "enroll" and .status != "completed" and .status != "skipped")]|length == 1' "$WORK/enrollment" >/dev/null 2>&1; then
    not_yet enrollment-not-signed
fi
COMMAND='exec sudo -n /usr/local/libexec/boss/root-tree-delivery/infra/gcp/root-tree-token-recv.sh'
jq -e --arg enrollment "$ENROLLMENT" --arg public "$PUBLIC" --arg prefix "command=\"$COMMAND\",restrict " '
  .id == $enrollment and .kind == "prepare-the-root-tree-deposit-key" and .subject.id == "root-tree-deposit-key"
  and ((.metadata.abandoned // "")|tostring) != "true"
  and ([.steps[]|select(.spec_slug == "install" and .status == "completed")|.metadata.public_key] as $keys
    | ($keys|length) == 1 and ($keys[0]|split(" ")|.[0:2]|join(" ")) == $public
    and ([.steps[]|select(.spec_slug == "enroll" and .status == "completed"
      and .metadata.decision == "approved" and .metadata.receiver_host == "boss-gcp"
      and .metadata.purpose == "root-tree credential deposit"
      and (.metadata.enrollment_record|type == "string" and length > 0)
      and .metadata.authorized_keys_line == ($prefix + $keys[0]))]|length) == 1)
' "$WORK/enrollment" >/dev/null 2>&1 || refuse enrollment-binding
if [ -n "${BOSS_ROOT_TREE_SEND_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_ROOT_TREE_SEND_KUBECTL"
else
    KUBECTL=(kubectl)
fi
CRED=boss-gcp-root-tree-token
read_secret() {
    READ_VALUE="" READ_STATE=unreadable
    if "${KUBECTL[@]}" -n boss get secret "$CRED" -o json > "$WORK/secret" 2> "$WORK/secret-error" \
        && jq_doc_file "$WORK/secret" \
        && jq -er '.data.token' "$WORK/secret" | base64 -d > "$WORK/value" 2>/dev/null; then
        READ_VALUE="$(cat "$WORK/value")"
        READ_STATE=read
    fi
}
read_secret
[ "$READ_STATE" = read ] || fail secret-read
[[ "$READ_VALUE" =~ ^[A-Za-z0-9_-]{32,4096}$ ]] || refuse secret-shape
SECRET_VALUE="$READ_VALUE"
"$API_CURL" -fsS "${HDRS[@]}" "$BOSS_JOBS_URL/api/jobs?kind=rotate-a-credential&status=open&limit=500&full=true" > "$WORK/jobs" 2> "$WORK/api-error" || fail rotation-read
jq_doc_file "$WORK/jobs" || refuse rotation-document
jq -e '.total == (.data|length)' "$WORK/jobs" >/dev/null 2>&1 || refuse rotation-incomplete
jq -c --arg c "$CRED" '[.data[]|select(.subject.id == $c)|select(any(.steps[]; .spec_slug == "delivered" and (.status == "ready" or .status == "active")))]' "$WORK/jobs" > "$WORK/pending"
COUNT="$(jq length "$WORK/pending")"
[ "$COUNT" != 0 ] || { echo "$ME: no rotation awaits delivery; no connection made"; exit 0; }
[ "$COUNT" = 1 ] || refuse rotation-ambiguous
JOB="$(jq -r '.[0].id' "$WORK/pending")"
SECRET_UID="$(jq -r .metadata.uid "$WORK/secret")"
# The envelope value goes straight from its private file into a private
# document, never to argv. Binding fields contain no credential material.
jq -n --arg job "$JOB" --arg uid "$SECRET_UID" --rawfile value "$WORK/value" \
  '{version:1,host:"boss-gcp",credential_id:"boss-gcp-root-tree-token",job_id:$job,secret_uid:$uid,value:$value}' > "$WORK/envelope"
DIGEST="$(sha256sum "$WORK/value")"; DIGEST="${DIGEST%% *}"
LENGTH="$(stat -c %s "$WORK/value")"
# No credential byte is handed to a shell: observe the receiver first.
coproc DEPOSIT { timeout 60s ssh -T -o BatchMode=yes -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=10 -o "UserKnownHostsFile=$KNOWN" -i "$WORK/key" "$TARGET" 2> "$WORK/ssh-error"; }
PID=$DEPOSIT_PID
IN=${DEPOSIT[1]} OUT=${DEPOSIT[0]}
IFS= read -r -t 30 -u "$OUT" GREETING || fail receiver-greeting
[ "$GREETING" = 'root-tree-token-recv: boss-gcp: ready for one envelope' ] || refuse receiver-greeting
cat "$WORK/envelope" >&"$IN" || fail transfer
exec {IN}>&-
cat <&"$OUT" > "$WORK/receipt" || fail receiver-receipt
wait "$PID" || fail receiver
jq -s -e --arg job "$JOB" --arg uid "$SECRET_UID" --arg digest "$DIGEST" --argjson length "$LENGTH" '
  length == 1 and .[0].version == 1 and .[0].host == "boss-gcp"
  and .[0].credential_id == "boss-gcp-root-tree-token" and .[0].job_id == $job
  and .[0].secret_uid == $uid and .[0].installed == true
  and .[0].value_sha256 == $digest and .[0].value_length == $length and (.[0]|has("value")|not)
' "$WORK/receipt" >/dev/null 2>&1 || refuse receipt-binding
# Use the existing Forgejo rotation's shared record door and late Secret
# re-read. Runner-generation acknowledgment is a different protocol.
last8() { printf '%s' "${1: -8}"; }
delivery_proof() { return 0; } # Exact pinned-host receipt checked above.
rc=0
DELIVERY_STATE="not recorded"
BOSS_NODE_ID=boss-gcp
. "$INFRA/lib/credential-delivery.sh"
record_credential_delivery /etc/boss/root-tree/forge.token
echo "$ME: $DELIVERY_STATE"
exit "$rc"
