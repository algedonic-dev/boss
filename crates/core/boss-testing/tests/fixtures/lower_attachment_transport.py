#!/usr/bin/env python3
"""Isolated Longhorn JSON-patch fixture, including a daemon outside the owner."""
import copy
import fcntl
import json
import os
from pathlib import Path
import signal
import sys
import time

root = Path(os.environ['STUB_STATE'])
volume = root / 'vol-pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56.json'


def save(path, value):
    temporary = path.with_name(path.name + '.next-' + str(os.getpid()))
    temporary.write_text(json.dumps(value))
    os.replace(temporary, path)


def wait_for(path):
    end = time.monotonic() + 90
    while not path.exists():
        if time.monotonic() >= end:
            raise RuntimeError('fixture barrier unavailable: ' + path.name)
        time.sleep(0.01)


def pointer(document, path):
    parts = [part.replace('~1', '/').replace('~0', '~') for part in path.split('/')[1:]]
    for part in parts[:-1]:
        document = document[part]
    return document, parts[-1]


def apply(patch, origin):
    with (root / 'attachment.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        before = json.loads((root / 'attachment.json').read_text())
        after = copy.deepcopy(before)
        status, error = 'applied', ''
        try:
            assert patch[:2] == [
                {'op': 'test', 'path': '/metadata/uid', 'value': before['metadata']['uid']},
                {'op': 'test', 'path': '/metadata/resourceVersion', 'value': before['metadata']['resourceVersion']}], 'UID/resourceVersion conflict'
            for operation in patch[2:]:
                path, op = operation['path'], operation['op']
                assert path == '/spec/attachmentTickets' or path.startswith('/spec/attachmentTickets/') or path == '/metadata/annotations' or path.startswith('/metadata/annotations/'), 'unsupported fixture patch path'
                parent, key = pointer(after, path)
                if op == 'test':
                    assert parent[key] == operation['value'], 'ticket test conflict'
                elif op == 'add':
                    parent[key] = operation['value']
                elif op == 'remove':
                    del parent[key]
                else:
                    raise AssertionError('unsupported fixture patch operation')
            if after == before:
                status = 'noop'
            else:
                after['metadata']['resourceVersion'] = str(int(before['metadata']['resourceVersion']) + 1)
                tickets = after['spec'].get('attachmentTickets', {})
                after.setdefault('status', {})['attachmentTicketStatuses'] = {
                    key: {'id': key, 'generation': value['generation'], 'satisfied': True}
                    for key, value in tickets.items()}
                v = json.loads(volume.read_text())
                node = next(iter(tickets.values()))['nodeID'] if tickets else ''
                v['spec']['nodeID'] = v['status']['currentNodeID'] = node
                v['status']['state'] = 'attached' if tickets else 'detached'
                v['status']['robustness'] = 'healthy' if tickets else 'unknown'
                reps = json.loads((root / 'replicas.json').read_text())
                for replica in reps:
                    replica['status']['currentState'] = 'running' if tickets else 'stopped'
                save(root / 'attachment.json', after)
                save(volume, v)
                save(root / 'replicas.json', reps)
        except (AssertionError, KeyError) as failure:
            status, error = 'conflict', str(failure)
            after = before
        with (root / 'attachment-patches.jsonl').open('a') as log:
            log.write(json.dumps(dict(origin=origin, patch=patch, status=status, error=error,
                before=before, after=after)) + '\n')
        return status, after


if sys.argv[1:] == ['--hold-count']:
    (root / 'count-patch-ready').write_text('engine count request reached, not applied')
    wait_for(root / 'never-release-count')
    sys.exit(0)

if sys.argv[1:] == ['--daemon']:
    (root / 'external-ready').write_text('outside-owner transport started')
    wait_for(root / 'queued-attach.json')
    (root / 'external-received').write_text('whole attach body retained')
    wait_for(root / 'release-queued')
    status, _ = apply(json.loads((root / 'queued-attach.json').read_text()), 'late-daemon')
    (root / 'external-done').write_text(status)
    sys.exit(0)

patch = json.loads(sys.argv[1])
worker = int(sys.argv[2])
attach = any(op['op'] == 'add' and op['path'] == '/spec/attachmentTickets' for op in patch)
annotation = any(op['path'].startswith('/metadata/annotations') for op in patch)
if attach and os.environ.get('STUB_EXTERNAL_ATTACH'):
    save(root / 'queued-attach.json', patch)
    (root / 'queued-worker').write_text(str(worker))
    mode = os.environ['STUB_EXTERNAL_ATTACH']
    if mode == 'lost-ack':
        print('fixture lost attach ACK before the queued write', file=sys.stderr)
        sys.exit(1)
    # Real Door.call timeout kills this client at25s. The external daemon lives
    # outside that process tree and retains the exact request until released.
    wait_for(root / 'never-release-client')

if annotation and os.environ.get('STUB_FENCE_RACE') and not (root / 'fence-race-fired').exists():
    (root / 'fence-race-fired').write_text('late attach wins before first fence CAS')
    (root / 'release-queued').write_text('release before fence')
    wait_for(root / 'external-done')
status, result = apply(patch, 'client')
if status == 'conflict':
    print('The request is invalid: UID/resourceVersion or ticket test conflict', file=sys.stderr)
    sys.exit(1)
if attach:
    (root / 'attached-worker').write_text(str(worker))
    if os.environ.get('STUB_CHANGED_OWN_TICKET'):
        for ticket in result['spec']['attachmentTickets'].values():
            ticket['generation'] = 2
        save(root / 'attachment.json', result)
        print('fixture changed admitted ticket', file=sys.stderr)
        sys.exit(1)
    if os.environ.get('STUB_REPLACED_ATTACHMENT'):
        result['metadata']['uid'] = 'replacement-attachment'
        save(root / 'attachment.json', result)
        print('fixture replaced admitted attachment', file=sys.stderr)
        sys.exit(1)
    if os.environ.get('STUB_KILL_AFTER_ATTACH'):
        (root / 'killed-worker').write_text(str(worker))
        os.kill(worker, signal.SIGKILL)
        sys.exit(1)
    if os.environ.get('STUB_ATTACH_LOST_ACK'):
        print('fixture lost attach ACK', file=sys.stderr)
        sys.exit(1)
if annotation and os.environ.get('STUB_FENCE_LOST_ACK') and not (root / 'fence-ack-lost').exists():
    (root / 'fence-ack-lost').write_text('fence applied; ACK lost')
    print('fixture lost fence ACK after applied patch', file=sys.stderr)
    sys.exit(1)
print('volumeattachment patched')
