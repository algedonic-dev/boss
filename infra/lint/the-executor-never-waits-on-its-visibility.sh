#!/usr/bin/env bash
# the-executor-never-waits-on-its-visibility.sh — boss-maintenance-wrap
# lets the chore run whatever the jobs API does: unreachable, an HTTP
# error answer, or a reply it cannot read.
#
# Runs a copy of the wrap beside a stub boss-api-curl.sh. 2026-09-05:
# the cluster converge could not start for four hours because its
# ExecStartPre (this wrap) failed on a dark system of record — the loop
# that would have restored the API was the one waiting on it. 2026-09-27
# (backlog 9fd7f51e): the same loop still stopped on an API that
# ANSWERED 503, which the policy service does while it is down. The
# real-socket version of this check, with the replay ledger, is
# crates/core/boss-testing/tests/the_chore_runs_through_a_sor_that_answers_badly.rs.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wrap="$here/../boss-maintenance-wrap.sh"
[[ -f "$wrap" ]] || { echo "the-executor-never-waits-on-its-visibility: missing $wrap" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cp "$wrap" "$tmp/boss-maintenance-wrap.sh"; chmod +x "$tmp/boss-maintenance-wrap.sh"
# The wrap hands the machine token to curl as a header file made by the
# lib beside it, and files nothing without it (backlog 5f3ad356): lib/
# next to the copy, as in the image.
mkdir -p "$tmp/lib" && cp "$here/../lib/secret-header.sh" "$tmp/lib/secret-header.sh" \
    || { echo "the-executor-never-waits-on-its-visibility: cannot lay infra/lib/secret-header.sh beside the wrap's copy" >&2; exit 1; }
stub() {  # $1 = exit code, $2 = stdout for GET
    printf '#!/usr/bin/env bash\necho "stub api-curl $*" >>"%s"\n[[ "$*" == *"-X POST"* ]] || printf %%s '"'"'%s'"'"'\nexit %s\n' "$tmp/calls" "$2" "$1" >"$tmp/boss-api-curl.sh"
    chmod +x "$tmp/boss-api-curl.sh"
}
# HOME is the run's own: the wrap keeps each miss in a ledger under it.
run() { HOME="$tmp/home" BOSS_JOBS_URL=http://stub bash "$tmp/boss-maintenance-wrap.sh" maintenance-cluster-converge "Cluster converge" >"$tmp/out" 2>&1; echo $?; }
fail() { echo "FAIL: $*" >&2; echo "--- wrap output:" >&2; cat "$tmp/out" >&2; exit 1; }

# 1. Unreachable (curl exit 7 past the deadline): the run proceeds, loudly.
stub 7 ""; rc=$(run)
[[ "$rc" -eq 0 ]] || fail "an unreachable API blocked the executor (rc $rc)"
grep -q "UNRECORDED" "$tmp/out" || fail "no loud UNRECORDED line on an unreachable API"
# 2. The API answered an error (curl exit 22 — a 503 from a policy
#    outage is one): the run STILL proceeds, loudly.
stub 22 ""; rc=$(run)
[[ "$rc" -eq 0 ]] || fail "an API error answer blocked the executor (rc $rc)"
grep -q "UNRECORDED" "$tmp/out" || fail "no loud UNRECORDED line on an API error answer"
# 3. A healthy API: the success paths are unchanged (needs jq, as the wrap does).
if command -v jq >/dev/null 2>&1; then
    stub 0 '{"rows":[]}'; rc=$(run)
    [[ "$rc" -eq 0 ]] && grep -q "UNRECORDED" "$tmp/out" || fail "a reply with no .data blocked the executor or went unsaid"
    grep -q -- "-X POST" "$tmp/calls" && fail "a reply with no .data spawned a packet (it could be a duplicate)"
    stub 0 '{"data":[{"id":"x"}]}'; rc=$(run)
    [[ "$rc" -eq 0 ]] && grep -q "recovery" "$tmp/out" || fail "an open packet was not reused"
    stub 0 '{"data":[]}'; rc=$(run)
    [[ "$rc" -eq 0 ]] && grep -q "spawned" "$tmp/out" || fail "a fresh packet was not spawned"
    healthy="healthy paths unchanged (reuse and spawn)"
else
    healthy="healthy paths not exercised here (no jq on this box)"
fi
echo "the-executor-never-waits-on-its-visibility: self-test ok — an unreachable API, an error answer and an unreadable reply each let the chore run with a loud line; $healthy"
exit 0
