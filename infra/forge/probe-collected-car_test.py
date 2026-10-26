"""Proof controls use a deliberately unsafe local client, never the SoR."""
import importlib.util
from pathlib import Path
import tempfile
import unittest


class CollectedCarProof(unittest.TestCase):
    def test_installed_identity_covers_the_conductor_merge_guard(self):
        path = Path(__file__).with_name('probe-collected-car.py')
        spec = importlib.util.spec_from_file_location('collected_car', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertIn('crates/orchestrators/boss-cli/src/train/conductor.rs', module.COMPONENTS)

    def test_an_unread_or_unpublished_delivery_guard_cannot_pass(self):
        path = Path(__file__).with_name('probe-collected-car.py')
        spec = importlib.util.spec_from_file_location('collected_car', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        expected = {'name':'agent-run-delivers-when-its-step-is-done', 'version':2,
                    'do':[{'handler':'jobs.complete_linked_step',
                           'args':{'target_when':'actual reviewer evidence predicate'}}]}
        active = {**expected, 'status':'active'}
        module.rule_contract({'rules':[active]}, expected)
        for response in ({'error':'unread','rules':[]}, {'rules':[]},
                         {'rules':[{**active, 'status':'draft'}]},
                         {'rules':[{**active, 'do':[]}]}, {'rules':[active, active]}):
            with self.assertRaises((AssertionError, ValueError)):
                module.rule_contract(response, expected)

    def test_a_client_that_mutates_before_refusing_cannot_pass(self):
        path = Path(__file__).with_name('probe-collected-car.py')
        spec = importlib.util.spec_from_file_location('collected_car', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            client = Path(temporary) / 'unsafe-client'
            client.write_text('''#!/usr/bin/env python3
import json, os, sys, urllib.request
from pathlib import Path
base = os.environ['BOSS_JOBS_URL']
token = Path(os.environ['BOSS_MACHINE_TOKEN_DIR'], 'current')
if not token.exists():
    print('machine credential unavailable', file=sys.stderr)
    raise SystemExit(1)
headers = {'x-boss-user': json.dumps({'id':'agent-codex'}),
           'x-boss-machine-token': token.read_text()}
for path in ('/api/jobs?kind=pr-train', '/api/jobs/12345678-1234-4234-8234-123456789012'):
    urllib.request.urlopen(urllib.request.Request(base + path, headers=headers), timeout=2).read()
req = urllib.request.Request(base + '/api/jobs/12345678-1234-4234-8234-123456789012/steps/review-step/metadata', headers=headers, data=b'{}', method='PATCH')
try: urllib.request.urlopen(req, timeout=2)
except Exception: pass
print('01234567-1234-4234-8234-012345678901')
raise SystemExit(1)
''')
            client.chmod(0o700)
            with self.assertRaises((AssertionError, RuntimeError)):
                module.exercise(client, False, {})


if __name__ == '__main__':
    unittest.main()
