#!/usr/bin/python3 -I
"""Rehearse a builder's isolated path before isolation is switched on.

    image-cli/<sha>/dev-control preflight <sha> [--workers N] [--record-on <packet>]

Writes the receipt `dev-control activate <sha>` requires: one record per
check, each naming what it read and the numbers it read, retained under
the controller store's preflight/ directory (every run kept, the latest
as <sha>.json). Exit 0 only when every check passed.

WHY (backlog b50b8953, David 2026-10-19T23:29Z: 'let us try and do more
prep work to help ensure these jobs get built successfully, as we keep
hitting errors after flipping on the feature'). Two activations that day
failed first in two different places, and both were findable before the
flip: at 14:41Z every worker volume refused at w-1's Longhorn floor
(67e98635); at 22:24Z every nested Claude Code worktree was refused by
exec-auto's binding selection (7dd185c6). Each time B was quiesced, the
flip happened, the first REAL build failed, and the flip was rolled back.
The conductor's hand rehearsal (worker_build_proof on b3071bf4) proved
the path the conductor used, /work/wt, not the one `boss dispatch` gives
builders. So this walks THAT path — a nested worktree under the shared
checkout, a worker started on it, a build through exec-auto, a commit
carrying a file, a second build that re-syncs the worker to that commit
and a read of the file there (the ceea10b8 class), the edit-level lint in the worker
(the bc6afd15 class), a stop, and the Job, pod and volume observed gone —
and activation reads the receipt instead of anyone's memory of it.

NO EVIDENCE IS NOT A PASS. A read that could not look fails its check
and says why; a step skipped because an earlier one failed reads "not
run". A receipt is judged against REQUIRED, never against its own
`passed` field, so a receipt that leaves a check out cannot pass.
"""
import argparse
import base64
import datetime
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import uuid


CHECKOUT = Path("/work/boss")
# Where `boss dispatch` starts builders: Claude Code worktrees NESTED in
# the shared checkout (7dd185c6), not /work/wt.
BUILDER_ROOT = CHECKOUT / ".claude/worktrees"
WORKERS = Path("/work/home/.config/boss/workers")
OBSERVATION = "/api/estate/observations?scope=kubernetes-nodes&limit=1"
NODES = "/api/estate/nodes"
# The pod has no Longhorn read (no cluster-scoped grant), so placeable
# space is the estate's own kubelet-filesystem reading (observe-nodefs,
# merged into kubernetes-nodes) less Longhorn's minimal-available floor:
# the measurement of a386866e and 67e98635. Longhorn's default disk is
# that filesystem on every Talos node (estate_compare.rs).
LONGHORN_MINIMAL_AVAILABLE_PCT = 25
WORKER_ROLE = "talos-worker"
# The observer posts every few minutes; an older reading is not today's.
OBSERVATION_MAX_AGE_S = 1800
CHECK_CRATE = "boss-expr"
EDIT_LEVEL_LINT = "infra/lint/a-car-stays-under-the-edit-level.sh"
GONE_POLLS = 36
GONE_INTERVAL_S = 5
REHEARSAL = (
    "rehearsal-worktree",
    "rehearsal-start",
    "rehearsal-check",
    "rehearsal-edit",
    "rehearsal-commit",
    "rehearsal-check-after-commit",
    "rehearsal-synced-after-commit",
    "rehearsal-edit-level-lint",
    "rehearsal-stop",
    "rehearsal-worker-gone",
    "rehearsal-cleanup",
)
REQUIRED = ("image-generation", "worker-storage", "bindings", *REHEARSAL)
# The builder's first commit carries a real file (ceea10b8): an empty
# commit would rehearse a re-sync that ships nothing. The worker must then
# hold exactly these bytes, read back through exec-auto.
# Under docs/, a path the lowest edit level (`data`, infra/platform/tiers.toml)
# admits, so the edit-level lint run after the commit never refuses the
# preflight's own file; the tree root is claimed by no tier (b831d89e N3).
REHEARSAL_FILE = "docs/preflight-rehearsal.txt"
REHEARSAL_CONTENT = b"dev-control preflight: a builder's first commit, followed by its worker (ceea10b8)\n"
SESSION = re.compile(r"dev-build-[0-9a-f]{16}")
COMMIT = re.compile(r"[0-9a-f]{40}")


def check(name, passed, read, numbers=None, detail="", steps=None):
    record = {"check": name, "passed": bool(passed), "read": read, "numbers": numbers or {}, "detail": detail}
    if steps is not None:
        record["steps"] = steps
    return record


def worker_module():
    """The SAME generation's dev-build.py: its volume size and its count."""
    path = Path(__file__).resolve().with_name("dev-build.py")
    spec = importlib.util.spec_from_file_location("dev_build_for_preflight", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def gib(quantity):
    found = re.fullmatch(r"([0-9]+)Gi", quantity)
    if not found:
        raise ValueError("worker volume size is not whole Gi: " + quantity)
    return int(found.group(1))


def image_generation(cli_store, commit):
    generation = cli_store / commit
    required = ("dev-control", "dev-control.tar", "config.json")
    missing = [n for n in required if (generation / n).is_symlink() or not (generation / n).is_file()]
    return check("image-generation", not missing, str(generation),
                 {"files_required": len(required), "files_present": len(required) - len(missing)},
                 "missing: " + ", ".join(missing) if missing else "")


def epoch(stamp):
    return datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()


def worker_storage(observation, roles, planned, volume_gib, now, max_age=OBSERVATION_MAX_AGE_S):
    age = int(now - epoch(observation["timestamp"]))
    rows = {row.get("id"): row for row in observation["payload"]["nodes"]}
    nodes, fit = {}, 0
    for node in sorted(n for n, role in roles.items() if role == WORKER_ROLE):
        row = rows.get(node)
        total = row.get("disk_gb") if row else None
        free = row.get("disk_free_gb") if row else None
        if row is None or type(total) is not int or type(free) is not int or total <= 0:
            nodes[node] = {"volumes": 0, "why": "unread: no disk figures in the observation"}
            continue
        floor = math.ceil(total * LONGHORN_MINIMAL_AVAILABLE_PCT / 100)
        placeable = free - floor
        volumes = max(0, placeable // volume_gib) if row.get("ready") is True else 0
        nodes[node] = {"disk_gib": total, "free_gib": free, "floor_gib": floor, "placeable_gib": placeable, "volumes": volumes}
        if row.get("ready") is not True:
            nodes[node]["why"] = "not ready"
        fit += volumes
    numbers = {"planned": planned, "volume_gib": volume_gib, "volumes_that_fit": fit,
               "observation_age_s": age, "nodes": nodes}
    each = ", ".join("%s %s" % (n, ("%d GiB placeable (%d free - %d floor)" % (v["placeable_gib"], v["free_gib"], v["floor_gib"]))
                                if "placeable_gib" in v else v["why"]) for n, v in nodes.items())
    if age > max_age:
        return check("worker-storage", False, OBSERVATION, numbers,
                     "the kubernetes-nodes observation is %d s old (over %d): no current reading" % (age, max_age))
    passed = planned >= 1 and fit >= planned
    detail = "%d x %d GiB worker volumes fit, %d planned: %s" % (fit, volume_gib, planned, each)
    return check("worker-storage", passed, OBSERVATION, numbers, detail)


def job_state(kube, name):
    result = kube(["get", "job", name, "-o", "name"])
    if result.returncode == 0 and result.stdout.strip():
        return "present"
    if b"NotFound" in result.stderr:
        return "absent"
    return "unread: " + said(result)


def bindings(root, builder_root, state):
    """Bindings exec-auto would select from a builder's worktree (7dd185c6)."""
    stale, live = [], []
    for path in sorted(root.glob("dev-build-*.binding.json")):
        try:
            record = json.loads(path.read_text())
            name, local = record["session"], record["local_root"]
        except (OSError, ValueError, KeyError, TypeError) as error:
            stale.append("%s: unreadable binding (%s)" % (path.name, error))
            continue
        if not isinstance(name, str) or not SESSION.fullmatch(name) or not isinstance(local, str) or not local.startswith("/"):
            stale.append("%s: malformed binding" % path.name)
            continue
        if (root / (name + ".retire-intent.json")).exists() or not builder_root.is_relative_to(Path(local)):
            continue
        if not (root / (name + ".json")).exists():
            if (root / (name + ".intent.json")).exists():
                stale.append("%s at %s: never admitted; `dev-build stop %s` retires its launch" % (name, local, name))
            else:
                stale.append("%s at %s: no admitted session or launch record, so dev-build stop cannot retire it" % (name, local))
            continue
        observed = state(name)
        if observed == "present":
            live.append(name)
        elif observed == "absent":
            stale.append("%s at %s: its Job is gone; `dev-build stop %s` records the retirement" % (name, local, name))
        else:
            stale.append("%s at %s: Job %s" % (name, local, observed))
    numbers = {"stale": len(stale), "live": len(live)}
    detail = "; ".join(stale) if stale else "%d live binding(s) at a root a builder matches: %s" % (len(live), ", ".join(live) or "none")
    return check("bindings", not stale, str(root / "*.binding.json"), numbers, detail)


def said(result):
    for stream in (result.stderr, result.stdout):
        lines = [line for line in (stream or b"").decode("utf-8", "replace").splitlines() if line.strip()]
        if lines:
            return "".join(c if c.isprintable() else "?" for c in lines[-1][:300])
    return "no output"


def step_record(argv, cwd, result):
    # The whole output is kept: a reduced record is the only copy lost.
    return {"argv": list(argv), "cwd": str(cwd) if cwd else None, "exit": result.returncode,
            "stdout_base64": base64.b64encode(result.stdout or b"").decode("ascii"),
            "stderr_base64": base64.b64encode(result.stderr or b"").decode("ascii")}


def worker_gone(session, kube, sleep, polls=GONE_POLLS):
    def gone(result, listing=False):
        if result.returncode == 0:
            return "present" if result.stdout.strip() else ("gone" if listing else "unread: empty answer")
        return "gone" if b"NotFound" in result.stderr and not listing else "unread: " + said(result)
    state = {}
    for attempt in range(1, polls + 1):
        state = {"job": gone(kube(["get", "job", session, "-o", "name"])),
                 "pods": gone(kube(["get", "pods", "-l", "job-name=" + session, "-o", "name"]), listing=True),
                 "pvc": gone(kube(["get", "pvc", session, "-o", "name"]))}
        if all(v == "gone" for v in state.values()):
            return check("rehearsal-worker-gone", True, "job, pods and pvc " + session, {"reads": attempt},
                         "Job, pod and PVC gone after %d read(s)" % attempt)
        if attempt < polls:
            sleep(GONE_INTERVAL_S)
    return check("rehearsal-worker-gone", False, "job, pods and pvc " + session, {"reads": polls},
                 "still present after %d reads: %s" % (polls, ", ".join("%s %s" % kv for kv in state.items())))


def write_file(path, raw):
    path.write_bytes(raw)


def rehearse(commit, generation, checkout, worktree, run, kube, sleep=time.sleep, write=write_file):
    controller = ["/usr/bin/python3", "-I", str(generation / "infra/dev/dev-build.py")]
    auto = [*controller, "exec-auto", "--"]
    build = [*auto, "/workspace/source/infra/dev/wt-cargo", "check", "-p", CHECK_CRATE]
    checks = []

    def step(name, argv, cwd=None):
        try:
            result = run(argv, cwd)
        except (OSError, subprocess.TimeoutExpired) as error:
            checks.append(check(name, False, " ".join(argv), detail="did not complete: %s" % error))
            return None
        ok = result.returncode == 0
        checks.append(check(name, ok, " ".join(argv), {"exit": result.returncode},
                            "" if ok else "exit %d: %s" % (result.returncode, said(result)),
                            [step_record(argv, cwd, result)]))
        return result if ok else None

    def not_run(names, why):
        checks.extend(check(n, False, "", detail="not run: " + why) for n in names)

    added = step("rehearsal-worktree", ["/usr/bin/git", "-C", str(checkout), "worktree", "add", "--detach", str(worktree), commit])
    if added is None:
        not_run(REHEARSAL[1:], "the nested worktree was not created")
        return checks
    started = step("rehearsal-start", [*controller, "start", commit, "--local-root", str(worktree)])
    words = started.stdout.decode("utf-8", "replace").split() if started is not None else []
    session = words[-1] if words and SESSION.fullmatch(words[-1]) else None
    if started is not None and session is None:
        checks[-1].update(passed=False, detail="start printed no worker session name: " + said(started))
    if session is None:
        not_run(REHEARSAL[2:-1], "the worker did not start")
    else:
        step("rehearsal-check", build, worktree)
        edited = None
        try:
            write(worktree / REHEARSAL_FILE, REHEARSAL_CONTENT)
        except OSError as error:
            checks.append(check("rehearsal-edit", False, str(worktree / REHEARSAL_FILE), detail="not written: %s" % error))
        else:
            edited = step("rehearsal-edit", ["/usr/bin/git", "-C", str(worktree), "add", "--", REHEARSAL_FILE])
        committed = None if edited is None else step(
            "rehearsal-commit", ["/usr/bin/git", "-C", str(worktree), "-c", "user.name=dev-control preflight",
                                 "-c", "user.email=preflight@boss.invalid", "commit", "--no-verify",
                                 "-m", "dev-control preflight: a builder's first commit"])
        after = ("rehearsal-check-after-commit", "rehearsal-synced-after-commit")
        if edited is None:
            not_run(("rehearsal-commit", *after), "the rehearsal file was not staged")
        elif committed is None:
            not_run(after, "the rehearsal commit failed")
        else:
            # The build after the commit is where exec-auto re-syncs the
            # worker through the put door; the read proves what it holds.
            step("rehearsal-check-after-commit", build, worktree)
            synced = step("rehearsal-synced-after-commit", [*auto, "/usr/bin/cat", REHEARSAL_FILE], worktree)
            if synced is not None and synced.stdout != REHEARSAL_CONTENT:
                checks[-1].update(passed=False, detail="the worker's %s is not the committed bytes: it holds %r"
                                  % (REHEARSAL_FILE, synced.stdout[:300]))
        step("rehearsal-edit-level-lint", [*auto, "/bin/bash", EDIT_LEVEL_LINT], worktree)
        step("rehearsal-stop", [*controller, "stop", session])
        checks.append(worker_gone(session, kube, sleep))
    step("rehearsal-cleanup", ["/usr/bin/git", "-C", str(checkout), "worktree", "remove", "--force", str(worktree)])
    return checks


def failed(receipt):
    """The checks that keep activation refused: failed, or absent."""
    checks = receipt.get("checks") if isinstance(receipt, dict) else None
    if not isinstance(checks, list):
        return [{"check": "receipt", "detail": "carries no checks"}]
    seen = {c.get("check"): c for c in checks if isinstance(c, dict)}
    result = []
    for name in REQUIRED:
        record = seen.get(name)
        if record is None:
            result.append({"check": name, "detail": "absent from the receipt"})
        elif record.get("passed") is not True:
            result.append(record)
    return result


def receipts(store):
    directory = store / "preflight"
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.is_symlink() or directory.stat().st_uid != os.getuid() or directory.stat().st_mode & 0o077:
        raise ValueError("unsafe preflight receipt directory " + str(directory))
    return directory


def atomic(path, raw):
    with tempfile.NamedTemporaryFile(prefix=".preflight-", dir=path.parent, delete=False) as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(stream.name, path)


def write_receipt(store, receipt):
    commit = receipt.get("commit", "")
    if not COMMIT.fullmatch(commit):
        raise ValueError("a preflight receipt names the full commit")
    directory = receipts(store)
    raw = json.dumps(receipt, sort_keys=True).encode()
    # Every run is retained; the latest is a machine copy activate reads.
    retained = directory / ("%s-%s.json" % (commit, receipt.get("id") or uuid.uuid4().hex))
    atomic(retained, raw)
    atomic(directory / (commit + ".json"), raw)
    return retained


def require_preflight(store, commit, main):
    """What `dev-control activate <sha>` asks before it changes anything."""
    if not isinstance(commit, str) or not COMMIT.fullmatch(commit):
        raise ValueError("activation needs the full commit")
    if not isinstance(main, str) or not COMMIT.fullmatch(main):
        raise ValueError("activation needs --main, the commit the image store names current; the published bootstrap passes it")
    command = "image-cli/%s/dev-control preflight %s" % (commit, commit)
    path = store / "preflight" / (commit + ".json")
    try:
        receipt = json.loads(path.read_bytes())
    except FileNotFoundError:
        raise ValueError("no preflight receipt for %s at %s: run %s first; nothing activated" % (commit, path, command)) from None
    if not isinstance(receipt, dict) or receipt.get("commit") != commit:
        raise ValueError("%s is not a receipt for %s; run %s" % (path, commit, command))
    if receipt.get("main") != main:
        raise ValueError("main moved since the rehearsal: %s read main at %s and the image store now names %s; run %s again"
                         % (path, receipt.get("main"), main, command))
    refused = failed(receipt)
    if refused:
        raise ValueError("the preflight receipt %s does not pass: %s; nothing activated, rerun %s once repaired" % (
            path, "; ".join("%s (%s)" % (c.get("check"), c.get("detail") or "failed") for c in refused), command))
    return receipt


def preflight(commit, main, cli_store, store, generation, planned, worker_root, *, sor_get, run, kube,
              now=time.time, sleep=time.sleep, worktree_name=None, volume_gib=None,
              checkout=CHECKOUT, builder_root=BUILDER_ROOT, write=write_file):
    volume_gib = volume_gib or gib(worker_module().WORKER_VOLUME)
    checks = [image_generation(cli_store, commit)]
    try:
        observed = sor_get(OBSERVATION)["data"][0]
        roles = {node["id"]: node.get("role") for node in sor_get(NODES)["data"]}
        checks.append(worker_storage(observed, roles, planned, volume_gib, now()))
    except (KeyError, IndexError, TypeError, ValueError, OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        checks.append(check("worker-storage", False, OBSERVATION, {"planned": planned}, "unread: %s" % error))
    checks.append(bindings(worker_root, builder_root, lambda name: job_state(kube, name)))
    worktree = builder_root / (worktree_name or "preflight-" + uuid.uuid4().hex[:12])
    checks.extend(rehearse(commit, generation, checkout, worktree, run, kube, sleep, write))
    receipt = {"id": uuid.uuid4().hex, "commit": commit, "main": main, "workers_planned": planned,
               "recorded_at": datetime.datetime.fromtimestamp(now(), datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
               "worktree": str(worktree), "checks": checks}
    receipt["passed"] = not failed(receipt)
    receipt["path"] = str(write_receipt(store, receipt))
    return receipt


def record_on(packet, receipt, api):
    """Annotate the activation packet through the trusted door, not by hand."""
    key = "dev_control_preflight_%s_%s" % (receipt["commit"][:9], re.sub(r"[^0-9T]", "", receipt["recorded_at"]))
    summary = {"commit": receipt["commit"], "main": receipt["main"], "passed": receipt["passed"],
               "recorded_at": receipt["recorded_at"], "receipt": receipt["path"], "workers_planned": receipt["workers_planned"],
               "failed": [c["check"] for c in failed(receipt)],
               "checks": [{k: c.get(k) for k in ("check", "passed", "read", "numbers", "detail")} for c in receipt["checks"]],
               "by": "dev-control preflight (infra/dev/preflight-control.py)"}
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as body:
        json.dump({key: summary}, body)
    try:
        return api(["PATCH", "/api/jobs/%s/metadata" % packet, body.name])
    finally:
        os.unlink(body.name)


def main():
    worker = worker_module()
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("commit")
    parser.add_argument("--main", required=True, help="the commit the image store names current")
    parser.add_argument("--cli-store", type=Path, default=Path("/work/tools/image-cli"))
    parser.add_argument("--store", type=Path, default=Path("/work/tools/dev-control"))
    parser.add_argument("--workers", type=int, default=worker.WORKER_VOLUME_LIMIT,
                        help="worker volumes the activation plans for (default: the controller's own limit)")
    parser.add_argument("--record-on", help="the packet the receipt is recorded on (b3071bf4 or the activation packet)")
    args = parser.parse_args()
    if not COMMIT.fullmatch(args.commit) or not COMMIT.fullmatch(args.main):
        raise ValueError("preflight names full commits")
    if args.record_on and not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", args.record_on):
        raise ValueError("--record-on takes the packet's full id")
    generation = Path(__file__).resolve().parents[2]
    env = {"HOME": "/work/home", "PATH": "/usr/bin:/bin"}
    for key in ("KUBERNETES_SERVICE_HOST", "KUBERNETES_SERVICE_PORT", "KUBERNETES_SERVICE_PORT_HTTPS"):
        if key in os.environ:
            env[key] = os.environ[key]
    # The trusted door signs as the operator running the verb.
    api_env = dict(env, **{k: os.environ[k] for k in ("BOSS_ACTOR", "BOSS_ACTOR_FILE", "BOSS_AGENT_RUN", "BOSS_JOBS_URL") if k in os.environ})

    def api(argv):
        return subprocess.run(["/bin/bash", str(generation / "infra/dev/boss-api"), *argv], env=api_env,
                              capture_output=True, timeout=120, check=False)

    def sor_get(path):
        result = api(["GET", path])
        if result.returncode:
            raise RuntimeError("boss-api GET %s exit %d: %s" % (path, result.returncode, said(result)))
        return json.loads(result.stdout)

    def run(argv, cwd=None):
        return subprocess.run(argv, cwd=cwd, env=env, capture_output=True, timeout=3600, check=False)

    def kube(argv):
        return subprocess.run([str(generation / "bin/kubectl"), "--namespace=boss-dev", *argv], env=env,
                              capture_output=True, timeout=120, check=False)

    receipt = preflight(args.commit, args.main, args.cli_store, args.store, generation, args.workers, WORKERS,
                        sor_get=sor_get, run=run, kube=kube, volume_gib=gib(worker.WORKER_VOLUME))
    for record in receipt["checks"]:
        print("%s  %-30s %s" % ("PASS" if record["passed"] else "FAIL", record["check"], record["detail"]))
    print("dev-control preflight: %s for %s at main %s — receipt %s" % (
        "PASSED" if receipt["passed"] else "REFUSED", args.commit, args.main, receipt["path"]))
    code = 0 if receipt["passed"] else 1
    if args.record_on:
        recorded = record_on(args.record_on, receipt, api)
        if recorded.returncode:
            print("dev-control preflight: the receipt was NOT recorded on %s: %s" % (args.record_on, said(recorded)), file=sys.stderr)
            code = code or 2
        else:
            print("dev-control preflight: recorded on " + args.record_on)
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        print("dev-control preflight: REFUSED — " + str(error), file=sys.stderr)
        sys.exit(75)
