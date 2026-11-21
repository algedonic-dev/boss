#!/usr/bin/env bash
# A writeback is a stable packet, not a successful resize or a tree edit.
# Keep a failed filing beside the accepted effect; never undo or repeat
# a capacity write to recover an acknowledgement from the jobs API.
set -eu
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$here/../lib/sor.sh"
. "$here/../lib/secret-header.sh"
input="$1" producer="$2" output="$3"
case "$producer" in converge|expand) ;; *) exit 2 ;; esac
actor="${BOSS_ACTOR:-automation:cluster-deploy-runner}"
api="${JOBS_API:-${BOSS_JOBS_URL:-}}"
header=""
if [ -n "$api" ]; then
    machine_token_header header "$api" || header=""
fi
python3 -B "$here/standalone-pvc.py" "$input" --writebacks "$producer" \
    --api "$api" --actor "$actor" --header "$header" --receipt "$output"
cat "$output"
