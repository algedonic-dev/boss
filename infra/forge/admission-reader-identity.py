"""Fixed nonsecret identity facts for the installed retained-admission reader.

No credentials, server calls, arbitrary commands or diagnostic export. The
receipt is a point-in-time binding, never proof of historical coverage.
"""
import hashlib
import json
import os
import pathlib
import re
import sys
import types

sys.dont_write_bytecode = True
HERE = pathlib.Path(__file__).resolve().parent
# The fixed bootstrap issues a capture during this exact code execution.
_loader_capture = getattr(globals().get("__loader__"), "compiled_capture", None)
COMPILED_IDENTITY_CAPTURE = _loader_capture() if _loader_capture is not None else None
with (HERE / "admission-source.py").open("rb") as source_stream:
    native_bytes = source_stream.read(1024 * 1024 + 1)
if len(native_bytes) > 1024 * 1024:
    raise ValueError("fixed source exceeds its bound")
# Execute exactly the bytes whose identity is checked, without a stale pyc.
NATIVE_SHA = hashlib.sha256(native_bytes).hexdigest()
native = types.ModuleType("admission_native")
exec(compile(native_bytes, str(HERE / "admission-source.py"), "exec"), native.__dict__)
del native_bytes
Unavailable = native.Unavailable
need = native.need
SCHEMA = "boss.admission-reader-identity.v1"
REASONS = {
    "client_version_bound", "client_version_unavailable", "identity_file_bound",
    "identity_file_unavailable", "identity_file_changed", "identity_report_bound",
    "identity_report_unavailable", "identity_image_changed", "identity_method_changed",
    "operator_image_unavailable", "operator_binary_unavailable", "unexpected_arguments",
    "native_read_failed", "native_read_timeout", "native_read_size", "native_read_diagnostic",
    "identity_runtime_unavailable",
}
METHODS = {
    "source": "admission-source.py",
    "access": "probe-admission-source.py",
    "host": "probe-admission-source-access.sh",
    "report": "admission-access-report.jq",
    "target": "../ops/admission-discovery-target.json",
    "operator": "../estate/ops-credentials.sh",
    "estate": "../lib/sor.sh",
    "reader": "../lib/sor-reader.sh",
    "json": "../lib/jq.sh",
    "access_verb": "../ops/verbs/probe-admission-source-access.json",
    "identity_verb": "../ops/verbs/probe-admission-reader-identity.json",
    "identity": "admission-reader-identity.py",
    "identity_host": "probe-admission-reader-identity.sh",
    "identity_bootstrap": "admission-identity-bootstrap.py",
}
HEX = r"[0-9a-f]{64}"
CLIENT_PATTERNS = {
    "version": r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?",
    "revision": r"[0-9a-f]{7,40}",
    "go_version": r"go[0-9]+\.[0-9]+(?:\.[0-9]+)?",
    "os_arch": r"linux/(?:amd64|arm64)",
}


def client_version(raw):
    need(len(raw) <= 4096, "client_version_bound")
    try:
        text = raw.decode("ascii")
    except UnicodeError:
        raise Unavailable("client_version_unavailable") from None
    match = re.fullmatch(
        r"Client:\n\s*Tag:\s*(\S+)\n\s*SHA:\s*(\S+)\n"
        r"[ \t]*Built:[ \t]*([0-9TZ:+. -]*)\n"
        r"\s*Go version:\s*(\S+)\n\s*OS/Arch:\s*(\S+)\n", text)
    need(match is not None, "client_version_unavailable")
    fields = dict(zip(CLIENT_PATTERNS, [match[1], match[2], match[4], match[5]]))
    need(all(re.fullmatch(CLIENT_PATTERNS[key], value) for key, value in fields.items()),
         "client_version_unavailable")
    return fields


def snapshot(path, limit):
    digest = hashlib.sha256()
    count = 0
    try:
        with path.open("rb") as stream:
            before = os.fstat(stream.fileno())
            need(before.st_size > 0 and before.st_size <= limit, "identity_file_bound")
            while chunk := stream.read(65536):
                count += len(chunk)
                need(count <= limit, "identity_file_bound")
                digest.update(chunk)
            after = os.fstat(stream.fileno())
        current = path.stat()
    except OSError:
        raise Unavailable("identity_file_unavailable") from None
    key = lambda stat: (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)
    need(key(before) == key(after) == key(current) and count == before.st_size,
         "identity_file_changed")
    return {"sha256": digest.hexdigest(), "identity": key(before)}


def fingerprint(path, limit):
    return snapshot(path, limit)["sha256"]


def method_snapshots(inside=False):
    return {key: snapshot(pathlib.Path("/methods") / key if inside else HERE / name,
                          1024 * 1024) for key, name in METHODS.items()}


def method_hashes(inside=False):
    return {key: value["sha256"] for key, value in method_snapshots(inside).items()}


def _compiled_guard(captured):
    def require(captures):
        need(captured is not None and captures["identity"] == captured,
             "identity_method_changed")
    return require


# Later globals/environment are not evidence of the code this entry compiled.
require_compiled_identity = _compiled_guard(COMPILED_IDENTITY_CAPTURE)


def validate_report(raw, image, methods):
    need(len(raw) <= 4096, "identity_report_bound")
    try:
        def unique(pairs):
            result = {}
            for key, value in pairs:
                need(key not in result, "identity_report_unavailable")
                result[key] = value
            return result
        value = json.loads(raw, object_pairs_hook=unique)
    except (ValueError, UnicodeError):
        raise Unavailable("identity_report_unavailable") from None
    if isinstance(value, dict) and value.get("state") == "unavailable":
        need(set(value) == {"schema", "scope", "state", "history_verdict", "reason"}
             and value["schema"] == SCHEMA and value["scope"] == "installed_reader_identity"
             and value["history_verdict"] == "unavailable"
             and isinstance(value["reason"], str) and value["reason"] in REASONS,
             "identity_report_unavailable")
        return value
    need(isinstance(value, dict) and set(value) == {
        "schema", "scope", "state", "history_verdict", "image_id", "talos_sha256", "client", "methods"},
        "identity_report_unavailable")
    need(value["schema"] == SCHEMA and value["scope"] == "installed_reader_identity"
         and value["state"] == "available" and value["history_verdict"] == "unavailable",
         "identity_report_unavailable")
    need(isinstance(value["image_id"], str) and re.fullmatch("sha256:" + HEX, value["image_id"])
         and value["image_id"] == image, "identity_image_changed")
    need(isinstance(value["talos_sha256"], str) and re.fullmatch(HEX, value["talos_sha256"]),
         "identity_report_unavailable")
    need(value["methods"] == methods and set(methods) == set(METHODS)
         and all(isinstance(item, str) and re.fullmatch(HEX, item) for item in methods.values()),
         "identity_method_changed")
    client = value["client"]
    need(isinstance(client, dict) and set(client) == set(CLIENT_PATTERNS)
         and all(isinstance(client[key], str) and re.fullmatch(pattern, client[key])
                 for key, pattern in CLIENT_PATTERNS.items()), "identity_report_unavailable")
    return value


def inside():
    captures = method_snapshots(True)
    require_compiled_identity(captures)
    methods = {key: value["sha256"] for key, value in captures.items()}
    need(methods["source"] == NATIVE_SHA
         and methods["identity"] == fingerprint(HERE / "admission-reader-identity.py", 1024 * 1024),
         "identity_method_changed")
    binary = snapshot(pathlib.Path("/talosctl"), 256 * 1024 * 1024)
    # Official talosctl CLI: --client prints only the client version.
    # /dev/null prevents config discovery; the container has no network.
    client = client_version(native.NativeReadPort(timeout=5).read_bytes(
        ["/talosctl", "--talosconfig=/dev/null", "version", "--client"]))
    need(binary == snapshot(pathlib.Path("/talosctl"), 256 * 1024 * 1024)
         and captures == method_snapshots(True), "identity_file_changed")
    return {"schema": SCHEMA, "scope": "installed_reader_identity", "state": "available",
            "history_verdict": "unavailable", "image_id": os.environ.get("BOSS_READER_IMAGE_ID", ""),
            "talos_sha256": binary["sha256"], "client": client, "methods": methods}


def host():
    captures = method_snapshots()
    require_compiled_identity(captures)
    methods = {key: value["sha256"] for key, value in captures.items()}
    need(methods["source"] == NATIVE_SHA, "identity_method_changed")
    image = os.environ.get("BOSS_READER_IMAGE", "")
    need(image and len(image) <= 256 and re.fullmatch(r"[A-Za-z0-9._:/-]+", image),
         "operator_image_unavailable")
    port = native.NativeReadPort(timeout=25)
    def image_id():
        raw = port.read_bytes(["sudo", "-n", "docker", "image", "inspect", "--format", "{{.Id}}", image])
        need(len(raw) <= 80 and re.fullmatch(rb"sha256:[0-9a-f]{64}\n", raw),
             "operator_image_unavailable")
        return raw.decode("ascii").strip()
    immutable = image_id()
    binary_path = pathlib.Path(os.environ.get("BOSS_TALOSCTL", "/usr/local/bin/talosctl"))
    need(len(str(binary_path)) <= 256 and re.fullmatch(r"/[A-Za-z0-9._/-]+", str(binary_path)),
         "operator_binary_unavailable")
    binary = snapshot(binary_path, 256 * 1024 * 1024)
    argv = ["sudo", "-n", "docker", "run", "--pull=never", "--rm", "--network", "none",
            "--memory", "256m", "--memory-swap", "256m", "--pids-limit", "64",
            "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "--ulimit", "core=0", "--tmpfs", "/tmp:rw,noexec,nosuid,size=8m",
            "--env", "BOSS_READER_IMAGE_ID=" + immutable,
            "--mount", f"type=bind,src={binary_path},dst=/talosctl,readonly",
            "--mount", f"type=bind,src={HERE / 'admission-reader-identity.py'},dst=/identity/admission-reader-identity.py,readonly",
            "--mount", f"type=bind,src={HERE / 'admission-source.py'},dst=/identity/admission-source.py,readonly"]
    argv.extend(["--mount", f"type=bind,src={HERE / 'admission-identity-bootstrap.py'},dst=/identity/admission-identity-bootstrap.py,readonly"])
    for key, name in METHODS.items():
        argv.extend(["--mount", f"type=bind,src={(HERE / name).resolve()},dst=/methods/{key},readonly"])
    argv.extend(["--entrypoint", "timeout", immutable, "-k", "2", "20", "python3", "-B",
                 "/identity/admission-identity-bootstrap.py", "--inside"])
    result = validate_report(port.read_bytes(argv), immutable, methods)
    if result["state"] == "unavailable":
        raise Unavailable(result["reason"])
    need(result["talos_sha256"] == binary["sha256"]
         and binary == snapshot(binary_path, 256 * 1024 * 1024)
         and captures == method_snapshots(), "identity_file_changed")
    need(immutable == image_id(), "identity_image_changed")
    return result


def main():
    reason = "identity_runtime_unavailable"
    try:
        need(sys.argv[1:] in ([], ["--inside"]), "unexpected_arguments")
        result = inside() if sys.argv[1:] else host()
        print(json.dumps(result, separators=(",", ":"), sort_keys=True))
        return 0
    except Unavailable as error:
        if str(error) in REASONS:
            reason = str(error)
    except (OSError, ValueError, TypeError):
        pass
    print(json.dumps({"schema": SCHEMA, "scope": "installed_reader_identity", "state": "unavailable",
                      "history_verdict": "unavailable", "reason": reason},
                     separators=(",", ":"), sort_keys=True))
    # Internal exit0 acknowledges only a valid bounded envelope. Its parsed
    # state remains unavailable; the no-argument host door still exits4.
    # Native nonzero/stderr handling in the shared streaming reader is unchanged.
    return 0 if sys.argv[1:] == ["--inside"] else 4


if __name__ == "__main__":
    sys.exit(main())
