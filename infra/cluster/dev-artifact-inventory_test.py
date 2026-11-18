"""The artifact inventory's causal tests (backlog 25022367, design 195276f0).

Every fixture lives under BOSS_TEST_SCRATCH (boss_testing::scratch_dir,
uid and pid in the path), and the process table is a FAKE one the test
builds, so a live writer, an unreadable process and a link are planted
facts rather than whatever the host happens to be running. One test
reads the real kernel's table for its own open descriptor.
"""
import fcntl
import gzip
import hashlib
import importlib.util
import io
import json
import os
import stat
import struct
import tempfile
import unittest
import uuid
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


class ContentDoor:
    """The native content door as the FILESTORE adapter meets it
    (crates/core/boss-content/src/files/http.rs). The table holds ONE row
    per object_key sha256/<digest> in the bucket, so an upload of bytes it
    already holds is answered with that live row, keeping its ORIGINAL
    uploader, only on the same target (reuse_presented), and refused 409
    on any other — which the adapter reports as a ValueError. A recovery
    reads its prior file id back off the target's roster."""

    def __init__(self, actor="agent-one"):
        self.actor = actor
        self.rows, self.blobs, self.posts = {}, {}, []

    def __call__(self, path, entry):
        target = {"kind": "job", "id": entry["run"]["job"]}
        if "filestore" in entry:
            fid = entry["filestore"]["file_id"]
            row = next((r for r in self.rows.values() if r["id"] == fid and r["target"] == target), None)
            if row is None:
                raise ValueError("prior FILESTORE identity not uniquely present on native target")
            return dict(row), self.blobs[row["sha256"]]
        body = path.read_bytes()
        digest = hashlib.sha256(body).hexdigest()
        self.posts.append((target["id"], digest))
        row = self.rows.get(digest)
        if row is None:
            row = {"id": str(uuid.UUID(int=len(self.rows) + 1)), "target": target, "sha256": digest,
                   "size_bytes": len(body), "uploaded_by": self.actor, "deleted_at": None}
            self.rows[digest], self.blobs[digest] = row, body
        elif row["target"] != target:
            raise ValueError("FILESTORE request refused or unavailable; upload outcome may be unknown")
        return dict(row), self.blobs[digest]


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
    def test_invalid_unit_pattern_refuses_before_scanning(self):
        for pattern in ("", "x" * 257, "["):
            with self.subTest(pattern=pattern):
                text = POLICY_V2.replace('completion = "agent-run"',
                                         'unit_pattern = ' + json.dumps(pattern) + '\ncompletion = "agent-run"', 1)
                self.refused(text, "unit_pattern")

    def test_session_run_family_is_confined_and_still_grants_no_action(self):
        policy = inv.load_policy(HERE / "artifact-lifecycle.toml")
        rows = [f for f in policy["families"] if f["name"] == "codex-session-runs"]
        self.assertEqual(len(rows), 1, "the exact session/run family is missing")
        fam = rows[0]
        self.assertEqual(fam["root"], "agent-codex")
        self.assertEqual(fam["unit_depth"], 2)
        self.assertEqual(fam["unit_pattern"], "session-[0-9a-f]{8}/run-[0-9a-f]{8}")
        self.assertEqual(fam["completion"], "agent-run")
        self.assertEqual(fam["reader"], "unmeasured")
        self.assertIsNone(fam.get("action"))
        for name, root in (("codex-sessions", "home/.codex/sessions"),
                           ("claude-projects", "home/.claude/projects")):
            transcript = next(f for f in policy["families"] if f["name"] == name)
            self.assertEqual(transcript["root"], root)
            self.assertEqual(transcript["reader"], "unmeasured")
            self.assertIsNone(transcript.get("action"))

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
        self.assertEqual(names, ["codex-scratch", "codex-session-runs", "codex-sessions", "claude-projects"])
        for f in policy["families"]:
            self.assertTrue(f["reader_evidence"].strip(), f["name"])
            self.assertTrue(f["owner"].strip(), f["name"])
            self.assertIsNone(f.get("action"), f["name"])
        joined = {f["name"]: f.get("completion") for f in policy["families"]}
        self.assertEqual(joined, {"codex-scratch": "agent-run", "codex-session-runs": "agent-run",
                                  "codex-sessions": None, "claude-projects": None})

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

    # THE RESERVE, NAMED (design 195276f0, "Operational result"): when the
    # mount is short of its reserve the disk incident names what is
    # retained, whose it is and what clears it, largest first, so a
    # recurrence has a next action instead of a re-derivation.
    def test_a_short_reserve_names_what_is_retained_its_owner_and_what_clears_it(self):
        plant(self.scratch / "run-a/old.json", b"z" * 10_000, 9 * DAY)
        plant(self.sessions / "s1/t.jsonl", b"t" * 300_000, 9 * DAY)
        report = inv.run_inventory(self.policy, work_mount=self.work, proc_root=fake_proc(self.root),
                                   now=NOW, reserve_bytes=1 << 60)
        retention = report["retention"]
        self.assertGreater(retention["shortfall_bytes"], 0)
        self.assertIs(retention["met"], False)
        self.assertEqual(retention["freeable_bytes"], 0)
        first, second = retention["retained"][:2]
        self.assertEqual((first["family"], first["owner"], first["reason"]),
                         ("transcripts", "agent-two", "reader-unqualified"))
        self.assertEqual(first["clears_when"], inv.CLEARS["reader-unqualified"])
        self.assertEqual(first["allocated_bytes"],
                         family(report, "transcripts")["by_primary_reason"]["reader-unqualified"]["allocated_bytes"])
        self.assertEqual((second["family"], second["owner"], second["reason"]),
                         ("evidence", "agent-one", "completion-unproven"))
        says = retention["says"]
        self.assertIn("short of its", says)
        self.assertIn("no family grants an action", says)
        # Largest first, each with its owner and its clearance condition.
        self.assertLess(says.index("transcripts reader-unqualified"), says.index("evidence completion-unproven"))
        self.assertIn(f"agent-two; clears when {inv.CLEARS['reader-unqualified']}", says)

    def test_a_met_reserve_says_so(self):
        plant(self.scratch / "run-a/old.json", b"z" * 10_000, 9 * DAY)
        retention = self.run_inventory(fake_proc(self.root))["retention"]
        self.assertEqual(retention["shortfall_bytes"], 0)
        self.assertIs(retention["met"], True)
        self.assertIn("not short of its", retention["says"])

    def test_an_unread_mount_is_not_a_met_reserve(self):
        report = inv.run_inventory(self.policy, work_mount=self.root / "no-such-mount",
                                   proc_root=fake_proc(self.root), now=NOW, reserve_bytes=1 << 30)
        self.assertIsNone(report["retention"]["met"])
        self.assertIn("unread", report["retention"]["says"])

    def test_every_retaining_reason_names_what_clears_it(self):
        # One fact in two tables (REASONS says why a byte is held, CLEARS
        # what releases it): pinned equal, so a new reason cannot reach
        # the incident without its next action (CLAUDE.md §9a).
        self.assertEqual(sorted(inv.CLEARS), sorted(inv.REASONS))

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

    def test_unit_pattern_retains_wrong_session_even_with_a_closed_run(self):
        self.policy["families"][0]["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        good = self.snapshot(window="session-1234abcd")
        other = self.snapshot(window="coordinator")
        before = other.read_bytes()
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertFalse(good.exists(), "qualified fixture unit must actually archive")
        self.assertTrue(other.exists(), "a closed run is not authority over another unit shape")
        self.assertEqual(other.read_bytes(), before)
        self.assertEqual(family(report, "evidence")["by_primary_reason"]["unit-out-of-scope"]["files"], 1)

    def test_unit_pattern_also_keeps_out_of_scope_partial_files(self):
        snap = self.snapshot(window="coordinator")
        partial = snap.with_name(".snap.json.archive-tmp-123")
        partial.write_bytes(b"not this policy's partial to reclaim")
        self.policy["families"][0]["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(partial.exists(), "unit confinement must precede partial-file cleanup")
        self.assertEqual(partial.read_bytes(), b"not this policy's partial to reclaim")
        self.assertEqual(report["act"]["partials_cleared"], 0)

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

    def late_release_control(self, recovery=False, unread=False):
        snap = self.snapshot()
        data = snap.read_bytes()
        record = FakeRecord(runs={RUN: closed()})
        if recovery:
            with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop")):
                self.run_pass(record)
        self.policy["families"][0]["archive_store"] = "filestore"
        native = self.native_store()
        persist = inv.append_manifest
        opened = []

        def reader_after_durable_receipt(path, entry):
            persist(path, entry)
            if entry["action"] == "archived" and "filestore" in entry and not opened:
                if unread:
                    pid = self.proc / "987654"
                    pid.mkdir()
                    (pid / "comm").write_text("unread-reader\n")
                    (pid / "fd").write_text("not a readable process directory")
                    opened.append(None)
                else:
                    fd = os.open(snap, os.O_RDONLY)
                    opened.append(fd)
                    pid = self.proc / str(os.getpid())
                    pid.mkdir()
                    (pid / "comm").write_text("actual-readonly-reader\n")
                    (pid / "fd").symlink_to("/proc/self/fd", target_is_directory=True)
                    self.assertEqual(os.read(fd, 1), data[:1])

        try:
            with patch.object(inv, "append_manifest", side_effect=reader_after_durable_receipt):
                report = self.run_pass(record, archive_store=native)
            self.assertTrue(opened, "the reader must appear after the durable native receipt")
            self.assertTrue(snap.exists(), "late reader/unknown scan must retain the usable original")
            self.assertEqual(snap.read_bytes(), data)
            archive = snap.with_name(snap.name + ".archived.gz")
            self.assertEqual(inv.decoded_digest(archive), (hashlib.sha256(data).hexdigest(), len(data)))
            self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))
            self.assertEqual(report["act"]["recovered"], 0)
        finally:
            for fd in opened:
                if fd is not None:
                    os.close(fd)
        if not unread:
            # A reader's closure permits the ordinary idempotent recovery;
            # freshness must not turn into an unconditional release hold.
            resumed = self.run_pass(record, archive_store=native)
            self.assertFalse(snap.exists())
            self.assertEqual(resumed["act"]["recovered"], 1)
            archived = next(e for e in reversed(manifest_entries(self.manifest)) if e["action"] == "archived")
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
            with snap.open() as reader:
                self.assertEqual(json.load(reader), json.loads(data))
            restored = snap.stat()
            self.assertEqual(stat.S_IMODE(restored.st_mode), archived["mode"])
            self.assertEqual(restored.st_mtime_ns, archived["mtime_ns"])

    def test_readonly_reader_appearing_at_archive_release_keeps_both_copies(self):
        self.late_release_control()

    def test_readonly_reader_appearing_at_recovery_release_keeps_both_copies(self):
        self.late_release_control(recovery=True)

    def test_unread_process_table_at_archive_release_keeps_both_copies(self):
        self.late_release_control(unread=True)

    def test_new_unit_pattern_cannot_release_an_old_out_of_scope_archive(self):
        snap = self.snapshot()
        data = snap.read_bytes()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.policy["families"][0]["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists(), "policy narrowing must also fence recovery")
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["recovered"], 0)
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_malformed_archived_unit_is_an_explicit_selector_refusal(self):
        snap = self.snapshot(window="session-1234abcd")
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        entry = manifest_entries(self.manifest)[-1]
        inv.append_manifest(self.manifest, dict(entry, unit=None))
        self.policy["families"][0]["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertEqual(report["act"]["conflicts"][0]["kind"], "unit-out-of-scope")
        self.assertEqual(report["act"]["recovered"], 0)

    def recovery_path_binding(self, window, claimed=None, changed_root=False, allowed=False):
        snap = self.snapshot(window=window)
        data = snap.read_bytes()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        entry = manifest_entries(self.manifest)[-1]
        if claimed is not None:
            inv.append_manifest(self.manifest, dict(entry, unit=claimed))
        fam = self.policy["families"][0]
        fam["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        if changed_root:
            fam["root"] = "home/agent/other"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        self.assertEqual(inv.decoded_digest(archive), (hashlib.sha256(data).hexdigest(), len(data)))
        if allowed:
            self.assertFalse(snap.exists())
            self.assertEqual(report["act"]["recovered"], 1)
        else:
            self.assertTrue(snap.exists(), "a matching label cannot qualify a different actual path")
            self.assertEqual(snap.read_bytes(), data)
            self.assertEqual(report["act"]["recovered"], 0)
            self.assertEqual(report["act"]["conflicts"][0]["kind"], "unit-out-of-scope")
            self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_matching_unit_label_cannot_qualify_an_unconfined_recovery_path(self):
        self.recovery_path_binding("coordinator", claimed=f"session-1234abcd/run-{RUN}")

    def test_changed_family_root_cannot_release_its_previous_roots_archive(self):
        self.recovery_path_binding("session-1234abcd", changed_root=True)

    def test_matching_family_root_path_and_unit_allow_recovery(self):
        self.recovery_path_binding("session-1234abcd", allowed=True)

    # Review d2f5016a (car 0ede236e, urgent 8a1d0c74), probes P1, P2, P2b:
    # the unit check above is LEXICAL, so a path string can satisfy it while
    # the file it names lives elsewhere. Recovery binds the ACTUAL directory
    # and the archive label, the way archive_one already binds a fresh one.
    def interrupted_confined_pass(self):
        snap = self.snapshot(window="session-1234abcd")
        data = snap.read_bytes()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.policy["families"][0]["unit_pattern"] = "session-[0-9a-f]{8}/run-[0-9a-f]{8}"
        return snap, data

    def archive_moved_outside_the_root(self, snap):
        """Move the pass's own archive to another actor's tree, and append
        an archived entry whose label reaches it through '..'."""
        entry = manifest_entries(self.manifest)[-1]
        archive = snap.with_name(snap.name + ".archived.gz")
        outside = self.work / "home/other-actor/stolen.gz"
        outside.parent.mkdir(parents=True)
        os.rename(archive, outside)
        label = "home/agent/scratch/../../other-actor/stolen.gz"
        self.assertTrue((self.work / label).exists(), "fixture: the label must resolve to the moved archive")
        inv.append_manifest(self.manifest, dict(entry, archive=label))
        return outside

    def test_a_symlinked_session_directory_cannot_steer_recovery_release(self):
        snap, data = self.interrupted_confined_pass()
        session = self.scratch / "session-1234abcd"
        elsewhere = self.work / "home/other-actor/session-1234abcd"
        elsewhere.parent.mkdir(parents=True)
        os.rename(session, elsewhere)
        os.symlink(elsewhere, session, target_is_directory=True)
        victim = elsewhere / f"run-{RUN}" / "snap.json"
        dry = self.run_pass(FakeRecord(runs={RUN: closed()}), act=False)
        self.assertIn("symlink-not-followed", family(dry, "evidence")["by_primary_reason"])
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(victim.exists(), f"recovery released a file outside the family root: {report['act']}")
        self.assertEqual(victim.read_bytes(), data)
        self.assertEqual(report["act"]["recovered"], 0)
        self.assertEqual([c["kind"] for c in report["act"]["conflicts"]], ["path-escaped"])
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_an_archive_label_carrying_dotdot_cannot_carry_recovery_out_of_the_root(self):
        snap, data = self.interrupted_confined_pass()
        outside = self.archive_moved_outside_the_root(snap)
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        # The moved archive still verifies, so only the label can refuse it.
        self.assertEqual([c["kind"] for c in report["act"]["conflicts"]], ["path-escaped"])
        self.assertTrue(snap.exists(), f"recovery released against an unbound archive label: {report['act']}")
        self.assertEqual(snap.read_bytes(), data)
        self.assertTrue(outside.exists())
        self.assertEqual(report["act"]["recovered"], 0)

    def test_restore_after_a_dotdot_label_leaves_the_file_outside_the_root(self):
        snap, data = self.interrupted_confined_pass()
        outside = self.archive_moved_outside_the_root(snap)
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        if not snap.exists():
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
            self.assertEqual(snap.read_bytes(), data)
        self.assertTrue(outside.exists(), "restore unlinked a file outside the family root")

    def test_the_restore_door_refuses_an_archive_label_that_is_not_its_own_path(self):
        # The restore door reads no family, so it binds the label itself: a
        # fresh archive is always the plain path plus ARCHIVE_SUFFIX (archive_one),
        # and a released entry naming any other file is not this path's archive.
        snap = self.snapshot()
        data = snap.read_bytes()
        self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertFalse(snap.exists(), "fixture: the plain copy must have been released")
        entries = manifest_entries(self.manifest)
        archived = next(e for e in reversed(entries) if e["action"] == "archived")
        released = next(e for e in reversed(entries) if e["action"] == "released")
        own = snap.with_name(snap.name + ".archived.gz")
        outside = self.work / "home/other-actor/stolen.gz"
        outside.parent.mkdir(parents=True)
        outside.write_bytes(own.read_bytes())
        label = "home/agent/scratch/../../other-actor/stolen.gz"
        inv.append_manifest(self.manifest, dict(archived, archive=label))
        inv.append_manifest(self.manifest, dict(released, archive=label))
        with self.assertRaises(inv.LifecycleError) as caught:
            inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertIn("not the archive of", str(caught.exception))
        self.assertTrue(outside.exists(), "restore unlinked a file outside the family root")
        self.assertFalse(snap.exists())
        self.assertTrue(own.exists())
        # The genuine pair, appended again, still restores the exact file.
        inv.append_manifest(self.manifest, archived)
        inv.append_manifest(self.manifest, released)
        inv.restore(snap, manifest=self.manifest, work_mount=self.work, now=NOW)
        self.assertEqual(snap.read_bytes(), data)

    def test_native_archive_mode_never_releases_without_a_verified_native_copy(self):
        # Inject the selected mode after parsing so the causal assertion exercises
        # the existing release boundary, not a missing parser/interface.
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        original = snap.read_bytes()
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists(), "FILESTORE mode must retain the plain copy without native verification")
        self.assertEqual(snap.read_bytes(), original)
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))
        self.assertEqual(report["act"]["refused"], {"filestore-unverified": 1})

    def native_store(self, change=None):
        def store(path, entry):
            body = path.read_bytes()
            row = {"id": "11111111-2222-4333-8444-555555555555",
                   "target": {"kind": "job", "id": entry["run"]["job"]},
                   "sha256": hashlib.sha256(body).hexdigest(), "size_bytes": len(body),
                   "uploaded_by": "agent-one", "deleted_at": None}
            if change:
                return change(row, body)
            return row, body
        store.actor = "agent-one"
        return store

    def test_native_archive_mode_binds_the_full_native_archive_and_original_member(self):
        self.policy = inv.load_policy(write_policy(
            self.root, POLICY_V2.replace('action = "archive"', 'action = "archive"\narchive_store = "filestore"')))
        snap = self.snapshot()
        data = snap.read_bytes()
        os.chmod(snap, 0o640)
        before = os.stat(snap)
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["archived_files"], 1)
        entry = next(e for e in manifest_entries(self.manifest) if "filestore" in e)
        receipt = entry["filestore"]
        self.assertEqual(receipt["file_id"], "11111111-2222-4333-8444-555555555555")
        self.assertEqual(receipt["target"], {"kind": "job", "id": entry["run"]["job"]})
        self.assertEqual(receipt["member"], {"path": entry["path"], "sha256": entry["sha256"],
                                           "bytes": len(data)})
        self.assertEqual(receipt["policy"], entry["policy"])
        self.assertEqual(receipt["run"], entry["run"])
        self.assertEqual(receipt["archive_sha256"], hashlib.sha256(
            (self.work / entry["archive"]).read_bytes()).hexdigest())
        inv.restore(snap, self.manifest, self.work, NOW)
        restored = os.stat(snap)
        self.assertEqual(stat.S_IMODE(restored.st_mode), 0o640)
        self.assertEqual(restored.st_mtime_ns, before.st_mtime_ns)
        self.assertEqual(restored.st_atime_ns, before.st_atime_ns)
        with open(snap) as reader:
            self.assertEqual(json.load(reader), json.loads(data))
        self.assertEqual(snap.read_bytes(), data)

    def test_native_archive_mode_refuses_false_native_success_without_release(self):
        changes = {
            "malformed": lambda row, body: (None, body),
            "target": lambda row, body: (dict(row, target={"kind": "job", "id": "other"}), body),
            "sha": lambda row, body: (dict(row, sha256="0" * 64), body),
            "size": lambda row, body: (dict(row, size_bytes=len(body) + 1), body),
            "id": lambda row, body: (dict(row, id="not-a-file-id"), body),
            "deleted": lambda row, body: (dict(row, deleted_at="yesterday"), body),
            "remote-bytes": lambda row, body: (row, body[:-1] + b"X"),
        }
        for label, change in changes.items():
            with self.subTest(label=label):
                snap = self.snapshot(name=label + ".json")
                self.policy["families"][0]["archive_store"] = "filestore"
                report = self.run_pass(FakeRecord(runs={RUN: closed()}),
                                       archive_store=self.native_store(change))
                self.assertTrue(snap.exists(), label)
                self.assertTrue(report["act"]["conflicts"] or report["act"]["refused"], label)
                self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_native_archive_lost_ack_retains_original_and_reports_unknown(self):
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()
        calls = []

        def lost_ack(path, entry):
            calls.append(path)
            raise ValueError("upload outcome unknown")

        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=lost_ack)
        self.assertEqual(len(calls), 1)
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["refused"], {"filestore-unverified": 1})
        self.assertIn("unknown", report["act"]["refused_sample"][0]["detail"])
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_native_archive_timeout_keeps_original_and_bounded_failure(self):
        import subprocess
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()

        def timeout(*_):
            raise subprocess.TimeoutExpired("fixture-curl", 25)

        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=timeout)
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["refused"], {"filestore-unverified": 1})

    def test_native_archive_verifies_decoded_original_not_only_native_compressed_hash(self):
        import gzip
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()

        def changed_local_and_remote(path, entry):
            # Even an internally consistent native row + local/remote gzip
            # cannot substitute a different member or expand past its bound.
            path.write_bytes(gzip.compress(data + b"extra", mtime=0))
            return self.native_store()(path, entry)

        changed_local_and_remote.actor = "agent-one"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=changed_local_and_remote)
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["refused"], {"filestore-unverified": 1})
        self.assertIn("original member bound", report["act"]["refused_sample"][0]["detail"])

    def assert_retained_without_receipt(self, snap, data, report, detail):
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["refused"], {"filestore-unverified": 1})
        self.assertIn(detail, report["act"]["refused_sample"][0]["detail"])
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))
        self.assertFalse(any("filestore" in e for e in manifest_entries(self.manifest)))

    def test_native_bytes_that_decode_to_the_member_but_differ_from_the_archive_are_refused(self):
        # Review bf8e5e15 B1: the only remote-bytes case above corrupts the
        # gzip trailer, which the DECODE refuses before the byte-exact
        # read-back is consulted, so deleting `sha256(remote) != digest`
        # (and the length beside it) left the suite green. These remote
        # bytes are the archive with its header mtime changed: the same
        # length, the same decoded member, a different object. Only the
        # byte-exact read-back can refuse them, and it must, or the receipt
        # names an archive_sha256 the store does not hold.
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()

        def restamped(row, body):
            other = body[:4] + struct.pack("<I", 12345) + body[8:]
            self.assertEqual((len(other), gzip.decompress(other)), (len(body), gzip.decompress(body)))
            self.assertNotEqual(other, body)
            return row, other

        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store(restamped))
        self.assert_retained_without_receipt(snap, data, report, "whole archive bytes differ")

    def native_copy_of_another_member(self, member):
        # Review bf8e5e15 B2: the archive changes after its local verify (a
        # race) and the store then holds a copy CONSISTENT with that changed
        # archive — row, length and bytes all agree — of a member that is
        # not the original. Only the decoded-member equality refuses it.
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()
        other = member(data)
        self.assertNotEqual(other, data)

        def swap_then_store(path, entry):
            path.write_bytes(gzip.compress(other, mtime=0))
            return self.native_store()(path, entry)

        swap_then_store.actor = "agent-one"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=swap_then_store)
        self.assert_retained_without_receipt(snap, data, report, "does not decode to the original member")

    def test_a_native_copy_of_a_same_length_different_member_is_refused(self):
        self.native_copy_of_another_member(lambda data: data[:-2] + bytes([data[-2] ^ 1]) + data[-1:])

    def test_a_native_copy_of_a_shorter_member_is_refused(self):
        self.native_copy_of_another_member(lambda data: data[:-1])

    def test_a_native_row_another_actor_attached_is_refused(self):
        # Review bf8e5e15 N4: the door answers an upload of bytes the target
        # already holds with the EXISTING row, which keeps its original
        # uploader, so "uploaded_by is a non-empty string" verified another
        # actor's attachment as this pass's own. The row must name the actor
        # this adapter signs as; anything else retains.
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()
        store = self.native_store(lambda row, body: (dict(row, uploaded_by="agent-someone-else"), body))
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=store)
        self.assert_retained_without_receipt(snap, data, report, "not attached by this archiving actor")

    def test_a_store_that_cannot_name_its_actor_is_refused(self):
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()
        store = self.native_store()
        del store.actor
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=store)
        self.assert_retained_without_receipt(snap, data, report, "not attached by this archiving actor")

    def test_identical_evidence_in_two_runs_is_archived_to_each_runs_own_native_object(self):
        # Review bf8e5e15 N6, measured: archives are deterministic (mtime 0),
        # so identical plain files in two runs made identical archive bytes,
        # one object_key sha256/<digest>, and the second run's POST a 409 on
        # every pass: its evidence retained forever and re-sent each hour.
        # Each run's archive is keyed to the job it is attached to, so the
        # two objects differ and each verifies on its own target.
        self.policy["families"][0]["archive_store"] = "filestore"
        data = snapshot_bytes()
        snaps = {RUN: self.snapshot(run=RUN, data=data), OTHER: self.snapshot(run=OTHER, data=data)}
        door = ContentDoor()
        record = FakeRecord(runs={RUN: closed(), OTHER: closed()})
        report = self.run_pass(record, archive_store=door)
        self.assertEqual(report["act"]["refused"], {}, report["act"]["refused_sample"])
        self.assertEqual(report["act"]["archived_files"], 2)
        receipts = {e["run"]["id"]: e["filestore"] for e in manifest_entries(self.manifest) if "filestore" in e}
        self.assertEqual(sorted(receipts), [RUN, OTHER])
        self.assertNotEqual(receipts[RUN]["file_id"], receipts[OTHER]["file_id"])
        self.assertNotEqual(receipts[RUN]["archive_sha256"], receipts[OTHER]["archive_sha256"])
        for run, receipt in receipts.items():
            self.assertEqual(receipt["target"]["id"], f"{run}-1111-2222-3333-444455556666")
            self.assertEqual(receipt["member"]["sha256"], hashlib.sha256(data).hexdigest())
        posts = len(door.posts)
        settled = self.run_pass(record, archive_store=door)
        self.assertEqual(len(door.posts), posts, "a settled pass sends nothing again")
        self.assertEqual((settled["act"]["archived_files"], settled["act"]["refused"]), (0, {}))
        for snap in snaps.values():
            self.assertFalse(snap.exists())
            inv.restore(snap, self.manifest, self.work, NOW)
            self.assertEqual(snap.read_bytes(), data)

    def test_a_run_whose_job_id_cannot_key_its_archive_is_retained(self):
        # The archive's header carries the job id as its key (N6). An id the
        # record answers that is not a job id would be truncated or mangled
        # there (a slash makes gzip keep only the tail), so two runs could
        # share a key again. Such a run retains, in any family.
        class OddRecord(FakeRecord):
            def __call__(self, path):
                code, body = super().__call__(path)
                if not path.startswith("/api/jobs?") and isinstance(body, dict) and "id" in body:
                    body = dict(body, id=body["id"][:8] + "-not/a-job-id")
                return code, body

        snap = self.snapshot()
        data = snap.read_bytes()
        report = self.run_pass(OddRecord(runs={RUN: closed()}))
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(report["act"]["refused"], {"unkeyed-run": 1})
        self.assertEqual(manifest_entries(self.manifest), [])

    def test_identical_files_in_one_run_share_its_one_native_object(self):
        # The same bytes twice on ONE target: the door answers the second
        # upload with the row this pass attached a moment earlier, under
        # this pass's own actor, so both verify against the one object.
        self.policy["families"][0]["archive_store"] = "filestore"
        data = snapshot_bytes()
        first, second = self.snapshot(name="a.json", data=data), self.snapshot(name="b.json", data=data)
        door = ContentDoor()
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=door)
        self.assertEqual((report["act"]["archived_files"], report["act"]["refused"]), (2, {}))
        self.assertFalse(first.exists() or second.exists())
        self.assertEqual(len(door.rows), 1)
        ids = {e["filestore"]["file_id"] for e in manifest_entries(self.manifest) if "filestore" in e}
        self.assertEqual(ids, {next(iter(door.rows.values()))["id"]})

    def test_native_file_limit_refuses_before_calling_adapter(self):
        huge = self.root / "oversize.gz"
        with open(huge, "wb") as handle:
            handle.truncate(inv.FILESTORE_LIMIT + 1)
        with self.assertRaises(inv.Refused) as caught:
            inv.native_archive({"archive_store": lambda *_: self.fail("oversized file sent")},
                               {"archive_store": "filestore"}, huge, {})
        self.assertIn("file limit", caught.exception.detail)

    def test_native_recovery_without_plain_copy_cannot_claim_unverified_completion(self):
        snap = self.snapshot()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        snap.unlink()  # simulate a lost post-release manifest append
        self.policy["families"][0]["archive_store"] = "filestore"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertEqual(report["act"]["recovered"], 0, "native recovery cannot claim completion without verification")
        self.assertTrue(report["act"]["conflicts"])
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "archived")

    def test_recovery_persists_native_identity_before_releasing_plain(self):
        snap = self.snapshot()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.policy["families"][0]["archive_store"] = "filestore"
        release = inv.release_plain
        observed = []

        def require_durable_identity(path, planned):
            last = manifest_entries(self.manifest)[-1]
            self.assertEqual(last["action"], "archived")
            self.assertIn("filestore", last, "immutable native identity must be durable before recovery release")
            observed.append(last["filestore"]["file_id"])
            return release(path, planned)

        with patch.object(inv, "release_plain", side_effect=require_durable_identity):
            report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        self.assertEqual(observed, ["11111111-2222-4333-8444-555555555555"])
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["recovered"], 1)

    def test_recovery_manifest_receipt_failure_never_releases_plain(self):
        snap = self.snapshot()
        data = snap.read_bytes()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        archive = snap.with_name(snap.name + ".archived.gz")
        packed = archive.read_bytes()
        self.policy["families"][0]["archive_store"] = "filestore"
        fsync = inv.os.fsync
        attempts = []

        def failed_receipt(fd):
            if (os.readlink(f"/proc/self/fd/{fd}") == str(self.manifest)
                    and b'"filestore"' in self.manifest.read_bytes()):
                attempts.append(fd)
                raise OSError("fixture native receipt fsync failed")
            return fsync(fd)

        with patch.object(inv.os, "fsync", side_effect=failed_receipt):
            report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        self.assertTrue(attempts, "the actual manifest fsync boundary must be reached")
        self.assertEqual(snap.read_bytes(), data)
        self.assertEqual(archive.read_bytes(), packed)
        self.assertEqual(inv.decoded_digest(archive), (hashlib.sha256(data).hexdigest(), len(data)))
        self.assertEqual(report["act"]["status"], "refused")
        self.assertIn("receipt fsync failed", report["act"]["refusal"])
        self.assertEqual(manifest_entries(self.manifest)[-1]["action"], "archived")
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

    def test_recovery_interruption_keeps_the_verified_immutable_identity_for_retry(self):
        snap = self.snapshot()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.policy["families"][0]["archive_store"] = "filestore"
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "after native receipt")):
            self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        last = manifest_entries(self.manifest)[-1]
        self.assertIn("filestore", last)
        self.assertTrue(snap.exists())
        store = self.native_store()
        seen = []

        def retry(path, entry):
            seen.append(entry["filestore"]["file_id"])
            return store(path, entry)

        retry.actor = store.actor
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=retry)
        self.assertEqual(seen, [last["filestore"]["file_id"]])
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["recovered"], 1)

    def test_recovery_in_native_mode_cannot_bypass_missing_native_verification(self):
        snap = self.snapshot()
        # Existing legacy archive interrupted before release, then a publication
        # explicitly selects native verification. Current family policy governs
        # recovery too; a historical legacy entry cannot bypass it.
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.policy["families"][0]["archive_store"] = "filestore"
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertTrue(snap.exists())
        self.assertTrue(report["act"]["conflicts"])
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        self.assertFalse(snap.exists())
        self.assertEqual(report["act"]["recovered"], 1)
        self.assertIn("filestore", manifest_entries(self.manifest)[-1])

    def test_recovery_of_a_family_that_no_longer_acts_retains_and_names_the_policy_change(self):
        # Review bf8e5e15 B3: a filestore family's archive is interrupted
        # before release with its native receipt on the manifest. Then the
        # policy stops that family acting while ANOTHER family still acts,
        # so the act pass, and its recovery, run with no native adapter.
        # Recovery must not fall back to the pre-car legacy release: the
        # family that selected native verification no longer grants the
        # action at all, so the plain copy stays and the change is named.
        self.policy["families"][0]["archive_store"] = "filestore"
        snap = self.snapshot()
        data = snap.read_bytes()
        with patch.object(inv, "release_plain", side_effect=inv.Refused("fixture-stop", "before release")):
            self.run_pass(FakeRecord(runs={RUN: closed()}), archive_store=self.native_store())
        self.assertTrue(snap.exists())
        self.assertIn("filestore", manifest_entries(self.manifest)[-1])
        self.policy["families"][0].pop("action")
        self.policy["families"][0].pop("archive_store")
        self.policy["families"][1].update(action="archive", archive_budget_mb=1, reader="plain-path")
        report = self.run_pass(FakeRecord(runs={RUN: closed()}))
        self.assertEqual(report["act"]["status"], "ran", "another family still acts, so recovery ran")
        self.assertEqual(snap.read_bytes(), data, "a dropped family's recovery must not release the plain copy")
        self.assertEqual(report["act"]["recovered"], 0)
        rel = str(snap.relative_to(self.work))
        self.assertIn({"kind": "policy-changed", "path": rel,
                       "detail": "archive family no longer grants this action"}, report["act"]["conflicts"])
        self.assertFalse(any(e["action"] == "released" for e in manifest_entries(self.manifest)))

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

    def test_a_candidate_no_policy_acts_on_is_retained_until_activation(self):
        policy = inv.load_policy(HERE / "artifact-lifecycle.toml")
        plant(self.work / f"home/.codex/scratch/window-1/run-{RUN}/snap.json", snapshot_bytes(), 9 * DAY)
        report = inv.run_inventory(policy, work_mount=self.work, proc_root=self.proc, now=NOW,
                                   reserve_bytes=1 << 60, record=FakeRecord(runs={RUN: closed()}))
        retention = report["retention"]
        self.assertEqual(retention["freeable_bytes"], 0)
        row = next(r for r in retention["retained"] if r["reason"] == "candidate")
        self.assertEqual((row["family"], row["owner"]), ("codex-scratch", "agent-codex"))
        self.assertEqual(row["allocated_bytes"], report["candidate_bytes"])
        self.assertEqual(row["clears_when"], inv.CLEARS["candidate"])

    def test_a_candidate_an_acting_family_takes_is_freeable_not_retained(self):
        self.snapshot()
        report = self.run_pass(FakeRecord(runs={RUN: closed()}), act=False)
        retention = report["retention"]
        self.assertGreater(report["candidate_bytes"], 0)
        self.assertEqual(retention["freeable_bytes"], report["candidate_bytes"])
        self.assertNotIn("candidate", [r["reason"] for r in retention["retained"]])
        self.assertIs(retention["met"], True)
        self.assertNotIn("no family grants an action", retention["says"])

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


class FilestoreAdapterTests(LifecycleCase):
    """Real curl against this test's isolated HTTP server; no native upload.
    The server retains wire method/identity/body and can refuse an accepted
    write ACK without the client inventing success or retrying the write.
    """

    # The signed identity the sidecar sends, in the shape of its SOR_USER
    # (infra/cluster/dev-scratch-reclaim.sh): JSON, whose `id` the content
    # door records as a new row's uploaded_by.
    USER = json.dumps({"id": "agent-fixture", "role": "platform-admin", "access_tier": "operator"})

    def setUp(self):
        super().setUp()
        import http.server
        import threading
        self.requests = []
        self.reply = 201
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                fixture.requests.append(("POST", self.path, dict(self.headers), body))
                fixture.upload = body
                self.send_response(fixture.reply)
                self.end_headers()
                if fixture.reply == 201:
                    self.wfile.write(json.dumps(fixture.row).encode())

            def do_GET(self):
                fixture.requests.append(("GET", self.path, dict(self.headers), b""))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps([fixture.row]).encode() if self.path.startswith("/api/files?") else fixture.archive.read_bytes())

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.thread.join)
        self.addCleanup(self.server.shutdown)
        self.ports = self.root / "ports.env"
        self.ports.write_text(f"content={self.server.server_port}\n")
        self.archive = self.root / "member.archived.gz"
        import gzip
        self.archive.write_bytes(gzip.compress(b"original-member", mtime=0))
        self.entry = {"run": {"job": "native-run-123"}}
        self.row = {"id": "11111111-2222-4333-8444-555555555555", "target": {"kind": "job", "id": "native-run-123"}}
        self.adapter = inv.filestore_adapter("curl", "http://127.0.0.1:7900", self.USER, "x-boss-machine: fixture", self.ports)

    def test_existing_signed_adapter_uploads_once_and_gets_exact_returned_file_id(self):
        row, body = self.adapter(self.archive, self.entry)
        self.assertEqual(row, self.row)
        self.assertEqual(body, self.archive.read_bytes())
        self.assertEqual([(r[0], r[1]) for r in self.requests], [
            ("POST", "/api/files"), ("GET", "/api/files/" + self.row["id"])])
        for _, _, headers, _ in self.requests:
            self.assertEqual(headers["x-boss-user"], self.USER)
            self.assertEqual(headers["x-boss-machine"], "fixture")
        self.assertIn(b"native-run-123", self.upload)
        self.assertIn(self.archive.read_bytes(), self.upload)

    def test_unknown_upload_outcome_and_redirect_never_retry_or_follow(self):
        for status in (403, 503, 302):
            with self.subTest(status=status):
                self.requests.clear()
                self.reply = status
                with self.assertRaisesRegex(ValueError, "outcome may be unknown"):
                    self.adapter(self.archive, self.entry)
                self.assertEqual([(r[0], r[1]) for r in self.requests], [("POST", "/api/files")])

    def test_native_file_bound_tracks_the_actual_content_body_limit(self):
        import re
        source = (HERE.parent.parent / "crates/core/boss-content/src/files/http.rs").read_text()
        limit = re.search(r"pub const UPLOAD_BODY_LIMIT_BYTES: usize = ([0-9_]+);", source)
        self.assertIsNotNone(limit, "content upload body declaration moved: reconcile native file bound")
        self.assertEqual(inv.FILESTORE_LIMIT + 1024, int(limit.group(1).replace("_", "")),
                         "native FILESTORE bound drifted from content body limit minus attach framing")

    def test_present_invalid_prior_identity_never_falls_back_to_an_upload(self):
        for prior in ({}, {"file_id": "bad"}, [], "bad"):
            with self.subTest(prior=prior):
                self.requests.clear()
                self.entry["filestore"] = prior
                with self.assertRaisesRegex(ValueError, "prior FILESTORE identity"):
                    self.adapter(self.archive, self.entry)
                self.assertEqual(self.requests, [], "a present invalid identity must never trigger a POST")

    def test_prior_native_identity_is_reconciled_by_get_only_on_same_target(self):
        self.entry["filestore"] = {"file_id": self.row["id"]}
        row, body = self.adapter(self.archive, self.entry)
        self.assertEqual(row, self.row)
        self.assertEqual(body, self.archive.read_bytes())
        self.assertEqual([(r[0], r[1]) for r in self.requests], [
            ("GET", "/api/files?target_kind=job&target_id=native-run-123"),
            ("GET", "/api/files/" + self.row["id"])])

    def test_native_adapter_refuses_userinfo_and_unregistered_port_without_sending(self):
        for url in ("http://user:password@127.0.0.1", "http://127.0.0.1/path", "file:///tmp/native"):
            with self.assertRaises(inv.LifecycleError):
                inv.filestore_adapter("curl", url, self.USER, "header", self.ports)
        self.ports.write_text("jobs=7900\n")
        with self.assertRaises(inv.LifecycleError):
            inv.filestore_adapter("curl", "http://127.0.0.1", self.USER, "header", self.ports)
        self.assertEqual(self.requests, [])

    def test_the_adapter_names_the_actor_its_signed_identity_carries(self):
        # Review bf8e5e15 N4: the release boundary compares a native row's
        # uploaded_by to the actor this adapter signs as, which is the `id`
        # the door reads out of the x-boss-user JSON it is sent.
        self.assertEqual(self.adapter.actor, "agent-fixture")
        for user in ("agent-fixture", "", json.dumps({"role": "platform-admin"}), json.dumps({"id": ""}),
                     json.dumps(["agent-fixture"])):
            with self.subTest(user=user):
                with self.assertRaisesRegex(inv.LifecycleError, "signed SoR"):
                    inv.filestore_adapter("curl", "http://127.0.0.1:7900", user, "header", self.ports)
        self.assertEqual(self.requests, [])


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
