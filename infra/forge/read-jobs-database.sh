#!/usr/bin/env bash
# Fixed read-only native observations; the packet supplies no command or target.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Internal fixed read invoked by the lossless observation Recorder. No URL,
# query, credential or free-form argument can be supplied by a packet.
if [ "$#" -eq 1 ] && [ "$1" = --read-target ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$HERE/../lib/sor.sh" || exit 1
    sor_require BOSS_JOBS_URL || exit 1
    # shellcheck source=infra/lib/secret-header.sh
    . "$HERE/../lib/secret-header.sh" || exit 1
    MT_HDR=""
    machine_token_header MT_HDR "$BOSS_JOBS_URL" || exit 1
    if [ -z "$MT_HDR" ]; then
        echo 'read-jobs-database: protected target reader credential unavailable' >&2
        exit 1
    fi
    curl -fsS --connect-timeout 5 --max-time 15 --max-filesize 16384 \
        -H "$MT_HDR" -H "x-boss-user: $(sor_reader_header 'automation:read-jobs-database')" \
        "${BOSS_JOBS_URL%/}/api/jobs/database-observation"
    status=$?
    secret_header_close
    exit "$status"
fi
if [ "$#" -ne 0 ]; then
    echo 'read-jobs-database: REFUSED — no arguments accepted' >&2
    exit 78
fi
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
if ! ops_image_ready >&2; then
    echo 'read-jobs-database: CANNOT ANSWER — existing ops image unavailable' >&2
    exit 1
fi
DATABASE_OBSERVATION_KUBECTL="$(ops_kubectl)" || exit 1
export DATABASE_OBSERVATION_KUBECTL
exec python3 -B "$HERE/jobs-database.py"
