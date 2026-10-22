#!/usr/bin/python3 -I
"""dev-artifact-inventory — the dry run of the agent-artifact lifecycle.

WHY THIS EXISTS (backlog 25022367, design 195276f0, approved by David
2026-10-17). On 2026-10-16 the dev pod's /work volume reached 96% with
1.7 GiB free. The hourly reclaim (dev-scratch-reclaim.sh) has a
lifecycle for worktrees and cargo targets and none for the files agents
leave behind — ~14 GiB of Codex scratch, ~3.9 GiB of Codex sessions,
~4 GiB of Claude project records that day — and nothing could say which
of those bytes were active work, evidence an incident still needed, or
transcripts a harness reads back. Headroom was bought twice by hand
(byte-verified compression of old coordinator snapshots); neither was a
mechanism, and neither could be repeated without a person re-deriving
what was safe.

WHAT IT DOES. It reads the versioned policy (artifact-lifecycle.toml,
beside this file), walks each family's root under the work mount, and
accounts EVERY file it finds — logical bytes, allocated bytes with a
hard link counted once, its unit of ownership, the process holding it
open — under the first reason that retains it:

  open-writer         a process holds it open: active work
  recently-changed    changed inside the family's settle window
  reader-unqualified  the family's readers have not been measured
  completion-unproven no record joins it to a closed owner

So the report answers the operator's question when the reserve is
short — is it active work, an unqualified reader, or evidence growth
nobody has proven complete — with bytes and a named owner for each.

WHAT IT NEVER DOES. It mutates nothing it reads: no file is opened for
writing, moved, compressed or removed, and no link is followed (a linked
root, or a linked directory on the way to one, is refused; a link inside
a root is counted as a link and its target never read). Policy version 1
joins no completion record, so nothing is a candidate and the report
says so; version 1's loader refuses any key it does not know, so an
action cannot be switched on by editing the policy before the actuator
and its trust-boundary review exist. What it cannot read is UNCERTAINTY,
counted and named: an unreadable process table or process makes every
file's liveness unknown (a missing heartbeat is not a dead writer), an
unreadable directory is listed with its path, never read as empty.

The only files it writes are the two its caller names: the report,
replaced atomically (temporary file, fsync, rename, fsync of the
directory), and a bounded growth ring of one totals line per pass.

Exit: 0 written; 64 the policy or arguments were refused (nothing
written); 73 the report or ring could not be written.
"""
import argparse
import hashlib
import json
import os
import re
import stat
import sys
import tempfile
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

SCHEMA = "artifact-inventory/1"
FIELDS = {
    "name": str,
    "root": str,
    "owner": str,
    "unit_depth": int,
    "settle_hours": int,
    "reader": str,
    "reader_evidence": str,
}
READERS = ("plain-path", "unmeasured")
NAME = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
INVENTORY_ONLY = "policy version 1 is inventory only and grants no action"
REASONS = {
    "open-writer": "a process holds the file open; its writer is active",
    "recently-changed": "changed inside the family's settle window; age is necessary and never sufficient",
    "reader-unqualified": "the family's readers have not been measured, so no archived form is known to be readable by them",
    "completion-unproven": "no record joins the file to a closed owner; policy version 1 joins none, so nothing is a candidate",
    "symlink-not-followed": "a symbolic link: counted, never followed, its target never read",
    "not-a-regular-file": "a fifo, socket or device: counted, never read",
}
SAMPLE = 20


class PolicyError(ValueError):
    pass


def load_policy(path):
    path = Path(path)
    raw = path.read_bytes()
    try:
        data = tomllib.loads(raw.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise PolicyError(f"{path}: does not parse: {error}") from None
    for key in sorted(data):
        if key not in ("version", "family"):
            raise PolicyError(f"{path}: unknown key '{key}': {INVENTORY_ONLY}")
    version = data.get("version")
    if type(version) is not int or version != 1:
        raise PolicyError(f"{path}: version {version!r} is not one this inventory reads (it reads version 1)")
    families = data.get("family")
    if not isinstance(families, list) or not families:
        raise PolicyError(f"{path}: declares no [[family]]")
    out, roots = [], []
    for index, fam in enumerate(families):
        where = f"{path}: family {fam.get('name', index)!r}"
        for key in fam:
            if key not in FIELDS:
                raise PolicyError(f"{where}: unknown key '{key}': {INVENTORY_ONLY}")
        for key, kind in FIELDS.items():
            if key not in fam:
                raise PolicyError(f"{where}: missing '{key}'")
            if type(fam[key]) is not kind:
                raise PolicyError(f"{where}: '{key}' must be a {kind.__name__}")
        if not NAME.fullmatch(fam["name"]):
            raise PolicyError(f"{where}: name must be kebab-case")
        if any(f["name"] == fam["name"] for f in out):
            raise PolicyError(f"{path}: family {fam['name']!r} is named twice")
        root = fam["root"]
        parts = root.split("/")
        if not root or root.startswith("/"):
            raise PolicyError(f"{where}: root must be relative to the work mount, got {root!r}")
        if ".." in parts or "." in parts or "" in parts:
            raise PolicyError(f"{where}: root may not contain '..', '.' or an empty component, got {root!r}")
        if not 0 <= fam["unit_depth"] <= 6:
            raise PolicyError(f"{where}: unit_depth must be 0..6")
        if fam["settle_hours"] < 1:
            raise PolicyError(f"{where}: settle_hours must be at least 1")
        if fam["reader"] not in READERS:
            raise PolicyError(f"{where}: reader must be one of {', '.join(READERS)}, got {fam['reader']!r}")
        for key in ("owner", "reader_evidence"):
            if not fam[key].strip():
                raise PolicyError(f"{where}: '{key}' is blank")
        mine = PurePosixPath(root)
        for other_name, other in roots:
            if mine == other or other in mine.parents or mine in other.parents:
                raise PolicyError(f"{path}: the root of {fam['name']!r} and of {other_name!r} lie inside one another; a byte is counted once")
        roots.append((fam["name"], mine))
        out.append(dict(fam))
    return {"path": str(path), "version": 1, "sha256": hashlib.sha256(raw).hexdigest(), "families": out}


def iso(epoch):
    return datetime.fromtimestamp(epoch, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def listing(path):
    with os.scandir(path) as entries:
        return sorted(entries, key=lambda e: e.name)


def read_comm(pid_dir):
    try:
        return Path(pid_dir, "comm").read_text().strip() or "?"
    except OSError:
        return "?"


def read_liveness(proc_root):
    """Which regular files any process holds open, by (device, inode),
    and which processes could not be read. A table that cannot be read
    at all is not an empty one."""
    live = {"table_read": True, "processes_read": 0, "unreadable": [], "open_by": {}}
    try:
        pids = [e for e in listing(proc_root) if e.name.isdigit()]
    except OSError:
        live["table_read"] = False
        return live
    for entry in sorted(pids, key=lambda e: int(e.name)):
        pid = int(entry.name)
        try:
            fds = listing(os.path.join(entry.path, "fd"))
        except FileNotFoundError:
            continue  # it exited between the two reads
        except OSError:
            live["unreadable"].append({"pid": pid, "comm": read_comm(entry.path)})
            continue
        comm, refused = read_comm(entry.path), False
        for fd in fds:
            try:
                st = os.stat(fd.path)
            except FileNotFoundError:
                continue
            except OSError:
                refused = True
                continue
            if stat.S_ISREG(st.st_mode):
                live["open_by"].setdefault((st.st_dev, st.st_ino), []).append({"pid": pid, "comm": comm})
        if refused:
            live["unreadable"].append({"pid": pid, "comm": comm})
        else:
            live["processes_read"] += 1
    return live


def tally():
    return {"files": 0, "logical_bytes": 0, "allocated_bytes": 0}


def add(t, logical, allocated):
    t["files"] += 1
    t["logical_bytes"] += logical
    t["allocated_bytes"] += allocated


def walk_family(fam, work_mount, live, now, unit_limit, seen, uncertainty):
    out = {
        "name": fam["name"], "root": fam["root"], "owner": fam["owner"], "reader": fam["reader"],
        "reader_evidence": fam["reader_evidence"], "settle_hours": fam["settle_hours"], "status": "read",
        "files": 0, "links": 0, "logical_bytes": 0, "allocated_bytes": 0, "hardlinked_paths": 0,
        "by_primary_reason": {}, "units": [], "units_omitted": {"units": 0, "files": 0, "allocated_bytes": 0},
    }
    here = Path(work_mount)
    for part in PurePosixPath(fam["root"]).parts:
        here = here / part
        try:
            st = os.lstat(here)
        except FileNotFoundError:
            out["status"] = "absent"
            return out
        except OSError as error:
            out["status"], out["refusal"] = "refused", f"{here} could not be read: {error.strerror}"
            return out
        if stat.S_ISLNK(st.st_mode):
            out["status"] = "refused"
            out["refusal"] = f"{here} is a symbolic link; a root is never followed, so its bytes are not counted here"
            return out
        if not stat.S_ISDIR(st.st_mode):
            out["status"], out["refusal"] = "refused", f"{here} is not a directory"
            return out
    root, root_dev = here, st.st_dev
    settle = fam["settle_hours"] * 3600
    units = {}
    stack = [root]
    while stack:
        directory = stack.pop()
        try:
            entries = listing(directory)
        except FileNotFoundError:
            uncertainty["vanished"] += 1
            continue
        except OSError as error:
            uncertainty["unreadable_dirs"] += 1
            if len(uncertainty["unreadable_sample"]) < SAMPLE:
                uncertainty["unreadable_sample"].append(
                    {"path": str(Path(directory).relative_to(work_mount)), "error": error.strerror or str(error)})
            continue
        for entry in entries:
            try:
                st = os.lstat(entry.path)
            except FileNotFoundError:
                uncertainty["vanished"] += 1
                continue
            except OSError as error:
                uncertainty["unreadable_dirs"] += 1
                if len(uncertainty["unreadable_sample"]) < SAMPLE:
                    uncertainty["unreadable_sample"].append(
                        {"path": str(Path(entry.path).relative_to(work_mount)), "error": error.strerror or str(error)})
                continue
            if stat.S_ISDIR(st.st_mode):
                if st.st_dev == root_dev:
                    stack.append(entry.path)
                else:
                    uncertainty["other_filesystem"] += 1
                continue
            parts = Path(entry.path).relative_to(root).parts
            name = "/".join(parts[: min(fam["unit_depth"], len(parts) - 1)]) or "."
            unit = units.setdefault(name, {"unit": name, "files": 0, "logical_bytes": 0, "allocated_bytes": 0,
                                           "open_files": 0, "writers": [], "newest_change": 0.0,
                                           "primary_reasons": {}})
            if not stat.S_ISREG(st.st_mode):
                reason = "symlink-not-followed" if stat.S_ISLNK(st.st_mode) else "not-a-regular-file"
                out["links"] += 1
                add(out["by_primary_reason"].setdefault(reason, tally()), 0, 0)
                unit["primary_reasons"][reason] = unit["primary_reasons"].get(reason, 0) + 1
                continue
            key = (st.st_dev, st.st_ino)
            if key in seen:
                allocated = 0
                out["hardlinked_paths"] += 1
            else:
                seen.add(key)
                allocated = st.st_blocks * 512
            changed = max(st.st_mtime, st.st_ctime)
            writers = live["open_by"].get(key)
            if writers:
                reason = "open-writer"
                unit["open_files"] += 1
                for w in writers:
                    if w not in unit["writers"] and len(unit["writers"]) < SAMPLE:
                        unit["writers"].append(w)
            elif now - changed < settle:
                reason = "recently-changed"
            elif fam["reader"] == "unmeasured":
                reason = "reader-unqualified"
            else:
                reason = "completion-unproven"
            if not live["complete"]:
                uncertainty["liveness_unknown_files"] += 1
                uncertainty["liveness_unknown_bytes"] += allocated
            out["files"] += 1
            out["logical_bytes"] += st.st_size
            out["allocated_bytes"] += allocated
            add(out["by_primary_reason"].setdefault(reason, tally()), st.st_size, allocated)
            unit["files"] += 1
            unit["logical_bytes"] += st.st_size
            unit["allocated_bytes"] += allocated
            unit["newest_change"] = max(unit["newest_change"], changed)
            unit["primary_reasons"][reason] = unit["primary_reasons"].get(reason, 0) + 1
    ranked = sorted(units.values(), key=lambda u: (-u["allocated_bytes"], -u["logical_bytes"], u["unit"]))
    for u in ranked:
        u["writers"].sort(key=lambda w: w["pid"])
        u["newest_change"] = iso(u["newest_change"]) if u["files"] else None
    out["units"] = ranked[:unit_limit]
    for u in ranked[unit_limit:]:
        out["units_omitted"]["units"] += 1
        out["units_omitted"]["files"] += u["files"]
        out["units_omitted"]["allocated_bytes"] += u["allocated_bytes"]
    return out


def mount_reading(work_mount, reserve_bytes):
    reading = {"path": str(work_mount), "reserve_bytes": reserve_bytes}
    try:
        fs = os.statvfs(work_mount)
    except OSError as error:
        reading["unread"] = error.strerror or str(error)
        return reading
    reading["capacity_bytes"] = fs.f_frsize * fs.f_blocks
    reading["free_bytes"] = fs.f_frsize * fs.f_bavail
    reading["shortfall_bytes"] = max(0, reserve_bytes - reading["free_bytes"])
    return reading


def run_inventory(policy, work_mount, proc_root, now, reserve_bytes, unit_limit=40):
    work_mount = Path(work_mount)
    live = read_liveness(proc_root)
    live["complete"] = live["table_read"] and not live["unreadable"]
    uncertainty = {"liveness_unknown_files": 0, "liveness_unknown_bytes": 0, "unreadable_dirs": 0,
                   "unreadable_sample": [], "vanished": 0, "other_filesystem": 0}
    seen = set()
    families = [walk_family(f, work_mount, live, now, unit_limit, seen, uncertainty) for f in policy["families"]]
    totals = {"files": 0, "logical_bytes": 0, "allocated_bytes": 0, "by_primary_reason": {}}
    for fam in families:
        # `links` too, so the per-reason files sum to files + links here as they
        # do per family; the totals named no links before (backlog ea54c3ff).
        for key in ("files", "links", "logical_bytes", "allocated_bytes"):
            totals[key] = totals.get(key, 0) + fam[key]
        for reason, t in fam["by_primary_reason"].items():
            into = totals["by_primary_reason"].setdefault(reason, tally())
            for key in into:
                into[key] += t[key]
    return {
        "schema": SCHEMA,
        "mode": "dry-run",
        "mutated": False,
        "actions": "none: " + INVENTORY_ONLY + "; no archive, compression, deduplication or expiry is built",
        "observed_at": iso(now),
        "policy": {"path": policy["path"], "version": policy["version"], "sha256": policy["sha256"]},
        "mount": mount_reading(work_mount, reserve_bytes),
        "liveness": {
            "table_read": live["table_read"],
            "processes_read": live["processes_read"],
            "unreadable_processes": len(live["unreadable"]),
            "unreadable_sample": live["unreadable"][:SAMPLE],
            "open_regular_files": len(live["open_by"]),
        },
        "families": families,
        "totals": totals,
        "candidates": [],
        "candidate_bytes": 0,
        "uncertainty": uncertainty,
        "reasons": REASONS,
    }


def write_atomic(path, body):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            os.fchmod(handle.fileno(), 0o640)
            handle.write(body)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def append_ring(path, line, keep):
    path = Path(path)
    try:
        lines = path.read_text().splitlines()
    except FileNotFoundError:
        lines = []
    lines = (lines + [line])[-keep:]
    write_atomic(path, ("\n".join(lines) + "\n").encode())


def gib(n):
    return f"{n / (1 << 30):.2f} GiB"


def summary(report):
    t = report["totals"]
    held = ", ".join(f"{r} {gib(v['allocated_bytes'])}" for r, v in sorted(t["by_primary_reason"].items()))
    m = report["mount"]
    free = f"{gib(m['free_bytes'])} free of {gib(m['capacity_bytes'])}" if "free_bytes" in m else f"unread ({m.get('unread')})"
    u, live = report["uncertainty"], report["liveness"]
    return (f"artifact-inventory: dry run, inventory only — {t['files']} files, {gib(t['allocated_bytes'])} allocated "
            f"in {len(report['families'])} families; {len(report['candidates'])} candidates; retained by reason: "
            f"{held or 'nothing'}; liveness unknown for {u['liveness_unknown_files']} files "
            f"({live['unreadable_processes']} processes unreadable, table read: {live['table_read']}); "
            f"{u['unreadable_dirs']} unreadable paths; {m['path']} {free}, reserve {gib(m['reserve_bytes'])}")


def main(argv=None):
    parser = argparse.ArgumentParser(description="The artifact lifecycle's dry run: account, never act.")
    parser.add_argument("--policy", required=True)
    parser.add_argument("--work-mount", required=True)
    parser.add_argument("--proc-root", default="/proc")
    parser.add_argument("--reserve-gb", type=int, default=0)
    parser.add_argument("--report")
    parser.add_argument("--ring")
    parser.add_argument("--ring-keep", type=int, default=720)
    parser.add_argument("--unit-limit", type=int, default=40)
    parser.add_argument("--now", type=float)
    args = parser.parse_args(argv)
    if args.ring_keep < 1 or args.unit_limit < 1 or args.reserve_gb < 0:
        print("artifact-inventory: --ring-keep and --unit-limit must be positive, --reserve-gb not negative")
        return 64
    try:
        policy = load_policy(args.policy)
    except (PolicyError, OSError) as error:
        print(f"artifact-inventory: policy refused, nothing written: {error}")
        return 64
    now = time.time() if args.now is None else args.now
    report = run_inventory(policy, Path(args.work_mount), Path(args.proc_root), now,
                           args.reserve_gb << 30, args.unit_limit)
    body = (json.dumps(report, indent=1, sort_keys=True) + "\n").encode()
    for target, write in ((args.report, lambda p: write_atomic(p, body)),
                          (args.ring, lambda p: append_ring(p, ring_line(report), args.ring_keep))):
        if not target:
            continue
        try:
            write(target)
        except OSError as error:
            print(f"artifact-inventory: could not write {target}: {error}")
            return 73
    print(summary(report))
    return 0


def ring_line(report):
    t, m = report["totals"], report["mount"]
    return json.dumps({
        "observed_at": report["observed_at"],
        "policy_sha256": report["policy"]["sha256"],
        "files": t["files"],
        "allocated_bytes": t["allocated_bytes"],
        "free_bytes": m.get("free_bytes"),
        "liveness_unknown_files": report["uncertainty"]["liveness_unknown_files"],
        "families": {f["name"]: f["allocated_bytes"] for f in report["families"]},
    }, sort_keys=True)


if __name__ == "__main__":
    sys.exit(main())
