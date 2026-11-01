"""Native-shaped ports for the fixed, passkey-bound purge-only operation."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('purge', Path(__file__).with_name('instance-snapshot-purge.py'))
purge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(purge)

IMAGE = 'longhornio/longhorn-engine:v1.11.3'
VOL = 'pvc-11111111-1111-4111-8111-111111111111'


def obj(kind, name, namespace='longhorn-system', **rest):
    api = 'v1' if kind in ('PersistentVolumeClaim', 'PersistentVolume', 'Pod') else 'batch/v1' if kind == 'Job' else 'longhorn.io/v1beta2'
    return {'kind': kind, 'apiVersion': api,
            'metadata': {'name': name, 'namespace': namespace,
                         'uid': '11111111-1111-4111-8111-' + str(len(name)).zfill(12)}, **rest}


def fixture():
    p = obj('PersistentVolumeClaim', 'db', 'boss', spec={'volumeName': VOL}, status={'phase': 'Bound'})
    pv = obj('PersistentVolume', VOL, '', spec={'claimRef': {'name': 'db', 'namespace': 'boss', 'uid': p['metadata']['uid']},
                                             'csi': {'driver': 'driver.longhorn.io', 'volumeHandle': VOL}})
    v = obj('Volume', VOL, spec={'dataEngine': 'v1', 'image': IMAGE, 'numberOfReplicas': 1,
                                'nodeID': 'cp-1', 'migrationNodeID': '', 'Standby': False},
            status={'state': 'attached', 'robustness': 'healthy', 'currentImage': IMAGE,
                    'currentNodeID': 'cp-1', 'currentMigrationNodeID': '', 'restoreRequired': False,
                    'restoreInitiated': False, 'isStandby': False,
                    'kubernetesStatus': {'namespace': 'boss', 'pvcName': 'db'}})
    r = obj('Replica', 'replica-one', spec={'volumeName': VOL, 'nodeID': 'cp-1', 'diskID': 'disk-one',
                                         'active': True, 'healthyAt': '2026-10-01T00:00:00Z', 'failedAt': ''},
            status={'currentState': 'running'})
    e = obj('Engine', 'engine-one', spec={'volumeName': VOL, 'image': IMAGE, 'active': True, 'dataEngine': 'v1',
                                         'nodeID': 'cp-1', 'requestedBackupRestore': '', 'replicaAddressMap': {'replica-one': '10.0.0.1:10000'}},
            status={'currentImage': IMAGE, 'currentState': 'running', 'currentReplicaAddressMap': {'replica-one': '10.0.0.1:10000'},
                    'replicaModeMap': {'replica-one': 'RW'}, 'backupStatus': {}, 'restoreStatus': {},
                    'rebuildStatus': {}, 'cloneStatus': {}, 'purgeStatus': {}, 'snapshotsError': ''})
    snapshots = [{'name': 'volume-head', 'parent': 'system-one', 'children': {}, 'removed': False,
                  'usercreated': False, 'created': '', 'size': '100', 'labels': {}},
                 {'name': 'system-one', 'parent': '', 'children': {'volume-head': True}, 'removed': False,
                  'usercreated': False, 'created': '2026-10-01T00:00:00Z', 'size': '1000', 'labels': {}}]
    return {'pvc': p, 'pv': pv, 'volume': v, 'replicas': [r], 'engines': [e],
            'snapshots': snapshots, 'snapshot_crs': [], 'backups': [], 'jobs': [], 'pods': [], 'nodes': []}


class Purge(unittest.TestCase):
    def test_an_oversized_single_object_still_refuses_without_lifting_the_byte_cap(self):
        import sys
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            native = purge.Native([sys.executable, '-c', 'print("x" * 200000)'], Path(directory))
            with self.assertRaisesRegex(ValueError, 'one native list object exceeds'):
                native.listing('pods', 'Pod', 'boss')
            self.assertEqual(native.index, 3)
            self.assertEqual(native.total, 3 * purge.bounded.SCALAR_BYTES)
            for path in Path(directory).glob('native-*.json'):
                record = json.loads(path.read_text())
                self.assertFalse(record['stream_complete'])
                self.assertEqual(record['stdout']['bytes'], purge.bounded.SCALAR_BYTES)

    def test_an_ordinary_native_error_is_retained_and_never_retried_as_a_smaller_page(self):
        import sys
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            native = purge.Native([sys.executable, '-c', 'import sys; print("original refusal", file=sys.stderr); sys.exit(7)'], Path(directory))
            with self.assertRaisesRegex(ValueError, 'original refusal'):
                native.listing('jobs', 'Job', 'boss')
            self.assertEqual(native.index, 1)
            record = json.loads((Path(directory) / 'native-1.json').read_text())
            self.assertEqual(record['exit'], 7)
            self.assertTrue(record['stream_complete'])

    def test_live_sized_late_page_restarts_the_whole_census_with_a_fixed_limit(self):
        import sys
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            fixture = work / 'kubectl.py'
            fixture.write_text('''import json, sys
from urllib.parse import urlsplit, parse_qs
url = urlsplit(sys.argv[3]); query = parse_qs(url.query)
kind, api = ('Pod','v1') if url.path.endswith('/pods') else ('Job','batch/v1')
limit = int(query['limit'][0]); token = query.get('continue', [''])[0]
if token:
    prior_limit, offset = map(int, token.split(':'))
    if prior_limit != limit:
        print('continuation query changed', file=sys.stderr); sys.exit(1)
else:
    offset = 0
rows = []
for index in range(offset, min(offset + limit, 57)):
    rows.append({'metadata': {'name': 'pod-' + str(index), 'namespace': 'boss',
        'uid': '11111111-1111-4111-8111-' + str(index).zfill(12),
        'annotations': {'attempt': str(limit), 'padding': 'x' * (56160 if index in (16,17,18) else 2000)}}})
end = offset + len(rows)
print(json.dumps({'apiVersion':api,'kind':kind+'List','metadata': {
    'resourceVersion': str(limit), 'continue': str(limit)+':'+str(end) if end < 57 else '',
    'remainingItemCount': 57-end}, 'items':rows}))
''')
            native = purge.Native([sys.executable, str(fixture)], work)
            rows = native.listing('pods', 'Pod', 'boss')
            self.assertEqual(len(rows), 57)
            self.assertEqual(len({row['metadata']['uid'] for row in rows}), 57)
            self.assertEqual({row['metadata']['annotations']['attempt'] for row in rows}, {'2'})
            records = [json.loads(path.read_text()) for path in work.glob('native-*.json')]
            records.sort(key=lambda row: row['sequence'])
            self.assertTrue(any(not row['stream_complete'] for row in records))
            failed = next(index for index, row in enumerate(records) if not row['stream_complete'])
            self.assertIn('limit=2', records[failed + 1]['argv'][4])
            self.assertNotIn('continue=', records[failed + 1]['argv'][4])
            self.assertEqual(native.index, len(records))
            self.assertEqual(native.total, sum(row[key]['bytes'] for row in records for key in ('stdout', 'stderr')))
            self.assertLess(native.index * 2 + 300, 512)
            self.assertLess(native.total * 2, 8 * 1024 * 1024)
            # Same native-sized failure class applies to Jobs; these are full
            # transport reads, not a replacement of Native.call or its limiter.
            job_work = work / 'jobs'; job_work.mkdir()
            jobs = purge.Native([sys.executable, str(fixture)], job_work)
            self.assertEqual(len(jobs.listing('jobs', 'Job', 'boss')), 57)
            self.assertLess(2 * (native.index + jobs.index) + 300, 512)
            self.assertLess(2 * (native.total + jobs.total), 8 * 1024 * 1024)

    def test_typed_pod_and_job_lists_have_typemeta_only_on_the_list(self):
        import tempfile
        for resource, kind, api in [('pods', 'Pod', 'v1'), ('jobs', 'Job', 'batch/v1')]:
            row = obj(kind, 'listed', 'boss'); del row['kind']; del row['apiVersion']
            page = {'kind': kind + 'List', 'apiVersion': api,
                    'metadata': {'resourceVersion': '123'}, 'items': [row]}
            with tempfile.TemporaryDirectory() as directory:
                native = purge.Native(['fixture-kubectl'], Path(directory)); native.call = lambda _: page
                rows = native.listing(resource, kind, 'boss')
                self.assertEqual(rows[0]['metadata'], row['metadata'])
                bad = copy.deepcopy(page); bad['apiVersion'] = 'wrong'; native.call = lambda _: bad
                with self.assertRaises(ValueError): native.listing(resource, kind, 'boss')

    def test_smaller_workload_pages_do_not_make_snapshot_buildup_unreadable(self):
        import tempfile
        from urllib.parse import urlsplit, parse_qs
        rows = [obj('Snapshot', 'snapshot-' + str(i)) for i in range(400)]
        for i, row in enumerate(rows): row['metadata']['uid'] = '11111111-1111-4111-8111-' + str(i).zfill(12)
        calls = []
        def page(argv):
            query = parse_qs(urlsplit(argv[2]).query); size = int(query['limit'][0]); offset = int(query.get('continue', ['0'])[0])
            calls.append(argv); meta = {'resourceVersion': '123'}
            if offset + size < len(rows): meta['continue'] = str(offset + size)
            result = {'apiVersion': 'longhorn.io/v1beta2', 'kind': 'SnapshotList', 'metadata': meta, 'items': rows[offset:offset + size]}
            self.assertLess(len(json.dumps(result).encode()), 128 * 1024)
            return result
        with tempfile.TemporaryDirectory() as directory:
            native = purge.Native(['fixture-kubectl'], Path(directory)); native.call = page
            self.assertEqual(len(native.listing('snapshots.longhorn.io', 'Snapshot', 'longhorn-system')), 400)
            self.assertLessEqual(len(calls), 32)

    def test_post_action_uid_swap_and_malformed_health_are_unproven(self):
        b = fixture(); approved = purge.digest(purge.render(b, 'boss', 'db'))
        moved = copy.deepcopy(b); moved['replicas'][0]['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        reads = iter([b, b, moved]); actions = []
        with self.assertRaisesRegex(purge.Unproven, 'identity moved'):
            purge.execute(lambda: next(reads), actions.append, 'boss', 'db', approved)
        self.assertEqual(actions, [VOL])
        malformed = copy.deepcopy(b)
        malformed['volume']['status'] = None
        reads = iter([b, b, malformed]); actions = []
        with self.assertRaises(purge.Unproven):
            purge.execute(lambda: next(reads), actions.append, 'boss', 'db', approved)
        self.assertEqual(actions, [VOL])

    def test_completion_poll_reads_only_fixed_target_objects(self):
        import tempfile
        b = fixture(); plan = purge.render(b, 'boss', 'db')
        rows = {('pvc', 'db'): b['pvc'], ('pv', VOL): b['pv'],
                ('volumes.longhorn.io', VOL): b['volume'],
                ('engines.longhorn.io', 'engine-one'): b['engines'][0],
                ('replicas.longhorn.io', 'replica-one'): b['replicas'][0]}
        with tempfile.TemporaryDirectory() as directory:
            native = purge.Native(['fixture-kubectl'], Path(directory)); calls = []
            def get(resource, name, namespace):
                calls.append((resource, name, namespace)); return copy.deepcopy(rows[(resource, name)])
            native.get = get
            result = native.poll(plan)
            self.assertEqual(result['volume'], b['volume'])
            self.assertEqual(len(calls), 5)
            self.assertNotIn('snapshots', result)
            self.assertNotIn('backups', result)
            self.assertNotIn('jobs', result)
            self.assertNotIn('pods', result)

    def test_ten_minute_completion_keeps_the_existing_native_call_and_byte_bounds(self):
        import tempfile
        b = fixture(); plan = purge.render(b, 'boss', 'db'); now = [0]; engine_reads = [0]
        objects = {('pvc', 'db'): b['pvc'], ('pv', VOL): b['pv'],
                   ('volumes.longhorn.io', VOL): b['volume'],
                   ('engines.longhorn.io', 'engine-one'): b['engines'][0],
                   ('replicas.longhorn.io', 'replica-one'): b['replicas'][0]}
        def command(argv, timeout):
            self.assertEqual(argv[1], 'get')
            row = copy.deepcopy(objects[(argv[2], argv[3])])
            if argv[2] == 'engines.longhorn.io':
                engine_reads[0] += 1; complete = engine_reads[0] == 61
                row['status']['snapshots'] = {'observed-cache': 'x' * 50000}
                row['status']['purgeStatus'] = {'tcp://10.0.0.1:10000': {
                    'isPurging': not complete, 'state': 'complete' if complete else 'in_progress',
                    'progress': 100 if complete else 1, 'error': ''}}
            return 0, json.dumps(row).encode(), b'', True
        def sleep(seconds): now[0] += seconds
        with tempfile.TemporaryDirectory() as directory:
            native = purge.Native(['fixture-kubectl'], Path(directory))
            native.total = 2 * 1024 * 1024  # already-recorded pre-action inventory budget fixture
            original = purge.bounded.native_command
            try:
                purge.bounded.native_command = command
                result = purge.execute(lambda: b, lambda _: None, 'boss', 'db', purge.digest(plan),
                                       poll=native.poll, sleep=sleep, clock=lambda: now[0])
            finally:
                purge.bounded.native_command = original
            self.assertEqual(result['state'], 'purge-complete')
            self.assertEqual(now[0], 600)
            self.assertEqual(native.index, 305)
            self.assertLess(native.total, 8 * 1024 * 1024)

    def test_plan_is_purge_only_and_freezes_identity_not_head_bytes(self):
        b = fixture(); original = copy.deepcopy(b)
        plan = purge.render(b, 'boss', 'db')
        self.assertEqual(b, original)
        self.assertEqual(plan['operation'], 'snapshotPurge')
        self.assertEqual([s['name'] for s in plan['eligible']], ['system-one'])
        b['snapshots'][0]['size'] = '99999'
        self.assertEqual(plan, purge.render(b, 'boss', 'db'))
        b['volume']['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        self.assertNotEqual(plan, purge.render(b, 'boss', 'db'))

    def test_no_head_user_or_backup_bound_deletion(self):
        b = fixture(); b['snapshots'][1]['usercreated'] = True
        with self.assertRaisesRegex(ValueError, 'no eligible'): purge.render(b, 'boss', 'db')
        b['snapshots'][1]['removed'] = True
        b['backups'] = [obj('Backup', 'backup-one', spec={'snapshotName': 'system-one'},
                            status={'volumeName': VOL, 'snapshotName': 'system-one', 'state': 'Completed', 'progress': 100})]
        with self.assertRaisesRegex(ValueError, 'backup-bound'): purge.render(b, 'boss', 'db')

    def test_engine_version_is_per_volume(self):
        for path in [('volume', 'spec', 'image'), ('volume', 'status', 'currentImage'), ('engines', 'status', 'currentImage')]:
            b = fixture(); row = b[path[0]][0] if path[0] == 'engines' else b[path[0]]
            row[path[1]][path[2]] = 'longhornio/longhorn-engine:v1.11.2'
            with self.assertRaisesRegex(ValueError, '1.11.3'): purge.render(b, 'boss', 'db')

    def test_reject_unknown_or_active_native_operations(self):
        for key, value in [('backupStatus', {'x': {'state': 'in_progress', 'progress': 1}}),
                           ('backupStatus', {'x': {'state': 'mystery', 'progress': 0}}),
                           ('rebuildStatus', {'x': {'isRebuilding': True}}),
                           ('restoreStatus', {'x': {'isRestoring': True}}),
                           ('cloneStatus', {'x': {'isCloning': True}}),
                           ('purgeStatus', {'x': {'isPurging': True}})]:
            b = fixture(); b['engines'][0]['status'][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError): purge.render(b, 'boss', 'db')
        b = fixture(); del b['engines'][0]['status']['backupStatus']
        with self.assertRaises(ValueError): purge.render(b, 'boss', 'db')

    def test_binding_health_and_complete_replica_set(self):
        for change in [lambda b: b['pv']['spec']['claimRef'].update(uid='other'),
                       lambda b: b['volume']['status'].update(robustness='degraded'),
                       lambda b: b['volume']['spec'].update(migrationNodeID='w-1'),
                       lambda b: b['volume']['status'].update(restoreRequired=True),
                       lambda b: b['replicas'][0]['spec'].update(active=False),
                       lambda b: b['replicas'].clear(),
                       lambda b: b['engines'][0]['status']['replicaModeMap'].update({'replica-one': 'WO'})]:
            b = fixture(); change(b)
            with self.assertRaises(ValueError): purge.render(b, 'boss', 'db')

    def test_pending_job_and_job_owned_pod_hold_but_application_may_run(self):
        volumes = [{'name': 'data', 'persistentVolumeClaim': {'claimName': 'db'}}]
        b = fixture(); b['jobs'] = [obj('Job', 'backup', 'boss', spec={'template': {'spec': {'volumes': volumes}}}, status={'active': 0})]
        with self.assertRaisesRegex(ValueError, 'Job'): purge.render(b, 'boss', 'db')
        b['jobs'][0]['status']['conditions'] = [{'type': 'Complete', 'status': 'True'}]
        b['pods'] = [obj('Pod', 'backup-pod', 'boss', spec={'volumes': volumes}, status={'phase': 'Running'})]
        self.assertTrue(purge.render(b, 'boss', 'db')['application_pods'])
        b['pods'][0]['metadata']['ownerReferences'] = [{'kind': 'Job', 'name': 'missing-backup', 'uid': 'missing'}]
        with self.assertRaisesRegex(ValueError, 'Pod'): purge.render(b, 'boss', 'db')

    def test_snapshot_chain_and_unknown_rows_fail_closed(self):
        for change in [lambda b: b['snapshots'].append(copy.deepcopy(b['snapshots'][1])),
                       lambda b: b['snapshots'][1].update(usercreated='false'),
                       lambda b: b['snapshots'][0].update(parent='missing'),
                       lambda b: b['snapshots'][1].update(parent='system-one')]:
            b = fixture(); change(b)
            with self.assertRaises(ValueError): purge.render(b, 'boss', 'db')

    def test_hash_rerender_and_last_moment_backup_race_never_mutate(self):
        b = fixture(); approved = purge.digest(purge.render(b, 'boss', 'db')); actions = []
        changed = copy.deepcopy(b); changed['volume']['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        with self.assertRaises(ValueError): purge.execute(lambda: changed, actions.append, 'boss', 'db', approved)
        self.assertEqual(actions, [])
        changed = copy.deepcopy(b); changed['engines'][0]['status']['backupStatus'] = {'x': {'state': 'in_progress', 'progress': 5}}
        reads = iter([b, changed])
        with self.assertRaises(ValueError): purge.execute(lambda: next(reads), actions.append, 'boss', 'db', approved)
        self.assertEqual(actions, [])

    def test_completion_requires_actual_each_replica_not_old_success(self):
        b = fixture(); plan = purge.render(b, 'boss', 'db')
        approved = purge.digest(plan)
        after = copy.deepcopy(b); after['engines'][0]['status']['purgeStatus'] = {'tcp://10.0.0.1:10000': {'isPurging': False, 'state': 'complete', 'progress': 100, 'error': ''}}
        with self.assertRaisesRegex(ValueError, 'fresh purge'): purge.execute(lambda: after, lambda _: None, 'boss', 'db', purge.digest(purge.render(after, 'boss', 'db')), wait_seconds=0)
        running = copy.deepcopy(after); running['engines'][0]['status']['purgeStatus']['tcp://10.0.0.1:10000'].update(isPurging=True, state='in_progress', progress=1)
        reads = iter([b, b, running, after]); actions = []
        result = purge.execute(lambda: next(reads), actions.append, 'boss', 'db', approved, sleep=lambda _: None)
        self.assertEqual(actions, [VOL]); self.assertEqual(result['state'], 'purge-complete')
        self.assertFalse(result['capacity_recovery_proven'])
        for bad in [{}, {'tcp://10.0.0.1:10000': {'isPurging': False, 'state': 'complete', 'progress': 100, 'error': 'coalesce failed'}}]:
            after['engines'][0]['status']['purgeStatus'] = bad
            reads = iter([b, b, running, after])
            with self.assertRaises(ValueError): purge.execute(lambda: next(reads), lambda _: None, 'boss', 'db', approved, sleep=lambda _: None, wait_seconds=0)

    def test_wire_collections_refuse_silence_partial_duplicate_and_wrong_target(self):
        row = obj('Replica', 'replica-one')
        collection = {'kind': 'ReplicaList', 'apiVersion': 'longhorn.io/v1beta2', 'metadata': {}, 'items': [row]}
        self.assertEqual(purge.whole(collection, 'Replica', 'longhorn-system'), [row])
        for bad in [{}, {**collection, 'metadata': {'continue': 'later'}}, {**collection, 'items': [row, row]}]:
            with self.assertRaises(ValueError): purge.whole(bad, 'Replica', 'longhorn-system')
        with self.assertRaises(ValueError): purge.one(row, 'Replica', 'another', 'longhorn-system')

    def test_native_tcp_status_keys_and_reciprocal_chain_edges(self):
        b = fixture(); b['snapshots'][1]['children'] = {}
        with self.assertRaisesRegex(ValueError, 'reciprocal'): purge.render(b, 'boss', 'db')
        b = fixture(); approved = purge.digest(purge.render(b, 'boss', 'db'))
        running = copy.deepcopy(b)
        running['engines'][0]['status']['purgeStatus'] = {'tcp://10.0.0.1:10000': {'isPurging': True, 'state': 'in_progress', 'progress': 10, 'error': ''}}
        done = copy.deepcopy(running)
        done['engines'][0]['status']['purgeStatus']['tcp://10.0.0.1:10000'].update(isPurging=False, state='complete', progress=100)
        reads = iter([b, b, running, done])
        self.assertEqual(purge.execute(lambda: next(reads), lambda _: None, 'boss', 'db', approved, sleep=lambda _: None)['state'], 'purge-complete')

    def test_approval_hash_is_exact_stdout_plan_bytes(self):
        plan = purge.render(fixture(), 'boss', 'db')
        actual_stdout = (json.dumps(plan, sort_keys=True, indent=2) + '\n').encode()
        self.assertEqual(purge.digest(plan), hashlib.sha256(actual_stdout).hexdigest())

    def test_original_evidence_needs_native_attachment_ack(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory); (work / 'native-1.json').write_text('{"original":"bytes","sequence":1,"begin_monotonic":1,"end_monotonic":2}\n')
            calls = []
            def attach(argv, timeout):
                calls.append(argv)
                return 0, b'confirmed: byte-identical\n', b'', True
            manifest = purge.publish(work, '11111111-1111-4111-8111-111111111111', {'state': 'fixture'}, execute=attach)
            self.assertEqual(len(manifest['parts']), 1)
            self.assertTrue(calls)
            with self.assertRaisesRegex(ValueError, 'attachment'):
                purge.publish(work, '11111111-1111-4111-8111-111111111111', {}, execute=lambda *_: (0, b'', b'', True))

    def test_wire_api_identity_and_fixed_action_reply(self):
        row = obj('Replica', 'replica-one'); row['apiVersion'] = 'another/v9'
        with self.assertRaises(ValueError): purge.one(row, 'Replica', 'replica-one', 'longhorn-system')
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            native = purge.Native(['trusted-kubectl'], Path(d)); calls = []
            def call(args):
                calls.append(args)
                return {'type': 'volume', 'name': VOL}
            native.call = call
            native.action(VOL, 'snapshotPurge')
            self.assertEqual(calls, [['create', '--raw', purge.PROXY + VOL + '?action=snapshotPurge', '-f', '/dev/null', '--request-timeout=20s']])
            for volume, action in [('https://elsewhere', 'snapshotPurge'), (VOL, 'snapshotDelete')]:
                with self.assertRaises(ValueError): native.action(volume, action)
            native.call = lambda _: {'type': 'error', 'message': 'not performed'}
            with self.assertRaises(ValueError): native.action(VOL, 'snapshotPurge')

    def test_real_wrapper_emits_effect_only_after_adapter_success(self):
        import os
        import shutil
        import subprocess
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); forge = root / 'infra/forge'; forge.mkdir(parents=True)
            (root / 'infra/estate').mkdir(); (root / 'infra/cluster').mkdir()
            shutil.copy(Path(__file__).with_name('instance-snapshot-purge.sh'), forge)
            (root / 'infra/estate/ops-credentials.sh').write_text('ops_image_ready() { echo ready; }; ops_kubectl() { echo fake-kube; }\n')
            renderer = root / 'infra/cluster/render-instance.sh'
            renderer.write_text('#!/bin/sh\nprintf "brewery\\tboss\\n"\n'); renderer.chmod(0o700)
            (forge / 'instance-snapshot-purge.py').write_text('import os,sys\nprint("adapter completed")\nsys.exit(int(os.environ["FIXTURE_EXIT"]))\n')
            def run(code, mode):
                return subprocess.run(['bash', str(forge / 'instance-snapshot-purge.sh'), *mode],
                                      env={**os.environ, 'FIXTURE_EXIT': str(code)}, capture_output=True, text=True, check=False)
            good = run(0, ['boss', 'db', 'a' * 64]); self.assertEqual(good.returncode, 0)
            self.assertIn('per-replica purge complete', good.stdout)
            failed = run(78, ['boss', 'db', 'a' * 64]); self.assertEqual(failed.returncode, 78)
            self.assertNotIn('per-replica purge complete', failed.stdout)
            self.assertNotIn('per-replica purge complete', run(0, ['--plan', 'boss', 'db']).stdout)

    def test_after_action_failure_is_unproven_not_a_prewrite_refusal(self):
        b = fixture(); approved = purge.digest(purge.render(b, 'boss', 'db'))
        def lost(_):
            raise ValueError('transport vanished after request')
        with self.assertRaisesRegex(purge.Unproven, 'transport vanished'):
            purge.execute(lambda: b, lost, 'boss', 'db', approved)

    def test_native_lists_preserve_one_resource_version_and_every_page(self):
        import tempfile
        first = obj('Replica', 'one'); second = obj('Replica', 'two')
        second['metadata']['uid'] = '22222222-2222-4222-8222-222222222222'
        page1 = {'kind': 'ReplicaList', 'apiVersion': 'longhorn.io/v1beta2', 'metadata': {'resourceVersion': '123', 'continue': 'token /+'}, 'items': [first]}
        page2 = {'kind': 'ReplicaList', 'apiVersion': 'longhorn.io/v1beta2', 'metadata': {'resourceVersion': '123'}, 'items': [second]}
        with tempfile.TemporaryDirectory() as d:
            native = purge.Native(['trusted-kubectl'], Path(d)); calls = []; pages = iter([page1, page2])
            def call(args): calls.append(args); return next(pages)
            native.call = call
            self.assertEqual(native.listing('replicas.longhorn.io', 'Replica', 'longhorn-system'), [first, second])
            self.assertIn('continue=token+%2F%2B', calls[1][2])
            bad = copy.deepcopy(page2); bad['metadata']['resourceVersion'] = '456'; pages = iter([page1, bad])
            with self.assertRaisesRegex(ValueError, 'resource version'):
                native.listing('replicas.longhorn.io', 'Replica', 'longhorn-system')


    def test_twelve_native_receipts_keep_action_and_status_order(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            for sequence in range(1, 13):
                (work / ('native-' + str(sequence) + '.json')).write_text(json.dumps({
                    'sequence': sequence, 'begin_monotonic': sequence * 1.0,
                    'end_monotonic': sequence + 0.25, 'argv': ['call-' + str(sequence)]}))
            manifest = purge.publish(work, '11111111-1111-4111-8111-111111111111',
                                     {'state': 'fixture'}, execute=lambda *_: (0, b'confirmed: byte-identical\n', b'', True))
            raw = b''.join((work / part['name']).read_bytes() for part in manifest['parts'])
            rows = json.loads(raw)['native']
            self.assertEqual([r['sequence'] for r in rows], list(range(1, 13)),
                             'call10 must not move before the action/status witnesses in calls2..9')

    def test_native_receipt_retains_sequence_and_monotonic_interval(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory); native = purge.Native(['fixture-kubectl'], work)
            original = purge.bounded.native_command
            try:
                purge.bounded.native_command = lambda *_: (0, b'{}', b'original stderr', True)
                native.call(['get', 'fixture'])
            finally:
                purge.bounded.native_command = original
            row = json.loads((work / 'native-1.json').read_text())
            self.assertEqual(row['sequence'], 1)
            self.assertLessEqual(row['begin_monotonic'], row['end_monotonic'])
            self.assertEqual(purge.bounded.decode_record(row['stderr']), b'original stderr')

    def test_unknown_or_reordered_native_provenance_is_not_attached(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            first = {'sequence': 1, 'begin_monotonic': 1, 'end_monotonic': 2}
            (work / 'native-1.json').write_text(json.dumps(first))
            for second in [{'sequence': 1, 'begin_monotonic': 3, 'end_monotonic': 4},
                           {'sequence': 3, 'begin_monotonic': 3, 'end_monotonic': 4},
                           {'sequence': True, 'begin_monotonic': 3, 'end_monotonic': 4},
                           {'sequence': 2, 'begin_monotonic': 1.5, 'end_monotonic': 3},
                           {'sequence': 2, 'begin_monotonic': 4, 'end_monotonic': 3},
                           {'sequence': 2, 'begin_monotonic': None, 'end_monotonic': 3}]:
                (work / 'native-2.json').write_text(json.dumps(second))
                calls = []
                with self.assertRaises(ValueError):
                    purge.publish(work, '11111111-1111-4111-8111-111111111111', {},
                                  execute=lambda *args: calls.append(args))
                self.assertEqual(calls, [], 'unread provenance must not reach the attachment door as complete evidence')
            native = purge.Native(['fixture-kubectl'], work); original = purge.bounded.native_command
            try:
                def lost(*_): raise OSError(5, 'fixture spawn refused')
                purge.bounded.native_command = lost
                with self.assertRaises(OSError): native.call(['get', 'fixture'])
            finally:
                purge.bounded.native_command = original
            row = json.loads((work / 'native-1.json').read_text())
            self.assertEqual((row['sequence'], row['state']), (1, 'unavailable'))
            self.assertLessEqual(row['begin_monotonic'], row['end_monotonic'])


if __name__ == '__main__':
    unittest.main()
