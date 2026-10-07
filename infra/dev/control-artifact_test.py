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

    def test_published_bootstrap_validates_before_running_any_activation_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = root / "cli"
            generation = store / ("a" * 40)
            generation.mkdir(parents=True)
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
