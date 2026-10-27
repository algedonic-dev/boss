#!/usr/bin/env python3
"""The fixed native observation reader retains the whole protected response."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]


class FixedReader(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.here = self.root / "infra/forge"
        self.here.mkdir(parents=True)
        lib = self.root / "infra/lib"
        lib.mkdir()
        shutil.copyfile(ROOT / "infra/forge/read-membership-starting-state.sh", self.here / "read.sh")
        (lib / "sor.sh").write_text('sor_require() { export BOSS_JOBS_URL=http://fixed-fixture.invalid:7900; }\nsor_reader_header() { printf \'{"id":"%s","role":"audit-readonly"}\' "$1"; }\n')
        (lib / "secret-header.sh").write_text('machine_token_header() { [ "${NO_TOKEN:-0}" = 1 ] || printf -v "$1" %s "fixture-header: opaque"; }\nsecret_header_close() { :; }\n')
        bindir = self.root / "bin"
        bindir.mkdir()
        curl = bindir / "curl"
        curl.write_text('#!/usr/bin/env python3\nimport json,os,sys\nfrom pathlib import Path\nPath(os.environ["ARGV"]).write_text(json.dumps(sys.argv[1:]))\nsys.stdout.write(Path(os.environ["RESPONSE_FILE"]).read_text() if "RESPONSE_FILE" in os.environ else os.environ["RESPONSE"])\nsys.stderr.write("fixture transport stderr\\n")\nsys.exit(int(os.environ.get("CURL_STATUS","0")))\n')
        curl.chmod(0o755)
        boss = bindir / "boss"
        boss.write_text('#!/usr/bin/env python3\nimport json,os,sys\nfrom pathlib import Path\na=sys.argv[1:]\nassert a[0]=="attach" and len(a)==3,a\np=Path(a[2]); b=p.read_bytes()\nr=Path(os.environ["ATTACHMENTS"]); r.mkdir(exist_ok=True)\n(r/p.name).write_bytes(b)\nwith (r/"calls.jsonl").open("a") as f: f.write(json.dumps({"target":a[1],"name":p.name,"bytes":len(b)})+"\\n")\nsys.exit(int(os.environ.get("ATTACH_STATUS","0")))\n')
        boss.chmod(0o755)
        helper = ROOT / "infra/forge/membership-observation-retain.py"
        if helper.exists():
            shutil.copyfile(helper, self.here / helper.name)
        self.argv = self.root / "argv.json"
        self.env = dict(os.environ, PATH=str(bindir) + os.pathsep + os.environ["PATH"], ARGV=str(self.argv), OPS_REQUEST_ID="07198753-5436-491b-94dc-017b52078044", ATTACHMENTS=str(self.root / "attachments"), RESPONSE=json.dumps({"state":"observed","phases":[{"name":"seed","plan":{"text":"雪 \\\"quoted\\\""}}]}))

    def run_reader(self, *args):
        return subprocess.run(["bash", str(self.here / "read.sh"), *args], env=self.env, capture_output=True)

    def test_one_fixed_protected_read_preserves_complete_native_bytes(self):
        result = self.run_reader()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, self.env["RESPONSE"].encode())
        self.assertIn(b"fixture transport stderr", result.stderr)
        argv = json.loads(self.argv.read_text())
        self.assertIn("--fail-with-body", argv, "HTTP refusal bodies are original evidence")
        self.assertFalse(any(arg.startswith("-") and not arg.startswith("--") and "f" in arg[1:] for arg in argv), "curl -f would discard a typed refusal body")
        self.assertEqual(argv[-1], "http://fixed-fixture.invalid:7900/api/jobs/membership-starting-state-observation")
        self.assertEqual(argv[argv.index("--connect-timeout") + 1], "5")
        self.assertEqual(argv[argv.index("--max-time") + 1], "100")
        self.assertEqual(argv[argv.index("--max-filesize") + 1], "1048576")
        self.assertIn('x-boss-user: {"id":"automation:read-membership-starting-state","role":"audit-readonly"}', argv)
        self.assertIn("fixture-header: opaque", argv)
        self.assertNotIn(b"opaque", result.stdout + result.stderr)


    def test_original_body_is_attached_once_to_actual_request(self):
        result = self.run_reader()
        self.assertEqual(result.returncode, 0, result.stderr)
        attached = self.root / "attachments"
        calls = [json.loads(line) for line in (attached / "calls.jsonl").read_text().splitlines()]
        body = self.env["RESPONSE"].encode()
        matching = [row for row in calls if (attached / row["name"]).read_bytes() == body]
        self.assertEqual(len(matching), 1, "attach exact original body once, never only the capped preview")
        self.assertTrue(all(row["target"] == self.env["OPS_REQUEST_ID"] for row in calls))
        manifests = [json.loads(path.read_text()) for path in attached.glob("*manifest*.json")]
        self.assertEqual(len(manifests), 1)
        self.assertEqual(manifests[0]["original_query_exit"], 0)
        self.assertEqual(manifests[0]["stdout"]["sha256"], hashlib.sha256(body).hexdigest())

    def test_large_originals_exceed_preview_without_losing_bytes(self):
        for size in (439525, 1048576):
            with self.subTest(size=size):
                raw = ("0123456789abcdef" * ((size + 15) // 16))[:size].encode()
                source = self.root / "response"
                source.write_bytes(raw)
                self.env["RESPONSE_FILE"] = str(source)
                self.env["ATTACHMENTS"] = str(self.root / ("attached-" + str(size)))
                result = self.run_reader()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, raw)
                attached = Path(self.env["ATTACHMENTS"])
                self.assertEqual((attached / "stdout").read_bytes(), raw)
                self.assertEqual(json.loads((attached / "manifest.json").read_text())["stdout"]["bytes"], size)

    def test_oversized_original_refuses_publication_without_truncating(self):
        source = self.root / "oversized"
        source.write_bytes(b"x" * 1048577)
        self.env["RESPONSE_FILE"] = str(source)
        result = self.run_reader()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, source.read_bytes())
        self.assertIn(b"exceeds original attachment bound", result.stderr)
        self.assertFalse((self.root / "attachments").exists())

    def test_malformed_request_identity_never_queries(self):
        self.env["OPS_REQUEST_ID"] = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
        result = self.run_reader()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.argv.exists())

    def test_query_failure_and_attachment_failure_remain_distinct(self):
        self.env.update(CURL_STATUS="28", ATTACH_STATUS="1")
        result = self.run_reader()
        self.assertEqual(result.returncode, 28)
        self.assertEqual(result.stdout, self.env["RESPONSE"].encode())
        self.assertIn(b"query exit 28", result.stderr)
        self.assertIn(b"retention failed", result.stderr)

    def test_attachment_failure_is_not_successful_query_retention(self):
        self.env["ATTACH_STATUS"] = "1"
        result = self.run_reader()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"retention failed", result.stderr)
        self.assertIn(b"query exit 0", result.stderr)

    def test_missing_request_context_refuses_before_query(self):
        self.env.pop("OPS_REQUEST_ID")
        result = self.run_reader()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.argv.exists())

    def test_arguments_are_refused_before_any_read(self):
        result = self.run_reader("--sql", "SELECT secret")
        self.assertEqual(result.returncode, 78)
        self.assertFalse(self.argv.exists())
        self.assertIn(b"no arguments accepted", result.stderr)

    def test_missing_reader_token_never_makes_a_tokenless_request(self):
        self.env["NO_TOKEN"] = "1"
        result = self.run_reader()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.argv.exists())

    def test_transport_failure_retains_original_stdout_stderr_and_exit(self):
        self.env["CURL_STATUS"] = "28"
        result = self.run_reader()
        self.assertEqual(result.returncode, 28)
        self.assertEqual(result.stdout, self.env["RESPONSE"].encode())
        self.assertIn(b"fixture transport stderr", result.stderr)


if __name__ == "__main__":
    unittest.main()
