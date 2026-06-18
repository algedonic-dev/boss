#!/usr/bin/env bash
# Safe wrapper for `cargo kani` — caps memory, CPU time, and wall-time
# so a runaway proof can't OOM-kill the VM.
#
# Why: CBMC (Kani's SMT backend) is memory-hungry. Unbounded proofs
# or unexpectedly-deep symbolic state can grow to many GB. This VM
# has 7.8GiB RAM and no swap — a CBMC process that balloons past
# ~6GiB has reliably produced kernel OOM-kills that take the whole
# host down. We freed disk + added proofs for the balance invariant
# per docs/formal-methods (formal-methods track step 5, 2026-04-24).
#
# Usage: from repo root, prefix any cargo-kani invocation with this:
#
#   ./infra/safe-cargo-kani.sh cargo kani -p boss-ledger --lib
#
# Circuit breakers applied (in order of how they fire):
#   1. Pre-flight — refuse to start if free RAM < 2GiB or free disk < 2GiB.
#   2. ulimit -v  — 4_000_000 KB address-space cap per child process.
#                   CBMC allocating past this gets a malloc failure
#                   (process dies cleanly) rather than growing until
#                   the kernel OOM-killer fires.
#   3. ulimit -t  — 600s CPU-time cap per child. A process pinning a
#                   core for 10 wall-clock minutes gets SIGKILL from
#                   the kernel.
#   4. timeout    — 900s wall-clock cap on the whole invocation; Kani
#                   + CBMC often fan out across children so ulimit -t
#                   alone isn't enough.
#   5. nice + ionice — lowest priority so interactive SSH / builds
#                   keep responding even if CBMC is thrashing.

set -euo pipefail

if [[ $# -eq 0 ]]; then
    cat >&2 <<'USAGE'
safe-cargo-kani.sh: wrapper with ulimit + timeout + nice.

Usage: ./infra/safe-cargo-kani.sh <command> [args...]

Example:
  ./infra/safe-cargo-kani.sh cargo kani -p boss-ledger --lib
USAGE
    exit 2
fi

# ---- Pre-flight resource checks ----
free_mb=$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)
if (( free_mb < 2048 )); then
    echo "abort: only ${free_mb} MiB RAM available (need ≥2048 MiB)." >&2
    echo "       stop other heavy processes and retry." >&2
    exit 3
fi

free_gb=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
if (( free_gb < 2 )); then
    echo "abort: only ${free_gb} GiB disk free on / (need ≥2 GiB)." >&2
    echo "       clear /var/log or /data/builds/* and retry." >&2
    exit 4
fi

echo "safe-cargo-kani: ${free_mb} MiB RAM / ${free_gb} GiB disk free — ok to start."

# ---- Resource caps for all children ----
# Address-space cap (bash ulimit -v is in KB). 4 GiB = 4_000_000 KB.
ulimit -v 4000000
# CPU-time cap per process, in seconds.
ulimit -t 600

# ---- Wall-clock cap + priority ----
# --kill-after gives timeout 30s for SIGTERM cleanup before escalating
# to SIGKILL. 900s chosen empirically: the four boss-ledger proofs
# finish in under 5 min on this VM; 15 min is ample headroom.
exec nice -n 19 ionice -c 3 timeout --kill-after=30s 900s "$@"
