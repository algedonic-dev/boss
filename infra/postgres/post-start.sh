#!/usr/bin/env bash
# 78625ebd: a supervised chore, never an init-container prerequisite.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "$#" -ne 1 ]; then
    echo 'post-start.sh: expected --pending or --apply' >&2
    exit 2
fi
case "$1" in
    --pending) mode=(--phase post-start --pending) ;;
    --apply) mode=(--phase post-start) ;;
    *) echo 'post-start.sh: expected --pending or --apply' >&2; exit 2 ;;
esac
: "${DATABASE_URL:?DATABASE_URL must name the same database as the serving instance}"
: "${BOSS_JOBS_URL:?BOSS_JOBS_URL must name the serving instance}"
export PGDATABASE
PGDATABASE="$("$HERE/../oss-quickstart/database-from-url.sh" "$DATABASE_URL")"
# This is a post-start obligation. A dark/unreadable instance is a
# failed Job, never permission to turn this back into boot work.
. "$HERE/../lib/secret-header.sh"
machine_token_header MT_HDR "$BOSS_JOBS_URL"
curl -q --fail --silent --show-error --max-time 5 ${MT_HDR:+-H "$MT_HDR"} "$BOSS_JOBS_URL/api/jobs/health"
secret_header_close
exec /bin/bash "$HERE/migrate.sh" "${mode[@]}"
