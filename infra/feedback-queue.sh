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
#
# Reads jobs-api directly rather than through the gateway. The gateway
# is the BROWSER edge: it authenticates a session cookie and strips
# every inbound `x-boss-*` header, so an operator script has no way to
# present itself there. Terminal tooling goes to the service port with
# an actor header — the same path verify-smoke.sh and verify-replay.sh
# take.
#
# This script used to curl the gateway anonymously. That worked only
# because demo mode minted an `audit-readonly` session for anyone who
# asked; when that was removed the reader started returning 401 and a
# stack trace. Anonymous read was never the contract, it was a side
# effect.
set -euo pipefail

# jobs-api. Port from boss-ports (`name: "jobs", prod: 7900`); six
# infra scripts hardcode it the same way.
BASE="${BOSS_JOBS_URL:-http://127.0.0.1:7900}"
WANT="${1:-all}"

# Reads are policy-gated; an unheadered call lands as `guest`, which
# holds Workflow read and nothing else. Reading is all this does — the
# docstring above is the reason writes are not added here.
BOSS_USER='{"id":"it-triage-queue","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

curl -fsS -H "x-boss-user: $BOSS_USER" "$BASE/api/jobs?kind=user-feedback&limit=200" | python3 -c "
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

# The step that asks for a disposition. Found by the enum field it
# declares, which is the same data the board reads — not a second copy
# of the rule for which step is the triage step. A pipe-shaped
# field_type IS the fork marker; the Workflow lint reads it the same way
# to prove every value has a successor.
#
# NOTE: this whole program is embedded in a double-quoted shell string,
# so a literal double quote here silently truncates it. That is exactly
# how this function broke on first write.
def fork_step(job):
    for s in job.get('steps', []):
        for f in s.get('fields') or []:
            if f.get('required') and '|' in (f.get('field_type') or ''):
                return s, f['name']
    # Jobs opened before the fork existed keep their old steps forever
    # — a gated step with no disposition field. Same fallback the board
    # uses, for the same reason: without it a routed legacy item reads
    # as still waiting, which is a queue reader lying about the queue.
    for s in job.get('steps', []):
        if (s.get('metadata') or {}).get('authority_role'):
            return s, 'disposition'
    return None, None

def column(job):
    # A routed item is NOT waiting. Reporting it as waiting is how this
    # script hid two in-flight items during a triage session — the
    # opposite of what a queue reader is for.
    if job.get('status') == 'closed':
        return 'done'
    step, field = fork_step(job)
    if step and step.get('status') in ('completed', 'skipped'):
        chosen = (step.get('metadata') or {}).get(field)
        return f'routed:{chosen}' if chosen else 'routed'
    return 'with-agent' if agent_request(job) else 'waiting'

buckets = {'waiting': [], 'with-agent': [], 'done': []}
for j in rows:
    buckets.setdefault(column(j), []).append(j)

# Routed buckets are discovered from the data, so a new disposition in
# the registry shows up here without editing this script.
routed = sorted(k for k in buckets if k.startswith('routed'))
order = ['waiting', 'with-agent'] + routed + ['done'] if want == 'all' else [want]
if want != 'all' and want not in buckets:
    sys.exit(f'unknown column {want!r} — one of: ' + ', '.join(sorted(buckets)))

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
