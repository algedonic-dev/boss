"""The artifact inventory's causal tests (backlog 25022367, design 195276f0).

Every fixture lives under BOSS_TEST_SCRATCH (boss_testing::scratch_dir,
uid and pid in the path), and the process table is a FAKE one the test
builds, so a live writer, an unreadable process and a link are planted
facts rather than whatever the host happens to be running. One test
reads the real kernel's table for its own open descriptor.
"""
import hashlib
import importlib.util
import io
import json
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("inventory", HERE / "dev-artifact-inventory.py")
inv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inv)

NOW = 2_000_000_000.0
DAY = 86400

POLICY = """version = 1
[[family]]
name = "evidence"
root = "home/agent/scratch"
owner = "agent-one"
unit_depth = 1
settle_hours = 24
reader = "plain-path"
reader_evidence = "fixture"

[[family]]
name = "transcripts"
root = "home/agent/sessions"
owner = "agent-two"
unit_depth = 1
settle_hours = 24
reader = "unmeasured"
reader_evidence = "fixture"
"""


def fixture_root():
    base = os.environ.get("BOSS_TEST_SCRATCH")
    return Path(tempfile.mkdtemp(prefix=f"artifact-inventory-{os.getuid()}-{os.getpid()}-", dir=base))


def plant(path, data, age_s):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    os.utime(path, (NOW - age_s, NOW - age_s))
    return path


def fake_proc(root, holders=(), unreadable=()):
    """A process table: pid 1 is the pod's pause; `holders` are
    (pid, path) pairs whose fd/3 points at path; `unreadable` pids have
    an fd entry the reader cannot list."""
    proc = root / "proc"
    (proc / "1" / "fd").mkdir(parents=True)
    (proc / "1" / "comm").write_text("pause\n")
    for pid, target in holders:
        (proc / str(pid) / "fd").mkdir(parents=True)
        (proc / str(pid) / "comm").write_text("writer\n")
        (proc / str(pid) / "fd" / "3").symlink_to(target)
    for pid in unreadable:
        (proc / str(pid)).mkdir(parents=True)
        (proc / str(pid) / "comm").write_text("stranger\n")
        # A file where the directory should be: listing it fails the way
        # a permission refusal does, whichever uid runs the test.
        (proc / str(pid) / "fd").write_text("")
    return proc


def tree_state(root):
    """Every path under root with its bytes, mode and times: what a dry
    run must leave exactly as it found it."""
    out = {}
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in sorted(dirnames + filenames):
            p = Path(dirpath) / name
            st = os.lstat(p)
            digest = None
            if p.is_file() and not p.is_symlink():
                digest = hashlib.sha256(p.read_bytes()).hexdigest()
            out[str(p.relative_to(root))] = (st.st_mode, st.st_size, st.st_mtime_ns, st.st_ino, digest)
    return out


def write_policy(root, text=POLICY):
    path = root / "policy.toml"
    path.write_text(text)
    return path


def family(report, name):
    return next(f for f in report["families"] if f["name"] == name)


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.root = fixture_root()

    def refused(self, text, needle):
        with self.assertRaises(inv.PolicyError) as caught:
            inv.load_policy(write_policy(self.root, text))
        self.assertIn(needle, str(caught.exception))

    def test_the_tree_policy_loads_and_is_inventory_only(self):
        policy = inv.load_policy(HERE / "artifact-lifecycle.toml")
        self.assertEqual(policy["version"], 1)
        names = [f["name"] for f in policy["families"]]
        self.assertEqual(names, ["codex-scratch", "codex-sessions", "claude-projects"])
        for f in policy["families"]:
            self.assertTrue(f["reader_evidence"].strip(), f["name"])
            self.assertTrue(f["owner"].strip(), f["name"])

    def test_an_action_key_is_refused_because_version_one_grants_none(self):
        self.refused(POLICY.replace('reader = "plain-path"', 'reader = "plain-path"\narchive = true'),
                     "unknown key 'archive'")
        self.refused(POLICY + "\nexpire_days = 3\n", "unknown key 'expire_days'")

    def test_another_version_is_refused(self):
        self.refused(POLICY.replace("version = 1", "version = 2"), "version 2")

    def test_a_root_that_escapes_or_is_absolute_is_refused(self):
        self.refused(POLICY.replace('"home/agent/scratch"', '"/etc"'), "relative")
        self.refused(POLICY.replace('"home/agent/scratch"', '"home/../../etc"'), "'..'")

    def test_overlapping_roots_are_refused_so_a_byte_is_counted_once(self):
        self.refused(POLICY.replace('"home/agent/sessions"', '"home/agent/scratch/inner"'), "inside")

    def test_duplicate_names_missing_fields_and_unknown_readers_are_refused(self):
        self.refused(POLICY.replace('name = "transcripts"', 'name = "evidence"'), "twice")
        self.refused(POLICY.replace('owner = "agent-two"\n', ""), "missing 'owner'")
        self.refused(POLICY.replace('reader = "unmeasured"', 'reader = "trusted"'), "reader")


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.root = fixture_root()
        self.work = self.root / "work"
        self.scratch = self.work / "home/agent/scratch"
        self.sessions = self.work / "home/agent/sessions"
        self.policy = inv.load_policy(write_policy(self.root))

    def run_inventory(self, proc):
        return inv.run_inventory(self.policy, work_mount=self.work, proc_root=proc, now=NOW, reserve_bytes=0)

    def test_every_file_is_retained_with_the_reason_that_holds_it(self):
        held = plant(self.scratch / "run-a/live.log", b"x" * 100, 3 * DAY)
        plant(self.scratch / "run-a/fresh.json", b"y" * 10, 60)
        plant(self.scratch / "run-b/old.json", b"z" * 1000, 9 * DAY)
        plant(self.sessions / "s1/t.jsonl", b"t" * 50, 9 * DAY)
        proc = fake_proc(self.root, holders=[(42, held)])
        report = self.run_inventory(proc)

        evidence = family(report, "evidence")
        reasons = evidence["by_primary_reason"]
        self.assertEqual(reasons["open-writer"]["files"], 1)
        self.assertEqual(reasons["recently-changed"]["files"], 1)
        self.assertEqual(reasons["completion-unproven"]["files"], 1)
        self.assertEqual(family(report, "transcripts")["by_primary_reason"]["reader-unqualified"]["files"], 1)
        unit = next(u for u in evidence["units"] if u["unit"] == "run-a")
        self.assertEqual(unit["open_files"], 1)
        self.assertEqual(unit["writers"], [{"pid": 42, "comm": "writer"}])
        self.assertEqual(evidence["owner"], "agent-one")
        # Version 1 joins no completion record, so nothing is a candidate.
        self.assertEqual(report["candidates"], [])
        self.assertEqual(report["candidate_bytes"], 0)
        self.assertFalse(report["mutated"])

    def test_the_inventory_changes_nothing_it_reads(self):
        plant(self.scratch / "run-a/a.json", b"a" * 4096, 9 * DAY)
        plant(self.sessions / "s/b.jsonl", b"b" * 10, 2 * DAY)
        outside = plant(self.root / "outside/secret", b"not ours", 9 * DAY)
        (self.scratch / "run-a/link").symlink_to(outside)
        before = tree_state(self.root)
        self.run_inventory(fake_proc(self.root))
        proc_paths = {k for k in tree_state(self.root) if k.startswith("proc")}
        after = {k: v for k, v in tree_state(self.root).items() if k not in proc_paths}
        before = {k: v for k, v in before.items() if k not in proc_paths}
        self.assertEqual(before, after)

    def test_a_link_is_never_followed_and_its_target_is_never_counted(self):
        outside = plant(self.root / "outside/big", b"o" * 50000, 9 * DAY)
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        (self.scratch / "run-a/link").symlink_to(outside)
        (self.scratch / "linked-dir").symlink_to(self.root / "outside", target_is_directory=True)
        report = self.run_inventory(fake_proc(self.root))
        evidence = family(report, "evidence")
        self.assertEqual(evidence["logical_bytes"], 10)
        self.assertEqual(evidence["by_primary_reason"]["symlink-not-followed"]["files"], 2)
        self.assertEqual(evidence["by_primary_reason"]["symlink-not-followed"]["allocated_bytes"], 0)

    def test_a_linked_root_or_ancestor_is_refused_not_followed(self):
        real = self.root / "elsewhere"
        plant(real / "x.json", b"x" * 10, 9 * DAY)
        self.sessions.parent.mkdir(parents=True, exist_ok=True)
        self.sessions.symlink_to(real, target_is_directory=True)
        report = self.run_inventory(fake_proc(self.root))
        transcripts = family(report, "transcripts")
        self.assertEqual(transcripts["status"], "refused")
        self.assertIn("symbolic link", transcripts["refusal"])
        self.assertEqual(transcripts["files"], 0)

    def test_an_absent_root_is_reported_absent(self):
        report = self.run_inventory(fake_proc(self.root))
        self.assertEqual(family(report, "evidence")["status"], "absent")

    def test_an_unreadable_process_makes_liveness_unknown_and_names_it(self):
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        report = self.run_inventory(fake_proc(self.root, unreadable=[77]))
        self.assertEqual(report["liveness"]["unreadable_processes"], 1)
        self.assertEqual(report["liveness"]["unreadable_sample"], [{"pid": 77, "comm": "stranger"}])
        self.assertEqual(report["uncertainty"]["liveness_unknown_files"], 1)
        self.assertEqual(report["candidates"], [])

    def test_a_missing_process_table_is_unknown_liveness_not_an_empty_one(self):
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        report = self.run_inventory(self.root / "no-proc")
        self.assertFalse(report["liveness"]["table_read"])
        self.assertEqual(report["uncertainty"]["liveness_unknown_files"], 1)

    def test_an_unreadable_directory_is_uncertainty_with_its_path(self):
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        plant(self.scratch / "run-b/b.json", b"b" * 10, 9 * DAY)
        refused = str(self.scratch / "run-b")
        real_scandir = os.scandir

        def scandir(path):
            if str(path) == refused:
                raise PermissionError(13, "Permission denied", str(path))
            return real_scandir(path)

        with patch.object(inv.os, "scandir", scandir):
            report = self.run_inventory(fake_proc(self.root))
        self.assertEqual(report["uncertainty"]["unreadable_dirs"], 1)
        self.assertEqual(report["uncertainty"]["unreadable_sample"][0]["path"], "home/agent/scratch/run-b")
        self.assertEqual(family(report, "evidence")["files"], 1)

    def test_bytes_are_conserved_and_a_hardlink_is_allocated_once(self):
        a = plant(self.scratch / "run-a/a.bin", os.urandom(8192), 9 * DAY)
        os.link(a, self.scratch / "run-a/a-again.bin")
        plant(self.sessions / "s/t.jsonl", os.urandom(4096), 9 * DAY)
        report = self.run_inventory(fake_proc(self.root))
        evidence = family(report, "evidence")
        self.assertEqual(evidence["files"], 2)
        self.assertEqual(evidence["logical_bytes"], 16384)
        self.assertEqual(evidence["hardlinked_paths"], 1)
        self.assertEqual(evidence["allocated_bytes"], os.stat(a).st_blocks * 512)
        for f in report["families"]:
            self.assertEqual(sum(r["files"] for r in f["by_primary_reason"].values()), f["files"] + f["links"])
            self.assertEqual(sum(r["allocated_bytes"] for r in f["by_primary_reason"].values()), f["allocated_bytes"])
            self.assertEqual(sum(u["allocated_bytes"] for u in f["units"]) + f["units_omitted"]["allocated_bytes"],
                             f["allocated_bytes"])
        self.assertEqual(report["totals"]["allocated_bytes"],
                         sum(f["allocated_bytes"] for f in report["families"]))

    def test_the_totals_conserve_files_with_their_links_named(self):
        # Backlog ea54c3ff (review run 86515edf, 2026-10-18): a link is a row of
        # by_primary_reason and of its family's `links`, never of `files`, and the
        # totals carried no `links` — so on the dev pod the per-reason files summed
        # to 169,921 against totals.files 167,383, the 2,538 links named nowhere.
        outside = plant(self.root / "outside/big", b"o" * 500, 9 * DAY)
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        (self.scratch / "run-a/link").symlink_to(outside)
        plant(self.sessions / "s/t.jsonl", b"t" * 10, 9 * DAY)
        (self.sessions / "s/link").symlink_to(outside)
        os.mkfifo(self.sessions / "s/pipe")
        report = self.run_inventory(fake_proc(self.root))
        totals = report["totals"]
        self.assertEqual(totals["links"], 3)
        self.assertEqual(totals["links"], sum(f["links"] for f in report["families"]))
        self.assertEqual(sum(r["files"] for r in totals["by_primary_reason"].values()),
                         totals["files"] + totals["links"])

    def test_units_are_bounded_and_the_omitted_ones_still_counted(self):
        for i in range(5):
            plant(self.scratch / f"run-{i}/f.bin", b"q" * (1000 * (i + 1)), 9 * DAY)
        report = inv.run_inventory(self.policy, work_mount=self.work, proc_root=fake_proc(self.root),
                                   now=NOW, reserve_bytes=0, unit_limit=2)
        evidence = family(report, "evidence")
        self.assertEqual([u["unit"] for u in evidence["units"]], ["run-4", "run-3"])
        self.assertEqual(evidence["units_omitted"]["units"], 3)
        self.assertEqual(evidence["units_omitted"]["files"], 3)

    def test_the_same_tree_gives_the_same_report(self):
        plant(self.scratch / "run-a/a.json", b"a" * 10, 9 * DAY)
        plant(self.sessions / "s/t.jsonl", b"t" * 10, 9 * DAY)
        proc = fake_proc(self.root)
        first, second = self.run_inventory(proc), self.run_inventory(proc)
        first.pop("mount"), second.pop("mount")
        self.assertEqual(json.dumps(first, sort_keys=True), json.dumps(second, sort_keys=True))

    def test_the_real_kernel_names_this_process_as_the_writer(self):
        live = plant(self.scratch / "run-a/live.log", b"l" * 10, 9 * DAY)
        with open(live, "ab"):
            report = inv.run_inventory(self.policy, work_mount=self.work, proc_root=Path("/proc"),
                                       now=NOW, reserve_bytes=0)
        unit = next(u for u in family(report, "evidence")["units"] if u["unit"] == "run-a")
        self.assertEqual(unit["primary_reasons"], {"open-writer": 1})
        self.assertIn(os.getpid(), [w["pid"] for w in unit["writers"]])


class MainTests(unittest.TestCase):
    def setUp(self):
        self.root = fixture_root()
        self.work = self.root / "work"
        plant(self.work / "home/agent/scratch/run-a/a.json", b"a" * 10, 9 * DAY)
        self.policy = write_policy(self.root)
        self.out = self.root / "telemetry" / "artifact-inventory"

    def main(self, *extra):
        argv = ["--policy", str(self.policy), "--work-mount", str(self.work),
                "--proc-root", str(fake_proc(self.root) if not (self.root / "proc").exists() else self.root / "proc"),
                "--now", str(int(NOW)), "--reserve-gb", "1",
                "--report", str(self.out / "latest.json"), "--ring", str(self.out / "growth.jsonl"), *extra]
        buf = io.StringIO()
        with redirect_stdout(buf):
            code = inv.main(argv)
        return code, buf.getvalue()

    def test_the_report_is_written_whole_and_the_ring_is_bounded(self):
        for _ in range(3):
            code, said = self.main("--ring-keep", "2")
            self.assertEqual(code, 0, said)
        report = json.loads((self.out / "latest.json").read_text())
        self.assertEqual(report["schema"], "artifact-inventory/1")
        self.assertEqual(report["mount"]["reserve_bytes"], 1 << 30)
        ring = (self.out / "growth.jsonl").read_text().splitlines()
        self.assertEqual(len(ring), 2)
        self.assertEqual(json.loads(ring[-1])["allocated_bytes"], report["totals"]["allocated_bytes"])
        self.assertEqual(sorted(p.name for p in self.out.iterdir()), ["growth.jsonl", "latest.json"])
        self.assertIn("0 candidates", said)
        self.assertIn("inventory only", said)

    def test_a_refused_policy_exits_64_and_writes_nothing(self):
        self.policy.write_text(POLICY + "\narchive = true\n")
        code, said = self.main()
        self.assertEqual(code, 64)
        self.assertIn("unknown key 'archive'", said)
        self.assertFalse(self.out.exists())

    def test_an_unwritable_report_exits_73_and_says_where(self):
        self.root.joinpath("telemetry").write_text("a file where the directory should be")
        code, said = self.main()
        self.assertEqual(code, 73)
        self.assertIn("latest.json", said)


if __name__ == "__main__":
    unittest.main()
