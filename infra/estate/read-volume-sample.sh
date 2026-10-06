#!/usr/bin/env bash
# The idle claim's durable sample, through existing signed reads. The
# newest failed/no-sample attempt invalidates older successful evidence.
# Job identity survives Pod GC; no cached figure is relabelled current.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/ops-credentials.sh"
. "$here/../lib/secret-header.sh"
[[ $# == 3 ]] || exit 2
namespace=$1 claim=$2 volume=$3
blind() {
    jq -nc --arg reason "$1" '{capacity_bytes:null,used_bytes:null,free_bytes:null,unread:("mounted sample: " + $reason)}'
    exit 0
}
for name in "$namespace" "$claim"; do
    [[ "$name" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$ ]] || blind 'malformed claim identity'
done
sources=${BOSS_VOLUME_SAMPLE_SOURCES:-$here/mounted-volume-sources.json}
if ! source=$(jq -cse --arg ns "$namespace" --arg claim "$claim" '
    if length == 1 then .[0] else error("not one entire source registry") end
    | if type != "array" then error("not a registry array") else . end
    | map(select(.namespace == $ns and .claim == $claim))
    | if length == 1 then .[0] else error("missing or ambiguous producer") end' "$sources" 2>/dev/null); then
    blind 'no unique declared mounted producer'
fi
workflow=$(printf '%s' "$source" | jq -r '.workflow // empty')
[[ "$workflow" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$ ]] || blind 'malformed declared workflow'
[[ -n "${JOBS_API:-}" ]] || blind 'the system of record is not configured'
# One definition: the declared maintenance interval, not another TTL
# copied into this registry or a global nightly assumption.
cadence=${BOSS_VOLUME_SAMPLE_CADENCE:-$here/../dispatcher/rules/cadence-silence-sweep-daily.toml}
minutes=$(sed -n "s/.*\"interval_minutes\.$workflow\" = \"\([0-9]*\)\".*/\1/p" "$cadence" 2>/dev/null)
[[ "$minutes" =~ ^[1-9][0-9]{0,6}$ ]] || blind 'no unique positive declared cadence'
max_age=$((10#$minutes * 60))
days=$(((10#$minutes + 1439) / 1440 + 1))
work=$(mktemp -d) || blind 'cannot prepare a bounded reader workspace'
trap 'rm -rf "$work"' EXIT
machine_token_header MT_HDR "$JOBS_API" || blind 'cannot prepare the existing machine header'
user='{"id":"automation:estate-observer-host","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
if ! curl -fsS --connect-timeout 5 --max-time 15 --max-filesize 4194304 \
    -H "x-boss-user: $user" ${MT_HDR:+-H "$MT_HDR"} \
    "$JOBS_API/api/jobs?kind=$workflow&partition=real&closed_within=$days&limit=500&full=true" > "$work/native.json" 2> "$work/read.err"; then
    blind "the signed native job read failed: $(tr '\n' ' ' < "$work/read.err" | cut -c1-200)"
fi
if ! $(ops_kubectl) get jobs -n "$namespace" -o json --request-timeout=20s > "$work/attempts.json" 2> "$work/kube.err"; then
    blind "the native Kubernetes attempt read failed: $(tr '\n' ' ' < "$work/kube.err" | cut -c1-200)"
fi
now=$(date -u +%s)
if ! jq -nce --slurpfile native "$work/native.json" --slurpfile attempts "$work/attempts.json" \
    --argjson source "$source" --arg namespace "$namespace" --arg claim "$claim" --arg volume "$volume" \
    --argjson max_age "$max_age" --argjson now "$now" -f "$here/volume-sample.jq" > "$work/accepted.json" 2> "$work/accept.err"; then
    blind "$(tr '\n' ' ' < "$work/accept.err" | cut -c1-300)"
fi
cat "$work/accepted.json"
