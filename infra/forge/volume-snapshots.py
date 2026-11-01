#!/usr/bin/env python3
"""Fixed read-only Longhorn API inventory (34b7906a), not a cleanup plan.

Wire fields are the estate's longhorn-manager v1.11.3 definitions:
https://github.com/longhorn/longhorn-manager/blob/v1.11.3/k8s/pkg/apis/longhorn/v1beta2/snapshot.go
https://github.com/longhorn/longhorn-manager/blob/v1.11.3/k8s/pkg/apis/longhorn/v1beta2/engine.go
CRD records and engine caches are distinct observations, never a promise
that a snapshot can be deleted or that its size is reclaimable disk space.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sys
import tempfile
import time
from urllib.parse import urlencode

_spec = importlib.util.spec_from_file_location(
    'snapshot_bounded_transport', Path(__file__).with_name('gate-storage.py'))
bounded = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bounded)
KINDS = {'volumes': 'Volume', 'snapshots': 'Snapshot', 'engines': 'Engine', 'replicas': 'Replica'}
BASE = '/apis/longhorn.io/v1beta2/namespaces/longhorn-system/'
MAX_PAGES = 64
MAX_ROWS = 4096


def inventory(recorder, kubectl, plural):
    rows, seen, cursors = [], set(), set()
    cursor, version = '', None
    # Adapt only a retained byte-cap refusal, restarting the whole collection:
    # Kubernetes continuation tokens require an unchanged original query.
    page_size = 40
    for page in range(MAX_PAGES):
        query = {'limit': page_size}
        if cursor:
            query['continue'] = cursor
        reading = recorder.call(
            plural + '.' + str(page), kubectl + ['get', '--raw', BASE + plural + '?' + urlencode(query),
                                               '--request-timeout=20s'])
        if (reading.get('stream_complete') is False
                and sum(len(bounded.decode_record(reading[key])) for key in ('stdout', 'stderr')
                        if key in reading) == bounded.SCALAR_BYTES):
            if page_size == 1:
                raise ValueError(plural + ': one native object exceeds the per-call byte bound')
            page_size = max(1, page_size // 2)
            rows, seen, cursors = [], set(), set()
            cursor, version = '', None
            continue
        response = bounded.one_json(bounded.text(reading))
        meta = response.get('metadata', {})
        rv = meta.get('resourceVersion')
        if (response.get('apiVersion') != 'longhorn.io/v1beta2'
                or response.get('kind') != KINDS[plural] + 'List'
                or not isinstance(rv, str) or not rv
                or (version is not None and rv != version)
                or not isinstance(response.get('items'), list)):
            raise ValueError(plural + ': incomplete or changed native list')
        version = rv
        for row in response['items']:
            identity = row.get('metadata', {})
            name, uid = identity.get('name'), identity.get('uid')
            if (row.get('apiVersion') != 'longhorn.io/v1beta2' or row.get('kind') != KINDS[plural]
                    or identity.get('namespace') != 'longhorn-system'
                    or not isinstance(name, str) or not re.fullmatch(bounded.DNS, name)
                    or not isinstance(uid, str) or not re.fullmatch(bounded.UUID, uid)
                    or name in seen):
                raise ValueError(plural + ': duplicate or unread native identity')
            seen.add(name)
            rows.append(row)
            if len(rows) > MAX_ROWS:
                raise ValueError(plural + ': row bound reached')
        cursor = meta.get('continue', '')
        if not isinstance(cursor, str) or len(cursor) > 4096:
            raise ValueError(plural + ': unread continuation')
        remaining = meta.get('remainingItemCount', 0)
        if type(remaining) is not int or remaining < 0:
            raise ValueError(plural + ': unread remaining count')
        if not cursor:
            if remaining:
                raise ValueError(plural + ': rows remain without a continuation')
            return rows
        if cursor in cursors:
            raise ValueError(plural + ': repeated continuation')
        cursors.add(cursor)
    raise ValueError(plural + ': page bound reached')


def byte_count(value):
    if type(value) is int and value >= 0:
        return value
    if isinstance(value, str) and re.fullmatch(r'[0-9]{1,20}', value):
        return int(value)
    return None


def snapshot(name, status, engine=False):
    size = byte_count(status.get('size'))
    return {'name': name, 'size_bytes': size,
            'size_state': 'reported' if size is not None else 'unavailable: native size absent or invalid',
            'created': status.get('created' if engine else 'creationTime'),
            'removed': status.get('removed' if engine else 'markRemoved'),
            'user_created': status.get('usercreated' if engine else 'userCreated'),
            'ready_to_use': None if engine else status.get('readyToUse'),
            'parent': status.get('parent'), 'children': status.get('children'),
            'error': status.get('error')}


def engine(row):
    status = row.get('status', {})
    cache = status.get('snapshots')
    available = (status.get('currentState') == 'running' and status.get('snapshotsError') == ''
                 and isinstance(cache, dict))
    entries = []
    head = None
    if isinstance(cache, dict):
        for name, value in sorted(cache.items()):
            if not isinstance(value, dict) or value.get('name') != name:
                raise ValueError('engine snapshot cache identity differs')
            entry = snapshot(name, value, engine=True)
            if name == 'volume-head':
                head = entry
            else:
                entries.append(entry)
    return {'name': row['metadata']['name'], 'uid': row['metadata']['uid'],
            'current_state': status.get('currentState'), 'error': status.get('snapshotsError'),
            'cache_state': 'reported-running-cache' if available else 'unavailable',
            'snapshot_count': len(entries) if available else None,
            'snapshots': entries, 'head': head}


def collect(recorder, kubectl):
    record = {'schema': 1, 'claim': 'longhorn-snapshot-api-inventory-only', 'state': 'unavailable',
              'coverage': 'Separate API lists, not an atomic on-disk census or cleanup authorization; '
                          'engine caches may be unavailable or stale. Snapshot sizes are not reclaimable bytes.',
              'volumes': None, 'errors': []}
    lists = {}
    for plural in KINDS:
        try:
            lists[plural] = inventory(recorder, kubectl, plural)
        except (ValueError, KeyError, TypeError, UnicodeError) as error:
            record['errors'].append(plural + ': ' + str(error))
    if len(lists) == len(KINDS):
        try:
            names = {row['metadata']['name'] for row in lists['volumes']}
            record['orphan_snapshots'] = [row for row in lists['snapshots'] if row['spec']['volume'] not in names]
            if record['orphan_snapshots']:
                record['errors'].append('snapshot CRDs name volumes absent from the volume census')
            for plural in ('engines', 'replicas'):
                record['orphan_' + plural] = [
                    {'name': row['metadata']['name'], 'uid': row['metadata']['uid'],
                     'volume': row['spec']['volumeName']}
                    for row in lists[plural] if row['spec']['volumeName'] not in names]
                if record['orphan_' + plural]:
                    record['errors'].append(plural + ' name volumes absent from the volume census')
            volumes = []
            for row in sorted(lists['volumes'], key=lambda r: r['metadata']['name']):
                name, status = row['metadata']['name'], row.get('status', {})
                claim = status.get('kubernetesStatus', {})
                snapshots = [snapshot(s['metadata']['name'], s.get('status', {}))
                             for s in sorted(lists['snapshots'], key=lambda s: s['metadata']['name'])
                             if s['spec']['volume'] == name]
                volumes.append({'name': name, 'uid': row['metadata']['uid'],
                    'claim': {'namespace': claim.get('namespace'), 'pvc': claim.get('pvcName')},
                    'state': status.get('state'), 'size_bytes': byte_count(row['spec'].get('size')),
                    'actual_size_bytes': byte_count(status.get('actualSize')),
                    'snapshot_crd_count': len(snapshots), 'snapshot_crds': snapshots,
                    'engines': [engine(e) for e in sorted(lists['engines'], key=lambda e: e['metadata']['name'])
                                if e['spec']['volumeName'] == name],
                    'replicas': [{'name': r['metadata']['name'], 'uid': r['metadata']['uid'],
                                  'node': r['spec'].get('nodeID'), 'disk': r['spec'].get('diskID')}
                                 for r in sorted(lists['replicas'], key=lambda r: r['metadata']['name'])
                                 if r['spec']['volumeName'] == name]})
            record['volumes'] = volumes
            record['state'] = 'unavailable' if record['errors'] else 'observed'
        except (ValueError, KeyError, TypeError) as error:
            record['errors'].append('native projection: ' + str(error))
    record['readings'] = recorder.rows
    return record


def publish(work, request, record, start, execute=bounded.native_command, clock=time.monotonic):
    raw = bounded.write_json(work / 'observation.json', record)
    manifest = {'schema': 1, 'claim': record['claim'], 'state': record['state'],
                'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest(), 'parts': []}
    for index, offset in enumerate(range(0, len(raw), 1024 * 1024)):
        part = work / ('snapshot-inventory-part-' + str(index) + '.json.bytes')
        block = raw[offset:offset + 1024 * 1024]
        part.write_bytes(block)
        receipt = attach(part, request, start, execute, clock)
        manifest['parts'].append({'name': part.name, 'bytes': len(block),
                                  'sha256': hashlib.sha256(block).hexdigest(), 'receipt': receipt})
    path = work / 'snapshot-inventory-manifest.json'
    bounded.write_json(path, manifest)
    return attach(path, request, start, execute, clock)


def attach(path, request, start, execute, clock):
    remaining = 180 - (clock() - start)
    if remaining <= 0:
        raise ValueError('attachment deadline reached; originals retained')
    code, out, err, complete = execute(
        ['boss', 'attach', request, str(path)], min(20, remaining))
    bounded.write_json(path.with_suffix(path.suffix + '.receipt.json'),
                       {'exit': code, 'stdout': bounded.byte_record(out),
                        'stderr': bounded.byte_record(err), 'complete': complete})
    if code or not complete or not out.strip():
        raise ValueError('native attachment not verified; originals retained')
    return out.decode('utf-8', errors='strict')


def summary(record, retained_original, receipt):
    counts = None if record['volumes'] is None else [
        {key: volume[key] for key in ('name', 'claim', 'size_bytes', 'actual_size_bytes', 'snapshot_crd_count')}
        | {'engine_caches': [{key: engine[key] for key in ('name', 'cache_state', 'snapshot_count')}
                              for engine in volume['engines']]}
        for volume in record['volumes']]
    return {'claim': record['claim'], 'state': record['state'], 'errors': record['errors'],
            'volume_count': len(record['volumes']) if record['volumes'] is not None else None,
            'volume_counts': counts, 'orphan_engines': record.get('orphan_engines'),
            'orphan_replicas': record.get('orphan_replicas'), 'retained_original': retained_original,
            'manifest_attachment_receipt': receipt}


def main():
    if len(sys.argv) != 1 or not re.fullmatch(bounded.UUID, os.environ.get('OPS_REQUEST_ID', '')):
        print('read-volume-snapshots: REFUSED — no arguments; native ops request required', file=sys.stderr)
        return 78
    request = os.environ['OPS_REQUEST_ID']
    work = Path(tempfile.mkdtemp(prefix='boss-volume-snapshots-' + request + '-'))
    os.chmod(work, 0o700)
    start = time.monotonic()
    def retain(row):
        with (work / 'readings.jsonl').open('a') as stream:
            stream.write(json.dumps(row, sort_keys=True) + '\n')
    recorder = bounded.Recorder(sink=retain)
    try:
        kubectl = shlex.split(os.environ['VOLUME_SNAPSHOTS_KUBECTL'])
        if not kubectl:
            raise ValueError('existing Kubernetes door unavailable')
        record = collect(recorder, kubectl)
        record.update(request=request, source={path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                      for path in (Path(__file__), Path(bounded.__file__), Path(__file__).with_name('read-volume-snapshots.sh'))})
        receipt = publish(work, request, record, start)
        print(json.dumps(summary(record, str(work), receipt), sort_keys=True))
        return 0 if record['state'] == 'observed' else 1
    except (ValueError, KeyError, TypeError, OSError, UnicodeError) as error:
        print(json.dumps({'claim': 'longhorn-snapshot-api-inventory-only', 'state': 'unavailable',
                          'error': str(error), 'retained_original': str(work)}, sort_keys=True))
        return 1


if __name__ == '__main__':
    sys.exit(main())
