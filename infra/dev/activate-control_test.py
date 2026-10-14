import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


def module(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), Path(__file__).with_name(name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class ActivationContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = module("activate-control")
        cls.artifact = module("control-artifact")

    def fixture(self, root):
        binaries = root / "binaries"
        binaries.mkdir()
        for name in ("boss", "kubectl"):
            (binaries / name).write_bytes(b"published tool fixture")
        self.artifact.pack(Path(__file__).resolve().parents[2], binaries, root / "bundle.tar", "a" * 40)
        store = root / "control"
        store.mkdir(mode=0o700)
        (store / "old").mkdir()
        (store / "current").symlink_to("old")
        enabled = root / "enabled"
        enabled.symlink_to(store / "old", target_is_directory=True)
        config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}
        return store, enabled, config

    def test_unsupported_contract_preserves_the_existing_generation_and_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled, config = self.fixture(root)
            calls = []
            def runner(argv, env):
                calls.append((argv, env))
                return subprocess.CompletedProcess(argv, 0, b"unsupported contract\n", b"")
            with self.assertRaises(ValueError):
                self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertEqual(os.readlink(store / "current"), "old")
            self.assertEqual(enabled.resolve(), store / "old")
            self.assertEqual(len(calls), 1)
            self.assertNotIn("PYTHONPATH", calls[0][1])

    def test_supported_contract_activates_only_a_complete_verified_generation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled, config = self.fixture(root)
            def runner(argv, env):
                self.assertEqual(Path(argv[0]).parent.name, "bin")
                self.assertEqual(argv[1:], ["dev-control-contract"])
                return subprocess.CompletedProcess(argv, 0, CONTRACT, b"")
            self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertEqual(os.readlink(store / "current"), "generations/" + "a" * 40)
            generation = store / "current"
            self.assertTrue((generation / "bin/boss").is_file())
            self.assertTrue((generation / "bin/kubectl").is_file())
            self.assertEqual(json.loads((generation / "config.json").read_text()), config)
            self.assertEqual((generation / "config.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual(enabled.resolve(), store / "generations" / ("a" * 40))
            for tool in ("boss", "boss-api", "wt-cargo", "wt-web", "dev-build"):
                self.assertTrue((store / tool).is_symlink())
                self.assertEqual((store / tool).resolve(), (generation / "infra/dev/control-tools.py").resolve())
                self.assertEqual((store / tool).stat().st_mode & 0o777, 0o755)
            self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertEqual(os.readlink(store / "current"), "generations/" + "a" * 40)
            self.assertEqual(len(list(generation.glob("contract-check-*.json"))), 2)

    def test_interrupted_contract_check_retains_full_output_and_old_generation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled, config = self.fixture(root)
            def runner(argv, env):
                raise subprocess.TimeoutExpired(argv, 30, output=b"full partial contract output", stderr=b"timeout cause")
            with self.assertRaises(subprocess.TimeoutExpired):
                self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertEqual(os.readlink(store / "current"), "old")
            self.assertEqual(enabled.resolve(), store / "old")
            receipt = json.loads((store / "generations" / ("a" * 40) / "contract-check.json").read_text())
            import base64
            self.assertEqual(receipt["completion"], "unproven")
            self.assertEqual(base64.b64decode(receipt["stdout_base64"]), b"full partial contract output")
            self.assertEqual(base64.b64decode(receipt["stderr_base64"]), b"timeout cause")

    def test_one_atomic_generation_pointer_controls_every_frontend(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled, config = self.fixture(root)
            runner = lambda argv, env: subprocess.CompletedProcess(argv, 0, CONTRACT, b"")
            self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertTrue(enabled.is_symlink(), "the authority must be one atomic generation pointer")
            self.assertTrue(enabled.is_dir())
            for tool in ("boss", "boss-api", "wt-cargo", "wt-web", "dev-build"):
                self.assertEqual(os.readlink(store / tool), str(enabled / "infra/dev/control-tools.py"))
            from unittest.mock import patch
            original = self.mod.atomic_link
            def interrupted(parent, name, target):
                if parent / name == enabled:
                    raise KeyboardInterrupt("fixture interruption before activation commit")
                return original(parent, name, target)
            with patch.object(self.mod, "atomic_link", side_effect=interrupted):
                with self.assertRaises(KeyboardInterrupt):
                    self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, config, runner)
            self.assertEqual(enabled.resolve(), store / "generations" / ("a" * 40))
            changed = dict(config, forge_url="http://other.invalid/repo.git")
            with self.assertRaises(ValueError):
                self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40, changed, runner)
            self.assertEqual(json.loads((enabled / "config.json").read_text()), config)

    def test_concurrent_activations_cannot_interleave_authority_or_config(self):
        import threading
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled, config = self.fixture(root)
            entered, release, second_started, second_entered = (threading.Event() for _ in range(4))
            errors = []
            def first_runner(argv, env):
                entered.set()
                if not release.wait(5):
                    raise RuntimeError("fixture release did not arrive")
                return subprocess.CompletedProcess(argv, 0, CONTRACT, b"")
            def second_runner(argv, env):
                second_entered.set()
                return subprocess.CompletedProcess(argv, 0, CONTRACT, b"")
            def invoke(runner, altered=False):
                if altered:
                    second_started.set()
                try:
                    self.mod.activate(root / "bundle.tar", store, enabled, "a" * 40,
                        dict(config, forge_url="http://other.invalid/repo.git") if altered else config, runner)
                except BaseException as error:
                    errors.append(error)
            first = threading.Thread(target=invoke, args=(first_runner,))
            second = threading.Thread(target=invoke, args=(second_runner, True))
            first.start()
            try:
                self.assertTrue(entered.wait(5))
                second.start()
                self.assertTrue(second_started.wait(5))
                self.assertFalse(second_entered.wait(0.1), "another activation entered before lock release")
                self.assertEqual(enabled.resolve(), store / "old")
            finally:
                release.set()
                first.join(5)
                if second.ident is not None:
                    second.join(5)
            self.assertFalse(first.is_alive() or second.is_alive())
            self.assertEqual(len(errors), 1)
            self.assertIsInstance(errors[0], ValueError)
            self.assertEqual(json.loads((enabled / "config.json").read_text()), config)


CONTRACT = (Path(__file__).with_name("control-contract.txt").read_bytes().strip() + b"\n")


def supported(argv, env):
    return subprocess.CompletedProcess(argv, 0, CONTRACT, b"")


class ActivationFollowsMain(unittest.TestCase):
    """Design a68fc42a D6 (David, 2026-10-10): the store and its selector
    live on the /work PVC, so a pod roll keeps them, and the hourly
    installer re-activates each verified generation. Re-activation never
    turns isolation ON: with no selector it is inert, because activating
    is the operator's act (b3071bf4 held_by_david_20261009)."""

    @classmethod
    def setUpClass(cls):
        cls.mod = module("activate-control")
        cls.artifact = module("control-artifact")

    def bundle(self, root, commit):
        binaries = root / ("binaries-" + commit[:1])
        binaries.mkdir()
        for name in ("boss", "kubectl"):
            (binaries / name).write_bytes(b"published tool fixture " + commit.encode())
        generation = root / commit
        generation.mkdir(mode=0o700)
        (generation / "config.json").write_text(json.dumps(self.config))
        (generation / "config.json").chmod(0o600)
        archive = generation / "dev-control.tar"
        self.artifact.pack(Path(__file__).resolve().parents[2], binaries, archive, commit)
        return archive

    def store(self, root):
        store = root / "dev-control"
        store.mkdir(mode=0o700)
        return store, store / "enabled"

    config = {"worker_image": "registry.invalid/build@sha256:" + "b" * 64, "postgres_image": "registry.invalid/postgres@sha256:" + "c" * 64, "forge_url": "http://forge.invalid/owner/repo.git"}

    def test_supported_rollback_serializes_with_an_in_flight_reactivation(self):
        import threading
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            next_archive = self.bundle(root, "b" * 40)
            entered, resume, off_started, off_done = (threading.Event() for _ in range(4))
            errors = []
            def runner(argv, env):
                entered.set()
                if not resume.wait(5):
                    raise RuntimeError("rollback fixture timed out")
                return supported(argv, env)
            def follow():
                try:
                    self.mod.reactivate(next_archive, store, enabled, "b" * 40, runner)
                except BaseException as error:
                    errors.append(error)
            def turn_off():
                off_started.set()
                try:
                    self.mod.deactivate(store, enabled)
                except BaseException as error:
                    errors.append(error)
                finally:
                    off_done.set()
            first, off = threading.Thread(target=follow), threading.Thread(target=turn_off)
            first.start()
            try:
                self.assertTrue(entered.wait(5))
                off.start()
                self.assertTrue(off_started.wait(5))
                self.assertFalse(off_done.wait(0.1), "rollback must wait for the active transition")
            finally:
                resume.set()
                first.join(5)
                if off.ident is not None:
                    off.join(5)
            self.assertEqual(errors, [])
            self.assertFalse(first.is_alive() or off.is_alive())
            self.assertFalse(enabled.exists() or enabled.is_symlink())
            self.assertEqual(self.mod.reactivate(next_archive, store, enabled, "b" * 40, runner=supported), "inert")
            self.assertEqual(json.loads((store / "off.json").read_text())["state"], "off")

    def test_rollback_is_durable_idempotent_and_explicit_activation_can_restore_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            archive = self.bundle(root, "a" * 40)
            self.mod.activate(archive, store, enabled, "a" * 40, self.config, supported)
            self.mod.deactivate(store, enabled)
            self.mod.deactivate(store, enabled)
            self.assertFalse(enabled.is_symlink())
            self.assertEqual(self.mod.reactivate(archive, store, enabled, "a" * 40, runner=supported), "inert")
            self.mod.activate(archive, store, enabled, "a" * 40, self.config, supported)
            self.assertTrue(enabled.is_symlink())
            self.assertFalse((store / "off.json").exists())

    def test_a_recorded_off_intent_survives_interruption_before_selector_removal(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            archive = self.bundle(root, "a" * 40)
            self.mod.activate(archive, store, enabled, "a" * 40, self.config, supported)
            unlink = Path.unlink
            def interrupted(path, *args, **kwargs):
                if path == enabled:
                    raise KeyboardInterrupt("fixture interruption after off intent")
                return unlink(path, *args, **kwargs)
            with patch.object(Path, "unlink", interrupted):
                with self.assertRaises(KeyboardInterrupt):
                    self.mod.deactivate(store, enabled)
            self.assertTrue(enabled.is_symlink())
            self.assertEqual(self.mod.reactivate(archive, store, enabled, "a" * 40, runner=supported), "inert")
            self.assertFalse(enabled.is_symlink())

    def test_the_durable_home_is_the_work_volume_not_the_container_root(self):
        # The roll that empties the container root fs must not empty the
        # selector: /etc and /opt were the root fs (gap (a), a68fc42a).
        self.assertEqual(self.mod.STORE, Path("/work/tools/dev-control"))
        self.assertEqual(self.mod.SELECTOR, self.mod.STORE / "enabled")
        source = Path(__file__).with_name("activate-control.py").read_text()
        self.assertNotIn("/etc/boss-dev-control", source)
        self.assertNotIn("/opt/boss-dev-control", source)

    def test_reactivation_without_a_selector_is_inert_and_creates_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            archive = self.bundle(root, "a" * 40)
            calls = []
            outcome = self.mod.reactivate(archive, store, enabled, "a" * 40, runner=lambda argv, env: calls.append(argv) or supported(argv, env))
            self.assertEqual(outcome, "inert")
            self.assertFalse(enabled.is_symlink() or enabled.exists(), "re-activation must never turn isolation on")
            self.assertFalse((store / "generations").exists())
            self.assertEqual(calls, [], "an inert pass runs no published binary")

    def test_reactivation_uses_the_new_generations_prepared_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            archive = self.bundle(root, "b" * 40)
            changed = dict(self.config, worker_image="registry.invalid/build@sha256:" + "d" * 64)
            (archive.parent / "config.json").write_text(json.dumps(changed))
            outcome = self.mod.reactivate(archive, store, enabled, "b" * 40, runner=supported)
            self.assertEqual(outcome, "activated " + "b" * 40)
            self.assertEqual(os.readlink(enabled), str(store / "generations" / ("b" * 40)))
            self.assertEqual(json.loads((enabled / "config.json").read_text()), changed)
            for tool in ("boss", "boss-api", "wt-cargo", "wt-web", "dev-build"):
                self.assertEqual((store / tool).resolve(), (store / "generations" / ("b" * 40) / "infra/dev/control-tools.py").resolve())

    def test_reactivation_recovers_a_refused_selector_from_the_last_good_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            enabled.unlink()
            enabled.symlink_to("refused")
            outcome = self.mod.reactivate(self.bundle(root, "b" * 40), store, enabled, "b" * 40, runner=supported)
            self.assertEqual(outcome, "activated " + "b" * 40)
            self.assertEqual(os.readlink(enabled), str(store / "generations" / ("b" * 40)))

    def test_unprepared_new_generation_refuses_and_preserves_previous_authority(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            original = os.readlink(enabled)
            archive = self.bundle(root, "b" * 40)
            (archive.parent / "config.json").unlink()
            calls = []
            with self.assertRaises(FileNotFoundError):
                self.mod.reactivate(archive, store, enabled, "b" * 40, runner=lambda argv, env: calls.append(argv))
            self.assertEqual(os.readlink(enabled), original)
            self.assertEqual(calls, [])

    def test_a_failed_reactivation_leaves_the_old_authority_for_the_caller_to_refuse(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            unsupported = lambda argv, env: subprocess.CompletedProcess(argv, 0, b"another contract\n", b"")
            with self.assertRaises(ValueError):
                self.mod.reactivate(self.bundle(root, "b" * 40), store, enabled, "b" * 40, runner=unsupported)
            self.assertEqual(os.readlink(enabled), str(store / "generations" / ("a" * 40)))

    def test_a_store_the_volume_re_moded_is_refused_not_trusted(self):
        # fsGroup 1500 may re-mode the PVC on a roll (boss-dev.yaml): a
        # group-readable store fails closed, never open.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            self.mod.activate(self.bundle(root, "a" * 40), store, enabled, "a" * 40, self.config, supported)
            store.chmod(0o770)
            with self.assertRaises(ValueError):
                self.mod.reactivate(self.bundle(root, "b" * 40), store, enabled, "b" * 40, runner=supported)

    def test_following_main_keeps_a_bounded_number_of_generations(self):
        # Every train is a generation (~95 MB each); following main
        # hourly on a 40Gi volume must not keep them all.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            commits = [c * 40 for c in "abcdef0"]
            self.mod.activate(self.bundle(root, commits[0]), store, enabled, commits[0], self.config, supported)
            for commit in commits[1:]:
                self.mod.reactivate(self.bundle(root, commit), store, enabled, commit, runner=supported)
            kept = sorted(p.name for p in (store / "generations").iterdir())
            self.assertEqual(len(kept), self.mod.KEEP)
            self.assertIn(commits[-1], kept)
            self.assertIn(commits[-2], kept, "the generation just replaced is kept for a door still running from it")
            self.assertEqual(os.readlink(enabled), str(store / "generations" / commits[-1]))

    def test_the_command_line_reactivates_only_through_an_existing_selector(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store, enabled = self.store(root)
            cli = root / "cli"
            (cli / ("a" * 40)).mkdir(parents=True)
            (cli / ("a" * 40) / "dev-control.tar").write_bytes(self.bundle(root, "a" * 40).read_bytes())
            result = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("activate-control.py")), "a" * 40, "--cli-store", str(cli), "--store", str(store), "--reactivate"], capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(b"inert", result.stdout)
            self.assertFalse(enabled.is_symlink() or enabled.exists())


if __name__ == "__main__":
    unittest.main()
