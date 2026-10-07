"""Actual private subprocess streams conserve full members within fixed budgets."""
import importlib.util
import json
import pathlib
import sys
import unittest
import gzip
import hashlib
import os
import tempfile
import time

sys.dont_write_bytecode = True
source = pathlib.Path(__file__).resolve().parents[1] / "admission-source.py"
spec = importlib.util.spec_from_file_location("stream_source", source)
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)


class Streaming(unittest.TestCase):
    def event(self, number=1):
        return (json.dumps({"apiVersion": "audit.k8s.io/v1", "kind": "Event", "level": "Metadata",
                            "stage": "ResponseComplete", "auditID": f"00000000-0000-4000-8000-{number:012x}",
                            "requestReceivedTimestamp": "2026-10-05T00:00:00.000000001Z",
                            "stageTimestamp": "2026-10-05T00:00:01Z", "private": "秘密"}, ensure_ascii=False)
                + "\n").encode()

    def measure(self, raw, compressed=False, width=65536, budget=None, seen=None):
        class Port:
            def stream_bytes(self, argv, deadline):
                for offset in range(0, len(raw), width):
                    yield raw[offset:offset + width]
        return d.stream_member(Port(), [], compressed, budget or d.StreamBudget(),
                               set() if seen is None else seen)

    def test_arbitrary_utf8_newline_and_gzip_member_boundaries_conserve_every_byte(self):
        decoded = self.event() + self.event(2)
        compressed = gzip.compress(self.event()) + gzip.compress(self.event(2))
        for raw, zipped in [(decoded, False), (compressed, True)]:
            for width in [1, 7, 65536]:
                value = self.measure(raw, zipped, width)
                self.assertEqual(value["encoded_bytes"], len(raw))
                self.assertEqual(value["decoded_bytes"], len(decoded))
                self.assertEqual(value["content_identity"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(value["records"], 2)

    def test_large_joined_probe_survives_the_actual_host_receipt_contract(self):
        access_spec = importlib.util.spec_from_file_location("access_fixtures", source.with_name("tests") / "admission-access-test.py")
        fixtures = importlib.util.module_from_spec(access_spec)
        access_spec.loader.exec_module(fixtures)
        fixtures.d = d
        estate, port = fixtures.AccessProbe().fixtures()
        raw = b"".join(self.event(n).replace("秘密".encode(), b"x" * 1800) for n in range(5000))
        self.assertGreater(len(raw), 8 * d.MAX_BYTES)
        original = port.read_bytes
        def read_bytes(argv):
            value = original(argv)
            if argv[4] == "list":
                value = value.replace(str(len(original([*argv[:4], "read", "/var/log/apiserver/audit.log"]))).encode(), str(len(raw)).encode())
            return value
        def stream_bytes(argv, deadline):
            for offset in range(0, len(raw), 65536):
                yield raw[offset:offset + 65536]
        port.read_bytes = read_bytes
        port.stream_bytes = stream_bytes
        report = d.probe_access(estate.ESTATE, estate.TARGET, port)
        self.assertTrue(all(node["access"]["state"] == "measured" for node in report["nodes"]), report)
        self.assertEqual([node["access"]["records"] for node in report["nodes"]], [5000, 5000])
        self.assertEqual(report["history_verdict"], "unavailable")
        self.assertTrue(fixtures.ReceiptContract().accepts(report))

    def test_truncated_trailing_corrupt_duplicate_or_partial_population_refuses(self):
        good = gzip.compress(self.event())
        for raw, zipped in [(good[:-1], True), (good + b"private garbage", True),
                             (good[:-8] + b"\x00" * 8, True), (self.event() * 2, False),
                             (self.event()[:-1], False), (b"", False)]:
            with self.subTest(zipped=zipped, size=len(raw)), self.assertRaises(d.Unavailable):
                self.measure(raw, zipped, 7)

    def test_record_identity_and_exact_shared_work_boundaries_refuse_without_sampling(self):
        with self.assertRaises(d.Unavailable) as error:
            self.measure(b"x" * (256 * 1024 + 1))
        self.assertEqual(str(error.exception), "retained_stream_record_bound")
        budget = d.StreamBudget()
        budget.pass_records[0] = 10000
        with self.assertRaises(d.Unavailable) as error:
            self.measure(self.event(), budget=budget)
        self.assertEqual(str(error.exception), "retained_record_count")
        for field, value, reason in [
                ("encoded", 512 * d.MAX_BYTES, "retained_stream_work_bound"),
                ("decoded", 1024 * d.MAX_BYTES, "retained_stream_decoded_work_bound"),
                ("deadline", time.monotonic() - 1, "retained_stream_time_bound")]:
            budget = d.StreamBudget()
            setattr(budget, field, value)
            with self.subTest(field=field), self.assertRaises(d.Unavailable) as error:
                self.measure(self.event(), budget=budget)
            self.assertEqual(str(error.exception), reason)
        budget = d.StreamBudget()
        budget.reserve([{"size": 256 * d.MAX_BYTES}])
        with self.assertRaises(d.Unavailable):
            budget.reserve([{"size": 1}])
        for encoded, decoded, expected in [(256 * d.MAX_BYTES, 0, "retained_population_size"),
                                            (0, 512 * d.MAX_BYTES, "retained_stream_decoded_population_bound")]:
            budget = d.StreamBudget()
            budget.charge(encoded=encoded, decoded=decoded)
            with self.assertRaises(d.Unavailable) as error:
                budget.charge(encoded=int(encoded > 0), decoded=int(decoded > 0))
            self.assertEqual(str(error.exception), expected)

    def test_encoded_member_and_decoded_gzip_expansion_caps_consume_no_clean_prefix(self):
        line = self.event().replace("秘密".encode(), b"x" * 200000)
        raw = b"".join(line.replace(b"000000000001", f"{n:012x}".encode()) for n in range(350))
        for payload, compressed, reason in [(raw, False, "retained_stream_member_bound"),
                                           (gzip.compress(raw), True, "retained_stream_decoded_member_bound")]:
            with self.subTest(compressed=compressed), self.assertRaises(d.Unavailable) as error:
                self.measure(payload, compressed)
            self.assertEqual(str(error.exception), reason)

    def test_actual_diagnostic_nonzero_timeout_and_consumer_refusal_reap_the_process(self):
        for body, reason in [("sys.stderr.write('PRIVATE');sys.stderr.flush();time.sleep(5)", "native_read_diagnostic"),
                             ("sys.exit(2)", "native_read_failed"),
                             ("time.sleep(5)", "native_read_timeout"),
                             ("sys.stdout.buffer.write(b'x'*327681);sys.stdout.flush();time.sleep(5)",
                              "retained_stream_record_bound")]:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as directory:
                pid_file = pathlib.Path(directory) / "pid"
                program = "import sys,time,os,pathlib; pathlib.Path(" + repr(str(pid_file)) + ").write_text(str(os.getpid()));" + body
                with self.assertRaises(d.Unavailable) as error:
                    d.stream_member(d.NativeReadPort(timeout=0.2), [sys.executable, "-B", "-c", program],
                                    False, d.StreamBudget(), set())
                self.assertEqual(str(error.exception), reason)
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(pid_file.read_text()), 0)

    def test_real_underbudget_member_above_old_bound_is_read_entirely(self):
        event = {"apiVersion": "audit.k8s.io/v1", "kind": "Event",
                 "level": "Metadata", "stage": "ResponseComplete",
                 "auditID": "00000000-0000-4000-8000-000000000001",
                 "requestReceivedTimestamp": "2026-10-05T00:00:00Z",
                 "stageTimestamp": "2026-10-05T00:00:01Z"}
        payload = b"".join((json.dumps(dict(event, auditID=f"00000000-0000-4000-8000-{n:012x}",
                                           private="PRIVATE-NATIVE-" + "x" * 1800)) + "\n").encode()
                           for n in range(700))
        self.assertGreater(len(payload), d.MAX_BYTES)
        command = [sys.executable, "-B", "-c",
                   "import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())"]
        # Native reads intentionally have no caller-input pipe; write the
        # synthetic records from a subprocess program rather than a raw file.
        command[-1] = "import sys,json; e=" + repr(event) + "; " + (
            "[sys.stdout.buffer.write((json.dumps(dict(e,auditID=f'00000000-0000-4000-8000-{n:012x}',"
            "private='PRIVATE-NATIVE-'+'x'*1800))+'\\n').encode()) for n in range(700)]")
        with self.assertRaises(d.Unavailable):
            d.NativeReadPort().read_bytes(command)
        measurement = d.stream_member(d.NativeReadPort(), command, False, d.StreamBudget(), set())
        self.assertEqual(measurement["encoded_bytes"], len(payload))
        self.assertEqual(measurement["decoded_bytes"], len(payload))
        self.assertEqual(measurement["records"], 700)
        self.assertNotIn("PRIVATE-NATIVE", json.dumps(measurement))


if __name__ == "__main__":
    unittest.main()
