#!/usr/bin/python3 -I
"""Fixed synthetic effects only; no real API, credential or outbound target.

a21c491c R1. The random synthetic authority stays in this Pod's emptyDir.
One-attempt signatures can be conserved in full without exporting that
authority. Replaying an accepted attempt never creates another effect.
"""
import base64
import hashlib
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import threading
import time
import uuid

ROUTES = (
    "/api/jobs",
    "/api/v1/namespaces/synthetic/secrets/fake",
    "/api/v1/namespaces/synthetic/pods/fake/exec",
    "/apis/batch/v1/namespaces/synthetic/jobs",
)
ROOT = Path("/fixture")
PORT = 8099
MAX_REQUESTS = 128
MAX_JOURNAL = 2 * 1024 * 1024


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def helper_hash():
    return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()


def signature(authority, path, body):
    return hmac.new(authority, b"POST\0" + path.encode() + b"\0" + body, hashlib.sha256).hexdigest()


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, root, request, nonce, deadline, pod_uid):
        uuid.UUID(request)
        if len(nonce) != 64 or any(c not in "0123456789abcdef" for c in nonce) or not pod_uid:
            raise ValueError("synthetic fixture binding malformed")
        self.root = Path(root)
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.binding = {"schema": 1, "request": request, "nonce": nonce,
                        "pod_uid": pod_uid, "helper_sha256": helper_hash()}
        binding = self.root / "binding.json"
        authority = self.root / "authority"
        if binding.exists() or authority.exists():
            raise ValueError("receiver incarnation already initialized; no authority replacement")
        with binding.open("xb") as target:
            target.write(encoded(self.binding))
        fd = os.open(authority, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w") as target:
            target.write(secrets.token_hex(32))
            target.flush()
            os.fsync(target.fileno())
        self.deadline = deadline
        self.guard = threading.Lock()
        self.entries = []
        self.effects = {route: 0 for route in ROUTES}
        self.attempts = {}
        self.complete = True
        self.journal_bytes = 0
        super().__init__(address, Handler)

    def get_request(self):
        connection, peer = super().get_request()
        connection.settimeout(5)
        return connection, peer

    def handle_error(self, request, client_address):
        # An exception before/after a response can leave only an earlier
        # prefix on disk. Collection in another process must refuse it.
        # Never let the default handler print a raw exception or payload.
        with self.guard:
            self.mark_incomplete()

    def snapshot(self):
        with self.guard:
            return {**self.binding, "complete": self.complete,
                    "effects": dict(self.effects), "entries": list(self.entries)}

    def mark_incomplete(self):
        # Collection runs in another process. An in-memory flag would forget a
        # refused request and incorrectly qualify the earlier journal prefix.
        self.complete = False
        with (self.root / "incomplete").open("ab") as target:
            target.write(b"receiver evidence incomplete\n")
            target.flush()
            os.fsync(target.fileno())
        descriptor = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def answer(self, method, path, headers, body, readable=True):
        with self.guard:
            # Reserve enough for the complete request and bounded response
            # BEFORE an effect. Exhaustion cannot create an unrecorded fact.
            reserve = len(encoded(headers)) + 2 * len(body) + len(method) + len(path) + 4096
            if not self.complete or self.journal_bytes + reserve > MAX_JOURNAL:
                self.mark_incomplete()
                return 507, encoded({"synthetic": True, "effect": False, "complete": False})
            result = {"synthetic": True, "request": self.binding["request"], "effect": False}
            status = 403
            if not readable or len(self.entries) >= MAX_REQUESTS or time.time() >= self.deadline:
                self.mark_incomplete()
                status = 413 if not readable else 409
            elif method == "POST" and path in ROUTES:
                try:
                    value = json.loads(body)
                    valid_body = (isinstance(value, dict) and set(value) == {"nonce", "marker", "attempt"}
                                  and value["nonce"] == self.binding["nonce"]
                                  and value["marker"] == "synthetic-only"
                                  and isinstance(value["attempt"], str)
                                  and str(uuid.UUID(value["attempt"])) == value["attempt"])
                except (ValueError, TypeError, KeyError):
                    valid_body = False
                proofs = [v for k, v in headers if k.lower() == "x-boss-synthetic-proof"]
                authority = (self.root / "authority").read_text().encode()
                valid_proof = (len(proofs) == 1 and len(proofs[0]) == 64
                               and all(c in "0123456789abcdef" for c in proofs[0]))
                if valid_body and valid_proof and hmac.compare_digest(proofs[0], signature(authority, path, body)):
                    attempt = value["attempt"]
                    fingerprint = hashlib.sha256(path.encode() + b"\0" + body).hexdigest()
                    old = self.attempts.get(attempt)
                    if old is not None:
                        if old["fingerprint"] == fingerprint:
                            result = {**old["response"], "effect": False, "replayed": True}
                            status = 200
                        else:
                            status = 409
                    else:
                        self.effects[path] += 1
                        result = {**result, "attempt": attempt, "operation": path,
                                  "effect": True, "replayed": False, "count": self.effects[path]}
                        self.attempts[attempt] = {"fingerprint": fingerprint, "response": result}
                        status = 201
            response = encoded(result)
            entry = {"at": time.time(), "method": method, "path": path, "headers": headers,
                     "body_base64": base64.b64encode(body).decode(), "request_complete": readable,
                     "status": status, "response_base64": base64.b64encode(response).decode()}
            raw = encoded(entry) + b"\n"
            if self.journal_bytes + len(raw) > MAX_JOURNAL:
                self.mark_incomplete()
                return 507, encoded({"synthetic": True, "effect": False, "complete": False})
            with (self.root / "journal.jsonl").open("ab") as target:
                target.write(raw)
                target.flush()
                os.fsync(target.fileno())
            self.journal_bytes += len(raw)
            self.entries.append(entry)
            return status, response


class Handler(BaseHTTPRequestHandler):
    def send_error(self, code, message=None, explain=None):
        # HTTP parsing may refuse before do_POST can conserve the original
        # (for example 414/431). Never qualify an earlier journal prefix.
        with self.server.guard:
            self.server.mark_incomplete()
        super().send_error(code, message, explain)

    def log_message(self, *args):
        pass

    def log_error(self, *args):
        # BaseHTTPRequestHandler catches read TimeoutError itself and calls
        # this hook, so Server.handle_error never sees an unfinished body.
        # Keep the durable refusal, without rendering exception/payload text.
        with self.server.guard:
            self.server.mark_incomplete()

    def do_GET(self):
        if self.path == "/healthz":
            self.reply(200, encoded({**self.server.binding, "synthetic": True}))
        else:
            self.reply(404, encoded({"synthetic": True, "effect": False}))

    def do_POST(self):
        lengths = self.headers.get_all("Content-Length", [])
        length = lengths[0] if len(lengths) == 1 else ""
        readable = (length.isascii() and length.isdecimal() and len(length) <= 4
                    and int(length) <= 4096 and "Transfer-Encoding" not in self.headers)
        body = self.rfile.read(int(length)) if readable else b""
        readable = readable and len(body) == int(length)
        status, raw = self.server.answer("POST", self.path, list(self.headers.items()), body, readable)
        self.reply(status, raw)

    def reply(self, status, raw):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def bound(root, request, nonce, pod_uid):
    value = json.loads((root / "binding.json").read_bytes())
    if value != {"schema": 1, "request": request, "nonce": nonce,
                 "pod_uid": pod_uid, "helper_sha256": helper_hash()}:
        raise ValueError("receiver original binding changed")
    return value


def service_host(request):
    if str(uuid.UUID(request)) != request:
        raise ValueError("original request UUID required")
    return "isolation-fixture-" + request + ".boss-dev.svc.cluster.local"


def controls(port, root, request, nonce, pod_uid, host="127.0.0.1"):
    root = Path(root)
    binding = bound(root, request, nonce, pod_uid)
    authority = (root / "authority").read_text().encode()
    unsigned, forged = {}, {}
    def send(path, body, headers):
        connection = http.client.HTTPConnection(host, port, timeout=5)
        try:
            connection.request("POST", path, body=body, headers=headers)
            response = connection.getresponse()
            raw = response.read(65537)
            if len(raw) > 65536:
                raise ValueError("synthetic response exceeds its bound")
            return response.status, json.loads(raw)
        finally:
            connection.close()
    effects, replay = {}, {}
    for route in ROUTES:
        body = encoded({"nonce": nonce, "marker": "synthetic-only", "attempt": str(uuid.uuid4())})
        unsigned[route] = send(route, body, {})[0]
        forged[route] = send(route, body, {"x-boss-user": '{"id":"emp-david","role":"platform-admin"}',
                                          "x-boss-machine-token": "SYNTHETIC-FORGED"})[0]
        headers = {"x-boss-synthetic-proof": signature(authority, route, body)}
        status, answer = send(route, body, headers)
        if status != 201 or answer.get("effect") is not True:
            raise ValueError("known synthetic positive effect missing")
        effects[route] = answer["count"]
        status, answer = send(route, body, headers)
        if status != 200 or answer.get("effect") is not False or answer.get("replayed") is not True:
            raise ValueError("synthetic replay changed the effect")
        replay[route] = answer["count"]
    return {**binding, "complete": True, "effects": effects, "replay_effects": replay,
            "unsigned": unsigned, "forged": forged}


def collect(root, request, nonce, pod_uid):
    binding = bound(root, request, nonce, pod_uid)
    if (root / "incomplete").exists():
        raise ValueError("receiver evidence incomplete")
    raw = (root / "journal.jsonl").read_bytes()
    if len(raw) > MAX_JOURNAL or (raw and not raw.endswith(b"\n")):
        raise ValueError("receiver journal incomplete")
    entries = [json.loads(line) for line in raw.splitlines()]
    if len(entries) > MAX_REQUESTS or any(not e.get("request_complete") for e in entries):
        raise ValueError("receiver request originals incomplete")
    effects = {route: 0 for route in ROUTES}
    for entry in entries:
        answer = json.loads(base64.b64decode(entry["response_base64"], validate=True))
        if answer.get("effect") is True:
            effects[answer["operation"]] += 1
    if (root / "incomplete").exists():
        raise ValueError("receiver evidence incomplete")
    return {**binding, "complete": True, "entries": entries, "effects": effects}


def main(argv):
    if len(argv) != 4 or argv[0] not in ("serve", "controls", "collect"):
        raise ValueError("fixed receiver command required")
    action, request, nonce, last = argv
    if action == "serve":
        pod_uid = os.environ.get("BOSS_FIXTURE_POD_UID", "")
        deadline = int(last)
        server = Server(("0.0.0.0", PORT), ROOT, request, nonce, deadline, pod_uid)
        def expire():
            while time.time() < deadline:
                time.sleep(min(0.2, max(0, deadline - time.time())))
            server.shutdown()
        threading.Thread(target=expire, daemon=True).start()
        try:
            server.serve_forever(poll_interval=0.1)
        finally:
            server.server_close()
            print(json.dumps(collect(ROOT, request, nonce, pod_uid), sort_keys=True))
    else:
        pod_uid = os.environ.get("BOSS_FIXTURE_POD_UID", "")
        if last != pod_uid:
            raise ValueError("receiver Pod incarnation changed")
        value = controls(PORT, ROOT, request, nonce, pod_uid, host=service_host(request)) if action == "controls" else collect(ROOT, request, nonce, pod_uid)
        print(json.dumps(value, sort_keys=True))


if __name__ == "__main__":
    import sys
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, KeyError, TypeError, http.client.HTTPException) as error:
        print("synthetic receiver REFUSED: " + type(error).__name__, file=sys.stderr)
        raise SystemExit(78) from None
