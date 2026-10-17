"""a21 R1 owning controls: no cluster, credential, provider or real SoR."""
import copy
import hashlib
import http.client
import importlib.util
import io
import json
import re
from pathlib import Path
import socket
import subprocess
import sys
import threading
import tarfile
import unittest
import uuid

SOURCE = Path(sys.argv.pop(1))
SCRATCH = Path(sys.argv.pop(1))


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SOURCE / "infra/forge" / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


fixture = load("isolation-fixture")
receiver = load("isolation-receiver")
declaration = load("isolation-fixture-declaration")
REQUEST = "11111111-1111-4111-8111-111111111111"
RUNTIME = "22222222-2222-4222-8222-222222222222"
NONCE = "a" * 64
HEAD = "b" * 40
HELPER = "c" * 64
DIGEST = "d" * 64


def plan():
    return fixture.make_plan(REQUEST, RUNTIME, NONCE, HEAD, HELPER,
                             "fixture.invalid/david/boss@sha256:" + DIGEST, 300, 1000)


class NativeMemory:
    """An explicit native port with real UID/spec/absence semantics."""
    def __init__(self, original):
        self.plan = original
        self.objects = {}
        self.creates = []
        self.deletes = []
        self.lost_ack = None
        self.refuse_read = False
        self.ack_only = False
        self.rights = True
        self.quiet = True
        self.bad_controls = None
        self.pod_change = None

    def prerequisites(self):
        return {"known_connection": True, "rights": self.rights, "quiescent": self.quiet}

    def get(self, kind, name):
        if self.refuse_read:
            raise OSError("synthetic refused native read")
        return copy.deepcopy(self.objects.get(kind))

    def create(self, body):
        kind = body["kind"]
        self.creates.append(kind)
        if kind in self.objects:
            raise OSError("AlreadyExists")
        obj = copy.deepcopy(body)
        obj["metadata"]["uid"] = str(uuid.uuid4())
        obj["metadata"]["resourceVersion"] = "1"
        if kind == "Service":
            obj["spec"]["clusterIP"] = "192.0.2.20"
            obj["spec"]["clusterIPs"] = ["192.0.2.20"]
        self.objects[kind] = obj
        if self.lost_ack == kind:
            raise OSError("created, response lost")
        return copy.deepcopy(obj)

    def pod(self, job_uid):
        pod = copy.deepcopy(self.objects["Job"]["spec"]["template"])
        pod.update(apiVersion="v1", kind="Pod")
        pod["metadata"].update(name="fixture-pod", namespace="boss-dev", uid="33333333-3333-4333-8333-333333333333",
                               ownerReferences=[{"kind": "Job", "uid": job_uid, "controller": True}])
        pod["status"] = {"phase": "Running", "podIP": "192.0.2.21", "containerStatuses": [
            {"name": "receiver", "containerID": "containerd://original", "imageID": self.plan["image"],
             "ready": True, "restartCount": 0, "state": {"running": {"startedAt": "1970-01-01T00:16:40Z"}}}]}
        if self.pod_change:
            self.pod_change(pod)
        return pod

    def controls(self, binding):
        result = {"schema": 1, "request": REQUEST, "nonce": NONCE, "pod_uid": binding["pod_uid"],
                  "helper_sha256": HELPER, "complete": True,
                  "effects": {route: 1 for route in receiver.ROUTES},
                  "unsigned": {route: 403 for route in receiver.ROUTES},
                  "forged": {route: 403 for route in receiver.ROUTES},
                  "replay_effects": {route: 1 for route in receiver.ROUTES}}
        if self.bad_controls:
            self.bad_controls(result)
        return result

    def collect(self, binding):
        return {"schema": 1, "request": REQUEST, "nonce": NONCE, "pod_uid": binding["pod_uid"],
                "helper_sha256": HELPER, "complete": True, "entries": [{"synthetic": True}],
                "effects": {route: 1 for route in receiver.ROUTES}}

    def delete(self, kind, name, uid):
        self.deletes.append((kind, uid))
        actual = self.objects.get(kind)
        if actual is None or actual["metadata"]["uid"] != uid:
            raise OSError("UID precondition refused")
        if not self.ack_only:
            del self.objects[kind]
        return {"status": "Success"}


class ArtifactsMemory:
    def __init__(self):
        self.ack_only = False
        self.originals = []

    def publish(self, request, raw):
        self.originals.append(bytes(raw))
        return {"file_id": "44444444-4444-4444-8444-444444444444",
                "readback": None if self.ack_only else bytes(raw)}


class FixtureControls(unittest.TestCase):
    def engine(self):
        original = plan()
        self.directory = SCRATCH / str(uuid.uuid4())
        self.store = fixture.JournalStore(self.directory)
        self.native = NativeMemory(original)
        self.artifacts = ArtifactsMemory()
        return fixture.Engine(self.store, self.native, self.artifacts, lambda: 1001), original

    def test_effect_patterns_match_only_the_completed_readback_receipts(self):
        engine, original = self.engine()
        prepared = engine.prepare(original)
        retired = engine.retire(REQUEST)
        for verb, receipt in (("prepare", prepared), ("retire", retired)):
            row = json.loads((SOURCE / "infra/ops/verbs" / (verb + "-isolation-fixture.json")).read_bytes())
            pattern = row.get("effect")
            self.assertIsInstance(pattern, str)
            self.assertEqual(receipt["state"], "ready" if verb == "prepare" else "retired")
            script = (SOURCE / "infra/forge/isolation-fixture.sh").read_text()
            formatter = "fixture_effect() {" + script.split("fixture_effect() {", 1)[1].split("\n}", 1)[0] + "\n}\n"
            invocation = 'fixture_effect "$1" "$2" "$3"'
            output = subprocess.run(["bash", "-c", formatter + invocation, "fixture-control", verb, REQUEST, REQUEST],
                                    capture_output=True, check=True, text=True).stdout.rstrip("\n")
            self.assertRegex(output, pattern)
            self.assertNotRegex(output + " FAILED", pattern)
            # A shell failure from the native engine stops before the formatter.
            refused = subprocess.run(["bash", "-ec", formatter + "false\n" + invocation,
                                      "fixture-control", verb, REQUEST, REQUEST], capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(refused.stdout, "")
            for state in ("intent", "unknown", "retiring-job", "refused"):
                self.assertIsNone(re.search(pattern, json.dumps({**receipt, "state": state}, sort_keys=True)))
            self.assertIsNone(re.search(pattern, "created; deletion acknowledged"))

    def test_compressed_attachment_keeps_complete_original_bytes(self):
        engine, original = self.engine()
        engine.prepare(original)
        raw = bytes(range(256)) * 4096
        (self.store.path(REQUEST) / "full-native.stdout").write_bytes(raw)
        packed = self.store.bundle(REQUEST)
        self.assertEqual(packed[:2], b"\x1f\x8b")
        with tarfile.open(fileobj=io.BytesIO(packed), mode="r:gz") as archive:
            self.assertEqual(archive.extractfile("full-native.stdout").read(), raw)
            self.assertEqual(archive.extractfile("journal.jsonl").read(),
                             (self.store.path(REQUEST) / "journal.jsonl").read_bytes())

    def test_observation_conserves_full_snapshot_before_bounded_summary(self):
        engine, original = self.engine()
        engine.prepare(original)
        receipt = engine.observe(REQUEST)
        self.assertTrue(self.artifacts.originals)
        self.assertIn("artifact", receipt)
        self.assertNotIn("snapshot", receipt)
        with tarfile.open(fileobj=io.BytesIO(self.artifacts.originals[-1]), mode="r:gz") as archive:
            journal = archive.extractfile("journal.jsonl").read()
            self.assertIn(b'"receiver-originals"', journal)
            self.assertIn(b'"entries":[{"synthetic":true}]', journal)

    def test_inventory_requires_original_journal_uid_and_full_shape(self):
        engine, original = self.engine()
        engine.prepare(original)
        state = self.store.read(REQUEST)
        for kind in ("Job", "Service"):
            actual = copy.deepcopy(self.native.objects[kind])
            self.assertTrue(fixture.recognized_original(state, actual))
            actual["metadata"]["uid"] = str(uuid.uuid4())
            self.assertFalse(fixture.recognized_original(state, actual))
            actual = copy.deepcopy(self.native.objects[kind])
            if kind == "Job":
                actual["spec"]["template"]["spec"]["volumes"].append(
                    {"name": "forged", "secret": {"secretName": "copied-label-is-not-authority"}})
            else:
                actual["spec"]["selector"]["foreign"] = "target"
            self.assertFalse(fixture.recognized_original(state, actual))
        with self.assertRaises(fixture.Unknown):
            fixture.JournalStore(self.directory / "absent", readonly=True)
        self.assertFalse((self.directory / "absent").exists())
        reader = fixture.JournalStore(self.directory, readonly=True)
        self.assertEqual(reader.read(REQUEST), state)
        self.assertEqual(declaration.names({"items": [self.native.objects["Job"]]}, reader),
                         [self.native.objects["Job"]["metadata"]["name"]])
        forged = copy.deepcopy(self.native.objects["Job"])
        forged["metadata"]["uid"] = str(uuid.uuid4())
        self.assertEqual(declaration.names({"items": [forged]}, reader), [])
        forged["metadata"]["labels"]["boss.dev/request"] = str(uuid.uuid4())
        with self.assertRaises(declaration.kernel.Unknown):
            declaration.names({"items": [forged]}, reader)

    def test_fixed_plan_has_no_credential_or_native_resource_steering(self):
        original = plan()
        job, service = fixture.intents(original)
        self.assertEqual(job["metadata"]["namespace"], "boss-dev")
        self.assertEqual(service["spec"]["type"], "ClusterIP")
        pod = job["spec"]["template"]["spec"]
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertEqual(len(pod["containers"]), 1)
        self.assertEqual(pod["volumes"], [{"name": "fixture", "emptyDir": {"medium": "Memory", "sizeLimit": "16Mi"}}])
        self.assertEqual(pod["containers"][0]["securityContext"]["capabilities"]["drop"], ["ALL"])
        for field, bad in [("request", "../x"), ("runtime", "not-a-run"), ("nonce", "x"),
                           ("head", "main"), ("helper", "x"), ("image", "other:latest"),
                           ("lifetime", 0), ("lifetime", True), ("lifetime", 601)]:
            args = [REQUEST, RUNTIME, NONCE, HEAD, HELPER, original["image"], 300, 1000]
            args[["request", "runtime", "nonce", "head", "helper", "image", "lifetime", "at"].index(field)] = bad
            with self.subTest(field=field, bad=bad), self.assertRaises(ValueError):
                fixture.make_plan(*args)

    def test_real_forge_port_refusal_records_a_terminal_refusal_across_modules(self):
        port = load("isolation-fixture-port")
        engine, original = self.engine()
        def refused():
            raise port.kernel.Unknown("existing forge right positively refused")
        self.native.prerequisites = refused
        with self.assertRaises(fixture.Unknown):
            engine.prepare(original)
        state = self.store.read(REQUEST)
        self.assertEqual(state["state"], "refused")
        self.assertEqual(state["cause"], "existing forge right positively refused")
        self.assertEqual(self.native.creates, [])

    def test_original_runtime_decision_and_published_inputs_are_all_required(self):
        original = plan()
        runtime = {"id": RUNTIME, "kind": "agent-run", "status": "open", "partition": "real", "simulated": False,
                   "metadata": {"packet": "b3071bf4-5996-4cf8-96db-e4b23b2f67a1", "profile": "builder",
                                "worker_started": {"schema": 1, "run": RUNTIME, "actor": "agent-codex"}},
                   "steps": [{"spec_slug": "building", "status": "active", "assignee_id": "agent-codex"}]}
        request = {"id": REQUEST, "kind": "ops-request", "status": "open", "partition": "real", "simulated": False,
                   "metadata": {"host": "forge", "verb": "prepare-isolation-fixture", "args": [RUNTIME, NONCE, HEAD, HELPER, DIGEST, "300"]},
                   "steps": [{"spec_slug": "execute", "status": "ready", "metadata": {"credential_executor": "runner:ops"}}]}
        decision = {"id": "a21c491c-6a72-4698-804d-65cc38e68f9c", "kind": "design-doc", "partition": "real", "simulated": False,
                    "steps": [{"spec_slug": "review", "status": "completed", "completed_by": "emp-david",
                               "metadata": {"questions": [{"anchor": "R1", "proposal": "exact bounded synthetic purpose"}],
                                            "resolutions": [{"anchor": "R1", "decision": "exact bounded synthetic purpose"}]}}]}
        isolation = {"id": "b3071bf4-5996-4cf8-96db-e4b23b2f67a1", "kind": "backlog-item", "partition": "real", "simulated": False,
                     "metadata": {"bounded_isolation_rollout_authorization_20261013": {"user_statement": "Okay, let's do it"}}}
        published = {"head": HEAD, "helper_sha256": HELPER, "image": original["image"]}
        values = [original, request, runtime, decision, isolation, published, [runtime]]
        fixture.authorize(*copy.deepcopy(values))
        mutations = [
            lambda v: v[2]["metadata"].update(packet="2710c8fc-94e6-47d1-bee0-b4ec81706d0a"),
            lambda v: v[2]["metadata"]["worker_started"].update(run=REQUEST),
            lambda v: v[2]["steps"][0].update(status="ready"),
            lambda v: v[3]["steps"][0].update(completed_by="agent-codex"),
            lambda v: v[3]["steps"][0]["metadata"]["resolutions"][0].update(decision="declined"),
            lambda v: v[4]["metadata"].clear(),
            lambda v: v[5].update(helper_sha256="e" * 64),
            lambda v: v[5].update(image="other.invalid/boss@sha256:" + DIGEST),
            lambda v: v[1]["metadata"].update(args=[REQUEST, NONCE, HEAD, HELPER, DIGEST, "300"]),
            lambda v: v[1].update(partition="simulation"),
            lambda v: v[6].append({**copy.deepcopy(v[2]), "id": REQUEST}),
        ]
        for change in mutations:
            changed = copy.deepcopy(values)
            change(changed)
            with self.assertRaises(fixture.Unknown):
                fixture.authorize(*changed)

    def test_normal_and_lost_reply_replay_conserve_original_objects(self):
        engine, original = self.engine()
        first = engine.prepare(original)
        self.assertEqual(first["state"], "ready")
        self.assertEqual(engine.prepare(original), first)
        self.assertEqual(self.native.creates, ["Job", "Service"])
        changed = copy.deepcopy(original)
        changed["nonce"] = "e" * 64
        with self.assertRaises(fixture.Unknown):
            engine.prepare(changed)
        self.assertEqual(self.native.creates, ["Job", "Service"])

    def test_lost_create_ack_never_recreates_or_borrows_uid(self):
        for kind in ["Job", "Service"]:
            engine, original = self.engine()
            self.native.lost_ack = kind
            with self.subTest(kind=kind), self.assertRaises(fixture.Unknown):
                engine.prepare(original)
            count = list(self.native.creates)
            with self.assertRaises(fixture.Unknown):
                engine.prepare(original)
            self.assertEqual(self.native.creates, count)
            self.assertEqual(self.native.deletes, [])

    def test_foreign_resources_and_missing_rights_or_quiescence_refuse(self):
        for mode in ["foreign", "foreign-service", "rights", "quiescence", "read"]:
            engine, original = self.engine()
            if mode == "foreign":
                self.native.objects["Job"] = {"metadata": {"uid": "foreign"}}
            elif mode == "foreign-service":
                self.native.objects["Service"] = {"metadata": {"uid": "foreign"}}
            elif mode == "rights":
                self.native.rights = False
            elif mode == "quiescence":
                self.native.quiet = False
            else:
                self.native.refuse_read = True
            with self.subTest(mode=mode), self.assertRaises(fixture.Unknown):
                engine.prepare(original)
            self.assertEqual(self.native.creates, [])
            self.assertEqual(self.native.deletes, [])

    def test_changed_uid_refuses_observe_replay_and_teardown(self):
        for kind in ["Job", "Service"]:
            engine, original = self.engine()
            engine.prepare(original)
            self.native.objects[kind]["metadata"]["uid"] = "replacement"
            for call in [lambda: engine.observe(REQUEST), lambda: engine.prepare(original), lambda: engine.retire(REQUEST)]:
                with self.subTest(kind=kind), self.assertRaises(fixture.Unknown):
                    call()
            self.assertEqual(self.native.creates, ["Job", "Service"])
            self.assertEqual(self.native.deletes, [])

    def test_missing_positive_truncated_or_rebound_control_is_not_ready(self):
        for change in [lambda r: r.update(complete=False), lambda r: r.update(pod_uid="replacement"),
                       lambda r: r["effects"].update({receiver.ROUTES[0]: 0}),
                       lambda r: r["unsigned"].update({receiver.ROUTES[0]: 201}),
                       lambda r: r["replay_effects"].update({receiver.ROUTES[0]: 2})]:
            engine, original = self.engine()
            self.native.bad_controls = change
            with self.assertRaises(fixture.Unknown):
                engine.prepare(original)

    def test_service_response_without_effect_in_original_receiver_is_not_ready(self):
        engine, original = self.engine()
        genuine_collect = self.native.collect
        def no_effect(binding):
            return {**genuine_collect(binding), "effects": {route: 0 for route in receiver.ROUTES}}
        self.native.collect = no_effect
        with self.assertRaises(fixture.Unknown):
            engine.prepare(original)
        self.assertEqual(self.store.read(REQUEST)["state"], "unknown")
        self.assertEqual(self.native.creates, ["Job", "Service"])
        self.assertEqual(self.native.deletes, [])

    def test_native_defaulting_cannot_introduce_another_service_account(self):
        engine, original = self.engine()
        self.native.pod_change = lambda pod: pod["spec"].update(serviceAccountName="other-existing-account")
        with self.assertRaises(fixture.Unknown):
            engine.prepare(original)
        self.assertEqual(self.native.deletes, [])

    def test_second_unresolved_request_and_expiry_never_free_capacity(self):
        engine, original = self.engine()
        engine.prepare(original)
        other = {**original, "request": "55555555-5555-4555-8555-555555555555"}
        with self.assertRaises(fixture.Unknown):
            engine.prepare(other)
        engine.clock = lambda: 1301
        with self.assertRaises(fixture.Unknown):
            engine.observe(REQUEST)
        with self.assertRaises(fixture.Unknown):
            engine.prepare(other)
        self.assertEqual(self.native.creates, ["Job", "Service"])

    def test_teardown_requires_complete_artifact_readback_and_observed_absence(self):
        for mode in ["normal", "artifact-ack", "delete-ack"]:
            engine, original = self.engine()
            first = engine.prepare(original)
            self.artifacts.ack_only = mode == "artifact-ack"
            self.native.ack_only = mode == "delete-ack"
            if mode == "normal":
                result = engine.retire(REQUEST)
                self.assertEqual(result["state"], "retired")
                self.assertEqual(len(self.native.deletes), 2)
                self.assertEqual(self.native.deletes[0], ("Service", first["binding"]["service_uid"]))
                self.assertEqual(self.native.deletes[1], ("Job", first["binding"]["job_uid"]))
                self.assertEqual(self.native.objects, {})
                self.assertEqual(engine.retire(REQUEST), result)
            else:
                with self.subTest(mode=mode), self.assertRaises(fixture.Unknown):
                    engine.retire(REQUEST)
                self.assertLess(len(self.native.deletes), 2)
            self.assertTrue(self.artifacts.originals)


class WiringControls(unittest.TestCase):
    def test_kernel_and_receiver_share_the_same_fixed_effect_families(self):
        self.assertEqual(tuple(fixture.ROUTES), tuple(receiver.ROUTES))


class ForgePortControls(unittest.TestCase):
    def test_http_originals_require_actual_200_not_a_refused_redirect(self):
        module = load("isolation-fixture-host")
        status = [200]
        class Handler(receiver.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(status[0])
                self.send_header("Content-Length", "15")
                self.end_headers()
                self.wfile.write(b"original-packet")
            def log_message(self, *args):
                pass
        server = receiver.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            root = SCRATCH / str(uuid.uuid4())
            recorder = module.Recorder(root, REQUEST)
            header = recorder.directory / "synthetic-header"
            header.write_text("x-synthetic-fixture: source-only\n")
            host = module.Host(SOURCE, recorder, [], [], "@" + str(header), "http://127.0.0.1:" + str(server.server_port))
            self.assertEqual(host.get("/api/jobs/" + REQUEST), b"original-packet")
            status[0] = 302
            with self.assertRaises(Exception):
                host.get("/api/jobs/" + REQUEST)
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_receiver_publication_is_inherited_by_final_runtime_and_archive(self):
        roster = (SOURCE / "infra/dev/control-files.txt").read_text().splitlines()
        helper = "infra/forge/isolation-receiver.py"
        self.assertEqual(roster.count(helper), 1)
        stage = None
        carried = set()
        bases = {}
        for line in (SOURCE / "infra/oss-quickstart/Dockerfile").read_text().splitlines():
            words = line.split()
            if words and words[0].upper() == "FROM":
                stage = words[-1]
                bases[stage] = words[1]
            if words == ["COPY", helper, "/opt/boss/" + helper]:
                carried.add(stage)
        stage = "final"
        while stage not in carried and stage in bases:
            stage = bases[stage]
        self.assertIn(stage, carried)

    def test_native_roster_uses_actual_complete_data_envelope(self):
        module = load("isolation-fixture-host")
        host = module.Host(SOURCE, None, [], [], "@synthetic", "http://synthetic.invalid")
        envelope = {"data": [{"id": RUNTIME}], "full": True, "limit": 100, "offset": 0, "total": 1}
        host.get = lambda path: json.dumps(envelope).encode()
        self.assertEqual(host.active_runs(), [{"id": RUNTIME}])
        for field, value in (("full", False), ("total", 2)):
            broken = {**envelope, field: value}
            host.get = lambda path: json.dumps(broken).encode()
            with self.assertRaises(Exception):
                host.active_runs()

    def test_failed_process_keeps_whole_streams_without_becoming_absence(self):
        host = load("isolation-fixture-host")
        root = SCRATCH / str(uuid.uuid4())
        recorder = host.Recorder(root, REQUEST)
        with self.assertRaises(Exception):
            recorder.run([sys.executable, "-c", "import sys; print('original-out'); print('original-error',file=sys.stderr); sys.exit(7)"])
        self.assertEqual((recorder.directory / "000000.stdout").read_bytes(), b"original-out\n")
        self.assertEqual((recorder.directory / "000000.stderr").read_bytes(), b"original-error\n")
        self.assertEqual(json.loads((recorder.directory / "000000.result.json").read_bytes())["exit"], 7)

    def test_native_absence_and_uid_preconditions_use_fixed_targets(self):
        port = load("isolation-fixture-port")
        calls = []
        def run(argv, body=None):
            calls.append((argv, body))
            return b""
        native = port.NativePort(run, plan(), lambda: None)
        name = fixture.intents(plan())[0]["metadata"]["name"]
        self.assertIsNone(native.get("Job", name))
        self.assertEqual(calls[-1][0], ["get", "Job", name, "-n", "boss-dev", "--ignore-not-found=true", "-o", "json", "--request-timeout=10s"])
        def refuse(argv, body=None):
            raise fixture.Unknown("actual API refusal")
        native.run = refuse
        with self.assertRaises(Exception):
            native.get("Job", name)
        native.run = run
        uid = "33333333-3333-4333-8333-333333333333"
        native.delete("Service", name, uid)
        self.assertEqual(calls[-1][0], ["delete", "--raw", "/api/v1/namespaces/boss-dev/services/" + name, "-f", "-"])
        self.assertEqual(json.loads(calls[-1][1])["preconditions"], {"uid": uid})
        for kind, target in (("Secret", name), ("Job", "foreign")):
            with self.assertRaises(Exception):
                native.get(kind, target)

class ReceiverControls(unittest.TestCase):
    def test_received_malformed_values_are_whole_refusals_without_effects(self):
        for attempt, proof in ((str(uuid.uuid4()), "é"), (17, "0" * 64)):
            with self.subTest(attempt=attempt, proof=proof):
                root = SCRATCH / str(uuid.uuid4())
                server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
                thread = threading.Thread(target=server.serve_forever)
                thread.start()
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=3)
                try:
                    controls = receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
                    body = receiver.encoded({"nonce": NONCE, "marker": "synthetic-only", "attempt": attempt})
                    connection.request("POST", receiver.ROUTES[0], body=body,
                                       headers={"x-boss-synthetic-proof": proof})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 403)
                    self.assertFalse(json.loads(response.read())["effect"])
                    originals = receiver.collect(root, REQUEST, NONCE, "pod-fixture")
                    self.assertEqual(len(originals["entries"]), 17)
                    self.assertEqual(originals["effects"], controls["effects"])
                    self.assertTrue(originals["entries"][-1]["request_complete"])
                finally:
                    connection.close()
                    server.shutdown()
                    thread.join()
                    server.server_close()

    def test_short_http_body_invalidates_separate_collection(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            controls = receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
            with socket.create_connection(("127.0.0.1", server.server_port)) as stream:
                stream.sendall(b"POST " + receiver.ROUTES[0].encode() + b" HTTP/1.1\r\nHost: synthetic\r\nContent-Length: 12\r\n\r\n{}")
                stream.shutdown(socket.SHUT_WR)
                while stream.recv(4096):
                    pass
            self.assertEqual(server.snapshot()["effects"], controls["effects"])
            with self.assertRaises(ValueError):
                receiver.collect(root, REQUEST, NONCE, "pod-fixture")
            self.assertFalse(server.snapshot()["entries"][-1]["request_complete"])
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_unexpected_handler_failure_invalidates_an_earlier_complete_prefix(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=3)
        try:
            controls = receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
            def fail(*args):
                raise RuntimeError("synthetic internal failure; must not print raw exception")
            server.answer = fail
            connection.request("POST", receiver.ROUTES[0], body=b"{}")
            with self.assertRaises(http.client.RemoteDisconnected):
                connection.getresponse()
            self.assertEqual(server.snapshot()["effects"], controls["effects"])
            with self.assertRaises(ValueError):
                receiver.collect(root, REQUEST, NONCE, "pod-fixture")
        finally:
            connection.close()
            server.shutdown()
            thread.join()
            server.server_close()

    def test_body_read_timeout_invalidates_the_original_prefix(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            controls = receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
            with socket.create_connection(("127.0.0.1", server.server_port), timeout=7) as stream:
                stream.sendall(b"POST /api/jobs HTTP/1.1\r\nHost: synthetic\r\nContent-Length: 12\r\n\r\n{}")
                self.assertEqual(stream.recv(4096), b"")
            self.assertEqual(server.snapshot()["effects"], controls["effects"])
            with self.assertRaises(ValueError):
                receiver.collect(root, REQUEST, NONCE, "pod-fixture")
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_http_parser_refusal_invalidates_the_independent_journal(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            server.answer("POST", receiver.ROUTES[0], [], b"{}")
            with socket.create_connection(("127.0.0.1", server.server_port)) as stream:
                stream.sendall(b"GET /" + b"x" * 65536 + b" HTTP/1.1\r\nHost: synthetic\r\n\r\n")
                response = b""
                while chunk := stream.recv(4096):
                    response += chunk
            self.assertIn(b" 414 ", response.split(b"\r\n", 1)[0])
            with self.assertRaises(ValueError):
                receiver.collect(root, REQUEST, NONCE, "pod-fixture")
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_native_positive_target_is_only_the_original_service(self):
        self.assertEqual(receiver.service_host(REQUEST),
                         "isolation-fixture-" + REQUEST + ".boss-dev.svc.cluster.local")
        for value in ("localhost", "../" + REQUEST, REQUEST.upper(), REQUEST + ".foreign"):
            if value == REQUEST:
                continue
            with self.assertRaises(ValueError):
                receiver.service_host(value)

    def test_late_exhaustion_remains_unknown_to_independent_collection(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        try:
            server.answer("POST", receiver.ROUTES[0], [], b"{}")
            self.assertTrue(receiver.collect(root, REQUEST, NONCE, "pod-fixture")["complete"])
            server.journal_bytes = receiver.MAX_JOURNAL - 1
            status, _ = server.answer("POST", receiver.ROUTES[0], [], b"{}")
            self.assertEqual(status, 507)
            with self.assertRaises(ValueError):
                receiver.collect(root, REQUEST, NONCE, "pod-fixture")
        finally:
            server.server_close()

    def test_journal_capacity_refusal_precedes_a_synthetic_effect(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            server.journal_bytes = receiver.MAX_JOURNAL - 1
            with self.assertRaises(ValueError):
                receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
            self.assertTrue(all(n == 0 for n in server.snapshot()["effects"].values()))
            self.assertFalse(server.snapshot()["complete"])
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_real_http_only_generated_synthetic_authority_changes_bounded_effects(self):
        root = SCRATCH / str(uuid.uuid4())
        server = receiver.Server(("127.0.0.1", 0), root, REQUEST, NONCE, 9999999999, "pod-fixture")
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            for route in receiver.ROUTES:
                body = json.dumps({"nonce": NONCE, "marker": "synthetic-only", "attempt": str(uuid.uuid4())}).encode()
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=3)
                connection.request("POST", route, body=body, headers={"x-boss-user": '{"id":"emp-david","role":"platform-admin"}',
                                                                         "x-boss-machine-token": "SYNTHETIC-FORGED"})
                response = connection.getresponse()
                self.assertEqual(response.status, 403)
                response.read()
                connection.close()
            self.assertTrue(all(n == 0 for n in server.snapshot()["effects"].values()))
            controls = receiver.controls(server.server_port, root, REQUEST, NONCE, "pod-fixture")
            self.assertTrue(controls["complete"])
            self.assertTrue(all(n == 1 for n in controls["effects"].values()))
            self.assertEqual(controls["effects"], controls["replay_effects"])
            journal = server.snapshot()
            self.assertGreater(len(journal["entries"]), 0)
            self.assertNotIn((root / "authority").read_text().strip(), json.dumps(journal))
            with self.assertRaises(ValueError):
                receiver.controls(server.server_port, root, REQUEST, NONCE, "replacement-pod")
            self.assertEqual(hashlib.sha256((SOURCE / "infra/forge/isolation-receiver.py").read_bytes()).hexdigest(), controls["helper_sha256"])
        finally:
            server.shutdown()
            thread.join()
            server.server_close()


if __name__ == "__main__":
    result = unittest.main(exit=False)
    if not result.result.wasSuccessful():
        raise SystemExit(1)
    print("fixture source controls: ok")
