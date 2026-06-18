#!/usr/bin/env bash
# Bootstrap the primary `boss` database. Drops + recreates +
# applies the per-module schema + grants + seeds the JobKind registry. Thin
# wrapper over bootstrap-db.sh.
#
# Usage:
#   sudo ./infra/postgres/bootstrap-boss.sh         # drops + recreates
#   sudo ./infra/postgres/bootstrap-boss.sh --init  # no-op if exists
#
# WARNING: this drops every row of `boss`. The DB carries user
# in-flight state (pending design decisions, queued flush jobs,
# inbox reads) — don't run it on a live operator's DB without
# coordination.

set -euo pipefail

exec "$(dirname "$0")/bootstrap-db.sh" boss "$@"
