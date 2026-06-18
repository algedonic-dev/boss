#!/usr/bin/env bash
# Bootstrap the `boss_scratch` database used by the Simulator UI
# track for isolated small-scale runs. Thin wrapper over
# bootstrap-db.sh — see that script for the actual init flow.
#
# Usage:
#   sudo ./infra/postgres/bootstrap-scratch.sh        # drops + recreates
#   sudo ./infra/postgres/bootstrap-scratch.sh --init # no-op if exists
#
# See docs/design/simulator-ui.md for the scratch-DB isolation
# decision.

set -euo pipefail

exec "$(dirname "$0")/bootstrap-db.sh" boss_scratch "$@"
