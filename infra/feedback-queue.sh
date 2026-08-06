#!/usr/bin/env bash
#
# feedback-queue — read the triage board from a terminal.
#
# The board at /system/feedback is the operator's view; this is the
# same thing for whoever is playing the agent. Read-only on purpose:
# taking an item, annotating it, or closing it goes through the board
# or the step API, so every state change carries an actor. A script
# that could also write would make "who triaged this" ambiguous
# exactly where the audit trail matters most.
#
#   ./infra/feedback-queue.sh            # all three columns
#   ./infra/feedback-queue.sh with-agent # just the agent queue
#
# Columns are derived, not stored — same as the board. But this
# derives them from facts the board does not own: a Job is done when
# the JOB is closed, and it is with an agent when ANY step carries an
# agent request.
#
# That is deliberate. The first version of this script found the
# triage step the way the board did at the time — by matching a step
# kind — and drifted the same day, when the board switched to finding
# it by its authority gate. The script then reported a freshly-filed
# item as already triaged, which is the worst way for a queue reader
# to be wrong. Two copies of "how to find the triage step" is the
# fact-that-lives-twice failure in CLAUDE.md §9a; the fix is not a
# comment telling the next person to sync them, it is not needing the
# rule here at all.
set -euo pipefail

BASE="${BOSS_GATEWAY_URL:-http://127.0.0.1:4443}"
WANT="${1:-all}"

curl -fsS "$BASE/api/jobs?kind=user-feedback&limit=200" | python3 -c "
import json, sys

want = sys.argv[1] if len(sys.argv) > 1 else 'all'
body = json.load(sys.stdin)
rows = body['data'] if isinstance(body, dict) and 'data' in body else body

def agent_request(job):
    '''The agent hand-off record, from whichever step carries it.'''
    for s in job.get('steps', []):
        md = s.get('metadata') or {}
        if md.get('agent_requested_at'):
            return md
    return None

def column(job):
    # The Job's own status is the authority on whether it is finished
    # — no step-kind knowledge required, and it cannot disagree with
    # the Job the way a re-derived guess can.
    if job.get('status') == 'closed':
        return 'done'
    return 'with-agent' if agent_request(job) else 'waiting'

buckets = {'waiting': [], 'with-agent': [], 'done': []}
for j in rows:
    buckets[column(j)].append(j)

order = ['waiting', 'with-agent', 'done'] if want == 'all' else [want]
if want != 'all' and want not in buckets:
    sys.exit(f'unknown column {want!r} — waiting | with-agent | done')

for col in order:
    items = buckets[col]
    print(f'{col}  ({len(items)})')
    if not items:
        print('    (empty)')
    for j in items:
        md = j.get('metadata') or {}
        tmd = agent_request(j) or {}
        print(f\"    {j['id'][:8]}  {md.get('route', '?')}\")
        msg = (md.get('message') or '').strip().replace(chr(10), ' ')
        print(f'       {msg[:100]}')
        print(f\"       from {j.get('owner_id', '?')}\" + (
            f\"  ·  with agent since {tmd['agent_requested_at'][:16]}\"
            if tmd.get('agent_requested_at') else ''))
    print()
" "$WANT"
