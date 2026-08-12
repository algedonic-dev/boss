#!/usr/bin/env bash
# conductor.sh — entry point for the pr-train conductor. The logic
# lives in `boss train` (crates/orchestrators/boss-cli/src/train.rs)
# since directive 26d61c97 retired python from the BOSS system; this
# shim survives so the systemd units, a terminal, and a dry run all
# invoke it the same way — with the flags they always passed.
#
#   ./infra/train/conductor.sh                  # reconcile + board
#   ./infra/train/conductor.sh --reconcile-only # advance open trains only
#   ./infra/train/conductor.sh --dry-run        # say what would happen
set -euo pipefail

# build-release.sh resolves cargo from CARGO_BIN or $HOME — under the
# timer HOME is the service user's, which is where the toolchain lives.

# Translate the historical flags to `boss train` verbs. --preflight
# wins over --reconcile-only, same as the python argv scan (preflight
# returned before reconcile was consulted); everything else —
# --dry-run today — passes through.
sub="run"
args=()
for a in "$@"; do
    case "$a" in
        --preflight)      sub="preflight" ;;
        --reconcile-only) [ "$sub" = preflight ] || sub="reconcile" ;;
        *)                args+=("$a") ;;
    esac
done
exec /usr/local/bin/boss train "$sub" ${args[@]+"${args[@]}"}
