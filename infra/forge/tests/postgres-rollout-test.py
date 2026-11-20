"""Drive the runner's real apply boundary with an uncredentialed kubectl port."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import argparse
import base64
import copy
from datetime import datetime, timedelta, timezone
import fcntl
import importlib.util
import http.server
import shutil
import sys
import threading
from urllib.parse import urlsplit, parse_qs

ROOT = Path(__file__).resolve().parents[3]
MODULE_SPEC = importlib.util.spec_from_file_location('postgres_rollout', ROOT / 'infra/forge/postgres-rollout.py')
MODULE = importlib.util.module_from_spec(MODULE_SPEC)
MODULE_SPEC.loader.exec_module(MODULE)


def node(name, labels, ready=True, cordoned=False, taints=()):
    """A Node as `kubectl get nodes -o json` returns one, reduced to what placement reads."""
    return {'metadata': {'name': name, 'labels': dict(labels, **{'kubernetes.io/hostname': name})},
            'spec': dict({'unschedulable': True} if cordoned else {}, **({'taints': list(taints)} if taints else {})),
            'status': {'conditions': [{'type': 'Ready', 'status': 'True' if ready else 'False'}]}}


# The car 526974b0 pin, verbatim in shape: required node affinity on the
# data node's purpose label (infra/cluster/manifests/boss.yaml on that car).
DATA_NODE_AFFINITY = {'nodeAffinity': {'requiredDuringSchedulingIgnoredDuringExecution': {'nodeSelectorTerms': [
    {'matchExpressions': [{'key': 'boss.dev/purpose', 'operator': 'In', 'values': ['database']}]}]}}}

# The pin boss.yaml carried before car 526974b0 and carries again after the
# revert #1787: a plain nodeSelector on the control-plane role label.
CONTROL_PLANE_SELECTOR = {'node-role.kubernetes.io/control-plane': ''}


def stale_pod(status):
    """postgres-0 left on the failed forward revision, owned by the StatefulSet."""
    return {'metadata': {'name': 'postgres-0', 'uid': 'pod-uid', 'labels': {'controller-revision-hash': 'bad'},
                         'ownerReferences': [{'name': 'postgres', 'uid': 'sts-uid', 'kind': 'StatefulSet', 'controller': True}]},
            'status': status}


class MemoryRecovery(MODULE.Recovery):
    """Kubectl port only: exercise the production recovery state machine."""
    def __init__(self, directory, failure=True):
        args = argparse.Namespace(namespace='boss', head='1' * 40, state=str(directory),
                                  apply='kubectl', mounted='/manifests/boss',
                                  receipt=str(directory / 'attempt.json'), read='kubectl', write='kubectl',
                                  directory=str(directory), repo=str(ROOT), last='2' * 40)
        super().__init__(args)
        self.desired = {'kind': 'StatefulSet', 'metadata': {'name': 'postgres', 'namespace': 'boss'}, 'spec': {'replicas': 1, 'template': {'version': 'bad'}}}
        self.old = {'replicas': 1, 'template': {'version': 'good'}}
        self.object = {'apiVersion': 'apps/v1', 'kind': 'StatefulSet', 'metadata': {'name': 'postgres', 'namespace': 'boss', 'uid': 'sts-uid', 'resourceVersion': '12', 'generation': 1},
                       'spec': copy.deepcopy(self.old), 'status': {'observedGeneration': 1, 'readyReplicas': 1,
                                                               'updatedReplicas': 1, 'currentRevision': 'good', 'updateRevision': 'good'}}
        self.failure = failure
        self.commands = []
        self.pods = []
        # The real cluster's two candidate homes for postgres-0: a Ready,
        # untainted control plane and the Ready, untainted data node w-3
        # carrying boss.dev/purpose=database (node-labels 7d3bdf90). With
        # cp-2 alone, a rendered manifest carrying car 526974b0's data-node
        # pin was refused by the placement check before the readiness path
        # these tests exercise (e7cbffa9). Each pin then fits exactly one.
        self.nodes = [node('cp-2', {'node-role.kubernetes.io/control-plane': ''}),
                      node('w-3', {'boss.dev/purpose': 'database'})]
        self.rollback_failure = False
        self.replace_identity = False
        self.rollback_waits = 0
        self.reject_patch = False
        self.lose_patch_ack = False
        self.replace_on_ready = False

    def run(self, argv, body=None, check=True, timeout=480, env=None):
        if argv[0] == 'git':
            return {'stdout': argv[-1].removesuffix('^{commit}'), 'exit_code': 0}
        if argv[0] == 'kubectl':
            return self.kubectl(argv[1:], body, check)
        return super().run(argv, body, check, timeout=timeout, env=env)

    def extract(self, directory, remove=False):
        return self.desired

    def named_good(self, live):
        return {'namespace': 'boss', 'name': 'postgres', 'source': '2' * 40, 'uid': 'sts-uid', 'spec': copy.deepcopy(self.old)}

    def kubectl(self, argv, body=None, check=True, timeout=480):
        self.commands.append((argv, body))
        if argv[0] == 'patch' and '--dry-run=server' in argv:
            patch = json.loads(body if body is not None else argv[argv.index('-p') + 1])
            for key in ('uid', 'resourceVersion'):
                if patch['metadata'][key] != self.object['metadata'][key]:
                    raise ValueError('server rejected dry-run identity fence')
            result = subprocess.run([shutil.which('kubectl'), 'patch', '--local', '--type=strategic',
                                     '-f', '-', '-p', json.dumps(patch), '-o', 'json'],
                                    input=json.dumps(self.object), capture_output=True, text=True, timeout=10)
            if result.returncode:
                raise ValueError(result.stderr)
            merged = json.loads(result.stdout)
            for claim in merged['spec'].get('volumeClaimTemplates', []):
                claim.setdefault('apiVersion', 'v1')
                claim.setdefault('kind', 'PersistentVolumeClaim')
                claim.setdefault('status', {'phase': 'Pending'})
                claim['spec'].setdefault('volumeMode', 'Filesystem')
            return {'stdout': json.dumps(merged), 'exit_code': 0}
        if argv[0] == 'patch' and '--local' in argv:
            result = subprocess.run([shutil.which('kubectl'), *argv], input=body,
                                    capture_output=True, text=True, timeout=10)
            if result.returncode:
                raise ValueError('local strategic merge refused: ' + result.stderr)
            return {'stdout': result.stdout, 'exit_code': 0}
        if argv[:2] == ['get', 'sts/postgres']:
            return {'stdout': json.dumps(self.object) if self.object else '', 'exit_code': 0}
        if argv[0] == 'patch':
            operations = json.loads(argv[argv.index('-p') + 1])
            assert operations[0] == {'op': 'test', 'path': '/metadata/uid', 'value': 'sts-uid'}
            assert operations[1]['path'] == '/metadata/resourceVersion'
            if self.reject_patch:
                raise ValueError('server rejected stale resourceVersion')
            for operation in operations:
                keys = [x.replace('~1', '/').replace('~0', '~') for x in operation['path'].lstrip('/').split('/')]
                parent = self.object
                for key in keys[:-1]:
                    parent = parent[key]
                key = keys[-1]
                if operation['op'] == 'test':
                    if parent.get(key) != operation['value']:
                        raise ValueError('server rejected changed JSON Patch value')
                elif operation['op'] == 'remove':
                    del parent[key]
                else:
                    parent[key] = copy.deepcopy(operation['value'])
            self.object['status']['updateRevision'] = self.revision()
            if self.lose_patch_ack:
                self.lose_patch_ack = False
                raise ValueError('write ACK lost after server accepted patch')
        if argv[0] == 'rollout':
            bad = self.object is None or self.revision() == getattr(self, 'bad_revision', 'bad')
            if not bad:
                self.rollback_waits += 1
                if self.pods and self.rollback_waits == 1:
                    raise ValueError('stale Pod requires forced rollback recovery')
            if (bad and self.failure) or (not bad and self.rollback_failure):
                if self.replace_identity:
                    self.object['metadata']['uid'] = 'replacement'
                raise ValueError('actual rollout port refused readiness')
            self.object['status']['currentRevision'] = self.object['status']['updateRevision']
            if self.replace_on_ready:
                self.object['metadata']['uid'] = 'replacement'
        if argv[:2] == ['get', 'pods']:
            return {'stdout': json.dumps({'items': self.pods}), 'exit_code': 0}
        if argv[:2] == ['get', 'nodes']:
            return {'stdout': json.dumps({'kind': 'NodeList', 'items': self.nodes}), 'exit_code': 0}
        return {'stdout': '{}', 'exit_code': 0}

    def revision(self):
        template = self.object['spec']['template']
        return template.get('version') or template['spec']['containers'][0]['image']


class RecoveryContracts(unittest.TestCase):
    def test_real_merged_spec_argv_reaches_only_owned_loopback_patch(self):
        kubectl = shutil.which('kubectl')
        self.assertIsNotNone(kubectl, 'real kubectl is required for the invocation contract')
        captured = []
        with tempfile.TemporaryDirectory() as temporary:
            recovery = MemoryRecovery(Path(temporary), failure=False)
            obj = copy.deepcopy(recovery.object)
            class Handler(http.server.BaseHTTPRequestHandler):
                def answer(self, value):
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.end_headers()
                    self.wfile.write(json.dumps(value).encode())
                def do_GET(self):
                    path = urlsplit(self.path).path
                    if path == '/api':
                        value = {'kind': 'APIVersions', 'versions': ['v1']}
                    elif path == '/apis':
                        value = {'kind': 'APIGroupList', 'groups': [{'name': 'apps', 'versions': [{'groupVersion': 'apps/v1', 'version': 'v1'}], 'preferredVersion': {'groupVersion': 'apps/v1', 'version': 'v1'}}]}
                    elif path == '/apis/apps/v1':
                        value = {'kind': 'APIResourceList', 'groupVersion': 'apps/v1', 'resources': [{'name': 'statefulsets', 'singularName': 'statefulset', 'namespaced': True, 'kind': 'StatefulSet', 'verbs': ['get', 'patch'], 'shortNames': ['sts']}]}
                    elif path == '/api/v1':
                        value = {'kind': 'APIResourceList', 'groupVersion': 'v1', 'resources': []}
                    else:
                        value = obj
                    self.answer(value)
                def do_PATCH(self):
                    length = int(self.headers.get('Content-Length', 0))
                    if not 0 < length < 16384:
                        self.send_error(400)
                        return
                    patch = json.loads(self.rfile.read(length))
                    captured.append((self.path, patch))
                    result = copy.deepcopy(obj)
                    result['spec'].update(patch['spec'])
                    self.answer(result)
                def log_message(self, *args):
                    pass
            with http.server.HTTPServer(('127.0.0.1', 0), Handler) as server:
                thread = threading.Thread(target=server.serve_forever)
                thread.start()
                config = Path(temporary) / 'kubeconfig'
                config.write_text(json.dumps({'apiVersion': 'v1', 'kind': 'Config', 'clusters': [{'name': 'owned', 'cluster': {'server': 'http://127.0.0.1:' + str(server.server_port)}}], 'contexts': [{'name': 'owned', 'context': {'cluster': 'owned', 'user': 'none'}}], 'current-context': 'owned', 'users': [{'name': 'none', 'user': {}}]}))
                recovery.read = recovery.write = [kubectl, '--kubeconfig', str(config), '--cache-dir', str(Path(temporary) / 'cache')]
                recovery.run = MODULE.Recovery.run.__get__(recovery)
                recovery.kubectl = MODULE.Recovery.kubectl.__get__(recovery)
                try:
                    merged = recovery.merged_spec(obj, {'replicas': 2})
                    self.assertEqual(merged['replicas'], 2)
                    self.assertEqual(len(captured), 1)
                    uri, patch = captured[0]
                    self.assertEqual(urlsplit(uri).path, '/apis/apps/v1/namespaces/boss/statefulsets/postgres')
                    self.assertEqual(parse_qs(urlsplit(uri).query)['dryRun'], ['All'])
                    self.assertEqual(patch, {'metadata': {'uid': 'sts-uid', 'resourceVersion': '12'}, 'spec': {'replicas': 2}})
                finally:
                    server.shutdown()
                    thread.join(timeout=2)
                    self.assertFalse(thread.is_alive())

    def test_running_wal_recovery_without_restarts_is_never_deleted(self):
        with tempfile.TemporaryDirectory() as temporary:
            recovery = MemoryRecovery(Path(temporary))
            recovery.pods = [{'metadata': {'name': 'postgres-0', 'uid': 'pod-uid', 'labels': {'controller-revision-hash': 'bad'}, 'ownerReferences': [{'name': 'postgres', 'uid': 'sts-uid', 'kind': 'StatefulSet', 'controller': True}]}, 'status': {'phase': 'Running', 'conditions': [{'type': 'Ready', 'status': 'False'}], 'containerStatuses': [{'name': 'postgres', 'restartCount': 0, 'state': {'running': {'startedAt': '2026-11-01T00:00:00Z'}}}]}}]
            with self.assertRaises(ValueError):
                recovery.execute()
            self.assertFalse(any(a[0] == 'delete' for a, _ in recovery.commands))

    def test_interrupted_forward_is_reconciled_by_a_fresh_instance(self):
        class LostProcess(BaseException):
            pass
        with tempfile.TemporaryDirectory() as temporary:
            first = self.seed(Path(temporary))
            interrupted = self.subsequent(first, '1' * 40)
            interrupted.wait = lambda uid: (_ for _ in ()).throw(LostProcess())
            with self.assertRaises(LostProcess):
                interrupted.execute()
            marker = json.loads((Path(temporary) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).read_text())
            self.assertEqual(marker['forward_inverse'], interrupted.record['forward_inverse'])
            following = self.subsequent(interrupted, '1' * 40)
            with self.assertRaisesRegex(ValueError, 'quarantin'):
                following.execute()
            self.assertEqual(following.object['spec'], first.old)
            self.assertEqual(following.record['rollback'], 'ready')
            self.assertFalse(any(a[0] in ('apply', 'create') for a, _ in following.commands))

    def test_interrupted_forward_is_reconciled_before_a_different_head(self):
        class LostProcess(BaseException):
            pass
        with tempfile.TemporaryDirectory() as temporary:
            first = self.seed(Path(temporary))
            interrupted = self.subsequent(first, '1' * 40)
            interrupted.wait = lambda uid: (_ for _ in ()).throw(LostProcess())
            with self.assertRaises(LostProcess):
                interrupted.execute()
            following = self.subsequent(interrupted, '3' * 40)
            with self.assertRaises(ValueError):
                following.execute()
            self.assertEqual(following.object['spec'], first.old)
            self.assertEqual(following.record['rollback'], 'ready')
            self.assertFalse(any(a[0] in ('apply', 'create') for a, _ in following.commands))

    def test_recorded_ready_target_is_not_undone_after_marker_interruption(self):
        class LostProcess(BaseException):
            pass
        with tempfile.TemporaryDirectory() as temporary:
            first = self.seed(Path(temporary))
            interrupted = self.subsequent(first, '1' * 40)
            original = MODULE.atomic
            def lose_ready_marker(path, value):
                if path.name.startswith('postgres-quarantine-') and value.get('state') == 'ready':
                    raise LostProcess()
                return original(path, value)
            MODULE.atomic = lose_ready_marker
            try:
                with self.assertRaises(LostProcess):
                    interrupted.execute()
            finally:
                MODULE.atomic = original
            following = self.subsequent(interrupted, '3' * 40)
            with self.assertRaises(ValueError):
                following.execute()
            self.assertEqual(following.object['spec'], interrupted.desired['spec'])
            self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete') and '--dry-run=server' not in a for a, _ in following.commands))

    def test_active_reference_rejects_inverse_digest_drift(self):
        class LostProcess(BaseException):
            pass
        with tempfile.TemporaryDirectory() as temporary:
            first = self.seed(Path(temporary))
            interrupted = self.subsequent(first, '1' * 40)
            interrupted.wait = lambda uid: (_ for _ in ()).throw(LostProcess())
            with self.assertRaises(LostProcess):
                interrupted.execute()
            path = Path(temporary) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')
            journal = json.loads(path.read_text())
            journal['forward_inverse']['changes'][0]['before'] = 'tampered'
            MODULE.atomic(path, journal)
            following = self.subsequent(interrupted, '3' * 40)
            with self.assertRaisesRegex(ValueError, 'digest'):
                following.execute()
            self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete') for a, _ in following.commands))

    def test_overlapping_namespace_recovery_refuses_before_any_port_call(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            recovery = MemoryRecovery(directory, failure=False)
            with (directory / 'postgres-recovery-boss.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaisesRegex(ValueError, 'already active'):
                    recovery.execute()
            self.assertFalse(recovery.commands)

    def test_no_write_failures_do_not_quarantine_the_head(self):
        for phase in ('dry-run', 'unchanged-wait'):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                recovery = MemoryRecovery(Path(temporary))
                if phase == 'dry-run':
                    recovery.merged_spec = lambda *args: (_ for _ in ()).throw(ValueError('dry-run refused'))
                else:
                    recovery.desired['spec'] = copy.deepcopy(recovery.old)
                    recovery.wait = lambda uid: (_ for _ in ()).throw(ValueError('readiness transient'))
                with self.assertRaises(ValueError):
                    recovery.execute()
                self.assertFalse((Path(temporary) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).exists())
                self.assertFalse(any(a[0] == 'patch' and '--dry-run=server' not in a for a, _ in recovery.commands))

    def test_interrupted_inverse_refuses_replacement_and_foreign_array_drift(self):
        class LostProcess(BaseException):
            pass
        for drift in ('identity', 'array', 'journal'):
            with self.subTest(drift=drift), tempfile.TemporaryDirectory() as temporary:
                first = self.seed(Path(temporary), containers=True)
                interrupted = self.subsequent(first, '1' * 40)
                interrupted.desired['spec']['template']['spec'] = {'containers': [{'name': 'postgres', 'image': 'postgres:17'}]}
                interrupted.wait = lambda uid: (_ for _ in ()).throw(LostProcess())
                with self.assertRaises(LostProcess):
                    interrupted.execute()
                following = self.subsequent(interrupted, '1' * 40)
                if drift == 'identity':
                    following.object['metadata']['uid'] = 'replacement'
                elif drift == 'array':
                    following.object['spec']['template']['spec']['containers'].append({'name': 'foreign', 'image': 'kept'})
                else:
                    path = Path(temporary) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')
                    journal = json.loads(path.read_text())
                    journal['forward_inverse'] = None
                    MODULE.atomic(path, journal)
                original = copy.deepcopy(following.object)
                with self.assertRaises(ValueError):
                    following.execute()
                self.assertEqual(following.object, original)
                self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete') for a, _ in following.commands))

    def test_stored_kubernetes_stdout_omits_only_managed_fields(self):
        with tempfile.TemporaryDirectory() as temporary:
            recovery = MemoryRecovery(Path(temporary))
            obj = copy.deepcopy(recovery.object)
            obj['metadata']['managedFields'] = [{'manager': 'large-field-manager'}]
            raw = json.dumps(obj)
            event = MODULE.Recovery.run(recovery, [sys.executable, '-c', 'import sys; sys.stdout.write(sys.argv[1]); sys.stderr.write("full diagnostic")', raw])
            stored = json.loads(Path(recovery.args.receipt).read_text())['commands'][0]
            expected = copy.deepcopy(obj)
            del expected['metadata']['managedFields']
            self.assertEqual(json.loads(stored['stdout']), expected)
            self.assertEqual(stored['stderr'], 'full diagnostic')
            self.assertEqual(json.loads(event['stdout']), obj)
            self.assertEqual(stored['stdout_original_bytes'], len(raw.encode()))
            self.assertEqual(stored['stdout_original_sha256'], MODULE.hashlib.sha256(raw.encode()).hexdigest())

    def rendered_recovery(self, directory, failure=False):
        rendered = directory / 'rendered'
        subprocess.run(['bash', str(ROOT / 'infra/cluster/render-instance.sh'), '--all', str(rendered)],
                       check=True, capture_output=True, text=True, timeout=60)
        source = MODULE.Recovery(argparse.Namespace(read='kubectl', write='kubectl', namespace='boss', head='2' * 40))
        # extract's document reader and real kubectl conversion use the actual rendered manifest.
        source.kubectl = lambda argv, body=None, **kw: {
            'stdout': subprocess.check_output([shutil.which('kubectl'), 'patch', '--local', '--type=strategic',
                                               '-f', '-', '-p', '{}', '-o', 'json'], input=body, text=True),
            'exit_code': 0}
        obj = source.extract(rendered / 'boss')
        self.assertTrue(obj['spec']['volumeClaimTemplates'])
        recovery = MemoryRecovery(directory, failure=failure)
        recovery.old = copy.deepcopy(obj['spec'])
        recovery.desired = copy.deepcopy(obj)
        recovery.object['spec'] = copy.deepcopy(recovery.old)
        for claim in recovery.object['spec']['volumeClaimTemplates']:
            claim.update(apiVersion='v1', kind='PersistentVolumeClaim', status={'phase': 'Pending'})
            claim['spec']['volumeMode'] = 'Filesystem'
        revision = recovery.revision()
        recovery.object['status'].update(currentRevision=revision, updateRevision=revision)
        recovery.bad_revision = revision + '-candidate'
        recovery.desired['spec']['template']['spec']['containers'][0]['image'] = recovery.bad_revision
        return recovery

    def test_rendered_claim_defaults_survive_adoption_forward_and_rollback(self):
        for failure in (False, True):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                recovery = self.rendered_recovery(Path(temporary), failure)
                original = copy.deepcopy(recovery.object['spec'])
                self.assertEqual(recovery.merged_spec(recovery.object, recovery.old), original)
                if failure:
                    with self.assertRaisesRegex(ValueError, 'readiness'):
                        recovery.execute()
                    self.assertEqual(recovery.record['rollback'], 'ready')
                    self.assertEqual(recovery.object['spec'], original)
                else:
                    recovery.execute()
                    self.assertEqual(recovery.record['outcome'], 'ready')
                    self.assertEqual(recovery.object['spec']['volumeClaimTemplates'], original['volumeClaimTemplates'])

    def test_ordinary_exception_after_forward_quarantines_and_rolls_back(self):
        with tempfile.TemporaryDirectory() as temporary:
            recovery = MemoryRecovery(Path(temporary), failure=False)
            original = recovery.wait
            calls = []
            def fail_once(expected_uid):
                calls.append(True)
                if len(calls) == 1:
                    raise TypeError('post-forward observation malformed')
                return original(expected_uid)
            recovery.wait = fail_once
            with self.assertRaisesRegex(TypeError, 'post-forward'):
                recovery.execute()
            self.assertEqual(recovery.record['outcome'], 'failed')
            self.assertEqual(recovery.record['rollback'], 'ready')
            self.assertEqual(recovery.object['spec'], recovery.old)
            quarantine = Path(temporary) / ('postgres-quarantine-boss-' + recovery.args.head + '.json')
            self.assertEqual(json.loads(quarantine.read_text())['state'], 'failed')

    def subsequent(self, previous, head, failure=False):
        recovery = MemoryRecovery(Path(previous.args.state), failure=failure)
        recovery.args.namespace = previous.args.namespace
        recovery.args.head = head
        recovery.args.last = previous.args.head
        recovery.object = copy.deepcopy(previous.object)
        recovery.desired['metadata']['namespace'] = recovery.args.namespace
        recovery.named_good = lambda live: (_ for _ in ()).throw(AssertionError('global stamp bootstrap must not replace namespace ready pointer'))
        return recovery

    def seed(self, directory, namespace='boss', containers=False):
        recovery = MemoryRecovery(directory, failure=False)
        recovery.args.namespace = namespace
        recovery.args.head = '2' * 40
        recovery.object['metadata']['namespace'] = namespace
        recovery.desired['metadata']['namespace'] = namespace
        if containers:
            recovery.old['template']['spec'] = {'containers': [{'name': 'postgres', 'image': 'postgres:16'}]}
        recovery.object['spec'] = copy.deepcopy(recovery.old)
        recovery.desired['spec'] = copy.deepcopy(recovery.old)
        recovery.named_good = lambda live: dict(namespace=namespace, name='postgres', source='2' * 40,
                                                uid='sts-uid', spec=copy.deepcopy(recovery.old))
        recovery.execute()
        return recovery

    def test_namespace_ready_target_survives_failed_and_skipped_global_stamp(self):
        for skipped in (False, True):
            with self.subTest(skipped=skipped), tempfile.TemporaryDirectory() as directory:
                first = self.seed(Path(directory), 'boss-playground')
                if not skipped:
                    failed = self.subsequent(first, '1' * 40, failure=True)
                    with self.assertRaises(ValueError):
                        failed.execute()
                    self.assertEqual(failed.record['rollback'], 'ready')
                    first = failed
                following = self.subsequent(first, '3' * 40)
                following.args.last = '1' * 40
                following.desired['spec']['template']['version'] = 'next'
                following.execute()
                self.assertEqual(following.record['rollback_target']['source'], '2' * 40)
                self.assertEqual(following.record['outcome'], 'ready')

    def test_failed_head_is_quarantined_across_recovery_instances(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.seed(Path(directory))
            failed = self.subsequent(first, '1' * 40, failure=True)
            with self.assertRaises(ValueError):
                failed.execute()
            repeated = self.subsequent(failed, '1' * 40, failure=True)
            repeated.args.last = first.args.head
            with self.assertRaisesRegex(ValueError, 'quarantin'):
                repeated.execute()
            self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete', 'rollout') for a, _ in repeated.commands))
            following = self.subsequent(failed, '3' * 40)
            following.args.last = first.args.head
            following.desired['spec']['template']['version'] = 'next'
            following.execute()
            self.assertEqual(following.record['outcome'], 'ready')

    def bootstrap(self, directory, create_lands, failure):
        """A fresh namespace: no live StatefulSet, so the tick must create one.

        bd5964f2: with `create_lands` the fake answers `create` with the new
        object (uid boot-uid) whose first readiness wait fails when `failure`
        is set; without it the create answers no identity, as a refused
        create does.
        """
        recovery = MemoryRecovery(Path(directory), failure=failure)
        recovery.object = None
        recovery.args.head = '1' * 40
        recovery.desired['spec'] = {'replicas': 1, 'template': {'version': 'boot'}}
        recovery.bad_revision = 'boot'
        recovery.named_good = lambda live: (_ for _ in ()).throw(AssertionError('a bootstrap adopts no named target'))
        # The journaled head renders to this fixture's desired StatefulSet.
        recovery.rendered_postgres = lambda source: copy.deepcopy(recovery.desired)
        if create_lands:
            original = recovery.kubectl

            def kubectl(argv, body=None, check=True, timeout=480):
                if argv[0] == 'create':
                    recovery.commands.append((argv, body))
                    desired = json.loads(body)
                    recovery.object = {'apiVersion': 'apps/v1', 'kind': 'StatefulSet',
                                       'metadata': {'name': 'postgres', 'namespace': 'boss', 'uid': 'boot-uid',
                                                    'resourceVersion': '1', 'generation': 1},
                                       'spec': copy.deepcopy(desired['spec']),
                                       'status': {'observedGeneration': 1, 'readyReplicas': 1, 'updatedReplicas': 1,
                                                  'currentRevision': 'boot', 'updateRevision': 'boot'}}
                    return {'stdout': json.dumps(recovery.object), 'exit_code': 0}
                return original(argv, body, check, timeout)
            recovery.kubectl = kubectl
        return recovery

    def test_a_slow_bootstrap_is_adopted_once_it_is_ready_instead_of_refusing_forever(self):
        # bd5964f2: the first readiness wait after a bootstrap create fails
        # (a fresh Longhorn volume and an image pull can exceed the bound).
        # The journal it leaves has nothing to undo, so the next tick must
        # verify the live StatefulSet and deliver, not refuse every tick.
        with tempfile.TemporaryDirectory() as directory:
            first = self.bootstrap(Path(directory), create_lands=True, failure=True)
            with self.assertRaisesRegex(ValueError, 'readiness'):
                first.execute()
            active = Path(directory) / 'postgres-active-journal-boss.json'
            quarantine = Path(directory) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')
            self.assertTrue(active.exists())
            journal = json.loads(quarantine.read_text())
            self.assertEqual(journal['state'], 'failed')
            self.assertEqual(journal['rollback_target'], {'state': 'not-applicable-bootstrap'})
            self.assertIsNone(journal['forward_inverse'])
            second = self.bootstrap(Path(directory), create_lands=True, failure=False)
            second.object = copy.deepcopy(first.object)
            second.execute()
            self.assertEqual(second.record['outcome'], 'ready')
            self.assertEqual(second.record['interrupted_forward'], 'bootstrap-verified-ready')
            self.assertFalse(active.exists())
            settled = json.loads(quarantine.read_text())
            self.assertEqual((settled['state'], settled['uid']), ('ready', 'boot-uid'))
            pointer = json.loads((Path(directory) / 'postgres-last-ready-boss.json').read_text())
            self.assertEqual((pointer['source'], pointer['uid']), ('1' * 40, 'boot-uid'))
            self.assertFalse(any(a[0] in ('create', 'delete') for a, _ in second.commands),
                             'the live StatefulSet is adopted, never recreated or rolled')
            third = self.bootstrap(Path(directory), create_lands=True, failure=False)
            third.object = copy.deepcopy(second.object)
            third.execute()
            self.assertEqual(third.record['outcome'], 'ready')
            self.assertNotIn('interrupted_forward', third.record)

    def test_a_bootstrap_whose_create_landed_nothing_is_retried_by_the_next_tick(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.bootstrap(Path(directory), create_lands=False, failure=False)
            with self.assertRaisesRegex(ValueError, 'no qualified identity'):
                first.execute()
            active = Path(directory) / 'postgres-active-journal-boss.json'
            self.assertTrue(active.exists())
            second = self.bootstrap(Path(directory), create_lands=True, failure=False)
            second.execute()
            self.assertEqual(second.record['outcome'], 'ready')
            self.assertEqual(second.record['interrupted_forward'], 'bootstrap-created-nothing')
            self.assertEqual(sum(1 for a, _ in second.commands if a[0] == 'create'), 1)
            self.assertFalse(active.exists())
            settled = json.loads((Path(directory) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).read_text())
            self.assertEqual((settled['state'], settled['uid']), ('ready', 'boot-uid'))

    def test_a_bootstrap_journal_refuses_a_live_statefulset_that_is_not_its_head(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.bootstrap(Path(directory), create_lands=True, failure=True)
            with self.assertRaisesRegex(ValueError, 'readiness'):
                first.execute()
            for drift, pattern in ((('spec', 'template', 'version'), 'does not match'), ((), 'readiness')):
                second = self.bootstrap(Path(directory), create_lands=True, failure=not drift)
                second.object = copy.deepcopy(first.object)
                if drift:
                    second.object['spec']['template']['version'] = 'other'
                with self.subTest(drift=drift), self.assertRaisesRegex(ValueError, pattern):
                    second.execute()
                self.assertTrue((Path(directory) / 'postgres-active-journal-boss.json').exists(),
                                'a refused resume keeps the journal for the next tick')
                self.assertFalse(any(a[0] in ('create', 'delete', 'patch') and '--dry-run=server' not in a
                                     for a, _ in second.commands))

    def test_no_attempt_marker_precedes_forward_preparation(self):
        class AbruptExit(BaseException):
            pass
        with tempfile.TemporaryDirectory() as directory:
            first = self.seed(Path(directory))
            interrupted = self.subsequent(first, '1' * 40)
            interrupted.patch = lambda *args: (_ for _ in ()).throw(AbruptExit())
            with self.assertRaises(AbruptExit):
                interrupted.execute()
            self.assertFalse((Path(directory) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).exists())

    def test_actual_child_exit_retains_prewrite_quarantine(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.seed(Path(directory))
            script = '''import importlib.util, os, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('owned_recovery_fixture', sys.argv[1])
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
recovery = fixture.MemoryRecovery(Path(sys.argv[2]))
recovery.args.head = '1' * 40
original = recovery.journal_forward
def exit_before_write(*args):
    original(*args)
    print('owned child exits after durable attempt marker, before PostgreSQL write', flush=True)
    os._exit(91)
recovery.journal_forward = exit_before_write
recovery.execute()
'''
            child = subprocess.run([sys.executable, '-B', '-c', script, str(Path(__file__).resolve()), directory],
                                   capture_output=True, text=True, timeout=15)
            self.assertEqual(child.returncode, 91, child.stdout + child.stderr)
            print(json.dumps({'owned_child_exit': child.returncode, 'stdout': child.stdout, 'stderr': child.stderr}))
            marker = json.loads((Path(directory) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).read_text())
            self.assertEqual(marker['state'], 'attempting')
            repeated = self.subsequent(first, '1' * 40)
            with self.assertRaisesRegex(ValueError, 'quarantin'):
                repeated.execute()
            self.assertEqual(repeated.record['rollback'], 'ready')
            self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete') for a, _ in repeated.commands))

    def test_foreign_annotation_sidecar_and_env_survive_forward_and_rollback(self):
        for failure in (False, True):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                first = self.seed(Path(directory), containers=True)
                following = self.subsequent(first, '1' * 40, failure=failure)
                following.object['apiVersion'] = 'apps/v1'
                following.object['spec']['template']['metadata'] = {'annotations': {'kubectl.kubernetes.io/restartedAt': 'foreign'}}
                following.object['spec']['template']['spec'] = {'containers': [
                    {'name': 'postgres', 'image': 'postgres:16', 'env': [{'name': 'FOREIGN', 'value': 'kept'}]},
                    {'name': 'foreign-sidecar', 'image': 'sidecar:1'}]}
                following.desired['spec']['template']['spec'] = {'containers': [{'name': 'postgres', 'image': 'postgres:17'}]}
                if failure:
                    with self.assertRaises(ValueError):
                        following.execute()
                else:
                    following.execute()
                template = following.object['spec']['template']
                self.assertEqual(template.get('metadata', {}).get('annotations'), {'kubectl.kubernetes.io/restartedAt': 'foreign'})
                by_name = {c['name']: c for c in template['spec']['containers']}
                self.assertEqual(by_name['foreign-sidecar']['image'], 'sidecar:1')
                self.assertEqual(by_name['postgres']['env'], [{'name': 'FOREIGN', 'value': 'kept'}])
                self.assertEqual(by_name['postgres']['image'], 'postgres:16' if failure else 'postgres:17')

    def test_rollback_removes_bad_args_added_by_failed_desired(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.seed(Path(directory), containers=True)
            following = self.subsequent(first, '1' * 40, failure=True)
            following.desired['spec']['template']['spec'] = {'containers': [
                {'name': 'postgres', 'image': 'postgres:17', 'args': ['-c', 'invalid_setting=on']}]}
            with self.assertRaises(ValueError):
                following.execute()
            self.assertEqual(following.record['rollback'], 'ready')
            postgres = following.object['spec']['template']['spec']['containers'][0]
            self.assertEqual(postgres['image'], 'postgres:16')
            self.assertNotIn('args', postgres)

    def test_rollback_retains_later_unrelated_edit_or_refuses_changed_array(self):
        for array in (False, True):
            with self.subTest(array=array), tempfile.TemporaryDirectory() as directory:
                first = self.seed(Path(directory), containers=True)
                following = self.subsequent(first, '1' * 40, failure=True)
                following.desired['spec']['template']['spec'] = {'containers': [{'name': 'postgres', 'image': 'postgres:17'}]}
                port = following.kubectl
                def concurrent(argv, body=None, check=True, timeout=480):
                    try:
                        return port(argv, body, check, timeout)
                    except ValueError:
                        if argv[0] == 'rollout' and following.object['spec']['template']['version'] == 'bad':
                            if array:
                                following.object['spec']['template']['spec']['containers'].append({'name': 'late-sidecar', 'image': 'keep:1'})
                            else:
                                following.object['spec']['template']['metadata'] = {'annotations': {'late-writer': 'kept'}}
                        raise
                following.kubectl = concurrent
                with self.assertRaises(ValueError):
                    following.execute()
                if array:
                    self.assertIn('failed', following.record['rollback'])
                    self.assertIn('late-sidecar', [c['name'] for c in following.object['spec']['template']['spec']['containers']])
                else:
                    self.assertEqual(following.record['rollback'], 'ready')
                    self.assertEqual(following.object['spec']['template']['metadata']['annotations'], {'late-writer': 'kept'})

    def test_rollback_restores_two_explicitly_removed_owned_dictionary_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            first = MemoryRecovery(Path(directory), failure=False)
            first.args.head = '2' * 40
            first.old['template']['metadata'] = {'annotations': {'one': 'old', 'two': 'old'}}
            first.object['spec'] = copy.deepcopy(first.old)
            first.desired['spec'] = copy.deepcopy(first.old)
            first.execute()
            following = self.subsequent(first, '1' * 40, failure=True)
            following.desired['spec']['template']['metadata'] = {'annotations': {'one': None, 'two': None}}
            with self.assertRaises(ValueError):
                following.execute()
            self.assertEqual(following.record['rollback'], 'ready')
            self.assertEqual(following.object['spec']['template']['metadata']['annotations'], {'one': 'old', 'two': 'old'})

    def test_present_invalid_namespace_pointer_or_quarantine_refuses_before_mutation(self):
        for kind in ('pointer', 'quarantine', 'receipt'):
            for invalid in ([], {}, 'unread'):
                with self.subTest(kind=kind, invalid=invalid), tempfile.TemporaryDirectory() as directory:
                    first = self.seed(Path(directory))
                    following = self.subsequent(first, '1' * 40)
                    path = Path(directory) / ({'pointer': 'postgres-last-ready-boss.json',
                                               'quarantine': 'postgres-quarantine-boss-' + following.args.head + '.json',
                                               'receipt': 'postgres-ready-boss-' + first.args.head + '.json'}[kind])
                    path.write_text(json.dumps(invalid))
                    with self.assertRaises(ValueError):
                        following.execute()
                    self.assertFalse(any(a[0] in ('apply', 'patch', 'create', 'delete', 'rollout') for a, _ in following.commands))

    def test_failed_ready_marker_restores_previous_pointer_and_remains_quarantined(self):
        with tempfile.TemporaryDirectory() as directory:
            first = self.seed(Path(directory))
            following = self.subsequent(first, '1' * 40)
            following.desired['spec']['template']['version'] = 'next'
            original = MODULE.atomic
            failed = []
            def fail_ready_once(path, value):
                if path.name.startswith('postgres-quarantine-') and value.get('state') == 'ready' and not failed:
                    failed.append(True)
                    raise OSError('injected READY marker persistence failure')
                return original(path, value)
            MODULE.atomic = fail_ready_once
            try:
                with self.assertRaisesRegex(OSError, 'READY marker'):
                    following.execute()
            finally:
                MODULE.atomic = original
            self.assertEqual(following.record['outcome'], 'failed')
            self.assertEqual(following.record['rollback'], 'ready')
            pointer = json.loads((Path(directory) / 'postgres-last-ready-boss.json').read_text())
            self.assertEqual(pointer['source'], first.args.head)
            repeated = self.subsequent(first, following.args.head)
            with self.assertRaisesRegex(ValueError, 'quarantin'):
                repeated.execute()

    def test_postgres_resource_is_pinned_to_the_shared_forge_default(self):
        defaults = (ROOT / 'infra/forge/forge-defaults.sh').read_text()
        workload = re.search(r'^PG_WORKLOAD="([^"]+)"$', defaults, re.M).group(1)
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory), failure=False)
            recovery.execute()
            # The placement read is of nodes, not of a PostgreSQL workload (76f89301).
            resources = [a[2] if a[0] == 'rollout' else a[1] for a, _ in recovery.commands
                         if a[0] in ('get', 'patch', 'rollout') and '--local' not in a and a[:2] != ['get', 'nodes']]
            self.assertGreaterEqual(len(resources), 3)
            self.assertEqual(set(resources), {workload})

    def test_ready_replacement_is_not_credited_to_the_applied_statefulset(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory), failure=False)
            recovery.replace_on_ready = True
            with self.assertRaisesRegex(ValueError, 'identity changed'):
                recovery.execute()

    def test_named_target_resolves_real_commit_and_renders_its_own_registry(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.args.last = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
            recovery.run = MODULE.Recovery.run.__get__(recovery)
            # The real archive and the real renderer, with no wall clock:
            # this test proves what a named commit renders, not how fast a
            # gate bay's drive reads it. Under the production bound it lost
            # to w-1's drive while two suites wrote to it (gate 35600cc4,
            # backlog aa0013a5), and on a 280 s io.full stall a budget of
            # any size is the wrong shape (9bff920d). The bound's own
            # refusal is pinned below, on a stub that never answers.
            recovery.named_source_seconds = None
            recovery.desired['spec'] = recovery.old
            good = MODULE.Recovery.named_good(recovery, recovery.object)
            self.assertEqual(good['source'], recovery.args.last)
            self.assertEqual(good['namespace'], 'boss')
            self.assertEqual(good['name'], 'postgres')
            recovery.args.namespace = 'boss-not-in-old-registry'
            with self.assertRaisesRegex(ValueError, 'namespace absent'):
                MODULE.Recovery.named_good(recovery, recovery.object)

    def test_an_export_past_its_bound_is_refused_in_words_naming_the_archive_and_the_time(self):
        # Production keeps a decided bound (backlog aa0013a5); what it owes
        # is a refusal that says which archive ran out and after how long,
        # in the receipt and the error, never a raw TimeoutExpired.
        self.assertEqual(MODULE.NAMED_SOURCE_SECONDS, 60)
        with tempfile.TemporaryDirectory() as directory:
            stubs = Path(directory) / 'bin'
            stubs.mkdir()
            git = stubs / 'git'
            # Never answers, so the bound is the only way out: a loaded
            # drive can make this slower, never make it answer.
            git.write_text('#!/bin/sh\nexec sleep 30\n')
            git.chmod(0o755)
            recovery = MemoryRecovery(Path(directory))
            self.assertEqual(recovery.named_source_seconds, MODULE.NAMED_SOURCE_SECONDS)
            recovery.named_source_seconds = 1
            source = '3' * 40
            path = os.environ['PATH']
            os.environ['PATH'] = str(stubs) + os.pathsep + path
            try:
                with self.assertRaisesRegex(ValueError, r'named PostgreSQL source export timed out: git archive of '
                                            + source + r' \(infra/cluster, examples\) from ' + re.escape(str(ROOT))
                                            + r' ran [0-9]+\.[0-9] s against its 1 s bound'):
                    MODULE.Recovery.rendered_postgres(recovery, source)
            finally:
                os.environ['PATH'] = path
            export = recovery.record['named_source_export']
            self.assertEqual(export['source'], source)
            self.assertTrue(export['timed_out'])
            self.assertEqual(export['bound_seconds'], 1)
            self.assertGreaterEqual(export['elapsed_seconds'], 1)

    def test_stale_resource_version_refuses_without_rolling_back_an_unapplied_spec(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.reject_patch = True
            with self.assertRaisesRegex(ValueError, 'stale resourceVersion'):
                recovery.execute()
            self.assertEqual(recovery.object['spec'], recovery.old)
            self.assertEqual(len([a for a, _ in recovery.commands if a[0] == 'patch' and '--local' not in a and '--dry-run=server' not in a]), 1)
            self.assertIn('moved independently', recovery.record['rollback']['failed'])

    def test_lost_write_ack_reconciles_actual_spec_before_restoring(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.lose_patch_ack = True
            with self.assertRaisesRegex(ValueError, 'ACK lost'):
                recovery.execute()
            self.assertEqual(recovery.object['spec'], recovery.old)
            self.assertEqual(recovery.record['rollback'], 'ready')
            self.assertEqual(recovery.record['outcome'], 'failed')

    def test_component_ready_does_not_replace_the_named_global_target(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory), failure=False)
            recovery.execute()
            previous = json.loads((Path(directory) / ('postgres-ready-boss-' + '2' * 40 + '.json')).read_text())
            current = json.loads((Path(directory) / ('postgres-ready-boss-' + '1' * 40 + '.json')).read_text())
            self.assertEqual(previous['spec'], recovery.old)
            self.assertEqual(current['spec'], recovery.desired['spec'])
            self.assertEqual(recovery.record['outcome'], 'ready')

    def test_failed_rollout_restores_named_spec_but_still_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            with self.assertRaises(ValueError):
                recovery.execute()
            self.assertEqual(recovery.object['spec'], recovery.old)
            self.assertEqual(recovery.record['rollback'], 'ready')
            self.assertEqual(recovery.record['outcome'], 'failed')
            self.assertTrue(Path(recovery.args.receipt).is_file())
            self.assertFalse(any('jobs' in str(a) for a in recovery.commands))

    def test_replacement_statefulset_is_never_rolled_back(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.replace_identity = True
            with self.assertRaises(ValueError):
                recovery.execute()
            self.assertEqual(len([a for a, _ in recovery.commands if a[0] == 'patch' and '--local' not in a and '--dry-run=server' not in a]), 1)
            self.assertIn('identity changed', recovery.record['rollback']['failed'])

    def test_failed_rollback_readiness_remains_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.rollback_failure = True
            with self.assertRaises(ValueError):
                recovery.execute()
            self.assertIn('failed', recovery.record['rollback'])

    def test_failed_owned_stale_pod_delete_is_uid_preconditioned_after_restore(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.pods = [{'metadata': {'name': 'postgres-0', 'uid': 'pod-uid', 'labels': {'controller-revision-hash': 'bad'},
                                          'ownerReferences': [{'name': 'postgres', 'uid': 'sts-uid', 'kind': 'StatefulSet', 'controller': True}]}, 'status': {'conditions': [], 'containerStatuses': [{'name': 'postgres', 'restartCount': 1, 'state': {'waiting': {'reason': 'CrashLoopBackOff'}}}]}}]
            with self.assertRaises(ValueError):
                recovery.execute()
            deletes = [(a, json.loads(b)) for a, b in recovery.commands if a[0] == 'delete']
            self.assertEqual(len(deletes), 1)
            self.assertEqual(deletes[0][1]['preconditions'], {'uid': 'pod-uid'})
            self.assertEqual(deletes[0][0][2], '/api/v1/namespaces/boss/pods/postgres-0')
            self.assertEqual([a[0] for a, _ in recovery.commands if a[0] in ('patch', 'delete') and '--local' not in a and '--dry-run=server' not in a], ['patch', 'patch', 'delete'])

    def test_foreign_or_ready_or_current_revision_pods_are_never_deleted(self):
        for owner, ready, revision in [('foreign', False, 'bad'), ('sts-uid', True, 'bad'), ('sts-uid', False, 'good')]:
            with self.subTest(owner=owner, ready=ready, revision=revision), tempfile.TemporaryDirectory() as directory:
                recovery = MemoryRecovery(Path(directory))
                recovery.pods = [{'metadata': {'name': 'postgres-0', 'uid': 'pod-uid', 'labels': {'controller-revision-hash': revision},
                                              'ownerReferences': [{'name': 'postgres', 'uid': owner, 'kind': 'StatefulSet', 'controller': True}]},
                                  'status': {'conditions': [{'type': 'Ready', 'status': 'True' if ready else 'False'}]}}]
                with self.assertRaises(ValueError):
                    recovery.execute()
                self.assertFalse(any(a[0] == 'delete' for a, _ in recovery.commands))

    def placed(self, directory, pin, nodes):
        recovery = MemoryRecovery(Path(directory))
        recovery.failure = False
        recovery.desired['spec']['template']['spec'] = copy.deepcopy(pin)
        recovery.nodes = nodes
        return recovery

    def test_a_pin_no_ready_node_can_take_is_refused_before_any_write(self):
        # 76f89301 (review dd6733cc F2): the forward patch used to roll
        # postgres-0 onto a pin with no check that any node could take it,
        # and the system of record went Pending. Every way a labelled node
        # can still refuse the pod is here, and each is named in the refusal.
        nodes = [node('cp-2', {'node-role.kubernetes.io/control-plane': ''}),
                 node('w-1', {'boss.dev/purpose': 'database'}, cordoned=True),
                 node('w-2', {'boss.dev/purpose': 'database'}, taints=[{'key': 'dedicated', 'value': 'builds', 'effect': 'NoSchedule'}]),
                 node('w-3', {'boss.dev/purpose': 'database'}, ready=False)]
        for shape, pin in (('nodeSelector', {'nodeSelector': {'boss.dev/purpose': 'database'}}),
                           ('affinity', {'affinity': DATA_NODE_AFFINITY})):
            with self.subTest(shape=shape), tempfile.TemporaryDirectory() as directory:
                recovery = self.placed(directory, pin, nodes)
                with self.assertRaisesRegex(ValueError, 'fits no Ready node'):
                    recovery.execute()
                message = recovery.record['primary_failure']
                for reason in ('cp-2: ', 'w-1: cordoned', 'w-2: untolerated taint dedicated=builds:NoSchedule', 'w-3: not Ready'):
                    self.assertIn(reason, message)
                self.assertEqual(recovery.object['spec'], recovery.old)
                self.assertFalse(any(a[0] == 'patch' and '--dry-run=server' not in a for a, _ in recovery.commands))
                self.assertNotIn('rollback', recovery.record)
                self.assertFalse((Path(directory) / 'postgres-active-journal-boss.json').exists())
                self.assertFalse((Path(directory) / ('postgres-quarantine-boss-' + '1' * 40 + '.json')).exists(),
                                 'a refused placement wrote nothing, so the head may try again once a node can take it')

    def test_a_pin_a_ready_node_can_take_rolls_forward(self):
        tolerated = {'key': 'dedicated', 'value': 'database', 'effect': 'NoSchedule'}
        nodes = [node('cp-2', {'node-role.kubernetes.io/control-plane': ''}),
                 node('w-3', {'boss.dev/purpose': 'database'}, taints=[tolerated])]
        toleration = [{'key': 'dedicated', 'operator': 'Equal', 'value': 'database', 'effect': 'NoSchedule'}]
        for shape, pin in (('nodeSelector', {'nodeSelector': {'boss.dev/purpose': 'database'}, 'tolerations': toleration}),
                           ('affinity', {'affinity': DATA_NODE_AFFINITY, 'tolerations': toleration})):
            with self.subTest(shape=shape), tempfile.TemporaryDirectory() as directory:
                recovery = self.placed(directory, pin, nodes)
                recovery.execute()
                self.assertEqual(recovery.record['outcome'], 'ready')
                self.assertEqual(recovery.record['placement']['fits'], ['w-3'])

    def moved(self, directory, before, after, failure):
        """A live pod-template pin `before` and a rendered pin `after`."""
        recovery = MemoryRecovery(Path(directory), failure=failure)
        recovery.old['template']['spec'] = copy.deepcopy(before)
        recovery.object['spec'] = copy.deepcopy(recovery.old)
        recovery.desired['spec']['template']['spec'] = copy.deepcopy(after)
        return recovery

    def test_a_pin_moved_between_selector_and_affinity_drops_the_old_pin_and_rolls_back_to_it(self):
        # 9123d811, the root of converge stop c701480c: boss.yaml moved
        # postgres from the control-plane nodeSelector to a required
        # affinity on boss.dev/purpose=database. The forward was a strategic
        # merge, which adds what the rendering gains and keeps what it
        # drops, so the merged template demanded both pins, the placement
        # check refused every node (maintenance-cluster-converge 31789097:
        # fits [], forward_inverse holding only the affinity) and every
        # converge failed for 3.5 hours. The rendering owns the pin: the
        # forward removes the one it dropped, the placement check judges the
        # template as it will be, and the inverse puts the old pin back.
        selector = {'nodeSelector': CONTROL_PLANE_SELECTOR}
        affinity = {'affinity': DATA_NODE_AFFINITY}
        for before, after, home, dropped in ((selector, affinity, 'w-3', 'nodeSelector'),
                                             (affinity, selector, 'cp-2', 'affinity')):
            for failure in (False, True):
                with self.subTest(dropped=dropped, failure=failure), tempfile.TemporaryDirectory() as directory:
                    recovery = self.moved(directory, before, after, failure)
                    if failure:
                        with self.assertRaisesRegex(ValueError, 'readiness'):
                            recovery.execute()
                        self.assertEqual(recovery.record['rollback'], 'ready')
                    else:
                        recovery.execute()
                        self.assertEqual(recovery.record['outcome'], 'ready')
                    self.assertEqual(recovery.record['placement']['fits'], [home])
                    rows = {row['path']: row for row in recovery.record['forward_inverse']['changes']}
                    removal = rows['/spec/template/spec/' + dropped]
                    self.assertEqual((removal['before_present'], removal['after_present']), (True, False))
                    self.assertEqual(removal['before'], before[dropped])
                    forward = [json.loads(a[a.index('-p') + 1]) for a, _ in recovery.commands
                               if a[0] == 'patch' and '--type=json' in a][0]
                    self.assertEqual(forward[-1]['value']['template']['spec'], after)
                    self.assertEqual(recovery.object['spec']['template']['spec'], before if failure else after)
                    if failure:
                        self.assertEqual(recovery.object['spec'], recovery.old)

    def test_the_rendering_owns_every_placement_key_and_nothing_else(self):
        # A key the rendering drops at any depth of a placement field goes:
        # a stale selector label, a stale anti-affinity beside the wanted
        # node affinity, a toleration list the rendering no longer names.
        # Every other field keeps the merge's answer, so a foreign sidecar
        # and a server default survive as they always have (9123d811).
        merged = {'replicas': 1, 'template': {'spec': {
            'containers': [{'name': 'postgres'}, {'name': 'foreign-sidecar'}], 'dnsPolicy': 'ClusterFirst',
            'nodeSelector': dict(CONTROL_PLANE_SELECTOR, stale='x'),
            'affinity': dict(DATA_NODE_AFFINITY, podAntiAffinity={'stale': True}),
            'tolerations': [{'key': 'stale', 'operator': 'Exists'}]}}}
        original = copy.deepcopy(merged)
        desired = {'replicas': 1, 'template': {'spec': {'containers': [{'name': 'postgres'}],
                                                        'nodeSelector': CONTROL_PLANE_SELECTOR,
                                                        'affinity': DATA_NODE_AFFINITY}}}
        pod = MODULE.owned_placement(merged, desired)['template']['spec']
        self.assertEqual(pod['nodeSelector'], CONTROL_PLANE_SELECTOR)
        self.assertEqual(pod['affinity'], DATA_NODE_AFFINITY)
        self.assertNotIn('tolerations', pod)
        self.assertEqual(pod['containers'], original['template']['spec']['containers'])
        self.assertEqual(pod['dnsPolicy'], 'ClusterFirst')
        self.assertEqual(merged, original, 'the merge answer is read, never mutated')

    def test_a_placement_requirement_this_check_cannot_read_refuses(self):
        pin = {'affinity': {'nodeAffinity': {'requiredDuringSchedulingIgnoredDuringExecution': {'nodeSelectorTerms': [
            {'matchExpressions': [{'key': 'boss.dev/purpose', 'operator': 'Near', 'values': ['database']}]}]}}}}
        with tempfile.TemporaryDirectory() as directory:
            recovery = self.placed(directory, pin, [node('w-3', {'boss.dev/purpose': 'database'})])
            with self.assertRaisesRegex(ValueError, 'unread'):
                recovery.execute()
            self.assertFalse(any(a[0] == 'patch' and '--dry-run=server' not in a for a, _ in recovery.commands))

    def test_an_unschedulable_stale_pod_is_failure_evidence_and_is_recycled(self):
        # The pin moved postgres-0 where nothing could take it: Pending,
        # never scheduled, no container status at all. Before 76f89301 the
        # recycle read that as no evidence, refused, and left the SoR dark.
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            recovery.pods = [stale_pod({'phase': 'Pending', 'conditions': [
                {'type': 'PodScheduled', 'status': 'False', 'reason': 'Unschedulable',
                 'lastTransitionTime': '2026-11-15T16:00:00Z'}]})]
            with self.assertRaisesRegex(ValueError, 'readiness'):
                recovery.execute()
            deletes = [(a, json.loads(b)) for a, b in recovery.commands if a[0] == 'delete']
            self.assertEqual(len(deletes), 1)
            self.assertEqual(deletes[0][1]['preconditions'], {'uid': 'pod-uid'})
            self.assertEqual(recovery.record['rollback'], 'ready')

    def test_a_stale_pod_stuck_creating_past_its_bound_is_recycled_and_within_it_is_not(self):
        now = datetime.now(timezone.utc)
        for age, recycled in ((MODULE.CONTAINER_CREATING_SECONDS + 60, True), (5, False)):
            scheduled = (now - timedelta(seconds=age)).strftime('%Y-%m-%dT%H:%M:%SZ')
            with self.subTest(age=age), tempfile.TemporaryDirectory() as directory:
                recovery = MemoryRecovery(Path(directory))
                recovery.pods = [stale_pod({'phase': 'Pending', 'conditions': [
                    {'type': 'PodScheduled', 'status': 'True', 'lastTransitionTime': scheduled}],
                    'containerStatuses': [{'name': 'postgres', 'restartCount': 0,
                                           'state': {'waiting': {'reason': 'ContainerCreating'}}}]})]
                with self.assertRaises(ValueError):
                    recovery.execute()
                self.assertEqual(any(a[0] == 'delete' for a, _ in recovery.commands), recycled)
                if not recycled:
                    self.assertIn('no evidence of failure', recovery.record['rollback']['failed'])

    def test_a_running_postgres_beside_a_creating_container_is_never_deleted(self):
        # Review cc5a878a B1 on car 1d5379b5: container state is
        # per-container, so one sidecar aged in ContainerCreating said
        # nothing about PostgreSQL, yet it recycled a pod whose postgres was
        # running with no restart, the WAL-recovery shape the single-container
        # test above protects. The creating and Unschedulable reasons now
        # count only when no container in the pod has started, and creating
        # only when postgres is itself seen waiting. The last row is the
        # control: the same aged pod with nothing started IS recycled.
        scheduled = (datetime.now(timezone.utc)
                     - timedelta(seconds=MODULE.CONTAINER_CREATING_SECONDS + 300)).strftime('%Y-%m-%dT%H:%M:%SZ')
        on_node = {'type': 'PodScheduled', 'status': 'True', 'lastTransitionTime': scheduled}
        unplaced = {'type': 'PodScheduled', 'status': 'False', 'reason': 'Unschedulable', 'lastTransitionTime': scheduled}
        running = {'name': 'postgres', 'restartCount': 0, 'started': True,
                   'state': {'running': {'startedAt': '2026-11-01T00:00:00Z'}}}
        creating = {'name': 'foreign-sidecar', 'restartCount': 0,
                    'state': {'waiting': {'reason': 'ContainerCreating'}}}
        postgres_creating = dict(creating, name='postgres')
        completed_init = {'name': 'foreign-init', 'restartCount': 0,
                          'state': {'terminated': {'reason': 'Completed', 'exitCode': 0}}}
        for shape, status, recycled in (
                ('postgres running, sidecar creating', {'phase': 'Running', 'conditions': [on_node, {'type': 'Ready', 'status': 'False'}],
                                                        'containerStatuses': [running, creating]}, False),
                ('postgres running, init creating', {'phase': 'Running', 'conditions': [on_node],
                                                     'containerStatuses': [running], 'initContainerStatuses': [creating]}, False),
                ('postgres unseen, sidecar creating', {'phase': 'Pending', 'conditions': [on_node],
                                                       'containerStatuses': [creating]}, False),
                ('an init container ran, postgres creating', {'phase': 'Pending', 'conditions': [on_node],
                                                              'containerStatuses': [postgres_creating],
                                                              'initContainerStatuses': [completed_init]}, False),
                ('Unschedulable beside a running postgres', {'phase': 'Pending', 'conditions': [unplaced],
                                                             'containerStatuses': [running]}, False),
                ('nothing started, both creating', {'phase': 'Pending', 'conditions': [on_node],
                                                    'containerStatuses': [postgres_creating, creating]}, True)):
            with self.subTest(shape=shape), tempfile.TemporaryDirectory() as directory:
                recovery = MemoryRecovery(Path(directory))
                recovery.pods = [stale_pod(status)]
                with self.assertRaises(ValueError):
                    recovery.execute()
                deletes = [json.loads(b) for a, b in recovery.commands if a[0] == 'delete']
                self.assertEqual(len(deletes), 1 if recycled else 0)
                if recycled:
                    self.assertEqual(deletes[0]['preconditions'], {'uid': 'pod-uid'})
                    self.assertEqual(recovery.record['rollback'], 'ready')
                else:
                    self.assertIsNone(recovery.record['recycle_evidence'])
                    self.assertIn('no evidence of failure', recovery.record['rollback']['failed'])

    def test_an_unschedulable_pod_already_bound_to_a_node_is_not_recycled(self):
        # Unschedulable is evidence only for a pod no kubelet holds: one
        # bound to a node by name has a kubelet that may have started it.
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            pod = stale_pod({'phase': 'Pending', 'conditions': [
                {'type': 'PodScheduled', 'status': 'False', 'reason': 'Unschedulable',
                 'lastTransitionTime': '2026-11-15T16:00:00Z'}]})
            pod['spec'] = {'nodeName': 'w-3'}
            recovery.pods = [pod]
            with self.assertRaises(ValueError):
                recovery.execute()
            self.assertFalse(any(a[0] == 'delete' for a, _ in recovery.commands))

    def test_unqualified_previous_receipt_refuses_before_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            MODULE.atomic(Path(directory) / ('postgres-ready-boss-' + '2' * 40 + '.json'), {'namespace': 'boss', 'uid': 'other', 'source': '2' * 40, 'spec': recovery.old})
            recovery.named_good = lambda live: (_ for _ in ()).throw(ValueError('receipt identity cannot be reconstructed'))
            with self.assertRaisesRegex(ValueError, 'receipt identity'):
                recovery.execute()
            self.assertFalse(any(a[0] in ('patch', 'create', 'delete') for a, _ in recovery.commands))

    def test_readiness_requires_observed_generation_and_both_revisions(self):
        with tempfile.TemporaryDirectory() as directory:
            recovery = MemoryRecovery(Path(directory))
            for field, value in [('observedGeneration', 0), ('readyReplicas', 0), ('updatedReplicas', 0), ('currentRevision', 'old')]:
                obj = copy.deepcopy(recovery.object)
                obj['status'][field] = value
                self.assertFalse(recovery.ready(obj), field)

    def test_actual_kubectl_raw_delete_transmits_uid_and_grace_to_loopback_only(self):
        kubectl = shutil.which('kubectl')
        if kubectl is None:
            self.skipTest('actual kubectl unavailable in isolated worker; transport not measured')
        version = subprocess.run([kubectl, 'version', '--client', '-o', 'json'], capture_output=True, text=True, timeout=10)
        self.assertEqual(version.returncode, 0, version.stderr)
        print('loopback transport client version: ' + version.stdout)
        captured = []
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_DELETE(self):
                self.connection.settimeout(3)
                if self.headers.get('Transfer-Encoding') == 'chunked':
                    chunks = []
                    size = int(self.rfile.readline(128).strip().split(b';')[0], 16)
                    total = 0
                    while size:
                        if len(chunks) >= 16:
                            raise ValueError('unexpected fixture HTTP chunk count')
                        total += size
                        if total > 16384:
                            raise ValueError('unexpected fixture HTTP body size')
                        chunks.append(self.rfile.read(size))
                        if self.rfile.read(2) != b'\r\n':
                            raise ValueError('invalid fixture HTTP chunk')
                        size = int(self.rfile.readline(128).strip().split(b';')[0], 16)
                    for _ in range(8):
                        trailer = self.rfile.readline(512)
                        if trailer == b'\r\n':
                            break
                        if not trailer.endswith(b'\r\n'):
                            raise ValueError('invalid fixture HTTP trailer')
                    else:
                        raise ValueError('unexpected fixture HTTP trailer count')
                    raw = b''.join(chunks)
                else:
                    length = int(self.headers.get('Content-Length', 0))
                    if not 0 <= length <= 16384:
                        raise ValueError('unexpected fixture HTTP body size')
                    raw = self.rfile.read(length)
                captured.append((self.path, raw))
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(b'{"kind":"Status","apiVersion":"v1","status":"Success"}')
            def log_message(self, *args):
                pass
        with tempfile.TemporaryDirectory() as directory, http.server.HTTPServer(('127.0.0.1', 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            config = Path(directory) / 'kubeconfig'
            config.write_text(json.dumps({'apiVersion': 'v1', 'kind': 'Config', 'clusters': [{'name': 'loopback', 'cluster': {'server': 'http://127.0.0.1:' + str(server.server_port)}}],
                                          'contexts': [{'name': 'loopback', 'context': {'cluster': 'loopback', 'user': 'no-credentials'}}], 'current-context': 'loopback', 'users': [{'name': 'no-credentials', 'user': {}}]}))
            body = {'kind': 'DeleteOptions', 'apiVersion': 'v1', 'gracePeriodSeconds': 30, 'preconditions': {'uid': 'exact-failed-pod'}}
            try:
                result = subprocess.run([kubectl, '--kubeconfig', str(config), 'delete', '--raw', '/api/v1/namespaces/boss/pods/postgres-0', '-f', '-', '--request-timeout=5s'],
                                        input=json.dumps(body), capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(len(captured), 1)
                print(json.dumps({'actual_delete_uri': captured[0][0], 'actual_delete_body_base64': base64.b64encode(captured[0][1]).decode()}))
                uri = urlsplit(captured[0][0])
                self.assertEqual(uri.path, '/api/v1/namespaces/boss/pods/postgres-0')
                self.assertEqual(parse_qs(uri.query), {'timeout': ['5s']})
                self.assertEqual(json.loads(captured[0][1]), body)
                print('actual kubectl loopback DELETE captured exact UID and graceful30s DeleteOptions')
            finally:
                server.shutdown()
                thread.join(timeout=2)


class PostgresRollout(unittest.TestCase):
    def test_bootstrap_dependencies_apply_before_postgres_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            result, calls = self.exercise(Path(directory), 'boss')
            self.assertNotEqual(result.returncode, 0)
            creates = [i for i, a in enumerate(calls) if a[0] == 'create' and '--dry-run=client' not in a]
            applies = [i for i, a in enumerate(calls) if a[0] == 'apply']
            self.assertTrue(creates, calls)
            self.assertTrue(applies and max(applies) < min(creates), calls)

    def exercise(self, root, namespace):
        stage = root / namespace
        stage.mkdir()
        (root / 'state').mkdir()
        (stage / 'boss.yaml').write_text('''apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: postgres
  namespace: ''' + namespace + '''
spec:
  template:
    spec:
      containers:
        - name: postgres
          image: postgres:16
          args: [postgres, -c, invalid_setting=on]
''')
        adapter = root / 'kubectl'
        adapter.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
a=sys.argv[1:]
with open(os.environ['CALLS'],'a') as f: f.write(json.dumps(a)+'\\n')
if a[0]=='rollout' or a[0]=='wait':
    print('postgres cannot boot: invalid_setting',file=sys.stderr)
    sys.exit(1)
if a[0]=='apply': sys.exit(0)
if a[0]=='get' and a[1]=='sts/postgres': sys.exit(0)
if a[0]=='create' and '--dry-run=client' in a:
    text=sys.stdin.read()
    ns=next(line.split(':',1)[1].strip() for line in text.splitlines() if line.strip().startswith('namespace:'))
    print(json.dumps({'apiVersion':'apps/v1','kind':'StatefulSet','metadata':{'name':'postgres','namespace':ns},'spec':{'template':{'spec':{'containers':[{'name':'postgres','image':'postgres:16','args':['postgres','-c','invalid_setting=on']}]}}}}))
    sys.exit(0)
if a[0]=='create':
    obj=json.load(sys.stdin); obj['metadata']['uid']='created-postgres'
    print(json.dumps(obj)); sys.exit(0)
print('{}')
''')
        adapter.chmod(0o700)
        source = (ROOT / 'infra/forge/cluster-deploy-runner.sh').read_text()
        apply = re.search(r'^apply_instance\(\) \{\n.*?^\}', source, re.M | re.S).group()
        finish = re.search(r'^_finish\(\) \{\n.*?^\}', source, re.M | re.S).group()
        script = root / 'run.sh'
        script.write_text('set -eu\n. "$REPO/infra/run-summary.sh"\n'
                          '. "$REPO/infra/forge/cluster-deploy-lib.sh"\n'
                          'apply_namespaces() { :; }\n'
                          'standalone_pvc_preserve() { printf "[]" > "$3"; }\n'
                          'answer_converge_requests() { printf "%s\\n" "$2" > "$ANSWER_FILE"; }\n'
                          + apply + '\n' + finish + '\ntrap _finish EXIT\napply_instance "$TEST_NS"\n')
        env = dict(os.environ, REPO=str(ROOT), APPLY_DIR=str(root), K=str(adapter),
                   KM=str(adapter), KAPPLY=str(adapter), CALLS=str(root / 'calls'),
                   TEST_NS=namespace, LAST='none', HEAD_FULL='1' * 40,
                   OUTCOME='converged=source-ready' if namespace == 'boss-playground' else '',
                   POSTGRES_FAILURE='', STAGE='apply ' + namespace, ANSWER_FILE=str(root / 'answer'),
                   CONVERGE_STATE=str(root / 'state'),
                   BOSS_RUN_SUMMARY_FILE=str(root / 'summary.json'),
                   BOSS_MACHINE_TOKEN_DIR=str(root / 'no-token'),
                   BOSS_SOR_ENV=str(root / 'no-sor.env'))
        result = subprocess.run(['bash', str(script)], env=env, capture_output=True,
                                text=True, timeout=30)
        calls = [json.loads(x) for x in (root / 'calls').read_text().splitlines()]
        self.assertEqual((root / 'answer').read_text().strip(), 'converge_failed', result.stderr)
        return result, calls

    def test_source_postgres_failure_is_not_a_successful_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            result, calls = self.exercise(Path(directory), 'boss')
            self.assertNotEqual(result.returncode, 0, (result.stdout, result.stderr, calls))
            self.assertTrue(any(a[:3] == ['rollout', 'status', 'sts/postgres'] for a in calls), calls)

    def test_playground_postgres_failure_is_not_a_successful_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            result, calls = self.exercise(Path(directory), 'boss-playground')
            self.assertNotEqual(result.returncode, 0, (result.stdout, result.stderr, calls))
            self.assertTrue(any(a[:3] == ['rollout', 'status', 'sts/postgres'] for a in calls), calls)


class ConvergeExitContract(unittest.TestCase):
    def test_real_exit_trap_reports_late_playground_failure_over_prior_source_converged(self):
        source = (ROOT / 'infra/forge/cluster-deploy-runner.sh').read_text()
        finish = re.search(r'^_finish\(\) \{\n.*?^\}', source, re.M | re.S).group()
        for exit_code, outcome, pg_failure, expected in [(37, 'converged=source-ready', 'boss-playground', 'converge_failed'),
                                                        (37, 'converged=source-ready', '', 'converged'),
                                                        (0, 'converged=source-ready', '', 'converged'),
                                                        (0, 'held=already-known-bad', '', 'held')]:
            with self.subTest(exit_code=exit_code, outcome=outcome, pg_failure=pg_failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                script = root / 'finish.sh'
                record = root / 'answer.json'
                script.write_text('set -eu\n'
                                  'answer_converge_requests() { printf "%s\\n" "$2" > "$RECORD"; }\n'
                                  'run_summary_field() { printf "%s=%s\\n" "$1" "$2" >> "$FIELDS"; }\n'
                                  + finish + '\ntrap _finish EXIT\nexit "$TEST_EXIT"\n')
                result = subprocess.run(['bash', str(script)], capture_output=True, text=True, timeout=10,
                                        env=dict(os.environ, RECORD=str(record), FIELDS=str(root / 'fields'),
                                                 CONVERGE_STATE=str(root), STAGE='apply boss-playground',
                                                 OUTCOME=outcome, POSTGRES_FAILURE=pg_failure, TEST_EXIT=str(exit_code)))
                self.assertEqual(result.returncode, exit_code, result.stderr)
                self.assertEqual(record.read_text().strip(), expected)
                if exit_code:
                    self.assertIn('failed_stage=apply boss-playground', (root / 'fields').read_text())


if __name__ == '__main__':
    unittest.main()
