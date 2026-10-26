#!/usr/bin/python3 -I
"""Execute repository programs in an uncredentialed Pod, never here.

Installed from published main by install-control.sh. The controller owns
Kubernetes calls and its private session record; worker output is data.
"""
import argparse
import base64
import contextlib
from dataclasses import asdict, dataclass
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import sys
import termios
import time
import uuid
from urllib.parse import urlsplit


# Backlog 2346fe79 (2026-10-18): nine workers started in one burst after
# re-activation, each with its own 150Gi Longhorn volume (about 1.35 TiB
# reserved); the eighth and ninth could not place a replica ("not ready
# for workloads") and sat in Init until the launch gave up, and every
# given-up launch left its Job, and the volume it owns, behind. So the
# size and the count are both bounded here. 100Gi still holds the cold
# `cargo test --all-features` target of the whole workspace (~74 GB, the
# dev pod's own measurement in infra/cluster/manifests/boss-dev.yaml,
# where the largest single builder target read 22.7 GB) with room for
# the source, cargo home and bun cache. Six of them reserve 600Gi, under
# the ~1 TiB that seven 150Gi volumes placed before the burst failed.
WORKER_VOLUME_GIB = 100
WORKER_VOLUME = "%dGi" % WORKER_VOLUME_GIB
WORKER_VOLUME_LIMIT = 6
CHECKOUT_RENEWAL_DOOR = "/work/tools/dev-control/dev-build renew-checkout"
# Backlog a386866e (2026-10-19): six is a ceiling, not the budget. The
# space is: node-disk-usage w-1 (ca3c5881) read 928.9 GiB capacity and
# 603.8 GiB available, and Longhorn refuses a replica that would take a
# disk under its minimal-available floor (25%), so 371.6 GiB was
# placeable, three volumes, and a fourth start would have been refused
# by Longhorn naming its floor. The budget reads Longhorn's own disks
# and floor before each start. The read is `list` on its Nodes and `get`
# on the one setting, granted by dev-session-storage-read in
# infra/cluster/manifests/boss-dev-access.yaml, and nothing else there.
LONGHORN_NAMESPACE = "longhorn-system"
LONGHORN_FLOOR_SETTING = "storage-minimal-available-percentage"
# Read with each refusal: what a pod is waiting on is in events, never in
# its status (a pod whose volume cannot attach reads PodInitializing).
UNPLACEABLE_STORAGE = "not ready for workloads"


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


def pod_security():
    return {"fsGroup": 1500, "fsGroupChangePolicy": "OnRootMismatch"}


def security_matches(actual, expected):
    """Authority is exact, including nested keys and boolean types."""
    if isinstance(expected, dict):
        return isinstance(actual, dict) and actual.keys() == expected.keys() and all(security_matches(actual[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(security_matches(a, e) for a, e in zip(actual, expected))
    return type(actual) is type(expected) and actual == expected


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
    if isinstance(expected, bool):
        return actual is expected
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(k in actual and declared_matches(actual[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(declared_matches(a, e) for a, e in zip(actual, expected))
    return actual == expected


# Backlog 07df587a (2026-10-22): the bootstrap text is part of the Job's
# shape, so when a controller with a changed bootstrap was published,
# every worker the previous one launched stopped being recognised:
# observe() refused exec ("worker Job differs from recorded controller
# intent") and a-deleted-manifest-leaves-no-object called them undeclared
# until each was stopped. Recognition takes a live bootstrap only by its
# sha256 here, and every other field must still be today's factory's.
# A bootstrap edit ADDS its digest (a test names it) and keeps the one it
# replaces; an earlier digest may go once every worker launched under it
# has gone, which activeDeadlineSeconds plus ttlSecondsAfterFinished
# bound at 25 hours after the last controller that launched one.
ADMITTED_WORKER_BOOTSTRAPS = frozenset({
    "3b1b901a9352a960602e7cbf9d2860f405e205a7ba0f5e049a9c8a26d1033194",
    # Backlog 12369ed7 (2026-10-22): the trunk fetch and the g-s clear.
    "d2291037c05b8824c9434cb5ca9f03899f12245172fa1751e802f0cd6401c067",
})


def worker_job(name, head, image, forge_url, postgres_image=None, admitted_bootstrap=None):
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
    #
    # Two things make the worker's clone the shape the gate's clone has,
    # because the pre-flight run here is judged by the same lints and pins
    # (backlog 12369ed7, 2026-10-22: `infra/gate.sh --lint` in a worker
    # could never be green on any branch):
    #   - THE TRUNK. A gate clones the forge, so its `origin/main` is the
    #     forge's main; this clone fetched one exact commit and carried no
    #     trunk ref at all, so five baseline lints answered "no trunk ref
    #     found" and a pin that runs one of them failed. The forge's main is
    #     fetched into `refs/remotes/origin/main` — the name the lints try
    #     (infra/lint/lib/trunk-ref.sh) — AFTER the admitted commit is
    #     checked, so FETCH_HEAD still vouches for exactly that commit.
    #   - NO SETGID. The pod's fsGroup makes the volume root setgid, and a
    #     directory made under a setgid directory inherits it, so every
    #     scratch directory a test made under TMPDIR was 02755 where the
    #     gate's is 0755 (converge_hold_sh, registry_token_deposit_sh,
    #     secret_header_sh compare the mode they set). The bit buys nothing
    #     here: runAsGroup is already the fsGroup, so a new file's group is
    #     1500 either way. It is cleared on the directories this bootstrap
    #     makes, before anything is made inside them.
    bootstrap = r'''set -euo pipefail
export HOME=/workspace/bootstrap GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
mkdir -p /workspace/bootstrap /workspace/home /workspace/cargo /workspace/target /workspace/cache /workspace/tmp /workspace/bin /workspace/source
chmod g-s /workspace/bootstrap /workspace/home /workspace/cargo /workspace/target /workspace/cache /workspace/tmp /workspace/bin /workspace/source
git -c core.hooksPath=/dev/null init -q /workspace/source
cd /workspace/source
forge_fetch() { git -c http.followRedirects=false -c core.hooksPath=/dev/null -c credential.helper= -c 'credential.helper=!f() { printf "username=x-access-token\npassword="; cat /bootstrap-forge/token; printf "\n"; }; f' fetch --no-tags -- "$forge" "$@"; }
forge=$2
forge_fetch "$1"
test "$(git rev-parse FETCH_HEAD)" = "$1"
forge_fetch +refs/heads/main:refs/remotes/origin/main
git rev-parse --verify --quiet refs/remotes/origin/main >/dev/null
git -c core.hooksPath=/dev/null checkout -q --detach "$1"
git config --local core.hooksPath /dev/null
git config --local --unset-all credential.helper || test "$?" = 5
test "$(git rev-parse HEAD)" = "$1"
ln -s /workspace/source/infra/dev/wt-cargo /workspace/bin/wt-cargo
ln -s /workspace/source/infra/dev/wt-web /workspace/bin/wt-web
'''

    # A live Job's own bootstrap, recognised only when admitted (07df587a).
    if admitted_bootstrap is not None:
        if not isinstance(admitted_bootstrap, str) or hashlib.sha256(admitted_bootstrap.encode()).hexdigest() not in ADMITTED_WORKER_BOOTSTRAPS:
            raise ValueError("worker bootstrap is not one this controller admits")
        bootstrap = admitted_bootstrap
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
           "securityContext": pod_security(),
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


def observed_pod_spec(spec):
    # The API omits these non-pointer false booleans (first native
    # worker, 2026-10-14). Normalize only their documented false defaults;
    # token mounting and process sharing still need explicit refusal.
    # https://kubernetes.io/docs/reference/kubernetes-api/core/pod-v1/
    return {**spec, **{field: spec.get(field, False)
                      for field in ("hostNetwork", "hostPID", "hostIPC")}}


def native_job_matches(job, expected=None):
    """Conserve the owning Job shape after the shared bounded API defaults."""
    try:
        path = Path(__file__).resolve().parent.parent / "consist-worker/declaration.py"
        loader = importlib.util.spec_from_file_location("worker_api_defaults", path)
        defaults = importlib.util.module_from_spec(loader)
        loader.loader.exec_module(defaults)
        pod = job["spec"]["template"]["spec"]
        init = pod["initContainers"][0]["command"]
        factory = worker_job(job["metadata"]["name"], init[4], pod["containers"][0]["image"], init[5], pod["containers"][1]["image"], init[2])
        if expected is not None and not native_job_matches(expected):
            return False
        actual = defaults.normalized(job)
        if actual is None or worker_error(job):
            return False
        if not defaults.remove_default(actual["spec"]["template"]["spec"]["initContainers"][0], "resources", {}):
            return False
        probe = actual["spec"]["template"]["spec"]["containers"][1]["readinessProbe"]
        for key, value in {"failureThreshold": 3, "successThreshold": 1, "timeoutSeconds": 1}.items():
            if not defaults.remove_default(probe, key, value):
                return False
        return defaults.matches(factory if expected is None else expected, actual, {})
    except (OSError, KeyError, TypeError, IndexError, ValueError, AttributeError):
        return False


def worker_error(job):
    try:
        if job.get("metadata", {}).get("deletionTimestamp") is not None:
            return "worker Job is retiring"
        pod = observed_pod_spec(job["spec"]["template"]["spec"])
        if any(pod.get(k) is not False for k in ("automountServiceAccountToken", "hostNetwork", "hostPID", "hostIPC", "shareProcessNamespace")):
            return "worker carries host or token authority"
        if pod.get("enableServiceLinks") is not False or pod.get("restartPolicy") != "Never":
            return "worker lifecycle or inherited environment changed"
        if len(pod["containers"]) != 2 or len(pod["initContainers"]) != 1:
            return "unexpected container"
        # API defaults may extend other fields; security authority must be
        # exactly the factory's, including the credential-bearing bootstrap.
        if not security_matches(pod.get("securityContext"), pod_security()):
            return "worker Pod security context changed"
        for container in pod["containers"] + pod["initContainers"]:
            context = postgres_container(container.get("image"))["securityContext"] if container.get("name") == "postgres" else security()
            if not security_matches(container.get("securityContext"), context):
                return "container security context changed"
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
        expected = worker_job(job["metadata"]["name"], command[4], worker["image"], command[5], postgres["image"], command[2])
        expected_init = expected["spec"]["template"]["spec"]["initContainers"][0]
        if not declared_matches(init, expected_init) or init.get("envFrom") or init.get("args"):
            return "bootstrap code, mounts or authority changed"
        return None
    except (KeyError, TypeError, IndexError, ValueError):
        return "incomplete worker manifest"


def session_error(session, pod):
    try:
        if pod.get("metadata", {}).get("deletionTimestamp") is not None:
            return "worker Pod is retiring"
        if pod["metadata"]["name"] != session.pod or pod["metadata"]["uid"] != session.pod_uid:
            return "worker Pod identity changed"
        owners = pod["metadata"].get("ownerReferences", [])
        if not any(o.get("kind") == "Job" and o.get("uid") == session.job_uid for o in owners):
            return "worker Job identity changed"
        error = worker_error({"metadata": {"name": session.job}, "spec": {"template": {"spec": pod["spec"]}}})
        if error:
            return error
        spec = pod["spec"]
        expected = worker_job(session.job, session.source_head, spec["containers"][0]["image"], spec["initContainers"][0]["command"][5], spec["containers"][1]["image"])
        if not declared_matches(pod["metadata"].get("labels"), expected["spec"]["template"]["metadata"]["labels"]):
            return "worker Pod fence or controller labels changed"
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


def worker_input(verb, forward, stream):
    # Backlog c0a24b52 (2026-10-17 isolation trial): `exec` read the
    # caller's stdin to EOF before it executed anything, so a caller
    # holding stdin open and idle (an agent shell) hung to its own
    # timeout (180 s, 400 s) with no output, while the same command
    # with </dev/null returned in 5-11 s. The worker now gets a closed
    # stdin unless the caller asks with --stdin; `put` is the one verb
    # whose input IS its stdin.
    if verb == "put" or (forward and verb != "get"):
        return stream.read(16 * 1024 * 1024 + 1)
    # Backlog 056fc8b6 (reviewer run 9bead677): closing it silently
    # turned a script piped to `exec <s> -- bash` into a run of NOTHING
    # that exited 0. Input the caller sent is refused, never dropped.
    if verb in ("exec", "exec-auto") and input_waiting(stream):
        raise ValueError("stdin carries input that %s would drop: the worker gets a closed stdin unless --stdin is given "
                         "(before the session); nothing executed" % verb)
    return b""


def nested_tree(local, path):
    """The innermost working tree strictly inside `local` that holds `path`.

    Backlog f7b43945 (2026-10-21, reviewer runs b52acf3f and 0e001103):
    a worktree under /work/boss/.claude/worktrees took the checkout's
    binding as its nearest ancestor (7dd185c6) and was mapped beneath
    /workspace/source, but the worker's source is the bound tree's own
    commit, fetched from the forge: another working tree nested in it
    (a worktree's .git file, a clone's .git directory) is never there,
    and env died with exit 125 before cargo started. Lexical, so a path
    that does not exist yet answers None.
    """
    for directory in (path, *path.parents):
        if directory == local or not directory.is_relative_to(local):
            return None
        if os.path.lexists(directory / ".git"):
            return directory
    return None


def input_waiting(stream):
    """Whether the caller's stdin holds input, judged without reading it.

    A non-empty regular file, or a pipe or socket with bytes queued now
    (FIONREAD, which is 0 at EOF for both).
    A terminal, /dev/null and an open pipe nobody has written to answer
    no: the last is the idle agent shell c0a24b52 must not wait on, and a
    writer that has not written YET cannot be told from it, so that one
    still runs with a closed stdin. A stream with no descriptor cannot be
    judged without reading it and answers yes, so it is refused.
    """
    try:
        fd = stream.fileno()
    except (AttributeError, OSError, ValueError):
        return True
    info = os.fstat(fd)
    if stat.S_ISREG(info.st_mode):
        return info.st_size > os.lseek(fd, 0, os.SEEK_CUR)
    if stat.S_ISFIFO(info.st_mode) or stat.S_ISSOCK(info.st_mode):
        # The bytes queued, not POLLIN: a socket whose peer shut down or
        # closed reports POLLIN with nothing to read, and libuv (Node)
        # makes a child's stdio a socketpair, so a caller that sent
        # nothing was refused as carrying input (review 42ea788b).
        return struct.unpack("i", fcntl.ioctl(fd, termios.FIONREAD, b"\0\0\0\0"))[0] > 0
    return False


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
    for path in (current, program):
        tree = nested_tree(local, path) if path.is_absolute() else None
        if tree is not None:
            raise ValueError("%s is inside the working tree %s, nested in %s, the source bound to session %s; "
                             "the worker does not hold it (its source is the bound tree's own commit), so it would "
                             "only fail to find the path; bind that tree's own worker with `dev-build start <a commit "
                             "the forge holds that %s descends from> --local-root %s` and run again; nothing executed"
                             % (path, tree, local, binding.get("session"), tree, tree))
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


# Backlog bc6afd15 (2026-10-18, run 46a922d6): `exec-auto -- /bin/bash
# infra/gate.sh --lint` refused with exit 2 before judging any check,
# because a-car-stays-under-the-edit-level asked the instance for its
# edit level from a worker that has, by design, no route to it, and read
# HTTP 000. The lint already takes a launcher's answer as
# BOSS_EDIT_LEVEL_ANSWER (backlog 934ccad1), and the controller is this
# worker's launcher: it holds the door, so it reads the level ONCE and
# hands the door's own object in. Only to the two programs that read it
# (the lint itself, and gate.sh in a pre-flight mode, which scopes it to
# its pre-flight): exported to every exec it would reach the suites that
# run this lint against a stub registry, the class gate-run 5d576fba went
# red on. A caller that names its own instance (BOSS_JOBS_URL) is left to
# the lint, as gate.sh's own door leaves such a pre-flight alone. A read
# that fails is a refusal here, naming the door, never a worker run that
# reports 000.
EDIT_LEVEL_LINT = "infra/lint/a-car-stays-under-the-edit-level.sh"
EDIT_LEVEL_PATH = "/api/tenant/edit-level"


def reads_the_edit_level(argv, cwd, local_root):
    words = argv[1:] if argv and Path(argv[0]).name in ("bash", "sh") else argv
    if not words:
        return False
    script = Path(os.path.normpath(Path(cwd) / words[0]))
    if not script.is_relative_to(local_root):
        return False
    relative = script.relative_to(local_root).as_posix()
    return relative == EDIT_LEVEL_LINT or (relative == "infra/gate.sh" and bool({"--quick", "--lint"} & set(words[1:])))


def door_read(api_path, run=subprocess.run):
    generation = Path(__file__).resolve().parents[2]
    # The generation's own door at its own address: BOSS_JOBS_URL is not
    # passed, so the machine token never goes where a caller pointed it.
    env = {k: os.environ[k] for k in ("HOME", "TMPDIR", "BOSS_ACTOR", "BOSS_ACTOR_FILE", "BOSS_AGENT_RUN") if k in os.environ}
    env.update(PATH=os.environ.get("PATH", "/usr/bin:/bin"), BOSS_SOR_WAIT_SECONDS="60")
    return run(["/bin/bash", str(generation / "infra/dev/boss-api"), "GET", api_path], env=env,
               stdin=subprocess.DEVNULL, capture_output=True, timeout=120, check=False)


def edit_level_hand_in(argv, cwd, local_root, entries, read):
    """The worker env entries that hand a pre-flight reader the instance's edit level."""
    if not reads_the_edit_level(argv, cwd, local_root) or any(e.startswith("BOSS_JOBS_URL=") for e in entries):
        return []
    result = read()
    if result.returncode:
        raise RuntimeError("the instance's edit level could not be read through the door (boss-api GET %s exit %d: %s); "
                           "nothing executed — the worker has no route of its own and would only report HTTP 000"
                           % (EDIT_LEVEL_PATH, result.returncode, (result.stderr or b"").decode("utf-8", "replace").strip()[-2000:]))
    try:
        answer = json.loads(result.stdout)
    except ValueError:
        answer = None
    if not isinstance(answer, dict) or "edit_level" not in answer:
        raise RuntimeError("the instance's edit level could not be read: the door answered GET %s with something that is "
                           "not the edit-level object; nothing executed and no level is guessed" % EDIT_LEVEL_PATH)
    return ["BOSS_EDIT_LEVEL_ANSWER=" + json.dumps(answer, separators=(",", ":"), sort_keys=True)]


def stale_binding(binding, current_head):
    # Backlog 3165dc62 (2026-10-19): "stop the old session" named
    # neither the session nor the start after it, so an operator
    # started a second worker and left this binding LIVE.
    return ValueError("source binding is stale: session %s is bound to %s at %s, and that source is at %s; "
                      "retire it with `dev-build stop %s`, then admit the new exact source with "
                      "`dev-build start %s --local-root %s`" % (
                          binding.get("session"), binding.get("local_root"), binding.get("source_head"),
                          current_head, binding.get("session"), current_head, binding.get("local_root")))


def same_worker(binding, session, current_head):
    """The binding names this worker and the sha it was admitted on."""
    if binding.get("session") != session.job or binding.get("source_head") != session.source_head:
        raise stale_binding(binding, current_head)


def check_binding(binding, session, current_head, synced_head=None):
    """The worktree is at the head the worker holds: the admitted sha, or
    the last commit a re-sync carried it to (ceea10b8)."""
    same_worker(binding, session, current_head)
    if current_head != (synced_head or session.source_head):
        raise stale_binding(binding, current_head)


# Backlog ceea10b8, design ac162341 (Q1, answered yes by David,
# 2026-10-20): a worker is admitted on one exact sha, so a builder's first
# commit made every later exec-auto refuse stale, and the only way on was
# push, stop and a fresh 100Gi worker per commit; the activation rehearsal
# could never pass its own commit-then-build check. The worker's tree was
# already "admitted sha plus whatever the builder put", so a commit that
# DESCENDS from the head the worker holds is now shipped through the same
# put door, plus a remove under the same path rules, path by path. The
# binding record is never rewritten (save_record publishes once): the
# admitted sha stays its `source_head`, and each re-sync is its own
# record, <session>.sync-<from>.json, naming the commit it carried the
# worker to, so the head the worker holds is that chain followed from the
# admitted sha. No credential and no network path reaches the worker; the
# bytes are the ones a hand put already carries, now with a receipt each.
SOURCE_LIMIT = 16 * 1024 * 1024
# The only file modes a re-sync carries; the put door writes exactly these.
SHIPPED_MODES = {"100644": "644", "100755": "755"}


def followed_head(root, binding):
    """The head the bound worker holds: the admitted sha, then every re-sync from it."""
    head, seen = binding["source_head"], set()
    while True:
        path = root / ("%s.sync-%s.json" % (binding["session"], head))
        try:
            record = load_record(path)
        except FileNotFoundError:
            return head
        if (record.get("session") != binding["session"] or record.get("from") != head or head in seen
                or record.get("admitted_head") != binding["source_head"]
                or not re.fullmatch(r"[0-9a-f]{40}", str(record.get("synced_head")))):
            raise ValueError("re-sync record %s does not continue this binding; nothing executed" % path)
        seen.add(head)
        head = record["synced_head"]


def changed_files(raw):
    """`git diff --raw -z --no-renames` as (old mode, new mode, new blob, status, path)."""
    fields = raw.split(b"\0")
    if fields and fields[-1] == b"":
        fields.pop()
    if len(fields) % 2:
        raise ValueError("git diff answered an unparseable raw listing")
    entries = []
    for meta, path in zip(fields[0::2], fields[1::2]):
        words = meta.decode("ascii").lstrip(":").split(" ")
        if len(words) != 5:
            raise ValueError("git diff answered an unparseable raw entry")
        entries.append((words[0], words[1], words[3], words[4], path.decode("utf-8")))
    return entries


def resync(session, binding, synced, head, root, git, ship):
    """Carry the worker from `synced` to the worktree's committed `head`.

    `git(args, stdin=None)` reads the bound worktree's metadata; `ship(argv,
    stdin)` runs one put-door argv in the observed worker. Everything is
    judged before anything ships, and the head advances only after every
    path's receipt: a refusal ships nothing, a failure part way leaves the
    worker's head where it was and an intent naming what was attempted.
    """
    def refused(why):
        return ValueError("source binding cannot follow the worktree from %s to %s: %s; nothing shipped and the worker "
                          "still holds %s — `dev-build stop %s`, push the commit and admit it as the new exact source"
                          % (synced, head, why, synced, session.job))

    def interrupted(planned):
        # A re-sync that stopped part way left the worker between two
        # commits. Shipping again is exact only when this head's diff
        # covers every path that attempt may have written.
        for path in sorted(root.glob(session.job + ".resync-intent-*.json")):
            intent = load_record(path)
            if intent.get("from") == synced and not set(intent.get("paths", [])) <= planned:
                raise ValueError("source binding cannot follow the worktree from %s to %s: an interrupted re-sync from "
                                 "it (%s) wrote paths this commit does not carry, so what the worker holds is unknown; "
                                 "this exec-auto shipped nothing — `dev-build stop %s`, push the commit and admit it as "
                                 "the new exact source" % (synced, head, path.name, session.job))

    same_worker(binding, session, head)
    if head == synced:
        interrupted(set())
        return synced
    ancestry = git(["merge-base", "--is-ancestor", synced, head])
    if ancestry.returncode == 1:
        raise refused("it does not descend from the head the worker holds (a rebase, reset or amend)")
    if ancestry.returncode:
        raise refused("git could not say whether it descends (merge-base exit %d)" % ancestry.returncode)
    diff = git(["diff", "--raw", "-z", "--no-renames", "--no-abbrev", "--no-ext-diff", "--no-textconv", synced, head])
    if diff.returncode:
        raise refused("git diff exit %d" % diff.returncode)
    try:
        entries = changed_files(diff.stdout)
    except (ValueError, UnicodeDecodeError) as error:
        raise refused("its changed paths cannot be read (%s)" % error) from None
    plan = []
    for old_mode, new_mode, blob, status, path in entries:
        try:
            source_file_argv("remove", path)
        except ValueError:
            raise refused("%s is a path the put door refuses" % path) from None
        if "120000" in (old_mode, new_mode):
            raise refused("%s is a symlink" % path)
        if "160000" in (old_mode, new_mode):
            raise refused("%s is a submodule" % path)
        if status == "D":
            plan.append(("remove", path, None, None))
        elif status == "M" and old_mode != new_mode:
            raise refused("%s carries a mode change (%s to %s)" % (path, old_mode, new_mode))
        elif status in ("A", "M") and new_mode in SHIPPED_MODES:
            plan.append(("put", path, blob, SHIPPED_MODES[new_mode]))
        else:
            raise refused("%s changed as %s %s to %s, which the put door cannot carry" % (path, status, old_mode, new_mode))
    # Review b831d89e N2: a commit that turns a directory into a file lists
    # the new file before the directory's removals, the put cannot replace
    # a directory, and the remove never deletes one, so no retry of it can
    # succeed. Refused here, before anything ships. (A file turned into a
    # directory is listed removal first and ships as it is.)
    removed = {path for op, path, _, _ in plan if op == "remove"}
    for op, path, _, _ in plan:
        if op == "put" and any(gone.startswith(path + "/") for gone in removed):
            raise refused("%s replaces a directory with a file, which the put door cannot carry" % path)
    interrupted({path for _, path, _, _ in plan})
    blobs = [blob for op, _, blob, _ in plan if op == "put"]
    if blobs:
        sizes = git(["cat-file", "--batch-check"], stdin="".join(b + "\n" for b in blobs).encode())
        lines = sizes.stdout.decode("ascii", "replace").splitlines()
        if sizes.returncode or len(lines) != len(blobs):
            raise refused("git could not size the committed files (cat-file exit %d)" % sizes.returncode)
        for (op, path, blob, _), line in zip([p for p in plan if p[0] == "put"], lines):
            words = line.split(" ")
            if len(words) != 3 or words[0] != blob or words[1] != "blob" or not words[2].isdigit():
                raise refused("git could not size %s" % path)
            if int(words[2]) > SOURCE_LIMIT:
                raise refused("%s is %s bytes, over the put door's 16 MiB" % (path, words[2]))
    admitted = binding["source_head"]
    # Backlog 46b63903 (2026-10-22, run 96f79210): the files followed the
    # commit and the worker's git did not — HEAD stayed at the admitted
    # sha, so `git show HEAD:` there read the old tree and `git status`
    # called the commit unstaged edits. The commit's own objects ship too,
    # and HEAD becomes that exact sha. Packed back to the ADMITTED sha,
    # which every worker fetched at bootstrap, so a worker re-synced before
    # this (files at the synced head, HEAD still admitted) follows as well.
    objects = git(["pack-objects", "--stdout", "--revs", "-q"], stdin=("%s\n^%s\n" % (head, admitted)).encode())
    if objects.returncode:
        raise refused("git could not pack the commit's objects (pack-objects exit %d)" % objects.returncode)
    if len(objects.stdout) > SOURCE_LIMIT:
        raise refused("the commit's objects pack to %d bytes, over the put door's 16 MiB" % len(objects.stdout))
    save_record(root / ("%s.resync-intent-%s.json" % (session.job, uuid.uuid4().hex)),
                {"session": session.job, "admitted_head": admitted, "from": synced, "to": head,
                 "paths": sorted(path for _, path, _, _ in plan)})
    def stopped(path, said):
        return RuntimeError("re-sync stopped at %s: %s; the worker's head stays %s, and the next exec-auto on %s "
                            "ships every path again — if it stops here again, `dev-build stop %s` and admit a new "
                            "exact source" % (path, said, synced, head, session.job))
    receipts = []
    for op, path, blob, mode in plan:
        if op == "put":
            content = git(["cat-file", "blob", blob])
            if content.returncode or len(content.stdout) > SOURCE_LIMIT:
                raise stopped(path, "git cat-file exit %d" % content.returncode)
            # Review b831d89e N1: the bytes shipped are the blob the commit
            # names, whatever the repository's replace refs, alternates or
            # configuration say — checked against the id itself.
            if blob_id(content.stdout, len(blob)) != blob:
                raise stopped(path, "the bytes git read for blob %s do not hash to that id" % blob)
            digest = hashlib.sha256(content.stdout).hexdigest()
            result = ship(source_file_argv("put", path, mode), content.stdout)
            if result.returncode or result.stdout.decode("ascii", "replace").strip() != digest:
                raise stopped(path, "the worker answered exit %d, not the sha256 of the committed bytes" % result.returncode)
            receipts.append({"path": path, "op": "put", "mode": mode, "sha256": digest})
        else:
            result = ship(source_file_argv("remove", path), b"")
            said = result.stdout.decode("ascii", "replace").strip()
            if result.returncode or said not in ("removed", "absent"):
                raise stopped(path, "the worker answered exit %d to the remove" % result.returncode)
            receipts.append({"path": path, "op": "remove", "result": said})
    # Last, once every file is in place: the worker's git answers the head
    # it now holds, or the re-sync has not finished.
    result = ship(source_head_argv(head), objects.stdout)
    if result.returncode or result.stdout.decode("ascii", "replace").strip() != head:
        raise stopped("the worker's git HEAD", "the worker answered exit %d, not HEAD at %s" % (result.returncode, head))
    git_head = {"head": head, "objects_sha256": hashlib.sha256(objects.stdout).hexdigest(),
                "objects_bytes": len(objects.stdout)}
    record = root / ("%s.sync-%s.json" % (session.job, synced))
    try:
        save_record(record, {"session": session.job, "admitted_head": admitted, "from": synced,
                             "synced_head": head, "paths": receipts, "git_head": git_head})
    except FileExistsError:
        # A concurrent exec-auto from the same worktree shipped the same
        # bytes and recorded first; any other head is a second line.
        recorded = load_record(record).get("synced_head")
        if recorded != head:
            # Review b831d89e B1: `follow` serialises exec-autos, so only a
            # re-sync run outside it reaches here — but this one SHIPPED,
            # over a worker the record now says holds another head. Its
            # paths are left as an intent from the head the chain now
            # ends at, so the next exec-auto refuses unless it ships them.
            now = followed_head(root, binding)
            save_record(root / ("%s.resync-intent-%s.json" % (session.job, uuid.uuid4().hex)),
                        {"session": session.job, "admitted_head": admitted, "from": now, "to": head,
                         "paths": sorted(path for _, path, _, _ in plan)})
            raise ValueError("source binding cannot follow the worktree from %s to %s: another re-sync from it "
                             "recorded the worker at %s (%s) while this one shipped %d path(s) over it, so what the "
                             "worker holds is unknown — `dev-build stop %s`, push the commit and admit it as the new "
                             "exact source" % (synced, head, recorded, record.name, len(receipts), session.job)) from None
    return head


def blob_id(raw, width):
    """The git object id of `raw` as a blob, sha1 (40 hex) or sha256 (64)."""
    return hashlib.new("sha1" if width == 40 else "sha256", b"blob %d\0" % len(raw) + raw).hexdigest()


def follow(session, binding, head, root, git, ship):
    """Read the head the worker holds and carry it to `head`, under one lock.

    Review b831d89e B1 (2026-10-20): two exec-autos from one worktree, a
    commit between them, each read the held head unlocked; one recorded
    the worker at H2 while the other's put of a file at H1 landed after
    it, and the next exec-auto built on a tree the record did not attest.
    The storage_budget_lock pattern (8fb56e63): an exclusive flock per
    session, held from the read of the held head through the record
    write, makes read-plan-ship-record one step for every exec-auto on
    this worker. Returns (the head it held, the head it holds now).
    """
    with controller_lock(root / (session.job + ".resync.lock")):
        held = followed_head(root, binding)
        return held, resync(session, binding, held, head, root, git, ship)


def nearest_binding(bindings, cwd, retired):
    """The one live binding that owns `cwd`: the NEAREST ancestor root.

    Backlog 7dd185c6 (2026-10-19): a builder worktree under
    /work/boss/.claude/worktrees/<id> nests inside the shared checkout,
    whose own binding (the doors run from /work/boss) always matches as
    an ancestor too. Requiring exactly one match refused every nested
    builder the moment the checkout was bound, which was the first
    isolated build after re-activation. The nearest root wins; two live
    bindings on the SAME root are still ambiguous and still refused, and
    no match is still refused, in the same words the door always used.
    """
    here = Path(cwd)
    owning = [b for b in bindings if here.is_relative_to(Path(b["local_root"]))]
    # The nearest root is chosen among EVERY binding that ever owned it,
    # retired ones included: a worktree whose own worker was stopped must
    # get a fresh one, never fall through to its parent checkout's worker,
    # whose source does not contain the worktree at all.
    nearest = max((len(Path(b["local_root"]).parts) for b in owning), default=0)
    live = [b for b in owning if len(Path(b["local_root"]).parts) == nearest and not retired(b)]
    if len(live) != 1:
        raise ValueError("source has no unique controller-owned worker binding; nothing executed")
    return live[0]


def checkout_worker(cwd, root, read, retire, allocate, recover, head_at, release_failed=None, require_renewal=False):
    """Renew only the shared checkout's expired worker, under a durable lock.

    Builder bindings never enter this door. An interrupted allocation keeps
    its identity; an acknowledged retirement permits a recorded fresh attempt.
    """
    if cwd != "/work/boss":
        return None
    door = CHECKOUT_RENEWAL_DOOR
    old_name = "unknown"
    actionable = False
    try:
        with controller_lock(root / "checkout-renewal.lock"):
            pending = [p for p in root.glob("checkout-renewal-*.json")
                       if not p.name.endswith(".complete.json") and not p.with_suffix(".complete.json").exists()]
            if len(pending) > 1:
                raise ValueError("ambiguous unfinished checkout renewal")
            if pending:
                path = pending[0]
                intent = load_record(path)
                old_name = intent["old"]
                if (intent.get("local_root") != cwd or
                        not re.fullmatch(r"dev-build-[0-9a-f]{16}", old_name) or
                        not re.fullmatch(r"dev-build-[0-9a-f]{16}", intent.get("new", "")) or
                        not re.fullmatch(r"[0-9a-f]{40}", intent.get("head", "")) or
                        path.name != "checkout-renewal-" + old_name + ".json"):
                    raise ValueError("unsafe retained checkout renewal identity")
                actionable = True
                live = [load_record(p) for p in root.glob("dev-build-*.binding.json")]
                live = [b for b in live if b.get("local_root") == cwd and not retirement_requested(root, b["session"])]
                if len(live) > 1:
                    raise ValueError("ambiguous live checkout bindings")
                if live and live[0]["session"] not in (old_name, intent["new"]):
                    # A normal manual start may have repaired an unknown
                    # create. Validate that live identity before it supersedes
                    # this unfinished attempt; never erase the unknown cause.
                    binding = live[0]
                    replacement = Session(**load_record(root / (binding["session"] + ".json")))
                    same_worker(binding, replacement, binding["source_head"])
                    listing = read(["get", "jobs", "--field-selector", "metadata.name=" + replacement.job])
                    jobs = listing.get("items") if isinstance(listing, dict) else None
                    if (listing.get("kind") not in ("List", "JobList") or not isinstance(jobs, list) or
                            listing.get("metadata", {}).get("continue") or len(jobs) != 1 or
                            jobs[0].get("metadata", {}).get("name") != replacement.job or
                            jobs[0]["metadata"].get("namespace") != "boss-dev" or
                            jobs[0]["metadata"].get("uid") != replacement.job_uid or
                            not native_job_matches(jobs[0], retained_intent(replacement.job, root)["job"]) or
                            any(c.get("type") == "Failed" and c.get("status") == "True"
                                for c in jobs[0].get("status", {}).get("conditions", []))):
                        raise ValueError("superseding checkout native identity is not live")
                    save_record(path.with_suffix(".complete.json"),
                                {"intent": intent, "session": asdict(replacement), "superseded": True,
                                 "cause": "independently admitted live checkout binding supersedes unfinished renewal"})
                    return binding
            else:
                bindings = [load_record(p) for p in root.glob("dev-build-*.binding.json")]
                binding = nearest_binding(bindings, cwd, lambda b: retirement_requested(root, b["session"]))
                if binding["local_root"] != cwd:
                    raise ValueError("checkout has no exact worker binding")
                old_name = binding["session"]
                session = Session(**load_record(root / (old_name + ".json")))
                if session.job != old_name or session.source_head != binding["source_head"]:
                    raise ValueError("checkout session identity changed")
                listing = read(["get", "jobs", "--field-selector", "metadata.name=" + old_name])
                if (not isinstance(listing, dict) or listing.get("kind") not in ("List", "JobList") or
                        not isinstance(listing.get("items"), list) or not isinstance(listing.get("metadata", {}), dict) or
                        listing.get("metadata", {}).get("continue")):
                    raise ValueError("incomplete native Job inventory")
                if any(not isinstance(j, dict) or not isinstance(j.get("metadata"), dict) or
                       j["metadata"].get("name") != old_name or not j["metadata"].get("uid") or
                       j["metadata"].get("namespace") != "boss-dev" for j in listing["items"]):
                    raise ValueError("malformed native Job inventory")
                jobs = [j for j in listing["items"] if j["metadata"]["name"] == old_name]
                if len(jobs) > 1:
                    raise ValueError("ambiguous native worker identity")
                if jobs:
                    job = jobs[0]
                    if job["metadata"]["uid"] != session.job_uid:
                        raise ValueError("native worker identity changed")
                    if not native_job_matches(job, retained_intent(old_name, root)["job"]):
                        raise ValueError("native worker specification changed")
                    expired = any(c.get("type") == "Failed" and c.get("status") == "True" and
                                  c.get("reason") == "DeadlineExceeded" for c in job.get("status", {}).get("conditions", []))
                    if not expired:
                        if require_renewal:
                            raise RuntimeError("no renewal: checkout worker %s is not expired or absent" % old_name)
                        return binding
                # An empty inventory on the wrong connection is not proof of
                # absence. Check the named, existing dev deployment too.
                control = read(["get", "deployment", "boss-dev"])
                if (control.get("kind") != "Deployment" or control.get("metadata", {}).get("name") != "boss-dev" or
                        control["metadata"].get("namespace") != "boss-dev" or not control["metadata"].get("uid")):
                    raise ValueError("native checkout connection positive control failed")
                head = head_at(Path(cwd))
                if not re.fullmatch(r"[0-9a-f]{40}", head):
                    raise ValueError("checkout source identity is unavailable")
                intent = {"old": old_name, "new": "dev-build-" + uuid.uuid4().hex[:16],
                          "head": head, "local_root": cwd}
                path = root / ("checkout-renewal-" + old_name + ".json")
                save_record(path, intent)
                actionable = True
            old = Session(**load_record(root / (old_name + ".json")))
            if old.job != old_name:
                raise ValueError("retained checkout session identity changed")
            retired_path = root / (old_name + ".retired.json")
            if not retired_path.exists():
                # Existing retirement is UID-conditioned, including an absent
                # Job. Retrying an intent still retries that exact old UID.
                retire(old)
            if not retired_path.exists():
                raise ValueError("old checkout retirement is not acknowledged")
            retired_session = load_record(retired_path).get("session", {})
            if (retired_session.get("job"), retired_session.get("job_uid")) != (old.job, old.job_uid):
                raise ValueError("old checkout retirement identity changed")
            name = intent["new"]
            visited = set()
            while True:
                # Review 8ecbcd44: give_up saves retirement before refusing
                # launch. recover_start deliberately cannot revive that
                # identity. Keep each abandoned attempt, and advance only
                # after its exact Job retirement has been acknowledged.
                if name in visited:
                    raise ValueError("cyclic checkout replacement history")
                visited.add(name)
                abandoned = root / (name + ".abandoned.json")
                if retirement_requested(root, name) and not abandoned.exists():
                    failed = Session(**load_record(root / (name + ".retire-intent.json"))["session"])
                    if failed.job != name or failed.source_head != intent["head"] or not failed.job_uid:
                        raise ValueError("failed checkout replacement identity changed")
                    acknowledged = root / (name + ".retired.json")
                    if not acknowledged.exists():
                        retire(failed)
                    observed = load_record(acknowledged).get("session", {})
                    if (observed.get("job"), observed.get("job_uid")) != (failed.job, failed.job_uid):
                        raise ValueError("failed checkout replacement retirement is not acknowledged")
                    failure = root / (name + ".failure.json")
                    cause = load_record(failure).get("cause") if failure.exists() else "replacement retirement acknowledged"
                    save_record(abandoned, {"intent": intent, "cause": cause,
                        "next": dict(intent, new="dev-build-" + uuid.uuid4().hex[:16])})
                if not abandoned.exists():
                    if not (root / (name + ".json")).exists():
                        if (root / (name + ".intent.json")).exists():
                            try:
                                recover(name, Path(cwd))
                            except NoJobAcknowledged as error:
                                if release_failed is None:
                                    raise
                                # Stop may retire a matching observed Job by
                                # UID, or release an unowned volume. A Job
                                # retirement is recovered on the next call's
                                # retire-intent path, not guessed as this
                                # volume-release acknowledgment.
                                released = release_failed(name)
                                if released.get("name") != name or released.get("state") not in (
                                        "volume released to garbage collection", "no volume of this name exists; nothing held"):
                                    raise ValueError("unacknowledged checkout volume release was not acknowledged")
                                save_record(abandoned, {"intent": intent, "cause": str(error), "release": released,
                                    "next": dict(intent, new="dev-build-" + uuid.uuid4().hex[:16])})
                        else:
                            allocate(name, intent["head"], Path(cwd))
                    if not abandoned.exists():
                        break
                record = load_record(abandoned)
                next_intent = record.get("next", {})
                if (record.get("intent") != intent or not record.get("cause") or
                        set(next_intent) != {"old", "new", "head", "local_root"} or
                        any(next_intent.get(k) != intent[k] for k in ("old", "head", "local_root")) or
                        not re.fullmatch(r"dev-build-[0-9a-f]{16}", next_intent.get("new", "")) or
                        next_intent["new"] == name):
                    raise ValueError("unsafe abandoned checkout replacement record")
                intent = next_intent
                name = intent["new"]
            renewed = load_record(root / (name + ".binding.json"))
            session = Session(**load_record(root / (name + ".json")))
            if (renewed != {"session": name, "local_root": cwd, "source_head": intent["head"]} or
                    session.job != name or session.source_head != intent["head"] or retirement_requested(root, name)):
                raise ValueError("renewed checkout identity is incomplete")
            save_record(path.with_suffix(".complete.json"), {"intent": intent, "session": asdict(session)})
            print("dev-build: renewed expired checkout worker %s as %s" % (old_name, name), file=sys.stderr)
            return renewed
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        if str(error).startswith("no renewal:"):
            raise
        advice = "; retry the supported %s door from /work/boss" % door if actionable else ""
        raise RuntimeError("checkout worker %s unavailable%s: %s" % (old_name, advice, error)) from error


def retirement_requested(root, name):
    return (root / (name + ".retire-intent.json")).exists()


def retire_worker(session, root, delete):
    # Backlog bc8db3ba (2026-10-18): the intent is published with os.link
    # and so refuses an existing path; after one failed delete every later
    # `stop` refused with File exists and the Job and its volume stayed for
    # a day. An intent already on file for THIS Job incarnation is the
    # same request (execution is already disabled), so the delete is tried
    # again: it is conditioned on the Job UID, so a retry cannot touch a
    # replacement. A retired worker answers its record and is not deleted
    # twice. Pod fields are not compared: the handle `stop` builds for an
    # unadmitted worker has none, and the delete does not use them.
    def same_job(path):
        record = load_record(path)
        recorded = record.get("session") if isinstance(record, dict) else None
        recorded = recorded if isinstance(recorded, dict) else {}
        if (recorded.get("job"), recorded.get("job_uid")) != (session.job, session.job_uid):
            raise ValueError("%s records Job UID %s, not %s; no delete" % (path, recorded.get("job_uid"), session.job_uid))
    retired = root / (session.job + ".retired.json")
    if retired.exists():
        same_job(retired)
        return load_record(retired)
    intent = root / (session.job + ".retire-intent.json")
    retry = False
    try:
        save_record(intent, {"session": asdict(session), "state": "retirement requested; execution disabled"})
    except FileExistsError:
        same_job(intent)
        retry = True
    result = delete(session.job, session.job_uid,
                    lambda receipt: save_record(root / (session.job + ".lifecycle-" + uuid.uuid4().hex + ".json"), receipt))
    accepted = (result.get("kind") == "Status" and result.get("status") == "Success") or (
        result.get("kind") == "Job" and result.get("metadata", {}).get("uid") == session.job_uid and result.get("metadata", {}).get("deletionTimestamp"))
    # Backlog cae3b25b (2026-10-20): a retry whose first delete succeeded
    # with its answer lost met 404 and refused. Backlog 3165dc62
    # (2026-10-19): so did a FIRST stop of a session whose Job its TTL or
    # a clean-up had removed, and the stale-binding refusal sent the
    # operator to exactly that stop. A NotFound for this Job's name proves
    # no Job of that name exists, so none with this UID (a same-name
    # replacement answers 409 on the UID precondition, never 404), on any
    # attempt. It is recorded as retired with the 404 as its receipt,
    # never as an acknowledged delete, and `retried_intent` says whether
    # an earlier stop had asked (review 82b47175 N1 on 678fd302).
    # The check is the lifecycle client's own job_absent, so the two
    # cannot disagree about what a NotFound for this Job is (N2).
    absent = lifecycle_module().job_absent(json.dumps(result), session.job) is not None
    if absent and not accepted:
        record = {"session": asdict(session), "state": "local execution retired", "job_delete_acknowledged": False,
                  "job_absent": True, "job_absent_receipt": result, "retried_intent": retry,
                  "note": "the UID-conditioned delete found no Job of this name (404): a TTL, a clean-up or an earlier "
                          "delete whose answer was lost retired it; PVC garbage collection remains to be observed"}
        save_record(root / (session.job + ".retired.json"), record)
        return record
    if not accepted:
        raise RuntimeError("retirement acknowledgment is not a supported API result; execution stays disabled")
    record = {"session": asdict(session), "state": "local execution retired", "job_delete_acknowledged": True,
              "job_deletion_complete": False, "note": "Job/PVC garbage collection remains to be observed"}
    save_record(root / (session.job + ".retired.json"), record)
    return record


def local_git(path, args, record, run=subprocess.run, stdin=None, timeout=120):
    # Metadata-only built-in: no checkout, hooks, filters, network,
    # source executables, pager or inherited git configuration.
    argv = ["/usr/bin/git", "--no-optional-locks", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-C", str(path), *args]
    # No replace refs either (review b831d89e N1): `git replace` in the
    # worktree's repository would otherwise answer another object's bytes.
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
           "GIT_NO_REPLACE_OBJECTS": "1"}
    result = run(argv, env=env, input=stdin, capture_output=True, timeout=timeout, check=False)
    receipt = {"argv": argv, "exit": result.returncode,
               "stderr_base64": base64.b64encode(result.stderr).decode("ascii")}
    if args[:2] == ["cat-file", "blob"] or args[:1] == ["pack-objects"]:
        # A blob read is named by its id, which the object store keeps;
        # its receipt carries the digest, not a second copy of the file.
        # A re-sync's pack (46b63903) is the same: the commits it names
        # are in the store.
        receipt.update(stdout_sha256=hashlib.sha256(result.stdout).hexdigest(), stdout_bytes=len(result.stdout))
    else:
        receipt["stdout_base64"] = base64.b64encode(result.stdout).decode("ascii")
    record(receipt)
    return result


def source_head(path, record, run=subprocess.run):
    result = local_git(path, ["rev-parse", "HEAD"], record, run=run, timeout=30)
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


def hand_put_mode(root, session, path):
    """The mode a hand put writes: the bound worktree's own file's, as git
    records it (100755 when the owner may execute it), or None for the
    door's old 644 when there is no binding or no regular file to read.
    Backlog de1ec6d1 (2026-10-22): a hand put named no mode, so a 755
    script put into a worker landed 644 and its test failed falsely until
    a chmod inside the worker. Only the controller's own records and the
    builder's worktree are read; the path is checked by source_file_argv."""
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", session):
        return None
    record = root / (session + ".binding.json")
    if not record.is_file():
        return None
    try:
        info = os.lstat(Path(load_record(record)["local_root"]) / path)
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(info.st_mode):
        return None
    return SHIPPED_MODES["100755" if info.st_mode & stat.S_IXUSR else "100644"]


def hand_door_binding(root, session, cwd):
    """The binding a hand `put` or `exec` by id must be run from, or None
    for a worker started with no binding.

    Backlog f959a6b7 (2026-10-22, run dd16d27a): exec-auto from a builder
    worktree reached the shared checkout's worker, the one brief and
    dispatch read gate.sh --roster through; f7b43945 refuses that
    resolution. The hand doors name the worker BY ID and asked nothing of
    the caller's tree, so a builder anywhere could still put a file into,
    or run in, a worker bound to a tree that is not its own. The caller's
    directory must be the bound tree itself, never a working tree nested
    in it (nested_tree), the same test exec-auto applies to its cwd."""
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", session):
        return None
    record = root / (session + ".binding.json")
    if not record.is_file():
        return None
    binding = load_record(record)
    local, here = Path(binding["local_root"]), Path(cwd)
    if not here.is_relative_to(local) or nested_tree(local, here) is not None:
        raise ValueError("%s is not the tree bound to worker %s (%s): a hand put or exec by id runs only from that "
                         "tree, so a builder elsewhere cannot move a worker another tree's doors read through; run it "
                         "from %s, or bind this tree's own worker with `dev-build start <a commit the forge holds> "
                         "--local-root <this tree>`; nothing executed" % (here, session, local, local))
    return binding


def source_file_argv(verb, path, mode=None):
    forbidden = {".git", "target", "node_modules", ".cargo", ".config", ".ssh", ".env"}
    if verb not in ("put", "get", "remove") or not isinstance(path, str) or path.startswith("/") or "\\" in path or "\0" in path or any(p in ("", ".", "..") or p in forbidden for p in path.split("/")):
        raise ValueError("source transfer requires a relative source file, never control metadata or caches")
    # A re-sync (ceea10b8) names the committed mode so an executable stays
    # one; a hand put names its worktree file's (hand_put_mode, de1ec6d1),
    # and one with nothing to read names none and writes 644.
    if mode is not None and (verb != "put" or mode not in SHIPPED_MODES.values()):
        raise ValueError("a source file is shipped 644 or 755, nothing else")
    script = r'''import hashlib, os, stat, sys, uuid
limit = 16 * 1024 * 1024
verb, path, *mode = sys.argv[1:]
parts = path.split("/")
directory = os.open("/workspace/source", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    for part in parts[:-1]:
        if verb == "put":
            try:
                os.mkdir(part, 0o755, dir_fd=directory)
            except FileExistsError:
                pass
        try:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
        except FileNotFoundError:
            if verb != "remove":
                raise
            print("absent")
            sys.exit(0)
        os.close(directory)
        directory = child
    if verb == "put":
        raw = sys.stdin.buffer.read(limit + 1)
        if len(raw) > limit:
            raise ValueError("source edit exceeds 16 MiB; nothing written")
        temporary = ".source-" + uuid.uuid4().hex
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644, dir_fd=directory)
        with os.fdopen(fd, "wb") as output:
            if mode:
                os.fchmod(output.fileno(), int(mode[0], 8))
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, parts[-1], src_dir_fd=directory, dst_dir_fd=directory)
        print(hashlib.sha256(raw).hexdigest())
    elif verb == "remove":
        # Only a regular file goes, never a directory or a symlink; one
        # already gone is the same request finished (a re-sync retried).
        try:
            info = os.stat(parts[-1], dir_fd=directory, follow_symlinks=False)
        except FileNotFoundError:
            print("absent")
        else:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError("source removal requires a regular file; nothing removed")
            os.unlink(parts[-1], dir_fd=directory)
            print("removed")
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
    return ["/usr/bin/python3", "-I", "-c", script, verb, path, *([mode] if mode else [])]


def caller_input(verb, forward, stdin, allow_empty=False):
    """The bytes the caller's stdin hands the worker, read before anything
    is sent, or a ValueError (the REFUSED exit 75) naming what to do.

    Backlog 267af53d (review 42ea788b): with fd 0 closed (`<&-`) sys.stdin
    is None and `sys.stdin.buffer` raised an AttributeError the REFUSED
    handler does not catch, so every verb printed a traceback. A verb
    that sends no input needs none, so only one that would send it
    refuses. Backlog 06e994f7 (run df127edb, 2026-10-19): `put <s> <path>`
    with no `< <file>` wrote an EMPTY file over three of a worker's files,
    each answering e3b0c442..., the sha256 of nothing. A put refuses a
    terminal before reading it and zero bytes after, unless --allow-empty
    says an empty file is meant. It does NOT read the bound tree's file
    instead: the bytes a put sends are the ones its command names, and a
    caller with no input (an agent's /dev/null, not a terminal) would be
    the one case that fallback never reached."""
    stream = getattr(stdin, "buffer", None)
    sends = verb == "put" or (forward and verb != "get")
    if stream is None:
        if sends:
            raise ValueError("stdin is closed, and %s sends it to the worker: %s; nothing executed"
                             % (verb, "dev-build put <session> <path> < <file>" if verb == "put" else
                                "give it the input, or drop --stdin"))
        return b""
    if verb != "put":
        return worker_input(verb, forward, stream)
    if stream.isatty():
        raise ValueError("put reads the file from stdin and stdin is a terminal: "
                         "dev-build put <session> <path> < <file>; nothing sent")
    raw = worker_input(verb, forward, stream)
    if not raw and not allow_empty:
        raise ValueError("put read zero bytes from stdin, and an empty file is not written unless asked: "
                         "dev-build put <session> <path> < <file>, or put --allow-empty to mean it; nothing sent")
    return raw


def source_head_argv(head):
    """The worker argv that takes a re-sync's pack on stdin and makes `head`
    the worker's git HEAD (46b63903): the objects are indexed into the
    worker's repository, then HEAD and the index move to `head` and the
    working tree, which the put door already carried there, is untouched.
    It prints the HEAD git then answers, which the controller compares."""
    if not isinstance(head, str) or not re.fullmatch(r"[0-9a-f]{40}", head):
        raise ValueError("a worker's HEAD is moved only to an exact commit")
    script = r'''import os, subprocess, sys
limit = 16 * 1024 * 1024
head = sys.argv[1]
raw = sys.stdin.buffer.read(limit + 1)
if len(raw) > limit:
    raise ValueError("commit objects exceed 16 MiB; HEAD not moved")
env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1", GIT_NO_REPLACE_OBJECTS="1")
def git(*args, data=None):
    result = subprocess.run(["/usr/bin/git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
                             "-C", "/workspace/source", *args], input=data, env=env, capture_output=True)
    if result.returncode:
        sys.stderr.buffer.write(result.stderr)
        sys.exit(result.returncode)
    return result.stdout
git("index-pack", "--stdin", data=raw)
if git("cat-file", "-t", head).strip() != b"commit":
    raise ValueError("the shipped objects do not hold the commit; HEAD not moved")
git("reset", "-q", "--mixed", head)
sys.stdout.buffer.write(git("rev-parse", "--verify", "HEAD"))
'''
    return ["/usr/bin/python3", "-I", "-c", script, head]


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


def bound_exec(binding, session, argv, stdin, transport, inspect, record):
    """Keep a checkout expiry after the initial check actionable, without retry.

    A command may already have run before its final identity check failed.
    Preserve its receipt and original cause; never replay that command.
    """
    try:
        return observed_exec(session, argv, stdin, transport, inspect, record)
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        if binding is not None and binding.get("local_root") == "/work/boss":
            advice = ""
            try:
                read = lambda command: kube_json(command, transport=lambda native: transport(native, None), record=record)
                if checkout_renewal_needed(session, read):
                    advice = "; supported renewal door: %s from /work/boss" % CHECKOUT_RENEWAL_DOOR
            except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as observation:
                advice = "; renewal eligibility unread: %s" % observation
            raise RuntimeError("checkout worker %s failed; command was not retried%s; %s" %
                               (session.job, advice, error)) from error
        raise


def checkout_renewal_needed(session, read):
    """Name the renewal door only when a native observation says it helps."""
    listing = read(["get", "jobs", "--field-selector", "metadata.name=" + session.job])
    if (not isinstance(listing, dict) or listing.get("kind") not in ("List", "JobList") or
            not isinstance(listing.get("items"), list) or listing.get("metadata", {}).get("continue")):
        raise ValueError("incomplete native checkout observation")
    jobs = listing["items"]
    if len(jobs) > 1:
        raise ValueError("ambiguous native checkout identity")
    if jobs:
        job = jobs[0]
        meta = job.get("metadata", {})
        if (meta.get("name"), meta.get("namespace"), meta.get("uid")) != (session.job, "boss-dev", session.job_uid):
            raise ValueError("native checkout identity changed")
        return any(c.get("type") == "Failed" and c.get("status") == "True" and c.get("reason") == "DeadlineExceeded"
                   for c in job.get("status", {}).get("conditions", []))
    control = read(["get", "deployment", "boss-dev"])
    meta = control.get("metadata", {})
    if (control.get("kind") != "Deployment" or meta.get("name") != "boss-dev" or
            meta.get("namespace") != "boss-dev" or not meta.get("uid")):
        raise ValueError("native checkout connection positive control failed")
    return True


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


def observe(session, root=Path("/work/home/.config/boss/workers"), transport=kube):
    def read(argv):
        return kube_json(argv, transport=transport, record=lambda receipt: save_record(root / (session.job + ".observation-" + uuid.uuid4().hex + ".json"), receipt))
    job = read(["get", "job", session.job])
    if worker_error(job) or job["metadata"]["uid"] != session.job_uid:
        raise RuntimeError("worker Job changed")
    intent = load_record(root / (session.job + ".intent.json"))
    if intent["head"] != session.source_head or intent["name"] != session.job:
        raise RuntimeError("worker source binding changed")
    expected = intent["job"]["spec"]["template"]["spec"]
    if not native_job_matches(job, intent["job"]):
        raise RuntimeError("worker Job differs from recorded controller intent")
    pod = read(["get", "pod", session.pod])
    if not declared_matches(observed_pod_spec(pod["spec"]), expected):
        raise RuntimeError("worker Pod differs from recorded controller intent")
    volume = read(["get", "pvc", session.job])
    if volume["metadata"].get("deletionTimestamp") is not None or not session.volume_uid or volume["metadata"]["uid"] != session.volume_uid or not any(o.get("uid") == session.job_uid and o.get("kind") == "Job" for o in volume["metadata"].get("ownerReferences", [])):
        raise RuntimeError("private worker volume identity or lifecycle owner changed")
    error = session_error(session, pod)
    if error:
        raise RuntimeError(error)
    return pod


def launch_failure(pod, events, final=False):
    """Name why an unadmitted worker cannot become ready, or None.

    A terminated container or unplaceable storage is fatal at once; a
    pod that is merely waiting is named only once the deadline passed.
    """
    status = (pod or {}).get("status", {})
    containers = status.get("initContainerStatuses", []) + status.get("containerStatuses", [])
    for container in containers:
        ended = container.get("state", {}).get("terminated")
        if ended and (ended.get("exitCode") != 0 or container.get("name") != "published-bootstrap"):
            return "%s exited %s (%s)" % (container.get("name"), ended.get("exitCode"), ended.get("reason") or ended.get("message") or "no reason given")
    # Backlog 8fb56e63: events outlive what they describe by an hour, so
    # an attach warning is read only while no container has started; a
    # container that ran had its volume, whatever an older event said.
    started = any("running" in c.get("state", {}) or "terminated" in c.get("state", {}) for c in containers)
    for event in [] if started else events:
        if UNPLACEABLE_STORAGE in (event.get("message") or ""):
            return "storage cannot be placed: %s: %s" % (event.get("reason"), event.get("message"))
    if status.get("phase") in ("Failed", "Succeeded"):
        return "worker Pod %s before admission" % status["phase"]
    if not final:
        return None
    if pod is None:
        return "no worker Pod was created"
    # Most specific first: the scheduler's verdict, then a container's
    # own waiting reason, then the newest warning, then the generic.
    conditions = {c.get("type"): c for c in status.get("conditions", []) if c.get("status") == "False"}
    if "PodScheduled" in conditions:
        return "PodScheduled %s: %s" % (conditions["PodScheduled"].get("reason"), conditions["PodScheduled"].get("message") or "")
    for container in status.get("initContainerStatuses", []) + status.get("containerStatuses", []):
        waiting = container.get("state", {}).get("waiting") or {}
        if waiting.get("reason") not in (None, "PodInitializing", "ContainerCreating"):
            return "%s waiting: %s %s" % (container.get("name"), waiting["reason"], waiting.get("message") or "")
    warnings = [e for e in events if e.get("type") == "Warning"]
    if warnings:
        return "%s: %s" % (warnings[-1].get("reason"), warnings[-1].get("message"))
    if "Initialized" in conditions:
        return "Initialized %s: %s" % (conditions["Initialized"].get("reason"), conditions["Initialized"].get("message") or "")
    return "pod phase %s, no readiness" % status.get("phase")


def stoppable_session(name, root):
    """The session `stop` retires: the admitted record, else the launch's.

    A launch that never admitted its worker leaves no `<name>.json`, so
    `stop` refused exactly the workers that were stuck (2346fe79). The
    one acknowledged Job creation names the UID the retirement needs.
    """
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", name):
        raise ValueError("unknown controller session name")
    if (root / (name + ".json")).exists():
        return Session(**load_record(root / (name + ".json")))
    adoption = root / (name + ".stop-adoption.json")
    if adoption.exists():
        observed = load_record(adoption)
        session = Session(**observed["session"])
        if session.job != name or observed.get("creation_acknowledged") is not False:
            raise ValueError("stop adoption names another worker or fabricated creation acknowledgment")
        return session
    try:
        intent, job_uid = acknowledged_launch(name, root)
        volume = load_record(root / (name + ".volume.json"))
    except NoJobAcknowledged as error:
        raise NoJobAcknowledged("no admitted session or acknowledged launch for %s under %s (%s)" % (name, root, error)) from None
    except (FileNotFoundError, ValueError) as error:
        raise ValueError("no admitted session or acknowledged launch for %s under %s (%s)" % (name, root, error)) from None
    return Session(name, job_uid, "", "", "", intent["head"], volume.get("uid", ""))


def stop(name, root, retire, transport=kube):
    """`dev-build stop`: retire the session's Job, or, when its launch
    never acknowledged one, release the volume that launch left (86bd9fb8)."""
    try:
        session = stoppable_session(name, root)
    except NoJobAcknowledged:
        return release_unowned_volume(name, root, transport, retire=retire)
    if (root / (name + ".stop-adoption.json")).exists() and not (root / (name + ".json")).exists():
        return retire_adopted_volume(session, root, retire, transport)
    return retire(session)


def retire_adopted_volume(session, root, retire, transport):
    """Bind the pinned launch's volume before its UID-conditioned delete.

    A lost patch answer is observed on retry; a lost delete answer never
    changes the pinned UID. Neither cached retirement nor Job absence may
    skip the volume half of a committed-but-unacknowledged creation.
    """
    name = session.job
    intent = retained_intent(name, root)
    volume_ack = load_record(root / (name + ".volume.json"))
    if volume_ack.get("uid") != session.volume_uid or intent["head"] != session.source_head:
        raise ValueError("adopted launch or volume identity changed; nothing retired")
    def call(argv):
        receipt = {"argv": argv, "completion": "unproven"}
        try:
            out = transport(argv, None)
            receipt.update(exit=out.returncode, completion="observed",
                           stdout_base64=base64.b64encode(out.stdout).decode("ascii"),
                           stderr_base64=base64.b64encode(out.stderr).decode("ascii"))
        finally:
            save_record(root / (name + ".adoption-" + uuid.uuid4().hex + ".json"), receipt)
        if out.returncode:
            raise RuntimeError("adoption observation or owner binding failed; full transport evidence retained")
        return out
    def read(kind):
        raw = call(["get", kind, name, "--ignore-not-found", "-o", "json"]).stdout
        return json.loads(raw) if raw.strip() else None
    job = read("job")
    if job is not None and (job.get("metadata", {}).get("uid") != session.job_uid
                            or not native_job_matches(job, intent["job"])):
        raise RuntimeError("adopted Job changed or is already retiring; no owner patch or delete")
    volume = read("pvc")
    if volume is None:
        return {**retire(session), "volume_absent": True}
    meta = volume.get("metadata") or {}
    owner = {"apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": session.job_uid}
    if meta.get("name") != name or meta.get("namespace") != "boss-dev" or meta.get("uid") != session.volume_uid:
        raise RuntimeError("adopted private volume changed; no owner patch or delete")
    owners = meta.get("ownerReferences") or []
    if owners and owners != [owner]:
        raise RuntimeError("adopted private volume has a foreign owner; nothing retired")
    if not owners and meta.get("deletionTimestamp") is not None:
        return {**retire(session), "volume_deletion_observed": True}
    if not owners and meta.get("deletionTimestamp") is None:
        if not meta.get("resourceVersion"):
            raise RuntimeError("volume resourceVersion unread; no owner patch or delete")
        patch = [{"op": "test", "path": "/metadata/uid", "value": session.volume_uid},
                 {"op": "test", "path": "/metadata/resourceVersion", "value": meta["resourceVersion"]},
                 {"op": "add", "path": "/metadata/ownerReferences", "value": [owner]}]
        observed = json.loads(call(["patch", "pvc", name, "--type=json", "-p", json.dumps(patch), "-o", "json"]).stdout)
        if observed.get("metadata", {}).get("uid") != session.volume_uid or observed.get("metadata", {}).get("ownerReferences") != [owner]:
            raise RuntimeError("volume ownership was not acknowledged; no delete")
    binding = {"job_uid": session.job_uid, "volume_uid": session.volume_uid, "owner": owner}
    bound = root / (name + ".stop-volume-bound.json")
    if bound.exists():
        if load_record(bound) != binding:
            raise ValueError("recorded volume binding names another incarnation")
    else:
        save_record(bound, binding)
    return {**retire(session), "volume_handed_to_gc": True, "volume_binding": binding}


def release_unowned_volume(name, root, transport=kube, retire=None):
    """Hand a worker volume no Job owns to the garbage collector.

    Backlog 86bd9fb8 (review ca8d3763, 2026-10-19): a Job create that
    failed after the PVC create left a 100Gi volume with no owner. It
    counted toward WORKER_VOLUME_LIMIT, the budget refusal said `stop`
    releases one, and `stop` refused it for want of a Job UID. The
    dev-session Role may patch a PVC and may not delete one
    (infra/cluster/manifests/boss-dev.yaml), so ownership is the one
    release door it holds, the same one an admitted worker's volume
    leaves by: the volume is given an owner that names a Job of the
    session's name under a UID the API never assigned, the garbage
    collector's own lookup finds that owner absent and deletes the
    volume. It runs only once a read finds no Job of this name — a Job
    whose creation was not acknowledged refuses, rather than lose its
    volume under it — and the patch is tested against the volume UID
    and the resourceVersion that read observed, so it lands on exactly
    the unowned volume read and on nothing that changed since.
    """
    intent = retained_intent(name, root)
    released = root / (name + ".volume-released.json")
    if released.exists():
        return load_record(released)
    try:
        acknowledged_uid = load_record(root / (name + ".volume.json")).get("uid") or None
    except FileNotFoundError:
        # The create timed out or failed unread: no UID was acknowledged,
        # and the volume, if one exists, is identified by the read below.
        acknowledged_uid = None
    def call(argv):
        receipt = {"argv": argv, "completion": "unproven"}
        try:
            out = transport(argv, None)
            receipt.update(exit=out.returncode, completion="observed",
                           stdout_base64=base64.b64encode(out.stdout).decode("ascii"),
                           stderr_base64=base64.b64encode(out.stderr).decode("ascii"))
        finally:
            save_record(root / (name + ".release-" + uuid.uuid4().hex + ".json"), receipt)
        if out.returncode:
            sys.stderr.buffer.write(out.stderr)
            raise RuntimeError("%s: volume release call failed; full transport evidence retained" % name)
        return out
    def read(kind):
        out = call(["get", kind, name, "--ignore-not-found", "-o", "json"])
        body = json.loads(out.stdout) if out.stdout.strip() else None
        if isinstance(body, dict) and body.get("kind", "").endswith("List") and not body.get("items"):
            return None
        return body
    job = read("job")
    if job is not None:
        # c4e91a72: a timed-out create may have committed. An observed
        # adoption authorizes retirement only, never execution or a made-up
        # create ACK. Pin its UID before delete so retries cannot adopt a
        # replacement after an answer was lost.
        meta = job.get("metadata") or {}
        volume = read("pvc")
        volume_meta = (volume or {}).get("metadata") or {}
        owners = volume_meta.get("ownerReferences") or []
        if (retire is not None and acknowledged_uid and meta.get("name") == name
                and meta.get("namespace") == "boss-dev" and meta.get("uid")
                and native_job_matches(job, intent["job"])
                and volume_meta.get("name") == name and volume_meta.get("namespace") == "boss-dev"
                and volume_meta.get("uid") == acknowledged_uid
                and (not owners or owners == [{"apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": meta["uid"]}])):
            session = Session(name, meta["uid"], "", "", "", intent["head"], acknowledged_uid)
            save_record(root / (name + ".stop-adoption.json"),
                        {"session": asdict(session), "creation_acknowledged": False,
                         "purpose": "observed identity authorizes UID-conditioned retirement only"})
            return retire_adopted_volume(session, root, retire, transport)
        raise RuntimeError("%s: a Job of this name exists (uid %s) though no creation of it was acknowledged; no volume release"
                           % (name, (job.get("metadata") or {}).get("uid")))
    volume = read("pvc")
    record = {"name": name, "head": intent["head"], "volume_uid_acknowledged": acknowledged_uid is not None}
    if volume is None:
        record.update(state="no volume of this name exists; nothing held", volume_absent=True)
        save_record(released, record)
        return record
    meta = volume.get("metadata") or {}
    uid = meta.get("uid")
    if meta.get("name") != name or meta.get("namespace") != "boss-dev" or not uid or uid != (acknowledged_uid or uid):
        raise RuntimeError("%s: the volume read is not the one this launch created (uid %s, acknowledged %s); no release"
                           % (name, uid, acknowledged_uid))
    intent_path = root / (name + ".volume-release-intent.json")
    try:
        release = load_record(intent_path)
    except FileNotFoundError:
        release = {"name": name, "volume_uid": uid,
                   "owner": {"apiVersion": "batch/v1", "kind": "Job", "name": name, "uid": str(uuid.uuid4())}}
        save_record(intent_path, release)
    if release.get("volume_uid") != uid:
        raise RuntimeError("%s: an earlier release named volume uid %s, not %s; no release" % (name, release.get("volume_uid"), uid))
    owners = meta.get("ownerReferences") or []
    record.update(volume_uid=uid, owner=release["owner"],
                  note="the owner names a Job that was never created; the garbage collector finds it absent and "
                       "deletes the volume. Deletion remains to be observed")
    if meta.get("deletionTimestamp") is not None or owners == [release["owner"]]:
        # A volume already terminating, or one an earlier stop's patch
        # reached with its answer lost, is released already.
        record.update(state="volume released to garbage collection")
        save_record(released, record)
        return record
    if owners:
        raise RuntimeError("%s: the volume has an owner (%s); its owner's retirement releases it, not this one"
                           % (name, ", ".join("%s %s" % (o.get("kind"), o.get("uid")) for o in owners)))
    patch = [{"op": "test", "path": "/metadata/uid", "value": uid},
             {"op": "test", "path": "/metadata/resourceVersion", "value": meta.get("resourceVersion")},
             {"op": "add", "path": "/metadata/ownerReferences", "value": [release["owner"]]}]
    patched = json.loads(call(["patch", "pvc", name, "--type=json", "-p", json.dumps(patch), "-o", "json"]).stdout)
    if (patched.get("metadata") or {}).get("uid") != uid or release["owner"] not in (patched.get("metadata") or {}).get("ownerReferences", []):
        raise RuntimeError("%s: the volume release was not acknowledged; full transport evidence retained" % name)
    record.update(state="volume released to garbage collection")
    save_record(released, record)
    return record


def retire_unadmitted(name, job_uid, head, volume_uid, pod, root, retire):
    """Retire a launch that will never be admitted; its Job owns the volume."""
    if retire is None:
        return "not retired: this caller supplied no retirement door"
    session = Session(name, job_uid, (pod or {}).get("metadata", {}).get("name", ""),
                      (pod or {}).get("metadata", {}).get("uid", ""), "", head, volume_uid)
    try:
        return retire(session).get("state", "retired")
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        return "not retired: " + str(error)


def condition_true(conditions, kind):
    """Longhorn v1beta2 lists conditions; an absent one is not True."""
    return any(c.get("type") == kind and c.get("status") == "True" for c in conditions or [])


def placeable_disks(nodes, floor_percent):
    """Every disk Longhorn may place a new replica on, with its room.

    Room is what Longhorn's own floor leaves: available bytes less
    `floor_percent` of the disk's maximum, never below zero. A node that
    is not Ready and Schedulable (w-2 while it is off), a node or disk
    that refuses scheduling, and a disk being evicted hold no room. A
    figure the node does not report raises: no evidence is not a pass.
    """
    disks = []
    for node in nodes:
        spec, status = node.get("spec", {}), node.get("status", {})
        if not spec.get("allowScheduling") or spec.get("evictionRequested"):
            continue
        if not (condition_true(status.get("conditions"), "Ready") and condition_true(status.get("conditions"), "Schedulable")):
            continue
        for name, disk in sorted((spec.get("disks") or {}).items()):
            state = (status.get("diskStatus") or {}).get(name) or {}
            if not disk.get("allowScheduling") or disk.get("evictionRequested") or not condition_true(state.get("conditions"), "Ready"):
                continue
            maximum, available = int(state["storageMaximum"]), int(state["storageAvailable"])
            floor = maximum * floor_percent // 100
            disks.append({"node": node["metadata"]["name"], "disk": name, "maximum": maximum,
                          "available": available, "floor": floor, "placeable": max(0, available - floor)})
    return disks


def longhorn_floor(setting):
    value = str(setting.get("value", ""))
    if not value.isdigit() or int(value) > 100:
        raise ValueError("%s reads %r, not a percentage" % (LONGHORN_FLOOR_SETTING, value))
    return int(value)


def storage_refusal(held, disks, floor_percent):
    """Why one more worker volume would not fit, or None.

    Every held worker volume is counted at its full size: a thin volume
    grows to it as its build fills the target. Space it has already
    written is also gone from `available`, so the count errs toward
    refusing early, never toward a start Longhorn then refuses.
    """
    size = WORKER_VOLUME_GIB * 2 ** 30
    slots = sum(d["placeable"] // size for d in disks)
    if len(held) < slots:
        return None
    gib = lambda n: "%.1f" % (n / 2 ** 30)
    measured = "; ".join("%s/%s %s GiB placeable (%s GiB available less the %d%% floor of %s GiB)" % (
        d["node"], d["disk"], gib(d["placeable"]), gib(d["available"]), floor_percent, gib(d["maximum"])) for d in disks)
    return ("worker storage budget reached: %d worker volume(s) held (%s) and Longhorn can place %d of %s: %s; %snothing created"
            % (len(held), ", ".join(held) or "none", slots, WORKER_VOLUME,
               measured or "Longhorn reports no disk a worker volume may be placed on",
               "`dev-build stop <name>` releases one; " if held else ""))


class LaunchRefused(RuntimeError):
    """A start refused after its Job was acknowledged; it names the session."""


@contextlib.contextmanager
def storage_budget_lock(root):
    """Hold the one lock every `start` sharing this session directory takes.

    Backlog 8fb56e63 (2026-10-18): the budget is a list followed by a
    create, two client-side steps, so starts that each listed before any
    created all passed it. Every start runs this controller against the
    same directory, so an exclusive flock there makes list-then-create
    one step for all of them. Closing the descriptor releases it.
    """
    with controller_lock(root / "storage-budget.lock"):
        yield


@contextlib.contextmanager
def controller_lock(path, wait=True):
    """An exclusive flock on `path` in the session directory, released on close."""
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | (0 if wait else fcntl.LOCK_NB))
        yield
    finally:
        os.close(fd)


def launch(head, config, root, local_root=None, transport=None, retire=None, name=None, print_session=True):
    name = name or "dev-build-" + uuid.uuid4().hex[:16]
    job = worker_job(name, head, config["worker_image"], config["forge_url"], config["postgres_image"])
    if worker_error(job):
        raise RuntimeError(worker_error(job))
    transport = transport or kube
    # Backlog 3165dc62 (2026-10-19): the store held 1213 launch receipts,
    # 47 MB, because a waiting launch polls every 2 s and a stuck pod
    # answers the same bytes each time. A read (no stdin) whose answer
    # repeats its own last one is recorded as a pointer to the receipt
    # holding those bytes: every poll stays on the record, its bytes once.
    # A write is never folded; recovery reads the creation's own receipt.
    last = {}
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
            path = root / (name + ".launch-" + uuid.uuid4().hex + ".json")
            key = json.dumps(argv)
            answer = (receipt.get("exit"), receipt.get("stdout_base64"), receipt.get("stderr_base64"))
            if stdin is None and receipt["completion"] == "observed" and key in last and last[key][0] == answer:
                receipt = {"argv": argv, "completion": "observed", "exit": receipt["exit"], "unchanged_from": last[key][1]}
            elif stdin is None and receipt["completion"] == "observed":
                last[key] = (answer, path.name)
            save_record(path, receipt)
    def read(argv):
        result = call([*argv, "-o", "json"])
        if result.returncode:
            sys.stderr.buffer.write(result.stderr)
            raise RuntimeError("worker observation failed; full transport evidence retained")
        return json.loads(result.stdout)
    # The budget is read before anything is written: a refused launch
    # leaves this read's receipt and no intent, volume or Job (2346fe79).
    # A volume still terminating holds its storage, so it counts. The
    # read and the create share one lock (8fb56e63): the next start's
    # list is taken only after this one's volume exists or failed.
    with storage_budget_lock(root):
        held = sorted(v["metadata"]["name"] for v in read(["get", "pvc"]).get("items", [])
                      if re.fullmatch(r"dev-build-[0-9a-f]{16}", v.get("metadata", {}).get("name", "")))
        if len(held) >= WORKER_VOLUME_LIMIT:
            raise RuntimeError("worker storage budget reached: %d of %d worker volumes held (%s); `dev-build stop <name>` releases one; nothing created"
                               % (len(held), WORKER_VOLUME_LIMIT, ", ".join(held)))
        # Below the ceiling, the budget is the measured space (a386866e),
        # read under the same lock so a burst of starts sees each other.
        try:
            floor_percent = longhorn_floor(read(["get", "settings.longhorn.io", LONGHORN_FLOOR_SETTING, "--namespace=" + LONGHORN_NAMESPACE]))
            disks = placeable_disks(read(["get", "nodes.longhorn.io", "--namespace=" + LONGHORN_NAMESPACE]).get("items", []), floor_percent)
        except (RuntimeError, ValueError, KeyError, TypeError, AttributeError) as error:
            raise RuntimeError("worker storage budget cannot be measured: Longhorn's disks or floor were not read (%s); full transport evidence retained; nothing created" % error) from None
        refusal = storage_refusal(held, disks, floor_percent)
        if refusal:
            raise RuntimeError(refusal)
        intent = {"name": name, "head": head, "job": job}
        save_record(root / (name + ".intent.json"), intent)
        # Fresh storage is acknowledged BEFORE a token-bearing init can
        # start. A collision therefore creates no Job. Worker-written
        # source/cache is never mounted back into a privileged bootstrap.
        pvc = {"apiVersion": "v1", "kind": "PersistentVolumeClaim",
               "metadata": {"name": name, "namespace": "boss-dev"},
               "spec": {"accessModes": ["ReadWriteOnce"], "storageClassName": "longhorn-dev-disposable",
                        "resources": {"requests": {"storage": WORKER_VOLUME}}}}
        # Backlog 86bd9fb8 (C): a timed-out create escaped as a bare
        # TimeoutExpired, so a volume that may exist and count toward the
        # budget was named nowhere. Every refusal from here on names the
        # volume and the `stop` that releases what it left.
        release = "`dev-build stop %s` releases it" % name
        try:
            out = call(["create", "-f", "-", "-o", "json"], json.dumps(pvc).encode())
        except subprocess.TimeoutExpired:
            raise RuntimeError("%s: fresh private volume create timed out, outcome unknown: volume %s may exist and count toward "
                               "the worker storage budget; no Job created, no blind retry; %s" % (name, name, release)) from None
    if out.returncode:
        sys.stderr.buffer.write(out.stderr)
        raise RuntimeError("%s: fresh private volume not acknowledged; no Job created, outcome retained; if volume %s exists, %s"
                           % (name, name, release))
    created_volume = json.loads(out.stdout)
    if created_volume["metadata"]["name"] != name or not created_volume["metadata"].get("uid"):
        raise RuntimeError("%s: private volume identity not acknowledged; no Job created; %s" % (name, release))
    volume_uid = created_volume["metadata"]["uid"]
    save_record(root / (name + ".volume.json"), {"name": name, "uid": volume_uid, "lifecycle": "unowned until Job acknowledgment and owner patch"})
    try:
        out = call(["create", "-f", "-", "-o", "json"], json.dumps(job).encode())
    except subprocess.TimeoutExpired:
        raise RuntimeError("%s: worker Job create timed out, outcome unknown; volume %s is unowned and counts toward the worker "
                           "storage budget, no blind retry; %s once no Job of this name exists" % (name, name, release)) from None
    if out.returncode:
        sys.stderr.buffer.write(out.stderr)
        raise RuntimeError("%s: worker launch not acknowledged; volume %s is unowned and counts toward the worker storage budget, "
                           "no blind retry; %s" % (name, name, release))
    created = json.loads(out.stdout)
    if created["metadata"]["name"] != name or not created["metadata"].get("uid"):
        raise RuntimeError("worker Job identity not acknowledged; volume outcome retained")
    job_uid = created["metadata"]["uid"]
    def give_up(pod, events, cause, verdict):
        # Backlog 2346fe79: six workers failed and their Pods were reaped
        # before anyone read why. Keep the cause FIRST, then retire the
        # Job, whose owner reference releases the volume with it.
        # Backlog bc8db3ba: FIRST means before anything that can time out,
        # so the bootstrap log (a cluster read) is kept after the cause in
        # its own record, every step below is guarded, and the refusal
        # names the session and, unless it is retired, the handle that is.
        path = root / (name + ".failure.json")
        try:
            save_record(path, {"name": name, "head": head, "job_uid": job_uid, "volume_uid": volume_uid, "cause": cause,
                               "pod_status": (pod or {}).get("status"), "events": events})
            kept = "cause kept in %s" % path
        except OSError as error:
            kept = "cause NOT kept (%s); transport receipts are under %s" % (error, root)
        said = ""
        if pod is not None and pod.get("metadata", {}).get("name"):
            try:
                log = call(["logs", pod["metadata"]["name"], "-c", "published-bootstrap", "--limit-bytes=65536"])
                save_record(root / (name + ".bootstrap-log.json"), {"bootstrap_log_exit": log.returncode,
                            "bootstrap_log_base64": base64.b64encode(log.stdout).decode("ascii")})
                # The log's last line rides in the refusal; it is fetched
                # data, so nothing unprintable reaches the terminal.
                lines = log.stdout.decode("utf-8", "replace").strip().splitlines()[-1:] if log.returncode == 0 else []
                said = " (bootstrap said: %s)" % "".join(c if c.isprintable() else "?" for c in lines[0][:300]) if lines else ""
            except (OSError, subprocess.TimeoutExpired) as error:
                said = " (bootstrap log unread: %s)" % error
        retirement = retire_unadmitted(name, job_uid, head, volume_uid, pod, root, retire)
        handle = "; `dev-build stop %s` retires it" % name if retirement.startswith("not retired") else ""
        raise LaunchRefused("%s: %s: %s%s; %s; Job retirement: %s%s" % (
            name, verdict, cause, said, kept, retirement, handle))
    pod, events, polls = None, [], 0
    try:
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
            polls += 1
            if pods:
                pod = pods[0]
                # What a waiting pod is stuck on is only in its events; read
                # them every fifth poll rather than double every receipt.
                if polls % 5 == 0:
                    events = read(["get", "events", "--field-selector", "involvedObject.name=" + pod["metadata"]["name"]]).get("items", [])
                cause = launch_failure(pod, events)
                if cause:
                    give_up(pod, events, cause, "worker cannot be admitted")
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
                    if print_session:
                        print(name)
                    return name
            time.sleep(2)
        if pod is not None:
            events = read(["get", "events", "--field-selector", "involvedObject.name=" + pod["metadata"]["name"]]).get("items", [])
        give_up(pod, events, launch_failure(pod, events, final=True), "worker unavailable after 300 seconds, no local fallback")
    except LaunchRefused:
        raise
    except (RuntimeError, ValueError, OSError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        # Backlog bc8db3ba: a failed read, a second Pod or a timeout used
        # to refuse here with no session name, so nobody could stop it.
        give_up(pod, events, "%s (%s)" % (error, type(error).__name__), "worker refused after its Job was acknowledged")


class NoJobAcknowledged(ValueError):
    """A retained launch none of whose receipts acknowledges a Job creation."""


def retained_intent(name, root):
    """The launch intent this controller saved before creating anything."""
    intent = load_record(root / (name + ".intent.json"))
    if intent.get("name") != name or not re.fullmatch(r"[0-9a-f]{40}", intent.get("head", "")) or not native_job_matches(intent.get("job", {})):
        raise ValueError("retained worker intent is incomplete or unsafe")
    expected = intent["job"]["spec"]["template"]["spec"]
    if intent["job"]["metadata"].get("name") != name or intent["job"]["metadata"].get("namespace") != "boss-dev" or expected["initContainers"][0]["command"][4] != intent["head"]:
        raise ValueError("retained launch identity or source is inconsistent")
    return intent


def acknowledged_launch(name, root):
    """The retained intent and the ONE Job UID its creation acknowledged."""
    intent = retained_intent(name, root)
    acknowledged = []
    for path in root.glob(name + ".launch-*.json"):
        receipt = load_record(path)
        if receipt.get("argv") != ["create", "-f", "-", "-o", "json"] or receipt.get("completion") != "observed" or type(receipt.get("exit")) is not int or receipt["exit"] != 0:
            continue
        body = json.loads(base64.b64decode(receipt.get("stdout_base64", ""), validate=True))
        if not isinstance(body, dict):
            raise ValueError("original creation is not a Kubernetes object; no recovery")
        if body.get("kind") == "Job" and body.get("metadata", {}).get("name") == name:
            if body["metadata"].get("namespace") != "boss-dev" or not body["metadata"].get("uid"):
                raise ValueError("original creation did not acknowledge the bounded Job identity")
            if not native_job_matches(body, intent["job"]):
                raise ValueError("original creation differs from retained intent; no recovery")
            acknowledged.append(body["metadata"]["uid"])
    if not acknowledged:
        raise NoJobAcknowledged("original Job creation is unknown; no recovery or retry")
    if len(acknowledged) != 1:
        raise ValueError("original Job creation is unknown or ambiguous; no recovery or retry")
    return intent, acknowledged[0]


def recover_start(name, root, local_root=None, transport=kube):
    """Recover an acknowledged launch; never create, replace or execute it."""
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", name):
        raise ValueError("recovery needs a controller-owned worker name")
    if retirement_requested(root, name) or (root / (name + ".json")).exists():
        raise ValueError("worker is already admitted or retirement was requested")
    if local_root is not None and (not local_root.is_absolute() or ".." in local_root.parts):
        raise ValueError("local source binding must be absolute without traversal")
    intent, job_uid = acknowledged_launch(name, root)
    expected = intent["job"]["spec"]["template"]["spec"]
    acknowledged = [job_uid]
    volume = load_record(root / (name + ".volume.json"))
    if volume.get("name") != name or not volume.get("uid"):
        raise ValueError("original private volume identity is unknown")
    record = lambda receipt: save_record(root / (name + ".recovery-" + uuid.uuid4().hex + ".json"), receipt)
    def read(argv):
        return kube_json(argv, transport=lambda command: transport(command, None), record=record)
    job = read(["get", "job", name])
    if job["metadata"].get("name") != name or job["metadata"].get("namespace") != "boss-dev" or job["metadata"].get("uid") != acknowledged[0] or not native_job_matches(job, intent["job"]):
        raise RuntimeError("acknowledged worker Job changed; no recovery")
    current_volume = read(["get", "pvc", name])
    if current_volume["metadata"].get("deletionTimestamp") is not None or current_volume["metadata"].get("name") != name or current_volume["metadata"].get("namespace") != "boss-dev" or current_volume["metadata"].get("uid") != volume["uid"] or not any(owner.get("kind") == "Job" and owner.get("name") == name and owner.get("uid") == acknowledged[0] for owner in current_volume["metadata"].get("ownerReferences", [])):
        raise RuntimeError("private volume identity or owner changed; no recovery")
    pods = read(["get", "pods", "-l", "job-name=" + name])["items"]
    if len(pods) != 1:
        raise RuntimeError("worker does not have one observed Pod; no recovery")
    pod = pods[0]
    if pod["metadata"].get("namespace") != "boss-dev" or not declared_matches(observed_pod_spec(pod["spec"]), expected):
        raise RuntimeError("worker Pod differs from retained intent; no recovery")
    workers = [status for status in pod.get("status", {}).get("containerStatuses", []) if status.get("name") == "worker"]
    if len(workers) != 1 or not workers[0].get("containerID") or not pod["metadata"].get("uid"):
        raise RuntimeError("worker incarnation is not observed; no recovery")
    session = Session(name, acknowledged[0], pod["metadata"]["name"], pod["metadata"]["uid"], workers[0]["containerID"], intent["head"], volume["uid"])
    error = session_error(session, pod)
    if error:
        raise RuntimeError(error)
    if local_root is not None and source_head(local_root, record) != intent["head"]:
        raise ValueError("local source differs from the retained launch; no recovery")
    save_record(root / (name + ".json"), asdict(session))
    if local_root is not None:
        save_record(root / (name + ".binding.json"), {"session": name, "local_root": str(local_root), "source_head": intent["head"]})
    return session


def lifecycle_module():
    """dev-lifecycle.py, published beside this controller."""
    path = Path(__file__).resolve().with_name("dev-lifecycle.py")
    spec = importlib.util.spec_from_file_location("dev_lifecycle", path)
    lifecycle = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lifecycle)
    return lifecycle


def lifecycle_retire(root):
    """The UID-conditioned Job retirement `stop` and a failed launch share."""
    def retire(session):
        lifecycle = lifecycle_module()
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
        return retire_worker(session, root, delete)
    return retire


def run_worker_path(root, run, suffix="json"):
    if str(uuid.UUID(run)) != run:
        raise ValueError("worker ownership requires the canonical run UUID")
    return root / ("run-worker-" + run + "." + suffix)


def ensure_run(run, head, local_root, root, launch_worker, inspect=None, head_at=None):
    """Bind one native worker to this run, adopting its unique prebound tree.

    The controller keeps the identity BEFORE allocation or the API's receipt:
    an interrupted handoff is retried against that identity, never a new volume.
    """
    record_path = run_worker_path(root, run)
    intent_path = run_worker_path(root, run, "intent.json")
    if not local_root.is_absolute() or ".." in local_root.parts or local_root.resolve(strict=True) != local_root:
        raise ValueError("run worker needs an existing canonical absolute local root")
    try:
        linked = stat.S_ISREG((local_root / ".git").lstat().st_mode)
    except FileNotFoundError:
        linked = False
    if not linked:
        raise ValueError("a run worker requires its own linked worktree, never the shared checkout")
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        raise ValueError("run worker needs an exact source head")
    record_read = lambda receipt: save_record(root / ("run-worker-" + run + ".source-" + uuid.uuid4().hex + ".json"), receipt)
    actual = head_at(local_root) if head_at else source_head(local_root, record_read)
    if actual != head:
        raise ValueError("the run worker source changed before admission")
    inspect = inspect or (lambda session: observe(session, root))
    tree_lock = hashlib.sha256(str(local_root).encode()).hexdigest()
    with controller_lock(run_worker_path(root, run, "lock")), controller_lock(root / ("worker-tree-" + tree_lock + ".lock")):
        remembered = load_record(record_path) if record_path.exists() else None
        pending = load_record(intent_path) if intent_path.exists() else None
        intent = remembered or pending
        if intent and (intent.get("run") != run or intent.get("local_root") != str(local_root)):
            raise ValueError("this run already owns a worker for another tree")
        bindings = [load_record(path) for path in root.glob("dev-build-*.binding.json")]
        live = [b for b in bindings if b.get("local_root") == str(local_root) and not retirement_requested(root, b["session"])]
        if len(live) > 1:
            raise ValueError("the run tree has multiple live worker bindings; no worker allocated")
        if intent:
            name = intent["session"]
            if retirement_requested(root, name):
                raise ValueError("this run's worker retirement was requested; no replacement allocated")
            if not live or live[0]["session"] != name:
                raise ValueError("worker admission %s is retained; use `dev-build recover-start %s --local-root %s` or stop this run; no new allocation" % (name, name, local_root))
        else:
            name = live[0]["session"] if live else "dev-build-" + uuid.uuid4().hex[:16]
            owner_path = root / (name + ".run-owner.json")
            if owner_path.exists() and load_record(owner_path).get("run") != run:
                raise ValueError("the tree's worker belongs to another run")
            pending = {"schema":1,"run":run,"session":name,"local_root":str(local_root),"source_head":head}
            save_record(intent_path, pending)
            if not live:
                launch_worker(name, head, local_root)
        binding = load_record(root / (name + ".binding.json"))
        session = Session(**load_record(root / (name + ".json")))
        if binding.get("local_root") != str(local_root):
            raise ValueError("the worker no longer names this run's tree")
        same_worker(binding, session, head)
        inspect(session)
        owner_path = root / (name + ".run-owner.json")
        owner = {"run":run,"session":name,"local_root":str(local_root)}
        if owner_path.exists():
            if load_record(owner_path) != owner:
                raise ValueError("the worker belongs to another run")
        else:
            save_record(owner_path, owner)
        observed = {**(remembered or pending), "native":asdict(session)}
        if remembered:
            if observed != remembered:
                raise ValueError("the run's native worker identity changed")
        else:
            save_record(record_path, observed)
        return observed


def owned_runs(root, after="", limit=32, retired_after=""):
    """Read a bounded, rotating page of controller-owned run handles (B3).

    No API state authorizes a name or UID: stop_run rechecks the private
    record under its lock. A cursor only schedules observations, never stops.
    """
    if after:
        run_worker_path(root, after)
    if retired_after:
        run_worker_path(root, retired_after)
    if not 1 <= limit <= 32:
        raise ValueError("owned-run inventory limit must be between 1 and 32")
    handles = set()
    for path in root.glob("run-worker-*.json"):
        match = re.fullmatch(r"run-worker-([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})(?:\.intent)?\.json", path.name)
        if match:
            run_worker_path(root, match[1])
            handles.add(match[1])
    ended = {run for run in handles if os.path.lexists(run_worker_path(root, run, "stopped.json"))}
    def rotate(values, cursor):
        return sorted(run for run in values if run > cursor) + sorted(run for run in values if run <= cursor)
    active = rotate(handles - ended, after)
    retired = rotate(ended, retired_after)
    # Reserve a catchup slot, so failures publishing a prior receipt do
    # not starve; prioritize new owners over years of stopped history.
    active = active[:max(1, limit - 1) if retired else limit]
    retired = retired[:limit - len(active)]
    ordered = active + retired
    rows = []
    def read_owned(run):
        with controller_lock(run_worker_path(root, run, "lock"), wait=False):
            path = run_worker_path(root, run)
            owned = load_record(path if path.exists() else run_worker_path(root, run, "intent.json"))
            if owned.get("run") != run or not re.fullmatch(r"dev-build-[0-9a-f]{16}", owned.get("session", "")):
                raise ValueError("owned-run inventory identity differs from its handle")
            if not re.fullmatch(r"[0-9a-f]{40}", owned.get("source_head", "")):
                raise ValueError("owned-run inventory source identity is invalid")
            if path.exists():
                session = Session(**owned["native"])
                if session.job != owned["session"] or session.source_head != owned["source_head"] or load_record(root / (session.job + ".json")) != owned["native"]:
                    raise ValueError("owned-run inventory native identity changed")
                owner = load_record(root / (session.job + ".run-owner.json"))
                if owner != {"run":run,"session":session.job,"local_root":owned["local_root"]}:
                    raise ValueError("owned-run inventory owner identity changed")
            ended = run_worker_path(root, run, "stopped.json")
            receipt = load_record(ended) if ended.exists() else None
            if receipt and receipt.get("run") != run:
                raise ValueError("owned-run retirement identity changed")
            return {"run":run,"session":owned["session"],"source_head":owned["source_head"],
                    "retirement":receipt}
    for run in ordered[:limit]:
        try:
            rows.append(read_owned(run))
        except (OSError, ValueError, KeyError, TypeError) as error:
            # One corrupt/unread record must neither acquire authority nor
            # starve later valid owners. The caller records this refusal.
            rows.append({"run":run,"error":str(error)})
    return {"runs":rows,"next":active[-1] if active else "",
            "retired_next":retired[-1] if retired else ""}


def stop_run(run, root, stop_worker):
    """Stop only the native worker the controller recorded for this run."""
    record_path = run_worker_path(root, run)
    ended_path = run_worker_path(root, run, "stopped.json")
    with controller_lock(run_worker_path(root, run, "lock")):
        if ended_path.exists():
            ended = load_record(ended_path)
            if ended.get("run") != run:
                raise ValueError("retired worker record names another run")
            return ended
        intent_path = run_worker_path(root, run, "intent.json")
        if not record_path.exists() and not intent_path.exists():
            return {"run":run,"state":"no-worker"}
        owned = load_record(record_path if record_path.exists() else intent_path)
        if owned.get("run") != run:
            raise ValueError("run worker record names another run")
        name = owned["session"]
        if record_path.exists():
            if load_record(root / (name + ".json")) != owned["native"]:
                raise ValueError("native run worker identity changed; nothing stopped")
        elif not (root / (name + ".intent.json")).exists():
            # launch publishes this intent before its FIRST native create.
            # A budget refusal precedes it and left no resource to retire.
            result = {"run":run,"session":name,"state":"not-admitted",
                      "note":"no native launch intent: allocation stopped before any create"}
            save_record(ended_path, result)
            return result
        # An unadmitted launch may still own a PVC. The existing stop door
        # handles both its acknowledged Job and a volume with no Job owner.
        # The attempt, not a later publication, supplies these facts.
        # Cached old receipts stay unchanged and cannot acquire a fresh
        # timestamp merely because the observer retries its API PATCH.
        preexisting = any(os.path.lexists(root / (name + suffix))
                          for suffix in (".retire-intent.json", ".retired.json"))
        requested = int(time.time())
        result = {"run":run,"session":name,"retirement_preexisting":preexisting,
                  "retirement_requested_at_epoch":requested,"retirement":stop_worker(name)}
        save_record(ended_path, result)
        return result


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="verb", required=True)
    sub.add_parser("renew-checkout")
    start = sub.add_parser("start")
    start.add_argument("head")
    start.add_argument("--local-root", type=Path)
    ensure = sub.add_parser("ensure-run")
    ensure.add_argument("run")
    ensure.add_argument("head")
    ensure.add_argument("--local-root", type=Path, required=True)
    stop_owned = sub.add_parser("stop-run")
    stop_owned.add_argument("run")
    inventory = sub.add_parser("owned-runs")
    inventory.add_argument("--after", default="")
    inventory.add_argument("--retired-after", default="")
    inventory.add_argument("--limit", type=int, default=32)
    recover = sub.add_parser("recover-start")
    recover.add_argument("session")
    recover.add_argument("--local-root", type=Path)
    run = sub.add_parser("exec")
    run.add_argument("--stdin", action="store_true", help="forward the caller's stdin to the worker")
    run.add_argument("session")
    run.add_argument("argv", nargs=argparse.REMAINDER)
    stop = sub.add_parser("stop")
    stop.add_argument("session")
    auto = sub.add_parser("exec-auto")
    auto.add_argument("--stdin", action="store_true", help="forward the caller's stdin to the worker")
    auto.add_argument("--worker-env", action="append", default=[])
    auto.add_argument("--notfound-channel", type=Path)
    auto.add_argument("argv", nargs=argparse.REMAINDER)
    for verb in ("put", "get"):
        transfer = sub.add_parser(verb)
        transfer.add_argument("session")
        transfer.add_argument("path")
    sub.choices["put"].add_argument("--allow-empty", action="store_true",
                                    help="write an empty file when stdin gives zero bytes (refused otherwise)")
    return parser


def parse_arguments(argv=None):
    parser = arguments()
    args = parser.parse_args(argv)
    # Backlog 056fc8b6: after the session, --stdin fell into the worker's
    # argv ('--stdin', '--', 'cat') and failed naming a missing program.
    if args.verb == "exec" and args.argv[:1] == ["--stdin"]:
        parser.error("--stdin goes before the session (exec --stdin <session> -- <argv>); "
                     "after it, it would be the worker's command")
    return args


def main(argv=None, root=Path("/work/home/.config/boss/workers")):
    args = parse_arguments(argv)
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.is_symlink() or root.stat().st_uid != os.getuid() or root.stat().st_mode & 0o077:
        raise ValueError("unsafe controller session directory")
    renewed = None
    if args.verb == "renew-checkout" or (args.verb == "exec-auto" and os.getcwd() == "/work/boss"):
        if os.getcwd() != "/work/boss":
            raise ValueError("renew-checkout is only for the exact shared checkout /work/boss")
        config = load_record(Path(__file__).resolve().parents[2] / "config.json")
        record = lambda receipt: save_record(root / ("checkout-observation-" + uuid.uuid4().hex + ".json"), receipt)
        renewed = checkout_worker(os.getcwd(), root, lambda argv: kube_json(argv, record=record),
            lifecycle_retire(root),
            lambda name, head, local: launch(head, config, root, local, retire=lifecycle_retire(root), name=name, print_session=False),
            lambda name, local: recover_start(name, root, local), lambda local: source_head(local, record),
            release_failed=lambda name: stop(name, root, lifecycle_retire(root)), require_renewal=args.verb == "renew-checkout")
        if args.verb == "renew-checkout":
            print(json.dumps(renewed))
            return 0
    if args.verb == "owned-runs":
        print(json.dumps(owned_runs(root, args.after, args.limit, args.retired_after)))
        return 0
    if args.verb == "ensure-run":
        config = load_record(Path(__file__).resolve().parents[2] / "config.json")
        def allocate(name, head, local):
            return launch(head, config, root, local, retire=lifecycle_retire(root), name=name, print_session=False)
        print(json.dumps(ensure_run(args.run, args.head, args.local_root, root, allocate)))
        return 0
    if args.verb == "stop-run":
        print(json.dumps(stop_run(args.run, root, lambda name: stop(name, root, lifecycle_retire(root)))))
        return 0
    if args.verb == "start":
        config = load_record(Path(__file__).resolve().parents[2] / "config.json")
        local_root = args.local_root
        if local_root is not None and (not local_root.is_absolute() or ".." in local_root.parts):
            raise ValueError("local source binding must be an absolute path without traversal")
        launch(args.head, config, root, local_root, retire=lifecycle_retire(root))
        return 0
    if args.verb == "recover-start":
        print(json.dumps(asdict(recover_start(args.session, root, args.local_root))))
        return 0
    if args.verb == "stop":
        argv = []
    elif args.verb in ("put", "get"):
        argv = source_file_argv(args.verb, args.path)
        if args.verb == "put":
            argv = source_file_argv(args.verb, args.path, hand_put_mode(root, args.session, args.path))
    else:
        argv = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
    if args.verb == "exec-auto":
        cwd = os.getcwd()
        bindings = [load_record(path) for path in root.glob("dev-build-*.binding.json")]
        binding = renewed or nearest_binding(bindings, cwd, lambda b: retirement_requested(root, b["session"]))
        args.session = binding["session"]
        reader = argv
        argv = bound_argv(binding, cwd, argv)
        argv = probe_argv(argv, args.worker_env)
    if not re.fullmatch(r"dev-build-[0-9a-f]{16}", args.session):
        raise ValueError("unknown controller session name")
    if args.verb in ("put", "exec"):
        hand_door_binding(root, args.session, os.getcwd())
    if args.verb == "stop":
        print(json.dumps(stop(args.session, root, lifecycle_retire(root))))
        return 0
    session = Session(**load_record(root / (args.session + ".json")))
    if retirement_requested(root, session.job):
        raise ValueError("worker retirement is already requested; no execution or local fallback")
    if args.verb == "exec-auto":
        local_root = Path(binding["local_root"])
        git_record = lambda receipt: save_record(root / (args.session + ".source-" + uuid.uuid4().hex + ".json"), receipt)
        current_head = source_head(local_root, git_record)
        same_worker(binding, session, current_head)
        # A commit since the last exec-auto is shipped before this one runs
        # (ceea10b8); a head that cannot be followed refuses, naming stop.
        # The held head is read under the session's lock (b831d89e B1).
        held, synced = follow(session, binding, current_head, root,
                              lambda git_args, stdin=None: local_git(local_root, git_args, git_record, stdin=stdin),
                              lambda ship_argv, stdin: bound_exec(binding, session, ship_argv, stdin, kube, observe,
                                  lambda receipt: save_record(root / (args.session + ".exec-" + uuid.uuid4().hex + ".json"), receipt)))
        if synced != held:
            print("dev-build: the worker followed this worktree from %s to %s through the put door; per-path receipts in %s"
                  % (held, synced, root / ("%s.sync-%s.json" % (args.session, held))), file=sys.stderr)
        check_binding(binding, session, current_head, synced)
        handed = edit_level_hand_in(reader, cwd, binding["local_root"], args.worker_env, lambda: door_read(EDIT_LEVEL_PATH))
        if handed:
            print("dev-build: the instance's edit level was read through the door and is handed to the worker as BOSS_EDIT_LEVEL_ANSWER",
                  file=sys.stderr)
            argv = ["/usr/bin/env", "--", *handed, *argv]
    # stdin is plain worker input; returned bytes never become a host
    # command or a successful execution envelope.
    channel = args.notfound_channel if args.verb == "exec-auto" else None
    fd = probe_channel(channel) if channel is not None else None
    remote_channel = "/workspace/tmp/notfound-" + uuid.uuid4().hex
    if fd is not None:
        argv = ["/usr/bin/env", "--", "BOSS_PROBE_NOTFOUND=" + remote_channel, *argv]
    record = lambda receipt: save_record(root / (args.session + ".exec-" + uuid.uuid4().hex + ".json"), receipt)
    try:
        result = bound_exec(
            binding if args.verb == "exec-auto" else None, session, argv, caller_input(args.verb, getattr(args, "stdin", False), sys.stdin, getattr(args, "allow_empty", False)),
            kube, observe, record)
        if fd is not None:
            telemetry = bound_exec(binding, session, telemetry_argv(remote_channel), b"", kube, observe, record)
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


def cli():
    try:
        return main()
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
        print("dev-build: REFUSED — " + str(error), file=sys.stderr)
        return 75


if __name__ == "__main__":
    sys.exit(cli())
