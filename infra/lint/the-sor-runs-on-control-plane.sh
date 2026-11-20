#!/usr/bin/env bash
# The SoR stack stays off build nodes; PostgreSQL uses the data node.
#
# 2026-09-06 (backlog 90932d98): the boss app Deployment carried no
# nodeSelector, so the scheduler put it on worker w-1. When w-1 shed
# pods, the SoR was evicted to another node and :7900 went dark for
# minutes on a Multi-Attach stall of its RWO PVC. Its data StatefulSets
# were already pinned to control-plane; the app was the gap. This pins
# the invariant so an unpinned boss workload cannot creep back in — a
# config-channel check the software gate would otherwise miss.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
f="infra/cluster/manifests/boss.yaml"
[ -f "$f" ] || { echo "sor-control-plane: $f is missing"; exit 1; }

# The old global grep count could count a comment or another workload's
# selector. Read each actual pod spec, including PostgreSQL's approved
# required database affinity (3c475a19 car 4), and keep a named refusal.
command -v bun >/dev/null 2>&1 || { echo "sor-control-plane: bun is missing; placement was not read" >&2; exit 1; }
if ! workloads=$(bun infra/lint/lib/sor-placement.ts "$f"); then
    exit 1
fi
lint_scanned the-sor-runs-on-control-plane "$workloads" "workload(s) in $f"
echo "sor-control-plane: OK — all $workloads boss workload(s) have required placement off build nodes"
