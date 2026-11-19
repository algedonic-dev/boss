#!/usr/bin/env python3
"""Retained Longhorn CRD facts and UID-bound Events, never inferred rebuild cause.

Longhorn v1.11.3 replica.go documents failedAt/healthyAt as mutable latest
markers, not a complete history. engine.go exposes current rebuildStatus and
replicaTransitionTimeMap, not a durable rebuild start/end ledger:
https://github.com/longhorn/longhorn-manager/tree/v1.11.3/k8s/pkg/apis/longhorn/v1beta2
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

_spec = importlib.util.spec_from_file_location('replica_inventory_transport',
                                             Path(__file__).with_name('volume-snapshots.py'))
transport = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(transport)
bounded, BASE, KINDS = transport.bounded, transport.BASE, transport.KINDS
HISTORY_UNKNOWN = 'UNKNOWN: retained Events are not a complete rebuild history'


def identity(row):
    meta = row['metadata']
    return (row['apiVersion'], row['kind'], meta['namespace'], meta['name'], meta['uid'])


def event_identity(row):
    ref = row.get('involvedObject', {})
    if not isinstance(ref, dict):
        return None
    return tuple(ref.get(key) for key in ('apiVersion', 'kind', 'namespace', 'name', 'uid'))


def native_object(row, events):
    meta, spec, status = row['metadata'], row.get('spec', {}), row.get('status', {})
    name = spec.get('volumeName')
    owners = [ref.get('uid') for ref in meta.get('ownerReferences', [])
              if ref.get('apiVersion') == 'longhorn.io/v1beta2'
              and ref.get('kind') == 'Volume' and ref.get('name') == name
              and isinstance(ref.get('uid'), str) and re.fullmatch(bounded.UUID, ref['uid'])]
    return {'name': meta['name'], 'uid': meta['uid'], 'resource_version': meta.get('resourceVersion'),
            'created_at': meta.get('creationTimestamp'), 'current_state': status.get('currentState'),
            **({} if row['kind'] == 'Volume' else {
                'volume_reference': name, 'volume_owner_uid': owners[0] if len(owners) == 1 else None,
                'volume_identity_state': 'owner-reference-reported' if len(owners) == 1 else 'name-reference-only'}),
            'events': None if events is None else
                      [event for event in events if event_identity(event) == identity(row)],
            'history_completeness': HISTORY_UNKNOWN}


def collect(recorder, kubectl):
    record = {'schema': 1, 'claim': 'longhorn-replica-retained-history-only', 'state': 'unavailable',
              'coverage': 'Separate bounded API lists, not an atomic census. Latest CRD timestamps '
                          'can be cleared or overwritten; Events can expire. No inferred trigger, '
                          'exact rebuild start/end, offline health, bytes rebuilt or mutation authority.',
              'volumes': None, 'events_state': 'unavailable', 'errors': []}
    lists = {}
    for plural in ('volumes', 'replicas', 'engines', 'events'):
        try:
            lists[plural] = transport.inventory(recorder, kubectl, plural)
        except (ValueError, KeyError, TypeError, UnicodeError) as error:
            record['errors'].append(plural + ': ' + str(error))
    events = lists.get('events')
    if 'events' in lists:
        record['events_state'] = 'observed-retained-only'
    if all(plural in lists for plural in ('volumes', 'replicas', 'engines')):
        try:
            objects = lists['volumes'] + lists['replicas'] + lists['engines']
            identities = {identity(row) for row in objects}
            record['unmatched_events'] = None if events is None else [
                event for event in events if event_identity(event) not in identities]
            volume_ids = {row['metadata']['name']: row['metadata']['uid'] for row in lists['volumes']}
            for plural in ('replicas', 'engines'):
                record['orphan_' + plural] = [row for row in lists[plural]
                    if row.get('spec', {}).get('volumeName') not in volume_ids]
                if record['orphan_' + plural]:
                    record['errors'].append(plural + ': native volume reference absent from census')
            volumes = []
            for row in sorted(lists['volumes'], key=lambda value: value['metadata']['name']):
                volume = native_object(row, events)
                name, status = row['metadata']['name'], row.get('status', {})
                volume['claim'] = status.get('kubernetesStatus', {})
                volume['replicas'], volume['engines'] = [], []
                for plural in ('replicas', 'engines'):
                    for child in sorted(lists[plural], key=lambda value: value['metadata']['name']):
                        spec, status = child.get('spec', {}), child.get('status', {})
                        if spec.get('volumeName') != name:
                            continue
                        item = native_object(child, events)
                        if item['volume_owner_uid'] is not None and item['volume_owner_uid'] != row['metadata']['uid']:
                            record['errors'].append(plural + ': volume owner UID differs from census')
                            item['volume_identity_state'] = 'owner-uid-mismatch'
                        item['node'] = spec.get('nodeID')
                        if plural == 'replicas':
                            item.update(disk=spec.get('diskID'), engine_reference=spec.get('engineName'),
                                history={key: spec.get(key) for key in
                                         ('failedAt', 'lastFailedAt', 'healthyAt', 'lastHealthyAt')},
                                conditions=status.get('conditions'))
                        else:
                            item.update(rebuild_status=status.get('rebuildStatus'),
                                replica_address_map=spec.get('replicaAddressMap'),
                                replica_mode_map=status.get('replicaModeMap'),
                                replica_transition_time_map=status.get('replicaTransitionTimeMap'))
                        volume[plural].append(item)
                volumes.append(volume)
            record['volumes'] = volumes
            record['state'] = 'unavailable' if record['errors'] else 'observed'
        except (ValueError, KeyError, TypeError) as error:
            record['errors'].append('native history projection: ' + str(error))
    record['readings'] = recorder.rows
    return record


def main():
    request = os.environ.get('OPS_REQUEST_ID', '')
    if len(sys.argv) != 1 or not re.fullmatch(bounded.UUID, request):
        print('read-volume-replica-history: REFUSED — no arguments; native ops request required', file=sys.stderr)
        return 78
    work = Path(tempfile.mkdtemp(prefix='boss-replica-history-' + request + '-'))
    os.chmod(work, 0o700)
    start = time.monotonic()

    def retain(row):
        with (work / 'readings.jsonl').open('a') as stream:
            stream.write(json.dumps(row, sort_keys=True) + '\n')

    recorder = bounded.Recorder(sink=retain)
    try:
        kubectl = shlex.split(os.environ['VOLUME_REPLICA_HISTORY_KUBECTL'])
        if not kubectl:
            raise ValueError('existing Kubernetes door unavailable')
        record = collect(recorder, kubectl)
        record.update(request=request, source={path.name: hashlib.sha256(path.read_bytes()).hexdigest()
            for path in (Path(__file__), Path(transport.__file__), Path(bounded.__file__),
                         Path(__file__).with_name('read-volume-replica-history.sh'))})
        receipt = transport.publish(work, request, record, start, prefix='replica-history')
        print(json.dumps({'claim': record['claim'], 'state': record['state'], 'errors': record['errors'],
            'history_completeness': HISTORY_UNKNOWN, 'events_state': record['events_state'],
            'volume_count': None if record['volumes'] is None else len(record['volumes']),
            'retained_original': str(work), 'manifest_attachment_receipt': receipt}, sort_keys=True))
        return 0 if record['state'] == 'observed' else 1
    except (ValueError, KeyError, TypeError, OSError, UnicodeError) as error:
        print(json.dumps({'claim': 'longhorn-replica-retained-history-only', 'state': 'unavailable',
                          'error': str(error), 'retained_original': str(work)}, sort_keys=True))
        return 1


if __name__ == '__main__':
    sys.exit(main())
