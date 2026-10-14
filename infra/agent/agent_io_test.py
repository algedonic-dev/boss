import importlib.util
import os
import unittest
from pathlib import Path
import json
import subprocess
import tempfile
import shlex

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
