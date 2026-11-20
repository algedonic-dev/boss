"""Native-shaped read-only history fixtures; no cluster or credentials."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[1]
target = sys.argv.pop(1) if len(sys.argv) > 1 else 'volume-replica-history.py'
spec = importlib.util.spec_from_file_location('history_reader', ROOT / target)
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)
VOLUME = 'pvc-11111111-1111-4111-8111-111111111111'
VUID = '11111111-1111-4111-8111-111111111111'
RUID = '22222222-2222-4222-8222-222222222222'
EUID = '33333333-3333-4333-8333-333333333333'


def obj(kind, name, uid, **fields):
    return {'apiVersion': 'longhorn.io/v1beta2', 'kind': kind,
            'metadata': {'namespace': 'longhorn-system', 'name': name, 'uid': uid}, **fields}


def event(uid=RUID, name='replica-a'):
    return {'metadata': {
        'namespace': 'longhorn-system', 'name': 'failure.1',
        'uid': '44444444-4444-4444-8444-444444444444'},
        'involvedObject': {'apiVersion': 'longhorn.io/v1beta2', 'kind': 'Replica',
                           'namespace': 'longhorn-system', 'name': name, 'uid': uid},
        'reason': 'FailedRebuilding', 'message': 'native failure text, not inferred cause',
        'firstTimestamp': '2026-10-29T17:34:00Z', 'lastTimestamp': '2026-10-29T17:35:00Z',
        'count': 2, 'type': 'Warning'}


def fixture():
    return {'volumes': [obj('Volume', VOLUME, VUID, spec={'size': '10'}, status={
        'state': 'attached', 'kubernetesStatus': {'namespace': 'boss', 'pvcName': 'pgdata'}})],
        'replicas': [obj('Replica', 'replica-a', RUID, spec={
            'volumeName': VOLUME, 'engineName': 'engine-a', 'nodeID': 'cp-2', 'diskID': 'disk-a',
            'failedAt': '', 'lastFailedAt': '2026-10-29T17:34:00Z',
            'healthyAt': '2026-10-29T20:37:00Z', 'lastHealthyAt': '2026-10-29T20:37:00Z'},
            status={'currentState': 'running', 'conditions': {'RebuildFailed': {
                'status': 'False', 'lastTransitionTime': '2026-10-29T20:37:00Z'}}})],
        'engines': [obj('Engine', 'engine-a', EUID, spec={
            'volumeName': VOLUME, 'replicaAddressMap': {'replica-a': 'tcp://10.0.0.1:10000'}},
            status={'currentState': 'running', 'replicaModeMap': {'replica-a': 'RW'},
                    'replicaTransitionTimeMap': {'replica-a': '2026-10-29T20:36:58Z'},
                    'rebuildStatus': {'tcp://10.0.0.1:10000': {'isRebuilding': False,
                        'progress': 100, 'state': 'complete', 'error': ''}}})],
        'snapshots': [], 'events': [event(), {
            **event(EUID, 'engine-a'), 'metadata': {'namespace': 'longhorn-system',
                'name': 'rebuilding.2', 'uid': '55555555-5555-4555-8555-555555555555'},
            'involvedObject': {'apiVersion': 'longhorn.io/v1beta2', 'kind': 'Engine',
                'namespace': 'longhorn-system', 'name': 'engine-a', 'uid': EUID},
            'reason': 'Rebuilt', 'message': 'Replica replica-a has been rebuilt'}]}


class Native:
    def __init__(self, data=None):
        self.data = fixture() if data is None else data
        self.calls = []
        self.fail = None
        self.paginate = False
        self.repeat = False
        self.bad_version = False

    def execute(self, argv, timeout):
        self.calls.append(argv)
        assert argv[:3] == ['kubectl', 'get', '--raw'], argv
        assert argv[-1] == '--request-timeout=20s'
        path = urlsplit(argv[3]); plural = path.path.rsplit('/', 1)[-1]
        base = '/api/v1/namespaces/longhorn-system/' if plural == 'events' else reader.BASE
        assert path.path == base + plural
        query = parse_qs(path.query)
        assert set(query) <= {'limit', 'continue'}
        if plural == self.fail:
            return 1, b'', b'Forbidden: event history unread', True
        token = query.get('continue', [''])[0]
        rows = self.data[plural]
        metadata = {'resourceVersion': 'different' if token and self.bad_version else '123'}
        if plural == 'events' and self.paginate:
            if not token:
                rows = []; metadata['continue'] = 'opaque/token+'
            elif self.repeat:
                metadata['continue'] = token
        kind = 'Event' if plural == 'events' else reader.KINDS[plural]
        return 0, json.dumps({'apiVersion': 'v1' if plural == 'events' else 'longhorn.io/v1beta2',
            'kind': kind + 'List', 'metadata': metadata, 'items': rows}).encode(), b'', True

    def collect(self):
        return reader.collect(reader.bounded.Recorder(execute=self.execute), ['kubectl'])


class History(unittest.TestCase):
    def test_native_failure_and_recovery_fields_and_uid_bound_events_survive(self):
        native = Native(); record = native.collect()
        self.assertEqual(record['state'], 'observed', record['errors'])
        volume = record['volumes'][0]; replica = volume['replicas'][0]
        self.assertEqual(replica['node'], 'cp-2'); self.assertEqual(replica['disk'], 'disk-a')
        self.assertEqual(replica['history']['lastFailedAt'], '2026-10-29T17:34:00Z')
        self.assertEqual(replica['history']['healthyAt'], '2026-10-29T20:37:00Z')
        self.assertEqual(replica['events'][0]['reason'], 'FailedRebuilding')
        self.assertEqual(replica['events'][0]['involvedObject']['uid'], RUID)
        self.assertEqual(volume['engines'][0]['rebuild_status']['tcp://10.0.0.1:10000']['progress'], 100)
        self.assertEqual(volume['engines'][0]['events'][0]['reason'], 'Rebuilt')
        self.assertEqual(len(replica['events']), 1, 'engine prose must not become replica UID evidence')
        self.assertEqual(replica['history_completeness'], 'UNKNOWN: retained Events are not a complete rebuild history')
        self.assertNotIn('rebuild_start', replica)
        self.assertNotIn('inferred_cause', replica)
        self.assertTrue(any('/events?' in call[3] for call in native.calls))

    def test_recreated_name_and_wrong_kind_namespace_or_missing_uid_do_not_join(self):
        for change in ({'uid': VUID}, {'uid': ''}, {'kind': 'Volume'}, {'namespace': 'other'}):
            with self.subTest(change=change):
                data = fixture(); data['events'][0]['involvedObject'].update(change)
                record = Native(data).collect()
                self.assertEqual(record['volumes'][0]['replicas'][0]['events'], [])
                self.assertEqual(len(record['unmatched_events']), 1)

    def test_missing_events_is_unknown_not_no_rebuild_and_failed_channel_keeps_crds(self):
        data = fixture(); data['events'] = []
        record = Native(data).collect()
        self.assertEqual(record['state'], 'observed')
        self.assertEqual(record['unmatched_events'], [])
        for volume in record['volumes']:
            for item in [volume, *volume['replicas'], *volume['engines']]:
                self.assertEqual(item['events'], [])
        self.assertTrue(record['volumes'][0]['replicas'][0]['history_completeness'].startswith('UNKNOWN'))
        native = Native(); native.fail = 'events'; record = native.collect()
        self.assertEqual(record['state'], 'unavailable')
        self.assertEqual(record['volumes'][0]['replicas'][0]['history']['lastFailedAt'], '2026-10-29T17:34:00Z')
        self.assertEqual(record['events_state'], 'unavailable')
        self.assertIsNone(record['unmatched_events'])
        for volume in record['volumes']:
            for item in [volume, *volume['replicas'], *volume['engines']]:
                self.assertIsNone(item['events'])
        self.assertIn('Forbidden', reader.bounded.decode_record(record['readings'][-1]['stderr']).decode())

    def test_complete_events_pages_and_changed_or_repeated_cursor_refuse(self):
        native = Native(); native.paginate = True
        self.assertEqual(native.collect()['state'], 'observed')
        for flag in ('repeat', 'bad_version'):
            native = Native(); native.paginate = True; setattr(native, flag, True)
            self.assertEqual(native.collect()['state'], 'unavailable')

    def test_typed_event_items_allow_absent_or_matching_typemeta_only(self):
        for fields in ({}, {'apiVersion': 'v1', 'kind': 'Event'}):
            data = fixture()
            for row in data['events']:
                row.update(fields)
            record = Native(data).collect()
            self.assertEqual(record['state'], 'observed', record['errors'])
            raw = json.loads(reader.bounded.decode_record(record['readings'][-1]['stdout']))
            self.assertEqual(raw['items'], data['events'], 'normalization must preserve raw evidence')
            self.assertEqual(record['volumes'][0]['replicas'][0]['events'][0]['involvedObject']['uid'], RUID)
        for fields in ({'apiVersion': 'other/v1'}, {'kind': 'Replica'}):
            data = fixture(); data['events'][0].update(fields)
            record = Native(data).collect()
            self.assertEqual(record['events_state'], 'unavailable')
        for key in ('apiVersion', 'kind'):
            data = fixture(); del data['replicas'][0][key]
            record = Native(data).collect()
            self.assertEqual(record['state'], 'unavailable')
            self.assertIsNone(record['volumes'])

    def test_volume_identity_is_not_labelled_as_its_own_child_reference(self):
        volume = Native().collect()['volumes'][0]
        for key in ('volume_reference', 'volume_owner_uid', 'volume_identity_state'):
            self.assertNotIn(key, volume)

    def test_volume_name_reference_is_not_a_fabricated_owner_uid(self):
        record = Native().collect(); replica = record['volumes'][0]['replicas'][0]
        self.assertEqual(replica['volume_reference'], VOLUME)
        self.assertIsNone(replica['volume_owner_uid'])
        self.assertEqual(replica['volume_identity_state'], 'name-reference-only')


if __name__ == '__main__':
    unittest.main()
