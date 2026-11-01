"""The dev controller forwards execution; it never runs worker programs."""
import contextlib
import copy
import base64
import hashlib
import dataclasses
import json
import os
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest


GIB = 2 ** 30


def longhorn_node(name, maximum_gib, available_gib, ready=True, allow=True, disk_allow=True, disk_eviction=False):
    """One Longhorn v1beta2 Node with one disk, in the shape the API lists."""
    def conditions(**kinds):
        return [{"type": kind, "status": "True" if value else "False"} for kind, value in kinds.items()]
    return {"metadata": {"name": name},
            "spec": {"allowScheduling": allow, "evictionRequested": False,
                     "disks": {"default-disk": {"allowScheduling": disk_allow, "evictionRequested": disk_eviction,
                                                "path": "/var/lib/longhorn", "storageReserved": 0}}},
            "status": {"conditions": conditions(Ready=ready, Schedulable=ready),
                       "diskStatus": {"default-disk": {"storageMaximum": int(maximum_gib * GIB),
                                                       "storageAvailable": int(available_gib * GIB),
                                                       "storageScheduled": 0,
                                                       "conditions": conditions(Ready=True, Schedulable=True)}}}}


GIT_ENV = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", "GIT_CONFIG_GLOBAL": "/dev/null",
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "fixture", "GIT_AUTHOR_EMAIL": "fixture@boss.invalid",
           "GIT_COMMITTER_NAME": "fixture", "GIT_COMMITTER_EMAIL": "fixture@boss.invalid"}


def git(repo, *args):
    """A fixture commit in a real repository, isolated from any user config."""
    return subprocess.run([shutil.which("git"), "-C", str(repo), *args], env=GIT_ENV,
                          capture_output=True, check=True).stdout.decode().strip()


def host_git(argv, **kwargs):
    """The controller's metadata read, with this host's git for its fixed path."""
    return subprocess.run([shutil.which("git"), *argv[1:]], **kwargs)


# Room for every launch a test makes that is not about the budget.
ROOMY = [longhorn_node("w-1", 2000, 2000)]


def longhorn_reply(argv, nodes=ROOMY, floor="25"):
    """The fake reader's answer to the two Longhorn reads, or None."""
    if argv[:2] == ["get", "nodes.longhorn.io"]:
        return subprocess.CompletedProcess(argv, 0, json.dumps({"items": nodes}).encode(), b"")
    if argv[:3] == ["get", "settings.longhorn.io", "storage-minimal-available-percentage"]:
        return subprocess.CompletedProcess(argv, 0, json.dumps({"value": floor}).encode(), b"")
    return None


class ControllerContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).with_name("dev-build.py")
        spec = importlib.util.spec_from_file_location("dev_build", path)
        cls.mod = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = cls.mod
        spec.loader.exec_module(cls.mod)

    def setUp(self):
        from unittest.mock import patch
        # A captured default bypasses a patched Path constructor. Refuse
        # its first filesystem operation before a fixture can touch the
        # operator's controller records (review 1ee318ed, item e04d2fe8).
        default_root = self.mod.main.__defaults__[-1]
        self.assertIsInstance(default_root, Path)
        for operation in ("mkdir", "open", "stat", "lstat"):
            original = getattr(Path, operation)
            def guarded(path, *args, _original=original, **kwargs):
                if path == default_root or default_root in path.parents:
                    raise AssertionError("controller fixture must pass a temporary root explicitly: " + str(path))
                return _original(path, *args, **kwargs)
            guard = patch.object(Path, operation, guarded)
            guard.start()
            self.addCleanup(guard.stop)

    def spec(self):
        return self.mod.worker_job(
            "dev-build-1234567890abcdef", "a" * 40,
            "registry.invalid/build@sha256:" + "b" * 64,
            "http://forge.invalid/owner/repo.git",
        )

    def test_owned_runs_inventory_is_bounded_fair_and_never_trusts_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runs = [str(__import__('uuid').UUID(int=n)) for n in range(1, 5)]
            for run in runs:
                self.mod.save_record(self.mod.run_worker_path(root, run, "intent.json"),
                                     {"run":run,"session":"dev-build-1234567890abcdef",
                                      "source_head":"a" * 40,"local_root":"/work/tree"})
            first = self.mod.owned_runs(root, "", 2)
            second = self.mod.owned_runs(root, first["next"], 2)
            self.assertEqual([r["run"] for r in first["runs"] + second["runs"]], runs)
            self.assertEqual(self.mod.owned_runs(root, second["next"], 2)["runs"], first["runs"])
            path = self.mod.run_worker_path(root, runs[0], "intent.json")
            target = root / "untrusted"
            path.rename(target)
            path.symlink_to(target)
            refused = self.mod.owned_runs(root, "", 2)
            self.assertIn("error", refused["runs"][0])
            self.assertEqual(refused["runs"][1]["run"], runs[1], "one unread record cannot starve another")

    def test_owned_runs_inventory_refuses_mismatched_native_and_run_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = "11111111-1111-4111-8111-111111111111"
            name = "dev-build-1234567890abcdef"
            owned = {"run":run,"session":name,"source_head":"a" * 40,"local_root":"/work/tree",
                     "native": self.mod.asdict(self.mod.Session(name,"job","pod","uid","container","a" * 40,"volume"))}
            self.mod.save_record(self.mod.run_worker_path(root, run), owned)
            self.mod.save_record(root / (name + ".json"), {**owned["native"],"job_uid":"replacement"})
            self.assertIn("identity", self.mod.owned_runs(root, "", 2)["runs"][0]["error"])

    def test_owned_runs_prioritizes_unretired_owners_and_reserves_retirement_catchup(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runs = [str(__import__('uuid').UUID(int=n)) for n in range(1, 5)]
            for run in runs:
                self.mod.save_record(self.mod.run_worker_path(root, run, "intent.json"),
                                     {"run":run,"session":"dev-build-1234567890abcdef",
                                      "source_head":"a" * 40,"local_root":"/work/tree"})
            for run in runs[:3]:
                self.mod.save_record(self.mod.run_worker_path(root, run, "stopped.json"),
                                     {"run":run,"session":"dev-build-1234567890abcdef","retirement":{"exact":True}})
            first = self.mod.owned_runs(root, "", 2)
            self.assertEqual([r["run"] for r in first["runs"]], [runs[3],runs[0]])
            second = self.mod.owned_runs(root, first["next"], 2, first["retired_next"])
            self.assertEqual([r["run"] for r in second["runs"]], [runs[3],runs[1]])

    def test_stop_run_refuses_a_replaced_identity_and_retries_one_failed_stop(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = "11111111-1111-4111-8111-111111111111"
            name = "dev-build-1234567890abcdef"
            native = self.mod.asdict(self.mod.Session(name,"original","pod","uid","container","a" * 40,"volume"))
            self.mod.save_record(self.mod.run_worker_path(root, run), {"run":run,"session":name,"native":native})
            self.mod.save_record(root / (name + ".json"), {**native,"job_uid":"replacement"})
            calls = []
            def retire(worker):
                calls.append(worker)
                if len(calls) == 1:
                    raise RuntimeError("UID-conditioned native delete refused")
                return {"exact":"original retirement"}
            with self.assertRaisesRegex(ValueError, "identity changed"):
                self.mod.stop_run(run, root, retire)
            self.assertEqual(calls, [])
            (root / (name + ".json")).unlink()
            self.mod.save_record(root / (name + ".json"), native)
            with self.assertRaisesRegex(RuntimeError, "UID-conditioned"):
                self.mod.stop_run(run, root, retire)
            receipt = self.mod.stop_run(run, root, retire)
            self.assertEqual(self.mod.stop_run(run, root, retire), receipt)
            self.assertEqual(calls, [name, name], "failed attempt retries once; success is retained")

    def test_owned_runs_does_not_wait_on_an_active_admission_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runs = [str(__import__('uuid').UUID(int=n)) for n in (1, 2)]
            for run in runs:
                self.mod.save_record(self.mod.run_worker_path(root, run, "intent.json"),
                                     {"run":run,"session":"dev-build-1234567890abcdef",
                                      "source_head":"a" * 40,"local_root":"/work/tree"})
            done = threading.Event()
            result = []
            def inventory():
                result.append(self.mod.owned_runs(root, "", 2))
                done.set()
            try:
                with self.mod.controller_lock(self.mod.run_worker_path(root, runs[0], "lock")):
                    thread = threading.Thread(target=inventory)
                    thread.start()
                    self.assertTrue(done.wait(1), "one ongoing admission must not block later run observations")
            finally:
                thread.join(2)
            self.assertIn("error", result[0]["runs"][0])
            self.assertEqual(result[0]["runs"][1]["run"], runs[1])

    def test_stop_run_retains_the_actual_attempt_time_and_distinguishes_old_retirement(self):
        for marker in (None, ".retired.json", ".retire-intent.json"):
            with self.subTest(marker=marker), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                run = "11111111-1111-4111-8111-111111111111"
                name = "dev-build-1234567890abcdef"
                native = self.mod.asdict(self.mod.Session(name,"original","pod","uid","container","a" * 40,"volume"))
                self.mod.save_record(root / (name + ".json"), native)
                self.mod.save_record(self.mod.run_worker_path(root, run), {"run":run,"session":name,"native":native})
                if marker:
                    self.mod.save_record(root / (name + marker), {"original":"unchanged"})
                before = int(self.mod.time.time())
                calls = []
                def retire(worker):
                    calls.append(worker)
                    return {"native":"exact retirement receipt"}
                receipt = self.mod.stop_run(run, root, retire)
                self.assertEqual(receipt["retirement_preexisting"], marker is not None)
                self.assertGreaterEqual(receipt["retirement_requested_at_epoch"], before)
                self.assertLessEqual(receipt["retirement_requested_at_epoch"], int(self.mod.time.time()))
                self.assertEqual(receipt["retirement"], {"native":"exact retirement receipt"})
                self.assertEqual(self.mod.stop_run(run, root, retire), receipt)
                self.assertEqual(calls, [name])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run = "11111111-1111-4111-8111-111111111111"
            old = {"run":run,"session":"dev-build-1234567890abcdef","retirement":{"original":"old receipt"}}
            self.mod.save_record(self.mod.run_worker_path(root, run, "stopped.json"), old)
            self.assertEqual(self.mod.stop_run(run, root, lambda name: self.fail("cached receipt must not run a stop")), old,
                             "old evidence is returned byte-shaped unchanged, never retroactively qualified")

    def test_run_binding_adopts_one_prebound_worker_and_retries_without_allocating(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, local = Path(tmp), Path(tmp) / "tree"
            local.mkdir()
            (local / ".git").write_text("gitdir: fixture\n")
            name, run = "dev-build-1234567890abcdef", "11111111-1111-4111-8111-111111111111"
            session = self.mod.Session(name, "job", "pod", "pod-uid", "container", "a" * 40, "volume")
            self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
            self.mod.save_record(root / (name + ".binding.json"), {"session":name,"local_root":str(local),"source_head":"a" * 40})
            def never(*args):
                self.fail("a prebound tree must not allocate a second worker")
            options = {"inspect":lambda session: None,"head_at":lambda tree: "a" * 40}
            first = self.mod.ensure_run(run, "a" * 40, local, root, never, **options)
            self.assertEqual(first["session"], name)
            self.assertEqual(first["run"], run)
            self.assertEqual(self.mod.ensure_run(run, "a" * 40, local, root, never, **options), first)
            with self.assertRaisesRegex(ValueError, "another run"):
                self.mod.ensure_run("22222222-2222-4222-8222-222222222222", "a" * 40, local, root, never, **options)

    def test_run_binding_retains_failed_allocation_identity_and_does_not_retry_blindly(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, local = Path(tmp), Path(tmp) / "tree"
            local.mkdir()
            (local / ".git").write_text("gitdir: fixture\n")
            calls = []
            def refused(name, head, tree):
                calls.append(name)
                raise RuntimeError("storage budget: 25 GiB reserved; full allocation refusal")
            run = "11111111-1111-4111-8111-111111111111"
            with self.assertRaisesRegex(RuntimeError, "full allocation refusal"):
                self.mod.ensure_run(run, "a" * 40, local, root, refused, head_at=lambda tree: "a" * 40)
            with self.assertRaisesRegex(ValueError, calls[0]):
                self.mod.ensure_run(run, "a" * 40, local, root, refused, head_at=lambda tree: "a" * 40)
            self.assertEqual(len(calls), 1)
            retired = self.mod.stop_run(run, root, lambda name: self.fail("a budget refusal created nothing"))
            self.assertEqual(retired["state"], "not-admitted")

    def test_run_binding_allocates_once_and_stop_uses_its_retained_native_identity_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, local = Path(tmp), Path(tmp) / "tree"
            local.mkdir()
            (local / ".git").write_text("gitdir: fixture\n")
            allocated, stopped, inspected = [], [], []
            def allocate(name, head, tree):
                allocated.append(name)
                session = self.mod.Session(name, "job", "pod", "pod-uid", "container", head, "volume")
                self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
                self.mod.save_record(root / (name + ".binding.json"), {"session":name,"local_root":str(tree),"source_head":head})
            run = "11111111-1111-4111-8111-111111111111"
            options = {"inspect":lambda session: inspected.append(session), "head_at":lambda tree: "a" * 40}
            receipt = self.mod.ensure_run(run, "a" * 40, local, root, allocate, **options)
            self.assertEqual(self.mod.ensure_run(run, "a" * 40, local, root, allocate, **options), receipt)
            self.assertEqual(len(allocated), 1)
            self.assertEqual(len(inspected), 2, "a retry observes the native worker again")
            def retire(name):
                stopped.append(name)
                return {"state":"retired","job_uid":"job","volume_uid":"volume"}
            first = self.mod.stop_run(run, root, retire)
            self.assertEqual(self.mod.stop_run(run, root, retire), first)
            self.assertEqual(stopped, allocated)

    def test_run_binding_refuses_a_shared_checkout_before_any_allocation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, local = Path(tmp), Path(tmp) / "tree"
            local.mkdir()
            (local / ".git").mkdir()
            def never(*args):
                self.fail("the shared checkout is not a run worktree")
            with self.assertRaisesRegex(ValueError, "own linked worktree"):
                self.mod.ensure_run("11111111-1111-4111-8111-111111111111", "a" * 40,
                    local, root, never, head_at=lambda tree: "a" * 40)

    def test_stop_run_delegates_an_unadmitted_native_launch_to_the_volume_cleanup_door(self):
        with tempfile.TemporaryDirectory() as tmp:
            root, local = Path(tmp), Path(tmp) / "tree"
            local.mkdir()
            (local / ".git").write_text("gitdir: fixture\n")
            def interrupted(name, head, tree):
                config = self.config()
                job = self.mod.worker_job(name, head, config["worker_image"], config["forge_url"], config["postgres_image"])
                self.mod.save_record(root / (name + ".intent.json"), {"name":name,"head":head,"job":job})
                raise RuntimeError("Job create response unknown after volume admission")
            run = "11111111-1111-4111-8111-111111111111"
            with self.assertRaises(RuntimeError):
                self.mod.ensure_run(run, "a" * 40, local, root, interrupted, head_at=lambda tree: "a" * 40)
            stopped = []
            def release(name):
                stopped.append(name)
                return {"state":"volume ownership handed to garbage collector","gc_observed":False}
            receipt = self.mod.stop_run(run, root, release)
            self.assertEqual(len(stopped), 1, "no Job acknowledgment does not mean no PVC")
            self.assertFalse(receipt["retirement"]["gc_observed"])

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

    def test_the_bootstrap_clone_carries_the_trunk_and_makes_no_setgid_directory(self):
        # Backlog 12369ed7 (2026-10-22): the pre-flight run in a worker
        # could never be green. Its clone held one fetched commit and no
        # trunk ref, so five baseline lints refused on every branch; and
        # the fsGroup volume root is setgid, which every directory made
        # under it inherited, so mode pins read 02755 for the 0755 they
        # set. The bootstrap itself runs here against a fixture forge,
        # with /workspace and the token mount moved under a scratch root
        # that is setgid the way the volume root is.
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            forge, workspace, token = tmp / "forge", tmp / "workspace", tmp / "token"
            forge.mkdir()
            git(forge, "init", "-q", "-b", "main")
            git(forge, "commit", "-q", "--allow-empty", "-m", "trunk base")
            git(forge, "checkout", "-q", "-b", "car")
            git(forge, "commit", "-q", "--allow-empty", "-m", "the car")
            car = git(forge, "rev-parse", "car")
            git(forge, "checkout", "-q", "main")
            git(forge, "commit", "-q", "--allow-empty", "-m", "trunk moved on")
            trunk = git(forge, "rev-parse", "main")
            token.write_text("fixture\n")
            workspace.mkdir()
            os.chmod(workspace, 0o2775)
            self.assertTrue(os.stat(workspace).st_mode & 0o2000, "the fixture root must be setgid, as the volume root is")
            script = self.spec()["spec"]["template"]["spec"]["initContainers"][0]["command"][2]
            script = script.replace("/bootstrap-forge/token", str(token)).replace("/workspace", str(workspace))
            env = {"PATH": os.path.dirname(shutil.which("git")) + ":/usr/bin:/bin", "TMPDIR": str(tmp)}
            ran = subprocess.run(["/bin/bash", "-c", script, "bootstrap", car, str(forge)],
                                 env=env, capture_output=True)
            self.assertEqual(ran.returncode, 0, ran.stderr.decode())
            source = workspace / "source"
            self.assertEqual(git(source, "rev-parse", "HEAD"), car, "the admitted commit is what is checked out")
            self.assertEqual(git(source, "rev-parse", "refs/remotes/origin/main"), trunk, "the forge's main is the trunk")
            # Read the way the lints read it, not only by name.
            lib = Path(__file__).resolve().parent.parent / "lint/lib/trunk-ref.sh"
            found = subprocess.run(["/bin/bash", "-c", '. "$0" && resolve_trunk_ref fixture', str(lib)],
                                   cwd=source, env={**env, "HOME": str(tmp)}, capture_output=True)
            self.assertEqual((found.returncode, found.stdout.decode().strip()), (0, "origin/main"), found.stderr.decode())
            for made in ("bootstrap", "home", "cargo", "target", "cache", "tmp", "bin", "source"):
                self.assertFalse(os.stat(workspace / made).st_mode & 0o2000, made + " kept the volume's setgid bit")
            scratch = workspace / "tmp" / "a-test-scratch"
            scratch.mkdir()
            os.chmod(scratch, 0o755)
            self.assertEqual(os.stat(scratch).st_mode & 0o7777, 0o755, "a test's 0755 directory reads 0755")

    def test_admission_defaults_do_not_invalidate_the_declared_boundary(self):
        job = self.spec()
        pod = job["spec"]["template"]["spec"]
        pod["containers"][1]["readinessProbe"].update(timeoutSeconds=1, successThreshold=1, failureThreshold=3)
        for container in pod["containers"] + pod["initContainers"]:
            container.update(terminationMessagePath="/dev/termination-log", terminationMessagePolicy="File")
        self.assertIsNone(self.mod.worker_error(job))

    def test_every_bootstrap_the_factory_launches_is_an_admitted_one(self):
        # Backlog 07df587a (2026-10-22): a bootstrap edit that is not
        # admitted here leaves every live worker unrecognised the moment
        # it lands. Keep the earlier digest beside the new one.
        launched = self.spec()["spec"]["template"]["spec"]["initContainers"][0]["command"][2]
        digest = hashlib.sha256(launched.encode()).hexdigest()
        self.assertIn(digest, self.mod.ADMITTED_WORKER_BOOTSTRAPS,
                      "the worker bootstrap changed: add %s to ADMITTED_WORKER_BOOTSTRAPS in "
                      "infra/dev/dev-build.py and keep the earlier digest there" % digest)

    def test_a_worker_launched_under_an_earlier_admitted_bootstrap_is_still_recognised(self):
        # Backlog 07df587a (2026-10-22): a controller published with a
        # changed bootstrap refused exec into every worker the previous
        # one launched ("worker Job differs from recorded controller
        # intent"), and a-deleted-manifest-leaves-no-object called them
        # undeclared, until each was stopped. The earlier text is made
        # here by appending a line: the shape is what the history holds.
        job = self.spec()
        init = job["spec"]["template"]["spec"]["initContainers"][0]
        earlier = init["command"][2] + "# an earlier controller's bootstrap\n"
        init["command"][2] = earlier
        admitted = frozenset(getattr(self.mod, "ADMITTED_WORKER_BOOTSTRAPS", frozenset()))
        before = getattr(self.mod, "ADMITTED_WORKER_BOOTSTRAPS", None)
        self.mod.ADMITTED_WORKER_BOOTSTRAPS = admitted | {hashlib.sha256(earlier.encode()).hexdigest()}
        try:
            self.assertIsNone(self.mod.worker_error(job))
            self.assertTrue(self.mod.native_job_matches(job))
            self.assertTrue(self.mod.native_job_matches(job, copy.deepcopy(job)))
            # The bootstrap is the only part that may be an earlier one:
            # an admitted bootstrap does not admit a changed mount.
            widened = copy.deepcopy(job)
            widened["spec"]["template"]["spec"]["initContainers"][0]["volumeMounts"].append({"name": "forge-read", "mountPath": "/elsewhere"})
            self.assertFalse(self.mod.native_job_matches(widened))
            # Nor does a recorded intent under one admitted bootstrap
            # vouch for a live Job under another.
            self.assertFalse(self.mod.native_job_matches(self.spec(), copy.deepcopy(job)))
        finally:
            if before is None:
                del self.mod.ADMITTED_WORKER_BOOTSTRAPS
            else:
                self.mod.ADMITTED_WORKER_BOOTSTRAPS = before
        # A bootstrap nobody admitted is still refused.
        self.assertIsNotNone(self.mod.worker_error(job))
        self.assertFalse(self.mod.native_job_matches(job))

    def test_native_api_omits_false_host_namespaces_without_adding_authority(self):
        # Shape observed on the first native worker, 2026-10-14: these
        # non-pointer PodSpec booleans disappear on API serialization.
        job = self.spec()
        pod = job["spec"]["template"]["spec"]
        for field in ("hostNetwork", "hostPID", "hostIPC"):
            pod.pop(field)
        self.assertIsNone(self.mod.worker_error(job))
        for field in ("hostNetwork", "hostPID", "hostIPC"):
            for value in (True, "false", None, 0):
                changed = copy.deepcopy(job)
                changed["spec"]["template"]["spec"][field] = value
                self.assertIsNotNone(self.mod.worker_error(changed), (field, value))
        for field in ("automountServiceAccountToken", "shareProcessNamespace", "enableServiceLinks"):
            changed = copy.deepcopy(job)
            changed["spec"]["template"]["spec"].pop(field)
            self.assertIsNotNone(self.mod.worker_error(changed), field)

    def test_native_namespace_defaults_match_intent_but_changed_authority_does_not(self):
        expected = self.spec()["spec"]["template"]["spec"]
        actual = copy.deepcopy(expected)
        for field in ("hostNetwork", "hostPID", "hostIPC"):
            actual.pop(field)
        self.assertTrue(self.mod.declared_matches(self.mod.observed_pod_spec(actual), expected))
        for field in ("hostNetwork", "hostPID", "hostIPC", "automountServiceAccountToken"):
            for value in (True, "false", None, 0):
                changed = {**actual, field: value}
                self.assertFalse(self.mod.declared_matches(self.mod.observed_pod_spec(changed), expected), (field, value))
        changed = copy.deepcopy(actual)
        changed["containers"][0]["volumeMounts"].append({"name": "forge-read", "mountPath": "/unexpected"})
        self.assertFalse(self.mod.declared_matches(self.mod.observed_pod_spec(changed), expected))

    def recovery_fixture(self, root, mutate=lambda _: None, acknowledged=True, mutate_ack=lambda _: None):
        name = "dev-build-1234567890abcdef"
        intent = self.spec()
        self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": "a" * 40, "job": intent})
        self.mod.save_record(root / (name + ".volume.json"), {"name": name, "uid": "volume-uid"})
        job = {**copy.deepcopy(intent), "metadata": {**copy.deepcopy(intent["metadata"]), "uid": "job-uid"}}
        pod = {"metadata": {"name": name + "-pod", "namespace": "boss-dev", "uid": "pod-uid", "labels": copy.deepcopy(intent["spec"]["template"]["metadata"]["labels"]), "ownerReferences": [{"kind": "Job", "uid": "job-uid"}]},
               "spec": copy.deepcopy(intent["spec"]["template"]["spec"]),
               "status": {"phase": "Running", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"terminated": {"exitCode": 0}}}], "containerStatuses": [{"name": "worker", "ready": True, "restartCount": 0, "containerID": "container-id", "state": {"running": {}}}, {"name": "postgres", "ready": True, "restartCount": 0, "state": {"running": {}}}]}}
        volume = {"kind": "PersistentVolumeClaim", "metadata": {"name": name, "namespace": "boss-dev", "uid": "volume-uid", "ownerReferences": [{"apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": "job-uid"}]}}
        if acknowledged:
            original = {**copy.deepcopy(job), "kind": "Job"}
            mutate_ack(original)
            self.mod.save_record(root / (name + ".launch-original.json"), {"argv": ["create", "-f", "-", "-o", "json"], "completion": "observed", "exit": 0, "stdout_base64": base64.b64encode(json.dumps(original).encode()).decode()})
        for spec in (job["spec"]["template"]["spec"], pod["spec"]):
            for field in ("hostNetwork", "hostPID", "hostIPC"):
                spec.pop(field)
        originals = {"job": job, "pod": pod, "volume": volume}
        mutate(originals)
        calls = []
        def transport(argv, stdin=None):
            calls.append(argv)
            data = {"job": originals["job"], "pods": {"items": [originals["pod"]]}, "pvc": originals["volume"]}[argv[1]]
            return subprocess.CompletedProcess(argv, 0, json.dumps(data).encode(), b"")
        return name, originals, calls, transport

    def test_recovery_uses_original_creation_uid_without_creating_or_executing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, _, calls, transport = self.recovery_fixture(root)
            session = self.mod.recover_start(name, root, transport=transport)
            self.assertEqual(session.job_uid, "job-uid")
            self.assertEqual(session.volume_uid, "volume-uid")
            self.assertEqual(session.source_head, "a" * 40)
            self.assertEqual(self.mod.load_record(root / (name + ".json"))["pod_uid"], "pod-uid")
            self.assertEqual([call[:2] for call in calls], [["get", "job"], ["get", "pvc"], ["get", "pods"]])
            self.assertEqual(len(list(root.glob(name + ".recovery-*.json"))), 3)
            with self.assertRaises(ValueError):
                self.mod.recover_start(name, root, transport=transport)
            self.assertEqual(len(calls), 3)

    def test_recovery_and_observation_conserve_fence_labels_and_job_bounds(self):
        cases = [
            ("positive", lambda _: None),
            ("lost fence label", lambda o: o["pod"]["metadata"]["labels"].pop("boss.dev/purpose")),
            ("changed fence label", lambda o: o["pod"]["metadata"]["labels"].update({"boss.dev/purpose": "other"})),
            ("changed session label", lambda o: o["pod"]["metadata"]["labels"].update({"boss.dev/session": "other"})),
            ("changed deadline", lambda o: o["job"]["spec"].update(activeDeadlineSeconds=999999)),
            ("changed retries", lambda o: o["job"]["spec"].update(backoffLimit=6)),
            ("missing deadline", lambda o: o["job"]["spec"].pop("activeDeadlineSeconds")),
            ("changed parallelism", lambda o: o["job"]["spec"].update(parallelism=2)),
        ]
        failures = []
        for label, mutate in cases:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, objects, calls, transport = self.recovery_fixture(root, mutate)
                refused = False
                try:
                    self.mod.recover_start(name, root, transport=transport)
                except (ValueError, RuntimeError):
                    refused = True
                if refused != (label != "positive"):
                    failures.append((label, "recovery", refused))
                if label != "positive":
                    self.assertEqual((root / (name + ".json")).exists(), not refused)
                def observe_transport(argv, stdin=None):
                    calls.append(argv)
                    value = objects[{"job": "job", "pod": "pod", "pvc": "volume"}[argv[1]]]
                    return subprocess.CompletedProcess(argv, 0, json.dumps(value).encode(), b"")
                session = self.mod.Session(name, "job-uid", name + "-pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
                refused = False
                try:
                    self.mod.observe(session, root=root, transport=observe_transport)
                except (ValueError, RuntimeError):
                    refused = True
                if refused != (label != "positive"):
                    failures.append((label, "observation", refused))
                self.assertTrue(all(call[0] == "get" for call in calls))
        self.assertFalse(failures, failures)

    def test_recovery_refuses_unknown_creation_replacement_and_changed_authority(self):
        cases = [
            (False, lambda _: None),
            (True, lambda r: r["job"]["metadata"].update(uid="replacement")),
            (True, lambda r: r["volume"]["metadata"].update(uid="replacement")),
            (True, lambda r: r["volume"]["metadata"].update(ownerReferences=[])),
            (True, lambda r: r["volume"]["metadata"].update(namespace="other")),
            (True, lambda r: r["pod"]["metadata"].update(namespace="other")),
            (True, lambda r: r["pod"]["spec"].update(hostNetwork=True)),
            (True, lambda r: r["pod"]["spec"].pop("automountServiceAccountToken")),
            (True, lambda r: r["pod"]["status"]["containerStatuses"][0].update(restartCount=1)),
            (True, lambda r: r["pod"]["status"]["initContainerStatuses"][0].update(state={"running": {}})),
        ]
        for acknowledged, mutate in cases:
            with self.subTest(acknowledged=acknowledged, mutate=mutate), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, calls, transport = self.recovery_fixture(root, mutate, acknowledged)
                with self.assertRaises((ValueError, RuntimeError)):
                    self.mod.recover_start(name, root, transport=transport)
                self.assertFalse((root / (name + ".json")).exists())
                self.assertTrue(all(call[0] == "get" for call in calls))
                if not acknowledged:
                    self.assertEqual(calls, [])

    def test_recovery_requires_original_creation_to_match_the_retained_intent(self):
        for mutate in (
            lambda j: j["spec"]["template"]["spec"]["initContainers"][0]["command"].__setitem__(4, "b" * 40),
            lambda j: j["spec"]["template"]["spec"].update(automountServiceAccountToken=True),
        ):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, calls, transport = self.recovery_fixture(root, mutate_ack=mutate)
                with self.assertRaises(ValueError):
                    self.mod.recover_start(name, root, transport=transport)
                self.assertEqual(calls, [])
                self.assertFalse((root / (name + ".json")).exists())

    def test_recovery_refuses_native_retirement_without_publishing_a_session(self):
        for kind in ("job", "pod", "volume"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                def mutate(objects):
                    objects[kind]["metadata"]["deletionTimestamp"] = "2026-10-14T11:00:00Z"
                name, _, calls, transport = self.recovery_fixture(root, mutate)
                with self.assertRaises((ValueError, RuntimeError)):
                    self.mod.recover_start(name, root, transport=transport)
                self.assertFalse((root / (name + ".json")).exists())
                self.assertTrue(all(call[0] == "get" for call in calls))

    def test_pre_execution_observation_refuses_retiring_resources(self):
        for kind in (None, "job", "pod", "volume"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, objects, calls, _ = self.recovery_fixture(root)
                if kind:
                    objects[kind]["metadata"]["deletionTimestamp"] = "2026-10-14T11:00:00Z"
                def transport(argv, stdin=None):
                    calls.append(argv)
                    value = objects[{"job": "job", "pod": "pod", "pvc": "volume"}[argv[1]]]
                    return subprocess.CompletedProcess(argv, 0, json.dumps(value).encode(), b"")
                session = self.mod.Session(name, "job-uid", name + "-pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
                if kind:
                    with self.assertRaises(RuntimeError):
                        self.mod.observe(session, root=root, transport=transport)
                else:
                    self.assertEqual(self.mod.observe(session, root=root, transport=transport), objects["pod"])
                self.assertTrue(all(call[0] == "get" for call in calls))

    def test_recovery_and_session_refuse_extra_container_and_pod_security_authority(self):
        for section, index in (("containers", 0), ("containers", 1), ("initContainers", 0), (None, None)):
            with self.subTest(section=section, index=index), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                def mutate(objects):
                    spec = objects["pod"]["spec"]
                    context = spec["securityContext"] if section is None else spec[section][index]["securityContext"]
                    context["runAsUser" if section is None else "privileged"] = 0 if section is None else True
                name, objects, calls, transport = self.recovery_fixture(root, mutate)
                with self.assertRaises((ValueError, RuntimeError)):
                    self.mod.recover_start(name, root, transport=transport)
                session = self.mod.Session(name, "job-uid", name + "-pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
                self.assertIsNotNone(self.mod.session_error(session, objects["pod"]))
                self.assertFalse((root / (name + ".json")).exists())
                self.assertTrue(all(call[0] == "get" for call in calls))

    def test_recovery_refuses_changed_job_template_authority_before_admission(self):
        for section, index in (("containers", 1), ("initContainers", 0)):
            with self.subTest(section=section), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                def mutate(objects):
                    objects["job"]["spec"]["template"]["spec"][section][index]["securityContext"]["privileged"] = True
                name, _, calls, transport = self.recovery_fixture(root, mutate)
                with self.assertRaises(RuntimeError):
                    self.mod.recover_start(name, root, transport=transport)
                self.assertFalse((root / (name + ".json")).exists())
                self.assertTrue(all(call[0] == "get" for call in calls))

    def test_recovery_refuses_malformed_ambiguous_or_retired_creation_before_reads(self):
        for mode in ("malformed", "duplicate", "retired"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, calls, transport = self.recovery_fixture(root)
                path = root / (name + ".launch-original.json")
                receipt = self.mod.load_record(path)
                if mode == "malformed":
                    path.unlink()
                    receipt["stdout_base64"] = base64.b64encode(b"[]").decode()
                    self.mod.save_record(path, receipt)
                elif mode == "duplicate":
                    self.mod.save_record(root / (name + ".launch-duplicate.json"), receipt)
                else:
                    self.mod.save_record(root / (name + ".retire-intent.json"), {"job": name})
                with self.assertRaises(ValueError):
                    self.mod.recover_start(name, root, transport=transport)
                self.assertEqual(calls, [])
                self.assertFalse((root / (name + ".json")).exists())

    def test_wrong_uid_restart_and_mutated_mount_refuse_before_exec(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        job = self.spec()
        pod = {"metadata": {"name": "pod-name", "uid": "pod-uid", "labels": copy.deepcopy(job["spec"]["template"]["metadata"]["labels"]), "ownerReferences": [{"uid": "job-uid", "kind": "Job"}]},
               "spec": copy.deepcopy(job["spec"]["template"]["spec"]),
               "status": {"phase": "Running", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"terminated": {"exitCode": 0}}}], "containerStatuses": [{"name": "worker", "ready": True, "restartCount": 0, "containerID": "container-id", "state": {"running": {}}}, {"name": "postgres", "ready": True, "restartCount": 0, "containerID": "postgres-id", "state": {"running": {}}}]}}
        self.assertIsNone(self.mod.session_error(session, pod))
        for mutate in (
            lambda p: p["metadata"].update(uid="replacement"),
            lambda p: p["metadata"].update(deletionTimestamp="2026-10-14T11:00:00Z"),
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

    def worker_input_with_idle_open_stdin(self, verb, forward):
        # Backlog c0a24b52 (2026-10-17 isolation trial): an agent shell
        # holds stdin open and writes nothing, and `exec` waited for an
        # EOF that never came until the caller's own timeout. The write
        # end stays open in the child for the whole call, as there.
        code = (
            "import importlib.util, os, sys\n"
            "spec = importlib.util.spec_from_file_location('dev_build', sys.argv[1])\n"
            "mod = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(mod)\n"
            "read, write = os.pipe()\n"
            "data = mod.worker_input(sys.argv[2], sys.argv[3] == 'forward', os.fdopen(read, 'rb'))\n"
            "print(repr(data))\n"
        )
        return subprocess.run(
            [sys.executable, "-I", "-c", code, self.mod.__file__, verb, "forward" if forward else "closed"],
            stdin=subprocess.DEVNULL, capture_output=True, timeout=5, check=False)

    def test_exec_with_an_open_idle_stdin_returns_without_reading_it(self):
        for verb in ("exec", "exec-auto", "get"):
            try:
                result = self.worker_input_with_idle_open_stdin(verb, False)
            except subprocess.TimeoutExpired:
                self.fail(verb + " waited on an open, idle caller stdin")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, b"b''\n", verb)

    def test_caller_stdin_is_forwarded_only_when_asked_or_for_put(self):
        stream = lambda: __import__("io").BytesIO(b"plain input")
        self.assertEqual(self.mod.worker_input("exec", True, stream()), b"plain input")
        self.assertEqual(self.mod.worker_input("exec-auto", True, stream()), b"plain input")
        self.assertEqual(self.mod.worker_input("put", False, stream()), b"plain input")
        with open(os.devnull, "rb") as nothing:
            self.assertEqual(self.mod.worker_input("exec", False, nothing), b"")
        self.assertEqual(self.mod.worker_input("get", True, stream()), b"")
        parser = self.mod.arguments()
        self.assertTrue(parser.parse_args(["exec", "--stdin", "dev-build-1234567890abcdef", "--", "cat"]).stdin)
        self.assertFalse(parser.parse_args(["exec", "dev-build-1234567890abcdef", "--", "cat"]).stdin)
        self.assertTrue(parser.parse_args(["exec-auto", "--stdin", "--", "cat"]).stdin)
        self.assertFalse(parser.parse_args(["exec-auto", "--", "cat"]).stdin)

    def test_exec_without_stdin_refuses_input_it_would_drop(self):
        # Backlog 056fc8b6 (reviewer run 9bead677 of car 5271ed88): after
        # c0a24b52 closed the worker's stdin, `exec <s> -- bash` with a
        # script on stdin ran NOTHING and exited 0 (python3 -, sh -s and
        # tee the same; tee left an empty file). Input the caller sent
        # and the worker would never see is refused, naming --stdin.
        with tempfile.TemporaryDirectory() as temporary:
            script = Path(temporary) / "script.sh"
            script.write_bytes(b"echo ran\n")
            empty = Path(temporary) / "empty"
            empty.write_bytes(b"")
            for verb in ("exec", "exec-auto"):
                with open(script, "rb") as stream, self.assertRaisesRegex(ValueError, "--stdin"):
                    self.mod.worker_input(verb, False, stream)
                read, write = os.pipe()
                os.write(write, b"echo ran\n")
                with os.fdopen(read, "rb") as stream, os.fdopen(write, "wb"):
                    with self.assertRaisesRegex(ValueError, "--stdin.*nothing executed"):
                        self.mod.worker_input(verb, False, stream)
                read, write = os.pipe()
                os.write(write, b"echo ran\n")
                os.close(write)
                with os.fdopen(read, "rb") as stream, self.assertRaisesRegex(ValueError, "--stdin"):
                    self.mod.worker_input(verb, False, stream)
                # Nothing sent: an empty file, /dev/null, a pipe whose
                # writer closed without writing. Each still runs with a
                # closed stdin, as c0a24b52 made it.
                with open(empty, "rb") as stream:
                    self.assertEqual(self.mod.worker_input(verb, False, stream), b"")
                with open(os.devnull, "rb") as stream:
                    self.assertEqual(self.mod.worker_input(verb, False, stream), b"")
                read, write = os.pipe()
                os.close(write)
                with os.fdopen(read, "rb") as stream:
                    self.assertEqual(self.mod.worker_input(verb, False, stream), b"")
                # Asked for, the same input is forwarded whole.
                with open(script, "rb") as stream:
                    self.assertEqual(self.mod.worker_input(verb, True, stream), b"echo ran\n")
        # A stream that cannot be judged without reading it is refused,
        # never read and never dropped.
        with self.assertRaisesRegex(ValueError, "--stdin"):
            self.mod.worker_input("exec", False, __import__("io").BytesIO(b"plain input"))

    def test_a_socket_stdin_is_judged_by_the_bytes_it_holds_not_by_eof(self):
        # Review 42ea788b of car f60b19e3: libuv (Node) gives a child's
        # stdio as a socketpair, and a socket whose peer shut down or
        # closed reports POLLIN with zero bytes to read, so a caller that
        # sent NOTHING was refused 75 as "stdin carries input". Only the
        # bytes actually queued count, as for a pipe.
        import socket
        for verb in ("exec", "exec-auto"):
            for end in ("shutdown", "close", "idle"):
                ours, peer = socket.socketpair()
                with ours, peer, ours.makefile("rb") as stream:
                    if end == "shutdown":
                        peer.shutdown(socket.SHUT_WR)
                    elif end == "close":
                        peer.close()
                    self.assertEqual(self.mod.worker_input(verb, False, stream), b"", (verb, end))
            for end in ("open", "close"):
                ours, peer = socket.socketpair()
                with ours, peer, ours.makefile("rb") as stream:
                    peer.sendall(b"echo ran\n")
                    if end == "close":
                        peer.close()
                    with self.assertRaisesRegex(ValueError, "--stdin.*nothing executed"):
                        self.mod.worker_input(verb, False, stream)

    def test_a_misplaced_stdin_is_refused_by_the_parser(self):
        # After the session, --stdin fell into the worker argv
        # ('--stdin', '--', 'cat') and failed naming a missing executable.
        session = "dev-build-1234567890abcdef"
        for argv in (["exec", session, "--stdin", "--", "cat"], ["exec", session, "--stdin", "cat"]):
            stderr = __import__("io").StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as refused:
                self.mod.parse_arguments(argv)
            self.assertEqual(refused.exception.code, 2, argv)
            self.assertIn("--stdin goes before the session", stderr.getvalue())
        self.assertTrue(self.mod.parse_arguments(["exec", "--stdin", session, "--", "cat"]).stdin)
        # After --, every word is the worker's, exactly as written.
        self.assertEqual(self.mod.parse_arguments(["exec", session, "--", "printf", "--stdin"]).argv[-2:], ["printf", "--stdin"])

    def test_main_routes_the_callers_stdin_through_worker_input(self):
        # Only the probe's grep held main() to worker_input until 056fc8b6;
        # this drives main() itself with a fake transport.
        import io
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        saved = (self.mod.kube, self.mod.observe, sys.stdin, sys.stdout, sys.stderr)
        sent = []
        def transport(argv, stdin=None, timeout=None):
            sent.append((argv, stdin))
            return subprocess.CompletedProcess(argv, 0, b"worker out", b"")
        def caller(data):
            read, write = os.pipe()
            if data is not None:
                os.write(write, data)
            os.close(write)
            return io.TextIOWrapper(os.fdopen(read, "rb"))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.mod.save_record(root / (session.job + ".json"), dataclasses.asdict(session))
            self.mod.kube, self.mod.observe = transport, lambda session: None
            try:
                sys.stdout, sys.stderr = io.TextIOWrapper(io.BytesIO()), io.TextIOWrapper(io.BytesIO())
                sys.stdin = caller(b"echo ran\n")
                self.assertEqual(self.mod.main(["exec", "--stdin", session.job, "--", "bash"], root=root), 0)
                self.assertEqual(sent[-1], (["exec", "-i", "pod-name", "-c", "worker", "--", "bash"], b"echo ran\n"))
                sys.stdin = caller(None)
                self.assertEqual(self.mod.main(["exec", session.job, "--", "true"], root=root), 0)
                self.assertEqual(sent[-1][1], b"")
                sys.stdin = caller(b"echo ran\n")
                calls = len(sent)
                with self.assertRaisesRegex(ValueError, "--stdin"):
                    self.mod.main(["exec", session.job, "--", "bash"], root=root)
                self.assertEqual(len(sent), calls, "refused input never reaches the transport")
                sys.stdin = caller(b"file bytes")
                self.assertEqual(self.mod.main(["put", session.job, "infra/dev/x.py"], root=root), 0)
                self.assertEqual(sent[-1][1], b"file bytes")
            finally:
                self.mod.kube, self.mod.observe, sys.stdin, sys.stdout, sys.stderr = saved

    def test_auto_execution_maps_only_the_bound_source_and_keeps_argv_data(self):
        binding = {"local_root": "/scratch/wt/agent-car"}
        argv = self.mod.bound_argv(binding, "/scratch/wt/agent-car/apps/web", ["/scratch/wt/agent-car/target/debug/boss", "literal;touch /control"])
        self.assertEqual(argv, ["/usr/bin/env", "-C", "/workspace/source/apps/web", "/workspace/source/target/debug/boss", "literal;touch /control"])
        with self.assertRaises(ValueError):
            self.mod.bound_argv(binding, "/work/home", ["bash"])
        with self.assertRaises(ValueError):
            self.mod.bound_argv(binding, "/scratch/wt/agent-car/../other", ["bash"])

    def test_a_cwd_in_a_nested_working_tree_refuses_naming_its_own_binding(self):
        # Backlog f7b43945 (2026-10-21, reviewer runs b52acf3f and
        # 0e001103): with only the shared checkout bound, a worktree under
        # its .claude/worktrees chose the checkout's binding as its nearest
        # ancestor and was mapped to /workspace/source/.claude/worktrees/<n>,
        # which the worker never holds (its source is the checkout's own
        # commit), so env died with exit 125 before cargo started.
        with tempfile.TemporaryDirectory() as tmp:
            checkout = Path(tmp) / "boss"
            review = checkout / ".claude" / "worktrees" / "review-x"
            (review / "crates").mkdir(parents=True)
            (review / ".git").write_text("gitdir: %s\n" % (checkout / ".git" / "worktrees" / "review-x"))
            (checkout / "apps" / "web").mkdir(parents=True)
            binding = {"session": "dev-build-" + "a" * 16, "local_root": str(checkout)}
            # A directory of the bound tree itself still maps.
            self.assertEqual(self.mod.bound_argv(binding, str(checkout / "apps" / "web"), ["bash"]),
                             ["/usr/bin/env", "-C", "/workspace/source/apps/web", "bash"])
            for cwd in (review, review / "crates"):
                with self.subTest(cwd=cwd):
                    with self.assertRaisesRegex(ValueError, "the worker does not hold it") as refused:
                        self.mod.bound_argv(binding, str(cwd), ["bash"])
                    message = str(refused.exception)
                    self.assertIn("dev-build start", message)
                    self.assertIn("--local-root %s" % review, message)
                    self.assertIn(binding["session"], message)
                    self.assertIn("nothing executed", message)
            # A program inside the nested tree, named from the bound tree,
            # would map to the same absent path.
            with self.assertRaisesRegex(ValueError, "--local-root %s" % review):
                self.mod.bound_argv(binding, str(checkout), [str(review / "infra" / "dev" / "wt-cargo")])
            # A nested clone (a .git directory, not a file) is another tree too.
            clone = checkout / "vendor" / "clone"
            (clone / ".git").mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, "--local-root %s" % clone):
                self.mod.bound_argv(binding, str(clone), ["bash"])

    def test_the_nearest_live_binding_owns_a_nested_worktree(self):
        # Backlog 7dd185c6: the shared checkout /work/boss carries a binding
        # for the doors run from it, and every Claude Code builder worktree
        # nests under /work/boss/.claude/worktrees/<id>. The first isolated
        # build after re-activation on 2026-10-19 was refused because both
        # matched and the door demanded exactly one.
        checkout = {"session": "dev-build-" + "a" * 16, "local_root": "/work/boss"}
        nested = {"session": "dev-build-" + "b" * 16, "local_root": "/work/boss/.claude/worktrees/agent-x"}
        other = {"session": "dev-build-" + "c" * 16, "local_root": "/work/wt/agent-y"}
        never = lambda b: False
        pick = self.mod.nearest_binding
        self.assertIs(pick([checkout, nested, other], "/work/boss/.claude/worktrees/agent-x/crates", never), nested)
        self.assertIs(pick([checkout, nested, other], "/work/boss/infra", never), checkout)
        self.assertIs(pick([checkout, nested, other], "/work/wt/agent-y", never), other)
        # Two live bindings on the SAME root are still ambiguous.
        twin = dict(checkout, session="dev-build-" + "d" * 16)
        with self.assertRaisesRegex(ValueError, "no unique controller-owned worker binding"):
            pick([checkout, twin], "/work/boss/infra", never)
        # No binding owns the source at all.
        with self.assertRaisesRegex(ValueError, "no unique controller-owned worker binding"):
            pick([checkout, nested], "/scratch/elsewhere", never)
        # A retired checkout binding no longer counts against the nested one...
        retired_checkout = lambda b: b is checkout
        self.assertIs(pick([checkout, nested], "/work/boss/.claude/worktrees/agent-x", retired_checkout), nested)
        self.assertIs(pick([checkout, nested], "/work/boss", lambda b: b is nested), checkout)
        # ...but a retired NESTED binding never falls through to the parent
        # checkout's worker, whose source does not hold the worktree.
        with self.assertRaisesRegex(ValueError, "no unique controller-owned worker binding"):
            pick([checkout, nested], "/work/boss/.claude/worktrees/agent-x", lambda b: b is nested)

    def test_checkout_expiry_renews_once_and_resumes_after_interruption(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = self.mod.Session("dev-build-" + "a" * 16, "old-uid", "old-pod", "pod-uid", "container", "a" * 40)
            binding = {"session": old.job, "local_root": "/work/boss", "source_head": old.source_head}
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"), binding)
            calls = []
            def read(argv):
                calls.append(argv)
                if argv[:2] == ["get", "deployment"]:
                    return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
                return {"kind": "JobList", "metadata": {}, "items": []}
            def retire(session):
                calls.append(("retire", session.job, session.job_uid))
                self.mod.save_record(root / (session.job + ".retire-intent.json"), {"session": self.mod.asdict(session)})
                self.mod.save_record(root / (session.job + ".retired.json"), {"session": self.mod.asdict(session)})
            attempts = []
            def allocate(name, head, local):
                attempts.append(name)
                if len(attempts) == 1:
                    raise RuntimeError("storage budget temporarily refused")
                new = self.mod.Session(name, "new-uid", "new-pod", "new-pod-uid", "container", head)
                self.mod.save_record(root / (name + ".json"), self.mod.asdict(new))
                self.mod.save_record(root / (name + ".binding.json"), {"session": name, "local_root": str(local), "source_head": head})
            def invoke():
                return self.mod.checkout_worker("/work/boss", root, read, retire, allocate,
                    lambda *args: self.fail("unacknowledged allocation must not recover"), lambda path: "b" * 40)
            with self.assertRaisesRegex(RuntimeError, "renew-checkout.*storage budget"):
                invoke()
            renewed = invoke()
            self.assertEqual(attempts[0], attempts[1], "retry retains the admitted renewal identity")
            self.assertEqual(renewed["source_head"], "b" * 40)
            self.assertEqual(sum(isinstance(c, tuple) and c[0] == "retire" for c in calls), 1)
            self.assertEqual(renewed["local_root"], "/work/boss")

    def test_checkout_retries_a_retired_replacement_with_a_fresh_identity(self):
        # Review 8ecbcd44: real launch saves its intent BEFORE scheduling,
        # and give_up records retirement before raising LaunchRefused.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = self.mod.Session("dev-build-" + "a" * 16, "old-uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"),
                                 {"session": old.job, "local_root": "/work/boss", "source_head": old.source_head})
            attempts = []
            def read(argv):
                if argv[:2] == ["get", "deployment"]:
                    return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
                return {"kind": "JobList", "items": []}
            def retire(session):
                for suffix in (".retire-intent.json", ".retired.json"):
                    path = root / (session.job + suffix)
                    if not path.exists():
                        self.mod.save_record(path, {"session": self.mod.asdict(session)})
            def allocate(name, head, local):
                attempts.append(name)
                new = self.mod.Session(name, "uid-" + name, "pod", "pod-uid", "container", head)
                if len(attempts) <= 2:
                    self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": head})
                    retire(new)
                    raise self.mod.LaunchRefused("replacement scheduling refused after intent was saved")
                self.mod.save_record(root / (name + ".json"), self.mod.asdict(new))
                self.mod.save_record(root / (name + ".binding.json"),
                                     {"session": name, "local_root": str(local), "source_head": head})
            def invoke():
                return self.mod.checkout_worker("/work/boss", root, read, retire, allocate,
                    lambda name, local: self.mod.recover_start(name, root, local), lambda local: "b" * 40)
            for _ in range(2):
                with self.assertRaisesRegex(RuntimeError, "replacement scheduling refused"):
                    invoke()
            renewed = invoke()
            self.assertEqual(len(set(attempts)), 3, "each acknowledged retired launch needs a fresh identity")
            self.assertEqual(renewed["session"], attempts[-1])
            abandoned = list(root.glob("*.abandoned.json"))
            self.assertEqual(len(abandoned), 2)
            self.assertTrue(all(self.mod.load_record(p).get("cause") for p in abandoned))

    def test_a_live_manual_checkout_binding_supersedes_an_unacknowledged_replacement(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = self.mod.Session("dev-build-" + "a" * 16, "old-uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"),
                                 {"session": old.job, "local_root": "/work/boss", "source_head": old.source_head})
            declared = self.spec()
            manual_name = declared["metadata"]["name"]
            manual = self.mod.Session(manual_name, "manual-uid", "pod", "pod-uid", "container", "a" * 40)
            native = copy.deepcopy(declared)
            native["metadata"]["uid"] = manual.job_uid
            installed = []
            def read(argv):
                if argv[:2] == ["get", "deployment"]:
                    return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
                return {"kind": "JobList", "items": [native] if installed else []}
            def retire(session):
                self.mod.save_record(root / (session.job + ".retire-intent.json"), {"session": self.mod.asdict(session)})
                self.mod.save_record(root / (session.job + ".retired.json"), {"session": self.mod.asdict(session)})
            def allocate(name, head, local):
                self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": head})
                raise RuntimeError("Job create outcome unknown; no blind retry")
            def invoke():
                return self.mod.checkout_worker("/work/boss", root, read, retire, allocate,
                    lambda name, local: self.mod.recover_start(name, root, local), lambda local: "a" * 40)
            with self.assertRaisesRegex(RuntimeError, "Job create outcome unknown"):
                invoke()
            # An unknown create alone still grants no second allocation.
            with self.assertRaises(RuntimeError):
                invoke()
            installed.append(manual_name)
            binding = {"session": manual_name, "source_head": manual.source_head, "local_root": "/work/boss"}
            self.mod.save_record(root / (manual_name + ".json"), self.mod.asdict(manual))
            self.mod.save_record(root / (manual_name + ".binding.json"), binding)
            self.mod.save_record(root / (manual_name + ".intent.json"), {"name": manual_name, "head": manual.source_head, "job": declared})
            self.assertEqual(invoke(), binding)
            completed = list(root.glob("checkout-renewal-*.complete.json"))
            self.assertEqual(len(completed), 1)
            self.assertTrue(self.mod.load_record(completed[0])["superseded"])

    def test_checkout_unknown_create_releases_only_through_the_bounded_stop_door(self):
        for release_ok in (False, True):
            with self.subTest(release_ok=release_ok), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                old = self.mod.Session("dev-build-" + "a" * 16, "uid", "pod", "pod-uid", "container", "a" * 40)
                self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
                self.mod.save_record(root / (old.job + ".binding.json"),
                    {"session": old.job, "local_root": "/work/boss", "source_head": old.source_head})
                attempts, releases = [], []
                def read(argv):
                    if argv[:2] == ["get", "deployment"]:
                        return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
                    return {"kind": "JobList", "items": []}
                def retire(session):
                    for suffix in (".retire-intent.json", ".retired.json"):
                        self.mod.save_record(root / (session.job + suffix), {"session": self.mod.asdict(session)})
                def allocate(name, head, local):
                    attempts.append(name)
                    if len(attempts) == 1:
                        self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": head, "job": self.spec()})
                        raise RuntimeError("Job create outcome unknown")
                    new = self.mod.Session(name, "new-uid", "pod", "pod-uid", "container", head)
                    self.mod.save_record(root / (name + ".json"), self.mod.asdict(new))
                    self.mod.save_record(root / (name + ".binding.json"),
                        {"session": name, "local_root": str(local), "source_head": head})
                def release(name):
                    releases.append(name)
                    if not release_ok:
                        raise RuntimeError("bounded stop could not prove Job absence")
                    return {"state": "volume released to garbage collection", "name": name}
                def invoke():
                    return self.mod.checkout_worker("/work/boss", root, read, retire, allocate,
                        lambda name, local: (_ for _ in ()).throw(self.mod.NoJobAcknowledged("original Job creation is unknown")),
                        lambda local: "a" * 40, release_failed=release)
                with self.assertRaisesRegex(RuntimeError, "Job create outcome unknown"):
                    invoke()
                if release_ok:
                    self.assertNotEqual(invoke()["session"], attempts[0])
                    self.assertEqual(len(attempts), 2)
                else:
                    with self.assertRaisesRegex(RuntimeError, "could not prove Job absence"):
                        invoke()
                    self.assertEqual(len(attempts), 1, "an uncertain Job never permits blind allocation")
                self.assertEqual(releases, attempts[:1])

    def test_checkout_renewal_never_claims_a_builder_or_foreign_job(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            forbidden = lambda *args: self.fail("no native effect is authorized")
            self.assertIsNone(self.mod.checkout_worker("/work/wt/agent-a", root, forbidden, forbidden, forbidden, forbidden, forbidden))
            old = self.mod.Session("dev-build-" + "a" * 16, "old-uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"), {"session": old.job, "local_root": "/work/boss", "source_head": old.source_head})
            def foreign(argv):
                return {"kind": "JobList", "items": [{"metadata": {"name": old.job, "namespace": "boss-dev", "uid": "foreign"}}]}
            with self.assertRaisesRegex(RuntimeError, "checkout worker.*identity") as refused:
                self.mod.checkout_worker("/work/boss", root, foreign, forbidden, forbidden, forbidden, forbidden)
            self.assertNotIn("renew-checkout", str(refused.exception))

    def test_checkout_running_worker_is_kept_and_deadline_is_the_only_terminal_trigger(self):
        for reason in (None, "BackoffLimitExceeded", "DeadlineExceeded"):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                job = self.spec()
                name = job["metadata"]["name"]
                declared = copy.deepcopy(job)
                job["metadata"]["uid"] = "job-uid"
                session = self.mod.Session(name, "job-uid", "pod", "pod-uid", "container", "a" * 40)
                binding = {"session": name, "source_head": session.source_head, "local_root": "/work/boss"}
                self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
                self.mod.save_record(root / (name + ".binding.json"), binding)
                self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": session.source_head, "job": declared})
                if reason:
                    job["status"] = {"conditions": [{"type": "Failed", "status": "True", "reason": reason}]}
                calls = []
                def read(argv):
                    calls.append(argv)
                    if argv[:2] == ["get", "jobs"]:
                        return {"kind": "JobList", "items": [job]}
                    raise RuntimeError("positive control deliberately refused")
                forbidden = lambda *args: self.fail("no replacement without complete expiry evidence")
                if reason == "DeadlineExceeded":
                    with self.assertRaisesRegex(RuntimeError, "positive control deliberately refused") as refused:
                        self.mod.checkout_worker("/work/boss", root, read, forbidden, forbidden, forbidden, forbidden)
                    self.assertNotIn("renew-checkout", str(refused.exception))
                    self.assertEqual(len(calls), 2)
                else:
                    self.assertEqual(self.mod.checkout_worker("/work/boss", root, read, forbidden, forbidden, forbidden, forbidden), binding)
                    self.assertEqual(len(calls), 1)
                self.assertEqual(calls[0], ["get", "jobs", "--field-selector", "metadata.name=" + name],
                                 "each checkout use observes only its own native Job")

    def test_checkout_unknown_or_partial_observation_never_retires_or_allocates(self):
        cases = [{}, {"kind": "JobList", "items": [], "metadata": {"continue": "next"}},
                 {"kind": "JobList", "items": [{}]}, {"kind": "JobList", "items": []}]
        for answer in cases:
            with self.subTest(answer=answer), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                old = self.mod.Session("dev-build-" + "a" * 16, "uid", "pod", "pod-uid", "container", "a" * 40)
                self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
                self.mod.save_record(root / (old.job + ".binding.json"), {"session": old.job, "source_head": old.source_head, "local_root": "/work/boss"})
                forbidden = lambda *args: self.fail("unknown observations authorize no effects")
                with self.assertRaisesRegex(RuntimeError, "checkout worker") as refused:
                    self.mod.checkout_worker("/work/boss", root, lambda argv: answer, forbidden, forbidden, forbidden, forbidden)
                self.assertNotIn("renew-checkout", str(refused.exception))
                self.assertFalse(list(root.glob("checkout-renewal-*.json")))

    def test_checkout_unacknowledged_retirement_cannot_allocate_a_replacement(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = self.mod.Session("dev-build-" + "a" * 16, "uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"), {"session": old.job, "source_head": old.source_head, "local_root": "/work/boss"})
            calls = []
            def read(argv):
                if argv[:2] == ["get", "jobs"]:
                    return {"kind": "JobList", "items": []}
                return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "known-dev"}}
            with self.assertRaisesRegex(RuntimeError, "renew-checkout.*retirement"):
                self.mod.checkout_worker("/work/boss", root, read, lambda session: None,
                    lambda *args: calls.append(args), lambda *args: None, lambda path: "a" * 40)
            self.assertEqual(calls, [], "unacknowledged retirement must not authorize allocation")

    def test_renew_checkout_cli_uses_the_native_controller_and_rejects_a_builder_cwd(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = self.mod.Session("dev-build-" + "a" * 16, "uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (old.job + ".json"), self.mod.asdict(old))
            self.mod.save_record(root / (old.job + ".binding.json"), {"session": old.job, "source_head": old.source_head, "local_root": "/work/boss"})
            real_load = self.mod.load_record
            def load(value):
                return {} if value.name == "config.json" else real_load(value)
            def read(argv, **kwargs):
                if argv == ["get", "jobs", "--field-selector", "metadata.name=" + old.job]:
                    return {"kind": "List", "items": [], "metadata": {"resourceVersion": ""}}
                self.assertEqual(argv, ["get", "deployment", "boss-dev"])
                return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
            def retire(session):
                self.assertEqual(session, old)
                self.mod.save_record(root / (old.job + ".retired.json"), {"session": self.mod.asdict(old)})
            def launch(head, config, store, local, **kwargs):
                self.assertEqual((head, store, str(local)), ("b" * 40, root, "/work/boss"))
                name = kwargs["name"]
                session = self.mod.Session(name, "new-uid", "new-pod", "new-pod-uid", "container", head)
                self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
                self.mod.save_record(root / (name + ".binding.json"), {"session": name, "source_head": head, "local_root": str(local)})
            with patch.object(self.mod, "load_record", side_effect=load), \
                 patch.object(self.mod, "kube_json", side_effect=read), patch.object(self.mod, "lifecycle_retire", return_value=retire), \
                 patch.object(self.mod, "launch", side_effect=launch) as launched, \
                 patch.object(self.mod, "source_head", return_value="b" * 40), \
                 patch.object(self.mod.os, "getcwd", return_value="/work/boss"):
                self.assertEqual(self.mod.main(["renew-checkout"], root=root), 0)
                self.assertEqual(launched.call_count, 1)
            with patch.object(self.mod.os, "getcwd", return_value="/work/wt/agent-car"):
                with self.assertRaisesRegex(ValueError, "only for the exact shared checkout"):
                    self.mod.main(["renew-checkout"], root=root)

    def test_explicit_checkout_renewal_refuses_an_unchanged_healthy_binding(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            declared = self.spec()
            name = declared["metadata"]["name"]
            session = self.mod.Session(name, "uid", "pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
            self.mod.save_record(root / (name + ".binding.json"), {"session": name, "local_root": "/work/boss", "source_head": session.source_head})
            self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": session.source_head, "job": declared})
            job = copy.deepcopy(declared)
            job["metadata"]["uid"] = session.job_uid
            with patch.object(self.mod, "load_record", wraps=self.mod.load_record) as loaded, \
                 patch.object(self.mod, "kube_json", return_value={"kind": "JobList", "items": [job]}), \
                 patch.object(self.mod.os, "getcwd", return_value="/work/boss"):
                original = loaded._mock_wraps
                loaded.side_effect = lambda p: {} if p.name == "config.json" else original(p)
                with self.assertRaisesRegex(RuntimeError, "no renewal:.*not expired or absent"):
                    self.mod.main(["renew-checkout"], root=root)

    def test_checkout_execution_failure_names_renewal_only_for_native_expiry_or_absence(self):
        from unittest.mock import patch
        declared = self.spec()
        name = declared["metadata"]["name"]
        session = self.mod.Session(name, "uid", "pod", "pod-uid", "container", "a" * 40)
        binding = {"session": name, "local_root": "/work/boss", "source_head": session.source_head}
        for reason in (None, "BackoffLimitExceeded", "DeadlineExceeded", "absent", "unknown"):
            with self.subTest(reason=reason):
                job = copy.deepcopy(declared)
                job["metadata"]["uid"] = session.job_uid
                if reason:
                    job["status"] = {"conditions": [{"type": "Failed", "status": "True", "reason": reason}]}
                def read(argv, **kwargs):
                    if argv[:2] == ["get", "deployment"]:
                        return {"kind": "Deployment", "metadata": {"name": "boss-dev", "namespace": "boss-dev", "uid": "dev-uid"}}
                    return {} if reason == "unknown" else {"kind": "JobList", "items": [] if reason == "absent" else [job]}
                with patch.object(self.mod, "observed_exec", side_effect=RuntimeError("original execution cause")), \
                     patch.object(self.mod, "kube_json", side_effect=read):
                    with self.assertRaises(RuntimeError) as raised:
                        self.mod.bound_exec(binding, session, ["/usr/bin/true"], None, lambda *a: None, lambda *a: None, lambda *a: None)
                self.assertIn("original execution cause", str(raised.exception))
                self.assertEqual("renew-checkout" in str(raised.exception), reason in ("DeadlineExceeded", "absent"))

    def test_checkout_expiry_during_execution_names_the_binding_and_renewal_door(self):
        import io
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            declared = self.spec()
            name = declared["metadata"]["name"]
            session = self.mod.Session(name, "uid", "expired-pod", "pod-uid", "container", "a" * 40)
            self.mod.save_record(root / (name + ".json"), self.mod.asdict(session))
            self.mod.save_record(root / (name + ".binding.json"), {"session": name, "source_head": session.source_head, "local_root": "/work/boss"})
            self.mod.save_record(root / (name + ".intent.json"), {"name": name, "head": session.source_head, "job": declared})
            job = copy.deepcopy(declared)
            job["metadata"]["uid"] = session.job_uid
            real_load, real_main = self.mod.load_record, self.mod.main
            def load(value):
                return {} if value.name == "config.json" else real_load(value)
            stderr = io.StringIO()
            reads = []
            def observation(argv, **kwargs):
                reads.append(argv)
                current = copy.deepcopy(job)
                if len(reads) > 1:
                    current["status"] = {"conditions": [{"type": "Failed", "status": "True", "reason": "DeadlineExceeded"}]}
                return {"kind": "List", "items": [current]}
            with patch.object(self.mod, "load_record", side_effect=load), \
                 patch.object(self.mod, "kube_json", side_effect=observation), \
                 patch.object(self.mod, "source_head", return_value=session.source_head), \
                 patch.object(self.mod, "follow", return_value=(session.source_head, session.source_head)), \
                 patch.object(self.mod, "edit_level_hand_in", return_value=[]), \
                 patch.object(self.mod, "observed_exec", side_effect=RuntimeError("pods expired-pod not found")), \
                 patch.object(self.mod.os, "getcwd", return_value="/work/boss"), \
                 patch.object(sys, "stderr", stderr), \
                 patch.object(self.mod, "main", side_effect=lambda: real_main(["exec-auto", "--", "/usr/bin/true"], root=root)):
                self.assertEqual(self.mod.cli(), 75)
            self.assertIn("pods expired-pod not found", stderr.getvalue())
            self.assertIn(name, stderr.getvalue())
            self.assertIn("/work/tools/dev-control/dev-build renew-checkout", stderr.getvalue())

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

    # Backlog ceea10b8, design ac162341 (Q1, David 2026-10-20): a builder's
    # commit used to make the binding stale. A commit that DESCENDS from the
    # synced head is now shipped through the put door, path by path, and
    # the binding follows it; anything else still refuses, naming stop.
    def worktree(self, tmp):
        """A real git worktree at commit A, and a worker tree holding A."""
        repo, worker = Path(tmp) / "wt", Path(tmp) / "worker"
        git(repo.parent, "init", "-q", str(repo))
        (repo / "crates/x/src").mkdir(parents=True)
        (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn a() {}\n")
        (repo / "gone.txt").write_bytes(b"deleted by the commit\n")
        (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho one\n")
        (repo / "tool.sh").chmod(0o755)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "A")
        admitted = git(repo, "rev-parse", "HEAD")
        # The worker is what the published bootstrap leaves: a repository
        # that fetched the admitted sha alone and checked it out detached
        # (46b63903: its git is part of what a re-sync must carry).
        git(worker.parent, "init", "-q", str(worker))
        git(worker, "fetch", "-q", "--no-tags", str(repo), admitted)
        git(worker, "checkout", "-q", "--detach", admitted)
        return repo, worker, admitted

    def follower(self, tmp, repo, worker, admitted, fail_on=None):
        root = Path(tmp) / "workers"
        root.mkdir(mode=0o700)
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod", "pod-uid", "container-id", admitted)
        binding = {"session": session.job, "local_root": str(repo), "source_head": admitted}
        self.mod.save_record(root / (session.job + ".binding.json"), binding)
        shipped = []

        def ship(argv, stdin):
            # The REAL worker script, on a directory standing in for
            # /workspace/source: what it writes is what a worker would hold.
            shipped.append(argv[4:])
            if fail_on is not None and fail_on in argv:
                return subprocess.CompletedProcess(argv, 1, b"", b"fixture transport failure")
            local = [*argv]
            local[3] = local[3].replace("/workspace/source", str(worker))
            return subprocess.run(local, input=stdin, capture_output=True, check=False)

        def local_git(args, stdin=None):
            return self.mod.local_git(repo, args, lambda receipt: None, run=host_git, stdin=stdin)
        return root, session, binding, ship, shipped, local_git

    def test_uncommitted_source_cannot_silently_measure_the_workers_committed_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            mutant = b"#!/bin/sh\necho mutant\n"
            (repo / "tool.sh").write_bytes(mutant)
            self.assertNotEqual((repo / "tool.sh").read_bytes(), (worker / "tool.sh").read_bytes())
            # The review's real shape: a passing committed worker program
            # while the host's uncommitted replacement is a different test.
            result = subprocess.run(["/bin/sh", str(worker / "tool.sh")], capture_output=True)
            self.assertEqual((result.returncode, result.stdout), (0, b"one\n"))
            with self.assertRaisesRegex(ValueError, "uncommitted.*dev-build put.*dev-build get.*dev-build exec"):
                self.mod.follow(session, binding, a, root, local_git, ship)
            self.assertEqual(shipped, [], "a refusal must happen before a committed re-sync can overwrite a hand-in")
            self.assertEqual((worker / "tool.sh").read_bytes(), b"#!/bin/sh\necho one\n")

    def test_explicit_put_get_and_hand_exec_run_a_dirty_mutant_without_a_commit(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            original = (repo / "tool.sh").read_bytes()
            for raw, expected in ((b"#!/bin/sh\necho mutant\n", b"mutant\n"), (original, b"one\n")):
                (repo / "tool.sh").write_bytes(raw)
                put = ship(self.mod.source_file_argv("put", "tool.sh", "755"), raw)
                self.assertEqual((put.returncode, put.stdout.strip()), (0, hashlib.sha256(raw).hexdigest().encode()))
                got = ship(self.mod.source_file_argv("get", "tool.sh"), b"")
                self.assertEqual((got.returncode, got.stdout), (0, raw))
                result = subprocess.run(["/bin/sh", str(worker / "tool.sh")], capture_output=True)
                self.assertEqual((result.returncode, result.stdout), (0, expected))
                self.assertEqual(git(repo, "rev-parse", "HEAD"), a)
                self.assertEqual(git(worker, "rev-parse", "HEAD"), a)
            self.assertEqual(self.mod.follow(session, binding, a, root, local_git, ship), (a, a))

    def test_main_refuses_a_dirty_mutation_before_any_worker_execution(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            self.mod.save_record(root / (session.job + ".json"), dataclasses.asdict(session))
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho mutant\n")
            ran = []
            def execute(*args):
                ran.append(args)
                return subprocess.run(["/bin/sh", str(worker / "tool.sh")], capture_output=True)
            with patch.object(self.mod.os, "getcwd", return_value=str(repo)), \
                 patch.object(self.mod, "bound_exec", side_effect=execute):
                with self.assertRaisesRegex(ValueError, 'uncommitted.*dev-build put'):
                    self.mod.main(["exec-auto", "--", "/bin/sh", "tool.sh"], root=root)
            self.assertEqual(ran, [])

    def test_staged_deleted_and_untracked_paths_refuse_and_data_is_never_shipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            (repo / "untracked-private.txt").write_bytes(b"not source and never transferred\n")
            with self.assertRaisesRegex(ValueError, 'uncommitted.*untracked-private.txt'):
                self.mod.follow(session, binding, a, root, local_git, ship)
            self.assertEqual(shipped, [])
            self.assertFalse((worker / "untracked-private.txt").exists())
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho staged\n")
            git(repo, "add", "tool.sh")
            with self.assertRaisesRegex(ValueError, 'uncommitted.*tool.sh'):
                self.mod.follow(session, binding, a, root, local_git, ship)
            (repo / "gone.txt").unlink()
            with self.assertRaisesRegex(ValueError, 'uncommitted.*gone.txt'):
                self.mod.follow(session, binding, a, root, local_git, ship)
            self.assertEqual(shipped, [])

    def test_a_dirty_source_read_failure_is_not_a_clean_tree(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "u", "p", "pu", "c", "a" * 40)
        for failed in ("config", "diff", "ls-files"):
            def read(args):
                if args[0] == "config" and failed != "config":
                    return subprocess.CompletedProcess(args, 1, b"", b"")
                if args[0] == "diff":
                    self.assertEqual(args[-2:], ["HEAD", "--"])
                return subprocess.CompletedProcess(args, 128 if args[0] == failed else 0, b"", b"unreadable")
            with self.assertRaisesRegex(ValueError, 'cannot judge uncommitted source.*exit 128'):
                self.mod.require_committed_source(session, read)

    def test_malformed_filter_names_refuse_before_reading_source(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "u", "p", "pu", "c", "a" * 40)
        for raw in (b"", b"filter.x.clean", b"filter.x=bad.clean\0", b"filter.x\n.clean\0", b"filter.\xff.clean\0"):
            calls = []
            def read(args):
                calls.append(args)
                return subprocess.CompletedProcess(args, 0, raw, b"")
            with self.assertRaisesRegex(ValueError, 'malformed filter configuration'):
                self.mod.require_committed_source(session, read)
            self.assertEqual(len(calls), 1)

    def test_a_dirty_source_metadata_read_never_executes_a_configured_clean_filter(self):
        import shlex
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            (repo / ".gitattributes").write_text("tool.sh filter=fixture\n")
            git(repo, "add", ".gitattributes")
            git(repo, "commit", "-q", "-m", "attributes")
            marker, clean = Path(tmp) / "filter-ran", Path(tmp) / "clean.sh"
            clean.write_text("printf ran > " + shlex.quote(str(marker)) + "\ncat\n")
            git(repo, "config", "filter.fixture.clean", "/bin/sh " + shlex.quote(str(clean)))
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho two\n")
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            with self.assertRaisesRegex(ValueError, 'uncommitted.*tool.sh'):
                self.mod.follow(session, binding, a, root, local_git, ship)
            self.assertFalse(marker.exists(), "a metadata read must not execute the repository's clean filter")
            git(repo, "hash-object", "--path=tool.sh", "tool.sh")
            self.assertTrue(marker.exists(), "positive control: the configured filter is live")
            self.assertEqual(shipped, [])

    def test_a_commit_that_descends_from_the_synced_head_is_shipped_and_the_binding_follows_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn b() {}\n")
            (repo / "crates/y").mkdir()
            (repo / "crates/y/new.rs").write_bytes(b"// added by the commit\n")
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho two\n")
            git(repo, "rm", "-q", "gone.txt")
            git(repo, "add", "-A")
            git(repo, "commit", "-q", "-m", "B")
            b = git(repo, "rev-parse", "HEAD")
            before = (root / (session.job + ".binding.json")).read_bytes()

            self.assertEqual(self.mod.resync(session, binding, a, b, root, local_git, ship), b)

            self.assertEqual((worker / "crates/x/src/lib.rs").read_bytes(), b"pub fn b() {}\n")
            self.assertEqual((worker / "crates/y/new.rs").read_bytes(), b"// added by the commit\n")
            self.assertEqual((worker / "tool.sh").read_bytes(), b"#!/bin/sh\necho two\n")
            self.assertTrue(os.access(worker / "tool.sh", os.X_OK), "an executable stays executable")
            self.assertFalse((worker / "gone.txt").exists())
            # A receipt per path: the sha256 the worker printed, or removed.
            record = self.mod.load_record(root / (session.job + ".sync-" + a + ".json"))
            self.assertEqual((record["admitted_head"], record["from"], record["synced_head"]), (a, a, b))
            receipts = {r["path"]: r for r in record["paths"]}
            self.assertEqual(set(receipts), {"crates/x/src/lib.rs", "crates/y/new.rs", "tool.sh", "gone.txt"})
            self.assertEqual(receipts["crates/y/new.rs"]["sha256"], hashlib.sha256(b"// added by the commit\n").hexdigest())
            self.assertEqual(receipts["gone.txt"]["result"], "removed")
            # admitted_head stays the fetched sha: the binding is untouched,
            # and the synced head is followed beside it.
            self.assertEqual((root / (session.job + ".binding.json")).read_bytes(), before)
            self.assertEqual(self.mod.followed_head(root, binding), b)
            self.mod.check_binding(binding, session, b, self.mod.followed_head(root, binding))
            with self.assertRaises(ValueError):
                self.mod.check_binding(binding, session, a, self.mod.followed_head(root, binding))
            # The next commit follows from B, not from the admitted sha.
            (repo / "crates/y/new.rs").write_bytes(b"// changed again\n")
            git(repo, "commit", "-q", "-am", "C")
            c = git(repo, "rev-parse", "HEAD")
            shipped.clear()
            self.assertEqual(self.mod.resync(session, binding, b, c, root, local_git, ship), c)
            # The changed path, then the worker's git HEAD (46b63903).
            self.assertEqual(shipped, [["put", "crates/y/new.rs", "644"], [c]])
            self.assertEqual(self.mod.followed_head(root, binding), c)
            # An empty commit ships no path and still moves the binding.
            git(repo, "commit", "-q", "--allow-empty", "-m", "D")
            d = git(repo, "rev-parse", "HEAD")
            shipped.clear()
            self.assertEqual(self.mod.resync(session, binding, c, d, root, local_git, ship), d)
            self.assertEqual((shipped, self.mod.followed_head(root, binding)), ([[d]], d))

    # Backlog 46b63903 (2026-10-22, run 96f79210 on f7b43945): the put
    # door carried every file and the record followed the commit, but the
    # worker's git stayed at the admitted sha, so a probe rehearsal reading
    # `git show HEAD:` through exec-auto read a tree the worker no longer
    # held, and `git status` there called the builder's commit unstaged
    # edits. The worker's HEAD is now the followed commit itself.
    def test_a_re_sync_moves_the_workers_git_head_to_the_followed_commit(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn b() {}\n")
            git(repo, "commit", "-q", "-am", "B")
            b = git(repo, "rev-parse", "HEAD")
            self.assertEqual(self.mod.resync(session, binding, a, b, root, local_git, ship), b)
            self.assertEqual(git(worker, "rev-parse", "HEAD"), b)
            self.assertEqual(git(worker, "show", "HEAD:crates/x/src/lib.rs"), "pub fn b() {}")
            self.assertEqual(git(worker, "status", "--porcelain"), "", "the commit is HEAD, not unstaged edits")
            record = self.mod.load_record(root / (session.job + ".sync-" + a + ".json"))
            self.assertEqual(record["git_head"]["head"], b)
            # A worker re-synced before this fix holds B's files with its
            # HEAD still at the admitted sha: the objects reach back to the
            # admitted sha, which every worker fetched, so it follows too.
            git(worker, "reset", "-q", "--mixed", a)
            self.assertEqual(git(worker, "status", "--porcelain"), "M crates/x/src/lib.rs", "the old worker's shape")
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho three\n")
            git(repo, "commit", "-q", "-am", "C")
            c = git(repo, "rev-parse", "HEAD")
            self.assertEqual(self.mod.resync(session, binding, b, c, root, local_git, ship), c)
            self.assertEqual(git(worker, "rev-parse", "HEAD"), c)
            self.assertEqual(git(worker, "status", "--porcelain"), "")
            # An empty commit ships no path and still moves HEAD.
            git(repo, "commit", "-q", "--allow-empty", "-m", "D")
            d = git(repo, "rev-parse", "HEAD")
            self.assertEqual(self.mod.resync(session, binding, c, d, root, local_git, ship), d)
            self.assertEqual(git(worker, "rev-parse", "HEAD"), d)
            for named in ("HEAD", "main", "-q", d[:12]):
                with self.assertRaises(ValueError):
                    self.mod.source_head_argv(named)

    def test_objects_over_the_put_doors_bound_refuse_before_anything_ships(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            (repo / "noise.bin").write_bytes(os.urandom(90))
            git(repo, "add", "noise.bin")
            git(repo, "commit", "-q", "-m", "B")
            b = git(repo, "rev-parse", "HEAD")
            # The file fits the bound; the commit's objects around it do not.
            limit, self.mod.SOURCE_LIMIT = self.mod.SOURCE_LIMIT, 100
            try:
                with self.assertRaises(ValueError) as refused:
                    self.mod.resync(session, binding, a, b, root, local_git, ship)
            finally:
                self.mod.SOURCE_LIMIT = limit
            self.assertIn("objects", str(refused.exception))
            self.assertIn("dev-build stop " + session.job, str(refused.exception))
            self.assertEqual(shipped, [])
            self.assertEqual(git(worker, "rev-parse", "HEAD"), a)

    def test_a_head_that_does_not_descend_from_the_synced_head_refuses_naming_stop(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
            (repo / "gone.txt").write_bytes(b"one line\n")
            git(repo, "commit", "-q", "-am", "B")
            b = git(repo, "rev-parse", "HEAD")
            git(repo, "reset", "-q", "--hard", a)
            (repo / "gone.txt").write_bytes(b"another line\n")
            git(repo, "commit", "-q", "-am", "B'")
            sibling = git(repo, "rev-parse", "HEAD")
            self.assertEqual(self.mod.resync(session, binding, a, b, root, local_git, ship), b)
            shipped.clear()
            with self.assertRaises(ValueError) as refused:
                self.mod.resync(session, binding, b, sibling, root, local_git, ship)
            self.assertIn("does not descend", str(refused.exception))
            self.assertIn("dev-build stop " + session.job, str(refused.exception))
            self.assertEqual(shipped, [], "a refused re-sync ships nothing")
            self.assertEqual(self.mod.followed_head(root, binding), b)

    def test_a_symlink_submodule_mode_change_oversize_file_or_forbidden_path_refuses_before_anything_ships(self):
        # Each stages its own change; the gitlink has no directory, so a
        # blanket `add -A` after it would stage it away again.
        def symlink(repo):
            os.symlink("crates/x/src/lib.rs", repo / "link")
            git(repo, "add", "link")
        def submodule(repo):
            git(repo, "update-index", "--add", "--cacheinfo", "160000," + "1" * 40 + ",vendored")
        def mode_only(repo):
            (repo / "gone.txt").chmod(0o755)
            git(repo, "add", "gone.txt")
        def oversize(repo):
            (repo / "big.bin").write_bytes(b"x" * (16 * 1024 * 1024 + 1))
            git(repo, "add", "big.bin")
        def forbidden(repo):
            (repo / ".cargo").mkdir()
            (repo / ".cargo/config.toml").write_bytes(b"[build]\n")
            git(repo, "add", ".cargo/config.toml")
        cases = {"symlink": (symlink, "symlink"), "submodule": (submodule, "submodule"),
                 "mode": (mode_only, "mode change"), "oversize": (oversize, "16 MiB"),
                 "forbidden": (forbidden, ".cargo/config.toml")}
        for case, (change, named) in cases.items():
            with self.subTest(case), tempfile.TemporaryDirectory() as tmp:
                repo, worker, a = self.worktree(tmp)
                root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a)
                # A good path beside the bad one: nothing ships, not even it.
                (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn ok() {}\n")
                git(repo, "add", "-A")
                change(repo)
                git(repo, "commit", "-q", "-m", case)
                head = git(repo, "rev-parse", "HEAD")
                with self.assertRaises(ValueError) as refused:
                    self.mod.resync(session, binding, a, head, root, local_git, ship)
                self.assertIn(named, str(refused.exception))
                self.assertIn("dev-build stop " + session.job, str(refused.exception))
                self.assertEqual(shipped, [])
                self.assertEqual((worker / "crates/x/src/lib.rs").read_bytes(), b"pub fn a() {}\n")
                self.assertEqual(self.mod.followed_head(root, binding), a)

    def test_a_re_sync_that_fails_part_way_does_not_advance_and_a_retry_completes_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, a = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, a, fail_on="tool.sh")
            (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn b() {}\n")
            (repo / "tool.sh").write_bytes(b"#!/bin/sh\necho two\n")
            git(repo, "commit", "-q", "-am", "B")
            b = git(repo, "rev-parse", "HEAD")
            with self.assertRaises(RuntimeError) as failed:
                self.mod.resync(session, binding, a, b, root, local_git, ship)
            self.assertIn("tool.sh", str(failed.exception))
            # The receipt comes before the advance: no sync record, the
            # binding still at A, and the intent names what was attempted.
            self.assertEqual(self.mod.followed_head(root, binding), a)
            self.assertFalse((root / (session.job + ".sync-" + a + ".json")).exists())
            intents = [self.mod.load_record(p) for p in root.glob(session.job + ".resync-intent-*.json")]
            self.assertEqual([(i["from"], i["to"]) for i in intents], [(a, b)])
            # A head whose diff no longer covers the interrupted paths
            # cannot say what the worker holds: refused, naming stop.
            git(repo, "reset", "-q", "--hard", a)
            (repo / "gone.txt").write_bytes(b"elsewhere\n")
            git(repo, "commit", "-q", "-am", "B'")
            other = git(repo, "rev-parse", "HEAD")
            with self.assertRaises(ValueError) as refused:
                self.mod.resync(session, binding, a, other, root, local_git, ship)
            self.assertIn("interrupted", str(refused.exception))
            self.assertIn("dev-build stop " + session.job, str(refused.exception))
            # The retry on the same head re-ships every path and advances.
            git(repo, "reset", "-q", "--hard", b)
            def ok(argv, stdin):
                local = [*argv]
                local[3] = local[3].replace("/workspace/source", str(worker))
                return subprocess.run(local, input=stdin, capture_output=True, check=False)
            self.assertEqual(self.mod.resync(session, binding, a, b, root, local_git, ok), b)
            self.assertEqual((worker / "tool.sh").read_bytes(), b"#!/bin/sh\necho two\n")
            self.assertEqual(self.mod.followed_head(root, binding), b)

    def caller_input_with_stdin(self, stdin, verb, *flags):
        # The caller's real sys.stdin in a child: stdin=None in the code
        # below CLOSES fd 0 there (preexec), which makes sys.stdin None,
        # the shape `<&-` gives (backlog 267af53d).
        code = (
            "import importlib.util, sys\n"
            "spec = importlib.util.spec_from_file_location('dev_build', sys.argv[1])\n"
            "mod = importlib.util.module_from_spec(spec)\n"
            "spec.loader.exec_module(mod)\n"
            "try:\n"
            "    data = mod.caller_input(sys.argv[2], 'forward' in sys.argv[3:], sys.stdin, 'allow-empty' in sys.argv[3:])\n"
            "except ValueError as error:\n"
            "    print('REFUSED ' + str(error))\n"
            "    sys.exit(75)\n"
            "print(repr(data))\n"
        )
        closed = stdin is None
        return subprocess.run(
            [sys.executable, "-I", "-c", code, self.mod.__file__, verb, *flags],
            stdin=subprocess.DEVNULL if closed else stdin, capture_output=True, timeout=5, check=False,
            preexec_fn=(lambda: os.close(0)) if closed else None)

    def test_a_put_with_no_input_refuses_and_sends_nothing(self):
        # Backlog 06e994f7 (run df127edb, 2026-10-19): `put <s> <path>`
        # without `< file` wrote an EMPTY file over a test, a script and
        # a verb in one worker, each answering e3b0c442..., the sha256
        # of nothing. Zero bytes is refused unless asked for by name.
        result = self.caller_input_with_stdin(subprocess.DEVNULL, "put")
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn(b"zero bytes", result.stdout)
        self.assertIn(b"< <file>", result.stdout)
        self.assertIn(b"--allow-empty", result.stdout)
        allowed = self.caller_input_with_stdin(subprocess.DEVNULL, "put", "allow-empty")
        self.assertEqual(allowed.stdout, b"b''\n", allowed.stderr)
        parser = self.mod.arguments()
        self.assertTrue(parser.parse_args(["put", "--allow-empty", "dev-build-1234567890abcdef", "a.txt"]).allow_empty)
        self.assertFalse(parser.parse_args(["put", "dev-build-1234567890abcdef", "a.txt"]).allow_empty)
        with tempfile.TemporaryFile() as given:
            given.write(b"the file")
            given.seek(0)
            self.assertEqual(self.caller_input_with_stdin(given, "put").stdout, b"b'the file'\n")

    def test_a_put_from_a_terminal_refuses_before_reading(self):
        class Terminal:
            def isatty(self):
                return True

            def read(self, size=-1):
                raise AssertionError("a terminal stdin was read")

        class Stdin:
            buffer = Terminal()

        with self.assertRaisesRegex(ValueError, "terminal.*< <file>"):
            self.mod.caller_input("put", False, Stdin(), False)

    def test_a_closed_stdin_is_a_named_refusal_not_a_traceback(self):
        # Backlog 267af53d (review 42ea788b): with fd 0 closed sys.stdin
        # is None and `sys.stdin.buffer` raised an AttributeError the
        # REFUSED handler does not catch, so every verb printed a trace.
        for verb, flags in (("put", ()), ("put", ("allow-empty",)), ("exec", ("forward",))):
            result = self.caller_input_with_stdin(None, verb, *flags)
            self.assertEqual(result.returncode, 75, (verb, result.stderr))
            self.assertIn(b"stdin is closed", result.stdout, verb)
            self.assertNotIn(b"Traceback", result.stderr, verb)
        for verb in ("exec", "exec-auto", "get"):
            result = self.caller_input_with_stdin(None, verb)
            self.assertEqual((result.returncode, result.stdout), (0, b"b''\n"), (verb, result.stderr))

    def test_the_remove_operation_keeps_the_put_doors_path_rules_and_removes_only_a_regular_file(self):
        for path in ("../control", ".git/config", "target/debug/boss", "apps/web/node_modules/x.js"):
            with self.assertRaises(ValueError):
                self.mod.source_file_argv("remove", path)
        for mode in ("777", "4755", "../x"):
            with self.assertRaises(ValueError):
                self.mod.source_file_argv("put", "a.txt", mode)
        with self.assertRaises(ValueError):
            self.mod.source_file_argv("exec", "a.txt")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "dir/sub").mkdir(parents=True)
            (root / "dir/file.txt").write_bytes(b"x")
            os.symlink("file.txt", root / "dir/link")

            def remove(path):
                argv = self.mod.source_file_argv("remove", path)
                argv[3] = argv[3].replace("/workspace/source", str(root))
                return subprocess.run(argv, capture_output=True, check=False)
            self.assertEqual(remove("dir/file.txt").stdout, b"removed\n")
            self.assertFalse((root / "dir/file.txt").exists())
            self.assertEqual(remove("dir/file.txt").stdout, b"absent\n")
            self.assertEqual(remove("missing/dir/file.txt").stdout, b"absent\n")
            self.assertNotEqual(remove("dir/sub").returncode, 0)
            self.assertNotEqual(remove("dir/link").returncode, 0)
            self.assertTrue((root / "dir/link").is_symlink(), "a symlink is never removed through the door")

    # Review b831d89e of car de0ed3a1 (B1, probe P1): two exec-autos from
    # one worktree with a commit between them each read the held head
    # unlocked, so one recorded X to H2 while the other's put of f.txt at
    # H1 landed after it; the record said H2, the worker held H1's bytes,
    # and the next exec-auto at H2 ran without refusing.
    def three_commits(self, tmp):
        repo, worker, x = self.worktree(tmp)
        (repo / "gone.txt").write_bytes(b"H1\n")
        git(repo, "commit", "-q", "-am", "H1")
        h1 = git(repo, "rev-parse", "HEAD")
        (repo / "gone.txt").write_bytes(b"H2\n")
        (repo / "crates/x/src/lib.rs").write_bytes(b"pub fn h2() {}\n")
        git(repo, "commit", "-q", "-am", "H2")
        return repo, worker, x, h1, git(repo, "rev-parse", "HEAD")

    def test_two_exec_autos_from_one_worktree_take_turns_and_the_record_matches_the_worker(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x, h1, h2 = self.three_commits(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, x)
            a_shipping, release = threading.Event(), threading.Event()

            def held_open(argv, stdin):
                # exec-auto A stops inside its first put until released.
                a_shipping.set()
                release.wait(10)
                return ship(argv, stdin)
            results = {}

            def run(name, head, through):
                try:
                    results[name] = self.mod.follow(session, binding, head, root, local_git, through)
                except Exception as error:  # the test reads what each answered
                    results[name] = error
            a = threading.Thread(target=run, args=("A", h1, held_open))
            a.start()
            self.assertTrue(a_shipping.wait(10))
            before = len(shipped)
            b = threading.Thread(target=run, args=("B", h2, ship))
            b.start()
            b.join(0.5)
            # B waits on A's lock: it has neither read the held head nor
            # shipped a byte while A is part way through its re-sync.
            self.assertTrue(b.is_alive(), "a second exec-auto must wait for the first one's re-sync")
            self.assertEqual(len(shipped), before)
            release.set()
            a.join(10)
            b.join(10)
            self.assertEqual(results["A"], (x, h1))
            # B read the head under the lock, after A recorded: H1 to H2.
            self.assertEqual(results["B"], (h1, h2))
            self.assertEqual(self.mod.followed_head(root, binding), h2)
            self.assertEqual((worker / "gone.txt").read_bytes(), b"H2\n")
            self.assertEqual((worker / "crates/x/src/lib.rs").read_bytes(), b"pub fn h2() {}\n")

    def test_a_re_sync_that_loses_the_record_says_it_shipped_and_the_next_exec_auto_refuses(self):
        # The unlocked race itself, run as P1 runs it: B whole inside A's
        # first put. The lock keeps exec-auto out of it; if anything does
        # reach it, the loser leaves an intent from the RECORDED head.
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x, h1, h2 = self.three_commits(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, x)
            first = {"yes": True}

            def racing(argv, stdin):
                if first.pop("yes", False):
                    self.assertEqual(self.mod.resync(session, binding, x, h2, root, local_git, ship), h2)
                return ship(argv, stdin)
            with self.assertRaises(ValueError) as lost:
                self.mod.resync(session, binding, x, h1, root, local_git, racing)
            self.assertNotIn("nothing shipped", str(lost.exception))
            self.assertIn("dev-build stop " + session.job, str(lost.exception))
            self.assertEqual(self.mod.followed_head(root, binding), h2)
            self.assertEqual((worker / "gone.txt").read_bytes(), b"H1\n", "the fixture reproduces P1")
            # The record says H2 and the worker does not hold it: the next
            # exec-auto at H2 refuses rather than build on that tree.
            with self.assertRaises(ValueError) as refused:
                self.mod.resync(session, binding, h2, h2, root, local_git, ship)
            self.assertIn("interrupted", str(refused.exception))
            self.assertIn("dev-build stop " + session.job, str(refused.exception))

    def test_a_replace_ref_cannot_ship_bytes_the_commit_does_not_name(self):
        # Review b831d89e N1 (probe P2): `git replace` in the worktree's
        # repository made the re-sync ship another blob's bytes under a
        # record naming the commit.
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, x)
            (repo / "gone.txt").write_bytes(b"committed\n")
            git(repo, "commit", "-q", "-am", "H")
            h = git(repo, "rev-parse", "HEAD")
            real = git(repo, "rev-parse", h + ":gone.txt")
            (Path(tmp) / "other").write_bytes(b"not what the commit names\n")
            other = git(repo, "hash-object", "-w", str(Path(tmp) / "other"))
            git(repo, "replace", real, other)
            self.assertEqual(self.mod.resync(session, binding, x, h, root, local_git, ship), h)
            self.assertEqual((worker / "gone.txt").read_bytes(), b"committed\n")
            record = self.mod.load_record(root / (session.job + ".sync-" + x + ".json"))
            self.assertEqual(record["paths"][0]["sha256"], hashlib.sha256(b"committed\n").hexdigest())

    def test_a_blob_whose_bytes_do_not_hash_to_its_id_stops_before_it_ships(self):
        # The same check without trusting git's configuration: the bytes
        # read for a blob id must be that id's bytes.
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x = self.worktree(tmp)
            root, session, binding, ship, shipped, _ = self.follower(tmp, repo, worker, x)
            (repo / "gone.txt").write_bytes(b"committed\n")
            git(repo, "commit", "-q", "-am", "H")
            h = git(repo, "rev-parse", "HEAD")

            def lying_git(args, stdin=None):
                answer = self.mod.local_git(repo, args, lambda receipt: None, run=host_git, stdin=stdin)
                if args[:2] == ["cat-file", "blob"]:
                    return subprocess.CompletedProcess(answer.args, 0, b"other bytes\n", b"")
                return answer
            with self.assertRaises(RuntimeError) as stopped:
                self.mod.resync(session, binding, x, h, root, lying_git, ship)
            self.assertIn("gone.txt", str(stopped.exception))
            self.assertEqual(shipped, [])
            self.assertEqual(self.mod.followed_head(root, binding), x)

    def test_a_commit_that_turns_a_directory_into_a_file_refuses_naming_stop(self):
        # Review b831d89e N2 (probe P3): the put of d onto the directory d
        # failed at os.replace on every retry while the message promised a
        # retry would ship it, and the remove never deletes a directory, so
        # no order of the two could succeed. Refused when planned.
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, x)
            git(repo, "rm", "-q", "-r", "crates/x/src")
            (repo / "crates/x").mkdir(parents=True, exist_ok=True)
            (repo / "crates/x/src").write_bytes(b"now a file\n")
            git(repo, "add", "-A")
            git(repo, "commit", "-q", "-m", "directory to file")
            head = git(repo, "rev-parse", "HEAD")
            with self.assertRaises(ValueError) as refused:
                self.mod.resync(session, binding, x, head, root, local_git, ship)
            self.assertIn("crates/x/src replaces a directory", str(refused.exception))
            self.assertIn("dev-build stop " + session.job, str(refused.exception))
            self.assertEqual(shipped, [])
            self.assertEqual(self.mod.followed_head(root, binding), x)

    def test_a_commit_that_turns_a_file_into_a_directory_is_shipped(self):
        # The other direction already works and stays working: git lists
        # the file's removal before the new directory's file, and the put
        # door makes the directory once the file is gone.
        with tempfile.TemporaryDirectory() as tmp:
            repo, worker, x = self.worktree(tmp)
            root, session, binding, ship, shipped, local_git = self.follower(tmp, repo, worker, x)
            git(repo, "rm", "-q", "gone.txt")
            (repo / "gone.txt").mkdir()
            (repo / "gone.txt/inside.txt").write_bytes(b"now a directory\n")
            git(repo, "add", "-A")
            git(repo, "commit", "-q", "-m", "file to directory")
            head = git(repo, "rev-parse", "HEAD")
            self.assertEqual(self.mod.resync(session, binding, x, head, root, local_git, ship), head)
            self.assertEqual((worker / "gone.txt/inside.txt").read_bytes(), b"now a directory\n")

    # Backlog 3165dc62 (2026-10-19): the refusal said only "stop the old
    # session", naming neither the session nor the start that follows, and
    # an operator who started a new one without stopping left the old
    # binding LIVE. It names both verbs, exactly, in the order they work.
    def test_a_stale_binding_names_the_stop_then_the_start_that_repair_it(self):
        session = self.mod.Session("dev-build-57373249e9754af4", "job-uid", "pod", "pod-uid", "container-id", "a" * 40)
        binding = {"session": session.job, "local_root": "/work/boss", "source_head": "a" * 40}
        with self.assertRaises(ValueError) as raised:
            self.mod.check_binding(binding, session, "c" * 40)
        message = str(raised.exception)
        self.assertTrue(message.startswith("source binding is stale"), message)
        stop = message.index("`dev-build stop dev-build-57373249e9754af4`")
        start = message.index("`dev-build start " + "c" * 40 + " --local-root /work/boss`")
        self.assertLess(stop, start, "retire first, then admit the new source")
        self.assertIn("a" * 40, message, "the head the session is bound to")
        # Car de0ed3a1 split the check: a binding naming another worker is
        # refused by same_worker, in the same named-verb words (merge of
        # 7893b4c9, run 20302afe).
        other = dict(binding, session="dev-build-ffffffffffffffff")
        with self.assertRaises(ValueError) as raised:
            self.mod.same_worker(other, session, "c" * 40)
        self.assertIn("`dev-build stop dev-build-ffffffffffffffff`", str(raised.exception))
        self.assertIn("`dev-build start " + "c" * 40 + " --local-root /work/boss`", str(raised.exception))

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
            self.assertEqual(len(calls), 1, "one stop attempts the delete once; it never loops on its own")

    # Backlog bc8db3ba (2026-10-18): the retire-intent was published with
    # os.link before the delete, so after one failed delete every later
    # `dev-build stop` refused with File exists and never reached the API.
    # A retry is safe because the delete is conditioned on the Job's UID.
    def test_a_failed_retirement_is_retried_by_the_next_stop_and_only_for_its_own_job(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = self.mod.Session("dev-build-1234567890abcdef", "known-job-uid", "pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
            calls = []
            def delete(name, uid, record):
                calls.append((name, uid))
                record({"completion": "observed"})
                if len(calls) == 1:
                    raise RuntimeError("UID-conditioned Job retirement unavailable; outcome retained, no retry")
                return {"kind": "Status", "status": "Success"}
            with self.assertRaises(RuntimeError):
                self.mod.retire_worker(session, root, delete)
            intent = (root / (session.job + ".retire-intent.json")).read_bytes()
            # The operator's handle for an unadmitted worker carries no pod.
            handle = self.mod.Session(session.job, session.job_uid, "", "", "", "a" * 40, "volume-uid")
            record = self.mod.retire_worker(handle, root, delete)
            self.assertEqual(record["state"], "local execution retired")
            self.assertEqual(calls, [(session.job, session.job_uid)] * 2, "the second stop attempts the UID-conditioned delete again")
            self.assertEqual((root / (session.job + ".retire-intent.json")).read_bytes(), intent, "the first intent is kept, never replaced")
            self.assertEqual(len(list(root.glob(session.job + ".lifecycle-*.json"))), 2, "each attempt keeps its own receipt")
            self.assertEqual(self.mod.retire_worker(handle, root, delete), record, "a retired worker answers its record")
            self.assertEqual(len(calls), 2, "a retired worker is not deleted again")
            other = self.mod.Session(session.job, "replacement-uid", "", "", "", "a" * 40, "volume-uid")
            with self.assertRaisesRegex(ValueError, "known-job-uid"):
                self.mod.retire_worker(other, root, delete)
            self.assertEqual(len(calls), 2, "an intent for another incarnation authorizes no delete")

    # Backlog cae3b25b (2026-10-20): a delete that succeeded on the server
    # but whose answer was lost leaves the intent on file; the retry then
    # meets a 404, which refused as "refused by API" forever. A NotFound
    # naming this Job, on a retry of an intent recorded for this same Job
    # name and UID, is the retirement having happened.
    def notfound(self, name):
        return {"kind": "Status", "status": "Failure", "reason": "NotFound", "code": 404,
                "details": {"name": name, "group": "batch", "kind": "jobs"}}

    def test_a_retried_delete_that_finds_the_job_gone_records_it_retired_with_the_404(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = self.mod.Session("dev-build-1234567890abcdef", "known-job-uid", "", "", "", "a" * 40, "volume-uid")
            calls = []
            def delete(name, uid, record):
                calls.append((name, uid))
                record({"completion": "unproven" if len(calls) == 1 else "observed"})
                if len(calls) == 1:
                    raise RuntimeError("UID-conditioned Job retirement unavailable; outcome retained, no retry")
                return self.notfound(name)
            with self.assertRaises(RuntimeError):
                self.mod.retire_worker(session, root, delete)
            record = self.mod.retire_worker(session, root, delete)
            self.assertEqual(record["state"], "local execution retired")
            self.assertTrue(record["job_absent"])
            self.assertFalse(record["job_delete_acknowledged"], "no delete was acknowledged; the 404 is the receipt")
            self.assertEqual(record["job_absent_receipt"], self.notfound(session.job))
            self.assertEqual(json.loads((root / (session.job + ".retired.json")).read_text()), record)
            self.assertEqual(self.mod.retire_worker(session, root, delete), record, "a retired worker answers its record")
            self.assertEqual(len(calls), 2, "a retired worker is not deleted again")

    # Backlog 3165dc62 (2026-10-19), and review 82b47175's N1 on 678fd302:
    # session dev-build-57373249e9754af4's Job was removed by its TTL or
    # the 10-18 clean-up, so the FIRST stop met 404 and refused, and the
    # stale-binding refusal sent the operator to that stop. A NotFound for
    # this Job's name proves no Job of that name exists, so none with its
    # UID (a same-name replacement answers 409 on the UID precondition,
    # never 404). The choice is: this Job's NotFound retires it on ANY
    # attempt, and the record says whether an earlier intent existed.
    def test_a_first_stop_that_finds_the_job_gone_records_the_404_as_its_retirement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = self.mod.Session("dev-build-57373249e9754af4", "known-job-uid", "pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
            calls = []
            def delete(name, uid, record):
                calls.append((name, uid))
                record({"completion": "observed", "http_status": 404})
                return self.notfound(name)
            record = self.mod.retire_worker(session, root, delete)
            self.assertEqual(record["state"], "local execution retired")
            self.assertTrue(record["job_absent"])
            self.assertFalse(record["job_delete_acknowledged"], "no delete was acknowledged; the 404 is the receipt")
            self.assertFalse(record["retried_intent"], "the record says no earlier stop asked")
            self.assertEqual(record["job_absent_receipt"], self.notfound(session.job))
            self.assertEqual(json.loads((root / (session.job + ".retired.json")).read_text()), record)
            self.assertEqual(self.mod.retire_worker(session, root, delete), record, "a retired worker answers its record")
            self.assertEqual(len(calls), 1, "a retired worker is not deleted again")

    def test_a_404_for_another_name_or_not_a_batch_job_is_not_a_retirement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = self.mod.Session("dev-build-1234567890abcdef", "known-job-uid", "", "", "", "a" * 40, "volume-uid")
            # A NotFound for some other name proves nothing about this Job.
            with self.assertRaisesRegex(RuntimeError, "execution stays disabled"):
                self.mod.retire_worker(session, root, lambda name, uid, record: self.notfound("dev-build-ffffffffffffffff"))
            self.assertTrue(self.mod.retirement_requested(root, session.job))
            self.assertFalse((root / (session.job + ".retired.json")).exists())
            # Review 82b47175 N2: the controller's check is the lifecycle
            # client's own, so a NotFound naming this Job but not as a
            # batch Job, or without code 404, retires nothing either.
            for key, value in (("code", 410), ("details", {"name": session.job, "group": "", "kind": "pods"})):
                answer = dict(self.notfound(session.job), **{key: value})
                with self.assertRaisesRegex(RuntimeError, "execution stays disabled"):
                    self.mod.retire_worker(session, root, lambda name, uid, record: answer)
                self.assertFalse((root / (session.job + ".retired.json")).exists(), key)

    def test_public_probe_values_are_worker_argv_and_never_controller_environment(self):
        argv = self.mod.probe_argv(["printf", "literal;touch /control"], ["BOSS_JOBS_URL=http://probe.invalid:7900", "BOSS_CAR_CONVERGED_AT=example time"])
        self.assertEqual(argv, ["/usr/bin/env", "--", "BOSS_JOBS_URL=http://probe.invalid:7900", "BOSS_CAR_CONVERGED_AT=example time", "printf", "literal;touch /control"])
        for entry in ("BASH_ENV=/candidate", "PATH=/candidate", "BOSS_ACTOR=admin", "BOSS_MACHINE_TOKEN=secret", "BOSS_JOBS_URL=bad\0value"):
            with self.assertRaises(ValueError):
                self.mod.probe_argv(["true"], [entry])

    def test_a_preflight_reader_is_handed_the_edit_level_the_controller_read_through_the_door(self):
        # Backlog bc6afd15 (2026-10-18, run 46a922d6): `exec-auto -- /bin/bash
        # infra/gate.sh --lint` refused with exit 2 before judging anything,
        # because a-car-stays-under-the-edit-level asked the instance itself
        # from a worker that has, by design, no route to it: HTTP 000. The
        # controller holds the door; it reads the level once and hands it in.
        root = "/work/boss/.claude/worktrees/agent-x"
        reads = []
        def door():
            reads.append(1)
            return subprocess.CompletedProcess([], 0, b'{"edit_level": null, "manifest": "tenant.toml"}\n', b"")
        handed = ["BOSS_EDIT_LEVEL_ANSWER=" + json.dumps({"edit_level": None, "manifest": "tenant.toml"}, separators=(",", ":"), sort_keys=True)]
        for cwd, argv in (
            (root, ["/bin/bash", "infra/lint/a-car-stays-under-the-edit-level.sh"]),
            (root, ["/bin/bash", "infra/gate.sh", "--lint"]),
            (root, ["bash", "infra/gate.sh", "--quick"]),
            (root, ["infra/gate.sh", "--lint"]),
            (root + "/infra", ["/bin/bash", "lint/a-car-stays-under-the-edit-level.sh"]),
            ("/work/home", ["/bin/bash", root + "/infra/lint/a-car-stays-under-the-edit-level.sh"]),
        ):
            self.assertEqual(self.mod.edit_level_hand_in(argv, cwd, root, [], door), handed, argv)
        self.assertEqual(len(reads), 6, "one door read per reader execution")

    def test_no_other_worker_program_is_handed_the_edit_level_or_costs_a_read(self):
        # Exported to every exec, the answer would reach the suites that run
        # this lint against a stub registry and be judged instead of the stub
        # (the class gate-run 5d576fba went red on; gate.sh scopes it to the
        # pre-flight for the same reason). So only the two readers get it.
        root = "/work/boss/.claude/worktrees/agent-x"
        def door():
            raise AssertionError("no door read for a program that does not read the level")
        for cwd, argv in (
            (root, ["/bin/bash", "infra/gate.sh"]),
            (root, ["/workspace/source/infra/dev/wt-cargo", "test", "-p", "boss-testing"]),
            (root, ["/bin/bash", "infra/lint/another-lint.sh"]),
            (root, ["/bin/bash", "/elsewhere/infra/lint/a-car-stays-under-the-edit-level.sh"]),
            (root, ["/bin/bash", "-c", "infra/gate.sh --lint"]),
            (root, ["/bin/bash"]),
        ):
            self.assertEqual(self.mod.edit_level_hand_in(argv, cwd, root, [], door), [], argv)
        # A caller that names its own instance is answered by the lint, as
        # gate.sh's door leaves a pre-flight that sets BOSS_JOBS_URL alone.
        self.assertEqual(self.mod.edit_level_hand_in(["/bin/bash", "infra/gate.sh", "--lint"], root, root,
                                                     ["BOSS_JOBS_URL=http://stub.invalid:7900"], door), [])

    def test_a_preflight_named_by_its_worker_path_is_handed_the_edit_level(self):
        # Backlog 04e7aa39 (review aeb40c35 N1): `/bin/bash
        # /workspace/source/infra/gate.sh --lint` is the bound tree's own
        # gate.sh, named the way the worker sees it, but it is not under the
        # local root, so it was handed nothing and answered CANNOT ANSWER.
        root = "/work/boss/.claude/worktrees/agent-x"
        reads = []
        def door():
            reads.append(1)
            return subprocess.CompletedProcess([], 0, b'{"edit_level": "core"}', b"")
        handed = ['BOSS_EDIT_LEVEL_ANSWER={"edit_level":"core"}']
        for cwd, argv in (
            (root, ["/bin/bash", "/workspace/source/infra/gate.sh", "--lint"]),
            (root, ["/workspace/source/infra/gate.sh", "--quick"]),
            (root, ["/bin/bash", "/workspace/source/infra/lint/a-car-stays-under-the-edit-level.sh"]),
            ("/workspace/source", ["/bin/bash", "infra/gate.sh", "--lint"]),
        ):
            self.assertEqual(self.mod.edit_level_hand_in(argv, cwd, root, [], door), handed, argv)
        self.assertEqual(len(reads), 4)
        def no_door():
            raise AssertionError("no door read for a program that does not read the level")
        for argv in (["/bin/bash", "/workspace/source/infra/gate.sh"],
                     ["/bin/bash", "/workspace/sourcery/infra/gate.sh", "--lint"],
                     ["/bin/bash", "/workspace/source/../elsewhere/infra/gate.sh", "--lint"],
                     ["/workspace/source/infra/dev/wt-cargo", "test"]):
            self.assertEqual(self.mod.edit_level_hand_in(argv, root, root, [], no_door), [], argv)

    def test_env_chdir_preflights_are_handed_the_level_in_the_script_directory(self):
        root = "/work/boss/.claude/worktrees/agent-x"
        reads = []
        def door():
            reads.append(1)
            return subprocess.CompletedProcess([], 0, b'{"edit_level":"core"}', b"")
        handed = ['BOSS_EDIT_LEVEL_ANSWER={"edit_level":"core"}']
        cases = (
            ["/usr/bin/env", "-C", "/workspace/source", "/bin/bash", "infra/gate.sh", "--lint"],
            ["/usr/bin/env", "-C", "/workspace/source", "/bin/bash", "/workspace/source/infra/gate.sh", "--lint"],
            ["/usr/bin/env", "-C", root, "--", "/bin/sh", "infra/gate.sh", "--quick"],
            ["/usr/bin/env", "--", "/bin/bash", "infra/gate.sh", "--lint"],
            ["/usr/bin/env", "-C", "infra", "/bin/sh", "gate.sh", "--lint"],
            ["/usr/bin/env", "-C", "/workspace/source", "infra/lint/a-car-stays-under-the-edit-level.sh"],
        )
        for argv in cases:
            with self.subTest(argv=argv):
                self.assertEqual(self.mod.edit_level_hand_in(argv, root, root, [], door), handed)
        self.assertEqual(len(reads), len(cases))
        for argv in (
            ["/usr/bin/env", "-C", "/workspace/sourcery", "/bin/bash", "infra/gate.sh", "--lint"],
            ["/usr/bin/env", "-C", "/workspace/source/../elsewhere", "/bin/bash", "infra/gate.sh", "--lint"],
            ["/usr/bin/env", "-C", "/workspace/source", "/bin/bash", "infra/gate.sh"],
            ["/usr/bin/env", "-C"],
            ["/usr/bin/env", "-C", "/workspace/source"],
            ["/usr/bin/env", "BOSS_JOBS_URL=http://stub.invalid", "/bin/bash", "infra/gate.sh", "--lint"],
        ):
            with self.subTest(argv=argv):
                self.assertEqual(self.mod.edit_level_hand_in(argv, root, root, [], door), [])
        self.assertEqual(len(reads), len(cases), "non-readers must not read the fixed door")
        self.assertEqual(self.mod.edit_level_hand_in(cases[0], root, root,
                         ["BOSS_JOBS_URL=http://stub.invalid"], door), [])
        self.assertEqual(len(reads), len(cases))

    def test_main_hands_the_level_to_the_verified_env_chdir_shape(self):
        argv = ["/usr/bin/env", "-C", "/workspace/source", "/bin/bash", "infra/gate.sh", "--lint"]
        ran, reads = self.run_main(["exec", "dev-build-0123456789abcdef", "--", *argv])
        self.assertEqual(ran, ["/usr/bin/env", "--", 'BOSS_EDIT_LEVEL_ANSWER={"edit_level":"core"}', *argv])
        self.assertEqual(reads, ["/api/tenant/edit-level"])

    def run_main(self, argv, door_answer=b'{"edit_level":"core"}'):
        """main() with every worker and cluster effect replaced; returns the
        argv main() handed the worker and the number of door reads."""
        from unittest.mock import patch
        import contextlib
        import io
        workers = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, workers)
        local = "/work/boss/.claude/worktrees/agent-x"
        binding = {"session": "dev-build-0123456789abcdef", "local_root": local}
        session = {"job": "dev-build-0123456789abcdef", "job_uid": "u", "pod": "p", "pod_uid": "pu",
                   "container_id": "c", "source_head": "h"}
        ran, reads = [], []
        def door(path):
            reads.append(path)
            return subprocess.CompletedProcess([], 0, door_answer, b"")
        def observed(session_, worker_argv, stdin, *rest):
            ran.append(worker_argv)
            return subprocess.CompletedProcess(worker_argv, 0, b"", b"")
        m = self.mod
        with contextlib.ExitStack() as stack:
            for name, value in (("load_record", lambda path: session),
                                ("nearest_binding", lambda *a: binding), ("retirement_requested", lambda *a: False),
                                ("hand_door_binding", lambda *a: None), ("source_head", lambda *a: "h"),
                                ("same_worker", lambda *a: None), ("follow", lambda *a: ("h", "h")),
                                ("check_binding", lambda *a: None), ("door_read", door), ("observed_exec", observed)):
                stack.enter_context(patch.object(m, name, value))
            stack.enter_context(patch.object(m.os, "getcwd", lambda: local))
            stack.enter_context(patch.object(m.sys, "argv", ["dev-build", *argv]))
            stack.enter_context(contextlib.redirect_stderr(io.TextIOWrapper(io.BytesIO())))
            self.assertEqual(m.main(root=workers), 0)
        return ran[0], reads

    def test_main_hands_the_edit_level_to_an_exec_auto_preflight_named_either_way(self):
        # Review aeb40c35 N2: main()'s wiring — the reader judged on the
        # caller's argv BEFORE bound_argv rewrites it, the answer put in
        # front of the bound argv — had no test of its own.
        handed = 'BOSS_EDIT_LEVEL_ANSWER={"edit_level":"core"}'
        for program in ("infra/gate.sh", "/workspace/source/infra/gate.sh"):
            ran, reads = self.run_main(["exec-auto", "--", "/bin/bash", program, "--lint"])
            self.assertEqual(ran, ["/usr/bin/env", "--", handed, "/usr/bin/env", "-C", "/workspace/source",
                                   "/bin/bash", program, "--lint"], program)
            self.assertEqual(reads, ["/api/tenant/edit-level"])
        ran, reads = self.run_main(["exec-auto", "--", "/bin/bash", "infra/gate.sh"])
        self.assertEqual(ran, ["/usr/bin/env", "-C", "/workspace/source", "/bin/bash", "infra/gate.sh"])
        self.assertEqual(reads, [], "a program that does not read the level costs no door read")

    def test_main_hands_the_edit_level_to_a_preflight_run_by_hand_exec(self):
        # Backlog fa72c9fa (2026-10-22): `dev-build exec <session> --
        # /bin/bash infra/gate.sh --lint` runs from the worker's workingDir
        # and was handed nothing, so the lint answered CANNOT ANSWER.
        handed = 'BOSS_EDIT_LEVEL_ANSWER={"edit_level":"core"}'
        for program in ("infra/gate.sh", "/workspace/source/infra/gate.sh"):
            ran, reads = self.run_main(["exec", "dev-build-0123456789abcdef", "--", "/bin/bash", program, "--quick"])
            self.assertEqual(ran, ["/usr/bin/env", "--", handed, "/bin/bash", program, "--quick"], program)
            self.assertEqual(reads, ["/api/tenant/edit-level"])
        ran, reads = self.run_main(["exec", "dev-build-0123456789abcdef", "--", "/usr/bin/true"])
        self.assertEqual((ran, reads), (["/usr/bin/true"], []))
        with self.assertRaisesRegex(RuntimeError, "not the edit-level object"):
            self.run_main(["exec", "dev-build-0123456789abcdef", "--", "/bin/bash", "infra/gate.sh", "--lint"],
                          door_answer=b"<html>")

    def test_a_door_that_cannot_answer_refuses_before_the_worker_runs(self):
        # "Refuse loudly if the trusted read fails rather than letting the
        # worker report 000": a failed or malformed read is a refusal that
        # names the door, never a worker execution and never a guessed level.
        root = "/work/boss/.claude/worktrees/agent-x"
        argv = ["/bin/bash", "infra/gate.sh", "--lint"]
        for result, said in (
            (subprocess.CompletedProcess([], 7, b"", b"curl: (7) could not connect"), "exit 7.*could not connect"),
            (subprocess.CompletedProcess([], 0, b"<html>gateway</html>", b""), "not the edit-level object"),
            (subprocess.CompletedProcess([], 0, b'{"level": "core"}', b""), "not the edit-level object"),
            (subprocess.CompletedProcess([], 0, b"", b""), "not the edit-level object"),
        ):
            with self.assertRaisesRegex(RuntimeError, "edit level.*" + said):
                self.mod.edit_level_hand_in(argv, root, root, [], lambda: result)

    def test_the_edit_level_is_read_through_the_generations_own_door_at_its_own_address(self):
        calls = []
        def run(argv, **kwargs):
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, b'{"edit_level":null}', b"")
        os.environ["BOSS_JOBS_URL"] = "http://caller-chosen.invalid:7900"
        try:
            self.mod.door_read("/api/tenant/edit-level", run=run)
        finally:
            del os.environ["BOSS_JOBS_URL"]
        generation = Path(self.mod.__file__).resolve().parents[2]
        argv, kwargs = calls[0]
        self.assertEqual(argv, ["/bin/bash", str(generation / "infra/dev/boss-api"), "GET", "/api/tenant/edit-level"])
        self.assertNotIn("BOSS_JOBS_URL", kwargs["env"], "the token goes only to the door's own address")
        self.assertEqual(kwargs["env"]["BOSS_SOR_WAIT_SECONDS"], "60")
        self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
        self.assertLessEqual(kwargs["timeout"], 120)

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

    def test_a_hand_put_carries_the_bound_worktree_files_mode(self):
        # Backlog de1ec6d1 (2026-10-22): a hand put named no mode, so an
        # executable script put into a worker landed 644 and its test
        # failed falsely until a chmod inside the worker. The mode is the
        # bound worktree's own file's, the bit git records as 100755.
        with tempfile.TemporaryDirectory() as tmp:
            root, local, worker = Path(tmp) / "control", Path(tmp) / "worktree", Path(tmp) / "worker"
            root.mkdir()
            (local / "infra/dev").mkdir(parents=True)
            (worker / "infra/dev").mkdir(parents=True)
            session = "dev-build-0123456789abcdef"
            self.mod.save_record(root / (session + ".binding.json"), {"session": session, "local_root": str(local), "source_head": "0" * 40})
            (local / "infra/dev/tool.sh").write_bytes(b"#!/bin/sh\n")
            (local / "infra/dev/tool.sh").chmod(0o755)
            (local / "infra/dev/notes.txt").write_bytes(b"notes\n")
            (local / "infra/dev/notes.txt").chmod(0o644)
            (local / "infra/dev/link.sh").symlink_to("tool.sh")
            self.assertEqual(self.mod.hand_put_mode(root, session, "infra/dev/tool.sh"), "755")
            self.assertEqual(self.mod.hand_put_mode(root, session, "infra/dev/notes.txt"), "644")
            # Nothing to read the mode from: the door's old 644, unchanged.
            self.assertIsNone(self.mod.hand_put_mode(root, session, "infra/dev/link.sh"))
            self.assertIsNone(self.mod.hand_put_mode(root, session, "infra/dev/absent.sh"))
            self.assertIsNone(self.mod.hand_put_mode(root, "dev-build-fedcba9876543210", "infra/dev/tool.sh"))
            self.assertIsNone(self.mod.hand_put_mode(root, "../escape", "infra/dev/tool.sh"))
            # Through the worker argv: a replaced executable stays one.
            (worker / "infra/dev/tool.sh").write_bytes(b"old\n")
            (worker / "infra/dev/tool.sh").chmod(0o755)
            argv = self.mod.source_file_argv("put", "infra/dev/tool.sh", self.mod.hand_put_mode(root, session, "infra/dev/tool.sh"))
            argv[3] = argv[3].replace("/workspace/source", str(worker))
            result = subprocess.run(argv, input=b"#!/bin/sh\n", capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((worker / "infra/dev/tool.sh").stat().st_mode & 0o777, 0o755)

    def test_a_hand_put_or_exec_by_id_runs_only_from_the_tree_bound_to_that_worker(self):
        # Backlog f959a6b7 (2026-10-22, run dd16d27a): exec-auto from a
        # builder worktree reached the shared checkout's worker, the one
        # brief and dispatch read gate.sh --roster through. f7b43945 refuses
        # that resolution; the hand doors named the worker BY ID and asked
        # nothing of the caller's tree, so a builder anywhere could still
        # put a file into (or exec in) the checkout's worker.
        with tempfile.TemporaryDirectory() as tmp:
            root, checkout = Path(tmp) / "control", Path(tmp) / "boss"
            builder = checkout / ".claude" / "worktrees" / "agent-x"
            root.mkdir()
            (checkout / "infra" / "dev").mkdir(parents=True)
            (builder / "infra").mkdir(parents=True)
            (builder / ".git").write_text("gitdir: %s\n" % (checkout / ".git" / "worktrees" / "agent-x"))
            elsewhere = Path(tmp) / "wt" / "agent-y"
            elsewhere.mkdir(parents=True)
            session = "dev-build-0123456789abcdef"
            binding = {"session": session, "local_root": str(checkout), "source_head": "0" * 40}
            self.mod.save_record(root / (session + ".binding.json"), binding)
            gate = self.mod.hand_door_binding
            # The bound tree, and any directory of it, is the tree.
            self.assertEqual(gate(root, session, str(checkout)), binding)
            self.assertEqual(gate(root, session, str(checkout / "infra" / "dev")), binding)
            # A worktree nested in it, or a tree beside it, is another tree.
            for cwd in (builder, builder / "infra", elsewhere, Path(tmp)):
                with self.subTest(cwd=cwd):
                    with self.assertRaisesRegex(ValueError, "not the tree bound to worker %s" % session) as refused:
                        gate(root, session, str(cwd))
                    message = str(refused.exception)
                    self.assertIn(str(cwd), message)
                    self.assertIn(str(checkout), message)
                    self.assertIn("--local-root", message)
                    self.assertIn("nothing executed", message)
            # A worker started with no binding has no tree to hold it to.
            self.assertIsNone(gate(root, "dev-build-fedcba9876543210", str(elsewhere)))
            self.assertIsNone(gate(root, "../escape", str(elsewhere)))

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

    def test_first_identity_refusal_records_the_cause_and_never_executes(self):
        session = self.mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
        records = []
        calls = []
        def inspect(_):
            raise RuntimeError("original worker identity refused")
        def transport(argv, stdin):
            calls.append(argv)
            raise AssertionError("candidate transport must not run")
        with self.assertRaisesRegex(RuntimeError, "original worker identity refused"):
            self.mod.observed_exec(session, ["candidate"], b"", transport, inspect, records.append)
        self.assertEqual(calls, [])
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["completion"], "unproven")
        self.assertEqual(records[0]["error"], "original worker identity refused")

    def test_first_identity_refusal_through_the_cli_exits_75_without_a_traceback(self):
        code = r'''
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("dev_build", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = mod
spec.loader.exec_module(mod)
session = mod.Session("dev-build-1234567890abcdef", "job-uid", "pod-name", "pod-uid", "container-id", "a" * 40)
def inspect(_):
    raise RuntimeError("original worker identity refused")
def transport(argv, stdin):
    raise AssertionError("candidate transport must not run")
mod.main = lambda: mod.observed_exec(session, ["candidate"], b"", transport, inspect, lambda receipt: print(json.dumps(receipt)))
sys.exit(mod.cli())
'''
        result = subprocess.run([sys.executable, "-I", "-c", code, self.mod.__file__],
                                capture_output=True, timeout=5, check=False)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn(b"REFUSED", result.stderr)
        self.assertIn(b"original worker identity refused", result.stderr)
        self.assertNotIn(b"Traceback", result.stderr)
        record = json.loads(result.stdout)
        self.assertEqual(record["completion"], "unproven")
        self.assertEqual(record["error"], "original worker identity refused")

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
            if argv[:2] == ["get", "pvc"]:
                return subprocess.CompletedProcess(argv, 0, b'{"items": []}', b"")
            if longhorn_reply(argv):
                return longhorn_reply(argv)
            return subprocess.CompletedProcess(argv, 28, b"returned bytes" * 10000, b"acknowledgment lost")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
            with self.assertRaises(RuntimeError):
                self.mod.launch("a" * 40, config, root, transport=transport)
            receipts = [json.loads(p.read_text()) for p in root.glob("*.launch-*.json")]
            receipts = [r for r in receipts if r["argv"][0] == "create"]
            self.assertEqual(len(receipts), 1, "a failed launch owes full transport evidence")
            receipt = receipts[0]
            self.assertEqual(base64.b64decode(receipt["stdout_base64"]), b"returned bytes" * 10000)
            self.assertEqual(base64.b64decode(receipt["stderr_base64"]), b"acknowledgment lost")
            self.assertEqual(len(calls), 4)
            self.assertEqual(receipt["exit"], 28)
            self.assertEqual(json.loads(calls[3][1])["kind"], "PersistentVolumeClaim", "a token-bearing init must never start before private fresh storage is acknowledged")

    # Backlog 2346fe79 (2026-10-18): nine workers in one burst each held a
    # 150Gi volume, six failed with no readable cause, and two more could
    # not place storage and sat in Init. The launch abandoned every one.
    def config(self):
        return {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}

    def failing_launch(self, pod_status, events=(), log=b"", log_exit=0):
        calls = []
        objects = {}
        def transport(argv, stdin=None):
            calls.append(argv)
            if argv[:2] == ["get", "pvc"]:
                return subprocess.CompletedProcess(argv, 0, b'{"items": []}', b"")
            if longhorn_reply(argv):
                return longhorn_reply(argv)
            if argv[0] == "create":
                obj = json.loads(stdin)
                obj["metadata"]["uid"] = "pvc-uid" if obj["kind"] == "PersistentVolumeClaim" else "job-uid"
                objects[obj["kind"]] = obj
                return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode(), b"")
            if argv[0] == "patch":
                volume = objects["PersistentVolumeClaim"]
                volume["metadata"]["ownerReferences"] = json.loads(argv[argv.index("-p") + 1])[1]["value"]
                return subprocess.CompletedProcess(argv, 0, json.dumps(volume).encode(), b"")
            if argv[:2] == ["get", "events"]:
                return subprocess.CompletedProcess(argv, 0, json.dumps({"items": list(events)}).encode(), b"")
            if argv[0] == "logs":
                return subprocess.CompletedProcess(argv, log_exit, log, b"" if log_exit == 0 else b"container is waiting to start")
            job = objects["Job"]
            pod = {"metadata": {"name": "bound-pod", "uid": "pod-uid", "labels": copy.deepcopy(job["spec"]["template"]["metadata"]["labels"]), "ownerReferences": [{"kind": "Job", "uid": "job-uid"}]},
                   "spec": job["spec"]["template"]["spec"], "status": pod_status}
            return subprocess.CompletedProcess(argv, 0, json.dumps({"items": [pod]}).encode(), b"")
        retired = []
        def retire(session):
            retired.append((session.job, session.job_uid, session.volume_uid))
            return {"state": "local execution retired"}
        return calls, transport, retired, retire

    def run_failing_launch(self, root, transport, retire, clock=None):
        from unittest.mock import patch
        ticks = iter(clock or [])
        monotonic = (lambda: next(ticks)) if clock else self.mod.time.monotonic
        with patch.object(self.mod.time, "sleep", lambda _: None), patch.object(self.mod.time, "monotonic", monotonic):
            with self.assertRaises(RuntimeError) as raised:
                self.mod.launch("a" * 40, self.config(), root, transport=transport, retire=retire)
        return str(raised.exception)

    def test_worker_volume_is_bounded_and_launch_refuses_past_the_storage_budget(self):
        held = [{"metadata": {"name": "dev-build-%016x" % i}} for i in range(self.mod.WORKER_VOLUME_LIMIT)]
        held.append({"metadata": {"name": "boss-dev-work"}})
        calls = []
        def transport(argv, stdin=None):
            calls.append(argv)
            return subprocess.CompletedProcess(argv, 0, json.dumps({"items": held}).encode(), b"")
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(RuntimeError) as raised:
                self.mod.launch("a" * 40, self.config(), Path(tmp), transport=transport)
        self.assertIn("worker storage budget", str(raised.exception))
        self.assertIn("dev-build-0000000000000005", str(raised.exception))
        self.assertEqual([argv[0] for argv in calls], ["get"], "a refused budget creates nothing")
        self.assertLessEqual(int(self.mod.WORKER_VOLUME.removesuffix("Gi")), 100)
        self.assertLessEqual(int(self.mod.WORKER_VOLUME.removesuffix("Gi")) * self.mod.WORKER_VOLUME_LIMIT, 600)

    # Backlog 8fb56e63 (2026-10-18): the budget above was a read followed
    # by a create, so a burst of starts that all read before any created
    # all passed it, and 2346fe79's incident WAS a burst. Here two starts
    # face five held volumes and each reading start waits (up to a
    # second) for the other to read the same list before it creates.
    def test_two_interleaved_launches_against_five_held_volumes_admit_at_most_one(self):
        import threading
        held = [{"metadata": {"name": "dev-build-%016x" % i}} for i in range(self.mod.WORKER_VOLUME_LIMIT - 1)]
        cluster = threading.Lock()
        both_read = threading.Barrier(2, timeout=1)
        created = []
        def transport(argv, stdin=None):
            if argv[:2] == ["get", "pvc"]:
                with cluster:
                    body = json.dumps({"items": list(held)}).encode()
                try:
                    both_read.wait()
                except threading.BrokenBarrierError:
                    pass
                return subprocess.CompletedProcess(argv, 0, body, b"")
            if longhorn_reply(argv):
                return longhorn_reply(argv)
            obj = json.loads(stdin)
            if argv[0] == "create" and obj["kind"] == "PersistentVolumeClaim":
                obj["metadata"]["uid"] = "pvc-uid-" + obj["metadata"]["name"]
                with cluster:
                    held.append(obj)
                    created.append(obj["metadata"]["name"])
                return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode(), b"")
            # The Job is refused: what is under test ends at the volume.
            return subprocess.CompletedProcess(argv, 1, b"", b"refused by the test")
        refusals = []
        def start(root):
            try:
                self.mod.launch("a" * 40, self.config(), root, transport=transport)
            except RuntimeError as error:
                refusals.append(str(error))
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            starts = [threading.Thread(target=start, args=(root,)) for _ in range(2)]
            for thread in starts:
                thread.start()
            for thread in starts:
                thread.join(timeout=30)
            self.assertFalse(any(thread.is_alive() for thread in starts), "a start never returned")
        self.assertEqual(len(created), 1, "two starts against five held volumes created %s" % created)
        self.assertEqual(len([r for r in refusals if "worker storage budget reached: 6 of 6" in r]), 1, refusals)

    # Backlog a386866e (2026-10-19): the count above admitted six 100Gi
    # volumes, but node-disk-usage w-1 (ca3c5881, 22:12Z) read 928.9 GiB
    # capacity and 603.8 GiB available, and Longhorn keeps 25% free, so
    # 371.6 GiB was placeable: three volumes. The fourth start would have
    # been refused at Longhorn's floor, naming the floor and not the
    # budget. The budget now reads the placeable space; the count stays
    # as a ceiling.
    def budget_launch(self, nodes, held_count, floor="25", longhorn_exit=0):
        held = [{"metadata": {"name": "dev-build-%016x" % i}} for i in range(held_count)]
        held.append({"metadata": {"name": "boss-dev-work"}})
        calls = []
        def transport(argv, stdin=None):
            calls.append(argv)
            if argv[:2] == ["get", "pvc"]:
                return subprocess.CompletedProcess(argv, 0, json.dumps({"items": held}).encode(), b"")
            reply = longhorn_reply(argv, nodes, floor)
            if reply and longhorn_exit:
                return subprocess.CompletedProcess(argv, longhorn_exit, b'{"items": []}', b"Error from server (Forbidden)")
            if reply:
                return reply
            # The volume create is refused: what is under test ends there.
            return subprocess.CompletedProcess(argv, 1, b"", b"refused by the test")
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(RuntimeError) as raised:
                self.mod.launch("a" * 40, self.config(), Path(tmp), transport=transport)
        return str(raised.exception), [argv[0] for argv in calls]

    def test_the_storage_budget_is_the_space_longhorn_can_place_not_a_count(self):
        w1 = longhorn_node("w-1", 928.9, 603.8)
        message, verbs = self.budget_launch([w1], 3)
        self.assertIn("worker storage budget reached", message)
        self.assertIn("w-1", message)
        self.assertIn("371.6 GiB placeable", message)
        self.assertIn(self.mod.WORKER_VOLUME, message)
        self.assertNotIn("create", verbs, "a refused budget creates nothing")
        # Two held leave room for a third: the start passes the budget and
        # reaches the volume create, which this test refuses.
        message, verbs = self.budget_launch([w1], 2)
        self.assertIn("fresh private volume not acknowledged", message)
        self.assertIn("create", verbs)
        # w-2 rejoining with ~930 GiB placeable admits the fourth.
        w2 = longhorn_node("w-2", 1240, 1240)
        message, verbs = self.budget_launch([w1, w2], 3)
        self.assertIn("fresh private volume not acknowledged", message)

    def test_the_floor_is_read_from_longhorn_and_an_unread_budget_refuses(self):
        w1 = longhorn_node("w-1", 928.9, 603.8)
        # At a 10% floor, 510.9 GiB is placeable: five volumes, so three
        # held admit a fourth. The floor is Longhorn's setting, not ours.
        message, verbs = self.budget_launch([w1], 3, floor="10")
        self.assertIn("fresh private volume not acknowledged", message)
        # No evidence is not a pass: a forbidden read refuses the start.
        message, verbs = self.budget_launch([w1], 0, longhorn_exit=1)
        self.assertIn("worker storage budget cannot be measured", message)
        self.assertNotIn("create", verbs)
        message, verbs = self.budget_launch([w1], 0, floor="most")
        self.assertIn("worker storage budget cannot be measured", message)
        self.assertNotIn("create", verbs)

    def test_only_disks_longhorn_may_place_on_count_toward_the_budget(self):
        nodes = [longhorn_node("w-1", 928.9, 603.8),
                 longhorn_node("w-2", 1000, 1000, ready=False),
                 longhorn_node("w-3", 1000, 1000, allow=False),
                 longhorn_node("w-4", 1000, 1000, disk_allow=False),
                 longhorn_node("w-5", 1000, 1000, disk_eviction=True),
                 longhorn_node("w-6", 1000, 100)]
        disks = self.mod.placeable_disks(nodes, 25)
        self.assertEqual([(d["node"], d["placeable"] // GIB) for d in disks], [("w-1", 371), ("w-6", 0)])
        message, verbs = self.budget_launch(nodes[1:5], 0)
        self.assertIn("worker storage budget reached", message)
        self.assertIn("no disk", message)
        self.assertNotIn("create", verbs)

    def test_storage_that_cannot_be_placed_refuses_by_name_keeps_the_cause_and_retires(self):
        status = {"phase": "Pending", "conditions": [{"type": "PodScheduled", "status": "True"}],
                  "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"waiting": {"reason": "PodInitializing"}}}]}
        event = {"reason": "FailedAttachVolume", "type": "Warning", "involvedObject": {"kind": "Pod", "name": "bound-pod"},
                 "message": "AttachVolume.Attach failed for volume \"pvc-1\" : volume pvc-1 is not ready for workloads"}
        calls, transport, retired, retire = self.failing_launch(status, [event], log_exit=1)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            message = self.run_failing_launch(root, transport, retire)
            self.assertIn("storage cannot be placed", message)
            self.assertIn("not ready for workloads", message)
            failures = list(root.glob("dev-build-*.failure.json"))
            self.assertEqual(len(failures), 1)
            failure = self.mod.load_record(failures[0])
            self.assertIn("not ready for workloads", failure["cause"])
            self.assertEqual(failure["pod_status"], status)
            self.assertEqual(failure["events"], [event])
            self.assertEqual(retired, [(failure["name"], "job-uid", "pvc-uid")])
            self.assertFalse((root / (failure["name"] + ".json")).exists(), "a failed launch admits no session")
        self.assertLess(len([a for a in calls if a[:2] == ["get", "pods"]]), 20, "refused before the 300 s deadline")

    def test_a_bootstrap_that_exits_keeps_its_log_and_exit_code_and_retires(self):
        status = {"phase": "Failed", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0,
                  "state": {"terminated": {"exitCode": 128, "reason": "Error"}}}]}
        log = b"fatal: remote error: upload-pack: not our ref " + b"a" * 40 + b"\n"
        calls, transport, retired, retire = self.failing_launch(status, log=log)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            message = self.run_failing_launch(root, transport, retire)
            self.assertIn("published-bootstrap exited 128", message)
            self.assertIn("not our ref", message)
            failure = self.mod.load_record(next(root.glob("dev-build-*.failure.json")))
            kept = self.mod.load_record(root / (failure["name"] + ".bootstrap-log.json"))
            self.assertEqual(base64.b64decode(kept["bootstrap_log_base64"]), log)
            self.assertEqual(kept["bootstrap_log_exit"], 0)
            self.assertEqual(len(retired), 1)
        self.assertIn(["logs", "bound-pod", "-c", "published-bootstrap", "--limit-bytes=65536"], calls)

    def test_a_launch_deadline_names_the_last_observed_state_and_retires(self):
        status = {"phase": "Pending", "conditions": [{"type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                  "message": "0/5 nodes are available: 2 Insufficient cpu."}]}
        calls, transport, retired, retire = self.failing_launch(status)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            message = self.run_failing_launch(root, transport, retire, clock=[0, 1, 2, 400, 400, 400])
            self.assertIn("unavailable after 300 seconds", message)
            self.assertIn("Insufficient cpu", message)
            self.assertEqual(len(retired), 1)
            self.assertTrue(list(root.glob("dev-build-*.failure.json")))

    # Backlog 3165dc62 (2026-10-19): the workers store held 1213
    # launch-*.json receipts, 47 MB, from the 10-18 incident: a launch
    # that waits writes one per 2 s poll, each carrying the whole pod
    # list, and a stuck pod answers the same bytes every time. A repeat
    # of a read's last answer is recorded as a pointer to the receipt
    # that holds those bytes, so every poll stays on the record and a
    # stuck launch stores its pod once.
    def test_a_waiting_launch_stores_an_unchanged_answer_once_and_keeps_every_poll(self):
        status = {"phase": "Pending", "conditions": [{"type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                  "message": "0/5 nodes are available: 2 Insufficient cpu."}]}
        calls, transport, retired, retire = self.failing_launch(status)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.run_failing_launch(root, transport, retire, clock=[0] + [1] * 30 + [400] * 5)
            polls = [a for a in calls if a[:2] == ["get", "pods"]]
            self.assertEqual(len(polls), 30)
            receipts = {p.name: self.mod.load_record(p) for p in root.glob("*.launch-*.json")}
            pods = {n: r for n, r in receipts.items() if r["argv"][:2] == ["get", "pods"]}
            self.assertEqual(len(pods), 30, "every poll keeps a receipt")
            full = [n for n, r in pods.items() if "stdout_base64" in r]
            self.assertEqual(len(full), 1, "an unchanged answer is stored once")
            for name, receipt in pods.items():
                if name in full:
                    continue
                self.assertEqual(receipt["unchanged_from"], full[0])
                self.assertEqual((receipt["completion"], receipt["exit"]), ("observed", 0))
            # Writes are never folded: the Job's creation stays whole.
            creates = [r for r in receipts.values() if r["argv"][0] == "create"]
            self.assertEqual(len(creates), 2)
            self.assertTrue(all("stdout_base64" in r for r in creates))
            self.assertLess(sum(p.stat().st_size for p in root.glob("*.launch-*.json")), 30 * 1024,
                            "thirty polls of one stuck pod no longer store thirty pods")

    def test_a_waiting_pod_is_named_only_at_the_deadline_and_by_its_most_specific_reason(self):
        pulling = {"metadata": {"name": "p"}, "status": {"phase": "Pending",
                   "conditions": [{"type": "Initialized", "status": "False", "reason": "ContainersNotInitialized", "message": "incomplete"}],
                   "initContainerStatuses": [{"name": "published-bootstrap", "state": {"waiting": {"reason": "ImagePullBackOff", "message": "pull denied"}}}]}}
        self.assertIsNone(self.mod.launch_failure(pulling, []))
        self.assertEqual(self.mod.launch_failure(pulling, [], final=True), "published-bootstrap waiting: ImagePullBackOff pull denied")
        started = {"metadata": {"name": "p"}, "status": {"phase": "Pending",
                   "initContainerStatuses": [{"name": "published-bootstrap", "state": {"terminated": {"exitCode": 0}}}],
                   "containerStatuses": [{"name": "worker", "state": {"running": {}}}]}}
        self.assertIsNone(self.mod.launch_failure(started, []), "a bootstrap that succeeded is not a failure")
        self.assertEqual(self.mod.launch_failure(None, [], final=True), "no worker Pod was created")

    # Backlog 8fb56e63 (2026-10-18): events persist for an hour, so an
    # attach warning from before the volume attached still sits beside a
    # pod whose bootstrap is running; that pod's storage WAS placed.
    def test_an_attach_warning_is_fatal_only_while_no_container_has_started(self):
        event = {"reason": "FailedAttachVolume", "type": "Warning",
                 "message": "AttachVolume.Attach failed for volume \"pvc-1\" : volume pvc-1 is not ready for workloads"}
        def pod(*states):
            return {"metadata": {"name": "p"}, "status": {"phase": "Pending",
                    "initContainerStatuses": [{"name": "published-bootstrap", "state": states[0]}],
                    "containerStatuses": [{"name": "worker", "state": state} for state in states[1:]]}}
        waiting = {"waiting": {"reason": "PodInitializing"}}
        self.assertIn("storage cannot be placed", self.mod.launch_failure(pod(waiting), [event]))
        self.assertIsNone(self.mod.launch_failure(pod({"running": {}}), [event]), "a running bootstrap has its volume")
        self.assertIsNone(self.mod.launch_failure(pod({"terminated": {"exitCode": 0}}, waiting), [event]), "a finished bootstrap had its volume")

    def test_a_failed_retirement_still_keeps_and_reports_the_cause(self):
        status = {"phase": "Failed", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0,
                  "state": {"terminated": {"exitCode": 1, "reason": "Error"}}}]}
        calls, transport, retired, _ = self.failing_launch(status)
        def retire(session):
            raise RuntimeError("UID-conditioned Job retirement refused by API")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            message = self.run_failing_launch(root, transport, retire)
            self.assertIn("published-bootstrap exited 1", message)
            self.assertIn("retirement refused by API", message)
            self.assertTrue(list(root.glob("dev-build-*.failure.json")))

    # Backlog bc8db3ba (2026-10-18), reproduced by review 68a60879 of car
    # 83dbb962: a start whose self-retirement failed printed a cause and a
    # path, and the `dev-build stop` it implied refused with File exists.
    def test_a_refused_start_whose_retirement_failed_is_retired_by_stop(self):
        status = {"phase": "Failed", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0,
                  "state": {"terminated": {"exitCode": 128, "reason": "Error"}}}]}
        calls, transport, _, _ = self.failing_launch(status)
        deletes = []
        def delete(name, uid, record):
            deletes.append((name, uid))
            record({"completion": "observed"})
            if len(deletes) == 1:
                raise RuntimeError("API unreachable")
            return {"kind": "Status", "status": "Success"}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            message = self.run_failing_launch(root, transport, lambda session: self.mod.retire_worker(session, root, delete))
            name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
            self.assertIn("API unreachable", message)
            self.assertIn("`dev-build stop %s`" % name, message, "a start that could not retire names the handle")
            session = self.mod.stoppable_session(name, root)
            self.assertEqual(self.mod.retire_worker(session, root, delete)["state"], "local execution retired")
            self.assertEqual(deletes, [(name, "job-uid")] * 2)

    def test_every_refusal_after_the_job_is_acknowledged_names_the_session_and_keeps_the_cause(self):
        waiting = {"phase": "Pending", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"waiting": {"reason": "PodInitializing"}}}]}
        exited = {"phase": "Failed", "initContainerStatuses": [{"name": "published-bootstrap", "restartCount": 0, "state": {"terminated": {"exitCode": 128, "reason": "Error"}}}]}
        def pods_read_fails(argv):
            return subprocess.CompletedProcess(argv, 1, b"", b"connection refused") if argv[:2] == ["get", "pods"] else None
        def two_pods(argv):
            if argv[:2] == ["get", "pods"]:
                return subprocess.CompletedProcess(argv, 0, json.dumps({"items": [{"metadata": {"name": "a"}}, {"metadata": {"name": "b"}}]}).encode(), b"")
        def events_read_fails(argv):
            return subprocess.CompletedProcess(argv, 1, b"", b"connection refused") if argv[:2] == ["get", "events"] else None
        def logs_time_out(argv):
            if argv[0] == "logs":
                raise subprocess.TimeoutExpired(argv, 120, output=b"partial log")
        modes = {"poll read": (waiting, pods_read_fails, "observation failed"),
                 "multiple pods": (waiting, two_pods, "multiple Pods"),
                 "events read": (waiting, events_read_fails, "observation failed"),
                 "bootstrap log timeout": (exited, logs_time_out, "published-bootstrap exited 128")}
        for mode, (status, fault, cause) in modes.items():
            for retires in (False, True):
                with self.subTest(mode=mode, retires=retires), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    _, inner, retired, succeed = self.failing_launch(status)
                    def transport(argv, stdin=None, fault=fault, inner=inner):
                        return fault(argv) or inner(argv, stdin)
                    def fail(session):
                        raise RuntimeError("API unreachable")
                    message = self.run_failing_launch(root, transport, succeed if retires else fail)
                    name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
                    self.assertIn(name, message, "the refusal names its session")
                    self.assertIn(cause, message)
                    failure = self.mod.load_record(root / (name + ".failure.json"))
                    self.assertIn(cause, failure["cause"], "the cause is kept before anything that can time out")
                    if retires:
                        self.assertEqual(retired, [(name, "job-uid", "pvc-uid")])
                    else:
                        self.assertIn("`dev-build stop %s`" % name, message)

    def test_stop_finds_an_unadmitted_worker_by_its_acknowledged_launch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, _, calls, _ = self.recovery_fixture(root)
            session = self.mod.stoppable_session(name, root)
            self.assertEqual((session.job, session.job_uid, session.volume_uid, session.source_head), (name, "job-uid", "volume-uid", "a" * 40))
            self.assertEqual(calls, [], "finding the record reads no cluster state")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, _, _, _ = self.recovery_fixture(root, acknowledged=False)
            with self.assertRaisesRegex(ValueError, str(root)):
                self.mod.stoppable_session(name, root)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, _, _, _ = self.recovery_fixture(root)
            admitted = self.mod.Session(name, "admitted-uid", "pod", "pod-uid", "container-id", "a" * 40, "volume-uid")
            self.mod.save_record(root / (name + ".json"), self.mod.asdict(admitted))
            self.assertEqual(self.mod.stoppable_session(name, root), admitted, "an admitted session's own record wins")

    def test_stop_retires_a_matching_job_after_its_create_answer_was_lost(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, objects, calls, transport = self.recovery_fixture(root, acknowledged=False)
            failed = {"argv": ["create", "-f", "-", "-o", "json"], "completion": "observed",
                      "exit": 1, "stdout_base64": "", "stderr_base64": base64.b64encode(b"etcdserver: request timed out").decode()}
            self.mod.save_record(root / (name + ".launch-timeout.json"), failed)
            retired = []
            def retire(session):
                retired.append(session)
                return self.mod.retire_worker(session, root, lambda *args: {"kind": "Status", "status": "Success"})
            result = self.mod.stop(name, root, retire, transport=transport)
            self.assertTrue(result["job_delete_acknowledged"])
            self.assertEqual([(s.job_uid, s.volume_uid) for s in retired], [("job-uid", "volume-uid")])
            self.assertEqual(self.mod.load_record(root / (name + ".launch-timeout.json")), failed)
            adoption = self.mod.load_record(root / (name + ".stop-adoption.json"))
            self.assertFalse(adoption["creation_acknowledged"])
            self.assertEqual(adoption["session"]["job_uid"], "job-uid")
            objects["job"]["metadata"]["uid"] = "replacement"
            with self.assertRaisesRegex(RuntimeError, "changed"):
                self.mod.stop(name, root, retire, transport=transport)
            self.assertEqual(retired[-1].job_uid, "job-uid", "retry must never adopt a replacement")
            with self.assertRaises(ValueError):
                self.mod.recover_start(name, root, transport=transport)

    def test_stop_observed_adoption_refuses_changed_job_volume_or_unread_identity(self):
        changes = {
            "job namespace": lambda o: o["job"]["metadata"].update(namespace="other"),
            "job spec": lambda o: o["job"]["spec"]["template"]["spec"].update(hostNetwork=True),
            "job labels": lambda o: o["job"]["metadata"]["labels"].update({"boss.dev/purpose": "other"}),
            "volume uid": lambda o: o["volume"]["metadata"].update(uid="replacement"),
            "volume owner": lambda o: o["volume"]["metadata"]["ownerReferences"][0].update(uid="other"),
            "extra owner": lambda o: o["volume"]["metadata"]["ownerReferences"].append({"kind": "Job", "uid": "other"}),
            "missing job uid": lambda o: o["job"]["metadata"].pop("uid"),
            "volume namespace": lambda o: o["volume"]["metadata"].update(namespace="other"),
        }
        for case, change in changes.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, _, transport = self.recovery_fixture(root, acknowledged=False, mutate=change)
                with self.assertRaises((ValueError, RuntimeError)):
                    self.mod.stop(name, root, lambda _: self.fail("unsafe retirement"), transport=transport)
                self.assertFalse((root / (name + ".stop-adoption.json")).exists())

    def test_stop_adoption_keeps_first_uid_when_delete_answer_is_lost(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, objects, calls, transport = self.recovery_fixture(root, acknowledged=False)
            deleted = []
            def delete(job, uid, record):
                deleted.append(uid)
                if len(deleted) == 1:
                    raise subprocess.TimeoutExpired(["delete", job], 120)
                raise RuntimeError("UID precondition rejected replacement")
            def retire(session):
                return self.mod.retire_worker(session, root, delete)
            with self.assertRaises(subprocess.TimeoutExpired):
                self.mod.stop(name, root, retire, transport=transport)
            objects["job"]["metadata"]["uid"] = "replacement"
            with self.assertRaisesRegex(RuntimeError, "changed"):
                self.mod.stop(name, root, retire, transport=transport)
            self.assertEqual(deleted, ["job-uid"], "the replacement is refused before another delete")

    def test_stop_adoption_refuses_unread_identity_without_retirement(self):
        for kind in ("job", "pvc"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, _, inner = self.recovery_fixture(root, acknowledged=False)
                def unread(argv, stdin=None):
                    if argv[:2] == ["get", kind]:
                        return subprocess.CompletedProcess(argv, 1, b"", b"API unavailable")
                    return inner(argv, stdin)
                with self.assertRaises(RuntimeError):
                    self.mod.stop(name, root, lambda _: self.fail("unread must not retire"), transport=unread)
                self.assertFalse((root / (name + ".stop-adoption.json")).exists())

    # Review d7990404 measured this via launch(), not recovery_fixture:
    # create may commit without ACK, BEFORE launch can bind PVC ownership.
    def committed_unacknowledged_launch(self, root, how):
        state, calls = {}, []
        def launching(argv, stdin=None):
            if argv[:2] == ["get", "pvc"]:
                return subprocess.CompletedProcess(argv, 0, b'{"items": []}', b"")
            if longhorn_reply(argv):
                return longhorn_reply(argv)
            obj = json.loads(stdin)
            if obj["kind"] == "Job":
                state["job"] = copy.deepcopy(obj)
                state["job"]["metadata"]["uid"] = "committed-job-uid"
                if how == "timeout":
                    raise subprocess.TimeoutExpired(argv, 120)
                return subprocess.CompletedProcess(argv, 1, b"", b"etcdserver: request timed out")
            obj["metadata"].update(uid="pvc-uid", resourceVersion="7")
            state["volume"] = copy.deepcopy(obj)
            return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode(), b"")
        with self.assertRaises(RuntimeError):
            self.mod.launch("a" * 40, self.config(), root, transport=launching,
                            retire=lambda _: self.fail("unknown create cannot retire during launch"))
        name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
        self.assertFalse(state["volume"]["metadata"].get("ownerReferences"))
        self.assertTrue(self.mod.native_job_matches(state["job"], self.mod.retained_intent(name, root)["job"]))
        def cluster(argv, stdin=None):
            calls.append(argv)
            if argv[:2] in (["get", "job"], ["get", "pvc"]):
                obj = state["job" if argv[1] == "job" else "volume"]
                return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode() if obj else b"", b"")
            if argv[:2] == ["patch", "pvc"]:
                patch = json.loads(argv[argv.index("-p") + 1])
                meta = state["volume"]["metadata"]
                self.assertEqual(patch[:2], [{"op":"test", "path":"/metadata/uid", "value":meta["uid"]},
                                            {"op":"test", "path":"/metadata/resourceVersion", "value":meta["resourceVersion"]}])
                meta["ownerReferences"] = patch[2]["value"]
                meta["resourceVersion"] = str(int(meta["resourceVersion"]) + 1)
                return subprocess.CompletedProcess(argv, 0, json.dumps(state["volume"]).encode(), b"")
            self.fail("unexpected lifecycle call %s" % argv)
        return name, state, calls, cluster

    def test_real_missing_create_ack_binds_unowned_volume_before_uid_retirement(self):
        for how in ("exit1", "timeout"):
            with self.subTest(how=how), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, state, calls, cluster = self.committed_unacknowledged_launch(root, how)
                originals = {p.name:p.read_bytes() for p in root.glob(name + ".launch-*.json")}
                deleted = []
                def delete(job, uid, record):
                    self.assertEqual(state["volume"]["metadata"]["ownerReferences"],
                                     [{"apiVersion":"batch/v1", "kind":"Job", "name":name, "uid":uid}])
                    deleted.append(uid)
                    state["job"] = None
                    return {"kind":"Status", "status":"Success"}
                def retire(session):
                    return self.mod.retire_worker(session, root, delete)
                result = self.mod.stop(name, root, retire, transport=cluster)
                self.assertTrue(result["volume_handed_to_gc"])
                self.assertEqual(deleted, ["committed-job-uid"])
                self.assertFalse(self.mod.load_record(root / (name + ".stop-adoption.json"))["creation_acknowledged"])
                self.assertEqual({p.name:p.read_bytes() for p in root.glob(name + ".launch-*.json")}, originals)
                self.mod.stop(name, root, retire, transport=cluster)
                self.assertEqual(len(deleted), 1)
                self.assertEqual(sum(a[0] == "patch" for a in calls), 1)
                with self.assertRaises(ValueError):
                    self.mod.recover_start(name, root, transport=cluster)

    def test_missing_create_ack_job_never_appears_keeps_existing_volume_release(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name, state, _, cluster = self.committed_unacknowledged_launch(root, "timeout")
            state["job"] = None
            result = self.mod.stop(name, root, lambda _:self.fail("no Job to retire"), transport=cluster)
            self.assertEqual(result["state"], "volume released to garbage collection")
            self.assertTrue(state["volume"]["metadata"]["ownerReferences"])
            self.assertFalse((root / (name + ".stop-adoption.json")).exists())

    def test_unowned_adoption_retries_lost_owner_patch_without_rebinding_replacements(self):
        for mode in ("committed", "not-committed", "job-replaced", "pvc-replaced", "foreign-owner"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, state, calls, inner = self.committed_unacknowledged_launch(root, "timeout")
                deletes, patches = [], []
                def lost_patch(argv, stdin=None):
                    if argv[:2] == ["patch", "pvc"]:
                        patches.append(argv)
                        # Identity must be pinned BEFORE an uncertain write.
                        self.assertEqual(self.mod.load_record(root / (name + ".stop-adoption.json"))["session"]["job_uid"], "committed-job-uid")
                        if mode != "not-committed":
                            inner(argv, stdin)
                        raise subprocess.TimeoutExpired(argv, 120)
                    return inner(argv, stdin)
                def delete(job, uid, record):
                    deletes.append(uid)
                    state["job"] = None
                    return {"kind":"Status", "status":"Success"}
                def retire(session):
                    return self.mod.retire_worker(session, root, delete)
                with self.assertRaises(subprocess.TimeoutExpired):
                    self.mod.stop(name, root, retire, transport=lost_patch)
                self.assertEqual(deletes, [], "no owner ACK, no delete")
                if mode == "job-replaced": state["job"]["metadata"]["uid"] = "replacement"
                if mode == "pvc-replaced": state["volume"]["metadata"]["uid"] = "replacement"
                if mode == "foreign-owner": state["volume"]["metadata"]["ownerReferences"][0]["uid"] = "foreign"
                if mode in ("job-replaced", "pvc-replaced", "foreign-owner"):
                    with self.assertRaises(RuntimeError):
                        self.mod.stop(name, root, retire, transport=inner)
                    self.assertEqual(deletes, [])
                else:
                    result = self.mod.stop(name, root, retire, transport=inner)
                    self.assertTrue(result["volume_handed_to_gc"])
                    self.assertEqual(deletes, ["committed-job-uid"])
                    self.assertEqual(sum(a[0] == "patch" for a in calls), 1)

    def test_unowned_adoption_lost_delete_answer_preserves_owner_and_original_uid(self):
        for replacement in (False, True):
            with self.subTest(replacement=replacement), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, state, calls, cluster = self.committed_unacknowledged_launch(root, "exit1")
                deleted = []
                def delete(job, uid, record):
                    deleted.append(uid)
                    if len(deleted) == 1:
                        state["job"] = None
                        raise subprocess.TimeoutExpired(["delete", job], 120)
                    return self.notfound(name)
                def retire(session):
                    return self.mod.retire_worker(session, root, delete)
                original = copy.deepcopy(state["job"])
                with self.assertRaises(subprocess.TimeoutExpired):
                    self.mod.stop(name, root, retire, transport=cluster)
                if replacement:
                    original["metadata"]["uid"] = "replacement"
                    state["job"] = original
                    with self.assertRaisesRegex(RuntimeError, "changed"):
                        self.mod.stop(name, root, retire, transport=cluster)
                    self.assertEqual(deleted, ["committed-job-uid"])
                else:
                    result = self.mod.stop(name, root, retire, transport=cluster)
                    self.assertTrue(result["job_absent"])
                    self.assertTrue(result["volume_handed_to_gc"])
                    self.assertEqual(deleted, ["committed-job-uid", "committed-job-uid"])
                self.assertEqual(sum(a[0] == "patch" for a in calls), 1)

    def test_unowned_adoption_unread_recheck_cannot_patch_or_delete(self):
        for kind in ("job", "pvc"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                name, _, calls, inner = self.committed_unacknowledged_launch(root, "timeout")
                reads = 0
                def unread(argv, stdin=None):
                    nonlocal reads
                    if argv[:2] == ["get", kind]:
                        reads += 1
                        if reads == 2:
                            return subprocess.CompletedProcess(argv, 1, b"", b"API unread")
                    return inner(argv, stdin)
                with self.assertRaises(RuntimeError):
                    self.mod.stop(name, root, lambda _:self.fail("unread must not delete"), transport=unread)
                self.assertFalse(any(a[0] == "patch" for a in calls))
                self.assertFalse((root / (name + ".stop-volume-bound.json")).exists())

    # Backlog 86bd9fb8 (review ca8d3763 of car 559cfe13, 2026-10-19): a
    # Job create that failed after the PVC create left a 100Gi volume with
    # no owner. It counted toward WORKER_VOLUME_LIMIT, and the refusal
    # that named it said `dev-build stop <name>` releases one, while stop
    # refused it: no acknowledged Job. These pin the release.
    def unowned_volume_cluster(self, job=None, owners=None, present=True):
        """A cluster holding one unowned worker volume and, maybe, a Job."""
        calls, state = [], {"volume": None}
        def volume(name):
            if state["volume"] is None:
                state["volume"] = {"apiVersion": "v1", "kind": "PersistentVolumeClaim",
                                   "metadata": {"name": name, "namespace": "boss-dev", "uid": "pvc-uid",
                                                "resourceVersion": "7", "ownerReferences": owners or []}}
            return state["volume"]
        def transport(argv, stdin=None):
            calls.append(argv)
            if argv[:2] == ["get", "job"]:
                self.assertIn("--ignore-not-found", argv)
                return subprocess.CompletedProcess(argv, 0, json.dumps(job).encode() if job else b"", b"")
            if argv[:2] == ["get", "pvc"]:
                self.assertIn("--ignore-not-found", argv)
                return subprocess.CompletedProcess(argv, 0, json.dumps(volume(argv[2])).encode() if present else b"", b"")
            if argv[:2] == ["patch", "pvc"]:
                patch = json.loads(argv[argv.index("-p") + 1])
                self.assertEqual(patch[:2], [{"op": "test", "path": "/metadata/uid", "value": "pvc-uid"},
                                             {"op": "test", "path": "/metadata/resourceVersion", "value": "7"}])
                current = volume(argv[2])
                current["metadata"]["ownerReferences"] = patch[2]["value"]
                current["metadata"]["resourceVersion"] = "8"
                return subprocess.CompletedProcess(argv, 0, json.dumps(current).encode(), b"")
            self.fail("unexpected cluster call %s" % argv)
        return calls, transport, state

    def job_create_fails(self, argv, stdin=None):
        if argv[:2] == ["get", "pvc"]:
            return subprocess.CompletedProcess(argv, 0, b'{"items": []}', b"")
        if longhorn_reply(argv):
            return longhorn_reply(argv)
        obj = json.loads(stdin)
        if obj["kind"] == "Job":
            return subprocess.CompletedProcess(argv, 1, b"", b"admission webhook denied the request")
        obj["metadata"]["uid"] = "pvc-uid"
        return subprocess.CompletedProcess(argv, 0, json.dumps(obj).encode(), b"")

    def test_a_job_create_failure_leaves_a_volume_that_stop_releases(self):
        def retire(session):
            self.fail("a launch with no acknowledged Job has no Job to retire")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(RuntimeError) as raised:
                self.mod.launch("a" * 40, self.config(), root, transport=self.job_create_fails, retire=retire)
            name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
            self.assertIn("`dev-build stop %s`" % name, str(raised.exception), "the refusal names the handle that releases the volume")
            calls, cluster, state = self.unowned_volume_cluster()
            released = self.mod.stop(name, root, retire, transport=cluster)
            self.assertEqual(released["state"], "volume released to garbage collection")
            self.assertTrue(released["volume_uid_acknowledged"])
            owner = state["volume"]["metadata"]["ownerReferences"]
            self.assertEqual(len(owner), 1)
            self.assertEqual((owner[0]["apiVersion"], owner[0]["kind"], owner[0]["name"]), ("batch/v1", "Job", name))
            self.assertNotIn(owner[0]["uid"], ("pvc-uid", ""), "the owner names a Job that was never created")
            self.assertEqual([argv[:2] for argv in calls], [["get", "job"], ["get", "pvc"], ["patch", "pvc"]])
            self.assertEqual(self.mod.load_record(root / (name + ".volume-released.json")), released)
            self.assertEqual(self.mod.stop(name, root, retire, transport=cluster), released, "a second stop answers its record")
            self.assertEqual(len(calls), 3)

    def test_a_volume_release_whose_answer_was_lost_is_finished_by_the_next_stop(self):
        def retire(session):
            self.fail("no Job to retire")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaises(RuntimeError):
                self.mod.launch("a" * 40, self.config(), root, transport=self.job_create_fails, retire=retire)
            name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
            calls, cluster, state = self.unowned_volume_cluster()
            def lost(argv, stdin=None):
                result = cluster(argv, stdin)
                if argv[0] == "patch":
                    raise subprocess.TimeoutExpired(argv, 120)
                return result
            with self.assertRaises(subprocess.TimeoutExpired):
                self.mod.stop(name, root, retire, transport=lost)
            released = self.mod.stop(name, root, retire, transport=cluster)
            self.assertEqual(released["state"], "volume released to garbage collection")
            self.assertEqual([argv[0] for argv in calls], ["get", "get", "patch", "get", "get"], "the release owner already on the volume is not patched twice")

    def test_stop_refuses_to_release_a_volume_a_job_or_another_owner_holds(self):
        def retire(session):
            self.fail("no Job to retire")
        cases = {"a Job of the name exists": ({"kind": "Job", "metadata": {"name": "x", "uid": "unacknowledged"}}, None, "Job"),
                 "the volume has an owner": (None, [{"apiVersion": "batch/v1", "kind": "Job", "name": "x", "uid": "other"}], "owner")}
        for case, (job, owners, said) in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                with self.assertRaises(RuntimeError):
                    self.mod.launch("a" * 40, self.config(), root, transport=self.job_create_fails, retire=retire)
                name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
                calls, cluster, _ = self.unowned_volume_cluster(job=job, owners=owners)
                with self.assertRaisesRegex(RuntimeError, said):
                    self.mod.stop(name, root, retire, transport=cluster)
                self.assertNotIn("patch", [argv[0] for argv in calls])
                self.assertFalse((root / (name + ".volume-released.json")).exists())

    def test_a_timed_out_volume_create_names_the_volume_and_stop_releases_what_it_left(self):
        def retire(session):
            self.fail("no Job to retire")
        def pvc_create_times_out(argv, stdin=None):
            if argv[0] == "create":
                raise subprocess.TimeoutExpired(argv, 120, output=b"")
            return self.job_create_fails(argv, stdin)
        for present in (True, False):
            with self.subTest(volume_left=present), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                with self.assertRaises(RuntimeError) as raised:
                    self.mod.launch("a" * 40, self.config(), root, transport=pvc_create_times_out, retire=retire)
                name = next(root.glob("dev-build-*.intent.json")).name.removesuffix(".intent.json")
                self.assertIn("volume %s" % name, str(raised.exception), "a timed-out create names the volume it may have left")
                self.assertIn("`dev-build stop %s`" % name, str(raised.exception))
                calls, cluster, _ = self.unowned_volume_cluster(present=present)
                released = self.mod.stop(name, root, retire, transport=cluster)
                if present:
                    self.assertEqual(released["state"], "volume released to garbage collection")
                    self.assertFalse(released["volume_uid_acknowledged"], "the UID was observed, never acknowledged by the create")
                else:
                    self.assertEqual(released["state"], "no volume of this name exists; nothing held")
                    self.assertEqual([argv[0] for argv in calls], ["get", "get"])

    def launch_transport(self, replacement=False):
        import json
        calls = []
        objects = {}
        def transport(argv, stdin=None):
            calls.append(argv)
            if argv[:2] == ["get", "pvc"]:
                return subprocess.CompletedProcess(argv, 0, b'{"items": []}', b"")
            if longhorn_reply(argv):
                return longhorn_reply(argv)
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
            pod = {"metadata": {"name": "bound-pod", "uid": "pod-uid", "labels": copy.deepcopy(job["spec"]["template"]["metadata"]["labels"]), "ownerReferences": [{"kind": "Job", "uid": "job-uid"}]},
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
            self.assertEqual([argv[0] for argv in calls], ["get", "get", "get", "create", "create", "patch", "get"])

    def test_replaced_volume_patch_refuses_before_worker_admission(self):
        calls, transport = self.launch_transport(replacement=True)
        with tempfile.TemporaryDirectory() as tmp:
            config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
            with self.assertRaises(RuntimeError):
                self.mod.launch("a" * 40, config, Path(tmp), transport=transport)
            self.assertEqual([argv[0] for argv in calls], ["get", "get", "get", "create", "create", "patch"])

    def test_a_receipt_fifo_is_refused_without_blocking_the_controller(self):
        import os
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "receipt.json"
            os.mkfifo(path, 0o600)
            original_open = os.open

            def nonblocking_open(receipt, flags, *args, **kwargs):
                # Judge the open itself, before a missing flag can block;
                # child interpreter startup is not the controller's behavior.
                self.assertEqual(receipt, path)
                self.assertTrue(flags & os.O_NONBLOCK, "receipt FIFO must be opened nonblocking")
                return original_open(receipt, flags, *args, **kwargs)

            with patch.object(self.mod.os, "open", side_effect=nonblocking_open) as opened:
                with self.assertRaisesRegex(ValueError, "controller record ownership or mode is unsafe"):
                    self.mod.load_record(path)
                opened.assert_called_once()

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
