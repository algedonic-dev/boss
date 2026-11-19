#!/usr/bin/env python3
"""Isolated fixed-port fixtures; no cluster or ops request is invoked."""
import importlib.util
import json
import base64
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('worker_teardown', Path(__file__).parents[1] / 'worker-teardown-witness.py')
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)

UID = '11111111-1111-4111-8111-111111111111'
OTHER = '22222222-2222-4222-8222-222222222222'
TARGET = {'namespace': 'boss-dev', 'job_uid': UID, 'pvc_uid': UID, 'run_id': UID,
          'acquisition': UID, 'generation': 1, 'pool': 'disk:ci-workspaces',
          'pool_version': 2, 'amount': '1', 'reclaimer': 'rule:return-fixture'}


class Inventory(unittest.TestCase):
    def test_manifest_amount_pattern_matches_the_canonical_decimal_argv(self):
        spec = json.loads(Path(reader.__file__).parents[1].joinpath('ops/verbs/worker-teardown-witness.json').read_text())
        pattern = next(p['pattern'] for p in spec['params'] if p['name'] == 'amount')
        import re
        for amount in ('1', '1.5', '0.125', '10.000000000000000001'):
            self.assertIsNotNone(re.fullmatch(pattern, amount), amount)
        for amount in ('1x5', '1\\x5', '-1', '1;exit'):
            self.assertIsNone(re.fullmatch(pattern, amount), amount)

    def collect(self, fault=None):
        calls = []
        def execute(argv, timeout):
            calls.append(argv)
            self.assertGreater(timeout, 0)
            self.assertIn('get', argv)
            self.assertNotIn('delete', argv)
            if '--raw' not in argv:
                if 'namespace' in argv:
                    value = {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': 'boss-dev', 'uid': OTHER, 'resourceVersion': '7'}}
                else:
                    value = {'apiVersion': 'v1', 'kind': 'ServiceAccount', 'metadata': {'name': 'default', 'namespace': 'boss-dev', 'uid': OTHER, 'resourceVersion': '7'}}
                if fault == 'control':
                    value['metadata']['uid'] = ''
                if fault == 'nil-control':
                    value['metadata']['uid'] = '00000000-0000-0000-0000-000000000000'
            else:
                url = argv[argv.index('--raw') + 1]
                jobs = '/jobs?' in url
                value = {'apiVersion': 'batch/v1' if jobs else 'v1', 'kind': 'JobList' if jobs else 'PersistentVolumeClaimList',
                         'metadata': {'resourceVersion': '12', 'continue': ''}, 'items': []}
                if fault in ('job-present', 'pvc-present', 'recycled-name') and jobs == (fault != 'pvc-present'):
                    value['items'] = [{'metadata': {'name': 'old-name', 'namespace': 'boss-dev', 'uid': OTHER if fault == 'recycled-name' else UID}}]
                if fault == 'wrong-namespace':
                    value['items'] = [{'metadata': {'name': 'old-name', 'namespace': 'foreign', 'uid': OTHER}}]
                if fault == 'wrong-shape':
                    value['items'] = [{'metadata': None}]
                if fault == 'wrong-kind':
                    value['items'] = [{'apiVersion': 'v1', 'kind': 'Secret', 'metadata': {'name': 'old-name', 'namespace': 'boss-dev', 'uid': OTHER}}]
                if fault == 'continuation':
                    value['metadata']['continue'] = 'same-token'
                if fault == 'remaining':
                    value['metadata']['remainingItemCount'] = 1
                if fault == 'changed-version' and jobs:
                    value['metadata']['resourceVersion'] = '13' if 'continue=' in url else '12'
                    value['metadata']['continue'] = '' if 'continue=' in url else 'next'
                if fault == 'second-page-target' and jobs:
                    if 'continue=' in url:
                        value['items'] = [{'metadata': {'name': 'late', 'namespace': 'boss-dev', 'uid': UID}}]
                    else:
                        value['metadata']['continue'] = 'next'
            raw = json.dumps(value).encode()
            if fault == 'malformed':
                raw = b'{'
            if fault == 'duplicate-json':
                raw = b'{"kind":"JobList","kind":"JobList"}'
            return (7 if fault == 'unread' else 0, raw, b'bounded original diagnostic', fault != 'truncated')
        recorder = reader.bounded.Recorder(execute=execute)
        result = reader.collect(recorder, ['fixed-kubectl'], TARGET)
        self.assertEqual(result['readings'], recorder.rows)
        self.assertEqual(result['target'], TARGET)
        self.assertEqual(calls[0][1:3], ['get', 'namespace'])
        if len(calls) > 1:
            self.assertIn('serviceaccount', calls[1])
        return result

    def test_complete_absence_and_recycled_name(self):
        for fault in (None, 'recycled-name'):
            with self.subTest(fault=fault):
                result = self.collect(fault)
                self.assertEqual(result['state'], 'absent', result)
                self.assertTrue(result['inventories']['jobs']['complete'])
                self.assertTrue(result['inventories']['pvcs']['complete'])

    def test_present_late_uid_and_every_unread_refuse(self):
        for fault in ('job-present', 'pvc-present', 'second-page-target', 'control', 'nil-control', 'wrong-namespace', 'wrong-shape', 'wrong-kind',
                      'continuation', 'remaining', 'changed-version', 'malformed', 'duplicate-json', 'unread', 'truncated'):
            with self.subTest(fault=fault):
                result = self.collect(fault)
                self.assertEqual(result['state'], 'unavailable', result)
                self.assertTrue(result['errors'])
                self.assertTrue(result['readings'], 'retain original failed observations')

    def test_actual_fixed_entry_retains_full_native_records_before_summary(self):
        for failure in ('', 'attachment'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(dir=os.environ['TEARDOWN_TEST_ROOT']) as directory:
                root = Path(directory)
                (root / 'bin').mkdir()
                (root / 'archive').mkdir()
                (root / 'tmp').mkdir()
                (root / 'ops').mkdir()
                (root / 'ops' / 'kubeconfig').write_text('fixture only')
                (root / 'ops' / 'kubeconfig').chmod(0o600)
                sudo = root / 'bin' / 'sudo'
                sudo.write_text('''#!/usr/bin/env python3
import json,sys
a=sys.argv[1:]
if 'image' in a or 'create' in a: sys.exit(0)
if 'container' in a: sys.exit(1)
a=a[a.index('--kubeconfig=/kc')+1:]
assert a[0]=='get', a
uid='22222222-2222-4222-8222-222222222222'
if a[1]=='namespace': v={'apiVersion':'v1','kind':'Namespace','metadata':{'name':'boss-dev','uid':uid,'resourceVersion':'7'}}
elif a[1]=='serviceaccount': v={'apiVersion':'v1','kind':'ServiceAccount','metadata':{'name':'default','namespace':'boss-dev','uid':uid,'resourceVersion':'7'}}
else:
 assert a[1]=='--raw',a
 jobs='/jobs?' in a[2]
 v={'apiVersion':'batch/v1' if jobs else 'v1','kind':'JobList' if jobs else 'PersistentVolumeClaimList','metadata':{'resourceVersion':'9','continue':''},'items':[]}
print(json.dumps(v))
''')
                sudo.chmod(0o755)
                boss = root / 'bin' / 'boss'
                boss.write_text('''#!/usr/bin/env python3
import os,sys,shutil
from pathlib import Path
assert sys.argv[1]=='attach' and sys.argv[2]==os.environ['OPS_REQUEST_ID'],sys.argv
p=Path(sys.argv[3]); shutil.copyfile(p,Path(os.environ['ARCHIVE'])/p.name)
if os.environ.get('FAIL_ATTACHMENT'): sys.exit(1)
print('isolated fixture attachment receipt: '+p.name)
''')
                boss.chmod(0o755)
                fields = ('namespace', 'job_uid', 'pvc_uid', 'run_id', 'acquisition', 'generation', 'pool', 'pool_version', 'amount', 'reclaimer')
                environment = dict(os.environ, PATH=str(root / 'bin') + ':' + os.environ['PATH'],
                    BOSS_OPS_DIR=str(root / 'ops'), BOSS_FORGE_REGISTRY_HOST='fixture.invalid',
                    OPS_REQUEST_ID=OTHER, ARCHIVE=str(root / 'archive'), TMPDIR=str(root / 'tmp'))
                if failure:
                    environment['FAIL_ATTACHMENT'] = '1'
                result = subprocess.run(['bash', str(Path(reader.__file__).with_suffix('.sh'))] + [str(TARGET[k]) for k in fields],
                    env=environment, capture_output=True, timeout=20)
                self.assertTrue(result.stdout, result.stderr.decode())
                summary = json.loads(result.stdout)
                if failure:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(summary['state'], 'unavailable')
                    self.assertIn('attachment', summary['errors'][0])
                else:
                    self.assertEqual(result.returncode, 0, result.stderr.decode())
                    self.assertEqual(summary['state'], 'absent')
                    self.assertIn('manifest', summary['manifest_attachment_receipt'])
                    manifest = json.loads((root / 'archive' / 'worker-teardown-manifest.json').read_text())
                    raw = b''.join((root / 'archive' / p['name']).read_bytes() for p in manifest['parts'])
                    self.assertEqual(len(raw), manifest['bytes'])
                    self.assertEqual(hashlib.sha256(raw).hexdigest(), manifest['sha256'])
                    observation = json.loads(raw)
                    self.assertEqual(observation['target'], TARGET)
                    self.assertEqual(len(observation['readings']), 6)
                    for row in observation['readings']:
                        for channel in ('stdout', 'stderr'):
                            record = row[channel]
                            body = base64.b64decode(record['base64'], validate=True)
                            self.assertEqual(len(body), record['bytes'])
                            self.assertEqual(hashlib.sha256(body).hexdigest(), record['sha256'])


if __name__ == '__main__':
    unittest.main()
