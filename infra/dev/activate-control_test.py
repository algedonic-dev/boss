import importlib.util
import json
import os
from pathlib import Path
import subprocess
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
                return subprocess.CompletedProcess(argv, 0, b"dev-control-routing:v1\n", b"")
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
            runner = lambda argv, env: subprocess.CompletedProcess(argv, 0, b"dev-control-routing:v1\n", b"")
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
                return subprocess.CompletedProcess(argv, 0, b"dev-control-routing:v1\n", b"")
            def second_runner(argv, env):
                second_entered.set()
                return subprocess.CompletedProcess(argv, 0, b"dev-control-routing:v1\n", b"")
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


if __name__ == "__main__":
    unittest.main()
