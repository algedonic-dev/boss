#!/usr/bin/env python3
"""The termination port is fake; no test contacts a production database."""
import copy
import hashlib
import importlib.util
import contextlib
import io
import sys
from types import SimpleNamespace
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('orphan_backends', Path(__file__).parents[1] / 'orphan-backends.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def observed():
    return {
        'observed_at': '2026-10-25T01:00:00+00:00',
        'binding': {'database': 'boss', 'pod_uid': '00000000-0000-0000-0000-000000000001',
                    'server_started_at': '2026-10-24T00:00:00+00:00'},
        'pods': [{'uid': '00000000-0000-0000-0000-000000000001', 'ips': ['10.1.0.1']},
                 {'uid': '00000000-0000-0000-0000-000000000002', 'ips': ['10.1.0.2']}],
        'nodes': [{'uid': '00000000-0000-0000-0000-000000000003', 'ips': ['10.1.0.254'], 'pod_cidrs': ['10.1.0.0/16']}],
        'backends': [{'pid': 123, 'backend_start': '2026-10-24T23:00:00+00:00',
                      'query_start': '2026-10-24T23:30:00+00:00', 'client_addr': '10.1.0.9',
                      'client_port': 32000, 'backend_type': 'client backend',
                      'state': 'active', 'database': 'boss'}],
    }


class Native:
    def __init__(self):
        self.value = observed()
        self.signals = []
        self.effect = True
        self.reads = 0
        self.before_signal = None

    def snapshot(self):
        self.reads += 1
        return copy.deepcopy(self.value)

    def terminate(self, binding, backend):
        self.signals.append((binding, backend))
        if self.before_signal:
            raise ValueError(self.before_signal)
        if self.effect:
            self.value['backends'] = [row for row in self.value['backends'] if row['pid'] != backend['pid']]
        return True


class Contract(unittest.TestCase):
    def test_loopback_link_local_and_node_addresses_are_not_orphans(self):
        for address in ['127.0.0.1', '::1', '::ffff:127.0.0.1', '169.254.2.3', 'fe80::1', '10.1.0.254', '10.20.5.9', '203.0.113.9']:
            native = Native()
            native.value['backends'][0]['client_addr'] = address
            with self.subTest(address=address), self.assertRaises(ValueError):
                module.plan(native)

    def test_unrelated_pod_churn_does_not_change_an_approved_absence(self):
        first = module.plan(Native())
        native = Native()
        native.value['pods'].append({'uid': '00000000-0000-0000-0000-000000000004', 'ips': ['10.1.0.88']})
        self.assertEqual(first['plan_sha256'], module.plan(native)['plan_sha256'])

    def test_six_orphans_plan_oldest_one_and_preserve_five_deferred(self):
        native = Native()
        native.value['backends'] = [dict(native.value['backends'][0], pid=123 + i) for i in range(6)]
        first = module.plan(native)
        self.assertEqual([row['pid'] for row in first['candidates']], [123])
        self.assertEqual([row['pid'] for row in first['deferred']], [124, 125, 126, 127, 128])
        self.assertEqual(first['eligible_count'], 6)
        result = module.execute(native, first['plan_sha256'])
        self.assertEqual(result['terminated'], [123])
        self.assertEqual(result['deferred'], first['deferred'])
        self.assertEqual([row['pid'] for row in native.value['backends']], [124, 125, 126, 127, 128])
        self.assertEqual([row['pid'] for row in module.plan(native)['candidates']], [124])

    def test_complete_native_census_includes_other_namespaces_and_nodes(self):
        db = module.load_database_module()
        commands = []
        def command(argv, timeout):
            commands.append(argv)
            if 'nodes' in argv:
                return 0, b'node-a\t00000000-0000-0000-0000-000000000003\t10.1.0.254\t10.1.0.0/16\n', b'', True
            return 0, (b'boss\tpostgres-0\t00000000-0000-0000-0000-000000000001\tRunning\t10.1.0.1\n'
                       b'boss-dev\tworker\t00000000-0000-0000-0000-000000000002\tRunning\t10.1.0.9\n'), b'', True
        native = module.Native(db.bounded.Recorder(execute=command), ['fake-kubectl'], db)
        pods = native.pods()
        self.assertEqual(len(pods), 2)
        self.assertIn('--all-namespaces', commands[0])
        self.assertEqual(native.nodes()[0]['ips'], ['10.1.0.254'])
        value = observed()
        value['pods'] = pods
        with self.assertRaisesRegex(ValueError, 'no qualifying'):
            module.render(value)

    def test_main_always_records_unknown_effect_before_failed_attachment(self):
        db = module.load_database_module()
        native = Native()
        native.before_signal = 'lost signal acknowledgment'
        sha = module.plan(native)['plan_sha256']
        old_native, old_load, old_argv = module.Native, module.load_database_module, sys.argv
        import os
        from unittest.mock import patch
        original_recorder = db.bounded.Recorder
        def recorder(**kwargs):
            kwargs.pop('execute', None)
            return original_recorder(execute=lambda argv, timeout: (1, b'', b'attachment refused', True), **kwargs)
        output = io.StringIO()
        try:
            module.Native = lambda *args: native
            module.load_database_module = lambda: SimpleNamespace(bounded=SimpleNamespace(Recorder=recorder, text=db.bounded.text))
            sys.argv = ['orphan-backends.py', sha]
            with patch.dict(os.environ, {'OPS_REQUEST_ID': '00000000-0000-0000-0000-000000000099', 'ORPHAN_BACKENDS_KUBECTL': 'fake'}), contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(module.main(), 1)
        finally:
            module.Native, module.load_database_module, sys.argv = old_native, old_load, old_argv
        result = __import__('json').loads(output.getvalue())['outcome']
        self.assertEqual(result['state'], 'unproven')
        self.assertEqual(result['attempted'], [123])
        self.assertEqual(result['terminated'], [])
        self.assertIn('lost signal acknowledgment', result['error'])

    def test_exhausted_observation_still_uploads_outcome_with_independent_budget(self):
        db = module.load_database_module()
        from unittest.mock import patch
        import os
        calls, recorders = [], []
        original_recorder = db.bounded.Recorder
        def command(argv, timeout):
            calls.append((argv, timeout))
            return 0, b'ack', b'', True
        def recorder(**kwargs):
            kwargs.setdefault('execute', command)
            value = original_recorder(**kwargs)
            recorders.append(value)
            return value
        class Exhausted(Native):
            def __init__(self, recorder, *args):
                super().__init__()
                self.recorder = recorder
            def terminate(self, binding, backend):
                for _ in range(module.MAX_NATIVE_CALLS + 1):
                    self.recorder.call('fixture-observation', ['fake'])
        sha = module.plan(Native())['plan_sha256']
        with patch.object(db.bounded, 'native_command', command), patch.object(db.bounded, 'Recorder', recorder), patch.object(module, 'load_database_module', lambda: db), patch.object(module, 'Native', Exhausted), patch.object(sys, 'argv', ['orphan-backends.py', sha]), patch.dict(os.environ, {'OPS_REQUEST_ID': '00000000-0000-0000-0000-000000000099', 'ORPHAN_BACKENDS_KUBECTL': 'fake'}), contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(module.main(), 1)
        self.assertEqual(len(recorders), 2)
        self.assertEqual(len(recorders[0].rows), module.MAX_NATIVE_CALLS)
        self.assertEqual(recorders[1].rows[0]['operation'], 'attach-evidence')
        self.assertEqual(recorders[1].rows[0]['state'], 'read')
        self.assertEqual(calls[-1][0][:2], ['boss', 'attach'])
        self.assertTrue(all(timeout <= module.NATIVE_CALL_SECONDS for argv, timeout in calls if argv == ['fake']))
        self.assertLessEqual(module.MAX_NATIVE_CALLS * module.NATIVE_CALL_SECONDS, 120)
        self.assertEqual(__import__('json').loads(output.getvalue())['outcome']['attempted'], [123])

    def test_complete_census_must_contain_the_exact_bound_database_pod(self):
        native = Native()
        native.value['pods'] = native.value['pods'][1:]
        with self.assertRaisesRegex(ValueError, 'bound database Pod'):
            module.plan(native)

    def test_runner_signs_exact_stdout_plan_bytes(self):
        document = module.plan(Native())
        self.assertEqual(document['plan_sha256'], hashlib.sha256(module.plan_bytes(document)).hexdigest())
        self.assertTrue(module.plan_bytes(document).endswith(b'\n'))

    def test_post_signal_overflow_cannot_hide_the_original_backend(self):
        class Overflow(Native):
            def terminate(self, binding, backend):
                super().terminate(binding, backend)
                self.value['backends'] = [dict(backend, pid=1000 + index) for index in range(129)]
                return True
        native = Overflow()
        sha = module.plan(native)['plan_sha256']
        with self.assertRaisesRegex(module.Unproven, 'effect NOT proven'):
            module.execute(native, sha)

    def test_real_bounded_transport_projects_a_complete_pod_census(self):
        db = module.load_database_module()
        raw = 'boss\tpostgres-0\t00000000-0000-0000-0000-000000000001\tRunning\t10.1.0.1\n'
        for index in range(57):
            raw += f'boss\twork-{index}\t00000000-0000-0000-0001-{index:012d}\tRunning\t10.2.0.{index+1}\n'
        commands = []
        def command(argv, timeout):
            commands.append(argv)
            return 0, raw.encode(), b'', True
        recorder = db.bounded.Recorder(execute=command)
        native = module.Native(recorder, ['fake-kubectl'], db)
        self.assertEqual(len(native.pods()), 58)
        self.assertIn('--chunk-size=32', commands[0])
        self.assertIn('jsonpath=', commands[0][-1])
        self.assertEqual(recorder.bytes, len(raw.encode()))
        for broken in ['', raw + raw.splitlines()[0] + '\n', raw.replace('Running', 'Unknown')]:
            recorder = db.bounded.Recorder(execute=lambda argv, timeout: (0, broken.encode(), b'', True))
            with self.assertRaises(ValueError):
                module.Native(recorder, ['fake-kubectl'], db).pods()

    def test_plan_is_stable_across_aging_but_binds_native_identity(self):
        first = module.plan(Native())
        native = Native()
        native.value['observed_at'] = '2026-10-25T01:01:00+00:00'
        self.assertEqual(first['plan_sha256'], module.plan(native)['plan_sha256'])
        for field, value in [('backend_start', '2026-10-24T22:00:00+00:00'),
                             ('client_addr', '10.1.0.8'), ('query_start', '2026-10-24T23:31:00+00:00')]:
            changed = Native()
            changed.value['backends'][0][field] = value
            self.assertNotEqual(first['plan_sha256'], module.plan(changed)['plan_sha256'])

    def test_only_old_active_orphan_client_backends_are_admitted(self):
        for field, value in [('state', 'idle'), ('backend_type', 'autovacuum worker'),
                             ('client_addr', '10.1.0.2'), ('client_addr', None),
                             ('client_addr', '::ffff:10.1.0.2'), ('client_addr', 'fe80::1%lo'),
                             ('query_start', '2026-10-25T00:59:00+00:00'), ('database', 'other')]:
            native = Native()
            native.value['backends'][0][field] = value
            with self.assertRaises(ValueError):
                module.plan(native)
            self.assertEqual(native.signals, [])

    def test_missing_or_duplicate_native_inputs_refuse(self):
        for field in ['binding', 'pods', 'nodes', 'backends', 'observed_at']:
            native = Native()
            native.value.pop(field)
            with self.assertRaises(ValueError):
                module.plan(native)
        native = Native()
        native.value['backends'] *= 2
        with self.assertRaises(ValueError):
            module.plan(native)

    def test_signed_plan_is_rechecked_before_any_signal(self):
        for change in ['backend', 'pod', 'server']:
            native = Native()
            sha = module.plan(native)['plan_sha256']
            if change == 'backend':
                native.value['backends'][0]['backend_start'] = '2026-10-24T22:00:00+00:00'
            elif change == 'pod':
                native.value['pods'][0]['ips'].append('10.1.0.9')
            else:
                native.value['binding']['server_started_at'] = '2026-10-25T00:00:00+00:00'
            with self.assertRaises(ValueError):
                module.execute(native, sha)
            self.assertEqual(native.signals, [])

    def test_effect_requires_absence_not_signal_acknowledgment(self):
        native = Native()
        sha = module.plan(native)['plan_sha256']
        native.effect = False
        with self.assertRaisesRegex(module.Unproven, 'effect NOT proven'):
            module.execute(native, sha)
        self.assertEqual(len(native.signals), 1)

    def test_post_signal_read_failure_never_says_nothing_written(self):
        native = Native()
        sha = module.plan(native)['plan_sha256']
        native.before_signal = 'lost acknowledgment'
        with self.assertRaisesRegex(module.Unproven, 'termination requested; effect NOT proven'):
            module.execute(native, sha)

    def test_one_exact_orphan_is_terminated_and_read_back(self):
        native = Native()
        sha = module.plan(native)['plan_sha256']
        result = module.execute(native, sha)
        self.assertEqual(result['terminated'], [123])
        self.assertEqual(len(native.signals), 1)
        self.assertGreaterEqual(native.reads, 3)


if __name__ == '__main__':
    unittest.main()
