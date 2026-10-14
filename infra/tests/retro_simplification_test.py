"""Owning no-network fixtures: real two-car squash history, native record shape."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SOURCE = Path(sys.argv.pop(1))
SCRATCH = Path(sys.argv.pop(1)).resolve()
SCRATCH.mkdir(parents=True, exist_ok=True)


class Readings(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("reading", SOURCE)
        self.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.reader)
        self.temp = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Synthetic fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.write("crates/core/fixture.rs", "base\n")
        self.write("other.txt", "one\ntwo\nthree\n")
        self.commit("initial", "2026-09-20T12:00:00Z")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-q", "-b", "car-a")
        self.write("crates/core/fixture.rs", "base\nadded\n")
        self.commit("source a", "2026-09-21T09:00:00Z")
        self.a = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-q", "-b", "car-b", self.base)
        self.write("other.txt", "three\n")
        self.write("extra.txt", "new\nnewer\n")
        self.commit("source b", "2026-09-21T10:00:00Z")
        self.b = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-q", "main")
        self.git("merge", "--squash", "car-a", "car-b")
        self.ids = ["11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222"]
        self.tid = "33333333-3333-4333-8333-333333333333"
        message = "train: fixture (2 changes) (#1)\n\n" + "\n".join(
            f"car-{name} — synthetic [ship-a-change {cid[:8]}, no item: fixture]"
            for name, cid in zip(["a", "b"], self.ids)
        )
        self.commit(message, "2026-09-22T12:00:00Z")
        self.merge = self.git("rev-parse", "HEAD").strip()
        self.cars = [self.car(cid, head) for cid, head in zip(self.ids, [self.a, self.b])]
        self.train = {"id": self.tid, "kind": "pr-train", "metadata": {"boarded_jobs": self.ids},
                      "steps": [{"spec_slug": "merged", "status": "completed", "metadata": {"merge_ref": self.merge[:12]}}]}
        self.bundle = {"pages": [{"total": 2, "data": self.cars}], "cars": self.cars, "trains": [self.train]}

    def git(self, *args, when=None):
        env = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1"}
        if when:
            env.update(GIT_AUTHOR_DATE=when, GIT_COMMITTER_DATE=when)
        result = subprocess.run(["git", "-C", str(self.repo), *args], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def write(self, path, value):
        p = self.repo / path
        p.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(value, bytes): p.write_bytes(value)
        else: p.write_text(value)

    def commit(self, message, when):
        self.git("add", ".")
        self.git("commit", "-q", "-m", message, when=when)

    def car(self, cid, head):
        return {"id": cid, "kind": "ship-a-change", "metadata": {"outcome": "merged", "merge_ref": self.merge[:12], "train": self.tid, "boarded_head": head},
                "steps": [{"spec_slug": "gate", "status": "completed", "metadata": {"receipt": json.dumps({"head": head, "verdict": "green"})}}]}

    def read(self, bundle=None, since="2026-09-21", until="2026-09-28"):
        return self.reader.measure(str(self.repo), "main", since, until, bundle or self.bundle)

    def test_differing_cars_in_the_actual_squash_shape(self):
        result = self.read()
        week = result["current"]
        self.assertEqual(week["cars_landed"], {"state": "measured", "value": 2})
        self.assertEqual(week["median_files"]["value"], 1.5)
        self.assertEqual(week["median_adds"]["value"], 1.5)
        self.assertEqual(week["median_removes"]["value"], 1)
        self.assertAlmostEqual(week["removes_adds_ratio"]["value"], 2 / 3)
        self.assertEqual(week["core_share"]["value"], 0.5)
        self.assertEqual(week["coverage"]["measured_cars"], 2)
        self.assertEqual({c["base"] for c in week["cars"]}, {self.base})
        self.assertEqual(result["trend"]["cars_landed"]["delta"], 2)
        self.assertEqual(result["previous"]["median_adds"]["state"], "undefined")

    def test_missing_or_wrong_receipts_never_enter_the_median(self):
        for mutate in [
            lambda b: b["cars"][0]["steps"].clear(),
            lambda b: b["cars"][0]["steps"][0]["metadata"].update(receipt='{"head":"bad","verdict":"green"}'),
            lambda b: b["cars"][0]["metadata"].update(regate_receipt='{"head":"bad","verdict":"green"}'),
            lambda b: b["cars"][0]["metadata"].update(boarded_head="f" * 40),
            lambda b: b["trains"][0]["metadata"].update(boarded_jobs=[self.ids[1]]),
            lambda b: b["cars"][0]["metadata"].update(train="44444444-4444-4444-8444-444444444444"),
        ]:
            bundle = copy.deepcopy(self.bundle)
            mutate(bundle)
            week = self.read(bundle)["current"]
            self.assertEqual(week["median_files"]["state"], "unknown")
            self.assertIsNone(week["median_files"]["value"])
            self.assertGreater(week["coverage"]["unknown_cars"], 0)

    def test_incomplete_duplicate_and_changing_pages_are_unknown(self):
        for pages in [
            [{"total": 3, "data": self.cars}],
            [{"total": 2, "data": [self.cars[0], self.cars[0]]}],
            [{"total": 2, "data": [self.cars[0]]}, {"total": 3, "data": [self.cars[1]]}],
            [{"data": self.cars}],
        ]:
            bundle = copy.deepcopy(self.bundle)
            bundle["pages"] = pages
            result = self.read(bundle)
            self.assertEqual(result["current"]["median_files"]["state"], "unknown")
            self.assertIsNone(result["current"]["median_files"]["value"])

    def test_regate_takes_precedence_and_complete_two_pages_are_measured(self):
        bundle = copy.deepcopy(self.bundle)
        bundle["cars"][0]["steps"][0]["metadata"]["receipt"] = '{"head":"old","verdict":"red"}'
        bundle["cars"][0]["metadata"]["regate_receipt"] = json.dumps({"head": self.a, "verdict": "green"})
        bundle["pages"] = [{"total": 2, "data": [bundle["cars"][0]]}, {"total": 2, "data": [bundle["cars"][1]]}]
        self.assertEqual(self.read(bundle)["current"]["coverage"]["measured_cars"], 2)

    def test_rename_binary_zero_adds_and_previous_week_are_explicit(self):
        self.git("checkout", "-q", "-b", "car-c", self.merge)
        self.git("mv", "extra.txt", "renamed.txt")
        self.write("binary.dat", b"\x00\x01")
        self.commit("source c", "2026-09-28T10:00:00Z")
        head = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-q", "main")
        self.git("merge", "--squash", "car-c")
        cid = "55555555-5555-4555-8555-555555555555"
        self.commit(f"train: fixture (1 changes) (#2)\n\ncar-c — synthetic [ship-a-change {cid[:8]}, no item: fixture]", "2026-09-29T12:00:00Z")
        merge = self.git("rev-parse", "HEAD").strip()
        bundle = copy.deepcopy(self.bundle)
        car = self.car(cid, head)
        car["metadata"]["merge_ref"] = merge[:12]
        tid = "66666666-6666-4666-8666-666666666666"
        car["metadata"]["train"] = tid
        bundle["cars"].append(car)
        bundle["pages"] = [{"total": 3, "data": bundle["cars"]}]
        bundle["trains"].append({"id": tid, "kind": "pr-train", "metadata": {"boarded_jobs": [cid]}, "steps": [{"spec_slug": "merged", "status": "completed", "metadata": {"merge_ref": merge[:12]}}]})
        result = self.read(bundle, "2026-09-28", "2026-10-05")
        self.assertEqual(result["previous"]["cars_landed"]["value"], 2)
        self.assertEqual(result["current"]["median_files"]["value"], 2)
        self.assertEqual(result["current"]["median_adds"]["state"], "unknown")
        self.assertEqual(result["current"]["core_share"]["value"], 0)
        self.assertEqual(result["trend"]["median_adds"]["state"], "unknown")
        # Removing the binary from the actual source produces a measured
        # rename-only zero-add reading, whose ratio is undefined, not zero.
        self.git("checkout", "-q", "car-c")
        self.git("rm", "binary.dat")
        self.commit("remove fixture binary", "2026-09-28T11:00:00Z")
        pure_head = self.git("rev-parse", "HEAD").strip()
        car["metadata"]["boarded_head"] = pure_head
        car["steps"][0]["metadata"]["receipt"] = json.dumps({"head": pure_head, "verdict": "green"})
        bundle["cars"][-1] = car
        bundle["pages"][0]["data"] = bundle["cars"]
        week = self.read(bundle, "2026-09-28", "2026-10-05")["current"]
        self.assertEqual(week["median_adds"]["value"], 0)
        self.assertEqual(week["removes_adds_ratio"]["state"], "undefined")

    def test_bad_ref_or_window_is_not_an_empty_week(self):
        with self.assertRaises(ValueError): self.read(since="2026-09-28", until="2026-09-21")
        with self.assertRaises(ValueError):
            self.reader.measure(str(self.repo), "missing", "2026-09-21", "2026-09-28", self.bundle)

    def test_shallow_history_is_not_an_empty_previous_week(self):
        shallow = self.repo / "shallow"
        out = subprocess.run(["git", "clone", "-q", "--depth=1", self.repo.as_uri(), str(shallow)], capture_output=True)
        self.assertEqual(out.returncode, 0, out.stderr)
        with self.assertRaisesRegex(ValueError, "shallow"):
            self.reader.measure(str(shallow), "main", "2026-09-21", "2026-09-28", self.bundle)

    def test_missing_git_object_has_a_specific_unknown_reason(self):
        bundle = copy.deepcopy(self.bundle)
        missing = "f" * 40
        bundle["cars"][0]["metadata"]["boarded_head"] = missing
        bundle["cars"][0]["steps"][0]["metadata"]["receipt"] = json.dumps({"head": missing, "verdict": "green"})
        week = self.read(bundle)["current"]
        self.assertEqual(week["median_files"]["state"], "unknown")
        self.assertIn("Git read failed", week["cars"][0]["reason"])

    def test_the_supported_collector_conserves_actual_pages_and_records(self):
        paths = []
        def read(path):
            paths.append(path)
            if path.startswith("/api/jobs?"):
                offset = int(path.split("offset=", 1)[1])
                return {"total": 2, "data": [self.cars[offset]]}
            records = {c["id"]: c for c in self.cars}
            records[self.tid] = self.train
            return records[path.rsplit("/", 1)[1]]
        bundle = self.reader.collect(str(self.repo), "main", "2026-09-21", "2026-09-28", read=read)
        self.assertEqual(self.read(bundle)["current"]["coverage"]["measured_cars"], 2)
        self.assertEqual(len(paths), 5)
        self.assertIn("offset=1", paths[1])

    def test_collector_refuses_a_dark_or_changing_listing(self):
        for answer in [{"total": 2, "data": []}, {"data": self.cars}]:
            with self.assertRaises(ValueError):
                self.reader.collect(str(self.repo), "main", "2026-09-21", "2026-09-28", read=lambda _: answer)

    def test_malformed_native_fields_are_unknown_rather_than_exceptions(self):
        for mutate in [
            lambda b: b["cars"][0].update(metadata=None),
            lambda b: b["trains"][0].update(metadata=[]),
            lambda b: b["cars"][0].update(steps=[None]),
            lambda b: b["cars"][0]["steps"][0].update(metadata=False),
        ]:
            bundle = copy.deepcopy(self.bundle)
            mutate(bundle)
            self.assertEqual(self.read(bundle)["current"]["median_files"]["state"], "unknown")


unittest.main()
