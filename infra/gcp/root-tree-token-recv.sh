#!/usr/bin/env bash
# Fixed-purpose receiver for broker-owned root tree tokens (db018094).
# Source preparation only: installer, signed enrollment and sender own
# activation. This receiver never grants a key or records broker delivery.
set +x
set -euo pipefail
export LC_ALL=C
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
. "$INFRA/lib/jq.sh"
DEST="${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}"
refuse() { echo "root-tree-token-recv: refused ($1); installed credential unchanged" >&2; exit 78; }
fail() { echo "root-tree-token-recv: failed ($1); delivery not acknowledged" >&2; exit 1; }
[ "$#" -eq 0 ] || refuse arguments
umask 077
WORK="$(mktemp -d)" || fail staging
TMP=""
cleanup() { rm -rf "$WORK"; [ -z "$TMP" ] || rm -f "$TMP"; }
trap cleanup EXIT
# Sender must observe this greeting before sending any credential byte.
printf '%s\n' 'root-tree-token-recv: boss-gcp: ready for one envelope'
timeout 10s head -c 8193 > "$WORK/input" || refuse unread
[ "$(stat -c %s "$WORK/input")" -le 8192 ] || refuse oversize
jq_doc_file "$WORK/input" || refuse document
jq -s -e 'length == 1' "$WORK/input" >/dev/null 2>&1 || refuse document
jq -n --stream -e 'reduce inputs as $r ([]; if ($r|length)==2 then .+[$r[0]] else . end) | group_by(.) | all(length == 1)' "$WORK/input" >/dev/null 2>&1 || refuse duplicate-key
jq -e '
  def uuid: type == "string" and test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$");
  type == "object" and keys == ["credential_id","host","job_id","secret_uid","value","version"]
  and .version == 1 and .host == "boss-gcp" and .credential_id == "boss-gcp-root-tree-token"
  and (.job_id|uuid) and (.secret_uid|uuid)
  and (.value|type == "string" and length >= 32 and length <= 4096 and test("^[A-Za-z0-9_-]+$"))
' "$WORK/input" >/dev/null 2>&1 || refuse shape
. "$INFRA/lib/host-check.sh"
host_seam BOSS_GCP_ROOT_TREE_TOKEN /etc/boss/root-tree/forge.token
host_check boss-gcp root-tree-token-recv
. "$INFRA/lib/sor.sh"
[ -n "${BOSS_FORGE_URL:-}" ] || refuse forge-unconfigured
DIR="$(dirname "$DEST")"
[ ! -L "$DIR" ] && [ ! -L "$DEST" ] || refuse symlink
[ ! -e "$DEST" ] || [ -f "$DEST" ] || refuse destination-type
mkdir -p -m 0700 "$DIR" || fail directory
[ "$(stat -c %a "$DIR")" = 700 ] && [ "$(stat -c %u "$DIR")" = "$(id -u)" ] || refuse directory-owner-mode
# A private leaf does not protect a path below a replaceable ancestor.
# Root-owned sticky temporary directories protect their owned entries;
# every other ancestor must be owned by root or this executor and unwritable
# by other actors. Check lexical components too, refusing intermediate links.
CURSOR="$DIR"
while :; do
    [ ! -L "$CURSOR" ] && [ -d "$CURSOR" ] || refuse ancestor-type
    OWNER="$(stat -c %u "$CURSOR")"
    MODE="$(stat -c %a "$CURSOR")"
    [[ "$MODE" =~ ^[0-7]{3,4}$ ]] || refuse ancestor-mode
    [ "$OWNER" = 0 ] || [ "$OWNER" = "$(id -u)" ] || refuse ancestor-owner
    if (( (8#$MODE & 0022) != 0 )); then
        [ "$OWNER" = 0 ] && (( (8#$MODE & 01000) != 0 )) || refuse ancestor-writable
    fi
    [ "$CURSOR" != / ] || break
    CURSOR="$(dirname "$CURSOR")"
done
TMP="$(mktemp "$DIR/.forge.token.XXXXXX")" || fail staging
jq -jr .value "$WORK/input" > "$TMP" 2>/dev/null || refuse value
chmod 0600 "$TMP" || fail staging-mode
# A public git read may succeed without presenting any credential. Require
# the authenticated repository API effect as well, using the broker's own
# read:repository proof route; redirect responses never receive the token.
. "$INFRA/lib/secret-header.sh"
VALUE="$(cat "$TMP")"
secret_header FORGE_HEADER "Authorization: token $VALUE" || fail authentication-header
unset VALUE
CODE="$(curl --noproxy '*' --max-time 30 --max-redirs 0 --proto '=http,https' \
    -sS -H "$FORGE_HEADER" -o "$WORK/repository" -w '%{http_code}' \
    "$BOSS_FORGE_URL/api/v1/repos/david/boss" 2> "$WORK/auth-error")" || fail repository-authentication
[ "$CODE" = 200 ] || fail repository-authentication
jq -s -e 'length == 1 and .[0].full_name == "david/boss"' \
    "$WORK/repository" >/dev/null 2>&1 || fail repository-identity
# Prove the exact staged file through the existing target-restricting helper.
# No token appears in argv, URL, git config or printed git diagnostics.
export BOSS_GCP_ROOT_TREE_TOKEN="$TMP" GIT_TERMINAL_PROMPT=0
unset GIT_ASKPASS SSH_ASKPASS
if ! timeout 30s git -c credential.helper= \
    -c "credential.helper=!bash '$HERE/root-tree-credential.sh'" \
    -c credential.useHttpPath=true ls-remote "$BOSS_FORGE_URL/david/boss.git" refs/heads/main \
    > "$WORK/refs" 2> "$WORK/git-error"; then
    fail repository-proof
fi
grep -Eq '^[0-9a-f]{40}[[:space:]]refs/heads/main$' "$WORK/refs" || fail repository-main
sync -f "$TMP" || fail file-sync
mv -fT "$TMP" "$DEST" || fail installation
TMP=""
sync -f "$DIR" || fail directory-sync
# Sender compares this value-free receipt to its bound Secret and re-reads
# that Secret before the shared Forgejo delivered-step door may record it.
DIGEST="$(sha256sum "$DEST")"
DIGEST="${DIGEST%% *}"
LENGTH="$(stat -c %s "$DEST")"
jq -c --arg digest "$DIGEST" --argjson length "$LENGTH" \
    'del(.value) + {installed:true,value_sha256:$digest,value_length:$length}' "$WORK/input"
