import importlib.util
import os
import unittest
from pathlib import Path
import json
import subprocess
import tempfile
import shlex
import uuid
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("agent_io", Path(__file__).with_name("agent-io.py"))
io = importlib.util.module_from_spec(spec)
spec.loader.exec_module(io)


class FenceTests(unittest.TestCase):
    def setUp(self):
        # Synthetic PodUID only: the other three facts come from this kernel.
        os.environ["AGENT_POD_UID"] = "synthetic-pod-uid"

    def test_exact_kernel_incarnation_is_accepted_and_replacements_refuse(self):
        original = io.identity()
        self.assertGreater(original["pid1_start_ticks"], 0)
        self.assertGreater(original["pid_namespace_inode"], 0)
        self.assertEqual(io.require_fence(original), original)
        for field in original:
            changed = dict(original)
            changed[field] = "other" if isinstance(changed[field], str) else changed[field] + 1
            with self.assertRaises(ValueError, msg=field):
                io.require_fence(changed)

    def test_managed_cache_without_bound_private_plan_refuses_before_any_login_path(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-cache-{os.getuid()}-{os.getpid()}-", dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            prompt = b"synthetic prompt"
            io.private_write(root / "prompt.txt", prompt)
            request = {"prompt_sha256": io.digest(prompt), "argv": ["/usr/local/bin/codex"],
                       "snapshot": {}, "login_file": "auth.json", "home_env": "CODEX_HOME"}
            with patch.object(io, "import_snapshot", return_value=root), patch.object(io.os, "symlink") as link:
                with self.assertRaises(ValueError):
                    io.run_harness(request, root, lambda: 5)
            link.assert_not_called()

    def cache_plan(self):
        return {"schema": 1, "run": str(uuid.uuid4()), "nonce": str(uuid.uuid4()),
                "credential": "fixture-login", "issuer": "fixture-issuer", "principal": "fixture-principal",
                "generation": "fixture-generation", "binding": {"job_uid": "fixture-job", "pod": "fixture-pod",
                "pod_uid": "synthetic-pod-uid", "container_id": "containerd://fixture"}}

    def test_managed_materialization_is_private_writable_and_never_writes_issuer(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-managed-{os.getuid()}-{os.getpid()}-", dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            issuer, state = root / "issuer", root / "state"
            issuer.mkdir(mode=0o700)
            state.mkdir(mode=0o700)
            original = b'{"synthetic_private_fixture":"original"}'
            io.private_write(issuer / "auth.json", original)
            os.chmod(issuer / "auth.json", 0o400)
            plan = self.cache_plan()
            request = {"run": plan["run"], "managed_cache": plan, "login_file": "auth.json", "home_env": "CODEX_HOME"}
            io.prepare_managed_login(request, state, issuer)
            self.assertFalse((state / "auth.json").is_symlink())
            self.assertEqual((state / "auth.json").stat().st_mode & 0o777, 0o600)
            (state / "auth.json").write_bytes(b'{"synthetic_private_fixture":"renewed"}')
            self.assertEqual((issuer / "auth.json").read_bytes(), original)
            with self.assertRaises(ValueError):
                io.prepare_managed_login(request, state, issuer)

    def test_projected_readonly_issuer_is_pinned_and_arbitrary_links_are_refused(self):
        with tempfile.TemporaryDirectory(prefix="agent-projection-", dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            issuer = root / "issuer"
            issuer.mkdir(mode=0o700)
            version = issuer / "..fixture-version"
            version.mkdir(mode=0o700)
            io.private_write(version / "auth.json", b"synthetic-private-fixture")
            os.chmod(version / "auth.json", 0o400)
            (issuer / "..data").symlink_to(version.name)
            (issuer / "auth.json").symlink_to("..data/auth.json")
            self.assertEqual(io.issuer_bytes(issuer), b"synthetic-private-fixture")
            (issuer / "..data").unlink()
            (issuer / "..data").symlink_to("../outside-private-fixture")
            with self.assertRaises((OSError, ValueError)):
                io.issuer_bytes(issuer)
            (issuer / "auth.json").unlink()
            (issuer / "auth.json").symlink_to(version / "auth.json")
            with self.assertRaises((OSError, ValueError)):
                io.issuer_bytes(issuer)

    def test_private_materialization_refuses_hardlinks_special_files_and_foreign_state(self):
        with tempfile.TemporaryDirectory(prefix="agent-cache-refusal-", dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            issuer, state = root / "issuer", root / "state"
            issuer.mkdir(mode=0o700)
            state.mkdir(mode=0o700)
            plan = self.cache_plan()
            request = {"run":plan["run"], "managed_cache":plan, "login_file":"auth.json", "home_env":"CODEX_HOME"}
            for kind in ["hardlink", "fifo", "writable", "state-permission", "state-symlink"]:
                leaf = issuer / "auth.json"
                if leaf.exists():
                    leaf.unlink()
                if kind == "fifo":
                    os.mkfifo(leaf)
                else:
                    io.private_write(leaf, b"synthetic-private-fixture")
                    os.chmod(leaf, 0o400)
                if kind == "hardlink":
                    os.link(leaf, root / "other-link")
                if kind == "writable":
                    leaf.chmod(0o600)
                target = state
                if kind == "state-permission":
                    state.chmod(0o755)
                if kind == "state-symlink":
                    target = root / "linked-state"
                    target.symlink_to(state)
                with self.assertRaisesRegex(ValueError, "^private managed cache preparation refused$"):
                    io.prepare_managed_login(request, target, issuer)
                state.chmod(0o700)
                self.assertFalse((state / "auth.json").exists())

    def test_unbound_or_foreign_private_plan_refuses_before_issuer_read(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-plan-{os.getuid()}-{os.getpid()}-", dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            root.chmod(0o700)
            for bad in ["run", "nonce", "pod_uid", "principal", "extra"]:
                plan = self.cache_plan()
                request = {"run": plan["run"], "managed_cache": plan, "login_file": "auth.json", "home_env": "CODEX_HOME"}
                if bad == "run":
                    request["run"] = str(uuid.uuid4())
                elif bad == "nonce":
                    plan["nonce"] = str(uuid.UUID(int=0))
                elif bad == "pod_uid":
                    plan["binding"]["pod_uid"] = "replacement"
                elif bad == "principal":
                    plan["principal"] = ""
                else:
                    plan["issuer_path"] = "/untrusted/path"
                with patch.object(io, "read_regular") as read:
                    with self.assertRaises(ValueError):
                        io.prepare_managed_login(request, root, root / "missing")
                    read.assert_not_called()

    def test_private_plan_schema_is_an_integer_not_a_python_boolean_or_float(self):
        for schema in [True, 1.0, "1", None]:
            plan = self.cache_plan()
            plan["schema"] = schema
            with patch.dict(os.environ, {"AGENT_POD_UID": plan["binding"]["pod_uid"]}):
                with self.assertRaises(ValueError):
                    io.managed_plan({"run": plan["run"], "managed_cache": plan})

    def test_image_entrypoint_ignores_candidate_python_site_before_the_fence(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-io-{os.getuid()}-{os.getpid()}-",dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            marker = root / "candidate-code-ran"
            (root / "sitecustomize.py").write_text("from pathlib import Path\nPath(" + repr(str(marker)) + ").write_text('unsafe')\n")
            argv = shlex.split(Path(io.__file__).read_text().splitlines()[0][2:])
            answer = subprocess.run([*argv, io.__file__, "identity"], input=b"", capture_output=True,
                                    env={"AGENT_POD_UID": "synthetic-pod-uid", "PYTHONPATH": str(root)})
            self.assertEqual(answer.returncode, 0, answer.stderr)
            self.assertEqual(json.loads(answer.stdout)["pod_uid"], "synthetic-pod-uid")
            self.assertFalse(marker.exists(), "candidate Python startup code ran before the trusted image fence")

    def test_original_binary_artifacts_and_missing_files_survive_interrupted_recovery(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-artifact-{os.getuid()}-{os.getpid()}-",dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            (root / "artifacts").mkdir()
            original = b"stderr-before-interruption\x00\xff\n"
            io.private_write(root / "artifacts/native.stderr", original)
            answer = io.recover(root, {"runtime_event": "thread.started", "runtime_key": "thread_id",
                                       "transcript_pattern": "sessions/*/*/*/rollout-*-{runtime}.jsonl"})
            self.assertEqual(answer["completion"], "interrupted")
            self.assertEqual(io.base64.b64decode(answer["artifacts"]["stderr"]["data"]), original)
            self.assertIn("stdout", answer["missing"])
            self.assertIn("native_metadata", answer["missing"])
            self.assertIn("transcript", answer["missing"])

    def test_links_special_files_and_oversized_artifacts_are_refused_with_a_regular_positive(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-files-{os.getuid()}-{os.getpid()}-",dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            io.private_write(root / "regular", b"original")
            self.assertEqual(io.read_regular(root / "regular", 8), b"original")
            (root / "linked").symlink_to(root / "regular")
            os.mkfifo(root / "special")
            for path, limit in [(root / "linked", 8), (root / "special", 8), (root / "regular", 3)]:
                with self.assertRaises((OSError, ValueError)):
                    io.read_regular(path, limit)

    def test_thread_started_is_not_an_acknowledgment_and_exact_assistant_ack_is_required(self):
        request = {"run": "run", "prompt_sha256": "a" * 64, "snapshot": {"sha256": "b" * 64}}
        ack = {"schema": 1, "run": "run", "prompt_sha256": "a" * 64, "snapshot_sha256": "b" * 64, "start_read": True}
        raw = json.dumps({"type": "thread.started", "thread_id": "runtime"}).encode()
        with self.assertRaises(ValueError):
            io.event_ack(raw, request)
        raw += b"\n" + json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": "BOSS_START_ACK " + json.dumps(ack)}}).encode()
        self.assertEqual(io.event_ack(raw, request), ack)
        with self.assertRaises(ValueError):
            io.event_ack(raw + b"\n" + raw, request)

    def test_exact_bundle_import_retains_diverged_main_without_host_remotes_or_config(self):
        with tempfile.TemporaryDirectory(prefix=f"agent-git-{os.getuid()}-{os.getpid()}-",dir=os.environ.get("BOSS_TEST_SCRATCH")) as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            def git(*args):
                return subprocess.check_output(["git", "-C", str(source), *args], stderr=subprocess.PIPE,
                    env={"PATH": "/usr/bin:/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
                         "GIT_AUTHOR_NAME": "fixture", "GIT_AUTHOR_EMAIL": "fixture@example.test",
                         "GIT_COMMITTER_NAME": "fixture", "GIT_COMMITTER_EMAIL": "fixture@example.test"}).decode().strip()
            git("init", "-b", "main")
            (source / "fact").write_text("base")
            git("add", ".")
            git("commit", "-m", "base")
            git("checkout", "-b", "candidate")
            (source / "candidate").write_text("candidate")
            git("add", ".")
            git("commit", "-m", "candidate")
            candidate = git("rev-parse", "HEAD")
            tree = git("rev-parse", "HEAD^{tree}")
            git("checkout", "main")
            (source / "main").write_text("new main")
            git("add", ".")
            git("commit", "-m", "main")
            main = git("rev-parse", "HEAD")
            git("update-ref", "refs/remotes/origin/main", main)
            git("checkout", "candidate")
            workspace = root / "workspace"
            workspace.mkdir()
            git("bundle", "create", str(workspace / "source.bundle"), "HEAD", "refs/remotes/origin/main")
            raw = (workspace / "source.bundle").read_bytes()
            imported = io.import_snapshot(workspace, {"commit": candidate, "main": main, "tree": tree,
                                                    "bytes": len(raw), "sha256": io.digest(raw)}, lambda: 30)
            for ref, expected in [("HEAD", candidate), ("refs/remotes/origin/main", main), ("HEAD^{tree}", tree)]:
                self.assertEqual(subprocess.check_output(["git", "-C", str(imported), "rev-parse", ref]).decode().strip(), expected)
            self.assertEqual(subprocess.check_output(["git", "-C", str(imported), "remote"]), b"")
            self.assertFalse((imported / ".git/config").read_text().find("credential") >= 0)


if __name__ == "__main__":
    unittest.main()
