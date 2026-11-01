import importlib.util
import importlib._bootstrap_external
import hashlib
import json
import os
from pathlib import Path
import shutil
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
        # A generation's bin/ is exactly the published pair (e17b0135).
        kubectl = generation / 'bin/kubectl'
        kubectl.write_text('#!/bin/bash\nexit 0\n')
        kubectl.chmod(0o755)
        record = {'commit': generation.name, 'files': {
            str(path.relative_to(generation)): {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': path.stat().st_mode & 0o777}
            for path in (helper, binary, kubectl)}}
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

    def test_the_trusted_doors_carry_who_signs_and_which_run_it_is(self):
        # Backlog 19dbaab9 (measured 2026-10-18, the first hour of
        # isolation ON, run ec92b4f2): this door forwarded BOSS_ACTOR and
        # dropped the other two names the CLI reads for its own identity.
        # Without BOSS_AGENT_RUN, `boss dispatch <run> --started` refused
        # and every `boss gate` filed a gate-run claimed by no run; without
        # BOSS_ACTOR_FILE, a session that points it at /dev/null so an
        # unset BOSS_ACTOR signs as NOBODY fell back to the shared
        # $HOME/.config/boss/actor — another session's id. The worker gets
        # none of them: it is not a signing actor.
        identity = {"BOSS_ACTOR": "agent-claude-b", "BOSS_ACTOR_FILE": "/dev/null", "BOSS_AGENT_RUN": "fbdb0d24-3108-4cac-b575-6dcda6e49986"}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "infra/dev").mkdir(parents=True)
            (root / "infra/dev/machine-token-dir").write_text("/fake-owned-control/machine\n")
            (root / "infra/dev/sor-url").write_text("http://control.invalid:7900\n")
            (root / "infra/dev/sor-read-url").write_text("http://read.invalid:7900\n")
            (root / "artifact.json").write_text('{"commit":"' + "a" * 40 + '"}')
            for tool in ("boss", "boss-api"):
                with self.subTest(door=tool):
                    _, env = self.mod.tool_plan(root, tool, ["dispatch"], dict(identity))
                    self.assertEqual({k: env.get(k) for k in identity}, identity)
            for tool, args in (("wt-cargo", []), ("wt-web", []), ("dev-build", ["start"]), ("boss", ["--built", "gate"])):
                with self.subTest(worker=tool, args=args):
                    _, env = self.mod.tool_plan(root, tool, args, dict(identity))
                    self.assertEqual([k for k in identity if k in env], [])

    def test_the_trusted_doors_carry_what_dispatch_refuses_on_and_what_a_brief_prints(self):
        # Backlog c02f30c5 (found 2026-10-18 building 19dbaab9): two
        # names the coordinator's `boss` reads that are not identity
        # were dropped here too. Without BOSS_SESSION_AGENT_DEFINITIONS
        # the hook's `boss dispatch --from-hook` knew nothing about what
        # the session loaded, so its unloaded-definitions refusal passed
        # every call in silence; without BOSS_COMMIT_TRAILER every brief
        # rendered with no attribution lines. The worker gets neither.
        data = {
            "BOSS_SESSION_AGENT_DEFINITIONS": "/work/home/.boss/sessions/s-1/definitions",
            "BOSS_COMMIT_TRAILER": "Co-Authored-By: Example <noreply@example.invalid>",
        }
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "infra/dev").mkdir(parents=True)
            (root / "infra/dev/machine-token-dir").write_text("/fake-owned-control/machine\n")
            (root / "infra/dev/sor-url").write_text("http://control.invalid:7900\n")
            (root / "infra/dev/sor-read-url").write_text("http://read.invalid:7900\n")
            (root / "artifact.json").write_text('{"commit":"' + "a" * 40 + '"}')
            for tool in ("boss", "boss-api"):
                with self.subTest(door=tool):
                    _, env = self.mod.tool_plan(root, tool, ["dispatch"], dict(data))
                    self.assertEqual({k: env.get(k) for k in data}, data)
            for tool, args in (("wt-cargo", []), ("wt-web", []), ("dev-build", ["start"]), ("boss", ["--built", "gate"])):
                with self.subTest(worker=tool, args=args):
                    _, env = self.mod.tool_plan(root, tool, args, dict(data))
                    self.assertEqual([k for k in data if k in env], [])

    RUN = {"BOSS_AGENT_RUN": "d5f6346d-8bd9-46f6-bbd0-7f5d558c52f1"}

    def checkout(self, root, shape):
        # The three shapes wt-cargo's own guard tells apart with git
        # rev-parse: the main checkout worktrees hang off (git-dir equal
        # to git-common-dir, worktrees registered), a linked worktree
        # (`.git` is a pointer file), and a lone clone (no worktrees).
        tree = root / shape
        inner = tree / "crates/boss-cli"
        inner.mkdir(parents=True)
        if shape == "shared":
            (tree / ".git/worktrees/agent-1").mkdir(parents=True)
        elif shape == "linked":
            (tree / ".git").write_text("gitdir: " + str(root / "shared/.git/worktrees/agent-1") + "\n")
        elif shape == "lone":
            (tree / ".git/worktrees").mkdir(parents=True)
        return inner

    def test_a_run_is_refused_the_shared_checkout_at_the_worker_door(self):
        # Backlog c02f30c5, the third half. wt-cargo refuses a run that
        # builds the shared checkout (916fe290) by reading BOSS_AGENT_RUN
        # — but with isolation on it forwards to this door BEFORE that
        # guard runs, the worker plan carries no BOSS_AGENT_RUN, and the
        # worker's source is not the shared shape. The controller's cwd
        # binding does not stand in for it: the shared checkout is itself
        # a binding (the doors run from it, 7dd185c6), so a builder whose
        # cd never took builds that tree in a worker, silently. This door
        # is the one place that still sees both the run and the real cwd.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shared = self.checkout(root, "shared")
            refused = self.mod.shared_checkout_refusal("wt-cargo", dict(self.RUN), shared)
            self.assertIsNotNone(refused)
            self.assertIn("REFUSED", refused)
            self.assertIn(str(root / "shared"), refused)
            self.assertIn(self.RUN["BOSS_AGENT_RUN"], refused)
            self.assertIn("916fe290", refused)
            # What wt-cargo's own guard lets build still builds here.
            (root / "empty/.git/worktrees").mkdir(parents=True)
            for label, tool, inherited, cwd in (
                ("the run's own worktree", "wt-cargo", self.RUN, self.checkout(root, "linked")),
                ("a lone clone", "wt-cargo", self.RUN, self.checkout(root, "lone")),
                ("no worktrees registered", "wt-cargo", self.RUN, root / "empty"),
                ("no tree at all", "wt-cargo", self.RUN, root),
                ("the operator naming no run", "wt-cargo", {}, shared),
                ("a blank run", "wt-cargo", {"BOSS_AGENT_RUN": " \t"}, shared),
                ("the trusted CLI", "boss", self.RUN, shared),
                ("the trusted API door", "boss-api", self.RUN, shared),
            ):
                with self.subTest(label):
                    self.assertIsNone(self.mod.shared_checkout_refusal(tool, dict(inherited), cwd))

    def test_the_real_frontend_refuses_a_run_in_the_shared_checkout_before_the_controller(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            generation = self.generation(root)
            frontend = generation / 'infra/dev/control-tools.py'
            frontend.write_bytes(Path(__file__).with_name('control-tools.py').read_bytes())
            started = root / 'controller-started'
            controller = generation / 'infra/dev/dev-build.py'
            controller.write_text('open(' + repr(str(started)) + ', "w").write("ran")\n')
            record_path = generation / 'artifact.json'
            record = json.loads(record_path.read_text())
            for path in (frontend, controller):
                path.chmod(0o644)
                record['files'][str(path.relative_to(generation))] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': 0o644}
            record_path.write_text(json.dumps(record))
            env = {'PATH': '/usr/bin:/bin', **self.RUN}
            invoke = lambda cwd: subprocess.run(['/usr/bin/python3', '-I', str(frontend), 'wt-cargo', 'build'], cwd=cwd, env=env, capture_output=True, check=False)
            refused = invoke(self.checkout(root, 'shared'))
            self.assertEqual(refused.returncode, 2, refused.stderr)
            self.assertIn(b'916fe290', refused.stderr)
            self.assertFalse(started.exists(), 'the controller started for a run in the shared checkout')
            # Control: the run's own worktree reaches the controller.
            admitted = invoke(self.checkout(root, 'linked'))
            self.assertEqual(admitted.returncode, 0, admitted.stderr)
            self.assertTrue(started.exists(), 'the control leg never reached the controller')

    def test_a_removed_cwd_is_read_only_by_the_tool_that_needs_a_tree(self):
        # Backlog 2e5429b6 (release review 18b9706f): main() read
        # os.getcwd() for EVERY tool before it knew which one was called,
        # so a session whose worktree was removed under it lost every boss
        # and boss-api call — the hook's dispatch among them — to a
        # FileNotFoundError traceback. Only wt-cargo naming a run needs the
        # tree; it gets a named refusal, every other door never asks.
        here = os.getcwd()
        with tempfile.TemporaryDirectory() as tmp:
            gone = Path(tmp) / "removed-worktree"
            gone.mkdir()
            os.chdir(gone)
            gone.rmdir()
            try:
                for tool, inherited in (
                    ("boss", self.RUN), ("boss-api", self.RUN), ("wt-web", self.RUN),
                    ("dev-build", self.RUN), ("wt-cargo", {}),
                ):
                    with self.subTest(tool=tool, inherited=inherited):
                        self.assertIsNone(self.mod.shared_checkout_refusal(tool, dict(inherited)))
                refused = self.mod.shared_checkout_refusal("wt-cargo", dict(self.RUN))
            finally:
                os.chdir(here)
        self.assertIsNotNone(refused)
        self.assertIn("REFUSED", refused)
        self.assertIn("no longer exists", refused)
        self.assertIn(self.RUN["BOSS_AGENT_RUN"], refused)
        self.assertIn("2e5429b6", refused)

    def test_an_unreadable_worktrees_directory_is_a_named_refusal(self):
        # Backlog 2e5429b6: iterdir() on a worktrees/ the caller cannot
        # read raised PermissionError out of the door. Whether the tree
        # is shared cannot be judged, so a run is refused, by name.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shared = self.checkout(root, "shared")
            worktrees = root / "shared/.git/worktrees"
            worktrees.chmod(0)
            try:
                if os.access(worktrees, os.R_OK):
                    self.skipTest("this uid reads a mode-0 directory (root); the refusal cannot be staged")
                refused = self.mod.shared_checkout_refusal("wt-cargo", dict(self.RUN), shared)
            finally:
                worktrees.chmod(0o755)
        self.assertIsNotNone(refused)
        self.assertIn("REFUSED", refused)
        self.assertIn(str(worktrees), refused)
        self.assertIn("2e5429b6", refused)

    def test_the_real_frontend_survives_a_removed_cwd(self):
        # Backlog 2e5429b6, end to end: the published frontend started
        # from a deleted directory reaches the trusted doors (boss,
        # boss-api) and refuses wt-cargo for a run by name — no traceback
        # on either leg.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            generation = self.generation(root)
            frontend = generation / 'infra/dev/control-tools.py'
            frontend.write_bytes(Path(__file__).with_name('control-tools.py').read_bytes())
            api = generation / 'infra/dev/boss-api'
            api.write_text('printf "api door reached\\n"\n')
            started = root / 'controller-started'
            controller = generation / 'infra/dev/dev-build.py'
            controller.write_text('open(' + repr(str(started)) + ', "w").write("ran")\n')
            record_path = generation / 'artifact.json'
            record = json.loads(record_path.read_text())
            for path in (frontend, api, controller):
                path.chmod(0o644)
                record['files'][str(path.relative_to(generation))] = {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'mode': 0o644}
            record_path.write_text(json.dumps(record))
            (generation / 'infra/dev/machine-token-dir').write_text('/fake-owned-control/machine\n')
            (generation / 'infra/dev/sor-url').write_text('http://control.invalid:7900\n')
            (generation / 'infra/dev/sor-read-url').write_text('http://read.invalid:7900\n')
            env = {'PATH': '/usr/bin:/bin', **self.RUN}

            def invoke(*argv):
                gone = root / 'removed-worktree'
                gone.mkdir()
                # The shell removes its own cwd, then becomes the door.
                return subprocess.run(['/bin/sh', '-c', 'rmdir -- "$PWD" && exec "$0" "$@"', '/usr/bin/python3', '-I', str(frontend), *argv],
                                      cwd=gone, env=env, capture_output=True, check=False)

            # self.generation's bin/boss prints 'published' (its printf
            # escape arrives as a literal n), so the boss leg reads a prefix.
            for argv, expected in ((('boss', 'orient'), b'published'), (('boss-api', 'GET', '/api/jobs'), b'api door reached\n')):
                with self.subTest(door=argv[0]):
                    reached = invoke(*argv)
                    self.assertNotIn(b'Traceback', reached.stderr)
                    self.assertEqual(reached.returncode, 0, reached.stderr)
                    self.assertTrue(reached.stdout.startswith(expected), reached.stdout)
            refused = invoke('wt-cargo', 'build')
            self.assertNotIn(b'Traceback', refused.stderr)
            self.assertEqual(refused.returncode, 2, refused.stderr)
            self.assertIn(b'no longer exists', refused.stderr)
            self.assertFalse(started.exists(), 'the controller started from a removed cwd')

    def test_the_trusted_cli_resolves_the_generations_own_kubectl_and_no_writable_one(self):
        # Backlog 63d59f45 (isolation trial b3071bf4, 2026-10-17): with
        # isolation on, `boss gate` launch, withdraw and every gate-Job
        # read failed — the CLI runs `kubectl` by name, and this door's
        # PATH was /usr/bin:/bin, while the pod's kubectl is in the
        # group-writable /work/tools/bin. The generation already carries
        # a hash-verified bin/kubectl (dev-build.py runs it by absolute
        # path), so the trusted CLI must find exactly that one, and an
        # inherited PATH entry — a writable tree — must never win.
        #
        # Backlog 7ed1251a (RELEASE review f6e0c3e5 of car 77f528cc): it
        # found that one by putting bin/ first on PATH, and the listing of
        # bin/ is checked ONCE, before the exec, while the CLI resolves
        # names for as long as it runs — so a name planted in bin/ after
        # the check (a `git`) resolved there. The door now hands the CLI
        # the validated kubectl by absolute path (BOSS_DOOR_KUBECTL, read
        # by boss-cli door_env.rs) and puts nothing of the generation on
        # PATH, so no name the CLI looks up can resolve into bin/.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "bin").mkdir()
            (root / "infra/dev").mkdir(parents=True)
            (root / "infra/dev/machine-token-dir").write_text("/fake-owned-control/machine\n")
            (root / "infra/dev/sor-url").write_text("http://control.invalid:7900\n")
            (root / "infra/dev/sor-read-url").write_text("http://read.invalid:7900\n")
            (root / "artifact.json").write_text('{"commit":"' + "a" * 40 + '"}')
            writable = root / "writable-tools"
            writable.mkdir()
            for directory in (root / "bin", writable):
                kubectl = directory / "kubectl"
                kubectl.write_text("#!/bin/bash\nexit 0\n")
                kubectl.chmod(0o755)
            # Resolves `kubectl` the way the CLI's kubectl_program() does
            # (the door's absolute path, else the name on PATH), and then
            # a name the CLI runs bare, `git`, the way Command::new does.
            boss = root / "bin/boss"
            boss.write_text('#!/bin/bash\ncommand -v "${BOSS_DOOR_KUBECTL:-kubectl}" || echo "kubectl: not found"\n'
                            'command -v git || echo "git: not found"\n')
            boss.chmod(0o755)
            argv, env = self.mod.tool_plan(root, "boss", ["gate", "--withdraw"], {"HOME": str(root), "PATH": str(writable) + ":/usr/bin:/bin"})
            self.assertEqual(argv[0], str(root / "bin/boss"))
            self.assertEqual(env["BOSS_DOOR_KUBECTL"], str(root / "bin/kubectl"))
            self.assertEqual(env["PATH"], "/usr/bin:/bin")
            # Planted AFTER the plan: what the check could not see.
            planted = root / "bin/git"
            planted.write_text("#!/bin/bash\nexit 0\n")
            planted.chmod(0o755)
            result = subprocess.run(argv, env=env, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            kubectl_seen, git_seen = result.stdout.decode().splitlines()
            self.assertEqual(kubectl_seen, str(root / "bin/kubectl"))
            self.assertNotEqual(git_seen, str(planted), "a name planted in bin/ after the check resolved there")
        # Every other door keeps the minimal PATH: only the trusted CLI
        # drives the cluster from this side.
        _, env = self.mod.tool_plan(Path(self.STORE + "/generations/" + "a" * 40), "wt-cargo", [], {})
        self.assertEqual(env["PATH"], "/usr/bin:/bin")
        self.assertNotIn("BOSS_DOOR_KUBECTL", env)

    def test_cargo_web_and_candidate_cli_go_only_through_the_fixed_worker_controller(self):
        root = Path(self.STORE + "/generations/" + "a" * 40)
        for name in ("wt-cargo", "wt-web"):
            argv, env = self.mod.tool_plan(root, name, ["literal;touch /control"], {})
            self.assertEqual(argv[:4], ["/usr/bin/python3", "-I", str(root / "infra/dev/dev-build.py"), "exec-auto"])
            self.assertEqual(argv[-1], "literal;touch /control")
            self.assertNotIn("BOSS_ACTOR", env)
        argv, _ = self.mod.tool_plan(root, "boss", ["--built", "--version"], {})
        self.assertIn("exec-auto", argv)
        self.assertNotIn(str(root / "bin/boss"), argv)

    # THE ONE SPELLING of the durable store (design a68fc42a D6): the four
    # doors, the activation door and the CLI's child forwarding each name
    # it, so they are held equal here (CLAUDE.md §9a) — a door reading an
    # old root-fs path would see no selector and run candidates locally.
    STORE = "/work/tools/dev-control"
    DOORS = ("boss", "boss-api", "wt-cargo", "wt-web")

    def door(self, root, name, extra_env=None, args=("literal;touch /control",)):
        source = Path(__file__).with_name(name).read_text()
        self.assertEqual(source.count(self.STORE), 1, name + " spells the store once, in one assignment")
        entry = root / (name + "-entry")
        entry.write_text(source.replace(self.STORE, str(root / "store")))
        env = {"PATH": "/usr/bin:/bin", "HOME": str(root)}
        env.update(extra_env or {})
        return subprocess.run(["/bin/bash", str(entry), *args], env=env, capture_output=True, check=False)

    def published(self, root):
        store = root / "store"
        store.mkdir()
        for name in self.DOORS:
            target = store / name
            target.write_text('#!/bin/bash\nprintf "published:%s\\n" "$@"\n')
            target.chmod(0o755)
        return store

    # THE HELD ROUTE JUDGES THE SELECTOR WHEN IT RUNS (backlog 9a37d9d8,
    # 2026-10-18). `boss brief` reads the selector once, at render time,
    # and while it is absent names a LOCAL pre-flight; a builder runs that
    # line minutes to an hour later. A bare `/bin/bash infra/gate.sh` is
    # no door, so an activation in between ran candidate lints and clippy
    # beside this container's credentials, green and silent. The held
    # line goes through this door instead, which reads the selector again.
    HELD = "run-while-held.sh"

    def held(self, root, tree, args):
        source = Path(__file__).with_name(self.HELD).read_text()
        self.assertEqual(source.count(self.STORE), 1, self.HELD + " spells the store once, in one assignment")
        entry = root / "held-entry"
        entry.write_text(source.replace(self.STORE, str(root / "store")))
        env = {"PATH": "/usr/bin:/bin", "HOME": str(root)}
        return subprocess.run(["/bin/bash", str(entry), *args], cwd=tree, env=env, capture_output=True, check=False)

    def test_the_held_door_runs_its_command_locally_only_while_no_selector_exists(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            store = root / "store"
            store.mkdir()
            tree = root / "tree"
            (tree / "infra").mkdir(parents=True)
            ran = root / "candidate-ran"
            (tree / "infra/gate.sh").write_text("printf '%s\\n' \"$@\" > '" + str(ran) + "'\nexit 3\n")
            # Held: the command runs here, relative to the cwd the builder's
            # script cd'd into, and its exit status is the door's.
            result = self.held(root, tree, ("infra/gate.sh", "--lint"))
            self.assertEqual(result.returncode, 3, result.stderr)
            self.assertEqual(ran.read_text(), "--lint\n")
            # No command is a usage error, never an interactive shell.
            result = self.held(root, tree, ())
            self.assertEqual(result.returncode, 2, result.stderr)
            # Any selector at all — a generation, a refusal, a file, a
            # foreign link — and the local run is what it forbids.
            selector = store / "enabled"
            for shape in ("generation", "refused", "regular-file", "foreign-link"):
                with self.subTest(shape=shape):
                    if ran.exists():
                        ran.unlink()
                    if selector.is_symlink() or selector.exists():
                        selector.unlink()
                    if shape == "generation":
                        selector.symlink_to(store / "generations" / ("a" * 40))
                    elif shape == "refused":
                        selector.symlink_to("refused")
                    elif shape == "regular-file":
                        selector.write_text("not a generation")
                    else:
                        selector.symlink_to(root / "elsewhere" / ("a" * 40))
                    result = self.held(root, tree, ("infra/gate.sh", "--lint"))
                    self.assertEqual(result.returncode, 75, result.stderr)
                    self.assertFalse(ran.exists(), "an activated selector must never run the candidate here")
                    self.assertIn(str(selector).encode(), result.stderr)
                    self.assertIn(b"boss brief", result.stderr, "the refusal names the re-render")

    # THE GATE-UID DOOR JUDGES THE SELECTOR TOO (backlog aeac17f0, reviewer
    # run 355b595e, 2026-10-20). infra/dev/as-gate-uid.sh runs its command —
    # cargo test, infra/gate.sh: candidate code — as uid 65534 gid 1500,
    # and the machine token /etc/boss-doors/machine-token is 0440 root:1500,
    # so with isolation on it handed the token to candidate code without
    # ever reading the selector. It refuses with 75 now, before it bundles
    # anything, on any selector and on a store this caller cannot read
    # (the shape of backlog 1fddbe13); with no store, or no selector in a
    # store it can read, it is the held state and runs as before. Run as
    # the caller the test is and, as root, as uid 65534 too: the pod's
    # store is root-owned 2700, so the same file answers differently.
    UID_DOOR = "as-gate-uid.sh"

    def uid_door_tree(self, root):
        """A committed tree the uid door can verify: the runner manifest
        it reads the gate's ids from, the cargo bound, and a candidate
        that says when it ran. Returns the tree and its door entry."""
        repo = Path(__file__).resolve().parents[2]
        tree = root / "tree"
        for path in ("infra/gate-runner/gate-runner.yaml", "infra/dev/pod-build.env"):
            (tree / path).parent.mkdir(parents=True, exist_ok=True)
            (tree / path).write_bytes((repo / path).read_bytes())
        (tree / "infra/gate.sh").write_text("echo CANDIDATE-RAN\n")
        source = Path(__file__).with_name(self.UID_DOOR).read_text()
        self.assertEqual(source.count(self.STORE), 1, self.UID_DOOR + " spells the store once, in one assignment")
        entry = tree / "infra/dev" / self.UID_DOOR
        entry.write_text(source.replace(self.STORE, str(root / "parent" / "store")))
        entry.chmod(0o755)
        git = ["git", "-C", str(tree), "-c", "user.name=t", "-c", "user.email=t@t.invalid"]
        for args in (["init", "-q"], ["add", "-A"], ["commit", "-q", "-m", "fixture"], ["update-ref", "refs/remotes/origin/main", "HEAD"]):
            subprocess.run([*git, *args], check=True, capture_output=True)
        return tree, entry

    def test_the_gate_uid_door_refuses_a_selector_and_a_store_it_cannot_read(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            root.chmod(0o755)
            tree, entry = self.uid_door_tree(root)
            scratch = root / "tmp"
            scratch.mkdir(mode=0o777)
            scratch.chmod(0o777)
            # The fixture tree is the test's; uid 65534 reads it the way the
            # gate reads a clone, so git is told it is safe to.
            env = {"PATH": "/usr/bin:/bin", "HOME": str(root), "TMPDIR": str(scratch),
                   "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "safe.directory", "GIT_CONFIG_VALUE_0": "*"}
            ids = subprocess.run(["/bin/bash", str(entry), "--print-ids"], cwd=tree, env=env, capture_output=True, check=True)
            uid, gid = ids.stdout.split()
            callers = {"self": []}
            if os.geteuid() == 0:
                setpriv = shutil.which("setpriv")
                self.assertIsNotNone(setpriv, "as root this test needs setpriv to run the door as the gate's uid; absent, it proves nothing")
                callers["uid " + uid.decode()] = [setpriv, "--reuid=" + uid.decode(), "--regid=" + gid.decode(), "--clear-groups"]
            store = root / "parent" / "store"
            for caller, prefix in callers.items():
                def door():
                    return subprocess.run([*prefix, "/bin/bash", str(entry), "/bin/bash", "infra/gate.sh"], cwd=tree, env=env, capture_output=True, check=False)
                # Held: no store at all (the gate runner mounts no /work),
                # and a store this caller can read with no selector in it.
                for state in ("absent", "held"):
                    with self.subTest(caller=caller, state=state):
                        if state == "held":
                            store.mkdir(parents=True, mode=0o755)
                            store.parent.chmod(0o755)
                            store.chmod(0o755)
                        result = door()
                        self.assertEqual(result.returncode, 0, (result.stdout, result.stderr))
                        self.assertIn(b"CANDIDATE-RAN", result.stdout)
                        self.assertEqual(list(scratch.iterdir()), [], "the door left its workspace behind")
                # Any selector, in a store this caller can read.
                for shape in ("generation", "refused", "regular-file"):
                    with self.subTest(caller=caller, selector=shape):
                        selector = store / "enabled"
                        if selector.is_symlink() or selector.exists():
                            selector.unlink()
                        if shape == "generation":
                            selector.symlink_to(store / "generations" / ("a" * 40))
                        elif shape == "refused":
                            selector.symlink_to("refused")
                        else:
                            selector.write_text("not a generation")
                        try:
                            result = door()
                        finally:
                            selector.unlink()
                        self.assertEqual(result.returncode, 75, (result.stdout, result.stderr))
                        self.assertNotIn(b"CANDIDATE-RAN", result.stdout, "an activated selector must never run the candidate as the gate uid")
                        self.assertIn(str(selector).encode(), result.stderr)
                        self.assertIn(b"isolation is on", result.stderr)
                        self.assertEqual(list(scratch.iterdir()), [], "a refusal bundles nothing")
                # Closed: the store, or its parent, refuses this caller a
                # search, whether or not a selector sits behind it.
                for veil, with_selector in (("store", False), ("store", True), ("ancestor", False)):
                    with self.subTest(caller=caller, closed=veil, selector=with_selector):
                        if with_selector:
                            (store / "enabled").symlink_to(store / "generations" / ("a" * 40))
                        closed = store if veil == "store" else store.parent
                        # Root-owned 0700 is the live store's shape for uid
                        # 65534; for the owner itself only mode 0 closes it.
                        closed.chmod(0o000 if caller == "self" else 0o700)
                        try:
                            seen = subprocess.run([*prefix, "/usr/bin/test", "-x", str(closed)], check=False)
                            if seen.returncode == 0:
                                self.skipTest(caller + " can search a mode-" + oct(closed.stat().st_mode & 0o777) + " directory (CAP_DAC_OVERRIDE), so no store is closed to it")
                            result = door()
                        finally:
                            closed.chmod(0o755)
                            if with_selector:
                                (store / "enabled").unlink()
                        self.assertEqual(result.returncode, 75, (result.stdout, result.stderr))
                        self.assertNotIn(b"CANDIDATE-RAN", result.stdout, "a store this caller cannot read is not the held state")
                        self.assertIn(str(store).encode(), result.stderr)
                        self.assertIn(b"cannot read", result.stderr)
                        self.assertEqual(list(scratch.iterdir()), [], "a refusal bundles nothing")
                store.rmdir()
                store.parent.rmdir()

    def test_every_reader_of_the_selector_spells_the_one_durable_store(self):
        repo = Path(__file__).resolve().parents[2]
        readers = [Path(__file__).with_name(n) for n in (*self.DOORS, self.HELD, self.UID_DOOR, "activate-control.py")]
        readers.append(repo / "crates/orchestrators/boss-cli/src/door_env.rs")
        for path in readers:
            with self.subTest(reader=path.name):
                text = path.read_text()
                self.assertIn(self.STORE, text)
                self.assertNotIn("/etc/boss-dev-control", text)
                self.assertNotIn("/opt/boss-dev-control", text)

    def test_each_source_door_routes_before_reading_candidate_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            store = self.published(root)
            (store / "enabled").symlink_to(store / "generations" / ("a" * 40))
            for name in self.DOORS:
                with self.subTest(door=name):
                    result = self.door(root, name)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, b"published:literal;touch /control\n")

    def test_a_refused_selector_stops_candidate_doors_and_keeps_trusted_reads(self):
        # Design a68fc42a D1 (David, 2026-10-10): when activation cannot be
        # re-established the selector is REFUSED, and candidate execution
        # stops with 75 naming the activation log, while boss-api and the
        # published CLI keep answering — the operator keeps the record.
        for shape in ("refused", "regular-file", "foreign-link"):
            with self.subTest(shape=shape), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                store = self.published(root)
                if shape == "refused":
                    (store / "enabled").symlink_to("refused")
                elif shape == "regular-file":
                    (store / "enabled").write_text("not a generation")
                else:
                    (store / "enabled").symlink_to(root / "elsewhere" / ("a" * 40))
                for name in ("wt-cargo", "wt-web"):
                    result = self.door(root, name)
                    self.assertEqual(result.returncode, 75, (name, result.stderr))
                    self.assertNotIn(b"published:", result.stdout)
                    self.assertIn(str(store / "activation.log").encode(), result.stderr)
                for extra, args in (({}, ("--built", "--version")), ({"BOSS_SHIM_BUILT": "1"}, ("--version",))):
                    result = self.door(root, "boss", extra, args)
                    self.assertEqual(result.returncode, 75, (extra, result.stderr))
                    self.assertIn(str(store / "activation.log").encode(), result.stderr)
                # Trusted reads fall through to the local door. A candidate
                # build in the target is never run, not even for --version.
                candidate = root / "scratch/target/debug/boss"
                candidate.parent.mkdir(parents=True)
                ran = root / "candidate-ran"
                candidate.write_text("#!/bin/sh\ntouch '" + str(ran) + "'\necho 'boss 0.1.0 built from unknown'\n")
                candidate.chmod(0o755)
                result = self.door(root, "boss", {"BOSS_JOBS_URL": "http://sor.invalid:7900", "BOSS_SHIM_IMAGE_STORE": str(root / "no-image"), "BOSS_SHIM_TARGET_ROOT": str(root / "scratch")}, ("orient",))
                self.assertNotIn(result.returncode, (0, 75), result.stderr)
                self.assertNotIn(b"published:", result.stdout)
                self.assertFalse(ran.exists(), "a refused selector must not run a candidate build, even to read its version")
                result = self.door(root, "boss-api")
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(b"usage: boss-api", result.stderr, "boss-api ran locally and answered")

    # A SELECTOR THIS CALLER CANNOT READ IS A REFUSAL (backlog 1fddbe13,
    # reviewer run 012c8dcf, 2026-10-19). activate-control.py keeps the
    # store root-owned 0700, so as uid 65534 — the documented
    # infra/dev/as-gate-uid.sh route — `[[ -e $store/enabled ]]` is FALSE
    # whether or not isolation is on: the stat fails with EACCES, and the
    # bash doors read that as "no selector" and ran the candidate here.
    # door_env::dev_child_route already refuses any error but NotFound;
    # the bash doors now agree. A store that does not exist at all (the
    # gate runner mounts no /work) stays the held state and runs locally.
    def blind(self):
        """The argv prefix that runs a door as a caller the store is closed
        to: as root, drop to the gate's uid (65534) the way
        infra/dev/as-gate-uid.sh does; already non-root (the gate itself),
        run as we are, and the store is closed with mode 0."""
        if os.geteuid() != 0:
            return []
        setpriv = shutil.which("setpriv")
        self.assertIsNotNone(setpriv, "as root this test needs setpriv to run a door as uid 65534; absent, it proves nothing")
        return [setpriv, "--reuid=65534", "--regid=65534", "--clear-groups"]

    def test_a_selector_the_caller_cannot_read_refuses_every_candidate_door(self):
        prefix = self.blind()
        for veil, selector in (("store", None), ("store", "generation"), ("ancestor", None)):
            with self.subTest(closed=veil, selector=selector), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                root.chmod(0o755)
                store = root / "parent" / "store"
                store.mkdir(parents=True)
                for name in (*self.DOORS, "dev-build"):
                    (store / name).write_text('#!/bin/bash\nprintf "published:%s\\n" "$@"\n')
                    (store / name).chmod(0o755)
                if selector == "generation":
                    (store / "enabled").symlink_to(store / "generations" / ("a" * 40))
                tree = root / "tree"
                (tree / "infra").mkdir(parents=True)
                (tree / "infra/gate.sh").write_text("echo CANDIDATE-RAN\n")
                entries = {}
                for name in (*self.DOORS, self.HELD):
                    source = Path(__file__).with_name(name).read_text()
                    self.assertEqual(source.count(self.STORE), 1, name + " spells the store once, in one assignment")
                    entry = root / (name + "-blind-entry")
                    entry.write_text(source.replace(self.STORE, str(store)))
                    entry.chmod(0o755)
                    entries[name] = entry
                closed = store if veil == "store" else store.parent
                # Root-owned 0700 is the live store's shape; as non-root the
                # owner's own mode-0 directory refuses the stat the same way.
                closed.chmod(0o700 if prefix else 0o000)
                env = {"PATH": "/usr/bin:/bin", "HOME": str(root)}
                try:
                    runs = {
                        self.HELD: (entries[self.HELD], ("infra/gate.sh", "--lint")),
                        "wt-cargo": (entries["wt-cargo"], ("build",)),
                        "wt-web": (entries["wt-web"], ("bun", "--version")),
                        "boss --built": (entries["boss"], ("--built", "--version")),
                    }
                    for name, (entry, args) in runs.items():
                        result = subprocess.run([*prefix, "/bin/bash", str(entry), *args], cwd=tree, env=env, capture_output=True, check=False)
                        self.assertEqual(result.returncode, 75, (name, result.stdout, result.stderr))
                        self.assertNotIn(b"CANDIDATE-RAN", result.stdout, name + " ran the candidate beside the credentials")
                        self.assertNotIn(b"published:", result.stdout, name + " forwarded on a selector it could not read")
                        self.assertIn(str(store).encode(), result.stderr, name + " names the store it could not read")
                        self.assertIn(b"cannot read", result.stderr, name + " says why it refused")
                finally:
                    closed.chmod(0o700)
        # The control: the same doors, the same caller, and NO store at all
        # (a host or runner without /work) is the held state — the parent
        # is searchable, so the selector's absence is proven, not assumed.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            root.chmod(0o755)
            tree = root / "tree"
            (tree / "infra").mkdir(parents=True)
            (tree / "infra/gate.sh").write_text("echo CANDIDATE-RAN\n")
            source = Path(__file__).with_name(self.HELD).read_text()
            entry = root / "held-blind-entry"
            entry.write_text(source.replace(self.STORE, str(root / "no-such" / "store")))
            entry.chmod(0o755)
            env = {"PATH": "/usr/bin:/bin", "HOME": str(root)}
            result = subprocess.run([*prefix, "/bin/bash", str(entry), "infra/gate.sh"], cwd=tree, env=env, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, b"CANDIDATE-RAN\n")


if __name__ == "__main__":
    unittest.main()
