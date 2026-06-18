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

# Stamp every built bin as current. After a clean build-release run every
# binary IS up to date (cargo either rebuilt it or confirmed it current), but
# cargo leaves a skipped bin's mtime untouched — which the deploy freshness
# guard would then false-flag as "older than the newest source". Touch them so
# the mtime tells the truth: built/verified now.
find "$RELEASE_DIR" -maxdepth 1 -type f -executable -name 'boss-*' -exec touch {} +

echo "==> release build complete. Next: sudo infra/deploy-services.sh prod"
