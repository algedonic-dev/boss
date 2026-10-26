#!/usr/bin/env python3
"""Native identity/counter fixtures; never contact a live database or cluster."""
import copy
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    'jobs_database', Path(__file__).resolve().parents[1] / 'jobs-database.py')
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)

POD = '11111111-1111-4111-8111-111111111111'
STS = '22222222-2222-4222-8222-222222222222'
PVC = '33333333-3333-4333-8333-333333333333'
PV = '44444444-4444-4444-8444-444444444444'
VOL = '55555555-5555-4555-8555-555555555555'
BOOT = '66666666-6666-4666-8666-666666666666'


def native_rows():
    return {
        'pod': '\t'.join(['boss', 'postgres-0', POD, 'w-1', 'StatefulSet',
                          'postgres', STS, 'Running', 'containerd://' + 'a' * 64,
                          '0', 'pgdata', 'pgdata-postgres-0']),
        'statefulset': '\t'.join(['boss', 'postgres', STS]),
        'pvc': '\t'.join(['boss', 'pgdata-postgres-0', PVC, 'Bound', 'pvc-' + PVC]),
        'pv': '\t'.join(['pvc-' + PVC, PV, 'boss', 'pgdata-postgres-0', PVC,
                         'driver.longhorn.io', 'pvc-' + PVC]),
        'volume': '\t'.join(['pvc-' + PVC, VOL, 'pvc-' + PVC]),
    }


def sample(read_bytes=100, write_bytes=200):
    return {'boot_id': BOOT, 'cgroup_identity': '29 12345', 'mount_device': '8:16',
            'mount_source': '/dev/longhorn/pvc-' + PVC,
            'io_stat': f'8:16 rbytes={read_bytes} wbytes={write_bytes} rios=5 wios=7\n',
            'io_pressure': 'some avg10=0.00 avg60=0.00 avg300=0.00 total=500\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=200\n'}


def target_observation(database='boss'):
    return {'process_id': 405, 'build_commit': 'a' * 40,
            'target': {'host': 'postgres', 'port': 5432, 'database': database}}


def service_fixture():
    return {'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': STS},
            'spec': {'ports': [{'port': 5432, 'targetPort': 5432, 'protocol': 'TCP'}]}}


def endpoints_fixture():
    return {'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': BOOT}, 'subsets': [
        {'addresses': [{'targetRef': {'kind': 'Pod', 'namespace': 'boss', 'name': 'postgres-0', 'uid': POD}}],
         'ports': [{'port': 5432, 'protocol': 'TCP'}]}]}


class Identity(unittest.TestCase):
    def test_positive_is_actual_pod_claim_and_csi_volume_chain(self):
        value = reader.identity(native_rows())
        self.assertEqual(value['pod_uid'], POD)
        self.assertEqual(value['pvc_uid'], PVC)
        self.assertEqual(value['volume_uid'], VOL)
        self.assertEqual(value['volume_handle'], 'pvc-' + PVC)

    def test_wrong_namespace_owner_uid_claim_and_driver_are_refused(self):
        for key, index, wrong in [('pod', 0, 'other'), ('pod', 2, ''),
                                  ('pod', 4, 'Deployment'), ('pod', 6, PVC),
                                  ('pod', 8, ''), ('pod', 10, 'other'),
                                  ('pvc', 0, 'other'), ('pvc', 3, 'Pending'),
                                  ('pv', 4, PV), ('pv', 5, 'other.csi'),
                                  ('volume', 0, 'wrong-volume'),
                                  ('volume', 2, 'wrong-pv')]:
            with self.subTest(key=key, index=index):
                rows = native_rows()
                fields = rows[key].split('\t')
                fields[index] = wrong
                rows[key] = '\t'.join(fields)
                with self.assertRaises(ValueError):
                    reader.identity(rows)

    def test_missing_or_ambiguous_rows_never_become_empty_inventory(self):
        for changed in ('', native_rows()['pod'] + '\n' + native_rows()['pod']):
            rows = native_rows()
            rows['pod'] = changed
            with self.assertRaises(ValueError):
                reader.identity(rows)

    def test_oversized_native_resource_names_are_not_command_arguments(self):
        rows = native_rows()
        fields = rows['pod'].split('\t')
        fields[11] = 'a' * 254
        rows['pod'] = '\t'.join(fields)
        fields = rows['pvc'].split('\t')
        fields[1] = 'a' * 254
        rows['pvc'] = '\t'.join(fields)
        fields = rows['pv'].split('\t')
        fields[3] = 'a' * 254
        rows['pv'] = '\t'.join(fields)
        with self.assertRaises(ValueError):
            reader.identity(rows)


class IoAccounting(unittest.TestCase):
    def test_same_lifetime_reports_exact_mount_accounting_delta(self):
        bound = reader.identity(native_rows())
        result = reader.io_delta(bound, bound, sample(), sample(140, 260))
        self.assertEqual(result['accounted_read_bytes'], 40)
        self.assertEqual(result['accounted_write_bytes'], 60)
        self.assertIn('not latency', result['meaning'])

    def test_replacement_or_reset_never_reports_a_delta(self):
        bound = reader.identity(native_rows())
        for key in ('pod_uid', 'container_id', 'pvc_uid', 'pv_uid', 'volume_uid'):
            with self.subTest(key=key):
                replacement = {**bound, key: 'replacement'}
                with self.assertRaises(ValueError):
                    reader.io_delta(bound, replacement, sample(), sample(140, 260))
        for key, wrong in [('boot_id', POD), ('cgroup_identity', '29 54321'),
                           ('mount_device', '8:17'), ('mount_source', '/dev/other')]:
            with self.subTest(key=key):
                changed = copy.deepcopy(sample(140, 260))
                changed[key] = wrong
                with self.assertRaises(ValueError):
                    reader.io_delta(bound, bound, sample(), changed)
        with self.assertRaises(ValueError):
            reader.io_delta(bound, bound, sample(), sample(99, 260))

    def test_missing_device_or_counter_is_unavailable_not_zero(self):
        bound = reader.identity(native_rows())
        for raw in ('', '8:17 rbytes=140 wbytes=260 rios=5 wios=7\n',
                    '8:16 rbytes=140 rios=5 wios=7\n',
                    '8:16 rbytes=140 wbytes=260 rios=5 wios=7\n8:16 rbytes=141 wbytes=261 rios=5 wios=7\n',
                    '8:16 rbytes=140 rbytes=141 wbytes=260 rios=5 wios=7\n'):
            after = {**sample(), 'io_stat': raw}
            with self.assertRaises(ValueError):
                reader.io_delta(bound, bound, sample(), after)


class FixedCollector(unittest.TestCase):
    def test_whole_fixed_roster_binds_both_samples_and_preserves_raw_bytes(self):
        calls = []
        io_reads = 0
        def execute(argv, timeout):
            nonlocal io_reads
            calls.append(argv)
            self.assertGreater(timeout, 0)
            self.assertLessEqual(timeout, 20)
            if '--read-target' in argv:
                return 0, json.dumps(target_observation()).encode(), b'', True
            self.assertEqual(argv[0], 'fixture-kubectl')
            if 'get' in argv:
                kind = argv[argv.index('get') + 1]
                if kind in ('service', 'endpoints'):
                    return 0, json.dumps(service_fixture() if kind == 'service' else endpoints_fixture()).encode(), b'', True
                name = {'pod': 'pod', 'statefulset': 'statefulset', 'pvc': 'pvc',
                        'pv': 'pv', 'volumes.longhorn.io': 'volume'}[kind]
                out = native_rows()[name] + '\n'
            elif 'psql' in argv:
                self.assertIn('SET default_transaction_read_only=on', argv)
                self.assertIn('SET statement_timeout=5000', argv)
                self.assertIn('SET lock_timeout=1000', argv)
                out = json.dumps({'database': 'boss', 'transaction_read_only': 'on',
                                  'io_timing': False, 'wait_groups': [], 'activities': []})
            else:
                self.assertIn('exec', argv)
                io_reads += 1
                out = ('BOOT\n' + BOOT + '\nCGROUP\n29 12345\nMOUNT\n'
                       '31 23 8:16 / /var/lib/postgresql/data rw - ext4 /dev/longhorn/pvc-' + PVC + ' rw\n'
                       f'IO_STAT\n8:16 rbytes={100 + io_reads * 10} wbytes={200 + io_reads * 20} rios=5 wios=7\n'
                       'IO_PRESSURE\nsome avg10=0.00 avg60=0.00 avg300=0.00 total=500\n'
                       'full avg10=0.00 avg60=0.00 avg300=0.00 total=200\n')
            return 0, out.encode(), b'', True
        recorder = reader.bounded.Recorder(execute=execute, limit=21)
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'observed', result)
        self.assertEqual(result['io_accounting']['accounted_read_bytes'], 10)
        self.assertEqual(result['io_accounting']['accounted_write_bytes'], 20)
        self.assertEqual(len(calls), 20)
        self.assertEqual(len(result['readings']), 20)
        self.assertTrue(all(reader.bounded.decode_record(row['stdout']) for row in result['readings']))
        self.assertFalse(result['samples'][0]['sql']['io_timing'])

    def test_native_refusal_is_retained_and_never_an_empty_healthy_read(self):
        recorder = reader.bounded.Recorder(execute=lambda argv, timeout: (1, b'', b'Forbidden: native read refused\n', True))
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(reader.bounded.decode_record(result['readings'][0]['stderr']), b'Forbidden: native read refused\n')
        self.assertNotIn('io_accounting', result)

    def test_native_transport_preserves_partial_bytes_and_names_its_bound(self):
        code, out, err, complete = reader.bounded.native_command(
            [sys.executable, '-c', 'import os; os.write(1,b"x"*200000)'], 2)
        self.assertFalse(complete)
        self.assertGreater(len(out), 0)
        self.assertLessEqual(len(out) + len(err), reader.bounded.SCALAR_BYTES)
        self.assertIsInstance(code, int)

    def test_public_wrapper_rejects_arguments_before_any_native_door(self):
        wrapper = Path(__file__).resolve().parents[1] / 'read-jobs-database.sh'
        result = subprocess.run(['bash', str(wrapper), '--query', 'SELECT secret'], capture_output=True)
        self.assertEqual(result.returncode, 78, result.stderr)
        self.assertIn(b'no arguments', result.stderr)

    def test_failed_native_collection_still_attaches_its_complete_original(self):
        attached = []
        def execute(argv, timeout):
            if argv[:2] == ['boss', 'attach']:
                original = Path(argv[3]).read_text()
                attached.append(json.loads(original))
                return 0, b'native fixture attachment receipt\n', b'', True
            return 1, b'partial native stdout\n', b'native forbidden\n', True
        def recorder_factory(**kwargs):
            return reader.bounded.Recorder(execute=execute, **kwargs)
        output = io.StringIO()
        with mock.patch.dict(os.environ, {'OPS_REQUEST_ID': POD, 'DATABASE_OBSERVATION_KUBECTL': 'fixture-kubectl'}, clear=True), \
                mock.patch.object(sys, 'argv', ['jobs-database.py']), contextlib.redirect_stdout(output):
            result = reader.main(recorder_factory=recorder_factory)
        self.assertEqual(result, 1)
        self.assertEqual(len(attached), 1)
        self.assertEqual(attached[0]['state'], 'unavailable')
        self.assertRegex(attached[0]['source']['read-jobs-database.sh'], r'^[0-9a-f]{64}$')
        reading = attached[0]['readings'][0]
        self.assertEqual(reader.bounded.decode_record(reading['stdout']), b'partial native stdout\n')
        self.assertEqual(reader.bounded.decode_record(reading['stderr']), b'native forbidden\n')
        self.assertEqual(json.loads(output.getvalue())['attachment_receipt'], 'native fixture attachment receipt\n')

    def test_partial_native_stream_never_proves_a_complete_observation(self):
        recorder = reader.bounded.Recorder(execute=lambda argv, timeout: (0, native_rows()['pod'].encode(), b'', False))
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertFalse(result['readings'][0]['stream_complete'])

    def test_native_byte_budget_stops_before_another_read_or_an_oversized_attachment(self):
        def unexpected(argv, timeout):
            self.fail('byte bound must stop before invoking the native door')
        recorder = reader.bounded.Recorder(execute=unexpected)
        recorder.bytes = 256 * 1024
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertIn('byte bound', result['error'])
        self.assertEqual(result['readings'], [])


class TargetBinding(unittest.TestCase):
    def test_internal_fixed_reader_keeps_headers_in_file_and_refuses_missing_credential(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'forge').mkdir()
            (root / 'lib').mkdir()
            wrapper = root / 'forge/read-jobs-database.sh'
            wrapper.write_bytes((Path(__file__).resolve().parents[1] / 'read-jobs-database.sh').read_bytes())
            (root / 'lib/sor.sh').write_text('sor_require() { [ -n "${BOSS_JOBS_URL:-}" ]; }\nsor_reader_header() { printf "fixture-readonly"; }\n')
            (root / 'lib/secret-header.sh').write_text('machine_token_header() { MT_HDR="${FIXTURE_HEADER:-}"; }\nsecret_header_close() { :; }\n')
            curl = root / 'curl'
            curl.write_text('#!/usr/bin/env python3\nimport json,os,sys\nopen(os.environ["FIXTURE_ARGS"],"w").write(json.dumps(sys.argv[1:]))\nprint(\'{"target":{"host":"postgres","port":5432,"database":"actual"}}\')\n')
            curl.chmod(0o700)
            args = root / 'args.json'
            env = {**os.environ, 'PATH': str(root) + ':' + os.environ['PATH'],
                   'BOSS_JOBS_URL': 'http://fixture.invalid:7900', 'FIXTURE_ARGS': str(args),
                   'FIXTURE_HEADER': '@/fixture/header-file'}
            result = subprocess.run(['bash', str(wrapper), '--read-target'], env=env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['target']['database'], 'actual')
            observed = json.loads(args.read_text())
            self.assertEqual(observed[-1], 'http://fixture.invalid:7900/api/jobs/database-observation')
            self.assertIn('@/fixture/header-file', observed)
            self.assertIn('--max-time', observed)
            self.assertNotIn('-X', observed)
            args.unlink()
            env['FIXTURE_HEADER'] = ''
            result = subprocess.run(['bash', str(wrapper), '--read-target'], env=env, capture_output=True)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(args.exists())
            for extra in (['--read-target', '/api/secret'], ['--url', 'http://other.invalid']):
                result = subprocess.run(['bash', str(wrapper), *extra], env=env, capture_output=True)
                self.assertEqual(result.returncode, 78)
                self.assertFalse(args.exists())

    def test_actual_database_is_bound_to_service_endpoint_pod_uid(self):
        target = {'host': 'postgres.boss.svc.cluster.local', 'port': 5432, 'database': 'production_current'}
        service = {'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': STS},
                   'spec': {'ports': [{'port': 5432, 'targetPort': 5432, 'protocol': 'TCP'}]}}
        endpoints = {'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': BOOT}, 'subsets': [
            {'addresses': [{'targetRef': {'kind': 'Pod', 'namespace': 'boss', 'name': 'postgres-0', 'uid': POD}}],
             'ports': [{'port': 5432, 'protocol': 'TCP'}]}]}
        result = reader.bind_target(target, service, endpoints, reader.identity(native_rows()))
        self.assertEqual(result['database'], 'production_current')
        self.assertEqual(result['pod_uid'], POD)
        self.assertEqual(result['endpoints_uid'], BOOT)
        for changed in ({**target, 'host': 'other'}, {**target, 'database': None},
                        {**target, 'database': '-d other'}, {**target, 'port': 6543}):
            with self.subTest(target=changed), self.assertRaises(ValueError):
                reader.bind_target(changed, service, endpoints, reader.identity(native_rows()))
        replaced = copy.deepcopy(endpoints)
        replaced['subsets'][0]['addresses'][0]['targetRef']['uid'] = PVC
        with self.assertRaises(ValueError):
            reader.bind_target(target, service, replaced, reader.identity(native_rows()))
        for uid in ('', 'malformed', None):
            replaced = copy.deepcopy(endpoints)
            replaced['metadata']['uid'] = uid
            with self.assertRaises(ValueError):
                reader.bind_target(target, service, replaced, reader.identity(native_rows()))

    def test_sql_timeout_retains_independent_io_and_after_lifetime(self):
        operations = []
        sql_fails, replace_process, target_reads = True, False, 0
        replace_endpoints, endpoint_reads = False, 0
        def execute(argv, timeout):
            nonlocal target_reads, endpoint_reads
            if '--read-target' in argv:
                target_reads += 1
                value = target_observation('actual_database')
                if replace_process and target_reads == 2:
                    value['process_id'] += 1
                out = json.dumps(value)
            elif 'get' in argv:
                kind = argv[argv.index('get') + 1]
                if kind == 'service':
                    out = json.dumps({'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': STS},
                                      'spec': {'ports': [{'port': 5432, 'targetPort': 5432, 'protocol': 'TCP'}]}})
                elif kind == 'endpoints':
                    endpoint_reads += 1
                    out = json.dumps({'metadata': {'namespace': 'boss', 'name': 'postgres', 'uid': PVC if replace_endpoints and endpoint_reads == 2 else BOOT}, 'subsets': [
                        {'addresses': [{'targetRef': {'kind': 'Pod', 'namespace': 'boss', 'name': 'postgres-0', 'uid': POD}}],
                         'ports': [{'port': 5432, 'protocol': 'TCP'}]}]})
                else:
                    out = native_rows()[{'pod': 'pod', 'statefulset': 'statefulset', 'pvc': 'pvc', 'pv': 'pv', 'volumes.longhorn.io': 'volume'}[kind]]
            elif 'psql' in argv:
                self.assertEqual(argv[argv.index('-d') + 1], 'actual_database')
                if sql_fails:
                    return 124, b'', b'exact native timeout\n', True
                out = json.dumps({'database': 'actual_database', 'transaction_read_only': 'on'})
            else:
                out = ('BOOT\n' + BOOT + '\nCGROUP\n29 12345\nMOUNT\n'
                       '31 23 8:16 / /var/lib/postgresql/data rw - ext4 /dev/longhorn/pvc-' + PVC + ' rw\n'
                       'IO_STAT\n8:16 rbytes=100 wbytes=200 rios=5 wios=7\nIO_PRESSURE\nsome total=5\n')
            return 0, out.encode(), b'', True
        recorder = reader.bounded.Recorder(execute=execute, limit=20,
            sink=lambda value: operations.append(value.get('operation')))
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertIn('after.pod', operations)
        self.assertIn('sample1.io', operations)
        self.assertIn('io_accounting', result)
        self.assertEqual(len(result['samples']), 2)
        self.assertIsNone(result['samples'][0]['sql'])
        failure = next(row for row in result['readings'] if row['operation'] == 'sample0.sql')
        self.assertEqual(reader.bounded.decode_record(failure['stderr']), b'exact native timeout\n')
        sql_fails, target_reads = False, 0
        recorder = reader.bounded.Recorder(execute=execute, limit=21)
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'observed', result)
        self.assertEqual(result['samples'][0]['sql']['database'], 'actual_database')
        self.assertEqual(len(result['readings']), 20)
        replace_process, target_reads = True, 0
        recorder = reader.bounded.Recorder(execute=execute, limit=21)
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertIn('lifetime changed', result['error'])
        replace_process, replace_endpoints, endpoint_reads = False, True, 0
        recorder = reader.bounded.Recorder(execute=execute, limit=21)
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertIn('lifetime changed', result['error'])


class EarlyFailure(unittest.TestCase):
    def scenario(self, failed, attached=False, stage='before', final_error=False, mode='timeout'):
        counts, originals = {}, []
        def execute(argv, timeout):
            if argv[:2] == ['boss', 'attach']:
                originals.append(json.loads(Path(argv[3]).read_text()))
                return 0, b'fixture attachment\n', b'', True
            kind = ('target' if '--read-target' in argv else
                    argv[argv.index('get') + 1] if 'get' in argv else
                    'sql' if 'psql' in argv else 'io')
            counts[kind] = counts.get(kind, 0) + 1
            if kind == failed and counts[kind] == (2 if stage == 'after' else 1):
                if mode == 'not-ready':
                    value = endpoints_fixture()
                    subset = value['subsets'][0]
                    subset['notReadyAddresses'] = subset.pop('addresses')
                    return 0, json.dumps(value).encode(), b'', True
                if mode == 'missing-target':
                    value = target_observation()
                    del value['target']
                    return 0, json.dumps(value).encode(), b'', True
                return 124, b'original partial bytes\n', b'exact first-channel timeout\n', True
            if kind == 'target': out = json.dumps(target_observation('actual_database'))
            elif kind == 'service': out = json.dumps(service_fixture())
            elif kind == 'endpoints': out = json.dumps(endpoints_fixture())
            elif kind == 'sql':
                database = argv[argv.index('-d') + 1]
                self.assertEqual(database, 'postgres' if failed == 'target' else 'actual_database')
                if failed == 'target':
                    self.assertNotIn('pg_stat_database', argv[-1])
                    self.assertNotIn('datname = current_database()', argv[-1], 'unbound server sample must include actual clients too')
                out = json.dumps({'database': database, 'transaction_read_only': 'on'})
            elif kind == 'io':
                out = ('BOOT\n' + BOOT + '\nCGROUP\n29 12345\nMOUNT\n'
                       '31 23 8:16 / /var/lib/postgresql/data rw - ext4 /dev/longhorn/pvc-' + PVC + ' rw\n'
                       'IO_STAT\n8:16 rbytes=100 wbytes=200 rios=5 wios=7\nIO_PRESSURE\nsome total=5\n')
            else: out = native_rows()[{'pod':'pod','statefulset':'statefulset','pvc':'pvc','pv':'pv','volumes.longhorn.io':'volume'}[kind]]
            return 0, out.encode(), b'', True
        def factory(**kwargs): return reader.bounded.Recorder(execute=execute, **kwargs)
        if attached:
            with mock.patch.dict(os.environ, {'OPS_REQUEST_ID': POD, 'DATABASE_OBSERVATION_KUBECTL':'fixture-kubectl'}, clear=True), mock.patch.object(sys, 'argv', ['jobs-database.py']), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(reader.main(recorder_factory=factory), 1)
            self.assertEqual(len(originals), 1)
            result = originals[0]
        else:
            patch = (mock.patch.object(reader.bounded, 'observation_bounds', side_effect=ValueError('exact final stream incomplete'))
                     if final_error else contextlib.nullcontext())
            with patch:
                result = reader.collect(factory(limit=21), ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(counts.get('sql'), 2, 'early refusal must not discard independent SQL')
        self.assertEqual(counts.get('io'), 2, 'early refusal must not discard independent IO')
        self.assertEqual(counts.get('target'), 2, 'the final process read remains independent')
        self.assertIsNotNone(result.get('after'))
        self.assertEqual(len(result['samples']), 2)
        self.assertLessEqual(len(result['readings']), 20)
        self.assertNotIn('lifetime changed', result['error'], 'unread is not evidence of replacement')
        if mode == 'timeout':
            self.assertIn('exact first-channel timeout', reader.bounded.decode_record(next(row for row in result['readings'] if row['exit'] == 124)['stderr']).decode())
        if failed == 'target':
            self.assertEqual(result['sampling_scope'], 'server-wide-unbound')
            self.assertIsNone(result['serving_database'])
        if final_error:
            self.assertIn('exact final stream incomplete', result['error'])
            self.assertIn('sample0.sql', result['error'])

    def test_each_first_channel_refusal_keeps_both_samples(self):
        for kind in ('target','pod','statefulset','pvc','pv','volumes.longhorn.io','service','endpoints'):
            with self.subTest(kind=kind): self.scenario(kind)

    def test_first_target_and_binding_failures_still_attach_originals(self):
        for kind in ('target','pod','service','endpoints'):
            with self.subTest(kind=kind): self.scenario(kind, attached=True)

    def test_unread_after_binding_does_not_claim_a_replacement(self):
        self.scenario('endpoints', stage='after')

    def test_final_validation_preserves_both_earlier_and_fatal_errors(self):
        self.scenario('sql', final_error=True)

    def test_not_ready_endpoint_and_missing_target_keep_unbound_samples(self):
        self.scenario('endpoints', mode='not-ready')
        self.scenario('target', mode='missing-target')

    def test_expired_total_budget_does_not_execute_more_native_calls(self):
        ticks = iter([0])
        def clock(): return next(ticks, 150)
        def forbidden(argv, timeout): self.fail('expired total budget must not execute a native call')
        recorder = reader.bounded.Recorder(execute=forbidden, clock=clock, limit=21)
        result = reader.collect(recorder, ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(len(result['samples']), 2)
        self.assertTrue(all(row['state'] == 'not-attempted' for row in result['readings']))
        self.assertLessEqual(len(result['readings']), 20)


class IoFailure(unittest.TestCase):
    def scenario(self, mode, main):
        attached, rows = [], []
        def execute(argv, timeout):
            if argv[:2] == ['boss', 'attach']:
                attached.append(json.loads(Path(argv[3]).read_text()))
                return 0, b'fixture-native-attachment-receipt\n', b'', True
            if '--read-target' in argv:
                return 0, json.dumps(target_observation()).encode(), b'', True
            if 'get' in argv:
                kind = argv[argv.index('get') + 1]
                if kind in ('service', 'endpoints'):
                    value = service_fixture() if kind == 'service' else endpoints_fixture()
                    return 0, json.dumps(value).encode(), b'', True
                name = {'pod':'pod','statefulset':'statefulset','pvc':'pvc','pv':'pv','volumes.longhorn.io':'volume'}[kind]
                return 0, (native_rows()[name] + '\n').encode(), b'', True
            if 'psql' in argv:
                return 0, b'{"database":"boss","transaction_read_only":"on","fixture_sql_retained":true}', b'', True
            # Exercise the real native subprocess transport, never a live IO read.
            code = ('import sys; sys.stdout.write("partial-native-io\\n"); sys.stderr.write("exact fixture IO timeout\\n"); sys.exit(124)' if mode == 'timeout' else 'print("malformed-native-io")')
            return reader.bounded.native_command([sys.executable, '-c', code], timeout)
        def factory(**kwargs):
            original_sink = kwargs.pop('sink', None)
            def sink(row):
                rows.append(row)
                if original_sink: original_sink(row)
            return reader.bounded.Recorder(execute=execute, sink=sink, **kwargs)
        if main:
            output = io.StringIO()
            with mock.patch.dict(os.environ, {'OPS_REQUEST_ID':POD,'DATABASE_OBSERVATION_KUBECTL':'fixture-kubectl'}, clear=True), mock.patch.object(sys,'argv',['jobs-database.py']), contextlib.redirect_stdout(output):
                status = reader.main(recorder_factory=factory)
            self.assertEqual(status, 1)
            self.assertEqual(len(attached), 1, 'failed IO must still attach complete original')
            result = attached[0]
            self.assertEqual(json.loads(output.getvalue())['attachment_receipt'], 'fixture-native-attachment-receipt\n')
        else:
            result = reader.collect(factory(limit=21), ['fixture-kubectl'], sleep=lambda _: None)
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(len(result['samples']), 2)
        self.assertTrue(all(s['sql']['fixture_sql_retained'] for s in result['samples']))
        self.assertTrue(all(s['io'] is None for s in result['samples']))
        self.assertIsNotNone(result['after'])
        self.assertIsNotNone(result['process_after'])
        self.assertNotIn('io_accounting', result)
        self.assertIn('sample0.io', result['error'])
        self.assertIn('after.pod', [r['operation'] for r in result['readings']])
        failure = next(r for r in result['readings'] if r['operation']=='sample0.io')
        if mode == 'timeout':
            self.assertEqual(failure['exit'],124)
            self.assertEqual(reader.bounded.decode_record(failure['stderr']),b'exact fixture IO timeout\n')
        else:
            self.assertEqual(reader.bounded.decode_record(failure['stdout']),b'malformed-native-io\n')
    def test_collect_native_io_124_keeps_sql_and_after(self): self.scenario('timeout',False)
    def test_collect_native_io_malformed_keeps_sql_and_after(self): self.scenario('malformed',False)
    def test_main_native_io_124_still_attaches(self): self.scenario('timeout',True)
    def test_main_native_io_malformed_still_attaches(self): self.scenario('malformed',True)


if __name__ == '__main__':
    unittest.main()
