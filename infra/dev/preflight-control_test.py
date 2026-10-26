import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


def module(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), Path(__file__).with_name(name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


A, B = "a" * 40, "b" * 40
NOW = 1_760_000_000


def done(argv, code=0, out=b"", err=b""):
    return subprocess.CompletedProcess(argv, code, out, err)


def observation(free_w1=616, age=60, free_w2=None):
    """The newest kubernetes-nodes observation, as /api/estate/observations serves it."""
    stamp = __import__("datetime").datetime.fromtimestamp(NOW - age, __import__("datetime").timezone.utc)
    return {"timestamp": stamp.strftime("%Y-%m-%dT%H:%M:%S.000000Z"), "payload": {"scope": "kubernetes-nodes", "nodes": [
        {"id": "cp-1", "disk_gb": 236, "disk_free_gb": 129, "ready": True},
        {"id": "w-1", "disk_gb": 929, "disk_free_gb": free_w1, "ready": True},
        {"id": "w-2", "disk_gb": 110, "disk_free_gb": free_w2, "ready": False}]}}


ROLES = {"cp-1": "talos-control-plane", "w-1": "talos-worker", "w-2": "talos-worker"}


class Readers:
    """Fake readers: every refusal below is driven by one of these."""

    def __init__(self, failing=(), jobs=None):
        self.failing = dict(failing)
        self.calls = []
        self.jobs = jobs or {}
        self.gone_after = 1
        self.written = []
        # What the worker answers when the rehearsal reads its committed
        # file back; the rehearsal sets it to what it wrote.
        self.content = b""

    def write(self, path, raw):
        self.written.append((str(path), raw))

    def run(self, argv, cwd=None):
        self.calls.append((tuple(argv), str(cwd) if cwd else None))
        for marker, code in self.failing.items():
            if marker in " ".join(argv):
                return done(argv, code, b"", ("refused at " + marker).encode())
        if "/usr/bin/cat" in argv:
            return done(argv, 0, self.content)
        if "start" in argv:
            return done(argv, 0, b"dev-build-0123456789abcdef\n")
        return done(argv, 0, b"ok\n")

    def kube(self, argv):
        self.calls.append((tuple(argv), None))
        name = argv[2] if len(argv) > 2 else ""
        if argv[:2] == ["get", "job"] and name in self.jobs:
            return done(argv, 0, ("job.batch/" + name).encode())
        if argv[:2] in (["get", "job"], ["get", "pvc"]):
            if self.gone_after > 0:
                self.gone_after -= 1
                return done(argv, 0, b"still/" + name.encode())
            return done(argv, 1, b"", b'Error from server (NotFound): "' + name.encode() + b'" not found')
        if argv[:2] == ["get", "pods"]:
            return done(argv, 0, b"")
        return done(argv, 1, b"", b"unexpected read")


class PreflightContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mod = module("preflight-control")

    def by_name(self, checks):
        return {c["check"]: c for c in checks}

    # (1) the image CLI generation for <sha>
    def test_a_missing_image_generation_is_named(self):
        with tempfile.TemporaryDirectory() as tmp:
            cli = Path(tmp)
            (cli / A).mkdir()
            (cli / A / "dev-control").write_text("door")
            (cli / A / "dev-control.tar").write_bytes(b"tar")
            check = self.mod.image_generation(cli, A)
            self.assertFalse(check["passed"])
            self.assertIn("config.json", check["detail"])
            (cli / A / "config.json").write_text("{}")
            self.assertTrue(self.mod.image_generation(cli, A)["passed"])

    # (2) placeable Longhorn space against planned workers x WORKER_VOLUME
    def test_storage_counts_what_fits_above_longhorns_floor_on_each_worker(self):
        # 2026-10-19 22:12Z: w-1 929 GiB, 616 free, floor 25% = 233: 383
        # placeable, three 100Gi volumes — not the six the budget counts.
        refused = self.mod.worker_storage(observation(), ROLES, 6, 100, NOW)
        self.assertFalse(refused["passed"])
        self.assertEqual(refused["numbers"]["volumes_that_fit"], 3)
        self.assertEqual(refused["numbers"]["planned"], 6)
        self.assertEqual(refused["numbers"]["nodes"]["w-1"]["placeable_gib"], 383)
        self.assertIn("w-1", refused["detail"])
        self.assertTrue(self.mod.worker_storage(observation(), ROLES, 3, 100, NOW)["passed"])

    def test_a_control_plane_never_counts_and_an_unread_worker_counts_zero(self):
        check = self.mod.worker_storage(observation(free_w1=300), ROLES, 1, 100, NOW)
        self.assertFalse(check["passed"], "300 free of 929 is 67 above the floor: no volume fits")
        self.assertNotIn("cp-1", check["numbers"]["nodes"])
        self.assertIn("unread", check["numbers"]["nodes"]["w-2"]["why"])

    def test_an_old_observation_is_no_evidence(self):
        check = self.mod.worker_storage(observation(age=4000), ROLES, 1, 100, NOW)
        self.assertFalse(check["passed"])
        self.assertIn("old", check["detail"])

    # (3) bindings a builder's worktree would match
    def write(self, path, record):
        path.write_text(json.dumps(record))

    def test_a_stale_binding_at_a_root_a_builder_matches_refuses(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            builders = Path("/work/boss/.claude/worktrees")
            # No admitted record: `dev-build stop` cannot retire it.
            self.write(root / "dev-build-1111111111111111.binding.json", {"session": "dev-build-1111111111111111", "local_root": "/work/boss", "source_head": A})
            # Admitted, but its Job is gone.
            self.write(root / "dev-build-2222222222222222.binding.json", {"session": "dev-build-2222222222222222", "local_root": "/work/boss", "source_head": A})
            self.write(root / "dev-build-2222222222222222.json", {"job": "dev-build-2222222222222222"})
            # Elsewhere: no builder matches it.
            self.write(root / "dev-build-3333333333333333.binding.json", {"session": "dev-build-3333333333333333", "local_root": "/work/wt/x", "source_head": A})
            # Retired: excluded from selection already.
            self.write(root / "dev-build-4444444444444444.binding.json", {"session": "dev-build-4444444444444444", "local_root": "/work/boss", "source_head": A})
            self.write(root / "dev-build-4444444444444444.retire-intent.json", {})
            states = {"dev-build-2222222222222222": "absent"}
            check = self.mod.bindings(root, builders, lambda name: states.get(name, "present"))
            self.assertFalse(check["passed"])
            self.assertIn("dev-build-1111111111111111", check["detail"])
            self.assertIn("cannot retire", check["detail"])
            self.assertIn("dev-build stop dev-build-2222222222222222", check["detail"])
            self.assertNotIn("3333", check["detail"])
            self.assertNotIn("4444", check["detail"])
            self.assertEqual(check["numbers"]["stale"], 2)

    def test_live_bindings_pass_and_an_unread_job_does_not(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write(root / "dev-build-1111111111111111.binding.json", {"session": "dev-build-1111111111111111", "local_root": "/work/boss", "source_head": A})
            self.write(root / "dev-build-1111111111111111.json", {"job": "dev-build-1111111111111111"})
            builders = Path("/work/boss/.claude/worktrees")
            live = self.mod.bindings(root, builders, lambda name: "present")
            self.assertTrue(live["passed"], live["detail"])
            self.assertEqual(live["numbers"]["live"], 1)
            unread = self.mod.bindings(root, builders, lambda name: "unread: Forbidden")
            self.assertFalse(unread["passed"])
            self.assertIn("Forbidden", unread["detail"])

    # (4) the builder's real path, in order
    def rehearse(self, readers, content=None):
        readers.content = self.mod.REHEARSAL_CONTENT if content is None else content
        return self.mod.rehearse(A, Path("/gen"), Path("/work/boss"), Path("/work/boss/.claude/worktrees/preflight-x"),
                                 readers.run, readers.kube, sleep=lambda s: None, write=readers.write)

    def test_a_clean_rehearsal_walks_every_step_and_watches_the_worker_go(self):
        readers = Readers()
        checks = self.by_name(self.rehearse(readers))
        for name in self.mod.REHEARSAL:
            self.assertTrue(checks[name]["passed"], (name, checks[name]["detail"]))
        # ceea10b8: the commit carries a real file, so the build after it
        # re-syncs content through the put door rather than an empty diff.
        self.assertEqual(readers.written, [("/work/boss/.claude/worktrees/preflight-x/" + self.mod.REHEARSAL_FILE, self.mod.REHEARSAL_CONTENT)])
        argvs = [" ".join(a) for a, _ in readers.calls]
        order = ["worktree add", " start " + A, "wt-cargo check", " add -- " + self.mod.REHEARSAL_FILE, " commit ", "wt-cargo check",
                 "/usr/bin/cat " + self.mod.REHEARSAL_FILE, "a-car-stays-under-the-edit-level", " stop dev-build-0123456789abcdef", "get job", "worktree remove"]
        position = 0
        for marker in order:
            found = [i for i, a in enumerate(argvs) if i >= position and marker in a]
            self.assertTrue(found, "step %r missing after %d in %s" % (marker, position, argvs))
            position = found[0] + 1
        cwd = [c for a, c in readers.calls if "exec-auto" in a]
        self.assertEqual(set(cwd), {"/work/boss/.claude/worktrees/preflight-x"}, "exec-auto runs from the NESTED worktree")
        self.assertIn("--local-root", [x for a, _ in readers.calls if "start" in a for x in a])

    def test_a_refused_re_sync_after_the_first_commit_is_named_and_the_worker_still_stops(self):
        # ceea10b8: the build after the commit is where the controller
        # re-syncs the worker; when it refuses, the rehearsal names that
        # check and still retires its worker and its worktree.
        readers = Readers()
        calls = {"n": 0}
        inner = readers.run

        def run(argv, cwd=None):
            if "wt-cargo" in " ".join(argv):
                calls["n"] += 1
                if calls["n"] == 2:
                    readers.calls.append((tuple(argv), str(cwd)))
                    return done(argv, 75, b"", b"dev-build: REFUSED - source binding cannot follow: it does not descend")
            return inner(argv, cwd)
        readers.run = run
        checks = self.by_name(self.rehearse(readers))
        self.assertFalse(checks["rehearsal-check-after-commit"]["passed"])
        self.assertIn("cannot follow", checks["rehearsal-check-after-commit"]["detail"])
        self.assertTrue(checks["rehearsal-stop"]["passed"])
        self.assertTrue(checks["rehearsal-worker-gone"]["passed"])
        self.assertTrue(checks["rehearsal-cleanup"]["passed"])

    def test_a_worker_that_does_not_hold_the_committed_file_is_named(self):
        # Exit 0 is not the proof: the worker must hold the bytes the
        # commit carried, read back through exec-auto.
        readers = Readers()
        checks = self.by_name(self.rehearse(readers, content=b"the admitted tree's bytes\n"))
        self.assertTrue(checks["rehearsal-check-after-commit"]["passed"])
        self.assertFalse(checks["rehearsal-synced-after-commit"]["passed"])
        self.assertIn(self.mod.REHEARSAL_FILE, checks["rehearsal-synced-after-commit"]["detail"])
        self.assertTrue(checks["rehearsal-stop"]["passed"])

    def test_a_rehearsal_file_that_cannot_be_written_is_named_and_nothing_is_committed(self):
        readers = Readers()

        def refuse(path, raw):
            raise OSError("fixture: read-only worktree")
        readers.write = refuse
        checks = self.by_name(self.rehearse(readers))
        self.assertFalse(checks["rehearsal-edit"]["passed"])
        self.assertIn("read-only", checks["rehearsal-edit"]["detail"])
        for name in ("rehearsal-commit", "rehearsal-check-after-commit", "rehearsal-synced-after-commit"):
            self.assertIn("not run", checks[name]["detail"], name)
        self.assertFalse(any(" commit " in " ".join(a) for a, _ in readers.calls))
        self.assertTrue(checks["rehearsal-stop"]["passed"])

    def test_the_rehearsal_file_sits_under_a_path_the_lowest_edit_level_admits(self):
        # Review b831d89e N3: at the tree root it was a path no tier claims,
        # so once the worker carries a trunk ref the edit-level lint run
        # right after the commit would refuse the preflight's own file on
        # any instance declaring a level below core. The lowest level is
        # the tier of greatest rank in infra/platform/tiers.toml.
        import tomllib
        tiers = tomllib.loads((Path(__file__).resolve().parents[2] / "infra/platform/tiers.toml").read_text())["tier"]
        lowest = max(tiers, key=lambda tier: tier["rank"])
        claimed = [p for p in lowest["paths"] if "*" not in p]
        self.assertTrue(any(self.mod.REHEARSAL_FILE.startswith(p) for p in claimed),
                        (self.mod.REHEARSAL_FILE, lowest["name"], claimed))

    def test_a_refused_start_runs_nothing_in_a_worker_and_says_not_run(self):
        readers = Readers(failing={" start ": 75})
        checks = self.by_name(self.rehearse(readers))
        self.assertFalse(checks["rehearsal-start"]["passed"])
        for name in ("rehearsal-check", "rehearsal-edit", "rehearsal-commit", "rehearsal-check-after-commit", "rehearsal-synced-after-commit",
                     "rehearsal-edit-level-lint", "rehearsal-stop", "rehearsal-worker-gone"):
            self.assertFalse(checks[name]["passed"], name)
            self.assertIn("not run", checks[name]["detail"])
        self.assertFalse(any("exec-auto" in " ".join(a) for a, _ in readers.calls))
        self.assertTrue(checks["rehearsal-cleanup"]["passed"])

    def test_a_worker_that_does_not_go_is_named(self):
        readers = Readers()
        readers.gone_after = 10 ** 6
        checks = self.by_name(self.rehearse(readers))
        self.assertFalse(checks["rehearsal-worker-gone"]["passed"])
        self.assertIn("still", checks["rehearsal-worker-gone"]["detail"])

    # (5) the receipt, and what activate reads of it
    def receipt(self, failing=()):
        checks = [{"check": n, "passed": n not in failing, "read": "fixture", "numbers": {}, "detail": "refused here" if n in failing else ""} for n in self.mod.REQUIRED]
        return {"commit": A, "main": A, "recorded_at": "2026-10-19T23:59:00Z", "checks": checks}

    def test_activate_refuses_without_a_receipt_and_names_the_verb(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError) as refused:
                self.mod.require_preflight(Path(tmp), A, A)
            self.assertIn("dev-control preflight " + A, str(refused.exception))

    def test_activate_refuses_a_receipt_with_one_check_failing_and_names_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            self.mod.write_receipt(store, self.receipt(failing=("rehearsal-edit-level-lint",)))
            with self.assertRaises(ValueError) as refused:
                self.mod.require_preflight(store, A, A)
            self.assertIn("rehearsal-edit-level-lint", str(refused.exception))
            self.assertIn("refused here", str(refused.exception))

    def test_activate_refuses_a_receipt_older_than_the_last_main_move(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            self.mod.write_receipt(store, self.receipt())
            with self.assertRaises(ValueError) as refused:
                self.mod.require_preflight(store, A, B)
            self.assertIn("main moved", str(refused.exception))
            self.mod.require_preflight(store, A, A)

    def test_a_receipt_that_skips_a_check_or_claims_a_pass_it_lacks_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            receipt = self.receipt()
            receipt["checks"] = [c for c in receipt["checks"] if c["check"] != "worker-storage"]
            receipt["passed"] = True
            self.mod.write_receipt(store, receipt)
            with self.assertRaises(ValueError) as refused:
                self.mod.require_preflight(store, A, A)
            self.assertIn("worker-storage", str(refused.exception))

    def test_a_receipt_for_another_commit_does_not_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            self.mod.write_receipt(store, self.receipt())
            with self.assertRaises(ValueError):
                self.mod.require_preflight(store, B, A)

    def test_every_run_is_retained_beside_the_latest(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            self.mod.write_receipt(store, self.receipt(failing=("bindings",)))
            self.mod.write_receipt(store, self.receipt())
            kept = sorted(p.name for p in (store / "preflight").iterdir())
            self.assertIn(A + ".json", kept)
            self.assertEqual(len([k for k in kept if k.startswith(A + "-")]), 2)
            self.assertEqual(os.stat(store / "preflight").st_mode & 0o077, 0)

    def test_the_whole_preflight_records_every_check_and_its_verdict(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cli = root / "cli"
            (cli / A).mkdir(parents=True)
            for name in ("dev-control", "dev-control.tar", "config.json"):
                (cli / A / name).write_text("x")
            workers = root / "workers"
            workers.mkdir()
            reads = {"/api/estate/observations?scope=kubernetes-nodes&limit=1": {"data": [observation()]},
                     "/api/estate/nodes": {"data": [{"id": k, "role": v} for k, v in ROLES.items()]}}
            readers = Readers()
            readers.content = self.mod.REHEARSAL_CONTENT
            receipt = self.mod.preflight(A, A, cli, root / "store", Path("/gen"), 3, workers,
                                         sor_get=reads.__getitem__, run=readers.run, kube=readers.kube,
                                         now=lambda: NOW, sleep=lambda s: None, worktree_name="preflight-x",
                                         write=readers.write)
            self.assertEqual([c["check"] for c in receipt["checks"]], list(self.mod.REQUIRED))
            self.assertEqual(self.mod.failed(receipt), [])
            self.mod.require_preflight(root / "store", A, A)
            again = Readers()
            again.content = self.mod.REHEARSAL_CONTENT
            refused = self.mod.preflight(A, A, cli, root / "store", Path("/gen"), 6, workers,
                                         sor_get=reads.__getitem__, run=again.run, kube=again.kube,
                                         now=lambda: NOW, sleep=lambda s: None, worktree_name="preflight-y",
                                         write=again.write)
            self.assertEqual([c["check"] for c in self.mod.failed(refused)], ["worker-storage"])
            with self.assertRaises(ValueError) as stopped:
                self.mod.require_preflight(root / "store", A, A)
            self.assertIn("worker-storage", str(stopped.exception))


if __name__ == "__main__":
    unittest.main()
