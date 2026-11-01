#!/usr/bin/env bash
# Fixed read-only query observation; a packet supplies no SQL, actor or target.
set -uo pipefail
if [ "$#" -ne 0 ]; then
    echo 'read-membership-starting-state: REFUSED — no arguments accepted' >&2
    exit 78
fi
case ${OPS_REQUEST_ID:-} in
  ????????-????-????-????-????????????) ;;
  *) echo 'membership observation: actual ops request identity required' >&2; exit 78;;
esac
if ! python3 -c 'import sys,uuid; assert str(uuid.UUID(sys.argv[1])) == sys.argv[1]' "$OPS_REQUEST_ID"; then
  echo 'membership observation: malformed ops request identity' >&2; exit 78
fi
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh" || exit 1
sor_require BOSS_JOBS_URL || exit 1
# shellcheck source=infra/lib/secret-header.sh
. "$HERE/../lib/secret-header.sh" || exit 1
MT_HDR=""
machine_token_header MT_HDR "$BOSS_JOBS_URL" || exit 1
if [ -z "$MT_HDR" ]; then
    echo 'read-membership-starting-state: protected reader credential unavailable' >&2
    exit 1
fi
# Equality-pinned to the server's MAX_REPORT_BYTES by the HTTP contract test.
MAX_REPORT_BYTES=1048576
umask 077
originals=$(mktemp -d "${TMPDIR:-/tmp}/boss-membership-observation.XXXXXXXX") || exit 74
curl --fail-with-body -sS --connect-timeout 5 --max-time 100 --max-filesize "$MAX_REPORT_BYTES" \
    -H "$MT_HDR" \
    -H "x-boss-user: $(sor_reader_header 'automation:read-membership-starting-state')" \
    "${BOSS_JOBS_URL%/}/api/jobs/membership-starting-state-observation" > "$originals/stdout" 2> "$originals/stderr"
status=$?
secret_header_close
cat "$originals/stdout"
cat "$originals/stderr" >&2
python3 "$HERE/membership-observation-retain.py" "$originals" "$OPS_REQUEST_ID" "$status"
retention_status=$?
if [ "$retention_status" -ne 0 ]; then
  echo "membership observation retention failed; original query exit $status; originals retained at $originals" >&2
  if [ "$status" -eq 0 ]; then exit "$retention_status"; fi
fi
exit "$status"
