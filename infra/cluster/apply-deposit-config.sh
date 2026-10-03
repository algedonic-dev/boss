#!/usr/bin/env bash
# Called only by the existing converge. K is its existing stdin-enabled
# operator door. Both writes are bounded to the named nonsecret objects.
# Tests are sent explicitly in JSON Patch: ordinary apply does not promise
# to include resourceVersion in the PATCH when the value has not changed.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../lib/jq.sh"
[ "$#" -eq 5 ] || exit 2
k="$1"; namespace="$2"; manifest="$3"; source="$4"; receipt="$5"
tmp="$(mktemp -d)" || exit 2
trap 'rm -rf "$tmp"' EXIT
phase() {
    jq --arg phase "$1" '.phase = $phase' "$receipt" > "$tmp/progress.json"
    mv "$tmp/progress.json" "$receipt"
}
refuse() {
    phase "$1"
    echo "deposit-config: REFUSED — $1; ordinary apply stopped, retained configuration records the resumable phase" >&2
    exit 2
}
jq_doc_file "$source" && jq_doc_file "$receipt" || refuse unread-typed-input
jq -ne --arg namespace "$namespace" --slurpfile receipt "$receipt" --slurpfile source "$source" \
    -f "$HERE/deposit-config-patch.jq" > "$tmp/patches.json" 2> "$tmp/error" || refuse invalid-typed-input
jq '.config_map' "$tmp/patches.json" > "$tmp/cm-patch.json"
jq '.cronjob' "$tmp/patches.json" > "$tmp/cron-patch.json"
if [ "$(jq -r '.mode' "$receipt")" = legacy-migration ]; then
    $k patch configmap break-glass-deposit-known-hosts -n "$namespace" --type=json --patch-file=/dev/stdin -o json \
        < "$tmp/cm-patch.json" > "$tmp/cm-after.json" || refuse config-map-patch-unconfirmed
    jq_doc_file "$tmp/cm-after.json" && jq -se --arg kind ConfigMap --slurpfile receipt "$receipt" --slurpfile source "$source" \
      -f "$HERE/deposit-config-confirm.jq" "$tmp/cm-after.json" > /dev/null || refuse config-map-receipt-unconfirmed
    response_sha="$(sha256sum "$tmp/cm-after.json")"
    jq --arg response_sha "${response_sha%% *}" --slurpfile cm "$tmp/cm-after.json" \
        '. + {phase:"config-map-migrated",config_map_after:{uid:$cm[0].metadata.uid,
          resource_version:$cm[0].metadata.resourceVersion,sha256:$response_sha}}' \
        "$receipt" > "$tmp/progress.json"
    mv "$tmp/progress.json" "$receipt"
else
    # Stable config, or an interrupted migration with exact literal/data
    # equality already proved by the read validator. Never overwrite it.
    phase config-map-retained
fi
$k patch cronjob boss-break-glass-deposit -n "$namespace" --type=json --patch-file=/dev/stdin -o json \
    < "$tmp/cron-patch.json" > "$tmp/cron-after.json" || refuse cronjob-patch-unconfirmed
jq_doc_file "$tmp/cron-after.json" && jq -se --arg kind CronJob --slurpfile receipt "$receipt" --slurpfile source "$source" \
  -f "$HERE/deposit-config-confirm.jq" "$tmp/cron-after.json" > /dev/null || refuse cronjob-receipt-unconfirmed
# Both now have explicit owners: deployment data is retained, source
# CronJob code is conditionally applied. Ordinary apply must touch neither.
awk '
  function emit() {
    if (doc ~ /(^|\n)kind: ConfigMap\n/) cm++
    else if (doc ~ /(^|\n)kind: CronJob\n/) cron++
    else { if (printed++) print "---"; printf "%s", doc }
    doc = ""
  }
  /^---$/ { emit(); next }
  { doc = doc $0 "\n" }
  END { emit(); if (cm != 1 || cron != 1) exit 2 }
' "$manifest" > "$tmp/ordinary.yaml" || refuse invalid-ordinary-apply-set
source_sha="$(sha256sum "$source")"; patch_sha="$(sha256sum "$tmp/patches.json")"
response_sha="$(sha256sum "$tmp/cron-after.json")"
jq --arg source_sha "${source_sha%% *}" --arg patch_sha "${patch_sha%% *}" --arg response_sha "${response_sha%% *}" \
    --slurpfile cron "$tmp/cron-after.json" \
    '. + {phase:"applied-conditionally",typed_source_sha256:$source_sha,patches_sha256:$patch_sha,
      cronjob_after:{uid:$cron[0].metadata.uid,resource_version:$cron[0].metadata.resourceVersion,sha256:$response_sha}}' \
    "$receipt" > "$tmp/final.json"
mv "$tmp/final.json" "$receipt"
mv "$tmp/ordinary.yaml" "$manifest"
