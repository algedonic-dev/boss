"""Published artifact extraction is data validation, never tar execution."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
import subprocess
import sys
import resource


class ArtifactContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("control_artifact", Path(__file__).with_name("control-artifact.py"))
        cls.mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.mod)

    def archive(self, path, extra=None):
        data = b"published inert bytes"
        record = {"commit": "a" * 40, "files": {"bin/boss": {"sha256": hashlib.sha256(data).hexdigest(), "mode": 0o755}}}
        with tarfile.open(path, "w") as archive:
            for name, value in [("artifact.json", json.dumps(record).encode()), ("bin/boss", data)]:
                item = tarfile.TarInfo(name)
                item.size = len(value)
                item.mode = 0o755 if name == "bin/boss" else 0o644
                archive.addfile(item, io.BytesIO(value))
            if extra:
                archive.addfile(extra)

    def test_regular_verified_members_extract_without_executing_them(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.archive(root / "bundle.tar")
            self.mod.extract(root / "bundle.tar", root / "generation", "a" * 40)
            self.assertEqual((root / "generation/bin/boss").read_bytes(), b"published inert bytes")
            self.assertEqual((root / "generation/bin/boss").stat().st_mode & 0o777, 0o755)

    def test_config_inputs_validate_the_whole_large_archive_under_the_reclaim_bound(self):
        # f1a22bd6: config-inputs read(MAX_BYTES+1) tried a 512 MiB allocation
        # in the 256 MiB reclaim sidecar even on an already-installed generation.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            generation = root / ("a" * 40)
            generation.mkdir()
            images = b'{"worker_image":{"repository":"david/worker","tag":"test"},"postgres_image":{"repository":"david/postgres","tag":"test"},"forge_repository":"david/boss"}'
            estate = b'forge_registry="registry.invalid:3000"\nforge_url="http://forge.invalid:3000"\n'
            helper = Path(__file__).with_name("control-artifact.py")
            members = {"infra/dev/control-artifact.py": helper.read_bytes(),
                       "infra/dev/control-images.json": images,
                       "infra/estate/estate.toml": estate}
            binary = root / "inert-binary"
            with binary.open("wb") as stream:
                stream.truncate(96 * 1024 * 1024)
            hasher = hashlib.sha256()
            with binary.open("rb") as stream:
                while chunk := stream.read(65536): hasher.update(chunk)
            record = {"commit": "a" * 40, "files": {
                **{name: {"sha256": hashlib.sha256(raw).hexdigest(), "mode": 0o644} for name, raw in members.items()},
                "bin/boss": {"sha256": hasher.hexdigest(), "mode": 0o755}}}
            def bundle(corrupt=False):
                with tarfile.open(generation / "dev-control.tar", "w") as archive:
                    for name, raw in [("artifact.json", json.dumps(record).encode()), *members.items()]:
                        item = tarfile.TarInfo(name); item.size = len(raw); item.mode = 0o644
                        archive.addfile(item, io.BytesIO(raw))
                    item = tarfile.TarInfo("bin/boss"); item.size = binary.stat().st_size; item.mode = 0o755
                    with binary.open("rb") as stream: archive.addfile(item, stream)
                if corrupt:
                    with (generation / "dev-control.tar").open("r+b") as stream:
                        stream.seek(-16384, 2); stream.write(b"corrupt irrelevant executable")
            def bounded():
                limit = 256 * 1024 * 1024
                resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
            for corrupt in (False, True):
                bundle(corrupt)
                result = subprocess.run([sys.executable, "-I", str(helper), "config-inputs", "a" * 40, "--cli-store", str(root)], capture_output=True, preexec_fn=bounded)
                if corrupt:
                    self.assertNotEqual(result.returncode, 0, "an irrelevant binary's digest is still part of the closure")
                    self.assertEqual(result.stdout, b"")
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout)["worker_image"], "registry.invalid:3000/david/worker:test")
                    print("config-inputs: 256 MiB address-space bound, child peak RSS KiB", resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)

    def test_selective_retention_still_refuses_conflicting_unretained_file_paths(self):
        for names in [("ignored", "ignored/child"), ("ignored/child", "ignored")]:
            with self.subTest(names=names), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                raw = b"inert"
                record = {"commit": "a" * 40, "files": {name: {"sha256": hashlib.sha256(raw).hexdigest(), "mode": 0o644} for name in names}}
                with tarfile.open(root / "bundle.tar", "w") as archive:
                    for name, value in [("artifact.json", json.dumps(record).encode()), *[(name, raw) for name in names]]:
                        item = tarfile.TarInfo(name); item.size = len(value); item.mode = 0o644
                        archive.addfile(item, io.BytesIO(value))
                with self.assertRaises(ValueError):
                    self.mod.extract(root / "bundle.tar", root / "generation", "a" * 40, retain=set())
                self.assertFalse((root / "generation").exists())

    def test_pack_carries_only_the_declared_source_closure_and_published_tools(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binaries = root / "binaries"
            binaries.mkdir()
            for name in ("boss", "kubectl"):
                (binaries / name).write_bytes(b"published tool bytes")
            source = Path(__file__).resolve().parents[2]
            self.mod.pack(source, binaries, root / "bundle.tar", "a" * 40)
            self.mod.extract(root / "bundle.tar", root / "generation", "a" * 40)
            record = json.loads((root / "generation/artifact.json").read_text())
            declared = {line for line in (source / "infra/dev/control-files.txt").read_text().splitlines() if line and not line.startswith("#")}
            self.assertEqual(set(record["files"]), declared | {"bin/boss", "bin/kubectl"})
            self.assertNotIn("target", set(record["files"]))
            self.assertIn("infra/consist-worker/declaration.py", record["files"])
            # Activation reads the preflight receipt through the SAME
            # generation's reader (b50b8953); a closure without it cannot
            # activate at all.
            self.assertIn("infra/dev/preflight-control.py", record["files"])
            loader = importlib.util.spec_from_file_location("packed_dev_build", root / "generation/infra/dev/dev-build.py")
            controller = importlib.util.module_from_spec(loader)
            sys.modules[loader.name] = controller
            loader.loader.exec_module(controller)
            native = json.loads((source / "infra/dev/tests/fixtures/defaulted-dev-job.json").read_text())
            self.assertTrue(controller.native_job_matches(native))
            native["spec"]["activeDeadlineSeconds"] = 999999
            self.assertFalse(controller.native_job_matches(native))

    def test_existing_generation_refuses_a_linked_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.archive(root / "bundle.tar")
            generation = root / "generation"
            self.mod.extract(root / "bundle.tar", generation, "a" * 40)
            metadata = generation / "artifact.json"
            raw = metadata.read_bytes()
            metadata.unlink()
            (root / "candidate-manifest").write_bytes(raw)
            metadata.symlink_to(root / "candidate-manifest")
            with self.assertRaises((ValueError, OSError)):
                self.mod.validate_generation(generation, "a" * 40)

    def published_generation(self, root, binaries=("boss", "kubectl")):
        """An extracted generation whose manifest lists exactly `binaries`."""
        data = b"published inert bytes"
        record = {"commit": "a" * 40, "files": {"bin/" + name: {"sha256": hashlib.sha256(data).hexdigest(), "mode": 0o755} for name in binaries}}
        with tarfile.open(root / "bundle.tar", "w") as archive:
            for name, value, mode in [("artifact.json", json.dumps(record).encode(), 0o644), *(("bin/" + name, data, 0o755) for name in binaries)]:
                item = tarfile.TarInfo(name)
                item.size = len(value)
                item.mode = mode
                archive.addfile(item, io.BytesIO(value))
        generation = root / "generation"
        self.mod.extract(root / "bundle.tar", generation, "a" * 40)
        generation.chmod(0o700)
        return generation

    def test_a_generation_bin_holding_anything_but_the_listed_binaries_is_refused(self):
        # Backlog e17b0135 (RELEASE review 7b6bc631 of car eb28f2c0): the
        # trusted boss door puts <generation>/bin first on PATH, and this
        # check hash-read only the files the manifest lists — so a bin/git
        # planted beside them passed and resolved as `git` under that PATH.
        # bin/ now holds exactly the manifest's binaries, which are exactly
        # the published pair: an extra file, a link (to anything, even a
        # listed binary), a directory, a missing binary, or a manifest
        # naming a third or fewer, each refuses the whole generation.
        with tempfile.TemporaryDirectory() as tmp:
            self.mod.validate_generation(self.published_generation(Path(tmp)), "a" * 40)
        plants = {
            "an extra regular file": lambda b: (b / "git").write_bytes(b"#!/bin/sh\n"),
            "a hidden extra file": lambda b: (b / ".git").write_bytes(b"#!/bin/sh\n"),
            "a link to a listed binary": lambda b: (b / "git").symlink_to(b / "boss"),
            "a dangling link": lambda b: (b / "git").symlink_to(b / "absent"),
            "a directory": lambda b: (b / "git").mkdir(),
            "a missing listed binary": lambda b: (b / "kubectl").unlink(),
        }
        for label, plant in plants.items():
            with self.subTest(plant=label), tempfile.TemporaryDirectory() as tmp:
                generation = self.published_generation(Path(tmp))
                plant(generation / "bin")
                with self.assertRaisesRegex(ValueError, "bin/"):
                    self.mod.validate_generation(generation, "a" * 40)
        for binaries in [("boss",), ("boss", "kubectl", "git")]:
            with self.subTest(manifest=binaries), tempfile.TemporaryDirectory() as tmp:
                generation = self.published_generation(Path(tmp), binaries)
                with self.assertRaisesRegex(ValueError, "bin/"):
                    self.mod.validate_generation(generation, "a" * 40)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            generation = self.published_generation(root)
            (generation / "bin").rename(root / "foreign-bin")
            (generation / "bin").symlink_to(root / "foreign-bin", target_is_directory=True)
            with self.assertRaises(ValueError):
                self.mod.validate_generation(generation, "a" * 40)

    def test_published_bootstrap_validates_before_running_any_activation_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = root / "cli"
            generation = store / ("a" * 40)
            generation.mkdir(parents=True)
            (store / "current").symlink_to("a" * 40)
            marker = root / "activated"
            code = ("import pathlib,sys,tarfile\n"
                    "private=pathlib.Path(sys.argv[sys.argv.index('--cli-store')+1])\n"
                    "assert private != pathlib.Path(" + repr(str(store)) + ")\n"
                    "assert (private/sys.argv[1]/'dev-control.tar').is_file()\n"
                    "pathlib.Path(" + repr(str(generation / "dev-control.tar")) + ").write_bytes(b'changed after capture')\n"
                    "with tarfile.open(private/sys.argv[1]/'dev-control.tar') as archive: assert archive.getmember('artifact.json').isfile()\n"
                    "pathlib.Path(sys.argv[-1]).write_text('published')\n").encode()
            payload = {"infra/dev/activate-control.py": code, "infra/dev/control-artifact.py": Path(__file__).with_name("control-artifact.py").read_bytes()}
            def bundle(fault=None):
                members = dict(payload)
                if fault == "bootstrap":
                    members["infra/dev/control-artifact.py"] += b"#different published bootstrap"
                record = {"commit": "b" * 40 if fault == "head" else "a" * 40, "files": {n: {"sha256": hashlib.sha256(v).hexdigest(), "mode": 0o644} for n, v in members.items()}}
                with tarfile.open(generation / "dev-control.tar", "w") as archive:
                    for name, raw in [("artifact.json", json.dumps(record).encode()), *members.items()]:
                        if fault == "partial" and name.endswith("activate-control.py"):
                            continue
                        item = tarfile.TarInfo(name)
                        item.mode = 0o755 if fault == "mode" else 0o644
                        if fault == "link" and name.endswith("activate-control.py"):
                            item.type = tarfile.SYMTYPE
                            item.linkname = str(marker)
                            archive.addfile(item)
                        else:
                            if fault == "corrupt" and name.endswith("activate-control.py"):
                                raw += b"#changed"
                            item.size = len(raw)
                            archive.addfile(item, io.BytesIO(raw))
            for fault in ("corrupt", "partial", "link", "head", "mode", "bootstrap", None):
                bundle(fault)
                result = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("control-artifact.py")), "activate", "a" * 40, "--cli-store", str(store), "--config", str(marker)], capture_output=True)
                if fault:
                    self.assertNotEqual(result.returncode, 0, (fault, result.stderr))
                    self.assertFalse(marker.exists(), fault)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(marker.read_text(), "published")

    def test_published_bootstrap_hands_a_follow_main_pass_its_store_and_no_config(self):
        # Design a68fc42a D6: the hourly installer re-activates through
        # the NEW generation's own bootstrap, which validates that
        # generation's archive first, exactly as an explicit activation.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = root / "cli"
            generation = store / ("a" * 40)
            generation.mkdir(parents=True)
            seen = root / "argv.json"
            code = ("import json,sys\n"
                    "open(" + repr(str(seen)) + ",'w').write(json.dumps(sys.argv[1:]))\n").encode()
            members = {"infra/dev/activate-control.py": code, "infra/dev/control-artifact.py": Path(__file__).with_name("control-artifact.py").read_bytes()}
            record = {"commit": "a" * 40, "files": {n: {"sha256": hashlib.sha256(v).hexdigest(), "mode": 0o644} for n, v in members.items()}}
            with tarfile.open(generation / "dev-control.tar", "w") as archive:
                for name, raw in [("artifact.json", json.dumps(record).encode()), *members.items()]:
                    item = tarfile.TarInfo(name)
                    item.mode = 0o644
                    item.size = len(raw)
                    archive.addfile(item, io.BytesIO(raw))
            control = root / "dev-control"
            for verb, flags, mode in [("activate", ["--reactivate"], "--reactivate"),
                                       ("deactivate", [], "--deactivate")]:
                result = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("control-artifact.py")), verb, "a" * 40, "--cli-store", str(store), "--store", str(control), *flags], capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                argv = json.loads(seen.read_text())
                self.assertEqual(argv[0], "a" * 40)
                self.assertEqual(argv[argv.index("--store") + 1], str(control))
                self.assertIn(mode, argv)
                self.assertNotIn("--config", argv)
                if mode == "--reactivate":
                    self.assertEqual(argv[argv.index("--prepared-config") + 1], str(generation / "config.json"))
            result = subprocess.run([sys.executable, "-I", str(Path(__file__).with_name("control-artifact.py")), "activate", "a" * 40, "--cli-store", str(store)], capture_output=True)
            self.assertNotEqual(result.returncode, 0, "an activation names its config or follows main; never neither")

    def published(self, root, members):
        store = root / "cli"
        generation = store / ("a" * 40)
        generation.mkdir(parents=True)
        members = dict(members, **{"infra/dev/control-artifact.py": Path(__file__).with_name("control-artifact.py").read_bytes()})
        record = {"commit": "a" * 40, "files": {n: {"sha256": hashlib.sha256(v).hexdigest(), "mode": 0o644} for n, v in members.items()}}
        with tarfile.open(generation / "dev-control.tar", "w") as archive:
            for name, raw in [("artifact.json", json.dumps(record).encode()), *members.items()]:
                item = tarfile.TarInfo(name)
                item.mode = 0o644
                item.size = len(raw)
                archive.addfile(item, io.BytesIO(raw))
        (generation / "config.json").write_text('{"prepared": true}')
        return store, generation

    def test_an_explicit_activation_names_the_main_the_image_store_holds(self):
        # b50b8953: the receipt is judged against the main the installer
        # last published; the bootstrap is what knows the real store.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seen = root / "argv.json"
            code = "import json,sys\nopen(" + repr(str(seen)) + ",'w').write(json.dumps(sys.argv[1:]))\n"
            store, _ = self.published(root, {"infra/dev/activate-control.py": code.encode()})
            argv = [sys.executable, "-I", str(Path(__file__).with_name("control-artifact.py")), "activate", "a" * 40,
                    "--cli-store", str(store), "--config", str(root / "config.json")]
            result = subprocess.run(argv, capture_output=True)
            self.assertNotEqual(result.returncode, 0, "no current main, no activation")
            self.assertFalse(seen.exists(), "refused before the published activation runs")
            (store / "current").symlink_to("b" * 40)
            result = subprocess.run(argv, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            passed = json.loads(seen.read_text())
            self.assertEqual(passed[passed.index("--main") + 1], "b" * 40)
            self.assertEqual(passed[-2:], ["--config", str((root / "config.json").resolve())])

    def test_the_preflight_verb_runs_the_published_rehearsal_with_the_prepared_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seen = root / "seen.json"
            code = ("import json,os,pathlib,sys\n"
                    "here=pathlib.Path(__file__).resolve().parents[2]\n"
                    "open(" + repr(str(seen)) + ",'w').write(json.dumps({'argv': sys.argv[1:], 'config': (here/'config.json').read_text(),"
                    " 'env': sorted(os.environ)}))\n"
                    "sys.exit(int(os.environ.get('BOSS_ACTOR') == 'refuse'))\n")
            store, _ = self.published(root, {"infra/dev/preflight-control.py": code.encode()})
            (store / "current").symlink_to("b" * 40)
            control = root / "dev-control"
            argv = [sys.executable, "-I", str(Path(__file__).with_name("control-artifact.py")), "preflight", "a" * 40,
                    "--cli-store", str(store), "--store", str(control), "--workers", "3",
                    "--record-on", "b3071bf4-5996-4cf8-96db-e4b23b2f67a1"]
            env = {"PATH": "/usr/bin:/bin", "BOSS_ACTOR": "agent-test", "KUBERNETES_SERVICE_HOST": "10.0.0.1", "OPENAI_API_KEY": "never"}
            result = subprocess.run(argv, capture_output=True, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            observed = json.loads(seen.read_text())
            passed = observed["argv"]
            self.assertEqual(passed[0], "a" * 40)
            for flag, value in (("--main", "b" * 40), ("--cli-store", str(store.resolve())), ("--store", str(control.resolve())),
                                ("--workers", "3"), ("--record-on", "b3071bf4-5996-4cf8-96db-e4b23b2f67a1")):
                self.assertEqual(passed[passed.index(flag) + 1], value, flag)
            self.assertEqual(observed["config"], '{"prepared": true}', "the rehearsal starts workers from the prepared config")
            self.assertIn("BOSS_ACTOR", observed["env"])
            self.assertIn("KUBERNETES_SERVICE_HOST", observed["env"])
            self.assertNotIn("OPENAI_API_KEY", observed["env"])
            result = subprocess.run(argv, capture_output=True, env=dict(env, BOSS_ACTOR="refuse"))
            self.assertNotEqual(result.returncode, 0, "a failed preflight is a failed verb")

    def test_bootstrap_rejects_a_linked_installed_archive_without_running_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            generation = root / ("a" * 40)
            generation.mkdir()
            (root / "worker.tar").write_bytes(b"candidate bytes")
            (generation / "dev-control.tar").symlink_to(root / "worker.tar")
            with self.assertRaises(OSError):
                self.mod.bootstrap(root, "a" * 40, root / "config")

    def test_path_escape_symlink_extra_member_and_wrong_commit_leave_destination_absent(self):
        for name, kind in [("../escape", tarfile.REGTYPE), ("bin/link", tarfile.SYMTYPE), ("undeclared", tarfile.REGTYPE)]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                extra = tarfile.TarInfo(name)
                extra.type = kind
                extra.linkname = "/control"
                self.archive(root / "bundle.tar", extra)
                with self.assertRaises(ValueError):
                    self.mod.extract(root / "bundle.tar", root / "generation", "a" * 40)
                self.assertFalse((root / "generation").exists())
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.archive(root / "bundle.tar")
            with self.assertRaises(ValueError):
                self.mod.extract(root / "bundle.tar", root / "generation", "b" * 40)
            self.assertFalse((root / "generation").exists())


if __name__ == "__main__":
    unittest.main()
