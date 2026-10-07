"""Actual TLS wire fixture; generated material only, never cluster mutation."""
import http.server
import importlib.util
import json
from pathlib import Path
import ssl
import subprocess
import tempfile
import threading
import unittest


class LifecycleWire(unittest.TestCase):
    def test_existing_context_supplies_only_nonsecret_server_and_identity(self):
        spec = importlib.util.spec_from_file_location("lifecycle", Path(__file__).with_name("dev-lifecycle.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        calls = []
        def read(argv):
            calls.append(argv)
            if argv == ["config", "current-context"]:
                return subprocess.CompletedProcess(argv, 0, b"in-cluster\n", b"")
            if argv[:2] == ["config", "view"]:
                return subprocess.CompletedProcess(argv, 0, b"https://kubernetes.default.svc", b"")
            return subprocess.CompletedProcess(argv, 0, json.dumps({"status":{"userInfo":{"username":"system:serviceaccount:boss-dev:dev-session"}}}).encode(), b"")
        client = module.existing_session(read)
        self.assertEqual(client.host, "kubernetes.default.svc")
        self.assertEqual(client.port, 443)
        self.assertEqual(len(calls), 3)
        self.assertTrue(any("jsonpath={.clusters[0].cluster.server}" in argv for argv in calls))
        def wrong(argv):
            return subprocess.CompletedProcess(argv, 0, b"other-context", b"")
        with self.assertRaises(ValueError):
            module.existing_session(wrong)

    def test_delete_sends_uid_preconditions_and_refuses_a_replacement(self):
        with tempfile.TemporaryDirectory(prefix="boss-lifecycle-wire-") as temporary:
            root = Path(temporary)
            certificate = root / "certificate.pem"
            key = root / "key.pem"
            generated = subprocess.run(["/usr/bin/openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(key), "-out", str(certificate), "-days", "1", "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1"], capture_output=True, check=False)
            self.assertEqual(generated.returncode, 0, generated.stderr)
            token = root / "fake-token"
            token.write_text("generated-fixture-token")
            observations = []
            behavior = {"mode": "normal"}
            class Receiver(http.server.BaseHTTPRequestHandler):
                def do_DELETE(self):
                    body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
                    authorized = self.headers.get("Authorization") == "Bearer generated-fixture-token"
                    accepted = authorized and body.get("preconditions", {}).get("uid") == "known-job-uid"
                    observations.append({"path": self.path, "body": body, "authorized": authorized, "accepted": accepted})
                    if behavior["mode"] == "redirect":
                        self.send_response(302)
                        self.send_header("Location", "/unsafe")
                        self.end_headers()
                        return
                    if behavior["mode"] == "partial":
                        self.send_response(200)
                        self.send_header("Content-Length", "100000")
                        self.end_headers()
                        self.wfile.write(b"full partial API observation")
                        return
                    if behavior["mode"] == "invalid-json":
                        self.send_response(200)
                        self.end_headers()
                        self.wfile.write(b"complete invalid API response")
                        return
                    self.send_response(200 if accepted else 409)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(json.dumps({"kind":"Status", "status":"Success" if accepted else "Failure"}).encode())
                def log_message(self, *_):
                    pass
            server = http.server.HTTPServer(("127.0.0.1", 0), Receiver)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, key)
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                spec = importlib.util.spec_from_file_location("lifecycle", Path(__file__).with_name("dev-lifecycle.py"))
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                client = module.DeleteTransport("127.0.0.1", server.server_port, certificate, token)
                records = []
                with self.assertRaises(RuntimeError):
                    client.delete_job("dev-build-1234567890abcdef", "stale-job-uid", records.append)
                client.delete_job("dev-build-1234567890abcdef", "known-job-uid", records.append)
                self.assertEqual([o["accepted"] for o in observations], [False, True])
                self.assertTrue(all(o["body"]["kind"] == "DeleteOptions" for o in observations))
                self.assertTrue(all(o["path"] == "/apis/batch/v1/namespaces/boss-dev/jobs/dev-build-1234567890abcdef" for o in observations))
                self.assertEqual([r["http_status"] for r in records], [409, 200])
                self.assertNotIn("generated-fixture-token", json.dumps(records))
                before = len(observations)
                wrong_host = module.DeleteTransport("localhost", server.server_port, certificate, token)
                with self.assertRaises(RuntimeError):
                    wrong_host.delete_job("dev-build-1234567890abcdef", "known-job-uid", records.append)
                self.assertEqual(len(observations), before, "CA/hostname refusal must precede authority delivery")
                behavior["mode"] = "redirect"
                with self.assertRaises(RuntimeError):
                    client.delete_job("dev-build-1234567890abcdef", "known-job-uid", records.append)
                self.assertEqual(len(observations), before + 1, "redirect must not cause a second request")
                behavior["mode"] = "partial"
                with self.assertRaises(RuntimeError):
                    client.delete_job("dev-build-1234567890abcdef", "known-job-uid", records.append)
                import base64
                self.assertEqual(base64.b64decode(records[-1]["response_base64"]), b"full partial API observation")
                self.assertEqual(records[-1]["completion"], "unproven")
                behavior["mode"] = "invalid-json"
                with self.assertRaises(RuntimeError):
                    client.delete_job("dev-build-1234567890abcdef", "known-job-uid", records.append)
                self.assertEqual(base64.b64decode(records[-1]["response_base64"]), b"complete invalid API response")
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
