#!/usr/bin/env python3
"""Installed maintenance recovery proof, with one shared localhost fixture.

Default mode requires the actual protected forge installation. --self-test
exercises source code without claiming deployment. --fixture-api is a local
file-backed transport used by both the shell tests and the HTTP fixture;
it opens no network connection. No mode reads the system of record.
"""
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from threading import Thread
from urllib.parse import parse_qs, urlsplit


def fixture_api(arguments):
    args = iter(arguments)
    method, data, url, actor = 'GET', None, '', None
    for arg in args:
        if arg == '-X':
            method = next(args)
        elif arg == '-H':
            header = next(args)
            if header.lower().startswith('x-boss-user:'):
                actor = json.loads(header.split(':', 1)[1])['id']
        elif arg in ('--max-time', '--connect-timeout'):
            next(args)
        elif arg in ('-d', '--data-binary'):
            raw = next(args)
            data = json.loads(Path(raw[1:]).read_text() if raw.startswith('@') else raw)
        elif arg.startswith(('http://', 'https://')):
            url = arg
    if actor not in ('automation:maintenance-timer', 'automation:boss-step'):
        raise ValueError('fixture request did not carry its actual named automation header')
    if urlsplit(url).hostname != 'fixture.invalid':
        raise ValueError('file transport accepts only its fictional fixture identity')
    store = Path(os.environ['FIXTURE_STORE'])
    jobs = json.loads(store.read_text())
    path = urlsplit(url).path
    with Path(os.environ['FIXTURE_REQUESTS']).open('a') as log:
        log.write(json.dumps({'method': method, 'path': path, 'actor': actor, 'data': data}) + '\n')
    if method == 'GET':
        if path == '/api/jobs/old':
            if os.environ.get('FIXTURE_READBACK') == 'unread':
                return 22
            if os.environ.get('FIXTURE_READBACK') == 'empty':
                return 0
            delayed = os.environ.get('FIXTURE_CLOSE_AFTER_READS')
            if delayed:
                counter = store.with_suffix('.reads')
                reads = int(counter.read_text()) + 1 if counter.exists() else 1
                counter.write_text(str(reads))
                if reads >= int(delayed):
                    jobs[0]['status'] = 'closed'
                    jobs[0]['steps'][1]['status'] = 'completed'
                    store.write_text(json.dumps(jobs))
        if path == '/api/jobs':
            kind = parse_qs(urlsplit(url).query)['kind'][0]
            rows = [j for j in jobs if j['kind'] == kind and j['status'] == 'open']
            print(json.dumps({'data': rows, 'total': len(rows)}))
        else:
            print(json.dumps(next(j for j in jobs if j['id'] == path.split('/')[3])))
    elif method == 'POST':
        job = {**data, 'id': 'new', 'status': 'open', 'steps': [
            {'id': 'new-run', 'job_id': 'new', 'spec_slug': 'run', 'title': 'run',
             'status': 'active', 'metadata': {}}]}
        jobs.append(job)
        print(json.dumps(job))
    else:
        job = next(j for j in jobs if j['id'] == path.split('/')[3])
        step = next(s for s in job['steps'] if s['id'] == path.split('/')[5])
        if method == 'PATCH':
            if step['status'] in ('completed', 'skipped'):
                return 22
            step['metadata'].update(data)
        elif method == 'PUT':
            if job['id'] == 'old' and os.environ.get('FIXTURE_REFUSE_SETTLE') == '1':
                return 22
            step.update(data)
            # Model the API settle and its independent marker consumer.
            if len(job['steps']) > 1:
                job['steps'][1]['status'] = 'completed'
                job['steps'][1]['metadata']['outcome_kind'] = (
                    'completed' if step['metadata']['result'] == 'ok' else 'aborted')
            if job['id'] != 'old' or not os.environ.get('FIXTURE_CLOSE_AFTER_READS'):
                job['status'] = 'closed'
        else:
            raise ValueError('fixture method is not supported')
        print('{}')
    store.write_text(json.dumps(jobs))
    return 0


def installed_root():
    managed = Path('/var/lib/boss/tree/current')
    actual = managed.resolve(strict=True)
    if not re.fullmatch(r'/var/lib/boss/tree/gen/[0-9a-f]{40}', str(actual)):
        raise ValueError('managed selector has no fixed generation identity')
    for path in (actual, actual.parent):
        info = path.stat()
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('managed generation is not protected root-owned source')
    if Path(__file__).resolve() != actual / 'infra/forge/probe-maintenance-recovery.py':
        raise ValueError('this is not the actual installed helper')
    digests = {}
    for name in ('infra/boss-maintenance-wrap.sh', 'infra/boss-step.sh',
                 'infra/forge/probe-maintenance-recovery.py'):
        raw = (actual / name).read_bytes()
        expected = subprocess.check_output(['git', 'show', 'HEAD:' + name])
        if raw != expected:
            raise ValueError(name + ' installed bytes do not yet match the proved source')
        digests[name] = hashlib.sha256(raw).hexdigest()
    return managed, actual, digests


def exercise(managed, actual, digests, installed):
    for original in ('ok', 'exit-code'):
        with tempfile.TemporaryDirectory(prefix='boss-maintenance-proof-') as temporary:
            root = Path(temporary)
            old = {'id': 'old', 'kind': 'maintenance-conservation-invariants', 'status': 'open', 'metadata': {},
                   'steps': [{'id': 'old-run', 'job_id': 'old', 'spec_slug': 'run', 'title': 'run', 'status': 'completed',
                              'metadata': {'result': original, 'output': 'original exact output\n'}},
                             {'id': 'old-outcome', 'job_id': 'old', 'spec_slug': 'completed', 'status': 'pending', 'metadata': {}}]}
            store = root / 'store.json'
            store.write_text(json.dumps([old]))
            fixture_env = {'PATH': os.environ['PATH'], 'FIXTURE_STORE': str(store),
                           'FIXTURE_REQUESTS': str(root / 'requests')}

            class Handler(BaseHTTPRequestHandler):
                def answer(self):
                    size = int(self.headers.get('Content-Length', 0))
                    if size > 2097152:
                        self.send_error(413)
                        return
                    args = [sys.executable, str(Path(__file__).resolve()), '--fixture-api',
                            '-X', self.command, '-H', 'x-boss-user:' + self.headers.get('x-boss-user', '')]
                    if size:
                        args += ['-d', self.rfile.read(size).decode()]
                    args += ['http://fixture.invalid' + self.path]
                    reply = subprocess.run(args, env=fixture_env, capture_output=True)
                    self.send_response(200 if reply.returncode == 0 else 503)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(reply.stdout)))
                    self.end_headers()
                    self.wfile.write(reply.stdout)

                do_GET = answer
                do_POST = answer
                do_PATCH = answer
                do_PUT = answer

                def log_message(self, *_):
                    pass  # Complete structured requests ride the proof receipt.

            server = HTTPServer(('127.0.0.1', 0), Handler)
            thread = Thread(target=server.serve_forever)
            thread.start()
            env = {'PATH': os.environ['PATH'], 'HOME': str(root), 'TMPDIR': str(root),
                   'BOSS_JOBS_URL': 'http://127.0.0.1:' + str(server.server_port),
                   'KUBERNETES_SERVICE_HOST': 'fixture',
                   'BOSS_MACHINE_TOKEN_DIR': str(root / 'absent-token'),
                   'BOSS_MACHINE_TOKEN_HOSTS': 'never.invalid', 'BOSS_API_RETRY_DEADLINE': '3'}
            try:
                for command in ([str(managed / 'infra/boss-maintenance-wrap.sh'), old['kind'], 'Conservation'],
                                [str(managed / 'infra/boss-step.sh'), old['kind'], 'run', 'result=ok', 'output=current distinct output']):
                    result = subprocess.run(['bash', *command], env=env, capture_output=True, text=True, timeout=120)
                    if result.returncode:
                        raise RuntimeError(result.stdout + result.stderr)
                jobs = json.loads(store.read_text())
                assert len(jobs) == 2, 'current invocation has no distinct record'
                assert jobs[0]['steps'][0] == old['steps'][0], 'original completion changed'
                assert jobs[0]['status'] == jobs[1]['status'] == 'closed'
                assert jobs[1]['steps'][0]['metadata']['output'] == 'current distinct output'
                assert jobs[0]['steps'][1]['metadata']['outcome_kind'] == ('completed' if original == 'ok' else 'aborted')
                print(json.dumps({'installed': installed, 'artifact_root': str(actual), 'script_sha256': digests,
                    'original_result': original, 'original_preserved': True, 'distinct_current_recorded': True,
                    'fixture_requests': [json.loads(row) for row in (root / 'requests').read_text().splitlines()]}))
                assert managed.resolve(strict=True) == actual, 'managed generation changed during proof'
            finally:
                server.shutdown()
                thread.join()
                server.server_close()


def main():
    if sys.argv[1:2] == ['--fixture-api']:
        return fixture_api(sys.argv[2:])
    if sys.argv[1:] == ['--self-test']:
        root = Path(__file__).resolve().parents[2]
        exercise(root, root, {}, False)
        print('maintenance recovery fixture self-test passed')
        return 0
    if sys.argv[1:]:
        raise ValueError('usage: probe-maintenance-recovery.py [--self-test]')
    try:
        managed, actual, digests = installed_root()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print('not yet: installed maintenance artifact unavailable or mismatched: ' + str(error))
        return 75
    exercise(managed, actual, digests, True)
    print('installed maintenance recovery records each invocation')
    return 0


if __name__ == '__main__':
    sys.exit(main())
