#!/usr/bin/env python3
"""Native-shaped passive memory fixtures; never read a live cluster."""
import copy
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


class StackMemory(unittest.TestCase):
    def reader(self):
        path = Path(__file__).resolve().parents[1] / 'stack-memory.py'
        self.assertTrue(path.is_file(), 'no fixed stack memory reader exists')
        spec = importlib.util.spec_from_file_location('stack_memory', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def fixture(self):
        return {'node': {'nodeName': 'cp-1', 'memory': {
                    'time': '2026-10-30T20:00:00Z', 'workingSetBytes': 1000,
                    'availableBytes': 2000, 'usageBytes': 1500}},
                'pods': [{'podRef': {'namespace': 'boss', 'name': 'boss-one',
                                    'uid': '11111111-1111-4111-8111-111111111111'},
                          # Native kubelet summary creation precedes CRI start.
                          'containers': [{'name': 'boss', 'startTime': '2026-10-30T18:59:59Z',
                                          'memory': {'time': '2026-10-30T20:00:00Z',
                                                     'workingSetBytes': 400,
                                                     'usageBytes': 600, 'rssBytes': 300}}]}]}

    def test_actual_working_set_is_typed_and_bound_to_the_exact_pod(self):
        reader = self.reader()
        result = reader.memory(self.fixture(), 'cp-1', 'boss-one',
                               '11111111-1111-4111-8111-111111111111',
                               '2026-10-30T19:00:00Z', '2026-10-30T20:00:05Z')
        self.assertEqual(result['container']['working_set_bytes'], 400)
        self.assertEqual(result['node']['available_bytes'], 2000)
        self.assertIs(type(result['container']['usage_bytes']), int)
        self.assertNotIn('recommended_limit', result)

    def test_replaced_missing_stale_duplicate_and_untyped_memory_refuse(self):
        reader = self.reader()
        for change in ('uid', 'node', 'missing', 'duplicate', 'stale', 'bool', 'negative', 'start'):
            value = self.fixture()
            if change == 'uid': value['pods'][0]['podRef']['uid'] = 'other'
            if change == 'node': value['node']['nodeName'] = 'cp-3'
            if change == 'missing': del value['pods'][0]['containers'][0]['memory']['workingSetBytes']
            if change == 'duplicate': value['pods'].append(copy.deepcopy(value['pods'][0]))
            if change == 'stale': value['pods'][0]['containers'][0]['memory']['time'] = '2026-10-30T19:00:00Z'
            if change == 'bool': value['pods'][0]['containers'][0]['memory']['usageBytes'] = True
            if change == 'negative': value['node']['memory']['availableBytes'] = -1
            if change == 'start': value['pods'][0]['containers'][0]['startTime'] = '2026-10-30T18:00:00Z'
            with self.subTest(change=change), self.assertRaises(ValueError):
                reader.memory(value, 'cp-1', 'boss-one',
                              '11111111-1111-4111-8111-111111111111',
                              '2026-10-30T19:00:00Z', '2026-10-30T20:00:05Z')

    def test_creation_time_is_not_runtime_start_and_has_a_small_bound(self):
        reader = self.reader()
        for created in ('2026-10-30T19:00:00Z', '2026-10-30T18:59:59Z',
                        '2026-10-30T18:59:55Z'):
            value = self.fixture()
            value['pods'][0]['containers'][0]['startTime'] = created
            with self.subTest(created=created):
                result = reader.memory(value, 'cp-1', 'boss-one',
                    '11111111-1111-4111-8111-111111111111',
                    '2026-10-30T19:00:00Z', '2026-10-30T20:00:05Z')
                self.assertEqual(result['container']['working_set_bytes'], 400)
        for created in ('2026-10-30T19:00:01Z', '2026-10-30T18:59:54Z'):
            value = self.fixture()
            value['pods'][0]['containers'][0]['startTime'] = created
            with self.subTest(created=created), self.assertRaises(ValueError):
                reader.memory(value, 'cp-1', 'boss-one',
                    '11111111-1111-4111-8111-111111111111',
                    '2026-10-30T19:00:00Z', '2026-10-30T20:00:05Z')

    def test_each_instance_conserves_only_its_own_readings(self):
        reader = self.reader()
        def execute(argv, timeout):
            self.assertEqual(argv[:3], ['kubectl', 'get', 'deployment'])
            namespace = argv[argv.index('-n') + 1]
            return 1, b'', ('Forbidden ' + namespace).encode(), True
        recorder = reader.bounded.Recorder(execute=execute)
        first = reader.collect(recorder, ['kubectl'], 'boss', 'cp-3')
        first_bytes = json.dumps(first, sort_keys=True)
        second = reader.collect(recorder, ['kubectl'], 'boss-playground', 'w-1')
        self.assertEqual(json.dumps(first, sort_keys=True), first_bytes,
                         'later instance must not mutate earlier evidence')
        self.assertEqual(len(first['readings']), 1)
        self.assertEqual(len(second['readings']), 1)
        self.assertEqual(recorder.rows, first['readings'] + second['readings'])
        for result, namespace in ((first, 'boss'), (second, 'boss-playground')):
            self.assertEqual(result['state'], 'unavailable')
            self.assertEqual(reader.bounded.decode_record(result['readings'][0]['stderr']),
                             ('Forbidden ' + namespace).encode())

    def test_public_main_prints_actual_per_instance_refusal_reasons(self):
        reader = self.reader()
        request = '55555555-5555-4555-8555-555555555555'
        attached, output = [], io.StringIO()
        def execute(argv, timeout):
            if argv[:3] == ['boss', 'attach', request]:
                attached.append(json.loads(Path(argv[3]).read_text()))
                return 0, b'attached original', b'', True
            self.assertEqual(argv[:3], ['kubectl', 'get', 'deployment'])
            return 1, b'', b'Forbidden: native fixture refusal', True
        def recorder_factory(**kwargs):
            return reader.bounded.Recorder(execute=execute, **kwargs)
        with tempfile.TemporaryDirectory(prefix='stack-memory-reasons-') as directory:
            with mock.patch.object(reader.tempfile, 'mkdtemp', return_value=directory), \
                 mock.patch.dict(os.environ, {'OPS_REQUEST_ID': request, 'STACK_MEMORY_KUBECTL': 'kubectl'}), \
                 mock.patch.object(reader.sys, 'argv', ['stack-memory.py']), \
                 contextlib.redirect_stdout(output):
                self.assertEqual(reader.main(recorder_factory), 1)
        printed = json.loads(output.getvalue())
        self.assertEqual(printed['state'], 'unavailable')
        self.assertEqual(printed['errors'], {
            'boss': 'ValueError: native reading unavailable: boss.before.deployment',
            'boss-playground': 'ValueError: native reading unavailable: boss-playground.before.deployment'})
        self.assertEqual(len(attached[0]['readings']), 2)
        self.assertEqual(len(attached[0]['instances']['boss']['readings']), 1)
        self.assertEqual(len(attached[0]['instances']['boss-playground']['readings']), 1)
        self.assertEqual(printed['call_bytes']['boss'], attached[0]['instances']['boss']['call_bytes'])

    def test_public_door_is_fixed_forge_read_with_no_packet_parameters(self):
        root = Path(__file__).resolve().parents[2]
        path = root / 'ops/verbs/read-stack-memory.json'
        self.assertTrue(path.is_file(), 'no registered fixed memory observation door')
        verb = json.loads(path.read_text())
        self.assertEqual(verb['hosts'], ['forge'])
        self.assertEqual(verb['params'], [])
        self.assertEqual(verb['argv'], ['infra/forge/read-stack-memory.sh'])

    def test_real_collector_only_gets_fixed_metadata_and_conserves_native_bytes(self):
        reader = self.reader()
        calls = []
        pod_uid = '11111111-1111-4111-8111-111111111111'
        deployment_uid = '22222222-2222-4222-8222-222222222222'
        replica_uid = '33333333-3333-4333-8333-333333333333'
        node_uid = '44444444-4444-4444-8444-444444444444'
        mode = ''
        namespace, stack_node = 'boss', 'cp-1'
        deployments = 0
        def execute(argv, timeout):
            nonlocal deployments
            calls.append(argv)
            self.assertEqual(argv[:2], ['kubectl', 'get'])
            self.assertLessEqual(timeout, 20)
            if argv[2] == 'deployment':
                self.assertEqual(argv[argv.index('-n') + 1], namespace)
                deployments += 1
                generation = 2 if mode == 'replacement' and deployments > 1 else 1
                out = f'{deployment_uid}\t{generation}\tboss\t1\n'.encode()
            elif argv[2] == 'pods':
                self.assertEqual(argv[argv.index('-n') + 1], namespace)
                out = ('continue\t\nboss-one\t' + pod_uid + '\t' + stack_node + '\tReplicaSet\tboss-rs\t'
                       + replica_uid + '\tRunning\t\tcontainerd://' + 'a' * 64
                       + '\t0\t2026-10-30T19:00:00Z\t4Gi\t12Gi\n').encode()
                if mode == 'continued': out = out.replace(b'continue\t\n', b'continue\tunread-tail\n')
            elif argv[2] == 'replicaset':
                out = f'{replica_uid}\tDeployment\tboss\t{deployment_uid}\n'.encode()
                if mode == 'owner': out = out.replace(deployment_uid.encode(), pod_uid.encode())
            elif argv[2] == 'node':
                out = f'{argv[3]}\t{node_uid}\t31000000Ki\t30000000Ki\n'.encode()
            elif argv[2] == '--raw':
                self.assertEqual(argv[3], '/api/v1/nodes/' + argv[3].split('/')[4]
                                 + '/proxy/stats/summary?only_cpu_and_memory=true')
                summary = self.fixture()
                summary['node']['nodeName'] = argv[3].split('/')[4]
                summary['pods'][0]['podRef']['namespace'] = namespace
                if mode.startswith('zero:'):
                    summary['pods'][0]['containers'][0]['memory'][mode.split(':', 1)[1]] = 0
                if mode == 'malformed': summary['pods'] = [None]
                out = json.dumps(summary).encode()
                if mode == 'partial': return 0, out, b'output bound reached', False
            else:
                self.fail('unexpected native command: ' + repr(argv))
            return 0, out, b'', True
        recorder = reader.bounded.Recorder(execute=execute,
            wall=lambda: '2026-10-30T20:00:05+00:00')
        result = reader.collect(recorder, ['kubectl'])
        self.assertEqual(result['state'], 'observed', result)
        self.assertEqual(result['samples'][0]['stack']['working_set_bytes'], 400)
        self.assertEqual(result['samples'][1]['node'], 'cp-3')
        self.assertEqual(result['identity_before'], result['identity_after'])
        self.assertEqual(len(calls), len(result['readings']))
        for row in result['readings']:
            original = reader.bounded.decode_record(row['stdout'])
            self.assertTrue(original)
            self.assertNotIn('exec', row['argv'])
            self.assertNotIn('patch', row['argv'])
        namespace, stack_node = 'boss-playground', 'w-1'
        calls.clear()
        recorder = reader.bounded.Recorder(execute=execute,
            wall=lambda: '2026-10-30T20:00:05+00:00')
        playground = reader.collect(recorder, ['kubectl'], namespace, stack_node)
        self.assertEqual(playground['state'], 'observed', playground)
        self.assertEqual(playground['samples'][0]['stack']['working_set_bytes'], 400)
        self.assertEqual(playground['identity_before']['node'], 'w-1')
        namespace, stack_node = 'boss', 'cp-1'
        for mode in ('replacement', 'continued', 'owner', 'partial', 'malformed',
                     'zero:workingSetBytes', 'zero:usageBytes', 'zero:rssBytes'):
            with self.subTest(mode=mode):
                calls.clear()
                deployments = 0
                recorder = reader.bounded.Recorder(execute=execute,
                    wall=lambda: '2026-10-30T20:00:05+00:00')
                changed = reader.collect(recorder, ['kubectl'])
                self.assertEqual(changed['state'], 'unavailable', (mode, changed))
                self.assertNotIn('samples', changed)
                if mode == 'partial':
                    self.assertEqual(reader.bounded.decode_record(changed['readings'][-1]['stderr']),
                                     b'output bound reached')

    def test_native_refusal_is_unavailable_with_complete_stderr_not_zero(self):
        reader = self.reader()
        raw = b'Forbidden: preserved native reason\n'
        recorder = reader.bounded.Recorder(execute=lambda argv, timeout: (1, b'', raw, True))
        result = reader.collect(recorder, ['kubectl'])
        self.assertEqual(result['state'], 'unavailable')
        self.assertEqual(reader.bounded.decode_record(result['readings'][0]['stderr']), raw)
        self.assertNotIn('samples', result)
        self.assertEqual(result.get('call_bytes'), [{
            'operation': 'boss.before.deployment', 'retained_stdout_bytes': 0,
            'retained_stderr_bytes': len(raw), 'retained_total_bytes': len(raw),
            'shared_stream_limit_bytes': 131072, 'remaining_per_call_retained_bytes': 131072 - len(raw),
            'stream_complete': True}])

    def test_real_recorder_bounds_narrow_summary_and_preserves_identity(self):
        reader = self.reader()
        uid = '11111111-1111-4111-8111-111111111111'
        deploy = '22222222-2222-4222-8222-222222222222'
        replica = '33333333-3333-4333-8333-333333333333'
        node = '44444444-4444-4444-8444-444444444444'
        outputs = {
            'deployment': f'{deploy}\t1\tboss\t1\n',
            'pods': 'continue\t\nboss-one\t' + uid + '\tcp-1\tReplicaSet\tboss-rs\t'
                    + replica + '\tRunning\t\tcontainerd://' + 'a' * 64
                    + '\t0\t2026-10-30T19:00:00Z\t4Gi\t12Gi\n',
            'replicaset': f'{replica}\tDeployment\tboss\t{deploy}\n'}
        with tempfile.TemporaryDirectory(prefix='memory-native-fixture-') as directory:
            fixture = Path(directory) / 'kubectl.py'
            summary = Path(directory) / 'summary.json'
            fixture.write_text('import json,sys\nfrom pathlib import Path\n'
                + 'outputs=' + repr(outputs) + '\n'
                + 'args=sys.argv[1:]\nkind=args[1]\n'
                + "if kind == '--raw':\n"
                + " assert args[2] == '/api/v1/nodes/' + args[2].split('/')[4] + '/proxy/stats/summary?only_cpu_and_memory=true'\n"
                + ' sys.stdout.buffer.write(Path(' + repr(str(summary)) + ').read_bytes())\n'
                + "elif kind == 'node': print(args[2] + '\\t" + node + "\\t31000000Ki\\t30000000Ki')\n"
                + 'else: sys.stdout.write(outputs[kind])\n')
            for oversized in (False, True):
                value = self.fixture()
                if oversized:
                    value['unused'] = 'x' * 200000
                summary.write_text(json.dumps(value))
                recorder = reader.bounded.Recorder(
                    wall=lambda: '2026-10-30T20:00:05+00:00')
                result = reader.collect(recorder, [reader.sys.executable, str(fixture)], 'boss', 'cp-1')
                with self.subTest(oversized=oversized):
                    self.assertEqual(result['state'], 'unavailable' if oversized else 'observed', result)
                    rows = [row for row in result['readings'] if row['operation'].endswith('.summary')]
                    self.assertEqual(len(rows), 1)
                    row = rows[0]
                    out = reader.bounded.decode_record(row['stdout'])
                    err = reader.bounded.decode_record(row['stderr'])
                    self.assertLessEqual(len(out) + len(err), 131072)
                    self.assertEqual(out, summary.read_bytes()[:len(out)])
                    self.assertEqual(row['stream_complete'], not oversized)
                    budget = next(item for item in result['call_bytes']
                                  if item['operation'] == row['operation'])
                    self.assertEqual(budget['retained_total_bytes'], len(out) + len(err))
                    self.assertEqual(budget['remaining_per_call_retained_bytes'], 131072 - len(out) - len(err))
                    self.assertEqual(budget['stream_complete'], not oversized)
                    if oversized:
                        self.assertNotIn('samples', result)
                    else:
                        self.assertEqual(result['identity_before'], result['identity_after'])
                        self.assertEqual(result['samples'][0]['stack']['working_set_bytes'], 400)

    def test_unattempted_or_missing_process_byte_counts_are_unknown(self):
        reader = self.reader()
        for recorder in (reader.bounded.Recorder(execute=lambda *_: (_ for _ in ()).throw(
                FileNotFoundError(2, 'isolated missing fixture'))), reader.bounded.Recorder()):
            if recorder.execute is reader.bounded.native_command:
                recorder.bytes = reader.bounded.TOTAL_BYTES
            result = reader.collect(recorder, ['never-executed-fixture'])
            self.assertEqual(result['state'], 'unavailable')
            self.assertIsNone(result['call_bytes'][0]['retained_total_bytes'])
            self.assertIsNone(result['call_bytes'][0]['remaining_per_call_retained_bytes'])
            self.assertIsNone(result['call_bytes'][0]['stream_complete'])

    def test_public_shell_refuses_arguments_before_opening_credentials(self):
        path = Path(__file__).resolve().parents[1] / 'read-stack-memory.sh'
        result = subprocess.run(['bash', str(path), 'cp-3'], capture_output=True)
        self.assertEqual(result.returncode, 78)
        self.assertIn(b'no arguments', result.stderr)

    def test_main_attaches_full_observation_and_does_not_turn_refusal_green(self):
        reader = self.reader()
        request = '55555555-5555-4555-8555-555555555555'
        originals = {'state': 'unavailable', 'claim': 'stack-memory-observation-only',
                     'readings': [{'stdout': reader.bounded.byte_record(b'complete native bytes'),
                                   'stderr': reader.bounded.byte_record(b'complete error')}]}
        attached = []
        def execute(argv, timeout):
            self.assertEqual(argv[:3], ['boss', 'attach', request])
            attached.append(json.loads(Path(argv[3]).read_text()))
            return 0, b'attached original', b'', True
        def recorder_factory(**kwargs):
            return reader.bounded.Recorder(execute=execute, **kwargs)
        with tempfile.TemporaryDirectory(prefix='stack-memory-fixture-') as directory:
            with mock.patch.object(reader.tempfile, 'mkdtemp', return_value=directory), \
                 mock.patch.object(reader, 'collect', return_value=copy.deepcopy(originals)) as collected, \
                 mock.patch.dict(os.environ, {'OPS_REQUEST_ID': request, 'STACK_MEMORY_KUBECTL': 'kubectl'}), \
                 mock.patch.object(reader.sys, 'argv', ['stack-memory.py']), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(reader.main(recorder_factory), 1)
                self.assertEqual([call.args[2:] for call in collected.call_args_list],
                                 [('boss', 'cp-3'), ('boss-playground', 'w-1')],
                                 'the fixed public door must observe BOTH instance lifetimes')
        self.assertEqual(attached[0]['instances']['boss']['readings'], originals['readings'])
        self.assertEqual(attached[0]['instances']['boss-playground']['readings'], originals['readings'])
        self.assertEqual(attached[0]['request'], request)
        for name in ('stack-memory.py', 'gate-storage.py', 'read-stack-memory.sh'):
            self.assertRegex(attached[0]['source'][name], r'^[0-9a-f]{64}$')


if __name__ == '__main__':
    unittest.main()
