#!/usr/bin/env bash
# General-purpose circuit breaker for cargo invocations.
#
# Why: this VM has 7.8 GiB RAM, no swap. A runaway test, proof, or
# build (e.g. Kani's CBMC, or a proptest with an unbounded generator)
# can push the kernel OOM-killer and reboot the whole host — we
# crashed it twice during the formal-methods work. This wrapper caps
# each child process's virtual memory + CPU time, forces a wall-
# clock timeout, and drops to lowest scheduler priority so an
# interactive SSH session stays alive even if the cargo command is
# thrashing.
#
# Usage:
#
#   ./infra/safe-cargo.sh cargo test -p boss-core --lib
#   ./infra/safe-cargo.sh cargo kani -p boss-ledger --lib
#   ./infra/safe-cargo.sh cargo build --release -p boss-gateway
#
# For the Kani-specific wrapper with higher wall-clock allowance,
# use infra/safe-cargo-kani.sh instead — this one is tuned for
# interactive `cargo test` / `cargo build` loops.
#
# Knobs (env-var overridable):
#   SAFE_MEM_LIMIT_KB   — virtual-memory cap per child (default 3_000_000 = ~3 GiB)
#   SAFE_CPU_LIMIT_SEC  — CPU-seconds cap per child (default 300 = 5 min)
#   SAFE_WALL_LIMIT_SEC — wall-clock seconds for the whole invocation (default 600)
#   SAFE_MIN_FREE_MB    — refuse to start if less RAM is free (default 1536)
#   SAFE_MIN_FREE_GB    — refuse to start if less disk is free on / (default 2)

set -euo pipefail

if [[ $# -eq 0 ]]; then
    cat >&2 <<'USAGE'
safe-cargo.sh: wrapper with ulimit + timeout + nice.

Usage: ./infra/safe-cargo.sh <command> [args...]

Example:
  ./infra/safe-cargo.sh cargo test -p boss-core --lib
  ./infra/safe-cargo.sh cargo build --release -p boss-gateway
USAGE
    exit 2
fi

SAFE_MEM_LIMIT_KB=${SAFE_MEM_LIMIT_KB:-3000000}
SAFE_CPU_LIMIT_SEC=${SAFE_CPU_LIMIT_SEC:-300}
SAFE_WALL_LIMIT_SEC=${SAFE_WALL_LIMIT_SEC:-600}
SAFE_MIN_FREE_MB=${SAFE_MIN_FREE_MB:-1536}
SAFE_MIN_FREE_GB=${SAFE_MIN_FREE_GB:-2}

# ---- Pre-flight resource checks ----
free_mb=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
if (( free_mb < SAFE_MIN_FREE_MB )); then
    echo "abort: only ${free_mb} MiB RAM available (need ≥${SAFE_MIN_FREE_MB} MiB)." >&2
    echo "       stop other heavy processes and retry." >&2
    exit 3
fi

free_gb=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
if (( free_gb < SAFE_MIN_FREE_GB )); then
    echo "abort: only ${free_gb} GiB disk free on / (need ≥${SAFE_MIN_FREE_GB} GiB)." >&2
    echo "       clear /var/log or /data/builds/* and retry." >&2
    exit 4
fi

echo "safe-cargo: ${free_mb} MiB RAM / ${free_gb} GiB disk free — ok to start."
echo "safe-cargo: caps — mem ≤ $(( SAFE_MEM_LIMIT_KB / 1024 )) MiB, cpu ≤ ${SAFE_CPU_LIMIT_SEC}s, wall ≤ ${SAFE_WALL_LIMIT_SEC}s."

# ---- Resource caps for all children ----
ulimit -v "${SAFE_MEM_LIMIT_KB}"
ulimit -t "${SAFE_CPU_LIMIT_SEC}"

# ---- Wall-clock cap + priority ----
exec nice -n 19 ionice -c 3 timeout --kill-after=30s "${SAFE_WALL_LIMIT_SEC}s" "$@"
