#!/usr/bin/python3 -I
"""Protected forge adapter. No default success, credentials or target seams.

The shell entry checks the host and existing credential doors before this
adapter runs. Tests construct explicit ports with private temporary data.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit, urlunsplit

HERE = Path(__file__).resolve().parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


kernel = load("fixture_host_kernel", HERE / "isolation-fixture.py")
ports = load("fixture_host_ports", HERE / "isolation-fixture-port.py")


def write(path, raw):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as target:
        target.write(raw)
        target.flush()
        os.fsync(target.fileno())
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Recorder:
    """Whole byte-one stdout/stderr, argv, input and actual exit before judging."""
    def __init__(self, root, request):
        kernel.identifier(request)
        root = Path(root)
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if root.is_symlink() or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o022:
            raise kernel.Unknown("invocation root is not protected")
        self.directory = root / (request + "-" + str(time.time_ns()))
        self.directory.mkdir(mode=0o700)
        self.index, self.destination = 0, None

    def attach(self, destination):
        self.destination = destination
        self.copy()

    def copy(self):
        if self.destination is not None:
            for path in sorted(self.directory.iterdir()):
                if path.is_file() and not path.is_symlink() and not path.name.endswith(".tar.gz"):
                    target = self.destination / (self.directory.name + "-" + path.name)
                    if not target.exists():
                        write(target, path.read_bytes())

    def run(self, argv, body=None):
        label = f"{self.index:06d}"
        self.index += 1
        write(self.directory / (label + ".argv.json"), kernel.encoded(argv))
        if body is not None:
            write(self.directory / (label + ".stdin"), body)
        out = self.directory / (label + ".stdout")
        err = self.directory / (label + ".stderr")
        with out.open("xb") as stdout, err.open("xb") as stderr:
            os.chmod(out, 0o600)
            os.chmod(err, 0o600)
            try:
                result = subprocess.run(argv, input=body, stdout=stdout, stderr=stderr, timeout=100, check=False)
                code = result.returncode
            except (OSError, subprocess.TimeoutExpired) as error:
                code = type(error).__name__
            stdout.flush()
            stderr.flush()
            os.fsync(stdout.fileno())
            os.fsync(stderr.fileno())
        write(self.directory / (label + ".result.json"), kernel.encoded({"exit": code}))
        self.copy()
        if code != 0 or out.stat().st_size > kernel.MAX_BYTES or err.stat().st_size > kernel.MAX_BYTES:
            raise kernel.Unknown("native invocation refused, exceeded bound or did not complete; originals retained")
        return out.read_bytes()


class Host:
    def __init__(self, source, recorder, ordinary, stdin, header, base):
        self.source, self.recorder = source, recorder
        self.ordinary, self.stdin, self.header, self.base = ordinary, stdin, header, base

    def kubectl(self, argv, body=None):
        return self.recorder.run((self.ordinary if body is None else self.stdin) + argv, body)

    def get(self, path, service="jobs"):
        if service == "jobs":
            if not re.fullmatch(r"/api/jobs(?:/[0-9a-f-]{36}|\?kind=agent-run&status=open&full=true&limit=100&offset=[0-9]+)", path):
                raise kernel.Unknown("non-fixed jobs read")
            base = self.base
        elif service == "content" and re.fullmatch(r"/api/files/[0-9a-f-]{36}", path):
            rows = dict(line.split("=", 1) for line in (self.source / "infra/forge/sor-ports.env").read_text().splitlines() if line and not line.startswith("#"))
            port = rows["content"]
            if not port.isdecimal():
                raise kernel.Unknown("invalid owning content port")
            parsed = urlsplit(self.base)
            base = urlunsplit((parsed.scheme, parsed.hostname + ":" + port, "", "", ""))
        else:
            raise kernel.Unknown("non-fixed content read")
        body = self.recorder.directory / ("http-" + str(time.time_ns()) + ".body")
        code = self.recorder.run(["curl", "--silent", "--show-error", "--fail-with-body", "--max-time", "20",
                                  "--max-redirs", "0", "--header", self.header, "--output", str(body),
                                  "--write-out", "%{http_code}", base + path])
        if code != b"200" or body.stat().st_size > kernel.MAX_BYTES:
            raise kernel.Unknown("native HTTP read is not complete status200; body retained")
        return body.read_bytes()

    def packet(self, identity):
        kernel.identifier(identity)
        value = json.loads(self.get("/api/jobs/" + identity))
        if value.get("id") != identity:
            raise kernel.Unknown("native packet identity mismatch")
        return value

    def active_runs(self):
        result, total = [], None
        for offset in range(0, 10000, 100):
            page = json.loads(self.get("/api/jobs?kind=agent-run&status=open&full=true&limit=100&offset=" + str(offset)))
            rows = page.get("data")
            if not isinstance(rows, list) or page.get("full") is not True or type(page.get("total")) is not int:
                raise kernel.Unknown("native run roster incomplete")
            if total is None:
                total = page["total"]
            if total != page["total"]:
                raise kernel.Unknown("native run roster changed while reading")
            result.extend(rows)
            if len(result) >= total:
                if len(result) != total or len({row["id"] for row in result}) != total:
                    raise kernel.Unknown("native run roster count/identity mismatch")
                return result
            if not rows:
                break
        raise kernel.Unknown("native run roster exceeds complete bounded read")

    def prerequisites(self, plan, store, published):
        self.recorder.attach(store.path(plan["request"]))
        kernel.authorize(plan, self.packet(plan["request"]), self.packet(plan["runtime"]),
                         self.packet(kernel.DESIGN), self.packet(kernel.ISOLATION), published, self.active_runs())
        namespace = json.loads(self.kubectl(["get", "namespace", "boss-dev", "-o", "json", "--request-timeout=10s"]))
        if namespace.get("kind") != "Namespace" or namespace.get("metadata", {}).get("name") != "boss-dev" or not namespace["metadata"].get("uid"):
            raise kernel.Unknown("known native connection is not boss-dev")
        for verb, resource in (("get", "jobs"), ("list", "jobs"), ("create", "jobs"), ("delete", "jobs"),
                               ("get", "services"), ("list", "services"), ("create", "services"), ("delete", "services"),
                               ("get", "pods"), ("list", "pods"), ("create", "pods/exec")):
            try:
                permitted = self.kubectl(["auth", "can-i", verb, resource, "-n", "boss-dev", "--quiet=false"]).strip() == b"yes"
            except kernel.Unknown:
                permitted = False
            if not permitted:
                raise kernel.Unknown("existing forge " + verb + " " + resource + " right in boss-dev not positively observed; no grant implied")
        self.recorder.run([str(self.source / "infra/forge/dev-build-fence-status.sh")])
        for kind in ("jobs", "pods"):
            live = json.loads(self.kubectl(["get", kind, "-n", "boss-dev", "-o", "json", "--request-timeout=10s"]))
            if not isinstance(live.get("items"), list):
                raise kernel.Unknown("native execution roster unreadable")
            for item in live["items"]:
                md, status = item["metadata"], item.get("status", {})
                purpose = md.get("labels", {}).get("boss.dev/purpose")
                relevant = md["name"].startswith(("gate-", "dev-build-", "consist-")) or purpose in ("isolated-build", "isolation-fixture")
                finished = (status.get("phase") in ("Succeeded", "Failed") if kind == "pods" else
                            any(condition.get("type") in ("Complete", "Failed") and condition.get("status") == "True" for condition in status.get("conditions", [])))
                if relevant and not finished:
                    raise kernel.Unknown("native gate/worker/fixture execution not quiescent")
        return {"known_connection": True, "rights": True, "quiescent": True}


def published(source, cli_root, head, helper_sha, image_digest, image_repo, recorder):
    generation = cli_root / head
    guard = load("fixture_publication_guard", source / "infra/forge/dev-build-fence-status.py")
    for path in (generation / "manifest.json", generation / "dev-control.tar", source / "infra/forge/isolation-receiver.py"):
        guard.protected(path, os.getuid())
        guard.directories(path, Path("/"), os.getuid())
    manifest = (generation / "manifest.json").read_bytes()
    if len(manifest) > 1024 * 1024 or hashlib.sha256(manifest).hexdigest() != image_digest:
        raise kernel.Unknown("published platform image manifest differs")
    helper = (source / "infra/forge/isolation-receiver.py").read_bytes()
    if hashlib.sha256(helper).hexdigest() != helper_sha:
        raise kernel.Unknown("published source helper differs")
    artifact = load("fixture_published_artifact", source / "infra/dev/control-artifact.py")
    with tempfile.TemporaryDirectory(dir=recorder.directory) as temporary:
        destination = Path(temporary) / "validated"
        artifact.extract(generation / "dev-control.tar", destination, head, retain={"infra/forge/isolation-receiver.py"})
        if (destination / "infra/forge/isolation-receiver.py").read_bytes() != helper:
            raise kernel.Unknown("published image control archive has another receiver")
        write(recorder.directory / "published-artifact.json", (destination / "artifact.json").read_bytes())
    write(recorder.directory / "published-manifest.json", manifest)
    write(recorder.directory / "published-receiver.py", helper)
    return {"head": head, "helper_sha256": helper_sha, "image": image_repo + "@sha256:" + image_digest}


class Artifacts:
    def __init__(self, host):
        self.host = host

    def publish(self, request, raw):
        if len(raw) > 2096128:
            raise kernel.Unknown("complete fixture evidence exceeds existing file-store bound; no deletion")
        path = self.host.recorder.directory / ("originals-" + str(time.time_ns()) + ".tar.gz")
        write(path, raw)
        output = self.host.recorder.run(["/usr/local/bin/boss", "attach", request, str(path)])
        matches = re.findall(rb"as file ([0-9a-f-]{36}),", output)
        if len(matches) != 1:
            raise kernel.Unknown("native attachment receipt missing or ambiguous")
        identity = kernel.identifier(matches[0].decode())
        return {"file_id": identity, "readback": self.host.get("/api/files/" + identity, "content")}


def main(argv):
    if argv and argv[0] == "validate":
        _, phase, current, *args = argv
        kernel.identifier(current)
        if phase == "prepare" and len(args) == 6:
            runtime, nonce, head, helper, digest, lifetime = args
            if not re.fullmatch(r"(?:60|[6-9][0-9]|[1-5][0-9]{2}|600)", lifetime):
                raise kernel.Unknown("bounded fixture lifetime required")
            kernel.make_plan(current, runtime, nonce, head, helper, "synthetic.invalid/boss@sha256:" + kernel.hex_string(digest, 64), int(lifetime), 0)
        elif phase in ("observe", "retire") and len(args) == 1:
            kernel.identifier(args[0])
        else:
            raise kernel.Unknown("fixed lifecycle arguments required")
        return
    # Arguments after phase are internal shell-derived authority, not registry
    # parameters. No environment switch can turn this into a local fixture.
    if len(argv) < 8:
        raise kernel.Unknown("fixed host entry required")
    phase, source_text, ordinary, stdin, header, base, image_repo, current, *args = argv
    source = Path(source_text)
    if os.getuid() != 0 or not re.fullmatch(r"/var/lib/boss/tree/gen/[0-9a-f]{40}", str(source)):
        raise kernel.Unknown("protected original forge generation required")
    kernel.identifier(current)
    if phase not in ("prepare", "observe", "retire") or len(args) != (6 if phase == "prepare" else 1):
        raise kernel.Unknown("fixed lifecycle arguments required")
    parsed = urlsplit(base)
    if parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ("", "/"):
        raise kernel.Unknown("owning jobs origin required")
    header_path = Path(header.removeprefix("@"))
    if not header.startswith("@") or header_path.is_symlink() or not header_path.is_file() or stat.S_IMODE(header_path.stat().st_mode) != 0o600 or header_path.stat().st_uid != 0:
        raise kernel.Unknown("protected existing machine header file required")
    recorder = Recorder(Path("/var/lib/boss/isolation-fixture-invocations"), current)
    host = Host(source, recorder, shlex.split(ordinary), shlex.split(stdin), header, base.rstrip("/"))
    store = kernel.JournalStore(kernel.JOURNAL_ROOT)
    if phase == "prepare":
        runtime, nonce, head, helper, digest, lifetime = args
        old = store.read(current)
        if old is None and source.name != head:
            raise kernel.Unknown("new fixture source generation differs from requested immutable head")
        plan = kernel.make_plan(current, runtime, nonce, head, helper, image_repo + "@sha256:" + digest, int(lifetime), old["plan"]["at"] if old else int(time.time()))
        publication = published(Path("/var/lib/boss/tree/gen") / head, Path("/opt/boss-cli"), head, helper, digest, image_repo, recorder)
        if old is not None:
            recorder.attach(store.path(current))
    else:
        original = kernel.identifier(args[0])
        old = store.read(original)
        if old is None:
            raise kernel.Unknown("original forge fixture journal missing")
        plan, publication = old["plan"], None
        request = host.packet(current)
        execute = kernel.unique_step(request, "execute")
        if (request.get("kind") != "ops-request" or request.get("partition") != "real" or request.get("simulated") is not False
                or request.get("status") != "open" or request.get("metadata", {}).get("host") != "forge"
                or request["metadata"].get("verb") != phase + "-isolation-fixture" or request["metadata"].get("args") != [original]
                or execute.get("status") not in ("ready", "active") or execute.get("metadata", {}).get("credential_executor") != "runner:ops"):
            raise kernel.Unknown("current lifecycle request is not protected original ops execution")
        recorder.attach(store.path(original))
    native = ports.NativePort(host.kubectl, plan, lambda: host.prerequisites(plan, store, publication))
    engine = kernel.Engine(store, native, Artifacts(host), lambda: int(time.time()))
    result = engine.prepare(plan) if phase == "prepare" else getattr(engine, phase)(plan["request"])
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except Exception as error:
        # Native files contain full diagnostics. No authority/header values or
        # caller payload is printed by an exception renderer.
        print("isolation-fixture: UNKNOWN " + str(getattr(error, "safe_cause", type(error).__name__)) + "; original intent and complete invocation evidence retained", file=sys.stderr)
        sys.exit(78)
