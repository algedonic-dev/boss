#!/usr/bin/env bash
# apply-script-configmap.sh — make the gate the tree says it is.
#
# WHY THIS EXISTS (2026-08-26). The gate Job cannot run run.sh out of
# the clone, because run.sh IS what performs the clone — so the script
# has to reach the pod some other way, and that way is ConfigMap
# `gate-runner-script`. gate-runner.yaml carried the regeneration
# command in a COMMENT and asked whoever applied it to remember.
#
# Nobody remembered, and nothing noticed. On 2026-08-26 the live
# ConfigMap held the run.sh from `perf/the-gate-uses-the-cpu-it-was-
# given` — a branch that was measured 16% slower and deliberately never
# parked (db127cb4) — while main carried a different script entirely.
# Ten gates ran that day and every receipt they issued was produced by
# a script that is not in the repository. The verdicts were sound (the
# manifest pins CARGO_BUILD_JOBS, so the rejected width never
# activated), but a receipt's claim is "the gate defined in this
# repository passed on this tree", and that first half was not true.
#
# CLAUDE.md 9a: a comment asking the next person to keep two copies in
# sync is not a mechanism. This is the mechanism. Run it whenever
# run.sh changes, and before a gate you intend to trust.
#
#   ./infra/gate-runner/apply-script-configmap.sh [namespace]
#
# --check exits non-zero if the live ConfigMap has drifted from the
# tree, without changing anything. That is the form to reach for when
# you are asking "is the thing that ran the thing I have?".
#
# SINCE 2026-08-30 the forge converge writes this ConfigMap on every
# train (infra/forge/cluster-deploy-runner.sh, 2b69220a), so running
# this by hand is a repair, and --check is its everyday use. SINCE
# e3100bc8 the ConfigMap holds TWO keys — `bootstrap.sh`, which fetches
# and execs the run.sh of the sha being gated, and `run.sh`, main's copy
# it falls back to — and --check compares both. The key set is pinned
# equal to the converge's by gate_runner_manifests.rs.
set -euo pipefail
cd "$(dirname "$0")/../.."

SCRIPT=infra/gate-runner/run.sh
BOOTSTRAP=infra/gate-runner/bootstrap.sh
NAME=gate-runner-script

CHECK=0
if [ "${1:-}" = "--check" ]; then
    CHECK=1
    shift
fi
NAMESPACE="${1:-boss-dev}"

for f in "$SCRIPT" "$BOOTSTRAP"; do
    [ -f "$f" ] || { echo "apply-script-configmap: $f not found" >&2; exit 1; }
done

rendered=$(kubectl -n "$NAMESPACE" create configmap "$NAME" \
    --from-file="run.sh=$SCRIPT" \
    --from-file="bootstrap.sh=$BOOTSTRAP" \
    --dry-run=client -o yaml)

if [ "$CHECK" = "1" ]; then
    # Compare each SCRIPT BODY, not the rendered YAML: the rendered form
    # carries metadata (creationTimestamp, resourceVersion) that differs
    # every time and would report drift on an identical script.
    drift=0
    for pair in "run.sh=$SCRIPT" "bootstrap.sh=$BOOTSTRAP"; do
        key=${pair%%=*} file=${pair#*=}
        live=$(kubectl -n "$NAMESPACE" get configmap "$NAME" -o jsonpath="{.data.${key//./\\.}}" 2>/dev/null || true)
        if [ -z "$live" ]; then
            echo "apply-script-configmap: ConfigMap $NAME in $NAMESPACE has no key $key." >&2
            echo "    A gate whose command names that key would fail to start rather than" >&2
            echo "    run the wrong script, which is the safe direction — but it cannot gate." >&2
            drift=1
            continue
        fi
        if [ "$live" = "$(cat "$file")" ]; then
            echo "apply-script-configmap: $NAME key $key matches $file"
            continue
        fi
        echo "apply-script-configmap: DRIFT — $NAME key $key does not match $file." >&2
        echo "    The gate that issues receipts is not the gate in this tree." >&2
        echo "    Diff (live vs tree):" >&2
        diff <(printf '%s' "$live") "$file" >&2 || true
        drift=1
    done
    if [ "$drift" = "1" ]; then
        echo "    Fix with: $0 $NAMESPACE (the converge also rewrites it on every train)" >&2
        exit 1
    fi
    exit 0
fi

printf '%s\n' "$rendered" | kubectl apply -f -
echo "apply-script-configmap: $NAME in $NAMESPACE now matches $SCRIPT and $BOOTSTRAP"
