#!/usr/bin/env bash
# caa2acc9 D2: a new core field must not break integration fixtures.
# Production row mappers remain exhaustive; no count or waiver is allowed.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 infra/lint/lib/extensible-fixtures.py --self-test
exec python3 infra/lint/lib/extensible-fixtures.py
