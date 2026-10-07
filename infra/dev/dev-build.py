#!/usr/bin/python3 -I
"""Execute repository programs in an uncredentialed Pod, never here.

Installed from published main by install-control.sh. The controller owns
Kubernetes calls and its private session record; worker output is data.
"""
import argparse
import base64
from dataclasses import asdict, dataclass
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
import uuid
from urllib.parse import urlsplit


@dataclass(frozen=True)
class Session:
    job: str
    job_uid: str
    pod: str
    pod_uid: str
    container_id: str
    source_head: str
    volume_uid: str = ""


def security():
    return {"runAsNonRoot": True, "runAsUser": 65534, "runAsGroup": 1500,
            "allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
            "capabilities": {"drop": ["ALL"]}, "seccompProfile": {"type": "RuntimeDefault"}}


def worker_environment():
    return [{"name": k, "value": v} for k, v in (
        ("HOME", "/workspace/home"), ("CARGO_HOME", "/workspace/cargo"),
        ("CARGO_TARGET_DIR", "/workspace/target"), ("RUSTUP_HOME", "/usr/local/rustup"),
        ("CARGO_BUILD_JOBS", "4"), ("TMPDIR", "/workspace/tmp"),
        ("PATH", "/workspace/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin"),
        ("BUN_INSTALL_CACHE_DIR", "/workspace/home/bun-cache"),
        ("WT_TARGET_ROOT", "/workspace/cache"), ("WT_SEED", "/workspace/no-seed"))]


def postgres_container(image):
    context = {**security(), "runAsUser": 999}
    return {"name": "postgres", "image": image, "imagePullPolicy": "IfNotPresent",
            "securityContext": context,
            "args": ["-c", "fsync=off", "-c", "synchronous_commit=off", "-c", "full_page_writes=off"],
            "env": [{"name": k, "value": v} for k, v in (
                ("POSTGRES_USER", "boss"), ("POSTGRES_PASSWORD", "boss"),
                ("POSTGRES_DB", "postgres"), ("PGDATA", "/pgdata/data"))],
            "readinessProbe": {"exec": {"command": ["pg_isready", "-U", "boss", "-d", "postgres"]},
                               "initialDelaySeconds": 2, "periodSeconds": 2},
            "resources": {"requests": {"cpu": "500m", "memory": "1Gi", "ephemeral-storage": "4Gi"},
                          "limits": {"cpu": "2", "memory": "4Gi", "ephemeral-storage": "12Gi"}},
            "volumeMounts": [{"name": "pgdata", "mountPath": "/pgdata"},
                             {"name": "pg-run", "mountPath": "/var/run/postgresql"},
                             {"name": "pg-shm", "mountPath": "/dev/shm"},
                             {"name": "pg-tmp", "mountPath": "/tmp"}]}


def postgres_volumes():
    return [{"name": "pgdata", "emptyDir": {"sizeLimit": "10Gi"}},
            {"name": "pg-run", "emptyDir": {"medium": "Memory", "sizeLimit": "16Mi"}},
            {"name": "pg-shm", "emptyDir": {"medium": "Memory", "sizeLimit": "1Gi"}},
            {"name": "pg-tmp", "emptyDir": {"sizeLimit": "1Gi"}}]


def declared_matches(actual, expected):
    """Compare owned fields while allowing API-added default fields."""
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(k in actual and declared_matches(actual[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(declared_matches(a, e) for a, e in zip(actual, expected))
    return actual == expected


def worker_job(name, head, image, forge_url, postgres_image=None):
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", name):
        raise ValueError("invalid controller-owned worker name")
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        raise ValueError("source must be an exact commit")
    if not re.fullmatch(r"[A-Za-z0-9.:/_-]+@sha256:[0-9a-f]{64}", image):
        raise ValueError("worker image must be pinned by published digest")
    if postgres_image is None:
        postgres_image = image
    if not re.fullmatch(r"[A-Za-z0-9.:/_-]+@sha256:[0-9a-f]{64}", postgres_image):
        raise ValueError("worker database image must be pinned by published digest")
    url = urlsplit(forge_url)
    if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.query or url.fragment or any(p in (".", "..") for p in url.path.split("/")) or "%" in url.path:
        raise ValueError("forge source must be an uncredentialed fixed URL")
    # No checkout hooks, global config, credential config or executable
    # artifacts survive the fixed bootstrap. Its mount is init-only.
    bootstrap = r'''set -euo pipefail
export HOME=/workspace/bootstrap GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
mkdir -p /workspace/bootstrap /workspace/home /workspace/cargo /workspace/target /workspace/cache /workspace/tmp /workspace/bin
git -c core.hooksPath=/dev/null init -q /workspace/source
cd /workspace/source
git -c http.followRedirects=false -c core.hooksPath=/dev/null -c credential.helper= -c 'credential.helper=!f() { printf "username=x-access-token\npassword="; cat /bootstrap-forge/token; printf "\n"; }; f' fetch --no-tags -- "$2" "$1"
test "$(git rev-parse FETCH_HEAD)" = "$1"
git -c core.hooksPath=/dev/null checkout -q --detach "$1"
git config --local core.hooksPath /dev/null
git config --local --unset-all credential.helper || test "$?" = 5
test "$(git rev-parse HEAD)" = "$1"
ln -s /workspace/source/infra/dev/wt-cargo /workspace/bin/wt-cargo
ln -s /workspace/source/infra/dev/wt-web /workspace/bin/wt-web
'''
    worker = {"name": "worker", "image": image, "imagePullPolicy": "IfNotPresent",
              "command": ["/bin/sleep", "infinity"], "workingDir": "/workspace/source",
              "securityContext": security(),
              "env": worker_environment(),
              "resources": {"requests": {"cpu": "2", "memory": "4Gi"},
                            "limits": {"cpu": "4", "memory": "16Gi"}},
              "volumeMounts": [{"name": "workspace", "mountPath": "/workspace"},
                               {"name": "worker-tmp", "mountPath": "/tmp"}]}
    init = {"name": "published-bootstrap", "image": image, "imagePullPolicy": "IfNotPresent",
            "command": ["/bin/bash", "-c", bootstrap, "bootstrap", head, forge_url],
            "securityContext": security(),
            "env": [{"name": "TMPDIR", "value": "/workspace/tmp"}],
            "volumeMounts": [{"name": "workspace", "mountPath": "/workspace"},
                             {"name": "forge-read", "mountPath": "/bootstrap-forge", "readOnly": True}]}
    labels = {"app.kubernetes.io/part-of": "boss", "boss.dev/purpose": "isolated-build",
              "boss.dev/session": name}
    pod = {"automountServiceAccountToken": False, "enableServiceLinks": False,
           "hostNetwork": False, "hostPID": False, "hostIPC": False,
           "shareProcessNamespace": False, "restartPolicy": "Never",
           "affinity": {"nodeAffinity": {
               "requiredDuringSchedulingIgnoredDuringExecution": {"nodeSelectorTerms": [
                   {"matchExpressions": [{"key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist"}]}]},
               "preferredDuringSchedulingIgnoredDuringExecution": [{"weight": 100, "preference": {
                   "matchExpressions": [{"key": "boss.dev/purpose", "operator": "In", "values": ["build"]}]}}]}},
           "securityContext": {"fsGroup": 1500, "fsGroupChangePolicy": "OnRootMismatch"},
           "containers": [worker, postgres_container(postgres_image)], "initContainers": [init],
           "volumes": [{"name": "workspace", "persistentVolumeClaim": {"claimName": name}},
                       {"name": "forge-read", "secret": {"secretName": "forge-read", "defaultMode": 0o440}},
                       *postgres_volumes(),
                       {"name": "worker-tmp", "emptyDir": {"sizeLimit": "2Gi"}}],
           "imagePullSecrets": [{"name": "forgejo-registry"}]}
    return {"apiVersion": "batch/v1", "kind": "Job",
            "metadata": {"name": name, "namespace": "boss-dev", "labels": labels},
            "spec": {"backoffLimit": 0, "activeDeadlineSeconds": 86400,
                     "ttlSecondsAfterFinished": 3600,
                     "template": {"metadata": {"labels": labels}, "spec": pod}}}


def worker_error(job):
    try:
        pod = job["spec"]["template"]["spec"]
        if any(pod.get(k) is not False for k in ("automountServiceAccountToken", "hostNetwork", "hostPID", "hostIPC", "shareProcessNamespace")):
            return "worker carries host or token authority"
        if pod.get("enableServiceLinks") is not False or pod.get("restartPolicy") != "Never":
            return "worker lifecycle or inherited environment changed"
        if len(pod["containers"]) != 2 or len(pod["initContainers"]) != 1:
            return "unexpected container"
        for container in pod["containers"] + pod["initContainers"]:
            if any(container.get(k) for k in ("envFrom", "lifecycle", "volumeDevices", "ports")):
                return "container carries an undeclared authority or startup hook"
        worker = pod["containers"][0]
        if worker["name"] != "worker" or worker["securityContext"] != security():
            return "worker security context changed"
        if worker.get("command") != ["/bin/sleep", "infinity"] or worker.get("args") or worker.get("workingDir") != "/workspace/source":
            return "worker startup changed"
        if worker["volumeMounts"] != [{"name": "workspace", "mountPath": "/workspace"}, {"name": "worker-tmp", "mountPath": "/tmp"}]:
            return "worker mount crosses control boundary"
        if worker["env"] != worker_environment():
            return "worker inherits undeclared environment"
        if any("valueFrom" in e for e in worker["env"]) or worker.get("envFrom"):
            return "worker environment reads another resource"
        volumes = pod["volumes"]
        if len(volumes) != 7 or volumes[0] != {"name": "workspace", "persistentVolumeClaim": {"claimName": job["metadata"]["name"]}}:
            return "worker storage is not private"
        if volumes[1] != {"name": "forge-read", "secret": {"secretName": "forge-read", "defaultMode": 0o440}}:
            return "bootstrap credential changed"
        if volumes[2:6] != postgres_volumes() or volumes[6] != {"name": "worker-tmp", "emptyDir": {"sizeLimit": "2Gi"}}:
            return "test database mounts changed"
        postgres = pod["containers"][1]
        if postgres.get("command"):
            return "test database startup changed"
        if not declared_matches(postgres, postgres_container(postgres["image"])):
            return "test database carries unexpected authority"
        init = pod["initContainers"][0]
        command = init.get("command", [])
        if len(command) != 6:
            return "bootstrap is not the fixed init"
        expected = worker_job(job["metadata"]["name"], command[4], worker["image"], command[5], postgres["image"])
        expected_init = expected["spec"]["template"]["spec"]["initContainers"][0]
        if not declared_matches(init, expected_init) or init.get("envFrom") or init.get("args"):
            return "bootstrap code, mounts or authority changed"
        return None
    except (KeyError, TypeError, IndexError, ValueError):
        return "incomplete worker manifest"


def session_error(session, pod):
    try:
        if pod["metadata"]["name"] != session.pod or pod["metadata"]["uid"] != session.pod_uid:
            return "worker Pod identity changed"
        owners = pod["metadata"].get("ownerReferences", [])
        if not any(o.get("kind") == "Job" and o.get("uid") == session.job_uid for o in owners):
            return "worker Job identity changed"
        error = worker_error({"metadata": {"name": session.job}, "spec": {"template": {"spec": pod["spec"]}}})
        if error:
            return error
        bootstrap = [c for c in pod["status"].get("initContainerStatuses", []) if c["name"] == "published-bootstrap"]
        if len(bootstrap) != 1 or bootstrap[0].get("restartCount") != 0 or bootstrap[0].get("state", {}).get("terminated", {}).get("exitCode") != 0:
            return "credential-bearing bootstrap completion is not observed"
        states = [c for c in pod["status"]["containerStatuses"] if c["name"] == "worker"]
        if len(states) != 1 or states[0].get("restartCount") != 0 or states[0].get("containerID") != session.container_id:
            return "worker container incarnation changed"
        if pod["status"]["phase"] != "Running" or not states[0].get("ready") or "running" not in states[0].get("state", {}):
            return "worker is not running and ready"
        databases = [c for c in pod["status"]["containerStatuses"] if c["name"] == "postgres"]
        if len(databases) != 1 or databases[0].get("restartCount") != 0 or not databases[0].get("ready") or "running" not in databases[0].get("state", {}):
            return "private test database is not running and ready"
        return None
    except (KeyError, TypeError, IndexError):
        return "incomplete worker observation"


def worker_exec(session, argv, stdin, transport):
    if not argv or any(not isinstance(a, str) or "\0" in a for a in argv):
        raise ValueError("worker execution needs an argv")
    if stdin is not None and len(stdin) > 16 * 1024 * 1024:
        raise ValueError("worker stdin exceeds 16 MiB; nothing executed")
    # No shell, host executable, context, namespace or container flag
    # comes from the candidate. Only the worker's argv follows --.
    return transport(["exec", "-i", session.pod, "-c", "worker", "--", *argv], stdin)


def bound_argv(binding, cwd, argv):
    local = Path(binding["local_root"])
    current = Path(cwd)
    if not local.is_absolute() or not current.is_absolute() or ".." in current.parts or ".." in local.parts:
        raise ValueError("source binding must use absolute paths without traversal")
    try:
        relative = current.relative_to(local)
    except ValueError:
        raise ValueError("current directory is outside the controller-owned source binding") from None
    if not argv:
        raise ValueError("worker execution needs an argv")
    program = Path(argv[0])
    if program.is_absolute() and program.is_relative_to(local):
        program = Path("/workspace/source") / program.relative_to(local)
    return ["/usr/bin/env", "-C", str(Path("/workspace/source") / relative), str(program), *argv[1:]]


def probe_argv(argv, entries):
    allowed = {"BOSS_JOBS_URL", "BOSS_SOR_PORTS", "BOSS_CAR_CONVERGED_AT", "BOSS_CAR_MERGE_REF"}
    seen = set()
    for entry in entries:
        key, separator, value = entry.partition("=")
        if not separator or key not in allowed or key in seen or "\0" in value or len(value.encode()) > 65536:
            raise ValueError("probe environment contains undeclared or unbounded data")
        if key == "BOSS_JOBS_URL":
            url = urlsplit(value)
            if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.query or url.fragment:
                raise ValueError("probe address contains authentication or undeclared routing data")
        seen.add(key)
    return ["/usr/bin/env", "--", *entries, *argv] if entries else argv


def check_binding(binding, session, current_head):
    if binding.get("session") != session.job or binding.get("source_head") != session.source_head or current_head != session.source_head:
        raise ValueError("source binding is stale; stop the old session and admit the new exact source")


def retirement_requested(root, name):
    return (root / (name + ".retire-intent.json")).exists()


def retire_worker(session, root, delete):
    save_record(root / (session.job + ".retire-intent.json"),
                {"session": asdict(session), "state": "retirement requested; execution disabled"})
    result = delete(session.job, session.job_uid,
                    lambda receipt: save_record(root / (session.job + ".lifecycle-" + uuid.uuid4().hex + ".json"), receipt))
    accepted = (result.get("kind") == "Status" and result.get("status") == "Success") or (
        result.get("kind") == "Job" and result.get("metadata", {}).get("uid") == session.job_uid and result.get("metadata", {}).get("deletionTimestamp"))
    if not accepted:
        raise RuntimeError("retirement acknowledgment is not a supported API result; execution stays disabled")
    record = {"session": asdict(session), "state": "local execution retired", "job_delete_acknowledged": True,
              "job_deletion_complete": False, "note": "Job/PVC garbage collection remains to be observed"}
    save_record(root / (session.job + ".retired.json"), record)
    return record


def source_head(path, record, run=subprocess.run):
    # Metadata-only built-in: no checkout, hooks, filters, network,
    # source executables, pager or inherited git configuration.
    argv = ["/usr/bin/git", "--no-optional-locks", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-C", str(path), "rev-parse", "HEAD"]
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
    result = run(argv, env=env, capture_output=True, timeout=30, check=False)
    record({"argv": argv, "exit": result.returncode,
            "stdout_base64": base64.b64encode(result.stdout).decode("ascii"),
            "stderr_base64": base64.b64encode(result.stderr).decode("ascii")})
    value = result.stdout.decode("ascii").strip()
    if result.returncode or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("local metadata did not supply one exact source commit")
    return value


def probe_channel(path):
    expected = r"boss-prove-notfound-[0-9]+-" + str(os.getuid()) + "-" + str(os.getppid())
    if not path.is_absolute() or not re.fullmatch(expected, path.name):
        raise ValueError("probe telemetry target is not the authenticated parent's channel")
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 1024 * 1024:
        os.close(fd)
        raise ValueError("probe telemetry target is not owned bounded regular data")
    return fd


def telemetry_argv(path):
    if not re.fullmatch(r"/workspace/tmp/notfound-[0-9a-f]{32}", path):
        raise ValueError("unknown controller-created worker telemetry path")
    script = r'''import os, stat, sys
path = sys.argv[1]
try:
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
except FileNotFoundError:
    sys.exit(0)
with os.fdopen(fd, "rb") as stream:
    info = os.fstat(stream.fileno())
    if not stat.S_ISREG(info.st_mode) or info.st_size > 1024 * 1024:
        raise ValueError("worker telemetry is not bounded regular data")
    raw = stream.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise ValueError("worker telemetry exceeds its bound")
os.unlink(path)
sys.stdout.buffer.write(raw)
'''
    return ["/usr/bin/python3", "-I", "-c", script, path]


def source_file_argv(verb, path):
    forbidden = {".git", "target", "node_modules", ".cargo", ".config", ".ssh", ".env"}
    if verb not in ("put", "get") or not isinstance(path, str) or path.startswith("/") or "\\" in path or "\0" in path or any(p in ("", ".", "..") or p in forbidden for p in path.split("/")):
        raise ValueError("source transfer requires a relative source file, never control metadata or caches")
    script = r'''import hashlib, os, stat, sys, uuid
limit = 16 * 1024 * 1024
verb, path = sys.argv[1:]
parts = path.split("/")
directory = os.open("/workspace/source", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    for part in parts[:-1]:
        if verb == "put":
            try:
                os.mkdir(part, 0o755, dir_fd=directory)
            except FileExistsError:
                pass
        child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
        os.close(directory)
        directory = child
    if verb == "put":
        raw = sys.stdin.buffer.read(limit + 1)
        if len(raw) > limit:
            raise ValueError("source edit exceeds 16 MiB; nothing written")
        temporary = ".source-" + uuid.uuid4().hex
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644, dir_fd=directory)
        with os.fdopen(fd, "wb") as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, parts[-1], src_dir_fd=directory, dst_dir_fd=directory)
        print(hashlib.sha256(raw).hexdigest())
    else:
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
                raise ValueError("source retrieval requires a bounded regular file")
            raw = source.read(limit + 1)
            if len(raw) > limit:
                raise ValueError("source retrieval exceeds 16 MiB; no partial output")
        sys.stdout.buffer.write(raw)
finally:
    os.close(directory)
'''
    return ["/usr/bin/python3", "-I", "-c", script, verb, path]


def observed_exec(session, argv, stdin, transport, inspect, record):
    receipt = {"session": asdict(session), "argv": argv, "completion": "unproven"}
    result = None
    try:
        inspect(session)
        result = worker_exec(session, argv, stdin, transport)
        receipt.update(exit=result.returncode,
                       stdout_base64=base64.b64encode(result.stdout).decode("ascii"),
                       stderr_base64=base64.b64encode(result.stderr).decode("ascii"))
        inspect(session)
        # The trusted transport's exit, never a worker envelope, says
        # whether execution completed. Nonzero is still completed FAIL.
        receipt["completion"] = "observed"
        return result
    except subprocess.TimeoutExpired as error:
        receipt.update(error="transport timed out; outcome unknown",
                       stdout_base64=base64.b64encode(error.stdout or b"").decode("ascii"),
                       stderr_base64=base64.b64encode(error.stderr or b"").decode("ascii"))
        raise
    except (ValueError, RuntimeError, OSError) as error:
        receipt["error"] = str(error)
        raise
    finally:
        # Even a failed identity recheck cannot erase returned output.
        record(receipt)


def kube(argv, stdin=None, timeout=None, run=subprocess.run):
    # This binary is installed into the published control generation;
    # PATH and worker caches cannot replace it.
    binary = Path(__file__).resolve().parents[2] / "bin/kubectl"
    if timeout is None:
        timeout = 86400 if argv[:1] == ["exec"] else 120
    return run([str(binary), "--namespace=boss-dev", *argv],
                          input=stdin, capture_output=True, timeout=timeout, check=False)


def kube_json(argv, transport=kube, record=None):
    receipt = {"argv": [*argv, "-o", "json"], "completion": "unproven"}
    try:
        out = transport(receipt["argv"])
        receipt.update(exit=out.returncode, completion="observed",
                       stdout_base64=base64.b64encode(out.stdout).decode("ascii"),
                       stderr_base64=base64.b64encode(out.stderr).decode("ascii"))
        if out.returncode:
            sys.stderr.buffer.write(out.stderr)
            raise RuntimeError("Kubernetes read failed; full observation retained, no local fallback")
        return json.loads(out.stdout)
    except subprocess.TimeoutExpired as error:
        receipt.update(error="Kubernetes observation timed out; outcome unknown",
                       stdout_base64=base64.b64encode(error.stdout or b"").decode("ascii"),
                       stderr_base64=base64.b64encode(error.stderr or b"").decode("ascii"))
        raise
    finally:
        if record is not None:
            record(receipt)


def save_record(path, record):
    raw = json.dumps(record, indent=2).encode() + b"\n"
    temporary = path.with_name(".record-" + uuid.uuid4().hex)
    try:
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        # Publish only flushed bytes, without replacing an earlier
        # observation or retirement intent (dev isolation b3071bf4).
        os.link(temporary, path, follow_symlinks=False)
    finally:
        temporary.unlink(missing_ok=True)


def load_record(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError("controller record ownership or mode is unsafe")
        return json.load(stream)


def observe(session):
    root = Path("/work/home/.config/boss/workers")
    def read(argv):
        return kube_json(argv, record=lambda receipt: save_record(root / (session.job + ".observation-" + uuid.uuid4().hex + ".json"), receipt))
    job = read(["get", "job", session.job])
    if job["metadata"]["uid"] != session.job_uid:
        raise RuntimeError("worker Job changed")
    intent = load_record(root / (session.job + ".intent.json"))
    if intent["head"] != session.source_head or intent["name"] != session.job:
        raise RuntimeError("worker source binding changed")
    expected = intent["job"]["spec"]["template"]["spec"]
    if not declared_matches(job["spec"]["template"]["spec"], expected):
        raise RuntimeError("worker Job differs from recorded controller intent")
    pod = read(["get", "pod", session.pod])
    if not declared_matches(pod["spec"], expected):
        raise RuntimeError("worker Pod differs from recorded controller intent")
    volume = read(["get", "pvc", session.job])
    if not session.volume_uid or volume["metadata"]["uid"] != session.volume_uid or not any(o.get("uid") == session.job_uid and o.get("kind") == "Job" for o in volume["metadata"].get("ownerReferences", [])):
        raise RuntimeError("private worker volume identity or lifecycle owner changed")
    error = session_error(session, pod)
    if error:
        raise RuntimeError(error)
    return pod


def launch(head, config, root, local_root=None, transport=None):
    name = "dev-build-" + uuid.uuid4().hex[:16]
    job = worker_job(name, head, config["worker_image"], config["forge_url"], config["postgres_image"])
    if worker_error(job):
        raise RuntimeError(worker_error(job))
    intent = {"name": name, "head": head, "job": job}
    save_record(root / (name + ".intent.json"), intent)
    transport = transport or kube
    def call(argv, stdin=None):
        receipt = {"argv": argv, "completion": "unproven"}
        try:
            result = transport(argv, stdin)
            receipt.update(exit=result.returncode, completion="observed",
                           stdout_base64=base64.b64encode(result.stdout).decode("ascii"),
                           stderr_base64=base64.b64encode(result.stderr).decode("ascii"))
            return result
        except subprocess.TimeoutExpired as error:
            receipt.update(error="launch transport timed out; outcome unknown",
                           stdout_base64=base64.b64encode(error.stdout or b"").decode("ascii"),
                           stderr_base64=base64.b64encode(error.stderr or b"").decode("ascii"))
            raise
        finally:
            save_record(root / (name + ".launch-" + uuid.uuid4().hex + ".json"), receipt)
    def read(argv):
        result = call([*argv, "-o", "json"])
        if result.returncode:
            sys.stderr.buffer.write(result.stderr)
            raise RuntimeError("worker observation failed; full transport evidence retained")
        return json.loads(result.stdout)
    # Fresh storage is acknowledged BEFORE a token-bearing init can
    # start. A collision therefore creates no Job. Worker-written
    # source/cache is never mounted back into a privileged bootstrap.
    pvc = {"apiVersion": "v1", "kind": "PersistentVolumeClaim",
           "metadata": {"name": name, "namespace": "boss-dev"},
           "spec": {"accessModes": ["ReadWriteOnce"], "storageClassName": "longhorn-dev-disposable",
                    "resources": {"requests": {"storage": "150Gi"}}}}
    out = call(["create", "-f", "-", "-o", "json"], json.dumps(pvc).encode())
    if out.returncode:
        sys.stderr.buffer.write(out.stderr)
        raise RuntimeError("fresh private volume not acknowledged; no Job created, outcome retained")
    created_volume = json.loads(out.stdout)
    if created_volume["metadata"]["name"] != name or not created_volume["metadata"].get("uid"):
        raise RuntimeError("private volume identity not acknowledged; no Job created")
    volume_uid = created_volume["metadata"]["uid"]
    save_record(root / (name + ".volume.json"), {"name": name, "uid": volume_uid, "lifecycle": "unowned until Job acknowledgment and owner patch"})
    out = call(["create", "-f", "-", "-o", "json"], json.dumps(job).encode())
    if out.returncode:
        sys.stderr.buffer.write(out.stderr)
        raise RuntimeError("worker launch not acknowledged; fresh volume may be orphaned, no blind retry")
    created = json.loads(out.stdout)
    if created["metadata"]["name"] != name or not created["metadata"].get("uid"):
        raise RuntimeError("worker Job identity not acknowledged; volume outcome retained")
    job_uid = created["metadata"]["uid"]
    # PVC patch is existing authority; PVC deletion is not. Test the
    # acknowledged UID in the same server operation that assigns GC.
    patch = [{"op": "test", "path": "/metadata/uid", "value": volume_uid},
             {"op": "add", "path": "/metadata/ownerReferences", "value": [
                 {"apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": job_uid}]}]
    out = call(["patch", "pvc", name, "--type=json", "-p", json.dumps(patch), "-o", "json"])
    if out.returncode:
        sys.stderr.buffer.write(out.stderr)
        raise RuntimeError("private volume ownership not acknowledged; Job and volume outcomes retained")
    owned_volume = json.loads(out.stdout)
    if owned_volume["metadata"].get("uid") != volume_uid or not any(o.get("uid") == job_uid and o.get("kind") == "Job" and o.get("name") == name for o in owned_volume["metadata"].get("ownerReferences", [])):
        raise RuntimeError("private volume replacement or wrong owner observed; no worker admission")
    deadline = time.monotonic() + 300
    while time.monotonic() < deadline:
        pods = read(["get", "pods", "-l", "job-name=" + name])["items"]
        if len(pods) > 1:
            raise RuntimeError("worker selected multiple Pods; refusing")
        if pods:
            pod = pods[0]
            if pod.get("status", {}).get("phase") in ("Failed", "Succeeded"):
                raise RuntimeError("worker terminated before admission")
            states = [s for s in pod.get("status", {}).get("containerStatuses", []) if s["name"] == "worker"]
            databases = [s for s in pod.get("status", {}).get("containerStatuses", []) if s["name"] == "postgres"]
            if states and states[0].get("ready") and databases and databases[0].get("ready"):
                session = Session(name, job_uid, pod["metadata"]["name"], pod["metadata"]["uid"], states[0]["containerID"], head, volume_uid)
                error = session_error(session, pod)
                if error:
                    raise RuntimeError(error)
                save_record(root / (name + ".json"), asdict(session))
                if local_root is not None:
                    binding = {"session": name, "local_root": str(local_root), "source_head": head}
                    save_record(root / (name + ".binding.json"), binding)
                print(name)
                return
        time.sleep(2)
    raise RuntimeError("worker unavailable after 300 seconds; no local fallback")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="verb", required=True)
    start = sub.add_parser("start")
    start.add_argument("head")
    start.add_argument("--local-root", type=Path)
    run = sub.add_parser("exec")
    run.add_argument("session")
    run.add_argument("argv", nargs=argparse.REMAINDER)
    stop = sub.add_parser("stop")
    stop.add_argument("session")
    auto = sub.add_parser("exec-auto")
    auto.add_argument("--worker-env", action="append", default=[])
    auto.add_argument("--notfound-channel", type=Path)
    auto.add_argument("argv", nargs=argparse.REMAINDER)
    for verb in ("put", "get"):
        transfer = sub.add_parser(verb)
        transfer.add_argument("session")
        transfer.add_argument("path")
    args = parser.parse_args()
    root = Path("/work/home/.config/boss/workers")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink() or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        raise ValueError("unsafe controller session directory")
    if args.verb == "start":
        config = load_record(Path(__file__).resolve().parents[2] / "config.json")
        local_root = args.local_root
        if local_root is not None and (not local_root.is_absolute() or ".." in local_root.parts):
            raise ValueError("local source binding must be an absolute path without traversal")
        launch(args.head, config, root, local_root)
        return 0
    if args.verb == "stop":
        argv = []
    elif args.verb in ("put", "get"):
        argv = source_file_argv(args.verb, args.path)
    else:
        argv = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
    if args.verb == "exec-auto":
        cwd = os.getcwd()
        bindings = [load_record(path) for path in root.glob("dev-build-*.binding.json")]
        bindings = [b for b in bindings if Path(cwd).is_relative_to(Path(b["local_root"])) and not retirement_requested(root, b["session"])]
        if len(bindings) != 1:
            raise ValueError("source has no unique controller-owned worker binding; nothing executed")
        args.session = bindings[0]["session"]
        argv = bound_argv(bindings[0], cwd, argv)
        argv = probe_argv(argv, args.worker_env)
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", args.session):
        raise ValueError("unknown controller session name")
    session = Session(**load_record(root / (args.session + ".json")))
    if args.verb == "stop":
        path = Path(__file__).resolve().with_name("dev-lifecycle.py")
        spec = importlib.util.spec_from_file_location("dev_lifecycle", path)
        lifecycle = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(lifecycle)
        def read(argv):
            result = kube(argv)
            save_record(root / (session.job + ".authority-" + uuid.uuid4().hex + ".json"),
                        {"argv": argv, "exit": result.returncode,
                         "stdout_base64": base64.b64encode(result.stdout).decode("ascii"),
                         "stderr_base64": base64.b64encode(result.stderr).decode("ascii")})
            return result
        def delete(name, uid, record):
            client = lifecycle.existing_session(read)
            return client.delete_job(name, uid, record)
        print(json.dumps(retire_worker(session, root, delete)))
        return 0
    if retirement_requested(root, session.job):
        raise ValueError("worker retirement is already requested; no execution or local fallback")
    if args.verb == "exec-auto":
        current_head = source_head(Path(bindings[0]["local_root"]),
            lambda receipt: save_record(root / (args.session + ".source-" + uuid.uuid4().hex + ".json"), receipt))
        check_binding(bindings[0], session, current_head)
    # stdin is plain worker input; returned bytes never become a host
    # command or a successful execution envelope.
    channel = args.notfound_channel if args.verb == "exec-auto" else None
    fd = probe_channel(channel) if channel is not None else None
    remote_channel = "/workspace/tmp/notfound-" + uuid.uuid4().hex
    if fd is not None:
        argv = ["/usr/bin/env", "--", "BOSS_PROBE_NOTFOUND=" + remote_channel, *argv]
    record = lambda receipt: save_record(root / (args.session + ".exec-" + uuid.uuid4().hex + ".json"), receipt)
    try:
        result = observed_exec(
            session, argv, b"" if args.verb == "get" else sys.stdin.buffer.read(16 * 1024 * 1024 + 1), kube, observe, record)
        if fd is not None:
            telemetry = observed_exec(session, telemetry_argv(remote_channel), b"", kube, observe, record)
            if telemetry.returncode or len(telemetry.stdout) > 1024 * 1024:
                raise RuntimeError("worker telemetry retrieval failed; execution evidence retained")
            current = channel.stat(follow_symlinks=False)
            opened = os.fstat(fd)
            if (current.st_ino, current.st_dev) != (opened.st_ino, opened.st_dev):
                raise RuntimeError("parent telemetry channel was replaced; no write to replacement")
            # Candidate command names remain untrusted telemetry. They
            # can neither select this target nor authorize an effect.
            os.write(fd, telemetry.stdout)
    finally:
        if fd is not None:
            os.close(fd)
    sys.stdout.buffer.write(result.stdout)
    sys.stderr.buffer.write(result.stderr)
    return result.returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        print("dev-build: REFUSED — " + str(error), file=sys.stderr)
        sys.exit(75)
