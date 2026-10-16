#!/usr/bin/python3 -I
"""a21 R1: one original request, fixed synthetic intents and audited effects.

The kernel takes explicit native and artifact ports. There is no default
success adapter. The forge adapter below must conserve every invocation;
this module grants neither a Kubernetes right nor isolation activation.
"""
import base64
import contextlib
import copy
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import tarfile
import uuid

SCHEMA = 1
NAMESPACE = "boss-dev"
PURPOSE = "isolation-fixture"
HELPER = "/opt/boss/infra/forge/isolation-receiver.py"
ROUTES = ("/api/jobs", "/api/v1/namespaces/synthetic/secrets/fake",
          "/api/v1/namespaces/synthetic/pods/fake/exec", "/apis/batch/v1/namespaces/synthetic/jobs")
MAX_BYTES = 64 * 1024 * 1024
DESIGN = "a21c491c-6a72-4698-804d-65cc38e68f9c"
ISOLATION = "b3071bf4-5996-4cf8-96db-e4b23b2f67a1"
JOURNAL_ROOT = "/var/lib/boss/isolation-fixture"


class Unknown(ValueError):
    """The record does not prove this transition; never retry a launch."""

    def __init__(self, reason):
        super().__init__(reason)
        # Only explicit domain messages cross the human-facing boundary.
        # Native stderr remains whole in protected originals, never rendered
        # from a parser/transport exception that might carry caller data.
        self.safe_cause = reason


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def identifier(value):
    if not isinstance(value, str) or str(uuid.UUID(value)) != value:
        raise ValueError("exact native UUID required")
    return value


def hex_string(value, count):
    if not isinstance(value, str) or not re.fullmatch("[0-9a-f]{" + str(count) + "}", value):
        raise ValueError("exact reviewed digest required")
    return value


def make_plan(request, runtime, nonce, head, helper, image, lifetime, now):
    identifier(request)
    identifier(runtime)
    hex_string(nonce, 64)
    hex_string(head, 40)
    hex_string(helper, 64)
    if not isinstance(image, str) or not re.fullmatch(r"[A-Za-z0-9.:/_-]+@sha256:[0-9a-f]{64}", image):
        raise ValueError("published fixture image digest required")
    if type(lifetime) is not int or not 60 <= lifetime <= 600 or type(now) is not int or now < 0:
        raise ValueError("fixture lifetime must be 60..600 seconds")
    return {"schema": SCHEMA, "request": request, "runtime": runtime, "nonce": nonce,
            "head": head, "helper_sha256": helper, "image": image, "lifetime": lifetime,
            "at": now, "deadline": now + lifetime - 5}


def unique_step(packet, slug):
    matches = [step for step in packet.get("steps", []) if step.get("spec_slug") == slug]
    if len(matches) != 1:
        raise Unknown("native step missing or ambiguous")
    return matches[0]


def authorize(plan, request, runtime, decision, isolation, published, open_runs):
    """Original native packets and published bytes, never caller JSON proof.

The forge adapter resolves these records itself and conserves their complete
responses. The worker receipt proves admission only, not native isolation.
"""
    try:
        if any(packet.get("partition") != "real" or packet.get("simulated") is not False
               for packet in (request, runtime, decision, isolation)):
            raise Unknown("fixture native records cross a partition")
        expected_args = [plan["runtime"], plan["nonce"], plan["head"], plan["helper_sha256"],
                         plan["image"].rsplit("@sha256:", 1)[1], str(plan["lifetime"])]
        execute = unique_step(request, "execute")
        if (request.get("id") != plan["request"] or request.get("kind") != "ops-request"
                or request.get("status") != "open" or request.get("metadata", {}).get("host") != "forge"
                or request["metadata"].get("verb") != "prepare-isolation-fixture"
                or request["metadata"].get("args") != expected_args
                or execute.get("status") not in ("ready", "active")
                or execute.get("metadata", {}).get("credential_executor") != "runner:ops"):
            raise Unknown("request is not the original forge fixture operation")
        building = unique_step(runtime, "building")
        worker = runtime.get("metadata", {}).get("worker_started")
        if (runtime.get("id") != plan["runtime"] or runtime.get("kind") != "agent-run"
                or runtime.get("status") != "open" or runtime.get("metadata", {}).get("packet") != ISOLATION
                or runtime["metadata"].get("profile") != "builder" or building.get("status") != "active"
                or not building.get("assignee_id") or worker != {"schema": 1, "run": plan["runtime"], "actor": building["assignee_id"]}):
            raise Unknown("separate registered runtime executor not admitted")
        review = unique_step(decision, "review")
        questions = [q for q in review.get("metadata", {}).get("questions", []) if q.get("anchor") == "R1"]
        resolutions = [q for q in review.get("metadata", {}).get("resolutions", []) if q.get("anchor") == "R1"]
        if (decision.get("id") != DESIGN or decision.get("kind") != "design-doc"
                or review.get("status") != "completed" or review.get("completed_by") != "emp-david"
                or len(questions) != 1 or len(resolutions) != 1 or not questions[0].get("proposal")
                or resolutions[0].get("decision") != questions[0]["proposal"]):
            raise Unknown("stored R1 approval unavailable")
        if (isolation.get("id") != ISOLATION or isolation.get("kind") != "backlog-item"
                or isolation.get("metadata", {}).get("bounded_isolation_rollout_authorization_20261013", {}).get("user_statement") != "Okay, let's do it"):
            raise Unknown("original bounded rollout authorization unavailable")
        if published != {"head": plan["head"], "helper_sha256": plan["helper_sha256"], "image": plan["image"]}:
            raise Unknown("reviewed helper/image publication differs")
        if len([run for run in open_runs if run.get("id") == plan["runtime"]]) != 1:
            raise Unknown("complete native run roster disagrees with known runtime")
        for run in open_runs:
            if run.get("id") != plan["runtime"] and any(
                    step.get("spec_slug") == "building" and step.get("status") in ("ready", "active")
                    for step in run.get("steps", [])):
                raise Unknown("another registered executor is not quiescent")
    except (KeyError, TypeError, ValueError) as error:
        raise Unknown("native fixture admission incomplete") from error


def intents(plan):
    # No caller-owned manifest, resource kind, namespace, path, argv or port.
    name = "isolation-fixture-" + identifier(plan["request"])
    labels = {"boss.dev/purpose": PURPOSE, "boss.dev/request": plan["request"]}
    annotations = {"boss.dev/intent-sha256": hashlib.sha256(encoded(plan)).hexdigest(),
                   "boss.dev/runtime": plan["runtime"], "boss.dev/helper-sha256": plan["helper_sha256"]}
    metadata = {"name": name, "namespace": NAMESPACE, "labels": labels, "annotations": annotations}
    security = {"runAsNonRoot": True, "runAsUser": 65534, "runAsGroup": 1500,
                "allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
                "capabilities": {"drop": ["ALL"]}, "seccompProfile": {"type": "RuntimeDefault"}}
    container = {"name": "receiver", "image": plan["image"], "imagePullPolicy": "IfNotPresent",
                 "command": ["/usr/bin/python3", "-I", HELPER, "serve", plan["request"], plan["nonce"], str(plan["deadline"])],
                 "securityContext": security,
                 "env": [{"name": "BOSS_FIXTURE_POD_UID", "valueFrom": {"fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.uid"}}},
                         {"name": "PYTHONDONTWRITEBYTECODE", "value": "1"}],
                 "ports": [{"containerPort": 8099, "protocol": "TCP"}],
                 "readinessProbe": {"httpGet": {"path": "/healthz", "port": 8099}, "periodSeconds": 1},
                 "resources": {"requests": {"cpu": "50m", "memory": "64Mi"}, "limits": {"cpu": "1", "memory": "128Mi"}},
                 "volumeMounts": [{"name": "fixture", "mountPath": "/fixture"}]}
    pod = {"restartPolicy": "Never", "serviceAccountName": "default", "automountServiceAccountToken": False, "enableServiceLinks": False,
           "hostNetwork": False, "hostPID": False, "hostIPC": False, "shareProcessNamespace": False,
           "securityContext": {"fsGroup": 1500, "fsGroupChangePolicy": "OnRootMismatch"},
           "containers": [container], "volumes": [{"name": "fixture", "emptyDir": {"medium": "Memory", "sizeLimit": "16Mi"}}]}
    job = {"apiVersion": "batch/v1", "kind": "Job", "metadata": metadata,
           "spec": {"backoffLimit": 0, "activeDeadlineSeconds": plan["lifetime"],
                    "template": {"metadata": {"labels": labels, "annotations": annotations}, "spec": pod}}}
    service = {"apiVersion": "v1", "kind": "Service", "metadata": metadata,
               "spec": {"type": "ClusterIP", "selector": labels,
                        "ports": [{"port": 8099, "targetPort": 8099, "protocol": "TCP"}]}}
    return copy.deepcopy(job), copy.deepcopy(service)


def includes(actual, expected):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(k in actual and includes(actual[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(includes(a, b) for a, b in zip(actual, expected))
    return type(actual) is type(expected) and actual == expected


def native_uid(value):
    return identifier(value["metadata"]["uid"])


def recognized_original(state, actual):
    """The original protected journal owns UIDs; labels never confer origin.

    Compare the entire conserved spec, not a namespace/name prefix or an
    annotation supplied by the object being judged. Missing context stays
    UNKNOWN at the reader, while a proven different object stays undeclared.
    """
    try:
        kind = actual["kind"]
        if kind not in ("Job", "Service") or state["state"] in ("retired", "refused"):
            return False
        original = state[kind.lower()]
        expected = intents(state["plan"])[0 if kind == "Job" else 1]
        if native_uid(actual) != state["binding"][kind.lower() + "_uid"] or native_uid(actual) != native_uid(original):
            return False
        if actual.get("apiVersion") != expected["apiVersion"] or actual.get("spec") != original.get("spec"):
            return False
        def metadata(value):
            return {key: item for key, item in value["metadata"].items()
                    if key not in ("resourceVersion", "managedFields", "generation", "creationTimestamp")}
        if metadata(actual) != metadata(original):
            return False
        return includes(original, expected)
    except (KeyError, ValueError, TypeError, IndexError):
        return False


def checked_objects(plan, job, service, pod):
    import ipaddress
    expected_job, expected_service = intents(plan)
    if not includes(job, expected_job) or not includes(service, expected_service):
        raise Unknown("native fixture intent changed")
    job_uid, service_uid = native_uid(job), native_uid(service)
    spec = pod["spec"]
    expected = expected_job["spec"]["template"]
    # Kubernetes omits false non-pointer defaults. Normalize only these.
    normalized = copy.deepcopy(spec)
    for key in ("hostNetwork", "hostPID", "hostIPC", "shareProcessNamespace"):
        normalized.setdefault(key, False)
    if not includes(normalized, expected["spec"]) or not includes(pod["metadata"], expected["metadata"]):
        raise Unknown("receiver Pod intent changed")
    if any(spec.get(key) for key in ("initContainers", "ephemeralContainers")):
        raise Unknown("receiver has another execution container")
    if spec.get("volumes") != expected["spec"]["volumes"]:
        raise Unknown("receiver has an undeclared mount")
    container = spec["containers"][0]
    if container.get("securityContext") != expected["spec"]["containers"][0]["securityContext"]:
        raise Unknown("receiver authority changed")
    if container.get("env") != expected["spec"]["containers"][0]["env"]:
        raise Unknown("receiver environment changed")
    if container.get("volumeMounts") != expected["spec"]["containers"][0]["volumeMounts"]:
        raise Unknown("receiver mount changed")
    if service["spec"].get("selector") != expected_service["spec"]["selector"] or any(
            service["spec"].get(key) for key in ("externalIPs", "externalName", "loadBalancerIP", "loadBalancerClass")):
        raise Unknown("fixture Service reaches another target or outside the cluster")
    owners = [x for x in pod["metadata"].get("ownerReferences", []) if x.get("controller") is True]
    if len(owners) != 1 or owners[0].get("kind") != "Job" or owners[0].get("uid") != job_uid:
        raise Unknown("receiver belongs to another Job incarnation")
    statuses = pod.get("status", {}).get("containerStatuses", [])
    if len(statuses) != 1:
        raise Unknown("receiver container identity unavailable")
    state = statuses[0]
    if (pod["status"].get("phase") != "Running" or state.get("name") != "receiver"
            or state.get("ready") is not True or state.get("restartCount") != 0
            or "running" not in state.get("state", {}) or not state.get("containerID")
            or state.get("imageID", "").removeprefix("docker-pullable://") != plan["image"]):
        raise Unknown("receiver is not the original ready image/container")
    pod_ip, service_ip = pod["status"]["podIP"], service["spec"]["clusterIP"]
    ipaddress.ip_address(pod_ip)
    ipaddress.ip_address(service_ip)
    return {"job_uid": job_uid, "service_uid": service_uid, "pod_uid": native_uid(pod),
            "pod": pod["metadata"]["name"], "container_id": state["containerID"],
            "pod_ip": pod_ip, "service_ip": service_ip,
            "service_dns": expected_service["metadata"]["name"] + ".boss-dev.svc.cluster.local", "port": 8099}


class JournalStore:
    """Append-only full records; no mutable intent pointer to guess from."""
    def __init__(self, root, readonly=False):
        self.root = Path(root)
        self.readonly = readonly
        if not readonly:
            self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if not self.root.exists() or self.root.is_symlink() or self.root.stat().st_uid != os.getuid() or self.root.stat().st_mode & 0o022:
            raise Unknown("fixture journal root is not owned private data")

    @contextlib.contextmanager
    def locked(self):
        if self.readonly:
            raise Unknown("read-only inventory cannot mutate fixture journal")
        fd = os.open(self.root / ".lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "rb") as target:
            fcntl.flock(target, fcntl.LOCK_EX)
            yield

    def path(self, request):
        return self.root / identifier(request)

    def read(self, request):
        directory = self.path(request)
        if directory.is_symlink():
            raise Unknown("fixture journal incarnation changed")
        path = directory / "journal.jsonl"
        if not path.exists():
            if directory.exists():
                raise Unknown("incomplete fixture admission remains unresolved")
            return None
        root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            directory_fd = os.open(identifier(request), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root_fd)
            try:
                info = os.fstat(directory_fd)
                if info.st_uid != os.getuid() or info.st_mode & 0o022:
                    raise Unknown("fixture request directory is not protected")
                fd = os.open("journal.jsonl", os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            os.close(root_fd)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or info.st_size > MAX_BYTES:
                raise Unknown("fixture journal is not owned bounded regular data")
            raw = source.read(MAX_BYTES + 1)
        if not raw or not raw.endswith(b"\n"):
            raise Unknown("fixture journal is incomplete")
        previous, result = "", None
        for index, line in enumerate(raw.splitlines()):
            entry = json.loads(line)
            digest = entry.pop("sha256")
            if entry["seq"] != index or entry["previous"] != previous or hashlib.sha256(encoded(entry)).hexdigest() != digest:
                raise Unknown("fixture journal conservation failed")
            previous, result = digest, entry["data"]
        return result

    def append(self, request, event, data):
        if self.readonly:
            raise Unknown("read-only inventory cannot mutate fixture journal")
        directory = self.path(request)
        directory.mkdir(mode=0o700, exist_ok=True)
        path = directory / "journal.jsonl"
        raw = path.read_bytes() if path.exists() else b""
        previous = json.loads(raw.splitlines()[-1])["sha256"] if raw else ""
        entry = {"seq": len(raw.splitlines()), "previous": previous, "event": event, "data": data}
        entry["sha256"] = hashlib.sha256(encoded(entry)).hexdigest()
        line = encoded(entry) + b"\n"
        if len(raw) + len(line) > MAX_BYTES:
            raise Unknown("fixture journal exceeds its bound; no evidence discarded")
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "ab") as target:
            target.write(line)
            target.flush()
            os.fsync(target.fileno())
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def unresolved(self):
        result = []
        for directory in self.root.iterdir():
            if directory.name == ".lock":
                continue
            if not directory.is_dir() or directory.is_symlink():
                raise Unknown("unrecognized fixture journal entry")
            value = self.read(directory.name)
            if value["state"] not in ("retired", "refused"):
                result.append(directory.name)
        return result

    def bundle(self, request):
        output = io.BytesIO()
        total = 0
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for path in sorted(self.path(request).rglob("*")):
                if path.is_symlink() or not path.is_file():
                    raise Unknown("fixture original is not regular data")
                raw = path.read_bytes()
                total += len(raw)
                if total > MAX_BYTES:
                    raise Unknown("fixture originals exceed bounded archive")
                item = tarfile.TarInfo(str(path.relative_to(self.path(request))))
                item.size, item.mode = len(raw), 0o600
                archive.addfile(item, io.BytesIO(raw))
        return output.getvalue()


class Engine:
    def __init__(self, store, native, artifacts, clock):
        self.store, self.native, self.artifacts, self.clock = store, native, artifacts, clock

    def save(self, state, event):
        self.store.append(state["plan"]["request"], event, copy.deepcopy(state))

    def fail(self, state, error):
        # A failed read/right check before any create intent proves no
        # native write was attempted. A sent create always stays UNKNOWN.
        terminal = "refused" if state["state"] == "admitted" and not state["binding"] else "unknown"
        state = {**state, "state": terminal, "cause": getattr(error, "safe_cause", type(error).__name__)}
        self.save(state, "unproven")
        raise Unknown("fixture transition unproven: " + state["cause"] + "; original intent/UIDs conserved") from None

    def live(self, state):
        plan = state["plan"]
        if self.clock() >= plan["deadline"]:
            raise Unknown("fixture lifetime expired")
        name = intents(plan)[0]["metadata"]["name"]
        job = self.native.get("Job", name)
        service = self.native.get("Service", name)
        pod = self.native.pod(state["binding"]["job_uid"])
        binding = checked_objects(plan, job, service, pod)
        if binding != state["binding"]:
            raise Unknown("fixture original native incarnation changed")
        return binding

    def prepare(self, plan):
        with self.store.locked():
            old = self.store.read(plan["request"])
            if old is not None:
                if old["plan"] != plan or old["state"] != "ready":
                    raise Unknown("fixture intent already exists; no replacement launch")
                try:
                    self.live(old)
                    return copy.deepcopy(old["receipt"])
                except (OSError, ValueError, KeyError, TypeError, Unknown) as error:
                    return self.fail(old, error)
            if self.store.unresolved():
                raise Unknown("another fixture remains unresolved")
            state = {"plan": copy.deepcopy(plan), "state": "admitted", "binding": {}}
            self.save(state, "admitted")
            try:
                before = self.native.prerequisites()
                state["before"] = before
                if before != {"known_connection": True, "rights": True, "quiescent": True}:
                    raise Unknown("existing native authority/readiness not positively observed")
                if self.clock() >= plan["deadline"]:
                    raise Unknown("fixture expired before provision")
                job, service = intents(plan)
                for body in (job, service):
                    if self.native.get(body["kind"], body["metadata"]["name"]) is not None:
                        raise Unknown("foreign or pre-existing resource; never adopted")
                for body in (job, service):
                    kind, name = body["kind"], body["metadata"]["name"]
                    state["state"] = "creating-" + kind.lower()
                    state["intent"] = body
                    self.save(state, "create-intent")
                    observed = self.native.create(body)
                    if not includes(observed, body):
                        raise Unknown("created resource differs from full intent")
                    state["binding"][kind.lower() + "_uid"] = native_uid(observed)
                    state[kind.lower()] = observed
                    self.save(state, "created")
                pod = self.native.pod(state["binding"]["job_uid"])
                state["pod"] = pod
                state["binding"] = checked_objects(plan, state["job"], state["service"], pod)
                controls = self.native.controls(state["binding"])
                expected = {"schema": 1, "request": plan["request"], "nonce": plan["nonce"],
                            "pod_uid": state["binding"]["pod_uid"], "helper_sha256": plan["helper_sha256"], "complete": True,
                            "effects": {route: 1 for route in ROUTES}, "replay_effects": {route: 1 for route in ROUTES},
                            "unsigned": {route: 403 for route in ROUTES}, "forged": {route: 403 for route in ROUTES}}
                if controls != expected:
                    raise Unknown("known positive/refusal/replay controls unproved")
                state["controls"] = controls
                # A Service response is not proof of an effect in this
                # original receiver. Conserve its actual counter journal
                # before any worker negative may rely on fixture readiness.
                self.snapshot(state)
                state["state"] = "ready"
                state["receipt"] = {"schema": 1, "state": "ready", "request": plan["request"],
                                    "runtime": plan["runtime"], "binding": state["binding"], "controls": controls,
                                    "observed_at": self.clock(), "deadline": plan["deadline"]}
                self.save(state, "ready")
                return copy.deepcopy(state["receipt"])
            except (OSError, ValueError, KeyError, TypeError, Unknown) as error:
                return self.fail(state, error)

    def snapshot(self, state):
        binding = self.live(state)
        snapshot = self.native.collect(binding)
        plan = state["plan"]
        if (snapshot.get("schema") != 1 or snapshot.get("request") != plan["request"]
                or snapshot.get("nonce") != plan["nonce"] or snapshot.get("pod_uid") != binding["pod_uid"]
                or snapshot.get("helper_sha256") != plan["helper_sha256"] or snapshot.get("complete") is not True
                or not snapshot.get("entries") or any(snapshot.get("effects", {}).get(route, 0) < 1 for route in ROUTES)):
            raise Unknown("complete receiver originals unavailable")
        self.live(state)
        state["snapshot"] = snapshot
        self.save(state, "receiver-originals")
        return snapshot

    def observe(self, request):
        with self.store.locked():
            state = self.store.read(request)
            if state is None or state["state"] != "ready":
                raise Unknown("fixture is not the original ready operation")
            try:
                snapshot = self.snapshot(state)
                self.publish(state)
                return {**copy.deepcopy(state["receipt"]), "artifact": copy.deepcopy(state["artifact"]),
                        "effects": snapshot["effects"]}
            except (OSError, ValueError, KeyError, TypeError, Unknown) as error:
                return self.fail(state, error)

    def publish(self, state):
        raw = self.store.bundle(state["plan"]["request"])
        receipt = self.artifacts.publish(state["plan"]["request"], raw)
        if not receipt.get("file_id") or receipt.get("readback") != raw:
            raise Unknown("complete fixture artifact readback missing")
        state["artifact"] = {"file_id": identifier(receipt["file_id"]), "bytes": len(raw),
                             "sha256": hashlib.sha256(raw).hexdigest()}
        self.save(state, "artifact-conserved")

    def retire(self, request):
        with self.store.locked():
            state = self.store.read(request)
            if state is None:
                raise Unknown("original fixture intent missing")
            if state["state"] == "retired":
                return copy.deepcopy(state["receipt"])
            if state["state"] != "ready":
                raise Unknown("unresolved fixture cannot borrow an incarnation")
            try:
                self.snapshot(state)
                self.publish(state)
                self.live(state)
                name = intents(state["plan"])[0]["metadata"]["name"]
                for kind in ("Service", "Job"):
                    uid = state["binding"][kind.lower() + "_uid"]
                    actual = self.native.get(kind, name)
                    if actual is None or native_uid(actual) != uid:
                        raise Unknown("retirement target incarnation changed")
                    state["state"] = "retiring-" + kind.lower()
                    state["delete_intent"] = {"kind": kind, "name": name, "uid": uid}
                    self.save(state, "delete-intent")
                    response = self.native.delete(kind, name, uid)
                    state["delete_response"] = response
                    self.save(state, "delete-acknowledged")
                    if self.native.get(kind, name) is not None:
                        raise Unknown("delete acknowledgment is not observed absence")
                    self.save(state, "absence-observed")
                self.publish(state)
                state["state"] = "retired"
                state["receipt"] = {"schema": 1, "state": "retired", "request": request,
                                    "binding": state["binding"], "artifact": state["artifact"], "observed_at": self.clock()}
                self.save(state, "retired")
                return copy.deepcopy(state["receipt"])
            except (OSError, ValueError, KeyError, TypeError, Unknown) as error:
                return self.fail(state, error)
