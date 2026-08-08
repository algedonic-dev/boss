#!/usr/bin/env bash
# Canonical release build for deploy. Run this before infra/deploy-services.sh.
#
# WHY THIS EXISTS
# ---------------
# Most service crates are `default = []` so their fast in-memory adapter is
# the default for tests. Two traps follow, and they bite every deploy:
#
#   1. `cargo build --release --workspace` silently produces *in-memory*
#      binaries for those crates (the postgres adapter is never compiled in).
#      Such a binary exit-1s on startup — or worse, serves an in-memory store
#      that loses every write — and nothing warns you at build time.
#
#   2. Each service binary gates its bin target behind its own
#      `required-features`: usually `postgres`, but some carry an umbrella
#      `<service>-api` feature (e.g. accounts-api = ["postgres", ...],
#      events-api = ["bin"]). `cargo build -p X --features postgres` *errors
#      or silently no-ops* when the real gate is `accounts-api`.
#
# Hand-maintaining a list of "which crates need --features what" drifts
# instantly (deploy-services.sh's NEEDS_POSTGRES_FEATURE listed 7 of 17).
# So this script asks cargo: for every bin target that declares
# `required-features`, build it with exactly those. No list to maintain.
#
# Multi-package `--features` is also unreliable (it skips some packages), so
# each gated bin is built in its own invocation.
#
# Usage:
#   ./infra/build-release.sh            # build everything for deploy
#   ./infra/build-release.sh --verify   # also assert each gated bin links sqlx
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

CARGO_BIN="${CARGO_BIN:-$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin}"
export PATH="$CARGO_BIN:$PATH"
RELEASE_DIR="$REPO_ROOT/target/release"

echo "==> [1/2] cargo build --release --workspace (libs + default-feature bins)"
cargo build --release --workspace

echo "==> [2/2] building every bin that declares required-features, with exactly those"
# cargo metadata is the single source of truth — read each workspace bin's
# required-features straight from it, so this never drifts from the Cargo.tomls.
mapfile -t GATED < <(
  cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
md = json.load(sys.stdin)
for p in md["packages"]:
    for t in p["targets"]:
        if "bin" in t["kind"] and t.get("required-features"):
            print("{}\t{}\t{}".format(p["name"], t["name"], ",".join(t["required-features"])))
'
)

for row in "${GATED[@]}"; do
    IFS=$'\t' read -r crate bin feats <<<"$row"
    echo "    $bin  (-p $crate --features $feats)"
    cargo build --release -p "$crate" --bin "$bin" --features "$feats"
done

if [[ "${1:-}" == "--verify" ]]; then
    echo "==> verifying gated bins are postgres-backed (link sqlx)"
    rc=0
    for row in "${GATED[@]}"; do
        IFS=$'\t' read -r _ bin _ <<<"$row"
        [[ -f "$RELEASE_DIR/$bin" ]] || continue
        if ! strings "$RELEASE_DIR/$bin" 2>/dev/null | grep -q "sqlx"; then
            echo "    WARN: $bin does not link sqlx — may be an in-memory build" >&2
            rc=1
        fi
    done
    [[ "$rc" == 0 ]] && echo "    all gated bins link sqlx"
fi

# NO mtime stamping. This used to `touch` every binary here, to stop the
# freshness guard false-flagging a bin cargo had skipped as "older than the
# newest source". The premise — after a clean run every binary is current,
# because cargo either rebuilt it or confirmed it — holds only while the tree
# does not move DURING the run.
#
# On 2026-08-07 it moved. A source edit landed mid-build, after phase 1 had
# already compiled that crate; the touch then stamped the resulting binary
# "current" and the guard passed it. The change (a sim granularity fix) was
# silently absent on a running box while the sim fell behind its own clock,
# and only `sha256` against a fresh build found it.
#
# The touch turned a conservative wrong answer into a confident wrong one, and
# those cost differently: a false STALE costs a rebuild, a false "fresh" costs
# an incident. So a skipped bin may now read STALE. That is the safe
# direction, and `check-binary-freshness.sh --rebuild` settles it for real —
# cargo is the only thing that actually knows.

# Stamp what this build was made from. `set -euo pipefail` is on, so
# reaching this line means every cargo invocation above succeeded — the
# stamp cannot outlive a failed build.
#
# The deploy compares this against the working tree instead of
# comparing mtimes, because git rewrites mtimes on every checkout and
# rebase; see infra/src-fingerprint.sh for the incident.
#
# The stamp failing IS a build failure: unstamped (or stale-stamped)
# binaries cannot be verified against the tree, and the deploy
# pre-flight will refuse them later with a far less useful message.
# This line carried `|| true` and lived up to it on 2026-08-08 — a
# root-owned stale stamp survived a sudo-era build, this script said
# "complete", and the mismatch surfaced only at deploy time.
if ! "$(dirname "$0")/src-fingerprint.sh" > "$RELEASE_DIR/.boss-src-fingerprint"; then
    echo "FATAL: could not stamp $RELEASE_DIR/.boss-src-fingerprint — the binaries" >&2
    echo "       are fine but unverifiable, so this build cannot be deployed." >&2
    echo "       (Root-owned stamp from an old sudo build? fix with:" >&2
    echo "        sudo chown $(id -un) $RELEASE_DIR/.boss-src-fingerprint )" >&2
    exit 1
fi

echo "==> release build complete. Next: sudo infra/deploy-services.sh prod"

# The build is a step of a regen when one is open, and a no-op otherwise.
# A bookkeeping failure must not fail the build, but it must be SEEN —
# the bare `|| true` here hid a month of every call matching nothing.
"$(dirname "$0")/boss-step.sh" regenerate-deployment build \
    "source_ref=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)" \
    || echo "WARN: build step NOT recorded on the regen Job (boss-step failed above)" >&2
