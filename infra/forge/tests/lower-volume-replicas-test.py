"""Owning boundary tests; fake reads implement the helper's transport port."""
import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
import json
import os
import re
import shutil
import subprocess
import tempfile
import signal
import sys
import time
import ctypes
import contextlib
import io
import shlex


def execution_source_closure(root):
    """Walk the fixed entry points, independently of the signed manifest."""
    pending = ["infra/forge/retire-volume-replica.sh",
               "infra/forge/lower-volume-replicas.sh",
               "infra/forge/lower-volume-replicas.py"]
    found = set()
    while pending:
        relative = pending.pop()
        if relative in found:
            continue
        target = (root / relative).resolve(strict=True)
        target.relative_to(root.resolve())
        found.add(relative)
        if target.suffix != '.sh':
            continue
        tokens = iter(shlex.split(target.read_text(), comments=True))
        for token in tokens:
            if token not in ('.', 'source'):
                continue
            expression = next(tokens, '')
            prefixes = ('$HERE/', '$(dirname ${BASH_SOURCE[0]})/')
            matches = [prefix for prefix in prefixes if expression.startswith(prefix)]
            if len(matches) != 1:
                raise ValueError('unresolved or ambiguous shell source: ' + expression)
            child = (target.parent / expression[len(matches[0]):]).resolve(strict=True)
            pending.append(child.relative_to(root.resolve()).as_posix())
    return found


def source_fixture(directory):
    root = Path(directory)
    for relative in FIXED_SOURCES:
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(path.parents[2] / relative, target)
    return root / 'infra/forge'


class ShellEffect(unittest.TestCase):
    def test_registered_shell_prints_only_verified_typed_effect(self):
        volume = "pvc-11111111-1111-4111-8111-111111111111"
        replica = volume + "-r-12345678"
        with tempfile.TemporaryDirectory(prefix="boss-lower-effect-") as tmp:
            root = Path(tmp) / "forge"
            root.mkdir()
            (root.parent / "lib").mkdir()
            shutil.copy(path.parents[1] / "lib/jq.sh", root.parent / "lib")
            shutil.copy(path.parent / "retire-volume-replica.sh", root)
            (root / "lower-volume-replicas.sh").write_text(
                '#!/bin/bash\n'
                'echo retained-helper-diagnostic >&2\n'
                'if [ "${1-}" = --effect-result ] && [ "$MODE" != missing ]; then\n'
                ' if [ "$MODE" = empty ]; then : > "$2"; else printf "%s" "$RESULT" > "$2"; fi\nfi\n'
                '[ "$MODE" != failed ]\n'
            )
            for mode, maintenance in (("success", False), ("success", True),
                                      ("missing", False), ("failed", False),
                                      ("wrong-volume", False), ("empty", False)):
                result = dict(schema=1, volume=volume, replica=replica,
                              target=2, maintenance=maintenance)
                if mode == "wrong-volume":
                    result["volume"] = "other"
                out = subprocess.run(
                    ["bash", str(root / "retire-volume-replica.sh"), "--lower",
                     volume, replica, "0" * 64], capture_output=True, text=True,
                    env={**os.environ, "MODE": mode, "RESULT": json.dumps(result)},
                    timeout=10,
                )
                with self.subTest(mode=mode, maintenance=maintenance):
                    self.assertIn("retained-helper-diagnostic", out.stderr)
                    if mode == "success":
                        self.assertEqual(out.returncode, 0, out.stderr)
                        effect = json.loads((path.parents[1] / "ops/verbs/lower-volume-replicas.json").read_text())["effect"]
                        self.assertRegex(out.stdout.strip(), re.compile(effect))
                    else:
                        self.assertNotEqual(out.returncode, 0)
                        self.assertNotIn(" declared ", out.stdout)

path = Path(__file__).resolve().parents[1] / "lower-volume-replicas.py"
spec = importlib.util.spec_from_file_location("lower", path)
lower = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lower)
FIXED_SOURCES = lower.SOURCE_FILES


class SourceManifest(unittest.TestCase):
    def test_both_approver_descriptions_name_the_complete_manifest(self):
        for name in ('plan-a-volume-replica-lowering', 'lower-volume-replicas'):
            with self.subTest(verb=name):
                about = json.loads((path.parents[1] / 'ops/verbs' / (name + '.json')).read_text())['about']
                self.assertIn('the complete fixed execution-source manifest', about)
                self.assertNotRegex(about, r'\b(?:seven|eight|[0-9]+) fixed execution')

    def test_signed_manifest_covers_recursive_shell_dependencies(self):
        closure = execution_source_closure(path.parents[2])
        self.assertEqual(closure, set(lower.SOURCE_FILES))
        self.assertIn('infra/lib/sor-reader.sh', closure)
        self.assertNotEqual(closure, set(lower.SOURCE_FILES) - {'infra/lib/sor-reader.sh'})

    def test_missing_or_unresolved_source_is_a_failure(self):
        for expression in ('"$HERE/missing.sh"', '"$UNKNOWN/helper.sh"',
                           '"$HERE/one.sh" "$HERE/two.sh"'):
            with self.subTest(expression=expression), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                forge = root / 'infra/forge'
                forge.mkdir(parents=True)
                (forge / 'retire-volume-replica.sh').write_text('. ' + expression + '\n')
                (forge / 'lower-volume-replicas.sh').write_text('# fixture\n')
                (forge / 'lower-volume-replicas.py').write_text('# fixture\n')
                with self.assertRaises((ValueError, FileNotFoundError)):
                    execution_source_closure(root)


class Reads:
    def __init__(self):
        self.v = {"metadata": {"name": "volume", "uid": "v"},
                  "spec": {"nodeID": "n1", "numberOfReplicas": 3},
                  "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "n1"}}
        self.reps = [{"metadata": {"name": f"r{i}", "uid": f"u{i}"},
                      "spec": {"volumeName": "volume", "nodeID": f"n{i}", "diskID": f"d{i}",
                               "healthyAt": "2026-10-01T00:00:00Z", "failedAt": "", "active": True},
                      "status": {"currentState": "running"}} for i in (1, 2, 3)]
        self.engine = {"metadata": {"name": "engine", "uid": "e"},
                       "spec": {"volumeName": "volume", "nodeID": "n1", "engineImage": "image",
                                "replicaAddressMap": {f"r{i}": "tcp://address" for i in (1, 2, 3)}},
                       "status": {"currentState": "running", "replicaModeMap": {f"r{i}": "RW" for i in (1, 2, 3)}}}
        self.nodes = [{"metadata": {"name": f"n{i}"},
                       "status": {"conditions": [{"type": "Ready", "status": "True"}],
                                  "diskStatus": {"disk": {"diskUUID": f"d{i}", "conditions": [{"type": "Ready", "status": "True"}]}}}}
                      for i in (1, 2, 3)]

    def get(self, resource, name=None, namespace=lower.LH):
        return copy.deepcopy({"volumes.longhorn.io": self.v,
                              "replicas.longhorn.io": {"items": self.reps},
                              "engines.longhorn.io": {"items": [self.engine]},
                              "nodes.longhorn.io": {"items": self.nodes}}[resource])

    def context(self):
        return {"volume": "volume", "volume_uid": "v", "replica": "r1", "replicas": [lower.identity(r) for r in self.reps],
                "attach_node": "n1", "image": "image", "maintenance": False, "count": 3, "target": 2}


class Runtime(unittest.TestCase):
    def test_floor_refusal_names_two_with_spacing(self):
        r = Reads()
        r.v['spec']['numberOfReplicas'] = 2
        with self.assertRaisesRegex(lower.Refusal, 'floor 2'):
            lower.make_plan(r, 'volume', 'r1')

    def test_plan_binds_helper_and_wrapper_bytes(self):
        r = Reads()
        r.v["spec"]["image"] = "image"
        with tempfile.TemporaryDirectory(prefix="boss-lower-source-") as tmp:
            here = source_fixture(tmp)
            original = lower.HERE
            try:
                lower.HERE = here
                first = lower.make_plan(r, "volume", "r1")
                self.assertEqual(set(first['source_sha256']), set(FIXED_SOURCES))
                for name in ("lower-volume-replicas.py", "lower-volume-replicas.sh"):
                    old = (here / name).read_bytes()
                    (here / name).write_bytes(old + b"\n# source identity control\n")
                    changed = lower.make_plan(r, "volume", "r1")
                    self.assertNotEqual(lower.digest(first), lower.digest(changed), name)
                    (here / name).write_bytes(old)
                self.assertEqual(first, lower.make_plan(r, "volume", "r1"))
            finally:
                lower.HERE = original

    def test_each_execution_source_change_refuses_old_signature_before_work(self):
        volume = "pvc-11111111-1111-4111-8111-111111111111"
        r = Reads()
        r.v['metadata']['name'] = volume
        r.v['spec']['image'] = 'image'
        for index, replica in enumerate(r.reps):
            replica['metadata']['name'] = volume + '-r-' + format(index + 1, '08x')
            replica['spec']['volumeName'] = volume
        r.engine['spec']['volumeName'] = volume
        r.engine['spec']['replicaAddressMap'] = {x['metadata']['name']: 'tcp://address' for x in r.reps}
        r.engine['status']['replicaModeMap'] = {x['metadata']['name']: 'RW' for x in r.reps}
        replica = r.reps[0]['metadata']['name']
        def render():
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch.object(lower.sys, 'argv', ['lower', '--kubectl', 'fixture', '--', '--plan', volume, replica]), \
                 contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                lower.main()
            return stdout.getvalue(), re.search(r'plan-sha256: ([0-9a-f]{64})', stderr.getvalue())[1]
        with tempfile.TemporaryDirectory(prefix='boss-lower-all-sources-') as tmp:
            here = source_fixture(tmp)
            with patch.object(lower, 'HERE', here), patch.object(lower, 'Door', return_value=r):
                original, signature = render()
                for relative in FIXED_SOURCES:
                    with self.subTest(source=relative):
                        target = Path(tmp) / relative
                        raw = target.read_bytes()
                        try:
                            target.write_bytes(raw + b'\n# one actual execution source changed\n')
                            with patch.object(lower.sys, 'argv', ['lower', '--effect-result', str(Path(tmp) / 'effect'), '--kubectl', 'fixture', '--', volume, replica, signature]), \
                                 patch.object(lower, 'supervised', side_effect=AssertionError('stale source reached destructive work')), \
                                 self.assertRaisesRegex(lower.Refusal, 'signed plan differs'):
                                lower.main()
                            self.assertFalse((Path(tmp) / 'effect').exists())
                        finally:
                            target.write_bytes(raw)
                self.assertEqual(render(), (original, signature))

    def test_attachment_reference_and_both_uids_are_bound(self):
        class AttachmentReads:
            def __init__(self):
                self.docs = {
                    "volumeattachments.longhorn.io": {"metadata": {"uid": "attachment"},
                                                       "spec": {"volume": "volume"}},
                    "volumes.longhorn.io": {"metadata": {"uid": "v"}}}

            def get(self, resource, name):
                return copy.deepcopy(self.docs[resource])

        c = {"volume": "volume", "volume_uid": "v", "va_uid": "attachment"}
        lower.attachment(AttachmentReads(), c)
        for kind in ("reference", "volume_uid", "attachment_uid"):
            r = AttachmentReads()
            if kind == "reference":
                r.docs["volumeattachments.longhorn.io"]["spec"]["volume"] = "other"
            elif kind == "volume_uid":
                r.docs["volumes.longhorn.io"]["metadata"]["uid"] = "replacement"
            else:
                r.docs["volumeattachments.longhorn.io"]["metadata"]["uid"] = "replacement"
            with self.subTest(kind=kind), self.assertRaises(lower.Refusal):
                lower.attachment(r, c)

    def test_current_count_is_not_inferred_from_the_rw_roster(self):
        r = Reads()
        c = r.context()
        c.update(count=3, target=2)
        r.v["spec"]["numberOfReplicas"] = 9
        with self.assertRaises(lower.Refusal):
            lower.runtime(r, c, "before-lower")

    def test_wrong_kind_and_incomplete_lists_cannot_authorize_writes(self):
        door = lower.Door([])
        door.call = lambda *args: json.dumps({"kind": "Secret", "metadata": {"name": "volume", "uid": "v", "namespace": lower.LH}})
        with self.assertRaises(lower.Refusal):
            door.get("volumes.longhorn.io", "volume")
        door.call = lambda *args: json.dumps({"kind": "ReplicaList", "metadata": {"continue": "unread"}, "items": []})
        with self.assertRaises(lower.Refusal):
            door.get("replicas.longhorn.io")
    def test_positive_rw_control(self):
        r = Reads()
        lower.runtime(r, r.context(), "before-lower")

    def test_foreign_consumer_still_refuses_destructive_work(self):
        claims = ClaimReads()
        claims.docs['cronjobs']['items'] = []
        claims.docs['pvc']['spec']['volumeName'] = 'volume'
        claims.v['status']['kubernetesStatus']['pvName'] = 'volume'
        ticket = {'id': 'owned', 'generation': 1, 'parameters': {'disableFrontend': 'true'}}
        va = {'spec': {'attachmentTickets': {'owned': ticket}},
              'status': {'attachmentTicketStatuses': {'owned': {'satisfied': True, 'generation': 1}}},
              'metadata': {'uid': 'attachment'}}
        class MaintenanceReads(Reads):
            def get(self, resource, name=None, namespace=lower.LH):
                if resource == 'volumeattachments.longhorn.io':
                    return copy.deepcopy(dict(va, spec=dict(va['spec'], volume='volume')))
                if resource in claims.docs:
                    return copy.deepcopy(claims.docs[resource])
                return super().get(resource, name, namespace)
        r = MaintenanceReads()
        r.v['status']['kubernetesStatus'] = copy.deepcopy(claims.v['status']['kubernetesStatus'])
        c = dict(r.context(), maintenance=True, ticket_id='owned', ticket=ticket, va_uid='attachment')
        c['claim'] = lower.claim_idle(r, r.v, 0)
        lower.runtime(r, c, 'before-lower')
        va['spec']['attachmentTickets']['consumer'] = {'id': 'consumer'}
        with self.assertRaisesRegex(lower.Refusal, 'foreign or changed attachment ticket'):
            lower.runtime(r, c, 'before-lower')

    def test_ready_node_is_required_even_with_stale_healthy_at_and_rw(self):
        r = Reads()
        r.nodes[1]["status"]["conditions"][0]["status"] = "False"
        with self.assertRaises(lower.Refusal):
            lower.runtime(r, r.context(), "before-lower")

    def test_engine_restoring_is_not_fresh_data_health(self):
        r = Reads()
        r.engine["status"]["restoreStatus"] = {"r2": {"isRestoring": True, "error": ""}}
        with self.assertRaises(lower.Refusal):
            lower.runtime(r, r.context(), "before-lower")

    def test_wo_and_changed_uid_are_not_current_rw_set(self):
        for kind in ("wo", "uid"):
            r = Reads()
            c = r.context()
            r.v["spec"]["numberOfReplicas"] = 2
            if kind == "wo":
                r.engine["status"]["replicaModeMap"]["r2"] = "WO"
            else:
                r.reps[1]["metadata"]["uid"] = "new-uid"
            with self.assertRaises(lower.Refusal):
                lower.runtime(r, c, "before-delete")


class FenceReads:
    def __init__(self, mode=None):
        self.mode, self.patches = mode, []
        self.va = {'metadata': {'uid': 'attachment', 'resourceVersion': '1',
                               'annotations': {'unrelated': 'preserved'}},
                   'spec': {'volume': 'volume', 'attachmentTickets': {}}}
        self.ticket = {'id': 'owned', 'generation': 1}

    def get(self, resource, name):
        if resource == 'volumes.longhorn.io':
            return {'metadata': {'uid': 'v'}}
        return copy.deepcopy(self.va)

    def context(self):
        return {'volume': 'volume', 'volume_uid': 'v', 'va_uid': 'attachment',
                'ticket_id': 'owned', 'ticket': self.ticket}

    def patch(self, resource, before, operations):
        self.patches.append(copy.deepcopy(operations))
        if self.mode == 'race' and len(self.patches) == 1:
            self.va['spec']['attachmentTickets']['owned'] = copy.deepcopy(self.ticket)
            self.va['metadata']['resourceVersion'] = '2'
            raise lower.Failure('actual losing resourceVersion CAS')
        if before['metadata']['resourceVersion'] != self.va['metadata']['resourceVersion']:
            raise lower.Failure('resourceVersion conflict')
        if self.mode == 'noop':
            return
        for operation in operations:
            if operation['op'] == 'remove':
                del self.va['spec']['attachmentTickets']['owned']
            elif operation['op'] == 'add':
                self.va['metadata']['annotations'][lower.FENCE_ANNOTATION] = operation['value']
        self.va['metadata']['resourceVersion'] = str(int(self.va['metadata']['resourceVersion']) + 1)
        if self.mode == 'missing-version':
            del self.va['metadata']['resourceVersion']
        if self.mode == 'lost-ack' and len(self.patches) == 1:
            raise lower.Failure('applied fence; transport ACK lost')


class CleanupFence(unittest.TestCase):
    def test_foreign_tickets_never_keep_our_frontend_disabled_ticket(self):
        foreign = {'id': 'consumer', 'type': 'csi-attacher', 'generation': 9}
        for arrives_during_fence in (False, True):
            with self.subTest(arrives_during_fence=arrives_during_fence):
                class ConsumerArrives(FenceReads):
                    def patch(self, resource, before, operations):
                        super().patch(resource, before, operations)
                        if arrives_during_fence and len(self.patches) == 1:
                            self.va['spec']['attachmentTickets']['consumer'] = copy.deepcopy(foreign)
                            self.va['metadata']['resourceVersion'] = str(int(self.va['metadata']['resourceVersion']) + 1)
                r = ConsumerArrives()
                r.ticket['parameters'] = {'disableFrontend': 'true'}
                r.va['spec']['attachmentTickets']['owned'] = copy.deepcopy(r.ticket)
                if not arrives_during_fence:
                    r.va['spec']['attachmentTickets']['consumer'] = copy.deepcopy(foreign)
                lower.reconcile_ticket(r, r.context(), lower.Budget(5))
                self.assertEqual(r.va['spec']['attachmentTickets'], {'consumer': foreign})
                self.assertEqual(r.va['metadata']['annotations']['unrelated'], 'preserved')
                removes = [op for operations in r.patches for op in operations if op['op'] == 'remove']
                self.assertEqual(removes, [{'op': 'remove', 'path': '/spec/attachmentTickets/owned'}])

    def test_changed_value_repeated_cleanup_preserves_unrelated_annotations(self):
        r = FenceReads()
        lower.reconcile_ticket(r, r.context(), lower.Budget(5))
        first = copy.deepcopy(r.va)
        lower.reconcile_ticket(r, r.context(), lower.Budget(5))
        self.assertNotEqual(first['metadata']['annotations'][lower.FENCE_ANNOTATION],
                            r.va['metadata']['annotations'][lower.FENCE_ANNOTATION])
        self.assertNotEqual(first['metadata']['resourceVersion'], r.va['metadata']['resourceVersion'])
        self.assertEqual(r.va['metadata']['annotations']['unrelated'], 'preserved')
        self.assertEqual(len(r.patches), 2)

    def test_losing_fence_cas_rereads_and_removes_only_exact_owned_ticket(self):
        r = FenceReads('race')
        lower.reconcile_ticket(r, r.context(), lower.Budget(5))
        self.assertEqual(r.va['spec']['attachmentTickets'], {})
        self.assertEqual(len(r.patches), 3)
        self.assertEqual(r.patches[-1][0], {'op': 'test',
            'path': '/spec/attachmentTickets/owned', 'value': r.ticket})

    def test_lost_fence_ack_is_reconciled_without_duplicate_write(self):
        r = FenceReads('lost-ack')
        lower.reconcile_ticket(r, r.context(), lower.Budget(5))
        self.assertEqual(len(r.patches), 1)
        self.assertEqual(r.va['metadata']['resourceVersion'], '2')

    def test_noop_or_unread_version_never_establishes_cleanup(self):
        for mode in ('noop', 'missing-version'):
            with self.subTest(mode=mode):
                r = FenceReads(mode)
                with self.assertRaisesRegex(lower.Refusal, 'version|resourceVersion'):
                    lower.reconcile_ticket(r, r.context(), lower.Budget(5))
                self.assertEqual(len(r.patches), 1)

    def test_changed_identity_and_expired_budget_preserve_tickets(self):
        for mode in ('changed', 'uid', 'budget'):
            with self.subTest(mode=mode):
                r = FenceReads()
                r.va['spec']['attachmentTickets']['owned'] = copy.deepcopy(r.ticket)
                if mode == 'changed':
                    r.va['spec']['attachmentTickets']['owned']['generation'] = 2
                elif mode == 'uid':
                    r.va['metadata']['uid'] = 'replacement'
                before = copy.deepcopy(r.va)
                budget = lower.Budget(end=time.monotonic() - 1) if mode == 'budget' else lower.Budget(5)
                with self.assertRaises(lower.Refusal):
                    lower.reconcile_ticket(r, r.context(), budget)
                self.assertEqual(r.va, before)
                self.assertEqual(r.patches, [])


class ClaimReads:
    def __init__(self):
        self.docs = {
            "pvc": {"metadata": {"uid": "claim"}, "spec": {"volumeName": "pv"}, "status": {"phase": "Bound"}},
            "pods": {"items": []}, "jobs": {"items": []},
            "volumeattachments.storage.k8s.io": {"items": []},
            "cronjobs": {"items": [{"metadata": {"name": "backup", "uid": "cron"},
                                     "spec": {"schedule": "10 9 * * *", "timeZone": "Etc/UTC",
                                              "jobTemplate": {"spec": {"template": {"spec": self.uses()}}}}}]}}
        self.v = {"metadata": {"name": "pv"}, "status": {"kubernetesStatus": {"namespace": "boss", "pvcName": "backups", "pvName": "pv"}}}

    @staticmethod
    def uses():
        return {"volumes": [{"persistentVolumeClaim": {"claimName": "backups"}}]}

    def get(self, resource, name=None, namespace=lower.LH):
        return copy.deepcopy(self.docs[resource])


class Quiescence(unittest.TestCase):
    def test_schedule_is_derived_and_exclusion_covers_the_operation(self):
        r = ClaimReads()
        noon = 12 * 3600
        c = lower.claim_idle(r, r.v, noon)
        self.assertEqual(c["schedules"][0]["schedule"], "10 9 * * *")
        for delta in (-lower.WINDOW, 0, lower.WINDOW):
            with self.assertRaises(lower.Refusal):
                lower.claim_idle(r, r.v, 9 * 3600 + 600 + delta)
        for delta in (-lower.WINDOW - 1, lower.WINDOW + 1):
            lower.claim_idle(r, r.v, 9 * 3600 + 600 + delta)

    def test_unknown_timezone_pending_job_pod_and_attachment_refuse(self):
        for kind in ("zone", "pod", "job", "attachment"):
            r = ClaimReads()
            if kind == "zone":
                del r.docs["cronjobs"]["items"][0]["spec"]["timeZone"]
            elif kind == "pod":
                r.docs["pods"]["items"] = [{"spec": r.uses(), "status": {"phase": "Pending"}}]
            elif kind == "job":
                r.docs["jobs"]["items"] = [{"spec": {"template": {"spec": r.uses()}}, "status": {"active": 0}}]
            else:
                r.docs["volumeattachments.storage.k8s.io"]["items"] = [{"spec": {"source": {"persistentVolumeName": "pv"}}, "status": {"attached": False}}]
            with self.subTest(kind=kind), self.assertRaises(lower.Refusal):
                lower.claim_idle(r, r.v, 12 * 3600)

    def test_backup_manifest_pins_the_clock_zone_read_by_the_guard(self):
        manifest = path.parents[1] / "cluster" / "manifests" / "boss-backup.yaml"
        text = manifest.read_text()
        self.assertIn("timeZone: Etc/UTC", text)


class Lifecycle(unittest.TestCase):
    def test_normal_exit_does_not_signal_the_reaped_leaders_numeric_group(self):
        with tempfile.TemporaryDirectory(prefix="boss-lower-pid-") as tmp:
            out = subprocess.run([sys.executable, str(Path(__file__).resolve()), "lifecycle-success", tmp],
                                 capture_output=True, text=True, timeout=10)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertFalse((Path(tmp) / "signalled-reaped-leader").exists())

    def test_failed_work_conserves_stdout_before_cleanup(self):
        with tempfile.TemporaryDirectory(prefix="boss-lower-output-") as tmp:
            out = subprocess.run([sys.executable, str(Path(__file__).resolve()), "lifecycle-failure", tmp],
                                 capture_output=True, text=True, timeout=10)
            self.assertNotEqual(out.returncode, 0)
            self.assertIn("retained-work-output", out.stdout)
            self.assertTrue((Path(tmp) / "cleaned").exists())

    def test_absolute_budget_caps_calls_and_refuses_expired_work(self):
        budget = lower.Budget(end=time.monotonic() + 0.2)
        self.assertGreater(budget.remaining(25), 0)
        self.assertLessEqual(budget.remaining(25), 0.2)
        with self.assertRaises(lower.Refusal):
            lower.Budget(end=time.monotonic() - 1).remaining(25)
        with patch.object(lower.os, "fork", side_effect=AssertionError("expired budget reached fork")):
            with self.assertRaises(lower.Refusal):
                lower.supervised(lambda: None, lambda: None, lower.Budget(0), 5)

    def test_runner_group_kill_and_worker_sigkill_leave_no_ticket_or_child(self):
        self.assertEqual(ctypes.CDLL(None).prctl(36, 1, 0, 0, 0), 0)
        for target in ("runner", "worker"):
            with self.subTest(target=target), tempfile.TemporaryDirectory(prefix="boss-lower-kill-") as tmp:
                root = Path(tmp)
                log = (root / "log").open("w")
                proc = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), "lifecycle-child", tmp],
                                        stdout=log, stderr=log, start_new_session=True)
                try:
                    end = time.monotonic() + 10
                    while not (root / "ticket").exists() and proc.poll() is None and time.monotonic() < end:
                        time.sleep(0.01)
                    self.assertTrue((root / "ticket").exists(), (root / "log").read_text())
                    worker = int((root / "worker").read_text())
                    if target == "runner":
                        os.killpg(proc.pid, signal.SIGKILL)
                    else:
                        os.kill(worker, signal.SIGKILL)
                    proc.wait(timeout=10)
                    while not (root / "cleaned").exists() and time.monotonic() < end:
                        time.sleep(0.01)
                    self.assertTrue((root / "cleaned").exists(), (root / "log").read_text())
                    self.assertFalse((root / "ticket").exists())
                    for name in ("worker", "descendant"):
                        pid = int((root / name).read_text())
                        self.assertFalse(Path(f"/proc/{pid}").exists(), f"{name} {pid} was not reaped")
                    if target == "runner":
                        os.waitpid(-1, 0)  # reap the fixture's adopted cleanup owner too
                finally:
                    if proc.poll() is None:
                        os.killpg(proc.pid, signal.SIGKILL)
                        proc.wait(timeout=10)
                    log.close()


def lifecycle_child(root, mode="child"):
    if mode == "success":
        original = lower.os.killpg
        def observe(group, sig):
            (root / "signalled-reaped-leader").write_text(str(group))
            original(group, sig)
        lower.os.killpg = observe

    def work():
        if mode in ("failure", "success"):
            (root / "ticket").write_text("exact-owned-ticket")
            print("retained-work-output")
            if mode == "failure":
                raise lower.Failure("measured work failure")
            return
        (root / "worker").write_text(str(os.getpid()))
        # Like a GNU-timeout transport, this child has a different group.
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                 start_new_session=True)
        (root / "descendant").write_text(str(child.pid))
        (root / "ticket").write_text("exact-owned-ticket")
        child.wait()

    def cleanup():
        (root / "ticket").unlink()
        (root / "cleaned").write_text("own ticket removed after child reaping")

    lower.supervised(work, cleanup, lower.Budget(15), 5)


if __name__ == "__main__":
    if sys.argv[1:2] in (["lifecycle-child"], ["lifecycle-failure"], ["lifecycle-success"]):
        lifecycle_child(Path(sys.argv[2]), sys.argv[1].removeprefix("lifecycle-"))
    else:
        unittest.main()
