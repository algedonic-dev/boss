import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(os.environ['PROOF_SCRATCH'])
probe = Path(os.environ['PROOF_SCRIPT'])
stamp = '2026-10-23T20:00:00Z'
inventory = [{'path': '/work/wt/registered', 'admin': '/work/boss/.git/worktrees/registered', 'state': 'held', 'why': 'actual held reading', 'git_admitted': True}]
digest = hashlib.sha256(json.dumps(inventory, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
metadata = dict(result='ok', problems='0', worktree_pass='ran', worktree_pass_reason='', worktree_hold_state='observed', worktree_hold_complete=True, worktree_hold_hash_format='compact-sorted-keys-no-newline', worktree_hold_at=stamp, worktree_hold_inventory=inventory, worktree_hold_total=1, worktree_hold_held=1, worktree_hold_sha256=digest)
job = dict(id='native-job', partition='real', simulated=False, opened_at=stamp, steps=[dict(spec_slug='run', status='completed', completed_by='automation:dev-scratch-reclaim', completed_at=stamp, metadata=metadata)])
fixture = tempfile.TemporaryDirectory()
folder = Path(fixture.name)
reader = folder / 'boss-sor-read'
reader.write_text('''#!/usr/bin/python3
import json,os,sys
if '2999-' in sys.argv[1]:
 print(json.dumps(dict(data=[],total=0,limit=1,offset=0,full=False)))
else:
 print(open(os.environ['PROOF_FIXTURE']).read())
''')
reader.chmod(0o700)
env = dict(os.environ, PATH=str(folder)+':'+os.environ['PATH'], BOSS_CAR_CONVERGED_AT='1761249600')
# Frontier intentionally 2025: valid native 2026 document is after it.
cases = []
def case(name, mutate=None, rc=75):
    document = dict(data=[copy.deepcopy(job)], total=1, limit=100, offset=0, full=True)
    if mutate:
        mutate(document)
    cases.append((name, document, rc))
case('whole-native-positive', rc=0)
def coverage(d, state):
    m = d['data'][0]['steps'][0]['metadata']
    m['worktree_hold_inventory'].append(dict(path='/outside-or-absent', admin='/work/boss/.git/worktrees/other', state=state, why='not admitted; no git', git_admitted=False))
    m['worktree_hold_total'] = 2
    m['worktree_hold_sha256'] = hashlib.sha256(json.dumps(m['worktree_hold_inventory'], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
case('absent-is-complete-nonadmission', lambda d: coverage(d, 'absent'), rc=0)
case('unseen-is-complete-nonadmission', lambda d: coverage(d, 'unseen'), rc=0)
def refused(d):
    coverage(d, 'unpaired')
    d['data'][0]['steps'][0]['metadata'].update(result='incomplete', problems='1', output='RED dev-scratch-reclaim/worktree-git-unpaired problem: /outside-or-absent/.git refused; no git')
case('named-unpaired-refusal-is-conserved', refused, rc=0)
case('partial-inventory-cannot-ack', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_complete=False))
case('unknown-row-is-not-complete', lambda d: coverage(d, 'unknown'))
def admitted_refusal(d):
    coverage(d, 'unheld')
    m = d['data'][0]['steps'][0]['metadata']
    m['worktree_hold_inventory'][1]['git_admitted'] = True
    m['worktree_hold_sha256'] = hashlib.sha256(json.dumps(m['worktree_hold_inventory'], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
case('refusal-never-admits-git', admitted_refusal)
def duplicate_paths(d, admitted=False):
    coverage(d, 'unpaired')
    m = d['data'][0]['steps'][0]['metadata']
    row = copy.deepcopy(m['worktree_hold_inventory'][1])
    row['admin'] += '-duplicate'
    m['worktree_hold_inventory'].append(row)
    if admitted:
        for item in m['worktree_hold_inventory']:
            item.update(state='held', git_admitted=True)
        m['worktree_hold_held'] = 3
    m['worktree_hold_total'] = 3
    m['worktree_hold_sha256'] = hashlib.sha256(json.dumps(m['worktree_hold_inventory'], sort_keys=True, separators=(',', ':')).encode()).hexdigest()
case('duplicate-refused-paths-conserve-admin-entries', duplicate_paths, rc=0)
case('duplicate-admitted-paths-are-not-independent-coverage', lambda d: duplicate_paths(d, True))
case('unrelated-problems-remain-visible', lambda d: d['data'][0]['steps'][0]['metadata'].update(result='incomplete', problems='2', output='RED dev-scratch-reclaim/scratch-floor-unmet problem: full disk'), rc=0)
def floor_after(d):
    floor = copy.deepcopy(d['data'][0])
    floor.update(id='later-floor', opened_at='2026-10-23T21:00:00Z')
    floor['steps'][0]['metadata'].update(worktree_pass='skipped', worktree_pass_reason='scratch-floor: the build-triggered pass runs only the floor\'s sibling reclaim', worktree_hold_state='not-read')
    d.update(data=d['data']+[floor], total=2)
case('newer-floor-does-not-hide-hourly', floor_after, rc=0)
def only_floor(d):
    floor_after(d)
    d.update(data=d['data'][1:], total=1)
case('only-floor-is-not-hourly-proof', only_floor)
case('empty', lambda d: d.update(data=[], total=0))
case('truncated', lambda d: d.update(total=2))
case('duplicate', lambda d: d.update(data=d['data']*2, total=2))
case('pending', lambda d: d['data'][0]['steps'][0].update(status='active'))
case('unacknowledged', lambda d: d['data'][0]['steps'][0].update(completed_by=None))
case('failed', lambda d: d['data'][0]['steps'][0]['metadata'].update(result='failed'))
case('not-observed', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_state='not-read'))
case('future', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_at='2999-01-01T00:00:00Z'))
case('old', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_at='2020-01-01T00:00:00Z'))
case('empty-inventory', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_inventory=[], worktree_hold_total=0, worktree_hold_held=0))
case('unheld', lambda d: d['data'][0]['steps'][0]['metadata']['worktree_hold_inventory'][0].update(state='unheld'))
case('typed-count', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_total='1'))
case('count-mismatch', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_total=2))
case('tampered-hash', lambda d: d['data'][0]['steps'][0]['metadata'].update(worktree_hold_sha256='0'*64))
case('newer-pending-supersedes', lambda d: d.update(data=d['data']+[dict(job, id='later', opened_at='2026-10-23T20:01:00Z', steps=[dict(job['steps'][0], status='active')])], total=2))
def fractional(d, newer_status="active", old_stamp="2026-10-23T20:00:00.1Z", newer_stamp="2026-10-23T20:00:00.11Z"):
    older = d['data'][0]
    older['opened_at'] = old_stamp
    newer = copy.deepcopy(older)
    newer.update(id='newer', opened_at=newer_stamp)
    newer['steps'][0]['status'] = newer_status
    d.update(data=[newer, older], total=2)
case('newer-fractional-pending', fractional)
case('newer-fractional-failed', lambda d: fractional(d, 'failed'))
case('whole-second-then-fractional-pending', lambda d: fractional(d, old_stamp='2026-10-23T20:00:00Z', newer_stamp='2026-10-23T20:00:00.01Z'))
case('equal-instant-is-ambiguous', lambda d: fractional(d, old_stamp='2026-10-23T20:00:00.1Z', newer_stamp='2026-10-23T20:00:00.100Z'))
def newer_clean(d):
    fractional(d, 'completed')
    d['data'][1]['steps'][0]['status'] = 'active'
case('newest-fractional-clean-positive', newer_clean, rc=0)
case('unparseable-native-admission', lambda d: d['data'][0].update(opened_at='not-an-instant'))
def hidden_malformed(d):
    fractional(d)
    d['data'][0]['opened_at'] = 'not-an-instant'
case('malformed-newer-cannot-disappear-behind-clean', hidden_malformed)

for name, document, expected in cases:
    path = folder / (name+'.json')
    path.write_text(json.dumps(document))
    result = subprocess.run(['bash', str(probe)], env=dict(env, PROOF_FIXTURE=str(path)), text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (root/(name+'.log')).write_text(result.stdout)
    print(name, 'exit', result.returncode, 'expected', expected, flush=True)
    if result.returncode != expected:
        print(result.stdout)
        raise SystemExit(1)
# Empty and malformed successful transports must not be positives.
for name, body in [('empty-body', ''), ('whitespace', ' \n'), ('malformed', '{'), ('two-documents', '{}\n{}')]:
    path = folder / (name+'.json')
    path.write_text(body)
    result = subprocess.run(['bash', str(probe)], env=dict(env, PROOF_FIXTURE=str(path)), text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (root/(name+'.log')).write_text(result.stdout)
    print(name, 'exit', result.returncode, 'expected', 75, flush=True)
    assert result.returncode == 75
print(f'{len(cases)+4} native-proof controls passed')
