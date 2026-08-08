#!/usr/bin/env bash
# conductor.sh — entry point for the pr-train conductor (see
# conductor.py for the actual logic; this exists so the systemd unit,
# a terminal, and a dry run all invoke it the same way).
#
#   ./infra/train/conductor.sh                  # reconcile + board
#   ./infra/train/conductor.sh --reconcile-only # advance open trains only
#   ./infra/train/conductor.sh --dry-run        # say what would happen
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"

# build-release.sh resolves cargo from CARGO_BIN or $HOME — under the
# timer HOME is the service user's, which is where the toolchain lives.
exec python3 "$DIR/conductor.py" "$@"
