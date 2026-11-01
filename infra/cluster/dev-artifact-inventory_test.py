"""The artifact inventory's causal tests (backlog 25022367, design 195276f0).

Every fixture lives under BOSS_TEST_SCRATCH (boss_testing::scratch_dir,
uid and pid in the path), and the process table is a FAKE one the test
builds, so a live writer, an unreadable process and a link are planted
facts rather than whatever the host happens to be running. One test
reads the real kernel's table for its own open descriptor.
"""
import fcntl
import hashlib
import importlib.util
import io
import json
import os
import stat
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlparse

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

# Version 2: the evidence family is joined to agent runs and may be
# archived; the transcript family is joined but its reader is unmeasured,
# so it may never be. A unit is a window and the run directory inside it,
# the shape of the live Codex scratch.
POLICY_V2 = """version = 2

[[family]]
name = "evidence"
root = "home/agent/scratch"
owner = "agent-one"
unit_depth = 2
settle_hours = 24
reader = "plain-path"
reader_evidence = "fixture"
completion = "agent-run"
action = "archive"
archive_budget_mb = 1

[[family]]
name = "transcripts"
root = "home/agent/sessions"
owner = "agent-two"
unit_depth = 2
settle_hours = 24
reader = "unmeasured"
reader_evidence = "fixture"
completion = "agent-run"
"""

RUN = "0000aaaa"
OTHER = "0000bbbb"


class FakeRecord:
    """The system of record as the lifecycle reads it: agent runs by id
    prefix and the non-terminal packets, paged. `fail` names reads that
    answer nothing ('open', or 'run:<id8>'); `on_run_read` is called with
    the id each time a run is read, so a test can change the tree at the
    exact moment the act phase re-reads its owner (a race, planted)."""

    def __init__(self, runs=None, open_records=(), fail=(), on_run_read=None):
        self.runs = dict(runs or {})
        self.open_records = list(open_records)
        self.fail = set(fail)
        self.on_run_read = on_run_read
        self.calls = []

    def __call__(self, path):
        self.calls.append(path)
        if path.startswith("/api/jobs?"):
            if "open" in self.fail:
                return None, None
            query = parse_qs(urlparse(path).query)
            self.last_query = query
            offset, limit = int(query.get("offset", ["0"])[0]), int(query["limit"][0])
            return 200, {"total": len(self.open_records), "data": self.open_records[offset:offset + limit]}
        id8 = path.rsplit("/", 1)[1]
        if self.on_run_read:
            self.on_run_read(id8)
        if f"run:{id8}" in self.fail:
            return None, None
        run = self.runs.get(id8)
        if run is None:
            return 404, {"error": "no such job"}
        steps = [{"spec_slug": "building", "status": "completed"},
                 {"spec_slug": "reported", "status": "completed" if run.get("reported") else "pending",
                  "completed_at": "2026-10-10T00:00:00Z" if run.get("reported") else None}]
        return 200, {"id": f"{id8}-1111-2222-3333-444455556666", "kind": run.get("kind", "agent-run"),
                     "status": run["status"], "steps": steps}

    def run_reads(self):
        return [c for c in self.calls if not c.startswith("/api/jobs?")]

    def open_reads(self):
        return [c for c in self.calls if c.startswith("/api/jobs?")]


def closed(reported=True):
    return {"status": "closed", "reported": reported}


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

    def test_the_tree_policy_loads_joins_and_grants_no_action(self):
        # Version 2 adds the completion join and the archive action. The
        # tree's policy JOINS the one family whose reader is measured and
        # grants NO action: activation is its own held car after native
        # acceptance (design 195276f0).
        policy = inv.load_policy(HERE / "artifact-lifecycle.toml")
        self.assertEqual(policy["version"], 2)
        names = [f["name"] for f in policy["families"]]
        self.assertEqual(names, ["codex-scratch", "codex-sessions", "claude-projects"])
        for f in policy["families"]:
            self.assertTrue(f["reader_evidence"].strip(), f["name"])
            self.assertTrue(f["owner"].strip(), f["name"])
            self.assertIsNone(f.get("action"), f["name"])
        joined = {f["name"]: f.get("completion") for f in policy["families"]}
        self.assertEqual(joined, {"codex-scratch": "agent-run", "codex-sessions": None, "claude-projects": None})

    def test_an_action_key_is_refused_because_version_one_grants_none(self):
        self.refused(POLICY.replace('reader = "plain-path"', 'reader = "plain-path"\narchive = true'),
                     "unknown key 'archive'")
        self.refused(POLICY + "\nexpire_days = 3\n", "unknown key 'expire_days'")

    def test_another_version_is_refused(self):
        self.refused(POLICY.replace("version = 1", "version = 3"), "version 3")

    def test_version_one_refuses_the_join_and_the_action(self):
        self.refused(POLICY.replace('reader = "plain-path"', 'reader = "plain-path"\ncompletion = "agent-run"'),
                     "unknown key 'completion'")
        self.refused(POLICY.replace('reader = "plain-path"', 'reader = "plain-path"\naction = "archive"'),
                     "unknown key 'action'")

    def test_version_two_grants_an_action_only_where_a_reader_is_measured_and_a_join_proves_completion(self):
        self.assertEqual(inv.load_policy(write_policy(self.root, POLICY_V2))["version"], 2)
        # An unmeasured reader (a transcript store) may never be archived.
        # (The transcripts family is the policy's last, so this lands in it.)
        self.refused(POLICY_V2 + 'action = "archive"\narchive_budget_mb = 1\n', "unmeasured")
        # No join, no action: age alone never proves completion.
        self.refused(POLICY_V2.replace('completion = "agent-run"\naction', 'action'), "completion")
        # A budget belongs to an action, and an action has one.
        self.refused(POLICY_V2.replace('action = "archive"\n', ""), "archive_budget_mb")
        self.refused(POLICY_V2.replace("archive_budget_mb = 1\n", ""), "archive_budget_mb")
        self.refused(POLICY_V2.replace('action = "archive"', 'action = "expire"'), "action")
        self.refused(POLICY_V2.replace('completion = "agent-run"', 'completion = "age"', 1), "completion")

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


def snapshot_bytes(n=64 * 1024):
    """A coordinator snapshot's shape: JSON, highly compressible, big
    enough that its archive frees whole blocks."""
    rows = [{"id": i, "status": "closed", "note": "the same evidence line"} for i in range(n // 60)]
    return json.dumps({"rows": rows}).encode()


def manifest_entries(path):
    try:
        return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]
    except FileNotFoundError:
        return []


class LifecycleCase(unittest.TestCase):
    """A fixture work mount under policy version 2, a complete fake
    process table, and a manifest beside the report, as the hourly pass
    names it."""

    def setUp(self):
        self.root = fixture_root()
        self.work = self.root / "work"
        self.scratch = self.work / "home/agent/scratch"
        self.sessions = self.work / "home/agent/sessions"
        self.policy = inv.load_policy(write_policy(self.root, POLICY_V2))
        self.manifest = self.root / "telemetry" / "archive-manifest.jsonl"
        self.proc = fake_proc(self.root)

    def snapshot(self, run=RUN, name="snap.json", data=None, age=9 * DAY, window="window-1"):
        return plant(self.scratch / window / f"run-{run}" / name, data if data is not None else snapshot_bytes(), age)

    def run_pass(self, record, act=True, proc=None, **extra):
        return inv.run_inventory(self.policy, work_mount=self.work, proc_root=proc or self.proc, now=NOW,
                                 reserve_bytes=0, record=record, manifest=self.manifest, act=act, **extra)

    def unit(self, report, fam, name):
        return next(u for u in family(report, fam)["units"] if u["unit"] == name)

    def untouched(self, before):
        after = {k: v for k, v in tree_state(self.work).items()}
        self.assertEqual(before, after)

    def files_untouched(self, before):
        """Every path still there and every file byte-identical; a
        directory's own mtime may move when a refused temporary was
        written and removed inside it."""
        after = tree_state(self.work)
        self.assertEqual(sorted(before), sorted(after))
        files = {k: v for k, v in before.items() if not stat.S_ISDIR(v[0])}
        self.assertEqual(files, {k: v for k, v in after.items() if k in files})


class JoinTests(LifecycleCase):
    """The completion join: a unit is complete only when the record holds
    its run CLOSED with its report recorded and no open packet names it.
    Every other answer retains and names the owner who can resolve it."""

    def test_a_dry_run_names_the_candidate_and_moves_nothing(self):
        snap = self.snapshot()
        before = tree_state(self.work)
        record = FakeRecord(runs={RUN: closed()})
        report = self.run_pass(record, act=False)
        self.untouched(before)
        self.assertFalse(report["mutated"])
        self.assertEqual(report["mode"], "dry-run")
        self.assertEqual([c["unit"] for c in report["candidates"]], [f"window-1/run-{RUN}"])
        self.assertEqual(report["candidates"][0]["run"], RUN)
        self.assertEqual(report["candidate_bytes"], os.stat(snap).st_blocks * 512)
        unit = self.unit(report, "evidence", f"window-1/run-{RUN}")
        self.assertEqual(unit["join"]["reason"], "candidate")
        self.assertEqual(unit["primary_reasons"], {"candidate": 1})

    def test_an_open_run_keeps_its_evidence_and_names_the_run_and_its_owner(self):
        self.snapshot()
        before = tree_state(self.work)
        report = self.run_pass(FakeRecord(runs={RUN: {"status": "open"}}))
        self.untouched(before)
        unit = self.unit(report, "evidence", f"window-1/run-{RUN}")
        self.assertEqual(unit["join"]["reason"], "run-open")
        self.assertEqual(unit["join"]["record"]["status"], "open")
        self.assertEqual(family(report, "evidence")["owner"], "agent-one")
        self.assertEqual(report["candidates"], [])

    def test_a_closed_run_whose_report_was_never_recorded_keeps_its_evidence(self):
        self.snapshot()
        before = tree_state(self.work)
        report = self.run_pass(FakeRecord(runs={RUN: closed(reported=False)}))
        self.untouched(before)
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]["reason"], "run-unreported")

    def test_evidence_an_open_packet_names_is_kept_and_the_packet_is_named(self):
        self.snapshot()
        before = tree_state(self.work)
        incident = {"id": "incident-1", "title": "disk incident", "metadata": {
            "evidence": f"/work/home/agent/scratch/window-1/run-{RUN}/snap.json holds the measurement"}}
        record = FakeRecord(runs={RUN: closed()}, open_records=[{"id": "noise", "metadata": {}}, incident])
        report = self.run_pass(record)
        self.untouched(before)
        join = self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]
        self.assertEqual(join["reason"], "referenced-by-open-record")
        self.assertEqual(join["referenced_by"], ["incident-1"])
        # The open set is read whole, step metadata included.
        self.assertEqual(record.last_query["full"], ["true"])
        self.assertEqual(record.last_query["terminal"], ["false"])

    def test_an_open_packet_naming_the_run_id_alone_also_holds_it(self):
        self.snapshot()
        record = FakeRecord(runs={RUN: closed()}, open_records=[{"id": "gate-1", "metadata": {"agent_run": RUN}}])
        report = self.run_pass(record, act=False)
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]["referenced_by"], ["gate-1"])

    def test_an_owner_the_record_does_not_hold_or_cannot_answer_retains(self):
        self.snapshot(run=RUN)
        self.snapshot(run=OTHER)
        plant(self.scratch / "window-1/coordinator/state.json", snapshot_bytes(), 9 * DAY)
        before = tree_state(self.work)
        record = FakeRecord(runs={}, fail={f"run:{OTHER}"})
        report = self.run_pass(record)
        self.untouched(before)
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]["reason"], "owner-unknown")
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{OTHER}")["join"]["reason"], "owner-unread")
        self.assertEqual(self.unit(report, "evidence", "window-1/coordinator")["join"]["reason"], "owner-unjoined")
        # Nothing completed, so the open set was never fetched.
        self.assertEqual(record.open_reads(), [])

    def test_an_unreadable_open_set_retains_every_completed_unit(self):
        self.snapshot()
        before = tree_state(self.work)
        report = self.run_pass(FakeRecord(runs={RUN: closed()}, fail={"open"}))
        self.untouched(before)
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]["reason"], "owner-unread")

    def test_no_system_of_record_named_retains_everything(self):
        self.snapshot()
        before = tree_state(self.work)
        report = self.run_pass(None)
        self.untouched(before)
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["join"]["reason"], "owner-unread")

    def test_unknown_liveness_reads_no_record_and_archives_nothing(self):
        self.snapshot()
        before = tree_state(self.work)
        record = FakeRecord(runs={RUN: closed()})
        report = self.run_pass(record, proc=fake_proc(self.root / "p2", unreadable=[77]))
        self.untouched(before)
        self.assertEqual(record.calls, [])
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["primary_reasons"],
                         {"liveness-unknown": 1})

    def test_record_reads_are_bounded_and_the_rest_retained(self):
        for run in ("0000aaa1", "0000aaa2", "0000aaa3"):
            self.snapshot(run=run)
        record = FakeRecord(runs={r: closed() for r in ("0000aaa1", "0000aaa2", "0000aaa3")})
        report = self.run_pass(record, act=False, max_reads=2)
        self.assertEqual(len(record.run_reads()), 2)
        reasons = sorted(u["join"]["reason"] for u in family(report, "evidence")["units"])
        self.assertEqual(reasons, ["candidate", "candidate", "owner-unread"])


class ArchiveTests(LifecycleCase):
    """The actuator: an eligible completed snapshot is archived, its plain
    copy released only after the archive decodes to the same bytes and the
    manifest is durable, and the restore door gives the reader back the
    exact file."""

    def test_restore_writes_every_byte_when_the_kernel_returns_short_counts(self):
        data = snapshot_bytes() * 5
        snap = self.snapshot(data=data)
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        write = os.write
        counts = []

        def short_write(fd, chunk):
            if ".restore-tmp-" not in os.readlink(f"/proc/self/fd/{fd}"):
                return write(fd, chunk)
            n = write(fd, chunk[:max(1, len(chunk) // 2)])
            counts.append((len(chunk), n))
            return n

        with patch.object(inv.os, "write", side_effect=short_write):
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertTrue(any(n < offered for offered, n in counts))
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "restored")

    def test_restore_verifies_the_actual_temporary_before_installing_or_releasing(self):
        snap = self.snapshot()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        original_archive = archive.read_bytes()
        write = os.write

        def corrupted_write(fd, chunk):
            if ".restore-tmp-" not in os.readlink(f"/proc/self/fd/{fd}"):
                return write(fd, chunk)
            return write(fd, b"X" + bytes(chunk[1:]))

        with patch.object(inv.os, "write", side_effect=corrupted_write):
            with self.assertRaisesRegex(inv.LifecycleError, "temporary.*manifest"):
                inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertFalse(os.path.lexists(snap))
        self.assertEqual(archive.read_bytes(), original_archive)
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "released")

    def test_restore_refuses_a_zero_write_without_losing_the_archive(self):
        snap = self.snapshot()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        write = os.write

        def zero_write(fd, chunk):
            if ".restore-tmp-" in os.readlink(f"/proc/self/fd/{fd}"):
                return 0
            return write(fd, chunk)

        with patch.object(inv.os, "write", side_effect=zero_write):
            with self.assertRaises(inv.LifecycleError):
                inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertFalse(os.path.lexists(snap))
        self.assertTrue(archive.is_file())
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "released")

    def test_a_kernel_file_size_limit_preserves_the_only_archive(self):
        import resource
        import signal

        self.policy = inv.load_policy(write_policy(
            self.root, POLICY_V2.replace("archive_budget_mb = 1", "archive_budget_mb = 4")))
        snap = self.snapshot(data=snapshot_bytes() * 24)
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        original = archive.read_bytes()
        child = os.fork()
        if child == 0:
            signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
            resource.setrlimit(resource.RLIMIT_FSIZE, (1280 * 1024, 1280 * 1024))
            try:
                inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
            except inv.LifecycleError:
                os._exit(0)
            os._exit(17)
        _, status = os.waitpid(child, 0)
        self.assertEqual(os.waitstatus_to_exitcode(status), 0,
                         "the real kernel short write must refuse restoration")
        self.assertFalse(os.path.lexists(snap))
        self.assertEqual(archive.read_bytes(), original)
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "released")

    def test_restore_keeps_archived_access_time_after_its_verification_read(self):
        snap = self.snapshot()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        archived = manifest_entries(self.manifest)[0]
        inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        # Inspect metadata BEFORE the real consumer reads and changes atime.
        restored = os.stat(snap)
        self.assertEqual(restored.st_atime_ns, archived["atime_ns"])
        self.assertEqual(restored.st_mtime_ns, archived["mtime_ns"])

    def test_a_completed_snapshot_is_archived_recorded_and_restored_for_its_real_reader(self):
        data = snapshot_bytes()
        snap = self.snapshot(data=data)
        os.chmod(snap, 0o640)
        st = os.stat(snap)
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        self.assertFalse(os.path.lexists(snap), "the plain copy is released")
        self.assertTrue(archive.is_file())
        self.assertTrue(report["mutated"])
        act = report["act"]
        self.assertEqual(act["archived_files"], 1)
        self.assertEqual(act["logical_bytes"], len(data))
        self.assertEqual(act["allocated_bytes_before"], st.st_blocks * 512)
        self.assertEqual(act["archive_allocated_bytes"], os.stat(archive).st_blocks * 512)
        self.assertEqual(act["saved_allocated_bytes"], act["allocated_bytes_before"] - act["archive_allocated_bytes"])
        self.assertGreater(act["saved_allocated_bytes"], 0)
        entries = manifest_entries(self.manifest)
        self.assertEqual([e["action"] for e in entries], ["archived", "released"])
        rec = entries[0]
        self.assertEqual(rec["path"], f"home/agent/scratch/window-1/run-{RUN}/snap.json")
        self.assertEqual(rec["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(rec["logical_bytes"], len(data))
        self.assertEqual((rec["dev"], rec["ino"]), (st.st_dev, st.st_ino))
        self.assertEqual(rec["mode"], 0o640)
        self.assertEqual(rec["mtime_ns"], st.st_mtime_ns)
        self.assertEqual(rec["run"]["id"], RUN)
        self.assertEqual(rec["owner"], "agent-one")
        self.assertEqual(rec["policy"]["version"], 2)
        self.assertEqual(rec["policy"]["sha256"], self.policy["sha256"])
        # The act time is the clock's, never the file's mtime.
        self.assertEqual(rec["at"], inv.iso(NOW))
        self.assertEqual(stat.S_IMODE(os.stat(archive).st_mode), 0o640)

        restored = inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW + 60)
        self.assertEqual(restored["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(snap.read_bytes(), data)
        # The family's measured reader opens the literal path with json.load.
        with open(snap) as handle:
            self.assertEqual(json.load(handle), json.loads(data))
        after = os.stat(snap)
        self.assertEqual(stat.S_IMODE(after.st_mode), 0o640)
        self.assertEqual(after.st_mtime_ns, st.st_mtime_ns)
        self.assertFalse(os.path.lexists(archive))
        self.assertEqual([e["action"] for e in manifest_entries(self.manifest)], ["archived", "released", "restored"])

    def test_the_next_inventory_counts_the_archive_and_not_the_released_copy(self):
        snap = self.snapshot()
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        later = self.run_pass(FakeRecord(runs={RUN: closed()}), act=False)
        held = family(later, "evidence")["by_primary_reason"]
        self.assertEqual(held["archive"]["files"], 1)
        self.assertEqual(held["archive"]["allocated_bytes"], os.stat(archive).st_blocks * 512)
        self.assertEqual(report["act"]["archive_allocated_bytes"], held["archive"]["allocated_bytes"])

    def test_an_open_runs_transcript_and_a_live_sessions_files_are_never_touched(self):
        transcript = plant(self.sessions / f"proj/run-{RUN}/subagents/agent-1.jsonl", snapshot_bytes(), 9 * DAY)
        live = plant(self.sessions / f"proj/run-{OTHER}/session.jsonl", snapshot_bytes(), 9 * DAY)
        held = self.snapshot(run=OTHER, name="live.json")
        before = tree_state(self.work)
        proc = fake_proc(self.root / "p3", holders=[(42, live), (43, held)])
        record = FakeRecord(runs={RUN: {"status": "open"}, OTHER: closed()})
        report = self.run_pass(record, proc=proc)
        self.untouched(before)
        self.assertEqual(self.unit(report, "transcripts", f"proj/run-{RUN}")["primary_reasons"],
                         {"reader-unqualified": 1})
        self.assertEqual(self.unit(report, "transcripts", f"proj/run-{OTHER}")["primary_reasons"],
                         {"open-writer": 1})
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{OTHER}")["primary_reasons"],
                         {"open-writer": 1})
        self.assertFalse(transcript.with_name(transcript.name + ".archived.gz").exists())
        self.assertEqual(report["act"]["archived_files"], 0)

    def test_the_tree_policy_archives_nothing_because_it_grants_no_action(self):
        policy = inv.load_policy(HERE / "artifact-lifecycle.toml")
        snap = plant(self.work / f"home/.codex/scratch/window-1/run-{RUN}/snap.json", snapshot_bytes(), 9 * DAY)
        before = tree_state(self.work)
        report = inv.run_inventory(policy, work_mount=self.work, proc_root=self.proc, now=NOW, reserve_bytes=0,
                                   record=FakeRecord(runs={RUN: closed()}), manifest=self.manifest, act=True)
        self.untouched(before)
        self.assertFalse(report["mutated"])
        self.assertEqual(report["mode"], "dry-run")
        self.assertEqual([c["unit"] for c in report["candidates"]], [f"window-1/run-{RUN}"])
        self.assertTrue(snap.exists())
        self.assertFalse(self.manifest.exists())

    def test_the_budget_bounds_one_pass(self):
        for name in ("a.json", "b.json", "c.json"):
            self.snapshot(name=name, data=snapshot_bytes(600 * 1024))
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertEqual(report["act"]["archived_files"], 1)
        self.assertEqual(report["act"]["refused"].get("budget-exhausted"), 2)
        self.assertEqual(report["act"]["budget_bytes"], 1 << 20)

    def test_a_one_block_file_is_not_worth_an_archive(self):
        small = self.snapshot(name="tiny.json", data=b"{}")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(small.exists())
        self.assertEqual(self.unit(report, "evidence", f"window-1/run-{RUN}")["primary_reasons"],
                         {"below-archive-floor": 1})

    def test_a_hardlinked_file_is_kept_because_its_bytes_are_shared(self):
        snap = self.snapshot()
        os.link(snap, self.root / "elsewhere-link")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["refused"].get("hardlinked"), 1)

    def test_a_second_pass_is_a_no_op(self):
        self.snapshot()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        before = tree_state(self.work)
        count = len(manifest_entries(self.manifest))
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.untouched(before)
        self.assertEqual(len(manifest_entries(self.manifest)), count)
        self.assertEqual(report["act"]["archived_files"], 0)


class RaceTests(LifecycleCase):
    """Revalidation right before the mutation: what the plan saw must
    still be what stands, or the candidate is refused and kept."""

    def test_a_file_changed_after_the_plan_is_refused_and_kept(self):
        snap = self.snapshot()
        reads = []

        def grow(id8):
            reads.append(id8)
            if len(reads) == 2:  # the act phase's re-read of the owner
                with open(snap, "ab") as handle:
                    handle.write(b" ")

        report = self.run_pass(FakeRecord(runs={RUN: closed()}, on_run_read=grow))
        self.assertTrue(snap.exists())
        self.assertFalse(snap.with_name(snap.name + ".archived.gz").exists())
        self.assertEqual(report["act"]["refused"].get("changed-since-plan"), 1)
        self.assertEqual(manifest_entries(self.manifest), [])

    def test_an_owner_that_reopened_after_the_plan_is_refused(self):
        snap = self.snapshot()
        record = FakeRecord(runs={RUN: closed()})

        def reopen(id8):
            if len(record.run_reads()) == 2:
                record.runs[RUN] = {"status": "open"}

        record.on_run_read = reopen
        report = self.run_pass(record)
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["refused"].get("raced:run-open"), 1)

    def test_a_symlink_swapped_in_for_the_file_is_never_followed(self):
        snap = self.snapshot()
        outside = plant(self.root / "outside/secret.json", snapshot_bytes(), 9 * DAY)
        outside_before = tree_state(self.root / "outside")
        record = FakeRecord(runs={RUN: closed()})

        def swap(id8):
            if len(record.run_reads()) == 2:
                os.unlink(snap)
                os.symlink(outside, snap)

        record.on_run_read = swap
        report = self.run_pass(record)
        self.assertTrue(os.path.islink(snap))
        self.assertEqual(tree_state(self.root / "outside"), outside_before)
        self.assertEqual(report["act"]["archived_files"], 0)
        self.assertEqual(report["act"]["refused"].get("changed-since-plan"), 1)

    def test_a_directory_swapped_for_a_link_is_refused(self):
        snap = self.snapshot()
        elsewhere = self.root / "elsewhere"
        record = FakeRecord(runs={RUN: closed()})

        def swap(id8):
            if len(record.run_reads()) == 2:
                run_dir = snap.parent
                os.rename(run_dir, elsewhere)
                os.symlink(elsewhere, run_dir, target_is_directory=True)

        record.on_run_read = swap
        report = self.run_pass(record)
        self.assertTrue((elsewhere / "snap.json").exists())
        self.assertFalse((elsewhere / "snap.json.archived.gz").exists())
        self.assertEqual(report["act"]["refused"].get("path-escaped"), 1)

    def test_a_held_claim_refuses_a_parallel_pass(self):
        snap = self.snapshot()
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        with open(str(self.manifest) + ".lock", "w") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["status"], "refused")
        self.assertIn("claim", report["act"]["refusal"])

    def test_a_claim_left_by_a_dead_pass_does_not_block(self):
        # The claim is a kernel lock: a pass that died released it, so the
        # lock FILE it leaves is no claim at all.
        snap = self.snapshot()
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        Path(str(self.manifest) + ".lock").write_text("pid 99999\n")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["archived_files"], 1)


class FailureTests(LifecycleCase):
    """Every failure keeps a usable original and names its reason; an
    interruption at any phase is recovered by the next pass, once."""

    def test_a_hash_mismatch_keeps_the_plain_file_and_leaves_no_partial(self):
        snap = self.snapshot()
        before = tree_state(self.work)
        with patch.object(inv, "decoded_digest", lambda path: ("0" * 64, 1)):
            report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.files_untouched(before)
        self.assertEqual(report["act"]["refused"].get("hash-mismatch"), 1)
        self.assertEqual(manifest_entries(self.manifest), [])
        self.assertTrue(snap.exists())

    def test_no_working_space_refuses_before_writing(self):
        self.snapshot()
        before = tree_state(self.work)
        real = os.statvfs

        class Full:
            def __init__(self, fs):
                self.f_frsize, self.f_blocks, self.f_bavail = fs.f_frsize, fs.f_blocks, 1

        with patch.object(inv.os, "statvfs", lambda p: Full(real(p))):
            report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.untouched(before)
        self.assertEqual(report["act"]["refused"].get("no-working-space"), 1)

    def test_an_interrupted_release_is_recovered_once(self):
        data = snapshot_bytes()
        snap = self.snapshot(data=data)

        def crash(*args, **kwargs):
            raise RuntimeError("the pass died between the manifest and the release")

        with patch.object(inv, "release_plain", crash):
            with self.assertRaises(RuntimeError):
                self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists(), "the plain copy outlives the crash")
        self.assertEqual([e["action"] for e in manifest_entries(self.manifest)], ["archived"])
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertFalse(snap.exists())
        entries = manifest_entries(self.manifest)
        self.assertEqual([e["action"] for e in entries], ["archived", "released"])
        self.assertTrue(entries[-1]["recovered"])
        self.assertEqual(report["act"]["recovered"], 1)
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertEqual(len(manifest_entries(self.manifest)), 2)
        inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertEqual(snap.read_bytes(), data)

    def test_a_partial_archive_left_by_a_dead_pass_is_cleared_and_the_file_archived(self):
        snap = self.snapshot()
        partial = snap.with_name(f".{snap.name}.archive-tmp-4242")
        partial.write_bytes(b"\x1f\x8b half an archive")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertFalse(partial.exists())
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["archived_files"], 1)

    def test_a_partial_archive_with_no_plain_original_is_kept_and_reported(self):
        snap = self.snapshot()
        orphan = snap.with_name(".gone.json.archive-tmp-4242")
        orphan.write_bytes(b"\x1f\x8b something")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(orphan.exists())
        self.assertTrue(any(c["kind"] == "partial-without-original" for c in report["act"]["conflicts"]))

    def test_a_plain_file_changed_after_its_archive_keeps_both_and_reports_the_conflict(self):
        snap = self.snapshot()

        def crash(*args, **kwargs):
            raise RuntimeError("died before release")

        with patch.object(inv, "release_plain", crash):
            with self.assertRaises(RuntimeError):
                self.run_pass(FakeRecord(runs={RUN: closed()}))
        with open(snap, "ab") as handle:
            handle.write(b"later bytes")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertTrue(snap.with_name(snap.name + ".archived.gz").exists())
        self.assertTrue(any(c["kind"] == "plain-differs-from-archive" for c in report["act"]["conflicts"]))

    def test_a_stray_archive_name_is_not_overwritten(self):
        snap = self.snapshot()
        stray = snap.with_name(snap.name + ".archived.gz")
        stray.write_bytes(b"someone else's file")
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        self.manifest.write_text("")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(stray.read_bytes(), b"someone else's file")
        self.assertEqual(report["act"]["refused"].get("archive-name-taken"), 1)

    def test_archives_without_a_manifest_refuse_every_action(self):
        snap = self.snapshot()
        plant(self.scratch / "window-1/run-0000cccc/old.json.archived.gz", b"\x1f\x8b", 9 * DAY)
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["status"], "refused")
        self.assertIn("manifest", report["act"]["refusal"])

    def test_a_manifest_line_that_does_not_read_back_stops_the_pass_and_keeps_the_original(self):
        data = snapshot_bytes()
        snap = self.snapshot(data=data)
        with patch.object(inv.os, "pread", lambda fd, n, off: b"not what was written"):
            report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertEqual(report["act"]["status"], "refused")
        self.assertIn("read back", report["act"]["refusal"])
        self.assertEqual(snap.read_bytes(), data, "the plain copy is never released on an unconfirmed entry")

    def test_a_corrupt_manifest_refuses_every_action(self):
        snap = self.snapshot()
        self.manifest.parent.mkdir(parents=True, exist_ok=True)
        self.manifest.write_text('{"action": "archived"\n')
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["status"], "refused")
        self.assertIn("manifest", report["act"]["refusal"])


class RestoreTests(LifecycleCase):
    def archived(self):
        snap = self.snapshot()
        data = snap.read_bytes()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        return snap, data, snap.with_name(snap.name + ".archived.gz")

    def test_restore_refuses_over_a_standing_plain_file(self):
        snap, data, archive = self.archived()
        snap.write_bytes(b"a newer file")
        with self.assertRaises(inv.LifecycleError) as caught:
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertIn("already", str(caught.exception))
        self.assertEqual(snap.read_bytes(), b"a newer file")
        self.assertTrue(archive.exists())

    def test_restore_of_a_corrupt_archive_installs_nothing(self):
        snap, data, archive = self.archived()
        raw = bytearray(archive.read_bytes())
        raw[len(raw) // 2] ^= 0xFF
        archive.write_bytes(bytes(raw))
        with self.assertRaises(inv.LifecycleError):
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertFalse(os.path.lexists(snap))
        self.assertTrue(archive.exists())
        self.assertEqual([p.name for p in snap.parent.iterdir()], [archive.name])

    def test_the_restore_door_and_a_refused_pass_speak_through_their_exit_codes(self):
        snap, data, archive = self.archived()
        buf = io.StringIO()
        with redirect_stdout(buf):
            code = inv.main(["--work-mount", str(self.work), "--manifest", str(self.manifest),
                             "--restore", str(snap)])
        self.assertEqual(code, 0, buf.getvalue())
        self.assertEqual(json.loads(buf.getvalue())["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(snap.read_bytes(), data)
        with redirect_stdout(io.StringIO()):
            again = inv.main(["--work-mount", str(self.work), "--manifest", str(self.manifest),
                              "--restore", str(snap)])
        self.assertEqual(again, 65)
        # An act pass whose action is refused writes its report and exits 65.
        Path(self.manifest).write_text("torn")
        out = self.root / "out" / "latest.json"
        with redirect_stdout(io.StringIO()):
            code = inv.main(["--policy", str(write_policy(self.root, POLICY_V2)), "--work-mount", str(self.work),
                             "--proc-root", str(self.proc), "--now", str(int(NOW)), "--manifest",
                             str(self.manifest), "--report", str(out)])
        self.assertEqual(code, 65)
        self.assertEqual(json.loads(out.read_text())["act"]["status"], "refused")

    def test_restore_of_a_path_never_archived_says_so(self):
        with self.assertRaises(inv.LifecycleError) as caught:
            inv.restore(self.scratch / "nothing.json", manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertIn("no archive", str(caught.exception))


class RecordReaderTests(unittest.TestCase):
    """The production reader: the reclaim pass's own curl helper, the
    pass's identity and machine-token header, the HTTP code read apart
    from the body so a 404 (no such owner) is never a failed read."""

    def setUp(self):
        self.root = fixture_root()
        self.curl = self.root / "curl"
        self.log = self.root / "curl.log"
        self.curl.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$*\" >> {self.log}\n"
            "for a in \"$@\"; do last=$a; done\n"
            "case \"$last\" in\n"
            "  */api/jobs/0000aaaa) printf '{\"id\":\"0000aaaa-x\",\"status\":\"closed\"}\\n200' ;;\n"
            "  */api/jobs/0000bbbb) printf '{\"error\":\"no\"}\\n404' ;;\n"
            "  *) exit 7 ;;\n"
            "esac\n")
        os.chmod(self.curl, 0o755)

    def test_the_reader_reads_code_and_body_and_sends_the_identity_and_header(self):
        read = inv.curl_reader(str(self.curl), "http://sor.test:7900", '{"id":"automation:dev-scratch-reclaim"}',
                               "@/hdr/file")
        self.assertEqual(read("/api/jobs/0000aaaa"), (200, {"id": "0000aaaa-x", "status": "closed"}))
        self.assertEqual(read("/api/jobs/0000bbbb")[0], 404)
        self.assertEqual(read("/api/jobs/0000cccc"), (None, None))
        sent = self.log.read_text()
        self.assertIn('x-boss-user: {"id":"automation:dev-scratch-reclaim"}', sent)
        self.assertIn("-H @/hdr/file", sent)
        self.assertIn("http://sor.test:7900/api/jobs/0000aaaa", sent)


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
        self.assertEqual(report["schema"], "artifact-inventory/2")
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
