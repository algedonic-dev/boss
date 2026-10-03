#!/usr/bin/env bash
# A dated statfs of an already mounted claim, recorded on its native
# maintenance step without closing it. Start evidence survives later
# backup failures; only a final sample is reusable by the idle reader.
# No Kubernetes API, device write, extra grant or dump-size inference.
set -euo pipefail
print=0
if [[ ${1:-} == --print ]]; then print=1; shift; fi
[[ $# == 5 ]] || { echo 'usage: boss-filesystem-sample.sh [--print] workflow step claim mount start|final' >&2; exit 2; }
workflow=$1 step=$2 claim=$3 mount=$4 position=$5
case "$position" in start|final) ;; *) exit 2 ;; esac
for name in "$workflow" "$step" "$claim" "${BOSS_FS_NAMESPACE:-}"; do
    [[ "$name" =~ ^[a-z0-9]([-a-z0-9]*[a-z0-9])?$ ]] || { echo 'filesystem-sample: missing or malformed identity' >&2; exit 2; }
done
[[ "$mount" =~ ^/[a-zA-Z0-9/_-]+$ ]] || exit 2
observed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
unread='' volume='' source=''
for name in "${BOSS_FS_POD_UID:-}" "${BOSS_FS_POD:-}" "${BOSS_FS_NODE:-}" "${BOSS_FS_JOB_UID:-}"; do
    [[ -n "$name" ]] || unread='the Downward API did not name this pod, node and controller Job'
done
# /proc/self/mountinfo proves an exact mount, rather than df of a
# directory that silently belongs to the container's root filesystem.
sources=$(awk -v mount="$mount" '$5 == mount { for(i=7;i<=NF;i++) if($i=="-") {print $(i+2); break} }' \
    "${BOSS_FS_MOUNTINFO:-/proc/self/mountinfo}" 2>/dev/null) || unread='the mount table could not be read'
if [[ -z "$unread" ]]; then
    if [[ "$sources" =~ ^/dev/longhorn/(pvc-[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12})$ ]]; then
        source=$sources volume=${BASH_REMATCH[1]}
    else
        unread='the exact mount does not name one Longhorn backing volume'
    fi
fi
capacity=null used=null free=null
if [[ -z "$unread" ]]; then
    if ! figures=$(timeout -k 2 10 stat -f -c '%S %b %f %a' -- "$mount" 2>/dev/null); then
        unread='statfs of the mounted claim failed'
    elif ! [[ "$figures" =~ ^([0-9]+)[[:space:]]([0-9]+)[[:space:]]([0-9]+)[[:space:]]([0-9]+)$ ]]; then
        unread='statfs did not return four integer block figures'
    else
        # Bound the product before shell arithmetic and before jq's
        # IEEE number representation: exactly representable bytes only.
        block=${BASH_REMATCH[1]} total=${BASH_REMATCH[2]} unused=${BASH_REMATCH[3]} available=${BASH_REMATCH[4]}
        if [[ ${#block} -gt 10 || ${#total} -gt 16 || ${#unused} -gt 16 || ${#available} -gt 16 ]] \
            || ! [[ "$block" =~ ^[1-9][0-9]*$ && "$total" =~ ^[1-9][0-9]*$ ]] \
            || (( 10#$block > 1073741824 || 10#$total > 9007199254740991 / 10#$block \
                 || 10#$unused > 10#$total || 10#$available > 10#$unused )); then
            unread='statfs block figures are zero, inconsistent or beyond exact byte arithmetic'
        else
            capacity=$((10#$block * 10#$total))
            used=$((10#$block * (10#$total - 10#$unused)))
            free=$((10#$block * 10#$available))
        fi
    fi
fi
summary=$(jq -nc --arg ns "$BOSS_FS_NAMESPACE" --arg claim "$claim" --arg volume "$volume" --arg source "$source" \
    --arg mount "$mount" --arg at "$observed_at" --arg position "$position" --arg unread "$unread" \
    --arg pod_uid "${BOSS_FS_POD_UID:-}" --arg pod "${BOSS_FS_POD:-}" --arg node "${BOSS_FS_NODE:-}" \
    --arg job_uid "${BOSS_FS_JOB_UID:-}" --argjson capacity "$capacity" --argjson used "$used" --argjson free "$free" '
    {filesystem_sample: ({schema: 1, namespace: $ns, claim: $claim, volume: $volume,
        mount: $mount, mount_source: $source, observed_at: $at, position: $position,
        pod_uid: $pod_uid, pod: $pod, node: $node, kubernetes_job_uid: $job_uid,
        capacity_bytes: $capacity, used_bytes: $used, free_bytes: $free}
        # A successful retry clears an earlier error in the step door
        # deep summary merge; omitting the key would retain that error.
        + {unread: (if $unread == "" then null else $unread end)})}')
if [[ "$print" == 1 ]]; then printf '%s\n' "$summary"; exit 0; fi
file=$(mktemp "${TMPDIR:-/tmp}/boss-filesystem-sample.XXXXXX")
trap 'rm -f "$file"' EXIT
printf '%s\n' "$summary" > "$file"
step_script=${BOSS_FS_STEP_SCRIPT:-$(dirname "$0")/boss-step.sh}
BOSS_RUN_SUMMARY_FILE="$file" BOSS_STEP_METADATA_ONLY=1 bash "$step_script" "$workflow" "$step"
