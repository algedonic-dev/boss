#!/usr/bin/env bash
# Derive the nonsecret runtime ConfigMap from one validated snapshot.
# Converge calls this before its apply port, so an unread declaration
# cannot replace a working deployment with an invented empty one.
set -euo pipefail
# THE OBJECT THIS RENDERS, named ONCE, as `Kind/name` (backlog cb0c9937).
# No manifest declares it — it is generated here, per instance namespace —
# so the orphan derivation (infra/cluster/undeclared-objects.sh
# derived_exemptions) has to account for it, and it READS this line as
# text rather than carrying the name a second time: from train #916
# (2026-10-03) until this line existed the name lived only in the jq
# below, the derivation knew nothing of it, and every rolling converge
# closed FAILED at `check orphans` on the object this script had just
# applied. Keep it one plain assignment at column 0: the derivation
# refuses (CANNOT ANSWER) a renderer whose line it cannot read, because
# an exemption it failed to derive is an object `delete-orphan-object`
# could be asked to delete.
RENDERS="ConfigMap/boss-instance-config"
if [ "$#" -ne 3 ]; then echo 'render-dev-door-config: expected source, stage and namespace' >&2; exit 2; fi
source_file="$1"
stage_dir="$2"
namespace="$3"
if [[ ! "$namespace" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]]; then
    echo 'render-dev-door-config: invalid namespace' >&2; exit 2
fi
bash "$(dirname "$0")/stage-dev-door.sh" "$source_file" "$stage_dir"
jq -n --arg kind "${RENDERS%%/*}" --arg name "${RENDERS#*/}" --arg namespace "$namespace" \
    --rawfile declaration "$stage_dir/dev-door.json" \
    '{apiVersion:"v1",kind:$kind,metadata:{name:$name,namespace:$namespace},data:{"dev-door.json":$declaration}}' \
    > "$stage_dir/configmap.json"
