"""Isolated native-adapter fixtures; no cluster, cleanup or credentials."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from urllib.parse import urlsplit, parse_qs

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('volume_snapshots', ROOT / 'volume-snapshots.py')
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)

VOL = 'pvc-11111111-1111-4111-8111-111111111111'
UID = '22222222-2222-4222-8222-222222222222'

def resource(kind, name, **fields):
    return {'apiVersion': 'longhorn.io/v1beta2', 'kind': kind,
            'metadata': {'name': name, 'namespace': 'longhorn-system', 'uid': UID}, **fields}

def fixture():
    return {
        'volumes': [resource('Volume', VOL, spec={'size': '73014444032'}, status={
            'state': 'attached', 'actualSize': 93522870272,
            'kubernetesStatus': {'namespace': 'boss', 'pvcName': 'pgdata-postgres-0'}})],
        'snapshots': [resource('Snapshot', 'old-snapshot', spec={'volume': VOL}, status={
            'size': 2147483648, 'creationTime': '2026-10-23T00:00:00Z',
            'readyToUse': True, 'markRemoved': False, 'userCreated': True})],
        'engines': [resource('Engine', VOL + '-e-0', spec={'volumeName': VOL}, status={
            'currentState': 'running', 'snapshotsError': '', 'snapshots': {
                'old-snapshot': {'name': 'old-snapshot', 'size': '2147483648',
                    'created': '2026-10-23T00:00:00Z', 'removed': False, 'usercreated': True},
                'volume-head': {'name': 'volume-head', 'size': '536870912', 'created': '',
                    'removed': False, 'usercreated': False}}})],
        'replicas': [resource('Replica', VOL + '-r-0', spec={
            'volumeName': VOL, 'nodeID': 'cp-1', 'diskID': 'disk-a'})]}

class Native:
    def __init__(self, data=None):
        self.data = fixture() if data is None else data
        self.calls = []
        self.fail = None
        self.empty = None
        self.partial = None
        self.paginate = False
        self.bad_rv = False
        self.repeat_token = False

    def execute(self, argv, timeout):
        self.calls.append(argv)
        assert argv[:3] == ['kubectl', 'get', '--raw'], argv
        assert argv[-1] == '--request-timeout=20s', argv
        path = urlsplit(argv[3])
        name = path.path.rsplit('/', 1)[-1]
        assert path.path == '/apis/longhorn.io/v1beta2/namespaces/longhorn-system/' + name
        assert set(parse_qs(path.query)) <= {'limit', 'continue'}
        if name == self.fail:
            return 1, b'', b'Forbidden: native inventory unavailable', True
        if name == self.empty:
            return 0, b'', b'', True
        rows = self.data[name]
        token = parse_qs(path.query).get('continue', [''])[0]
        meta = {'resourceVersion': 'different' if token and self.bad_rv else '123'}
        if self.paginate and name == 'snapshots':
            if not token:
                rows = []
                meta['continue'] = 'opaque/token+='  # must be encoded, never shell-expanded
            elif self.repeat_token:
                meta['continue'] = token
        if name == self.partial:
            meta['remainingItemCount'] = 1
        body = {'apiVersion': 'longhorn.io/v1beta2', 'kind': reader.KINDS[name] + 'List',
                'metadata': meta, 'items': rows}
        return 0, json.dumps(body).encode(), b'', True

    def collect(self):
        return reader.collect(reader.bounded.Recorder(execute=self.execute), ['kubectl'])

class Inventory(unittest.TestCase):
    def test_native_sized_engine_snapshot_maps_adapt_without_mixing_list_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory); script = work / 'kubectl.py'
            script.write_text('''import json,sys
from urllib.parse import urlsplit,parse_qs
query=parse_qs(urlsplit(sys.argv[3]).query)
limit=int(query['limit'][0]); token=query.get('continue',[''])[0]
if token:
    prior,offset=map(int,token.split(':'))
    if prior!=limit: sys.exit(9)
else: offset=0
rows=[]
for index in range(offset,min(offset+limit,12)):
    rows.append({'apiVersion':'longhorn.io/v1beta2','kind':'Engine','metadata': {
        'namespace':'longhorn-system','name':'engine-'+str(index),
        'uid':'22222222-2222-4222-8222-'+str(index).zfill(12)},
        'status':{'snapshots':{'system-snapshot': {'name':'system-snapshot','labels':{'padding':'x'*56160}}}}})
end=offset+len(rows)
print(json.dumps({'apiVersion':'longhorn.io/v1beta2','kind':'EngineList',
    'metadata':{'resourceVersion':str(limit),'continue':str(limit)+':'+str(end) if end<12 else '',
    'remainingItemCount':12-end},'items':rows}))
''')
            recorder = reader.bounded.Recorder()
            rows = reader.inventory(recorder, [sys.executable, str(script)], 'engines')
            self.assertEqual(len(rows), 12)
            self.assertEqual(len({row['metadata']['uid'] for row in rows}), 12)
            self.assertTrue(any(row['stream_complete'] is False for row in recorder.rows))
            self.assertEqual(recorder.bytes, sum(row[key]['bytes'] for row in recorder.rows for key in ('stdout', 'stderr')))
            self.assertLess(len(recorder.rows), reader.bounded.MAX_READS)

    def test_snapshot_buildup_and_engine_roster_fit_the_existing_total_bounds(self):
        data = fixture()
        data['snapshots'] = [resource('Snapshot', 'snapshot-' + str(i), spec={'volume': VOL},
                                     status={'size': 1, 'creationTime': '2026-10-23T00:00:00Z', 'padding': 'x' * 1200}) for i in range(400)]
        data['engines'] = [resource('Engine', 'engine-' + str(i), spec={'volumeName': VOL},
                                   status={'currentState': 'running', 'snapshotsError': '', 'snapshots': {}}) for i in range(80)]
        calls = []
        def execute(argv, timeout):
            calls.append(argv); path = urlsplit(argv[3]); plural = path.path.rsplit('/', 1)[-1]
            query = parse_qs(path.query); limit = int(query['limit'][0]); offset = int(query.get('continue', ['0'])[0])
            rows = data[plural][offset:offset + limit]; meta = {'resourceVersion': '123'}
            if offset + limit < len(data[plural]): meta['continue'] = str(offset + limit)
            raw = json.dumps({'apiVersion': 'longhorn.io/v1beta2', 'kind': reader.KINDS[plural] + 'List', 'metadata': meta, 'items': rows}).encode()
            self.assertLessEqual(len(raw), 128 * 1024)
            return 0, raw, b'', True
        result = reader.collect(reader.bounded.Recorder(execute=execute), ['kubectl'])
        self.assertEqual(result['state'], 'observed', result['errors'])
        self.assertEqual(result['volumes'][0]['snapshot_crd_count'], 400)
        self.assertEqual(len(result['volumes'][0]['engines']), 80)
        self.assertLess(len(calls), 20)

    def test_empty_repeated_cursor_refuses_on_second_page_without_duplicates(self):
        native = Native(); native.data['snapshots'] = []; native.paginate = True; native.repeat_token = True
        result = native.collect()
        self.assertEqual(result['state'], 'unavailable')
        self.assertTrue(any('repeated continuation' in error for error in result['errors']))
        self.assertEqual(sum('/snapshots?' in call[3] for call in native.calls), 2)

    def test_orphan_engines_and_replicas_reach_operator_summary(self):
        data = fixture()
        for kind in ['engines', 'replicas']: data[kind][0]['spec']['volumeName'] = 'missing-volume'
        record = Native(data).collect()
        self.assertEqual(record['state'], 'unavailable')
        view = reader.summary(record, 'retained', 'acknowledged')
        self.assertEqual(view['orphan_engines'][0]['name'], data['engines'][0]['metadata']['name'])
        self.assertEqual(view['orphan_replicas'][0]['name'], data['replicas'][0]['metadata']['name'])
        self.assertEqual(view['orphan_engines'][0]['volume'], 'missing-volume')

    def test_lists_real_snapshot_sizes_and_separates_head(self):
        native = Native()
        result = native.collect()
        self.assertEqual(result['state'], 'observed')
        volume = result['volumes'][0]
        self.assertEqual(volume['claim'], {'namespace': 'boss', 'pvc': 'pgdata-postgres-0'})
        self.assertEqual(volume['snapshot_crd_count'], 1)
        self.assertEqual(volume['snapshot_crds'][0]['size_bytes'], 2147483648)
        self.assertEqual(volume['engines'][0]['snapshot_count'], 1)
        self.assertEqual(volume['engines'][0]['head']['size_bytes'], 536870912)
        self.assertEqual(volume['replicas'][0]['node'], 'cp-1')
        self.assertNotIn('reclaimable_bytes', volume)
        self.assertTrue(all('get' in call and not any(x in call for x in ['delete', 'patch', 'exec']) for call in native.calls))

    def test_complete_empty_census_is_distinct_from_unread(self):
        self.assertEqual(Native({key: [] for key in fixture()}).collect()['volumes'], [])
        native = Native(); native.empty = 'snapshots'
        result = native.collect()
        self.assertEqual(result['state'], 'unavailable')
        self.assertNotEqual(result.get('volumes'), [])

    def test_failed_read_keeps_later_independent_channels(self):
        native = Native(); native.fail = 'snapshots'
        result = native.collect()
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(len(native.calls), 4)
        self.assertEqual(len(result['readings']), 4)
        self.assertIn('Forbidden', reader.bounded.decode_record(result['readings'][1]['stderr']).decode())

    def test_pages_are_complete_and_opaque_token_is_encoded(self):
        native = Native(); native.paginate = True
        result = native.collect()
        self.assertEqual(result['state'], 'observed')
        self.assertEqual(result['volumes'][0]['snapshot_crd_count'], 1)
        self.assertTrue(any('continue=opaque%2Ftoken%2B%3D' in call[3] for call in native.calls))

    def test_repeated_cursor_and_changed_snapshot_version_refuse(self):
        for change in ['repeat_token', 'bad_rv']:
            native = Native(); native.paginate = True; setattr(native, change, True)
            self.assertEqual(native.collect()['state'], 'unavailable', change)

    def test_missing_continuation_with_remaining_items_is_not_empty(self):
        native = Native(); native.partial = 'snapshots'
        self.assertEqual(native.collect()['state'], 'unavailable')

    def test_duplicate_or_wrong_scope_identity_refuses(self):
        for mutate in [lambda d: d['snapshots'].append(copy.deepcopy(d['snapshots'][0])),
                       lambda d: d['snapshots'][0]['metadata'].update(namespace='other'),
                       lambda d: d['snapshots'][0].update(kind='Volume')]:
            data = fixture(); mutate(data)
            self.assertEqual(Native(data).collect()['state'], 'unavailable')

    def test_missing_or_negative_bytes_are_not_zero(self):
        for value in [None, -1, True, 'bad']:
            data = fixture(); data['snapshots'][0]['status']['size'] = value
            result = Native(data).collect()
            self.assertIsNone(result['volumes'][0]['snapshot_crds'][0]['size_bytes'])
            self.assertIn('unavailable', result['volumes'][0]['snapshot_crds'][0]['size_state'])

    def test_detached_cache_is_explicitly_unavailable_not_empty_chain(self):
        data = fixture(); data['volumes'][0]['status']['state'] = 'detached'
        data['engines'][0]['status'].update(currentState='stopped', snapshots={})
        result = Native(data).collect()
        engine = result['volumes'][0]['engines'][0]
        self.assertIsNone(engine['snapshot_count'])
        self.assertEqual(engine['cache_state'], 'unavailable')
        self.assertEqual(result['volumes'][0]['snapshot_crd_count'], 1)

    def test_engine_error_does_not_claim_current_snapshot_count(self):
        data = fixture(); data['engines'][0]['status']['snapshotsError'] = 'engine timeout'
        engine = Native(data).collect()['volumes'][0]['engines'][0]
        self.assertEqual(engine['cache_state'], 'unavailable')
        self.assertIsNone(engine['snapshot_count'])
        self.assertEqual(engine['error'], 'engine timeout')

    def test_orphan_snapshot_is_retained_not_dropped(self):
        data = fixture(); data['snapshots'][0]['spec']['volume'] = 'missing-volume'
        result = Native(data).collect()
        self.assertEqual(len(result['orphan_snapshots']), 1)
        self.assertEqual(result['state'], 'unavailable')

    def test_native_partial_stream_is_not_success(self):
        native = Native()
        execute = native.execute
        native.execute = lambda argv, timeout: (*execute(argv, timeout)[:3], False)
        self.assertEqual(native.collect()['state'], 'unavailable')

    def test_total_budget_is_not_a_fabricated_empty_inventory(self):
        native = Native()
        now = [0]
        def clock():
            now[0] += 151
            return now[0]
        result = reader.collect(reader.bounded.Recorder(execute=native.execute, clock=clock), ['kubectl'])
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(native.calls, [])
        self.assertEqual(len(result['readings']), 4)

    def test_publisher_retains_complete_original_and_verifies_every_part(self):
        calls = []
        def execute(argv, seconds):
            self.assertEqual(argv[:3], ['boss', 'attach', UID])
            calls.append((Path(argv[3]).name, Path(argv[3]).read_bytes()))
            self.assertLessEqual(seconds, 20)
            return 0, b'fixture-only native attach acknowledgement', b'', True
        record = {'claim': 'fixture', 'state': 'observed', 'payload': 'x' * 1_100_000}
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            reader.publish(work, UID, record, 0, execute=execute, clock=lambda: 1)
            self.assertEqual(len(calls), 3)
            self.assertEqual(calls[0][1] + calls[1][1], (work / 'observation.json').read_bytes())
            self.assertEqual(json.loads(calls[2][1])['bytes'], len(calls[0][1]) + len(calls[1][1]))
            self.assertEqual(len(list(work.glob('*.receipt.json'))), 3)

    def test_failed_or_incomplete_attachment_keeps_local_original(self):
        for exit_code, complete, stdout in [(1, True, b'partial acknowledgement'),
                                            (0, False, b'partial acknowledgement'), (0, True, b'')]:
            def execute(argv, seconds):
                return exit_code, stdout, b'attachment failed', complete
            with tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                with self.assertRaisesRegex(ValueError, 'not verified'):
                    reader.publish(work, UID, {'claim': 'fixture', 'state': 'unavailable'},
                                   0, execute=execute, clock=lambda: 1)
                self.assertTrue((work / 'observation.json').exists())
                self.assertEqual(json.loads(next(work.glob('*.receipt.json')).read_text())['complete'], complete)

if __name__ == '__main__':
    unittest.main()
