"""The dev controller forwards execution; it never runs worker programs."""
import copy
import os
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class ControllerContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).with_name("dev-build.py")
        spec = importlib.util.spec_from_file_location("dev_build", path)
        cls.mod = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = cls.mod
        spec.loader.exec_module(cls.mod)

    def spec(self):
        return self.mod.worker_job(
            "dev-build-1234567890abcdef", "a" * 40,
            "registry.invalid/build@sha256:" + "b" * 64,
            "http://forge.invalid/owner/repo.git",
        )

    def test_worker_has_no_control_credentials_mounts_or_authority(self):
        job = self.spec()
        self.assertIsNone(self.mod.worker_error(job))
        pod = job["spec"]["template"]["spec"]
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertFalse(pod["shareProcessNamespace"])
        self.assertNotIn("nodeSelector", pod)
        required = pod["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"]
        self.assertEqual(required, [{"matchExpressions": [{"key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist"}]}])
        worker = pod["containers"][0]
        self.assertEqual(worker["name"], "worker")
        self.assertTrue(worker["securityContext"]["runAsNonRoot"])
        self.assertFalse(worker["securityContext"]["allowPrivilegeEscalation"])
        self.assertEqual(worker["securityContext"]["capabilities"]["drop"], ["ALL"])
        self.assertEqual([m["name"] for m in worker["volumeMounts"]], ["workspace", "worker-tmp"])
        self.assertEqual(next(e["value"] for e in worker["env"] if e["name"] == "PATH"), "/workspace/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin")
        self.assertEqual(pod["initContainers"][0]["name"], "published-bootstrap")
        self.assertNotIn("BOSS_ACTOR", {e["name"] for e in worker["env"]})

    def test_any_added_mount_secret_env_or_host_authority_is_refused(self):
        for mutate in (
            lambda p: p.update(hostPID=True),
            lambda p: p.update(hostNetwork=True),
            lambda p: p.update(automountServiceAccountToken=True),
            lambda p: p.update(shareProcessNamespace=True),
            lambda p: p["containers"][0]["volumeMounts"].append(
                {"name": "forge-read", "mountPath": "/etc/forge"}),
            lambda p: p["containers"][0]["env"].append(
                {"name": "BOSS_MACHINE_TOKEN", "value": "fake"}),
            lambda p: p["volumes"].append(
                {"name": "control", "hostPath": {"path": "/work"}}),
            lambda p: p["containers"][0]["securityContext"].update(
                allowPrivilegeEscalation=True),
            lambda p: p["containers"][0].update(command=["bash", "-c", "invented startup"]),
            lambda p: p["containers"][0]["env"][0].update(value="/work/home"),
            lambda p: p["containers"][0].update(envFrom=[{"secretRef": {"name": "fake"}}]),
            lambda p: p["containers"][1].update(envFrom=[{"secretRef": {"name": "fake"}}]),
            lambda p: p["containers"][0].update(lifecycle={"postStart": {"exec": {"command": ["candidate"]}}}),
        ):
            job = self.spec()
            mutate(job["spec"]["template"]["spec"])
            self.assertIsNotNone(self.mod.worker_error(job), job)

    def test_injection_cannot_choose_another_target_or_bootstrap_source(self):
        for name, head, image, url in (
            ("../another", "a" * 40, "registry.invalid/x@sha256:" + "b" * 64, "http://forge.invalid/x.git"),
            ("dev-build-1234567890abcdef", "main;echo bad", "registry.invalid/x@sha256:" + "b" * 64, "http://forge.invalid/x.git"),
            ("dev-build-1234567890abcdef", "a" * 40, "registry.invalid/x:latest", "http://forge.invalid/x.git"),
            ("dev-build-1234567890abcdef", "a" * 40, "registry.invalid/x@sha256:" + "b" * 64, "http://name:password@forge.invalid/x.git"),
        ):
            with self.assertRaises(ValueError):
                self.mod.worker_job(name, head, image, url)

    def test_bootstrap_cannot_be_replaced_with_credential_bearing_candidate_code(self):
        for mutate in (
            lambda c: c.update(command=["bash", "-c", "cat /bootstrap-forge/token"]),
            lambda c: c["securityContext"].update(runAsUser=0),
            lambda c: c["volumeMounts"].append({"name": "workspace", "mountPath": "/control"}),
            lambda c: c.update(envFrom=[{"secretRef": {"name": "machine-token"}}]),
        ):
            job = self.spec()
            mutate(job["spec"]["template"]["spec"]["initContainers"][0])
            self.assertIsNotNone(self.mod.worker_error(job))

    def test_admission_defaults_do_not_invalidate_the_declared_boundary(self):
        job = self.spec()
        pod = job["spec"]["template"]["spec"]
        pod["containers"][1]["readinessProbe"].update(timeoutSeconds=1, successThreshold=1, failureThreshold=3)
        for container in pod["containers"] + pod["initContainers"]:
            container.update(terminationMessagePath="/dev/termination-log", terminationMessagePolicy="File")
        self.assertIsNone(self.mod.worker_error(job))

    def test_wrong_uid_restart_and_mutated_mount_refuse_before_exec(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        job = self.spec()
        pod = {"metadata": {"name": "pod-name", "uid": "pod-uid", "ownerReferences": [{"uid": "job-uid", "kind": "Job"}]},
               "spec": copy.deepcopy(job["spec"]["template"]["spec"]),
               "status": {"phase": "Running", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"terminated": {"exitCode": 0}}}], "containerStatuses": [{"name": "worker", "ready": True, "restartCount": 0, "containerID": "container-id", "state": {"running": {}}}, {"name": "postgres", "ready": True, "restartCount": 0, "containerID": "postgres-id", "state": {"running": {}}}]}}
        self.assertIsNone(self.mod.session_error(session, pod))
        for mutate in (
            lambda p: p["metadata"].update(uid="replacement"),
            lambda p: p["status"]["containerStatuses"][0].update(restartCount=1),
            lambda p: p["status"]["containerStatuses"][0].update(containerID="replacement"),
            lambda p: p["spec"].update(automountServiceAccountToken=True),
            lambda p: p["status"]["containerStatuses"][1].update(ready=False),
            lambda p: p["status"]["initContainerStatuses"][0].update(state={"running": {}}),
            lambda p: p["status"]["initContainerStatuses"][0].update(restartCount=1),
            lambda p: p["status"].pop("initContainerStatuses"),
        ):
            bad = copy.deepcopy(pod)
            mutate(bad)
            self.assertIsNotNone(self.mod.session_error(session, bad))

    def test_candidate_output_is_inert_and_transport_failure_never_runs_locally(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        calls = []
        def transport(argv, stdin):
            calls.append((argv, stdin))
            return subprocess.CompletedProcess(argv, 42, b'{"success":true,"command":"touch /control"}', b"lost acknowledgment")
        result = self.mod.worker_exec(session, ["bash", "-c", "candidate command"], b"plain input", transport)
        self.assertEqual(result.returncode, 42)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], ["exec", "-i", "pod-name", "-c", "worker", "--", "bash", "-c", "candidate command"])
        self.assertEqual(calls[0][1], b"plain input")
        self.assertEqual(result.stdout, b'{"success":true,"command":"touch /control"}')

    def test_over_limit_stdin_is_refused_without_execution(self):
        calls = []
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        with self.assertRaises(ValueError):
            self.mod.worker_exec(session, ["cat"], b"x" * (16 * 1024 * 1024 + 1), lambda *args: calls.append(args))
        self.assertEqual(calls, [])

    def test_auto_execution_maps_only_the_bound_source_and_keeps_argv_data(self):
        binding = {"local_root": "/scratch/wt/agent-car"}
        argv = self.mod.bound_argv(binding, "/scratch/wt/agent-car/apps/web", ["/scratch/wt/agent-car/target/debug/boss", "literal;touch /control"])
        self.assertEqual(argv, ["/usr/bin/env", "-C", "/workspace/source/apps/web", "/workspace/source/target/debug/boss", "literal;touch /control"])
        with self.assertRaises(ValueError):
            self.mod.bound_argv(binding, "/work/home", ["bash"])
        with self.assertRaises(ValueError):
            self.mod.bound_argv(binding, "/scratch/wt/agent-car/../other", ["bash"])

    def test_stale_source_binding_refuses_before_any_worker_execution(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod", "pod-uid", "container-id", "a" * 40)
        binding = {"session": session.job, "local_root": "/scratch/wt/agent-car", "source_head": "a" * 40}
        self.mod.check_binding(binding, session, "a" * 40)
        for wrong in ("b" * 40, "not-a-commit"):
            with self.assertRaises(ValueError):
                self.mod.check_binding(binding, session, wrong)
        bad = dict(binding, source_head="b" * 40)
        with self.assertRaises(ValueError):
            self.mod.check_binding(bad, session, "a" * 40)

    def test_retirement_blocks_execution_and_preserves_unknown_outcome(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = self.mod.Session("dev-build-1234567890abcdef", "known-job-uid", "pod", "pod-uid", "container-id", "a" * 40)
            calls = []
            def delete(name, uid, record):
                calls.append((name, uid))
                self.assertTrue((root / (name + ".retire-intent.json")).exists())
                record({"completion": "unproven", "error": "lost API acknowledgment"})
                raise RuntimeError("lost API acknowledgment")
            with self.assertRaises(RuntimeError):
                self.mod.retire_worker(session, root, delete)
            self.assertEqual(calls, [(session.job, session.job_uid)])
            self.assertTrue(self.mod.retirement_requested(root, session.job))
            self.assertFalse((root / (session.job + ".retired.json")).exists())
            with self.assertRaises((ValueError, FileExistsError)):
                self.mod.retire_worker(session, root, delete)
            self.assertEqual(len(calls), 1, "unknown retirement is never blindly retried")

    def test_public_probe_values_are_worker_argv_and_never_controller_environment(self):
        argv = self.mod.probe_argv(["printf", "literal;touch /control"], ["BOSS_JOBS_URL=http://probe.invalid:7900", "BOSS_CAR_CONVERGED_AT=example time"])
        self.assertEqual(argv, ["/usr/bin/env", "--", "BOSS_JOBS_URL=http://probe.invalid:7900", "BOSS_CAR_CONVERGED_AT=example time", "printf", "literal;touch /control"])
        for entry in ("BASH_ENV=/candidate", "PATH=/candidate", "BOSS_ACTOR=admin", "BOSS_MACHINE_TOKEN=secret", "BOSS_JOBS_URL=bad\0value"):
            with self.assertRaises(ValueError):
                self.mod.probe_argv(["true"], [entry])

    def test_probe_channel_is_only_an_existing_owned_parent_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            channel = root / ("boss-prove-notfound-123-" + str(os.getuid()) + "-" + str(os.getppid()))
            channel.write_bytes(b"")
            fd = self.mod.probe_channel(channel)
            os.write(fd, b"missing-command\n")
            os.close(fd)
            self.assertEqual(channel.read_bytes(), b"missing-command\n")
            for path in (root / "arbitrary-control-config", root / "boss-prove-notfound-123-0-99999999"):
                path.write_bytes(b"")
                with self.assertRaises(ValueError):
                    self.mod.probe_channel(path)
            channel.unlink()
            channel.symlink_to(root / "arbitrary-control-config")
            with self.assertRaises((ValueError, OSError)):
                self.mod.probe_channel(channel)

    def test_kubernetes_transport_uses_the_same_published_generation_as_controller(self):
        calls = []
        def run(argv, **kwargs):
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, b"observation", b"")
        result = self.mod.kube(["get", "pod", "bound-worker"], b"input", run=run)
        generation = Path(self.mod.__file__).resolve().parents[2]
        self.assertEqual(calls[0][0], [str(generation / "bin/kubectl"), "--namespace=boss-dev", "get", "pod", "bound-worker"])
        self.assertEqual(calls[0][1]["input"], b"input")
        self.assertEqual(result.stdout, b"observation")

    def test_exec_has_a_build_bound_and_api_reads_have_a_short_bound(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(kwargs["timeout"])
            return subprocess.CompletedProcess(argv, 0, b"", b"")
        self.mod.kube(["exec", "-i", "worker", "-c", "worker", "--", "cargo", "test"], run=run)
        self.mod.kube(["get", "pod", "worker"], run=run)
        self.assertEqual(calls, [86400, 120])

    def test_failed_api_observation_retains_full_raw_evidence(self):
        records = []
        def transport(argv):
            return subprocess.CompletedProcess(argv, 1, b"full raw observation" * 10000, b"API refused the observed resource")
        with self.assertRaises(RuntimeError):
            self.mod.kube_json(["get", "job", "bound-job"], transport=transport, record=records.append)
        import base64
        self.assertEqual(base64.b64decode(records[0]["stdout_base64"]), b"full raw observation" * 10000)
        self.assertEqual(records[0]["exit"], 1)
        self.assertEqual(records[0]["completion"], "observed")

    def test_source_transfer_accepts_edits_and_refuses_control_or_cache_paths(self):
        argv = self.mod.source_file_argv("put", "crates/core/boss-core/src/lib.rs")
        self.assertEqual(argv[:3], ["/usr/bin/python3", "-I", "-c"])
        self.assertEqual(argv[-2:], ["put", "crates/core/boss-core/src/lib.rs"])
        for path in ("/work/home/.ssh/key", "../control", ".git/config", "apps/web/node_modules/pkg/index.js", "target/debug/boss", ".cargo/credentials.toml", ".config/boss/actor"):
            with self.assertRaises(ValueError):
                self.mod.source_file_argv("get", path)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates/core/boss-core/src").mkdir(parents=True)
            argv[3] = argv[3].replace("/workspace/source", str(root))
            edited = b"source edit bytes\n"
            result = subprocess.run(argv, input=edited, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / "crates/core/boss-core/src/lib.rs").read_bytes(), edited)
            read = self.mod.source_file_argv("get", "crates/core/boss-core/src/lib.rs")
            read[3] = read[3].replace("/workspace/source", str(root))
            result = subprocess.run(read, capture_output=True, check=False)
            self.assertEqual(result.stdout, edited)

    def test_database_is_local_disposable_and_cannot_hold_control_credentials(self):
        job = self.spec()
        pod = job["spec"]["template"]["spec"]
        postgres = next(c for c in pod["containers"] if c["name"] == "postgres")
        self.assertFalse(postgres["securityContext"]["allowPrivilegeEscalation"])
        self.assertTrue(postgres["securityContext"]["runAsNonRoot"])
        self.assertEqual(postgres["securityContext"]["capabilities"]["drop"], ["ALL"])
        self.assertEqual({m["name"] for m in postgres["volumeMounts"]}, {"pgdata", "pg-run", "pg-shm", "pg-tmp"})
        self.assertNotIn("workspace", {m["name"] for m in postgres["volumeMounts"]})
        self.assertTrue(all("secret" not in v and "hostPath" not in v for v in pod["volumes"] if v["name"].startswith("pg-")))

    def test_lost_incarnation_retains_full_output_without_success_receipt(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        reads = []
        records = []
        def inspect(_):
            reads.append(True)
            if len(reads) == 2:
                raise RuntimeError("worker disappeared")
        def transport(argv, stdin):
            return subprocess.CompletedProcess(argv, 0, b"\xff{" + b"x" * 100000 + b"}", b"diagnostic")
        with self.assertRaisesRegex(RuntimeError, "worker disappeared"):
            self.mod.observed_exec(session, ["candidate"], b"", transport, inspect, records.append)
        self.assertEqual(len(records), 1)
        record = records[0]
        self.assertEqual(record["completion"], "unproven")
        import base64
        self.assertEqual(base64.b64decode(record["stdout_base64"]), b"\xff{" + b"x" * 100000 + b"}")
        self.assertEqual(base64.b64decode(record["stderr_base64"]), b"diagnostic")

    def test_timeout_retains_partial_output_and_does_not_retry(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        calls = []
        records = []
        def transport(argv, stdin):
            calls.append(argv)
            raise subprocess.TimeoutExpired(argv, 1, output=b"full partial evidence", stderr=b"cause")
        with self.assertRaises(subprocess.TimeoutExpired):
            self.mod.observed_exec(session, ["candidate"], b"", transport, lambda _: None, records.append)
        self.assertEqual(len(calls), 1)
        self.assertEqual(records[0]["completion"], "unproven")
        import base64
        self.assertEqual(base64.b64decode(records[0]["stdout_base64"]), b"full partial evidence")

    def test_lost_launch_acknowledgment_retains_full_evidence_and_never_retries(self):
        calls = []
        def transport(argv, stdin=None):
            calls.append((argv, stdin))
            return subprocess.CompletedProcess(argv, 28, b"returned bytes" * 10000, b"acknowledgment lost")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
            with self.assertRaises(RuntimeError):
                self.mod.launch("a" * 40, config, root, transport=transport)
            receipts = list(root.glob("*.launch-*.json"))
            self.assertEqual(len(receipts), 1, "a failed launch owes full transport evidence")
            import base64
            import json
            receipt = json.loads(receipts[0].read_text())
            self.assertEqual(base64.b64decode(receipt["stdout_base64"]), b"returned bytes" * 10000)
            self.assertEqual(base64.b64decode(receipt["stderr_base64"]), b"acknowledgment lost")
            self.assertEqual(len(calls), 1)
            self.assertEqual(receipt["exit"], 28)
            self.assertEqual(json.loads(calls[0][1])["kind"], "PersistentVolumeClaim", "a token-bearing init must never start before private fresh storage is acknowledged")

    def launch_transport(self, replacement=False):
        import json
        calls = []
        objects = {}
        def transport(argv, stdin=None):
            calls.append(argv)
            if argv[0] == "create":
                obj = json.loads(stdin)
                obj["metadata"]["uid"] = "pvc-uid" if obj["kind"] == "PersistentVolumeClaim" else "job-uid"
                objects[obj["kind"]] = obj
                return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode(), b"")
            if argv[0] == "patch":
                patch = json.loads(argv[argv.index("-p") + 1])
                self.assertEqual(patch[0], {"op": "test", "path": "/metadata/uid", "value": "pvc-uid"})
                self.assertEqual(patch[1]["value"][0]["uid"], "job-uid")
                volume = objects["PersistentVolumeClaim"]
                volume["metadata"]["ownerReferences"] = patch[1]["value"]
                if replacement:
                    volume["metadata"]["uid"] = "replacement"
                return subprocess.CompletedProcess(argv, 0, json.dumps(volume).encode(), b"")
            job = objects["Job"]
            pod = {"metadata": {"name": "bound-pod", "uid": "pod-uid", "ownerReferences": [{"kind": "Job", "uid": "job-uid"}]},
                   "spec": job["spec"]["template"]["spec"],
                   "status": {"phase": "Running", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"terminated": {"exitCode": 0}}}], "containerStatuses": [{"name": name, "ready": True, "restartCount": 0, "containerID": name + "-id", "state": {"running": {}}} for name in ("worker", "postgres")]}}
            return subprocess.CompletedProcess(argv, 0, json.dumps({"items": [pod]}).encode(), b"")
        return calls, transport

    def test_fresh_volume_precedes_job_and_gc_patch_tests_its_acknowledged_uid(self):
        import json
        calls, transport = self.launch_transport()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
            self.mod.launch("a" * 40, config, root, transport=transport)
            records = [p for p in root.glob("dev-build-*.json") if not any(tag in p.name for tag in (".intent", ".volume", ".launch-"))]
            self.assertEqual(len(records), 1)
            self.assertEqual(json.loads(records[0].read_text())["volume_uid"], "pvc-uid")
            self.assertEqual([argv[0] for argv in calls], ["create", "create", "patch", "get"])

    def test_replaced_volume_patch_refuses_before_worker_admission(self):
        calls, transport = self.launch_transport(replacement=True)
        with tempfile.TemporaryDirectory() as tmp:
            config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
            with self.assertRaises(RuntimeError):
                self.mod.launch("a" * 40, config, Path(tmp), transport=transport)
            self.assertEqual([argv[0] for argv in calls], ["create", "create", "patch"])

    def test_a_receipt_fifo_is_refused_without_blocking_the_controller(self):
        import os
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "receipt.json"
            os.mkfifo(path, 0o600)
            code = 'import importlib.util,sys; spec=importlib.util.spec_from_file_location("controlled",sys.argv[1]); module=importlib.util.module_from_spec(spec); sys.modules[spec.name]=module; spec.loader.exec_module(module); module.load_record(sys.argv[2])'
            try:
                result = subprocess.run([sys.executable, "-I", "-c", code, self.mod.__file__, str(path)], capture_output=True, timeout=1, check=False)
            except subprocess.TimeoutExpired:
                self.fail("a receipt FIFO blocked before its regular-file validation")
            self.assertNotEqual(result.returncode, 0)

    def test_interrupted_record_flush_cannot_publish_a_partial_receipt(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "receipt.json"
            with patch.object(self.mod.os, "fsync", side_effect=OSError("fixture disk failure")):
                with self.assertRaises(OSError):
                    self.mod.save_record(path, {"completion": "observed", "output": "x" * 100000})
            self.assertFalse(path.exists(), "an interrupted write must leave no published receipt")
            self.mod.save_record(path, {"completion": "unproven"})
            self.assertEqual(self.mod.load_record(path), {"completion": "unproven"})
            with self.assertRaises(FileExistsError):
                self.mod.save_record(path, {"completion": "observed"})
            self.assertEqual(self.mod.load_record(path), {"completion": "unproven"})


if __name__ == "__main__":
    unittest.main()
