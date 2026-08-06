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
# Columns are derived from the triage step, the same way the board
# derives them — not from a status field, because there isn't one.
set -euo pipefail

BASE="${BOSS_GATEWAY_URL:-http://127.0.0.1:4443}"
WANT="${1:-all}"

curl -fsS "$BASE/api/jobs?kind=user-feedback&limit=200" | python3 -c "
import json, sys

want = sys.argv[1] if len(sys.argv) > 1 else 'all'
body = json.load(sys.stdin)
rows = body['data'] if isinstance(body, dict) and 'data' in body else body

def triage(job):
    return next((s for s in job.get('steps', []) if s['kind'] == 'acknowledgment'), None)

def column(job):
    t = triage(job)
    if not t or t['status'] in ('completed', 'skipped'):
        return 'done'
    md = t.get('metadata') or {}
    return 'with-agent' if md.get('agent_requested_at') else 'waiting'

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
        t = triage(j) or {}
        tmd = t.get('metadata') or {}
        print(f\"    {j['id'][:8]}  {md.get('route', '?')}\")
        msg = (md.get('message') or '').strip().replace(chr(10), ' ')
        print(f'       {msg[:100]}')
        print(f\"       from {j.get('owner_id', '?')}\" + (
            f\"  ·  with agent since {tmd['agent_requested_at'][:16]}\"
            if tmd.get('agent_requested_at') else ''))
    print()
" "$WANT"
