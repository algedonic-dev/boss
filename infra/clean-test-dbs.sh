#!/usr/bin/env bash
# Drop leaked per-test Postgres databases.
#
# Integration tests spin up an isolated DB each (the TestDb pattern). They
# drop it on the guard's Drop — but a panicking test, a killed `cargo test`,
# or a disk-full crash skips that cleanup, so they accumulate (we found 5210
# / 64 GB once). Run this before/after a test sweep, or from CI, to reclaim.
set -euo pipefail
PGHOST="${BOSS_PGHOST:-127.0.0.1}"; PGUSER="${BOSS_PGUSER:-boss}"
export PGPASSWORD="${BOSS_PGPASSWORD:-boss}"
n=$(psql -tA -h "$PGHOST" -U "$PGUSER" -d postgres -c \
  "select count(*) from pg_database where datname like 'test_%' or datname like 'boss_test%';")
echo "leaked test DBs: ${n// /}"
[[ "${n// /}" == "0" ]] && { echo "nothing to clean"; exit 0; }
psql -tA -h "$PGHOST" -U "$PGUSER" -d postgres -c \
  "select 'DROP DATABASE IF EXISTS \"'||datname||'\" WITH (FORCE);' from pg_database \
   where (datname like 'test_%' or datname like 'boss_test%') and datname <> 'boss';" \
  | psql -h "$PGHOST" -U "$PGUSER" -d postgres -f - >/dev/null
echo "dropped; reclaimed test DBs"
