"""The installed reader identity is evidence; a tag or raw version is not."""
import importlib.util
import json
import pathlib
import os
import tempfile
import subprocess
import contextlib
import io
import re
import shutil
import sys
from unittest.mock import patch
import unittest

SOURCE = pathlib.Path(__file__).resolve().parents[1] / "admission-reader-identity.py"
spec = importlib.util.spec_from_file_location("bootstrap", SOURCE.with_name("admission-identity-bootstrap.py"))
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)
d = bootstrap.load_identity()


class Identity(unittest.TestCase):
    def test_bootstrap_refusal_conserves_fixed_file_failure_without_diagnostics(self):
        for absent in [False, True]:
            for internal in [False, True]:
                with self.subTest(absent=absent, internal=internal), tempfile.TemporaryDirectory() as directory:
                    entry = pathlib.Path(directory) / "admission-identity-bootstrap.py"
                    shutil.copyfile(SOURCE.with_name(entry.name), entry)
                    if not absent:
                        entry.with_name(SOURCE.name).write_bytes(b"")
                    argv = ["python3", "-B", str(entry), *(["--inside"] if internal else [])]
                    result = subprocess.run(argv, capture_output=True, check=False)
                    self.assertEqual(result.returncode, 0 if internal else 4)
                    self.assertEqual(result.stderr, b"")
                    self.assertEqual(json.loads(result.stdout)["reason"],
                                     "identity_file_unavailable" if absent else "identity_file_bound")

    def test_uncaptured_entry_and_forged_globals_cannot_open_native_port(self):
        for forged in [False, True]:
            with self.subTest(forged=forged):
                specification = importlib.util.spec_from_file_location("uncaptured_identity", SOURCE)
                module = importlib.util.module_from_spec(specification)
                specification.loader.exec_module(module)
                if forged:
                    module.COMPILED_IDENTITY_CAPTURE = module.snapshot(SOURCE, 1024 * 1024)
                with patch.object(module.native, "NativeReadPort", side_effect=AssertionError("uncaptured entry reached native port")), \
                        patch.dict(os.environ, BOSS_READER_IMAGE="registry.invalid/fixture"):
                    with self.assertRaises(module.Unavailable):
                        module.host()

    def test_compiled_identity_refuses_replacement_before_first_snapshot(self):
        for boundary in ["inside", "host"]:
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as directory:
                scratch = pathlib.Path(directory)
                target = scratch / "infra/forge/admission-reader-identity.py"
                for relative in ["admission-reader-identity.py", *d.METHODS.values()]:
                    source = SOURCE.parent / relative
                    destination = target.parent / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, destination)
                bootstrap = SOURCE.with_name("admission-identity-bootstrap.py")
                if bootstrap.exists():
                    shutil.copyfile(bootstrap, target.with_name(bootstrap.name))
                def load(name):
                    bootstrap_path = target.with_name("admission-identity-bootstrap.py")
                    path = bootstrap_path if bootstrap_path.exists() else target
                    specification = importlib.util.spec_from_file_location(name, path)
                    module = importlib.util.module_from_spec(specification)
                    specification.loader.exec_module(module)
                    return module.load_identity() if bootstrap_path.exists() else module
                loaded = load("compiled_original")
                original = target.read_bytes()
                original_hash = d.hashlib.sha256(original).hexdigest()
                binary = scratch / "nonsecret-talosctl"
                binary.write_bytes(b"nonsecret fixture executable")
                real_snapshot = loaded.snapshot
                def redirected(path, limit):
                    if str(path) == "/talosctl": path = binary
                    elif str(path).startswith("/methods/"): path = target.parent / loaded.METHODS[path.name]
                    return real_snapshot(path, limit)
                raw = (b"Client:\n\tTag: v1.13.8\n\tSHA: 3de49322\n\tBuilt: \n"
                       b"\tGo version: go1.26.5\n\tOS/Arch: linux/amd64\n")
                class ClientPort:
                    def __init__(self, timeout):
                        self.timeout = timeout
                    def read_bytes(self, argv):
                        self_test.assertEqual(argv, ["/talosctl", "--talosconfig=/dev/null", "version", "--client"])
                        return raw
                self_test = self
                with patch.object(loaded, "snapshot", redirected), patch.object(loaded.native, "NativeReadPort", ClientPort), \
                        patch.dict(os.environ, BOSS_READER_IMAGE_ID="sha256:" + "a" * 64):
                    positive = loaded.inside()
                    self.assertEqual(positive["methods"]["identity"], original_hash)
                    replacement = original + b'\ndef host():\n    raise Unavailable("identity_method_changed")\n'
                    target.write_bytes(replacement)
                    current = load("compiled_replacement")
                    with self.assertRaises(current.Unavailable):
                        current.host()
                    if boundary == "inside":
                        with self.assertRaises(loaded.Unavailable):
                            loaded.inside()
                    else:
                        class HostPort:
                            def __init__(self, timeout): self.timeout = timeout
                            def read_bytes(self, argv):
                                if argv[3:5] == ["image", "inspect"]:
                                    return ("sha256:" + "a" * 64 + "\n").encode()
                                self_test.assertEqual(argv[3], "run")
                                with patch.object(current, "snapshot", redirected), patch.object(current.native, "NativeReadPort", ClientPort):
                                    return json.dumps(current.inside()).encode()
                        with patch.object(loaded.native, "NativeReadPort", HostPort), \
                                patch.dict(os.environ, BOSS_READER_IMAGE="registry.invalid/fixture", BOSS_TALOSCTL=str(binary)):
                            with self.assertRaises(loaded.Unavailable):
                                loaded.host()

    def test_internal_transport_retains_only_a_valid_finite_unavailable_envelope(self):
        unavailable = {"schema": d.SCHEMA, "scope": "installed_reader_identity", "state": "unavailable",
                       "history_verdict": "unavailable", "reason": "client_version_unavailable"}
        self.assertEqual(d.validate_report(json.dumps(unavailable).encode(), "sha256:" + "a" * 64, {}),
                         unavailable)
        for changed in [dict(unavailable, reason="PRIVATE-DIAGNOSTIC"),
                        dict(unavailable, raw="PRIVATE-DIAGNOSTIC"), dict(unavailable, history_verdict="clean")]:
            with self.assertRaises(d.Unavailable):
                d.validate_report(json.dumps(changed).encode(), "sha256:" + "a" * 64, {})
        duplicate = json.dumps(unavailable)[:-1] + ',"reason":"client_version_unavailable"}'
        with self.assertRaises(d.Unavailable):
            d.validate_report(duplicate.encode(), "sha256:" + "a" * 64, {})
        with patch.object(d, "inside", side_effect=d.Unavailable("client_version_unavailable")), \
                patch.object(d.sys, "argv", ["identity", "--inside"]):
            captured = io.StringIO()
            with contextlib.redirect_stdout(captured):
                self.assertEqual(d.main(), 0)
            self.assertEqual(json.loads(captured.getvalue()), unavailable)

    def test_container_executes_only_the_bound_client_and_refuses_changed_bytes(self):
        methods = {name: "b" * 64 for name in d.METHODS}
        methods["source"] = d.NATIVE_SHA
        methods["identity"] = d.COMPILED_IDENTITY_CAPTURE["sha256"]
        seen = []
        raw = (b"Client:\n\tTag: v1.13.8\n\tSHA: 3de49322\n\tBuilt: \n"
               b"\tGo version: go1.26.5\n\tOS/Arch: linux/amd64\n")
        class Port:
            def __init__(self, timeout):
                self.timeout = timeout
            def read_bytes(self, argv):
                seen.append(argv)
                return raw
        def fingerprint(path, limit):
            if str(path) == "/methods/identity":
                return dict(d.COMPILED_IDENTITY_CAPTURE)
            if str(path) == "/talosctl":
                sha = "c" * 64
            elif str(path).startswith("/methods/"):
                sha = methods[path.name]
            else:
                sha = methods["identity"]
            return {"sha256": sha, "identity": (1, 2, 3, 4, 5)}
        with patch.object(d, "snapshot", fingerprint), patch.object(d.native, "NativeReadPort", Port), \
                patch.dict(os.environ, BOSS_READER_IMAGE_ID="sha256:" + "a" * 64):
            result = d.inside()
            self.assertEqual(result["methods"], methods)
            self.assertEqual(seen, [["/talosctl", "--talosconfig=/dev/null", "version", "--client"]])
            original = d.method_hashes
            count = []
            def changing(inside=False):
                count.append(True)
                captures = {key: {"sha256": sha, "identity": (1, 2, 3, 4, 5)} for key, sha in methods.items()}
                captures["identity"] = dict(d.COMPILED_IDENTITY_CAPTURE)
                return captures if len(count) == 1 else dict(captures, target={"sha256": methods["target"], "identity": (1, 2, 3, 4, 6)})
            with patch.object(d, "method_snapshots", changing):
                with self.assertRaises(d.Unavailable):
                    d.inside()
            self.assertEqual(original(True), methods)

    def test_native_host_door_binds_consumed_mounts_and_sanitizes_all_refusals(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = pathlib.Path(directory)
            binary = scratch / "talosctl"
            binary.write_bytes(b"nonsecret-executable-fixture")
            sudo = scratch / "sudo"
            sudo.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
args=sys.argv[1:]
with open(os.environ["CALLS"],"a") as stream: stream.write(json.dumps(args)+"\\n")
if args[:4]==["-n","docker","image","inspect"]:
    print("sha256:"+"a"*64)
else:
    mode=os.environ.get("FIXTURE_MODE","")
    if mode=="stderr": print("PRIVATE-DIAGNOSTIC",file=sys.stderr)
    if mode=="overflow": print("PRIVATE"*200000)
    elif mode=="raw": print('{"raw":"PRIVATE"}')
    elif mode.startswith("inner_"):
        value={"schema":"boss.admission-reader-identity.v1","scope":"installed_reader_identity",
               "state":"unavailable","history_verdict":"unavailable","reason":"client_version_unavailable"}
        if mode=="inner_unknown": value["reason"]="PRIVATE-DIAGNOSTIC"
        if mode=="inner_extra": value["raw"]="PRIVATE-DIAGNOSTIC"
        if mode=="inner_diagnostic": print("PRIVATE-DIAGNOSTIC",file=sys.stderr)
        text=json.dumps(value)
        if mode=="inner_duplicate": text=text[:-1]+',"reason":"client_version_unavailable"}'
        if mode=="inner_malformed": text="{"
        print(text)
    else: print(pathlib.Path(os.environ["REPORT"]).read_text())
    if mode=="changed": pathlib.Path(os.environ["BOSS_TALOSCTL"]).write_bytes(b"changed")
    if mode=="same_bytes_replaced":
        path=pathlib.Path(os.environ["BOSS_TALOSCTL"])
        other=path.with_suffix(".next")
        other.write_bytes(path.read_bytes())
        other.replace(path)
    if mode=="same_inode_restored":
        path=pathlib.Path(os.environ["BOSS_TALOSCTL"])
        other=path.with_suffix(".old")
        path.rename(other)
        other.rename(path)
    if mode=="failure": sys.exit(137)
''')
            sudo.chmod(0o755)
            report = {"schema": d.SCHEMA, "scope": "installed_reader_identity",
                      "state": "available", "history_verdict": "unavailable",
                      "image_id": "sha256:" + "a" * 64,
                      "talos_sha256": d.fingerprint(binary, 1024),
                      "client": {"version": "v1.13.8", "revision": "3de49322",
                                 "go_version": "go1.26.5", "os_arch": "linux/amd64"},
                      "methods": d.method_hashes()}
            answer = scratch / "answer.json"
            answer.write_text(json.dumps(report))
            calls = scratch / "calls.jsonl"
            environment = dict(os.environ, PATH=str(scratch) + ":" + os.environ["PATH"],
                               BOSS_TALOSCTL=str(binary), BOSS_FORGE_REGISTRY_HOST="registry.invalid",
                               BOSS_SOR_ENV=str(scratch / "absent.env"), CALLS=str(calls), REPORT=str(answer))
            door = SOURCE.with_name("probe-admission-reader-identity.sh")
            good = subprocess.run(["bash", str(door)], env=environment, capture_output=True, check=False)
            self.assertEqual(good.returncode, 0, good.stderr)
            self.assertEqual(json.loads(good.stdout), report)
            invocation = [json.loads(line) for line in calls.read_text().splitlines()][1]
            self.assertIn(report["image_id"], invocation)
            # The container entry is the capturing bootstrap, never the identity file itself.
            self.assertEqual(invocation[-4:], ["python3", "-B", "/identity/admission-identity-bootstrap.py", "--inside"])
            for pair in [("--network", "none"), ("--memory", "256m"),
                         ("--memory-swap", "256m"), ("--pids-limit", "64"),
                         ("--cap-drop", "ALL"), ("--security-opt", "no-new-privileges")]:
                self.assertEqual(invocation[invocation.index(pair[0]) + 1], pair[1])
            self.assertIn("--read-only", invocation)
            self.assertIn("--pull=never", invocation,
                          "the credential-free read must refuse an evicted image instead of pulling")
            joined = " ".join(invocation)
            for forbidden in ["dst=/tc", "dst=/kc", "--network host", " pull ", " create "]:
                self.assertNotIn(forbidden, joined)
            for mode in ["stderr", "overflow", "raw", "failure", "same_bytes_replaced", "same_inode_restored", "changed",
                         "inner_unavailable", "inner_unknown", "inner_extra", "inner_duplicate", "inner_malformed", "inner_diagnostic"]:
                binary.write_bytes(b"nonsecret-executable-fixture")
                result = subprocess.run(["bash", str(door)], env=dict(environment, FIXTURE_MODE=mode),
                                        capture_output=True, check=False)
                self.assertEqual(result.returncode, 4, mode)
                self.assertEqual(result.stderr, b"", mode)
                self.assertNotIn(b"PRIVATE", result.stdout, mode)
                self.assertEqual(json.loads(result.stdout)["state"], "unavailable")
                expected = {"stderr": "native_read_diagnostic", "overflow": "native_read_size",
                            "raw": "identity_report_unavailable", "failure": "native_read_failed",
                            "changed": "identity_file_changed", "same_bytes_replaced": "identity_file_changed",
                            "same_inode_restored": "identity_file_changed", "inner_unavailable": "client_version_unavailable",
                            "inner_unknown": "identity_report_unavailable", "inner_extra": "identity_report_unavailable",
                            "inner_duplicate": "identity_report_unavailable", "inner_malformed": "identity_report_unavailable",
                            "inner_diagnostic": "native_read_diagnostic"}
                self.assertEqual(json.loads(result.stdout)["reason"], expected[mode])
            result = subprocess.run(["bash", str(door), "/arbitrary/path"], env=environment,
                                    capture_output=True, check=False)
            self.assertEqual(result.returncode, 4)
            self.assertEqual(json.loads(result.stdout)["state"], "unavailable")

    def test_client_version_is_whole_parsed_client_only_output(self):
        raw = (b"Client:\n\tTag:         v1.13.8\n\tSHA:         3de49322\n"
               b"\tBuilt:       \n\tGo version:  go1.26.5\n\tOS/Arch:     linux/amd64\n")
        self.assertEqual(d.client_version(raw), {
            "version": "v1.13.8", "revision": "3de49322", "go_version": "go1.26.5",
            "os_arch": "linux/amd64"})
        for changed in [b"", raw + b"Server:\nPRIVATE\n", raw.replace(b"v1.13.8", b"PRIVATE"),
                        raw + b"\tTag: v1.13.8\n", raw.replace(b"Built:       ", b"Built: PRIVATE"),
                        raw.replace(b"linux/amd64", b"darwin/arm64")]:
            with self.assertRaises(d.Unavailable):
                d.client_version(changed)

    def test_receipt_requires_exact_methods_and_immutable_image_and_binary(self):
        methods = {name: "b" * 64 for name in d.METHODS}
        report = {"schema": d.SCHEMA, "scope": "installed_reader_identity",
                  "state": "available", "history_verdict": "unavailable",
                  "image_id": "sha256:" + "a" * 64, "talos_sha256": "c" * 64,
                  "client": {"version": "v1.13.8", "revision": "3de49322",
                             "go_version": "go1.26.5", "os_arch": "linux/amd64"},
                  "methods": methods}
        self.assertEqual(d.validate_report(json.dumps(report).encode(), report["image_id"], methods), report)
        for key, value in [("image_id", "tag:latest"), ("talos_sha256", ""),
                           ("methods", {}), ("history_verdict", "clean"),
                           ("client", {"version": "PRIVATE"}), ("raw", "PRIVATE")]:
            with self.assertRaises(d.Unavailable):
                d.validate_report(json.dumps(dict(report, **{key: value})).encode(), report["image_id"], methods)
        with self.assertRaises(d.Unavailable):
            d.validate_report(json.dumps(report).encode() * 2, report["image_id"], methods)


ROOT = SOURCE.parents[2]
IMAGE = "sha256:" + "a" * 64
CLIENT = {"version": "v1.13.8", "revision": "3de49322", "go_version": "go1.26.5", "os_arch": "linux/amd64"}
RAW_CLIENT = (b"Client:\n\tTag: v1.13.8\n\tSHA: 3de49322\n\tBuilt: \n"
              b"\tGo version: go1.26.5\n\tOS/Arch: linux/amd64\n")
# The two registered verbs are the only roots named by hand; every other
# member of the closure is derived from what those entries actually run.
VERBS = ["probe-admission-reader-identity.json", "probe-admission-source-access.json"]
OPEN_TRACER = '''import importlib.util, os, sys
seen = []
sys.addaudithook(lambda event, args: seen.append(os.fspath(args[0]))
                 if event == "open" and not isinstance(args[0], int) else None)
spec = importlib.util.spec_from_file_location("traced_bootstrap", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.load_identity()
print("\\n".join(str(item) for item in seen))
'''


def consumed_closure(root):
    """Repo files the two doors execute or hand to their runtime, DERIVED.

    Review c6894a64 B1: infra/lib/sor-reader.sh is sourced by sor.sh and was
    in no hand-kept list. Three derivations, no list of members: each verb's
    own argv names its door; bash's xtrace of that door (on a dedicated fd,
    because the doors source with 2>/dev/null) names every file it sources or
    passes to a command; python's open audit names what the bootstrap reads
    to compile the identity module. Tools are stubbed: no docker, sudo,
    network or credential is reached.
    """
    root = root.resolve()
    consumed, traces = set(), {}
    with tempfile.TemporaryDirectory() as directory:
        scratch = pathlib.Path(directory)
        stubs = scratch / "bin"
        stubs.mkdir()
        for name, body in {"python3": "exit 9", "sudo": "printf '{}\\n'", "jq": "printf 'true\\n'",
                           "curl": "printf '{}\\n200'"}.items():
            (stubs / name).write_text("#!/bin/sh\n" + body + "\n")
            (stubs / name).chmod(0o755)
        environment = dict(os.environ, PATH=str(stubs) + ":" + os.environ["PATH"],
                           BOSS_FORGE_REGISTRY_HOST="registry.invalid", BASH_XTRACEFD="9",
                           BOSS_SOR_ENV=str(scratch / "absent.env"),
                           BOSS_ESTATE_NODES_URL="http://registry.invalid/nodes")
        for verb in VERBS:
            declared = root / "infra/ops/verbs" / verb
            door = root / json.loads(declared.read_text())["argv"][0]
            consumed.update([declared, door])
            trace = scratch / (verb + ".trace")
            subprocess.run(["bash", "-c", 'exec 9>"$0"; exec bash -x "$1"', str(trace), str(door)],
                           env=environment, capture_output=True, check=False)
            traces[verb] = trace.read_text()
            for token in re.findall(re.escape(str(root)) + r"/[^\s,'\"]+", traces[verb]):
                path = pathlib.Path(token).resolve()
                if path.is_file():
                    consumed.add(path)
    opened = subprocess.run([sys.executable, "-B", "-c", OPEN_TRACER,
                             str(root / "infra/forge/admission-identity-bootstrap.py")],
                            capture_output=True, text=True, check=True)
    for line in opened.stdout.splitlines():
        path = pathlib.Path(line).resolve()
        if path.is_file() and root in path.parents:
            consumed.add(path)
    return {str(path.relative_to(root)) for path in consumed}, traces


class Scratch:
    """A scratch copy of exactly the bound files, loaded through its own bootstrap."""

    def __init__(self, case):
        self.case = case
        directory = tempfile.TemporaryDirectory()
        case.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name).resolve()
        self.forge = self.root / "infra/forge"
        for name in d.METHODS.values():
            destination = self.forge / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE.parent / name, destination)
        self.target = self.forge / SOURCE.name
        self.binary = self.root / "nonsecret-talosctl"
        self.binary.write_bytes(b"nonsecret fixture executable")
        self.bootstrap, self.loaded = self.load()

    def load(self):
        specification = importlib.util.spec_from_file_location(
            "scratch_bootstrap", self.forge / "admission-identity-bootstrap.py")
        module = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(module)
        return module, module.load_identity()

    def host(self, images=(IMAGE, IMAGE), talos=None, during=None, binary=None):
        """host() against a container that answers a valid receipt for what it was handed."""
        loaded, case, binary = self.loaded, self.case, binary or self.binary
        answers = iter(images)

        class Port:
            def __init__(self, timeout):
                self.timeout = timeout

            def read_bytes(self, argv):
                if argv[3:5] == ["image", "inspect"]:
                    return (next(answers) + "\n").encode()
                case.assertEqual(argv[3], "run")
                report = {"schema": loaded.SCHEMA, "scope": "installed_reader_identity",
                          "state": "available", "history_verdict": "unavailable",
                          "image_id": argv[argv.index("--env") + 1].partition("=")[2],
                          "talos_sha256": talos or loaded.fingerprint(binary, 1024),
                          "client": CLIENT, "methods": loaded.method_hashes()}
                if during:
                    during()
                return json.dumps(report).encode()

        with patch.object(loaded.native, "NativeReadPort", Port), \
                patch.dict(os.environ, BOSS_READER_IMAGE="registry.invalid/fixture", BOSS_TALOSCTL=str(binary)):
            return loaded.host()

    def inside(self, during=None):
        loaded, real = self.loaded, self.loaded.snapshot

        def redirected(path, limit):
            if str(path) == "/talosctl":
                path = self.binary
            elif str(path).startswith("/methods/"):
                path = self.forge / loaded.METHODS[path.name]
            return real(path, limit)

        class Port:
            def __init__(self, timeout):
                self.timeout = timeout

            def read_bytes(self, argv):
                if during:
                    during()
                return RAW_CLIENT

        with patch.object(loaded, "snapshot", redirected), patch.object(loaded.native, "NativeReadPort", Port), \
                patch.dict(os.environ, BOSS_READER_IMAGE_ID=IMAGE):
            return loaded.inside()

    def refused(self, call, **arguments):
        with self.case.assertRaises(self.loaded.Unavailable) as caught:
            call(**arguments)
        return str(caught.exception)

    def replace_same_bytes(self, path):
        other = path.with_name(path.name + ".next")
        other.write_bytes(path.read_bytes())
        other.replace(path)


class Bound(unittest.TestCase):
    """One control per bound check review c6894a64 found untested (its mutate.py).

    Each starts from a positive on the untouched fixture, so a refusal is the
    tamper's and not the fixture's.
    """

    def test_method_table_equals_the_derived_consumed_closure(self):
        bound = {os.path.normpath(os.path.join("infra/forge", name)) for name in d.METHODS.values()}
        self.assertEqual(len(bound), len(d.METHODS))
        derived, traces = consumed_closure(ROOT)
        # The traces reached the stages that name the runtime files.
        self.assertIn("docker run", traces[VERBS[1]])
        self.assertIn("admission-identity-bootstrap.py", traces[VERBS[0]])
        self.assertEqual(sorted(derived), sorted(bound),
                         "a file a door consumes is not in METHODS, or METHODS binds one no door consumes")
        # Shape control: a copy holding ONLY the bound files still runs both
        # doors to the same closure, and one more sourced file is seen.
        with tempfile.TemporaryDirectory() as directory:
            copy = pathlib.Path(directory).resolve()
            for relative in bound:
                (copy / relative).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative, copy / relative)
            self.assertEqual(consumed_closure(copy)[0], bound)
            reader = copy / "infra/lib/sor-reader.sh"
            self.assertTrue(reader.is_file(), "the reader identity file is bound")
            reader.write_text(reader.read_text() + '. "$(dirname "${BASH_SOURCE[0]}")/planted.sh"\n')
            (copy / "infra/lib/planted.sh").write_text(": planted\n")
            self.assertEqual(consumed_closure(copy)[0] - bound, {"infra/lib/planted.sh"})

    def test_reader_identity_file_moves_the_method_table(self):
        fixture = Scratch(self)
        before = fixture.loaded.method_hashes()
        reader = fixture.root / "infra/lib/sor-reader.sh"
        reader.write_text(reader.read_text() + "\n# any change at all\n")
        after = fixture.loaded.method_hashes()
        self.assertEqual(sorted(key for key in before if before[key] != after[key]), ["reader"])

    def test_source_replaced_after_compile_is_refused_on_both_sides(self):
        for boundary in ["host", "inside"]:
            with self.subTest(boundary=boundary):
                fixture = Scratch(self)
                call = getattr(fixture, boundary)
                self.assertEqual(call()["state"], "available")
                source = fixture.forge / "admission-source.py"
                source.write_bytes(source.read_bytes() + b"\n# replaced after this entry compiled it\n")
                self.assertEqual(fixture.refused(call), "identity_method_changed")

    def test_image_changed_or_mismatched_is_refused(self):
        fixture = Scratch(self)
        self.assertEqual(fixture.host()["image_id"], IMAGE)
        other = "sha256:" + "b" * 64
        self.assertEqual(fixture.refused(fixture.host, images=(IMAGE, other)), "identity_image_changed")
        methods = d.method_hashes()
        report = {"schema": d.SCHEMA, "scope": "installed_reader_identity", "state": "available",
                  "history_verdict": "unavailable", "image_id": other, "talos_sha256": "c" * 64,
                  "client": CLIENT, "methods": methods}
        self.assertEqual(d.validate_report(json.dumps(report).encode(), other, methods), report)
        with self.assertRaises(d.Unavailable) as caught:
            d.validate_report(json.dumps(report).encode(), IMAGE, methods)
        self.assertEqual(str(caught.exception), "identity_image_changed")

    def test_binary_hashed_inside_must_be_the_one_hashed_on_the_host(self):
        fixture = Scratch(self)
        self.assertEqual(fixture.host()["talos_sha256"], fixture.loaded.fingerprint(fixture.binary, 1024))
        self.assertEqual(fixture.refused(fixture.host, talos="c" * 64), "identity_file_changed")

    def test_method_changed_during_the_run_is_refused_by_the_host(self):
        fixture = Scratch(self)
        self.assertEqual(fixture.host()["state"], "available")
        helper = fixture.root / "infra/lib/jq.sh"
        change = lambda: helper.write_bytes(helper.read_bytes() + b"\n# changed while the container ran\n")
        self.assertEqual(fixture.refused(fixture.host, during=change), "identity_file_changed")

    def test_binary_changed_during_the_client_read_is_refused_inside(self):
        fixture = Scratch(self)
        self.assertEqual(fixture.inside()["state"], "available")
        change = lambda: fixture.binary.write_bytes(b"another nonsecret executable")
        self.assertEqual(fixture.refused(fixture.inside, during=change), "identity_file_changed")

    def test_capture_binds_the_file_incarnation_not_only_its_bytes(self):
        # Guard half: same bytes, new inode after compile.
        fixture = Scratch(self)
        self.assertEqual(fixture.host()["state"], "available")
        fixture.replace_same_bytes(fixture.target)
        self.assertEqual(fixture.refused(fixture.host), "identity_method_changed")
        # Capture half: the path is replaced between the read and the path stat.
        fixture = Scratch(self)
        real, calls = os.fstat, []

        def swapping(descriptor):
            result = real(descriptor)
            calls.append(descriptor)
            if len(calls) == 2:
                fixture.replace_same_bytes(fixture.target)
            return result

        with patch.object(fixture.bootstrap.os, "fstat", swapping):
            with self.assertRaises(fixture.bootstrap.CaptureUnavailable) as caught:
                fixture.bootstrap.load_identity()
        self.assertEqual((str(caught.exception), len(calls)), ("identity_file_changed", 2))

    def test_loader_issues_the_capture_only_to_the_code_it_compiled(self):
        fixture = Scratch(self)
        self.assertIsNotNone(fixture.loaded.COMPILED_IDENTITY_CAPTURE)
        self.assertIsNone(fixture.loaded.__loader__.compiled_capture())
        # Other code presenting the genuine loader is still uncaptured.
        thief = type(sys)("thief")
        thief.__file__ = str(fixture.target)
        thief.__loader__ = fixture.loaded.__loader__
        exec(compile(fixture.target.read_bytes(), str(fixture.target), "exec"), thief.__dict__)
        self.assertIsNone(thief.COMPILED_IDENTITY_CAPTURE)
        with patch.object(thief.native, "NativeReadPort", side_effect=AssertionError("uncaptured entry reached native port")), \
                patch.dict(os.environ, BOSS_READER_IMAGE="registry.invalid/fixture"):
            with self.assertRaises(thief.Unavailable):
                thief.host()

    def test_door_refuses_every_status_but_the_two_it_declares(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = pathlib.Path(directory)
            (scratch / "python3").write_text('#!/bin/sh\nprintf \'%s\\n\' "$REPORT_TEXT"\nexit "$STATUS"\n')
            (scratch / "python3").chmod(0o755)
            text = '{"state":"available","raw":"PRIVATE"}'
            door = SOURCE.with_name("probe-admission-reader-identity.sh")
            def run(status):
                environment = dict(os.environ, PATH=str(scratch) + ":" + os.environ["PATH"], STATUS=str(status),
                                   REPORT_TEXT=text, BOSS_FORGE_REGISTRY_HOST="registry.invalid",
                                   BOSS_SOR_ENV=str(scratch / "absent.env"))
                return subprocess.run(["bash", str(door)], env=environment, capture_output=True, check=False)
            # Control: the stub is what the door runs, and 0 passes its text through.
            self.assertEqual((run(0).returncode, run(0).stdout.decode().strip()), (0, text))
            for status, reason in [(1, "identity_runtime_unavailable"), (3, "identity_runtime_unavailable"),
                                   (124, "identity_deadline"), (137, "identity_deadline")]:
                result = run(status)
                self.assertEqual(result.returncode, 4, status)
                self.assertNotIn(b"PRIVATE", result.stdout, status)
                self.assertEqual(json.loads(result.stdout)["reason"], reason, status)

    def test_binary_path_outside_the_fixed_shape_is_refused(self):
        fixture = Scratch(self)
        self.assertEqual(fixture.host()["state"], "available")
        odd = fixture.root / "odd dir"
        odd.mkdir()
        shutil.copyfile(fixture.binary, odd / "talosctl")
        self.assertEqual(fixture.refused(fixture.host, binary=odd / "talosctl"), "operator_binary_unavailable")


if __name__ == "__main__":
    unittest.main()
