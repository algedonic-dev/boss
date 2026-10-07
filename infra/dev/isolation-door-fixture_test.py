"""Synthetic published-generation surrogate, not live session attestation."""
import http.server
import importlib.util
import json
from pathlib import Path
import secrets
import shutil
import subprocess
import tempfile
import threading
import unittest

SOURCE = Path(__file__).resolve().parents[2]
ARTIFACTS = Path(tempfile.mkdtemp(prefix='boss-isolation-door-artifacts-'))
print('Synthetic door artifact directory:', ARTIFACTS, flush=True)

class DoorFixtureTests(unittest.TestCase):
    def test_trusted_door_attribution_and_worker_denial(self):
        with tempfile.TemporaryDirectory(prefix='boss-door-fake-') as directory:
            generation = Path(directory)
            token = secrets.token_hex(24)
            records = []
            class Receiver(http.server.BaseHTTPRequestHandler):
                def do_POST(self):
                    actor = json.loads(self.headers.get('X-Boss-User', '{}')).get('id')
                    authenticated = self.headers.get('x-boss-machine-token') == token
                    records.append({'actor': actor, 'method': self.command, 'authenticated': authenticated,
                                    'accepted': authenticated and actor == 'agent-codex', 'path': self.path})
                    self.send_response(200 if records[-1]['accepted'] else 401)
                    self.end_headers()
                    self.wfile.write(b'fixture response\n')
                def log_message(self, *_):
                    pass
            server = http.server.HTTPServer(('127.0.0.1', 0), Receiver)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                for relative in ('infra/dev/boss-api', 'infra/dev/door-freshness.sh',
                                 'infra/lib/secret-header.sh', 'infra/lib/curl-through-a-roll.sh',
                                 'infra/forge/probe-bin/sor-routes.sh'):
                    target = generation / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(SOURCE / relative, target)
                data = generation / 'infra/dev'
                mount = generation / 'synthetic-token'
                mount.mkdir()
                (mount / 'current').write_text(token)
                url = 'http://127.0.0.1:' + str(server.server_port)
                (data / 'machine-token-dir').write_text(str(mount))
                for name in ('sor-url', 'sor-read-url'):
                    (data / name).write_text(url)
                (generation / 'infra/forge/sor-ports.env').write_text('jobs=' + str(server.server_port))
                (data / 'sor-read-ports.env').write_text('')
                spec = importlib.util.spec_from_file_location('trusted_control_tools', SOURCE / 'infra/dev/control-tools.py')
                control = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(control)
                def invoke(args, actor):
                    inherited = {'BOSS_SOR_WAIT_SECONDS': '0'}
                    if actor is not None:
                        inherited['BOSS_ACTOR'] = actor
                    argv, env = control.tool_plan(generation, 'boss-api', args, inherited)
                    # Surrogate HOME prevents reading a real session actor file.
                    env['HOME'] = str(generation / 'empty-home')
                    result = subprocess.run(argv, env=env, capture_output=True, timeout=10)
                    index = len(list(ARTIFACTS.glob('call-*.exit')))
                    stem = ARTIFACTS / ('call-' + str(index))
                    stem.with_suffix('.stdout').write_bytes(result.stdout)
                    stem.with_suffix('.stderr').write_bytes(result.stderr)
                    stem.with_suffix('.exit').write_text(str(result.returncode) + '\n')
                    return result
                # RED control: an incomplete generated protocol closure refuses
                # before the receiver sees any request. Restore only fixture copy.
                roll = generation / 'infra/lib/curl-through-a-roll.sh'
                reviewed_roll = roll.read_bytes()
                roll.unlink()
                incomplete = invoke(['POST', '/api/jobs'], 'agent-codex')
                self.assertNotEqual(incomplete.returncode, 0)
                self.assertEqual(records, [])
                roll.write_bytes(reviewed_roll)
                positive = invoke(['POST', '/api/jobs'], 'agent-codex')
                self.assertEqual(positive.returncode, 0, positive.stderr.decode())
                self.assertEqual(records[-1]['actor'], 'agent-codex')
                self.assertTrue(records[-1]['accepted'])
                count = len(records)
                for args, actor in ((['POST', '/api/jobs'], None),
                                    (['TRACE', '/api/jobs'], 'agent-codex'),
                                    (['POST', '@evil.invalid/x'], 'agent-codex')):
                    result = invoke(args, actor)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(len(records), count)
                # Worker copy has only fake receiver config and no token slot.
                (mount / 'current').unlink()
                denied = invoke(['POST', '/api/jobs'], 'invented-worker')
                self.assertNotEqual(denied.returncode, 0)
                self.assertFalse(records[-1]['authenticated'])
                self.assertFalse(records[-1]['accepted'])
                # Dot segments are accepted and curl normalizes them: observed,
                # rather than asserting a stronger restriction than this door.
                dot = invoke(['POST', '/api/../api/jobs'], 'invented-worker')
                self.assertNotEqual(dot.returncode, 0)
                self.assertEqual(records[-1]['path'], '/api/jobs')
                (ARTIFACTS / 'receiver.json').write_text(json.dumps(records, indent=2))
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

if __name__ == '__main__':
    unittest.main()
