import importlib.util
import importlib._bootstrap_external
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class PublishedDoors(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("control_tools", Path(__file__).with_name("control-tools.py"))
        cls.mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.mod)

    def generation(self, root):
        generation = root / ('a' * 40)
        generation.mkdir(mode=0o700)
        helper = generation / 'infra/dev/control-artifact.py'
        helper.parent.mkdir(parents=True)
        helper.write_bytes(Path(__file__).with_name('control-artifact.py').read_bytes())
        helper.chmod(0o644)
        binary = generation / 'bin/boss'
        binary.parent.mkdir()
        binary.write_text('#!/bin/bash\nprintf published\\n\n')
        binary.chmod(0o755)
        record = {'commit': generation.name, 'files': {
            str(path.relative_to(generation)): {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': path.stat().st_mode & 0o777}
            for path in (helper, binary)}}
        (generation / 'artifact.json').write_text(json.dumps(record))
        return generation

    def test_published_generation_authority_is_observed_before_any_tool_exec(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            generation = self.generation(root)
            candidate = root / 'candidate'
            candidate.mkdir()
            started = root / 'candidate-started'
            (candidate / 'control-artifact.py').write_text('open(' + repr(str(started)) + ', "w").write("ran")')
            self.assertEqual(self.mod.observed_generation(generation), generation.name)
            self.assertFalse(started.exists(), 'candidate validation code was executed')
            record = json.loads((generation / 'artifact.json').read_text())
            record['commit'] = 'b' * 40
            (generation / 'artifact.json').write_text(json.dumps(record))
            with self.assertRaises(ValueError):
                self.mod.observed_generation(generation)

    def test_modified_or_linked_published_generation_cannot_claim_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            generation = self.generation(Path(temporary))
            (generation / 'bin/boss').write_text('changed bytes')
            with self.assertRaises(ValueError):
                self.mod.observed_generation(generation)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            generation = self.generation(root)
            record = generation / 'artifact.json'
            saved = record.read_bytes()
            record.unlink()
            foreign = root / 'foreign.json'
            foreign.write_bytes(saved)
            record.symlink_to(foreign)
            with self.assertRaises(OSError):
                self.mod.observed_generation(generation)

    def test_real_frontend_refuses_changed_generation_before_binary_effect(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            generation = self.generation(root)
            frontend = generation / 'infra/dev/control-tools.py'
            frontend.write_bytes(Path(__file__).with_name('control-tools.py').read_bytes())
            record_path = generation / 'artifact.json'
            record = json.loads(record_path.read_text())
            record['files']['infra/dev/control-tools.py'] = {'sha256': hashlib.sha256(frontend.read_bytes()).hexdigest(), 'mode': 0o644}
            record_path.write_text(json.dumps(record))
            for name, value in [('machine-token-dir', '/fake-owned-control/machine\n'), ('sor-url', 'http://control.invalid:7900\n'), ('sor-read-url', 'http://read.invalid:7900\n')]:
                path = generation / 'infra/dev' / name
                path.write_text(value)
                record['files']['infra/dev/' + name] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': 0o644}
            record_path.write_text(json.dumps(record))
            invoke = lambda: subprocess.run(['/usr/bin/python3', '-I', str(frontend), 'boss', 'orient'], env={'PATH': '/usr/bin:/bin', 'PYTHONPATH': str(root / 'candidate')}, capture_output=True, check=False)
            self.assertEqual(invoke().returncode, 0)
            effect = root / 'changed-binary-started'
            (generation / 'bin/boss').write_text('#!/bin/bash\ntouch ' + str(effect) + '\n')
            bad = invoke()
            self.assertNotEqual(bad.returncode, 0, bad.stdout)
            self.assertFalse(effect.exists(), 'changed binary ran before generation refusal')

    def test_fifo_manifest_or_validation_helper_refuses_without_waiting_for_a_writer(self):
        for name in ('artifact.json', 'infra/dev/control-artifact.py', 'bin/boss'):
            with self.subTest(member=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                generation = self.generation(root)
                path = generation / name
                path.unlink()
                os.mkfifo(path, 0o600)
                probe = root / 'probe.py'
                probe.write_text('import importlib.util\nfrom pathlib import Path\nimport sys\nspec = importlib.util.spec_from_file_location("tools", ' + repr(str(Path(__file__).with_name('control-tools.py'))) + ')\nmodule = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(module)\nmodule.observed_generation(Path(sys.argv[1]))\n')
                result = subprocess.run(['/usr/bin/python3', '-I', str(probe), str(generation)], capture_output=True, timeout=2, check=False)
                self.assertNotEqual(result.returncode, 0)

    def test_a_forged_helper_cache_cannot_replace_the_verified_source_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            generation = self.generation(root)
            helper = generation / 'infra/dev/control-artifact.py'
            effect = root / 'unverified-cache-executed'
            code = compile('open(' + repr(str(effect)) + ', "w").write("cache")\ndef validate_generation(*args): pass\n', str(helper), 'exec')
            info = helper.stat()
            cache = Path(importlib.util.cache_from_source(str(helper)))
            cache.parent.mkdir()
            cache.write_bytes(importlib._bootstrap_external._code_to_timestamp_pyc(code, int(info.st_mtime), info.st_size))
            self.mod.observed_generation(generation)
            self.assertFalse(effect.exists(), 'unadmitted pycache executed instead of verified helper bytes')

    def test_published_cli_starts_without_candidate_startup_paths_or_environment(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "bin").mkdir()
            (root / "infra/dev").mkdir(parents=True)
            (root / "infra/dev/machine-token-dir").write_text("/fake-owned-control/machine\n")
            (root / "infra/dev/sor-url").write_text("http://control.invalid:7900\n")
            (root / "infra/dev/sor-read-url").write_text("http://read.invalid:7900\n")
            (root / "artifact.json").write_text('{"commit":"' + "a" * 40 + '"}')
            marker = root / "candidate-started"
            startup = root / "startup.sh"
            startup.write_text("printf read > '" + str(marker) + "'\n")
            boss = root / "bin/boss"
            boss.write_text('#!/bin/bash\nprintf "%s\\n" "$BOSS_ACTOR" "$BOSS_BUILD_COMMIT" "$@"\n')
            boss.chmod(0o755)
            argv, env = self.mod.tool_plan(root, "boss", ["orient"], {"BASH_ENV": str(startup), "HOME": str(root), "PATH": str(root), "BOSS_ACTOR": "agent-codex", "BOSS_SHIM_BUILT": "1"})
            # BOSS_SHIM_BUILT deliberately selects the worker instead.
            self.assertIn("exec-auto", argv)
            argv, env = self.mod.tool_plan(root, "boss", ["orient"], {"BASH_ENV": str(startup), "HOME": str(root), "PATH": str(root), "BOSS_ACTOR": "agent-codex"})
            result = subprocess.run(argv, env=env, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertFalse(marker.exists())
            self.assertEqual(result.stdout, b"agent-codex\n" + b"a" * 40 + b"\norient\n")
            self.assertEqual(env["BOSS_MACHINE_TOKEN_DIR"], "/fake-owned-control/machine")
            self.assertEqual(env["BOSS_MACHINE_TOKEN_HOSTS"], "control.invalid,read.invalid")

    def test_cargo_web_and_candidate_cli_go_only_through_the_fixed_worker_controller(self):
        root = Path("/opt/boss-dev-control/generations/" + "a" * 40)
        for name in ("wt-cargo", "wt-web"):
            argv, env = self.mod.tool_plan(root, name, ["literal;touch /control"], {})
            self.assertEqual(argv[:4], ["/usr/bin/python3", "-I", str(root / "infra/dev/dev-build.py"), "exec-auto"])
            self.assertEqual(argv[-1], "literal;touch /control")
            self.assertNotIn("BOSS_ACTOR", env)
        argv, _ = self.mod.tool_plan(root, "boss", ["--built", "--version"], {})
        self.assertIn("exec-auto", argv)
        self.assertNotIn(str(root / "bin/boss"), argv)

    def test_each_source_door_routes_before_reading_candidate_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "enabled"
            marker.write_text("reviewed generation")
            for name in ("boss", "boss-api", "wt-cargo", "wt-web"):
                with self.subTest(door=name):
                    target = root / name
                    target.write_text('#!/bin/bash\nprintf "published:%s\\n" "$@"\n')
                    target.chmod(0o755)
                    source = Path(__file__).with_name(name).read_text()
                    source = source.replace("/etc/boss-dev-control/enabled", str(marker)).replace("/opt/boss-dev-control/" + name, str(target))
                    entry = root / (name + "-entry")
                    entry.write_text(source)
                    result = subprocess.run(["/bin/bash", str(entry), "literal;touch /control"], env={"PATH":"/usr/bin:/bin", "HOME":str(root)}, capture_output=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, b"published:literal;touch /control\n")


if __name__ == "__main__":
    unittest.main()
