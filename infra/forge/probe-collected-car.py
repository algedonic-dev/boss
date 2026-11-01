#!/usr/bin/env python3
"""Exercise the installed CLI against a strict, credential-free localhost conductor."""
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
from threading import Thread
import tomllib
from urllib.parse import parse_qs, urlsplit

CAR = '12345678-1234-4234-8234-123456789012'
TRAIN = '01234567-1234-4234-8234-012345678901'
CLI_COMPONENTS = ('crates/orchestrators/boss-cli/src/steps.rs',
                  'crates/orchestrators/boss-cli/src/dispatch.rs',
                  'crates/orchestrators/boss-cli/src/train/conductor.rs')
COMPONENTS = CLI_COMPONENTS + ('crates/orchestrators/boss-dispatcher-handlers/src/handlers/jobs_complete_linked_step.rs',
              'infra/dispatcher/rules/agent-run-delivers-when-its-step-is-done.toml',
              'infra/forge/probe-collected-car.py')


def rule_contract(response, expected):
    """The normal protected reader supplies this complete, unpaged registry."""
    assert isinstance(response, dict) and not response.get('error'), 'rule registry unread'
    rows = response.get('rules')
    assert isinstance(rows, list), 'rule registry is not an array'
    assert all(isinstance(row, dict) for row in rows), 'rule registry has malformed rows'
    matching = [row for row in rows if row.get('name') == expected['name']]
    assert len(matching) == 1, 'active delivery rule is absent or ambiguous'
    active = matching[0]
    assert active.get('status') == 'active', 'delivery rule is not enforced'
    assert active.get('version') == expected['version'], 'installed rule version is not enforced'
    assert expected['do'][0]['args'].get('target_when'), 'installed rule has no evidence predicate'
    assert active.get('do') == expected['do'], 'active delivery guard differs from installed source'
    return {'name': active['name'], 'version': active['version'], 'status': active['status']}


def exercise(binary, installed, digests):
    for status, operation in [('open', 'hold'), ('open', 'dispatch'),
                              ('closed', 'hold'), ('cancelled', 'hold'),
                              ('collect-during-hold', 'hold'), ('unread-after-hold', 'hold')]:
        fixture_token = secrets.token_urlsafe(24)
        car = {'id': CAR, 'kind': 'ship-a-change', 'status': 'open',
               'title': 'Local fixture only', 'metadata': {'branch': 'fix/local-fixture'},
               'steps': [{'id': 'review-step', 'spec_slug': 'review', 'kind': 'task',
                          'title': 'Open for review', 'status': 'ready',
                          'metadata': {'hold': 'existing fixture hold', 'authority_role': 'platform-admin'}}]}
        train = {'id': TRAIN, 'kind': 'pr-train', 'status': 'open' if status in ('collect-during-hold', 'unread-after-hold') else status,
                 'metadata': {'boarded_jobs': [] if status in ('collect-during-hold', 'unread-after-hold') else [CAR]}}
        requests, errors = [], []

        class Handler(BaseHTTPRequestHandler):
            def answer(self):
                parsed = urlsplit(self.path)
                requests.append({'method': self.command, 'path': self.path})
                try:
                    actor = json.loads(self.headers.get('x-boss-user', '{}')).get('id')
                    assert actor == 'agent-codex', 'fixture requires its named actor header'
                    assert self.headers.get('x-boss-machine-token') == fixture_token, 'fixture requires its private local token'
                    assert len(requests) <= 20, 'fixture request bound'
                    if self.command == 'GET' and parsed.path == '/api/jobs':
                        query = parse_qs(parsed.query)
                        kind = query.get('kind', [''])[0]
                        assert kind in ('ship-a-change', 'pr-train'), 'unexpected list'
                        rows = [car] if kind == 'ship-a-change' else ([train] if status in ('open', 'collect-during-hold', 'unread-after-hold') else [])
                        if kind == 'pr-train' and status == 'unread-after-hold' and car['steps'][0]['metadata']['hold'] == 'fixture review':
                            raw = b'{"error":"fixture post-write census unread"}'
                            self.send_response(503)
                            self.send_header('Content-Length', str(len(raw)))
                            self.end_headers()
                            self.wfile.write(raw)
                            return
                        body = {'data': rows, 'total': len(rows)}
                    elif self.command == 'GET' and parsed.path == '/api/jobs/' + CAR:
                        body = car
                    elif self.command == 'GET' and parsed.path == '/api/tenant/edit-level':
                        body = {'edit_level': None}
                    elif self.command == 'PATCH' and parsed.path == '/api/jobs/' + CAR + '/steps/review-step/metadata':
                        assert status != 'open', 'an aboard car was mutated'
                        size = int(self.headers.get('Content-Length', 0))
                        assert 0 < size <= 4096, 'bounded hold document'
                        patch = json.loads(self.rfile.read(size))
                        assert patch.get('hold') == 'fixture review', 'actual hold was not recorded'
                        car['steps'][0]['metadata'].update(patch)
                        if status == 'collect-during-hold':
                            train['metadata']['boarded_jobs'] = [CAR]
                        body = {}
                    else:
                        raise AssertionError('unexpected fixture request')
                    raw = json.dumps(body).encode()
                    self.send_response(200)
                except (ValueError, AssertionError) as error:
                    errors.append(str(error))
                    raw = b'{"error":"strict fixture refusal"}'
                    self.send_response(503)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            do_GET = answer
            do_POST = answer
            do_PATCH = answer
            do_PUT = answer

            def log_message(self, *_):
                pass  # Whole structured requests are retained below.

        with tempfile.TemporaryDirectory(prefix='boss-collected-car-proof-') as temporary:
            token_directory = Path(temporary) / 'fixture-token'
            token_directory.mkdir(mode=0o700)
            token = token_directory / 'current'
            token.write_text(fixture_token)
            token.chmod(0o600)
            server = HTTPServer(('127.0.0.1', 0), Handler)
            thread = Thread(target=server.serve_forever, kwargs={'poll_interval': 0.02})
            thread.start()
            environment = {'PATH': os.environ['PATH'], 'HOME': temporary, 'TMPDIR': temporary,
                           'BOSS_ACTOR': 'agent-codex',
                           'BOSS_JOBS_URL': 'http://127.0.0.1:' + str(server.server_port),
                           'BOSS_MACHINE_TOKEN_DIR': str(token_directory),
                           'BOSS_MACHINE_TOKEN_HOSTS': '',
                           'BOSS_API_RETRY_DEADLINE': '3'}
            arguments = ([str(binary), 'hold', CAR, '--reason', 'fixture review'] if operation == 'hold'
                         else [str(binary), 'dispatch', CAR, '--step', 'review'])
            try:
                if status == 'open' and operation == 'hold':
                    absent = {**environment, 'BOSS_MACHINE_TOKEN_DIR': temporary + '/absent-token'}
                    refusal = subprocess.run(arguments, env=absent, capture_output=True, text=True, timeout=10)
                    assert refusal.returncode != 0 and 'machine credential unavailable' in refusal.stderr
                    assert not requests, 'absent credential reached the fixture'
                result = subprocess.run(arguments, env=environment, capture_output=True,
                                        text=True, timeout=20)
                writes = [row for row in requests if row['method'] != 'GET']
                assert not errors, errors
                assert any(row['method'] == 'GET' and row['path'].startswith('/api/jobs?')
                           and 'kind=pr-train' in row['path'] for row in requests), ('train census was not read', result.returncode, result.stdout, result.stderr, requests)
                assert any(row['method'] == 'GET' and row['path'] == '/api/jobs/' + CAR
                           for row in requests), 'actual car was not read'
                if status == 'open':
                    assert result.returncode != 0, 'aboard operation was accepted'
                    assert TRAIN in result.stdout + result.stderr, 'refusal did not name the train'
                    assert not writes, 'refusal came after a mutation'
                elif status == 'unread-after-hold':
                    assert result.returncode != 0 and len(writes) == 1
                    assert 'hold was written' in result.stderr
                    assert 'collection raced' not in result.stderr, ('an unread census is not a measured race', result.stdout, result.stderr, requests)
                elif status == 'collect-during-hold':
                    assert result.returncode != 0, ('hold falsely reported success after collection raced its write', result.stdout, result.stderr, requests)
                    assert TRAIN in result.stdout + result.stderr, 'post-write refusal did not name the train'
                    assert len(writes) == 1, 'the already written hold must remain visible'
                    assert 'hold was written' in result.stderr and 'nothing written' not in result.stderr
                else:
                    assert result.returncode == 0, result.stdout + result.stderr
                    assert len(writes) == 1, 'historical train prevented the ordinary hold'
                print(json.dumps({'installed': installed, 'component_sha256': digests,
                                  'train_status': status, 'operation': operation,
                                  'exit': result.returncode, 'stdout': result.stdout,
                                  'stderr': result.stderr, 'fixture_requests': requests}))
            finally:
                server.shutdown()
                thread.join()
                server.server_close()


def main():
    if len(sys.argv) == 3 and sys.argv[1] == '--self-test':
        exercise(Path(sys.argv[2]).resolve(strict=True), False, {})
        print('collected-car localhost fixture self-test passed')
        return 0
    if sys.argv[1:]:
        raise ValueError('usage: probe-collected-car.py [--self-test <local test binary>]')
    try:
        actual = Path('/var/lib/boss/tree/current').resolve(strict=True)
        assert re.fullmatch(r'/var/lib/boss/tree/gen/[0-9a-f]{40}', str(actual))
        for path in (actual, actual.parent, Path('/usr/local/bin/boss').resolve(strict=True)):
            info = path.stat()
            assert info.st_uid == 0 and not info.st_mode & 0o022, 'unprotected installed artifact'
        assert Path(__file__).resolve() == actual / 'infra/forge/probe-collected-car.py'
        digests = {}
        for name in COMPONENTS:
            raw = (actual / name).read_bytes()
            assert raw == subprocess.check_output(['git', 'show', 'HEAD:' + name]), name + ' not installed'
            digests[name] = hashlib.sha256(raw).hexdigest()
        binary = Path('/usr/local/bin/boss')
        version = subprocess.check_output([str(binary), '--version'], text=True, timeout=5)
        match = re.fullmatch(r'boss \S+ built from ([0-9a-f]{40})\s*', version)
        assert match, 'installed CLI has no compiled source identity'
        for name in CLI_COMPONENTS:
            assert subprocess.check_output(['git', 'show', match[1] + ':' + name]) == (actual / name).read_bytes(), 'CLI component differs'
        expected = tomllib.loads((actual / 'infra/dispatcher/rules/agent-run-delivers-when-its-step-is-done.toml').read_text())['rule'][0]
        raw = sys.stdin.buffer.read(2097153)
        assert len(raw) <= 2097152, 'rule registry exceeds proof byte bound'
        active_rule = rule_contract(json.loads(raw), expected)
    except (OSError, ValueError, AssertionError, subprocess.SubprocessError) as error:
        print('not yet: installed collected-car artifact unavailable or mismatched: ' + str(error))
        return 75
    exercise(binary, True, digests)
    assert Path('/var/lib/boss/tree/current').resolve(strict=True) == actual, 'generation changed during proof'
    print(json.dumps({'installed_cli_version': version.strip(), 'active_delivery_rule': active_rule}))
    print('installed CLI refuses collected-car hold and review without writes')
    return 0


if __name__ == '__main__':
    sys.exit(main())
