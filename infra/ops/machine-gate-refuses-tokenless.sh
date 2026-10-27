#!/usr/bin/env bash
# A machine-gate enforcement probe measures the refusal itself.
# Deliberately does not source a credential reader. Curl's startup config
# is disabled and no machine header is sent, even if the
# runner inherited a token. Run only after the enforcing car converges.
set -uo pipefail
case ${BOSS_JOBS_URL:-} in
    http://*|https://*) ;;
    *) echo 'not yet: BOSS_JOBS_URL is not set to the system of record'; exit 75 ;;
esac
status=$(curl --disable --silent --show-error --max-time 15 --output /dev/null \
    --write-out '%{http_code}' \
    --header 'x-boss-user: {"id":"machine-gate-tokenless-probe","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}' \
    "${BOSS_JOBS_URL%/}/api/jobs?limit=1") || {
    echo 'not yet: the tokenless request could not reach the jobs API'
    exit 75
}
case "$status" in
    401) echo 'machine gate enforces: tokenless jobs request refused 401'; exit 0 ;;
    200) echo 'not yet: the jobs API still accepts a tokenless request'; exit 75 ;;
    *) echo "machine gate tokenless probe: unexpected HTTP $status (expected 401)"; exit 1 ;;
esac
