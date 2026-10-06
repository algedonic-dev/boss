#!/usr/bin/env bash
# The generic deposit template carries no deployment receiver or pin.
# Converge supplies two existing, nonsecret reads. Their strict shape is
# judged before a rendered manifest or positive receipt is published.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/jq.sh"
refuse() { echo 'deposit-config: REFUSED — unread, incomplete or invalid deployment inputs; nothing staged' >&2; exit 2; }
[ "$#" -eq 6 ] || refuse
template="$1"; cm="$2"; cron="$3"; namespace="$4"; output="$5"; receipt="$6"
[[ "$namespace" =~ ^boss(-[a-z0-9]+(-[a-z0-9]+)*)?$ ]] || refuse
[ -r "$template" ] && jq_doc_file "$cm" && jq_doc_file "$cron" || refuse
tmp="$(mktemp -d)" || refuse
trap 'rm -rf "$tmp"' EXIT
jq -ne --arg namespace "$namespace" --slurpfile cm "$cm" --slurpfile cron "$cron" \
    -f "$HERE/deposit-config.jq" > "$tmp/validated.json" 2> "$tmp/error" || refuse
jq -r '.data.known_hosts' "$tmp/validated.json" > "$tmp/known_hosts"
# Parsing the SSH public key rejects well-shaped base64 that is not an
# ed25519 host key. No connection or credential is involved.
ssh-keygen -lf "$tmp/known_hosts" > /dev/null 2>&1 || refuse
data="$(jq -cS '.data' "$tmp/validated.json")"
# JSON is a YAML flow mapping. Pass it as a file, not an awk variable:
# awk would interpret backslash escapes in the pinned known_hosts bytes.
printf 'data: %s\n' "$data" > "$tmp/data"
jq -r '"  uid: " + (.config_map.uid | tojson), "  resourceVersion: " + (.config_map.resource_version | tojson)' "$tmp/validated.json" > "$tmp/cm-identity"
jq -r '"  uid: " + (.cronjob.uid | tojson), "  resourceVersion: " + (.cronjob.resource_version | tojson)' "$tmp/validated.json" > "$tmp/cron-identity"
awk -v data_file="$tmp/data" -v cm_file="$tmp/cm-identity" -v cron_file="$tmp/cron-identity" '
  /^kind: ConfigMap$/ { config = 1 }
  /^kind: CronJob$/ { cron = 1 }
  /^---$/ { config = 0; cron = 0 }
  (config || cron) && /^  namespace: / {
    print
    identity = config ? cm_file : cron_file
    while ((getline line < identity) > 0) print line
    close(identity); identities++; next
  }
  config && /^data: \{\}$/ {
    while ((getline line < data_file) > 0) print line
    close(data_file); count++; next
  }
  { print }
  END { if (count != 1 || identities != 2) exit 2 }
' "$template" > "$tmp/rendered.yaml" || refuse
cm_sha="$(sha256sum "$cm")"; cron_sha="$(sha256sum "$cron")"
render_sha="$(sha256sum "$tmp/rendered.yaml")"
jq --arg cm_sha "${cm_sha%% *}" --arg cron_sha "${cron_sha%% *}" --arg render_sha "${render_sha%% *}" \
    '. + {config_map_sha256:$cm_sha,cronjob_sha256:$cron_sha,rendered_sha256:$render_sha}' \
    "$tmp/validated.json" > "$tmp/receipt.json" || refuse
mv "$tmp/rendered.yaml" "$output"
mv "$tmp/receipt.json" "$receipt"
