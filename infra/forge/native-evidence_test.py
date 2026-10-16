"""4196: actual decoder and protected-reader adapter, without a native effect."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import subprocess
import unittest

SOURCE = Path(sys.argv.pop(1))
spec = importlib.util.spec_from_file_location("evidence", SOURCE / "infra/forge/native-evidence.py")
e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e)


def packet():
    return {"id": "11111111-1111-4111-8111-111111111111", "kind": "control",
            "partition": "real", "simulated": False, "status": "closed",
            "opened_at": "2026-10-15T10:00:00Z", "metadata": {"host": "forge"},
            "steps": [{"id": "33333333-3333-4333-8333-333333333333", "job_id": "11111111-1111-4111-8111-111111111111",
                       "spec_slug": "execute", "status": "completed",
                       "completed_by": "automation:ops-runner", "completed_at": "2026-10-15T10:01:00Z",
                       "metadata": {"runner_host": "forge", "disposition": "answered", "exit_code": "0"}}]}


class Controls(unittest.TestCase):
    def test_archive_integrity_reuses_native_completion_without_claiming_restore(self):
        spec = importlib.util.spec_from_file_location("archives", SOURCE / "infra/forge/proof-observers/archive-integrity.py")
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        p = packet(); step = p["steps"][0]
        receipt = {"claim": "remote-archive-integrity", "result": "ok", "stage": "verified",
                   "gzip_test": True, "completion_trailer": True, "generation": "1", "object": "boss-20261015-100000.sql.gz",
                   "sha256": "a" * 64, "object_bytes": 12, "downloaded_bytes": 12,
                   "listed_pages": 1, "matching_dumps": 1, "checked_at": "2026-10-15T10:00:30Z"}
        step["metadata"].update(effect="check-gcs-backup: READ — remote archive integrity verified", output="receipt: " + json.dumps(receipt))
        contract = {"kind": "control", "host": "forge", "step": "execute", "completion_actor": "automation:ops-runner", "disposition": "answered"}
        cut = e.instant("2026-10-15T09:00:00Z"); now = e.instant("2026-10-15T11:00:00Z")
        self.assertEqual(module.observe(e, [p], cut, now, contract)[0], 0)
        # ops-runner.sh writes --arg rc, so only its canonical string
        # zero is a success; coercion could accept booleans or bad exits.
        for exit_code in [False, True, 0, 0.0, None, "", "00", "+0", " 0", "0 ", "0.0", "false", "1", "75", [], {}]:
            with self.subTest(exit_code=exit_code):
                step["metadata"]["exit_code"] = exit_code
                self.assertEqual(module.observe(e, [p], cut, now, contract)[0], 75)
        step["metadata"]["exit_code"] = "0"
        receipt["claim"] = "restore"; step["metadata"]["output"] = "receipt: " + json.dumps(receipt)
        with self.assertRaises(e.Unknown): module.observe(e, [p], cut, now, contract)

    def test_every_registered_contract_resolves_its_own_observer(self):
        spec = importlib.util.spec_from_file_location("observers", SOURCE / "infra/forge/proof-observer.py")
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        paths = list((SOURCE / "infra/platform/proof-observers").glob("*.json"))
        self.assertGreaterEqual(len(paths), 3)
        for path in paths:
            contract = module.contract(path.stem)
            self.assertEqual(type(contract["version"]), int)
            self.assertTrue(path.stem.endswith("-v" + str(contract["version"])))
            self.assertTrue((SOURCE / "infra/forge/proof-observers" / (contract["module"] + ".py")).is_file())

    def test_a_real_timeout_is_witnessed_while_unknown_runs_do_not_become_absence(self):
        spec = importlib.util.spec_from_file_location("timeouts", SOURCE / "infra/forge/proof-observers/gate-timeout.py")
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        p = packet(); p["metadata"].update(sha="a" * 40, outcome="failed", launched_at="2026-10-15T10:00:30Z")
        p["steps"][0].update(spec_slug="record-verdict", completed_by="automation:gate-runner")
        body = {"head": "a" * 40, "verdict": "failed", "checks": [{"name": "web-suite", "result": "fail"}],
                "fails": ["web-suite: playwright verdict line"],
                "fails_excerpt": {"web-suite": "  1) tests/control.spec.ts:1:1 › fixture\n    Test timeout of 1000ms exceeded.\n\n  1 failed\n    tests/control.spec.ts:1:1 › fixture\n"}}
        p["steps"][0]["metadata"] = {"verdict": "failed", "receipt": json.dumps(body)}
        cut = e.instant("2026-10-15T09:00:00Z"); now = e.instant("2026-10-15T11:00:00Z")
        contract = {"completion_actors": ["automation:gate-runner"]}
        code, result = module.observe(e, [p], cut, now, contract)
        self.assertEqual(code, 0); self.assertEqual(result["matches"][0]["id"], p["id"])
        self.assertEqual(result["matches"][0]["timed_out_specs"][0]["timeout"], "Test timeout of 1000ms exceeded.")
        p["metadata"]["outcome"] = "lost"; p["steps"][0]["metadata"] = {"verdict": "lost"}
        code, result = module.observe(e, [p], cut, now, contract)
        self.assertEqual(code, 75); self.assertEqual(result["unjudged"], [p["id"]])

    def test_existing_protected_reader_exposes_only_registered_evidence(self):
        result = subprocess.run(["bash", str(SOURCE / "infra/forge/probe-bin/boss-sor-read"),
                                 "--evidence", "../not-a-contract"], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 75)
        self.assertEqual(json.loads(result.stdout)["observation"], "unknown")

    def test_registered_sponsorship_uses_server_admission_and_refuses_old_false_positives(self):
        spec = importlib.util.spec_from_file_location("observers", SOURCE / "infra/forge/proof-observer.py")
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        contract = module.contract("sponsorship-v1")
        cut = e.instant("2026-10-15T09:20:00Z")
        env = {"BOSS_CAR_CONVERGED_AT": str(cut // 10**9), "BOSS_CAR_MERGE_REF": "a" * 40}
        control = packet(); control.update(id=contract["car"], kind="ship-a-change")
        control["metadata"].update(branch=contract["branch"], merge_ref="a" * 40)
        row = packet(); row["kind"] = contract["kind"]
        def git(*args): return env["BOSS_CAR_CONVERGED_AT"] if args[0] == "show" else "a" * 40
        def read(path):
            return json.dumps(control if path.startswith("/api/jobs/") else
                              {"data": [row], "total": 1, "full": True, "offset": 0, "limit": 100}).encode()
        now = e.instant("2026-10-15T11:00:00Z")
        self.assertEqual(module.run(contract, read, env, git, now)[0], 0)
        for field, value in [("simulated", True), ("partition", "simulated"), ("kind", "foreign")]:
            old = row[field]; row[field] = value
            with self.assertRaises(module.e.Unknown): module.run(contract, read, env, git, now)
            row[field] = old
        row["opened_at"] = "2026-10-15T08:00:00Z"
        row["metadata"]["opened_at"] = "2026-10-15T10:00:00Z"
        self.assertEqual(module.run(contract, read, env, git, now)[0], 75)

    def test_a_gate_binding_refusal_names_the_actual_gate_not_a_guessed_terminal(self):
        p = packet(); p["metadata"].update(sha="a" * 40, outcome="failed")
        p["steps"][0].update(spec_slug="record-verdict", completed_by="automation:gate-runner")
        p["steps"][0]["metadata"] = {"verdict": "failed", "receipt": {"head": "b" * 40, "verdict": "failed"}}
        with self.assertRaisesRegex(e.Unknown, p["id"] + ": receipt is not bound"):
            e.receipt(p, ["automation:gate-runner"])

    def test_registry_rejects_traversal_and_does_not_run_an_unregistered_module(self):
        spec = importlib.util.spec_from_file_location("observers", SOURCE / "infra/forge/proof-observer.py")
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        for name in ["../native-evidence", "/etc/passwd", "", "SPONSORSHIP"]:
            with self.assertRaises(module.e.Unknown): module.contract(name)

    def test_one_complete_document_never_silence_or_ambiguous_json(self):
        self.assertEqual(e.document(b'{"ok":true}'), {"ok": True})
        for raw in [b'', b' \n', b'{} {}', b'{"ok":true,"ok":false}', b'NaN', b'{', b'\xff']:
            with self.subTest(raw=raw), self.assertRaises(e.Unknown):
                e.document(raw)

    def test_server_bounds_and_native_completion_need_all_provenance(self):
        cut = e.cutoff(str(e.instant("2026-10-15T09:20:00Z") // 10**9))
        now = e.instant("2026-10-15T11:00:00Z")
        contract = {"kind": "control", "host": "forge", "step": "execute",
                    "completion_actor": "automation:ops-runner", "disposition": "answered"}
        self.assertIsNotNone(e.execution(packet(), contract, cut, now))
        changes = [lambda p: p.update(simulated=True), lambda p: p.update(partition="simulated"),
                   lambda p: p.update(kind="foreign"), lambda p: p["metadata"].update(simulated=True),
                   lambda p: p["metadata"].update(host="foreign"),
                   lambda p: p["steps"][0].update(completed_by="agent-control"),
                   lambda p: p["steps"][0]["metadata"].update(runner_host="foreign"),
                   lambda p: p["steps"][0]["metadata"].update(disposition="unanswered"),
                   lambda p: p["steps"][0].update(id=None),
                   lambda p: p["steps"][0].update(job_id="22222222-2222-4222-8222-222222222222"),
                   lambda p: p.update(opened_at=None),
                   lambda p: p["steps"][0].update(completed_at="2026-10-15T09:59:00Z"),
                   lambda p: p["steps"][0].update(completed_at="2026-10-16T09:59:00Z")]
        for change in changes:
            p = packet(); change(p)
            with self.assertRaises(e.Unknown): e.execution(p, contract, cut, now)
        p = packet(); p["opened_at"] = "2026-10-15T08:00:00Z"
        p["metadata"]["opened_at"] = "2026-10-15T10:00:00Z"
        self.assertIsNone(e.execution(p, contract, cut, now), "metadata date must not replace server admission")
        for value in [None, "", "0", "-1", "1.1", "true"]:
            with self.assertRaises(e.Unknown): e.cutoff(value)

    def test_complete_pages_conserve_identity_offset_total_and_shape(self):
        p = packet(); q = copy.deepcopy(p); q["id"] = "22222222-2222-4222-8222-222222222222"
        def read(path):
            offset = int(path.split("offset=")[1])
            return json.dumps({"data": [p if offset == 0 else q], "full": True,
                               "total": 2, "offset": offset, "limit": 1}).encode()
        self.assertEqual(len(e.pages(read, "/api/jobs?kind=control", "control", limit=1)), 2)
        for fault in ["duplicate", "total", "offset", "full", "short"]:
            def bad(path):
                body = e.document(read(path)); offset = body["offset"]
                if offset:
                    if fault == "duplicate": body["data"] = [p]
                    elif fault == "total": body["total"] = 3
                    elif fault == "offset": body["offset"] = 0
                    elif fault == "full": body["full"] = False
                    else: body["data"] = []
                return json.dumps(body).encode()
            with self.subTest(fault=fault), self.assertRaises(e.Unknown):
                e.pages(bad, "/api/jobs?kind=control", "control", limit=1)

    def test_no_compute_precedes_executed_binding_but_contradiction_names_packet(self):
        p = packet(); p["metadata"].update(sha="a" * 40, outcome="withdrawn")
        p["steps"][0].update(spec_slug="record-verdict")
        p["steps"][0]["metadata"] = {"verdict": "withdrawn", "receipt": json.dumps(
            {"verdict": "withdrawn", "head": "", "mode": "", "fails": [], "withdrawn_because": "before compute"})}
        self.assertEqual(e.receipt(p, ["automation:gate-runner"])[0], "no-compute")
        p["steps"][0]["metadata"]["receipt"] = '{"verdict":"green","head":""}'
        with self.assertRaisesRegex(e.Unknown, p["id"]): e.receipt(p, ["automation:gate-runner"])
        for verdict in ["lost", "refused"]:
            p["metadata"]["outcome"] = verdict
            p["steps"][0]["metadata"] = {"verdict": verdict}
            self.assertEqual(e.receipt(p, ["automation:gate-runner"])[0], "unknown")

    def test_observation_never_promotes_unmeasured_absence(self):
        cut = e.cutoff(str(e.instant("2026-10-15T09:20:00Z") // 10**9)); now = e.instant("2026-10-15T11:00:00Z")
        self.assertEqual(e.observation([{"id": "control"}], cut, now)[0], 0)
        self.assertEqual(e.observation([], cut, now)[0], 75)
        coverage = {"begin_at": "2026-10-15T08:00:00Z", "through_at": "2026-10-15T11:00:00Z", "gaps": [], "source": "native-control"}
        code, body = e.observation([], cut, now, coverage)
        self.assertEqual(code, 1); self.assertEqual(body["observation"], "unseen")
        coverage["gaps"] = ["unread"]
        with self.assertRaises(e.Unknown): e.observation([], cut, now, coverage)
        coverage["gaps"] = []
        coverage["through_at"] = "2026-10-16T11:00:00Z"
        with self.assertRaises(e.Unknown): e.observation([], cut, now, coverage)


if __name__ == "__main__": unittest.main()
