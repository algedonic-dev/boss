#!/usr/bin/env bash
# Both instances must refuse failed PostgreSQL readiness and recover only to
# a named, identity-fenced last-converged spec, without needing the jobs API.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 -B "$root/infra/forge/tests/postgres-rollout-test.py"
