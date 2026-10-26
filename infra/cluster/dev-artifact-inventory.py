#!/usr/bin/python3 -I
"""dev-artifact-inventory — the agent-artifact lifecycle: its dry run, its
completion join, its archive actuator and its restore door.

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

  open-writer          a process holds it open: active work
  recently-changed     changed inside the family's settle window
  reader-unqualified   the family's readers have not been measured
  completion-unproven  the family declares no completion join
  liveness-unknown     some process could not be read, so no file can be
                       shown free of a writer (a missing heartbeat is not
                       a dead writer)
  owner-unjoined       the unit names no run the join can resolve
  owner-unknown        the record holds no agent run by that id
  owner-unread         the record could not be read (or the pass's read
                       budget ran out): unknown, so retained
  run-open             the owning run is not closed
  run-unreported       the run closed without its report recorded
  referenced-by-open-record  an open packet names the run or its files
  below-archive-floor  one block or less: an archive cannot free a byte
  candidate            complete, unreferenced, settled, unwritten

So the report answers the operator's question when the reserve is
short — is it active work, an unqualified reader, an owner that has not
finished, or evidence nobody has proven complete — with bytes and a
named owner for each.

THE JOIN (policy version 2, `completion = "agent-run"`). Completion is
read from the system of record and never inferred from age, a filename
or a quiet process table: a unit is complete only when the run its
directory names (`run-<8 hex>`) is CLOSED with its `reported` step
completed, and NO non-terminal packet's text names that run — an open
incident, an in-flight gate or an item still quoting the evidence holds
it. The run is read first and the open set only if some run completed,
so a pass with nothing complete costs one small read per unit (bounded
by --max-reads) and no list at all.

THE ACTION (`action = "archive"`, only where the reader is measured as
`plain-path` and a join proves completion; the loader refuses any other
pairing). Lossless and reversible, never an expiry: under an exclusive
claim (a kernel lock beside the manifest, so a dead pass holds nothing)
each candidate is revalidated — same device, inode, size, mtime and
ctime as planned, no link anywhere on its path, no open writer in a
FRESH read of the process table, its owner re-read and still complete
— then gzip-streamed to a temporary name on the same filesystem, the
archive decoded in full and its sha256 and length compared to the
original's, fsynced, linked into place without clobbering, and an
`archived` line appended to the manifest and fsynced and read back.
Only then is the plain copy released (revalidated once more), and a
`released` line follows. An interruption at any phase is recovered by
the next pass, once: an `archived` line with no `released` after it is
re-verified and finished, a partial temporary beside its intact
original is cleared, and anything that does not verify — an archive
that will not decode, a plain file that changed after its archive was
written, a partial with no original — is KEPT and reported as a
conflict, never guessed at.

THE RESTORE DOOR (`--restore PATH`). The family's readers open literal
paths (artifact-lifecycle.toml, reader_evidence), so an archived file is
restored before use: decoded to a temporary, its bytes verified against
the manifest's digest, its mode, owner and times put back, linked into
place without clobbering, a `restored` line recorded, and only then the
archive removed. It refuses over a standing file.

WHAT IT NEVER DOES. No file is expired, truncated or excerpted; no link
is followed (a linked root, or a linked directory on the way to one, is
refused; a link inside a root is counted as a link and its target never
read). What it cannot read is UNCERTAINTY, counted and named, and every
unknown retains. The tree's policy grants no action: activation is a
change to that file, through a held car (see its header).

Exit: 0 written; 64 the policy or arguments were refused (nothing
written); 65 the report was written and the action was refused or met a
conflict a person must read; 73 the report or ring could not be written.
"""
import argparse
import fcntl
import gzip
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
import zlib
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

SCHEMA = "artifact-inventory/2"
FIELDS = {
    "name": str,
    "root": str,
    "owner": str,
    "unit_depth": int,
    "settle_hours": int,
    "reader": str,
    "reader_evidence": str,
}
# Version 2's optional keys. Version 1 knows none of them.
OPTIONAL = {"completion": str, "action": str, "archive_budget_mb": int}
READERS = ("plain-path", "unmeasured")
COMPLETIONS = ("agent-run",)
ACTIONS = ("archive",)
NAME = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
RUN_DIR = re.compile(r"run-([0-9a-f]{8})")
ARCHIVE_SUFFIX = ".archived.gz"
PARTIAL = re.compile(r"\.(.+)\.(archive|restore)-tmp-[0-9]+")
INVENTORY_ONLY = "policy version 1 is inventory only and grants no action"
VERSION_TWO = "policy version 2 knows only the completion join and the archive action"
REASONS = {
    "open-writer": "a process holds the file open; its writer is active",
    "recently-changed": "changed inside the family's settle window; age is necessary and never sufficient",
    "reader-unqualified": "the family's readers have not been measured, so no archived form is known to be readable by them",
    "completion-unproven": "the family declares no completion join, so no record proves the file complete",
    "liveness-unknown": "some process could not be read, so no file can be shown free of a writer",
    "owner-unjoined": "the unit names no run the completion join can resolve; its owner must say",
    "owner-unknown": "the system of record holds no agent run by the id the unit names",
    "owner-unread": "the system of record could not be read for this owner, or the pass's read budget ran out",
    "run-open": "the owning run is not closed",
    "run-unreported": "the owning run closed without its report recorded",
    "referenced-by-open-record": "an open packet names the run or its files",
    "below-archive-floor": "one block or less: an archive cannot free a byte",
    "candidate": "complete, unreferenced, settled and unwritten: the family's action may take it",
    "archive": "a lifecycle archive: the complete original, compressed; --restore gives the plain path back",
    "archive-partial": "a temporary a lifecycle pass left; the next acting pass clears it beside an intact original",
    "symlink-not-followed": "a symbolic link: counted, never followed, its target never read",
    "not-a-regular-file": "a fifo, socket or device: counted, never read",
}
SAMPLE = 20
BLOCK = 4096
WORK_MARGIN = 16 << 20
CHUNK = 1 << 20
OPEN_PAGE = 200
OPEN_PAGES_MAX = 50


class PolicyError(ValueError):
    pass


class LifecycleError(RuntimeError):
    pass


def load_policy(path):
    path = Path(path)
    raw = path.read_bytes()
    try:
        data = tomllib.loads(raw.decode())
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise PolicyError(f"{path}: does not parse: {error}") from None
    version = data.get("version")
    if type(version) is not int or version not in (1, 2):
        raise PolicyError(f"{path}: version {version!r} is not one this inventory reads (it reads versions 1 and 2)")
    grants = INVENTORY_ONLY if version == 1 else VERSION_TWO
    known = dict(FIELDS, **OPTIONAL) if version == 2 else FIELDS
    for key in sorted(data):
        if key not in ("version", "family"):
            raise PolicyError(f"{path}: unknown key '{key}': {grants}")
    families = data.get("family")
    if not isinstance(families, list) or not families:
        raise PolicyError(f"{path}: declares no [[family]]")
    out, roots = [], []
    for index, fam in enumerate(families):
        where = f"{path}: family {fam.get('name', index)!r}"
        for key in fam:
            if key not in known:
                raise PolicyError(f"{where}: unknown key '{key}': {grants}")
        for key, kind in FIELDS.items():
            if key not in fam:
                raise PolicyError(f"{where}: missing '{key}'")
            if type(fam[key]) is not kind:
                raise PolicyError(f"{where}: '{key}' must be a {kind.__name__}")
        for key, kind in OPTIONAL.items():
            if key in fam and type(fam[key]) is not kind:
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
        completion, action = fam.get("completion"), fam.get("action")
        if completion is not None and completion not in COMPLETIONS:
            raise PolicyError(f"{where}: completion must be one of {', '.join(COMPLETIONS)}, got {completion!r}")
        if action is not None:
            if action not in ACTIONS:
                raise PolicyError(f"{where}: action must be one of {', '.join(ACTIONS)}, got {action!r}; "
                                  "no expiry or deletion is built")
            if fam["reader"] != "plain-path":
                raise PolicyError(f"{where}: may not be archived: its reader is {fam['reader']}, and an archive "
                                  "is qualified only for a reader measured to restore from a plain path")
            if completion is None:
                raise PolicyError(f"{where}: an action needs a completion join; age alone never proves completion")
            if fam.get("archive_budget_mb", 0) < 1:
                raise PolicyError(f"{where}: an action needs 'archive_budget_mb' of at least 1, its bound per pass")
        elif "archive_budget_mb" in fam:
            raise PolicyError(f"{where}: 'archive_budget_mb' without an action bounds nothing")
        mine = PurePosixPath(root)
        for other_name, other in roots:
            if mine == other or other in mine.parents or mine in other.parents:
                raise PolicyError(f"{path}: the root of {fam['name']!r} and of {other_name!r} lie inside one another; a byte is counted once")
        roots.append((fam["name"], mine))
        out.append(dict(fam, completion=completion, action=action))
    return {"path": str(path), "version": version, "sha256": hashlib.sha256(raw).hexdigest(), "families": out}


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
        live["complete"] = False
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
    live["complete"] = not live["unreadable"]
    return live


def tally():
    return {"files": 0, "logical_bytes": 0, "allocated_bytes": 0}


def add(t, logical, allocated):
    t["files"] += 1
    t["logical_bytes"] += logical
    t["allocated_bytes"] += allocated


def ident(st):
    """What revalidation compares: the same file, unchanged."""
    return (st.st_dev, st.st_ino, st.st_size, st.st_mtime_ns, st.st_ctime_ns)


def run_of(unit):
    """The run a unit names: its deepest `run-<8 hex>` component."""
    for part in reversed(unit.split("/")):
        match = RUN_DIR.fullmatch(part)
        if match:
            return match.group(1)
    return None


def account(out, unit, reason, logical, allocated):
    add(out["by_primary_reason"].setdefault(reason, tally()), logical, allocated)
    unit["primary_reasons"][reason] = unit["primary_reasons"].get(reason, 0) + 1


def walk_family(fam, work_mount, live, now, seen, uncertainty):
    """Walk one family. Files whose reason waits on the completion join
    are held in `_pending` (per unit) and accounted after it."""
    out = {
        "name": fam["name"], "root": fam["root"], "owner": fam["owner"], "reader": fam["reader"],
        "reader_evidence": fam["reader_evidence"], "settle_hours": fam["settle_hours"],
        "completion": fam.get("completion"), "action": fam.get("action"), "status": "read",
        "files": 0, "links": 0, "logical_bytes": 0, "allocated_bytes": 0, "hardlinked_paths": 0,
        "by_primary_reason": {}, "units": [], "units_omitted": {"units": 0, "files": 0, "allocated_bytes": 0},
        "_units": {}, "_partials": [], "_archives": 0,
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
    units = out["_units"]
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
                                           "primary_reasons": {}, "_pending": []})
            if not stat.S_ISREG(st.st_mode):
                reason = "symlink-not-followed" if stat.S_ISLNK(st.st_mode) else "not-a-regular-file"
                out["links"] += 1
                account(out, unit, reason, 0, 0)
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
            if entry.name.endswith(ARCHIVE_SUFFIX):
                reason = "archive"
                out["_archives"] += 1
            elif PARTIAL.fullmatch(entry.name):
                reason = "archive-partial"
                out["_partials"].append(entry.path)
            elif writers:
                reason = "open-writer"
                unit["open_files"] += 1
                for w in writers:
                    if w not in unit["writers"] and len(unit["writers"]) < SAMPLE:
                        unit["writers"].append(w)
            elif now - changed < settle:
                reason = "recently-changed"
            elif fam["reader"] == "unmeasured":
                reason = "reader-unqualified"
            elif not fam.get("completion"):
                reason = "completion-unproven"
            elif not live["complete"]:
                reason = "liveness-unknown"
            else:
                reason = None  # waits on the join
            if not live["complete"]:
                uncertainty["liveness_unknown_files"] += 1
                uncertainty["liveness_unknown_bytes"] += allocated
            out["files"] += 1
            out["logical_bytes"] += st.st_size
            out["allocated_bytes"] += allocated
            unit["files"] += 1
            unit["logical_bytes"] += st.st_size
            unit["allocated_bytes"] += allocated
            unit["newest_change"] = max(unit["newest_change"], changed)
            if reason is None:
                unit["_pending"].append((entry.path, ident(st), st.st_size, allocated))
            else:
                account(out, unit, reason, st.st_size, allocated)
    return out


# ---------------------------------------------------------------------
# THE COMPLETION JOIN — read from the system of record, never inferred.
# ---------------------------------------------------------------------

def curl_reader(curl, url, user, header=None, timeout=120):
    """A GET through the reclaim pass's own curl helper (boss-api-curl.sh,
    which waits out a deploy roll), signed as the pass and carrying its
    machine-token header file. Answers (http code, JSON body or None),
    or (None, None) when nothing answered."""
    def read(path):
        argv = [curl, "-sS", "-H", f"x-boss-user: {user}"]
        if header:
            argv += ["-H", header]
        argv += ["-w", "\n%{http_code}", url.rstrip("/") + path]
        try:
            done = subprocess.run(argv, capture_output=True, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired):
            return None, None
        if done.returncode != 0:
            return None, None
        body, _, code = done.stdout.rpartition(b"\n")
        try:
            code = int(code.strip())
        except ValueError:
            return None, None
        try:
            return code, json.loads(body)
        except ValueError:
            return code, None
    return read


def read_run(record, id8):
    code, body = record(f"/api/jobs/{id8}")
    if code == 404:
        return {"reason": "owner-unknown", "why": f"the record holds no job {id8}"}
    if code != 200 or not isinstance(body, dict):
        return {"reason": "owner-unread", "why": f"the read of job {id8} answered {code if code else 'nothing'}"}
    job_id, kind = str(body.get("id", "")), body.get("kind")
    if kind != "agent-run" or not job_id.startswith(id8):
        return {"reason": "owner-unknown", "why": f"job {job_id or id8} is a {kind}, not an agent run"}
    steps = body.get("steps") if isinstance(body.get("steps"), list) else []
    reported = next((s for s in steps if isinstance(s, dict) and s.get("spec_slug") == "reported"), None)
    info = {"id": job_id, "status": body.get("status"),
            "reported": bool(reported and reported.get("status") == "completed"),
            "reported_at": reported.get("completed_at") if reported else None}
    if info["status"] != "closed":
        return {"reason": "run-open", "record": info}
    if not info["reported"]:
        return {"reason": "run-unreported", "record": info}
    return {"reason": "complete", "record": info}


def read_open_records(record):
    """Every non-terminal packet, whole (step metadata included), paged
    until the rows equal the total. Anything less is unread, never a
    smaller world."""
    rows, offset = [], 0
    for _ in range(OPEN_PAGES_MAX):
        code, body = record(f"/api/jobs?terminal=false&full=true&limit={OPEN_PAGE}&offset={offset}")
        if code != 200 or not isinstance(body, dict) or not isinstance(body.get("data"), list) \
                or type(body.get("total")) is not int:
            return None, f"the open packets could not be read whole (answered {code if code else 'nothing'})"
        page = body["data"]
        rows += page
        offset += len(page)
        if len(rows) >= body["total"]:
            return rows, None
        if not page:
            return None, f"the open packets ended at {len(rows)} of {body['total']}"
    return None, f"more than {OPEN_PAGE * OPEN_PAGES_MAX} open packets; the set was not read whole"


def join_units(names, record, max_reads):
    """name -> {reason, run, record?, referenced_by?, why?}"""
    out, runs, reads = {}, {}, 0
    for name in sorted(names):
        id8 = run_of(name)
        if id8 is None:
            out[name] = {"reason": "owner-unjoined", "why": "no run-<8 hex> directory in the unit's path"}
            continue
        if record is None:
            out[name] = {"reason": "owner-unread", "run": id8, "why": "no system of record named"}
            continue
        if id8 not in runs:
            if reads >= max_reads:
                out[name] = {"reason": "owner-unread", "run": id8,
                             "why": f"the pass's budget of {max_reads} record reads ran out"}
                continue
            reads += 1
            runs[id8] = read_run(record, id8)
        out[name] = dict(runs[id8], run=id8)
    complete = [n for n, j in out.items() if j["reason"] == "complete"]
    if complete:
        rows, why = read_open_records(record)
        texts = None if rows is None else [(str(r.get("id", "?")), json.dumps(r, sort_keys=True)) for r in rows]
        for name in complete:
            if texts is None:
                out[name].update(reason="owner-unread", why=why)
                continue
            hits = [rid for rid, text in texts if out[name]["run"] in text]
            if hits:
                out[name].update(reason="referenced-by-open-record", referenced_by=hits[:SAMPLE])
            else:
                out[name]["reason"] = "candidate"
    return out, reads


# ---------------------------------------------------------------------
# THE MANIFEST AND THE CLAIM.
# ---------------------------------------------------------------------

def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def load_manifest(manifest):
    """Every line, parsed. Missing is empty; one bad line is a refusal."""
    try:
        text = Path(manifest).read_text()
    except FileNotFoundError:
        return None
    entries = []
    for number, line in enumerate(text.split("\n"), 1):
        if not line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            raise LifecycleError(f"manifest {manifest} line {number} does not parse") from None
        if not isinstance(entry, dict) or not isinstance(entry.get("action"), str) \
                or not isinstance(entry.get("path"), str):
            raise LifecycleError(f"manifest {manifest} line {number} is not a lifecycle entry")
        entries.append(entry)
    if text and not text.endswith("\n"):
        raise LifecycleError(f"manifest {manifest} ends in a torn line")
    return entries


def append_manifest(manifest, entry):
    """Append one line, fsync it and its directory, and read it back."""
    line = (json.dumps(entry, sort_keys=True) + "\n").encode()
    fd = os.open(manifest, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o640)
    try:
        written = os.write(fd, line)
        if written != len(line):
            raise LifecycleError(f"manifest {manifest}: short write")
        os.fsync(fd)
        size = os.fstat(fd).st_size
    finally:
        os.close(fd)
    fsync_dir(Path(manifest).parent)
    rfd = os.open(manifest, os.O_RDONLY)
    try:
        if os.pread(rfd, len(line), size - len(line)) != line:
            raise LifecycleError(f"manifest {manifest}: the entry did not read back")
    finally:
        os.close(rfd)


def open_claim(manifest, block):
    path = Path(str(manifest) + ".lock")
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = open(path, "a")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | (0 if block else fcntl.LOCK_NB))
    except BlockingIOError:
        handle.close()
        return None
    return handle


def latest_by_path(entries):
    last = {}
    for entry in entries:
        last[entry["path"]] = entry
    return last


def last_archived(entries, rel):
    return next((e for e in reversed(entries) if e["path"] == rel and e["action"] == "archived"), None)


# ---------------------------------------------------------------------
# THE ACTUATOR.
# ---------------------------------------------------------------------

def decoded_digest(path):
    """Decode an archive in full: (sha256, length) of what it holds."""
    sha, n = hashlib.sha256(), 0
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as raw, gzip.GzipFile(fileobj=raw, mode="rb") as decoded:
        while True:
            chunk = decoded.read(CHUNK)
            if not chunk:
                break
            sha.update(chunk)
            n += len(chunk)
    return sha.hexdigest(), n


def file_digest(path):
    sha, n = hashlib.sha256(), 0
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as handle:
        while True:
            chunk = handle.read(CHUNK)
            if not chunk:
                break
            sha.update(chunk)
            n += len(chunk)
    return sha.hexdigest(), n


def keep_owner(fd, uid, gid):
    """Give a new file its original's owner, or prove it already has it."""
    try:
        os.fchown(fd, uid, gid)
        return True
    except PermissionError:
        st = os.fstat(fd)
        return (st.st_uid, st.st_gid) == (uid, gid)


class Refused(Exception):
    def __init__(self, reason, detail=""):
        super().__init__(reason)
        self.reason, self.detail = reason, detail


def release_plain(path, planned):
    """Release the plain copy, only if it is still exactly what was
    archived."""
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        return
    if not stat.S_ISREG(st.st_mode) or ident(st) != planned:
        raise Refused("changed-before-release", "the plain file changed after its archive was written; both kept")
    os.unlink(path)
    fsync_dir(Path(path).parent)


def archive_one(ctx, fam, join, path, planned):
    """One candidate, every phase revalidated. Returns the manifest entry
    on success; raises Refused with the plain copy intact otherwise."""
    work = ctx["work_real"]
    path = Path(path)
    rel = str(path.relative_to(ctx["work_mount"]))
    parent = path.parent
    if os.path.realpath(parent) != os.path.join(work, str(Path(rel).parent)):
        raise Refused("path-escaped", "a directory on the path is no longer the one planned")
    try:
        src = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        raise Refused("changed-since-plan", "the planned file is gone or is now a link") from None
    tmp, tfd = parent / f".{path.name}.archive-tmp-{os.getpid()}", None
    try:
        st = os.fstat(src)
        if not stat.S_ISREG(st.st_mode) or ident(st) != planned:
            raise Refused("changed-since-plan", "not the file the plan saw")
        if st.st_nlink != 1:
            raise Refused("hardlinked", "its bytes are shared with another path")
        if (st.st_dev, st.st_ino) in ctx["live"]["open_by"]:
            raise Refused("open-writer", "a process opened it since the plan")
        archive = parent / (path.name + ARCHIVE_SUFFIX)
        if os.path.lexists(archive):
            raise Refused("archive-name-taken", f"{archive.name} already stands")
        fs = os.statvfs(parent)
        if fs.f_bavail * fs.f_frsize < st.st_size + WORK_MARGIN:
            raise Refused("no-working-space", "too little free space to write the archive beside it")
        tfd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        sha, n = hashlib.sha256(), 0
        with os.fdopen(os.dup(tfd), "wb") as sink, \
                gzip.GzipFile(filename="", mode="wb", fileobj=sink, mtime=0) as packed:
            while True:
                chunk = os.read(src, CHUNK)
                if not chunk:
                    break
                sha.update(chunk)
                n += len(chunk)
                packed.write(chunk)
        os.fsync(tfd)
        if ident(os.fstat(src)) != planned or n != st.st_size:
            raise Refused("changed-while-reading", "the file changed while it was read")
        digest = sha.hexdigest()
        if decoded_digest(tmp) != (digest, n):
            raise Refused("hash-mismatch", "the archive does not decode to the original's bytes")
        os.fchmod(tfd, stat.S_IMODE(st.st_mode))
        if not keep_owner(tfd, st.st_uid, st.st_gid):
            raise Refused("cannot-preserve-ownership", "the archive could not be given the original's owner")
        os.fsync(tfd)
        packed_alloc = os.fstat(tfd).st_blocks * 512
        if packed_alloc >= st.st_blocks * 512:
            raise Refused("no-saving", "the archive would take as many blocks as the original")
        try:
            os.link(tmp, archive)
        except FileExistsError:
            raise Refused("archive-name-taken", f"{archive.name} appeared while the archive was written") from None
        os.unlink(tmp)
        fsync_dir(parent)
    except BaseException:
        # The temporary is this pass's own partial write and the original
        # is untouched, so it goes; a pass that DIED leaves it for the
        # next one's recovery instead.
        if tfd is not None:
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
        raise
    finally:
        os.close(src)
        if tfd is not None:
            os.close(tfd)
    entry = {
        "action": "archived", "at": iso(ctx["now"]), "path": rel,
        "archive": str(archive.relative_to(ctx["work_mount"])),
        "family": fam["name"], "owner": fam["owner"], "unit": join["unit"],
        "run": {"id": join["run"], "job": join["record"]["id"], "status": join["record"]["status"],
                "reported_at": join["record"]["reported_at"]},
        "dev": st.st_dev, "ino": st.st_ino, "logical_bytes": n, "allocated_bytes": st.st_blocks * 512,
        "archive_allocated_bytes": packed_alloc, "archive_bytes": os.lstat(archive).st_size,
        "sha256": digest, "mode": stat.S_IMODE(st.st_mode), "uid": st.st_uid, "gid": st.st_gid,
        "atime_ns": st.st_atime_ns, "mtime_ns": st.st_mtime_ns,
        "policy": ctx["policy_id"],
    }
    append_manifest(ctx["manifest"], entry)
    release_plain(path, planned)
    append_manifest(ctx["manifest"], {"action": "released", "at": iso(ctx["now"]), "path": rel,
                                      "sha256": digest, "archive": entry["archive"]})
    return entry


def conflict(act, kind, path, detail):
    act["conflicts"].append({"kind": kind, "path": path, "detail": detail})


def recover(ctx, act, entries, partials):
    """Finish or report what an interrupted pass left. Returns the paths
    it handled, so the candidates loop does not touch them again."""
    handled, work = set(), Path(ctx["work_mount"])
    for rel, last in sorted(latest_by_path(entries).items()):
        if last["action"] != "archived":
            continue
        handled.add(str(work / rel))
        plain, archive = work / rel, work / last["archive"]
        plain_here = os.path.lexists(plain)
        try:
            astat = os.lstat(archive)
            verified = stat.S_ISREG(astat.st_mode) and \
                decoded_digest(archive) == (last["sha256"], last["logical_bytes"])
        except (OSError, EOFError, zlib.error, gzip.BadGzipFile):
            verified = False
        if not verified:
            conflict(act, "archive-unverified", rel, "an archived entry whose archive is missing or does not decode; "
                     + ("the plain copy is kept" if plain_here else "NO plain copy stands either"))
            continue
        if plain_here:
            try:
                pst = os.lstat(plain)
                same = stat.S_ISREG(pst.st_mode) and file_digest(plain) == (last["sha256"], last["logical_bytes"])
            except OSError:
                same = False
            if not same:
                conflict(act, "plain-differs-from-archive", rel,
                         "the plain file no longer matches its archive; both kept for a person to judge")
                continue
            if (pst.st_dev, pst.st_ino) in ctx["live"]["open_by"]:
                conflict(act, "open-writer-on-archived", rel, "a process holds the plain copy open; release waits")
                continue
            try:
                release_plain(plain, ident(pst))
            except Refused as refused:
                conflict(act, refused.reason, rel, refused.detail)
                continue
        append_manifest(ctx["manifest"], {"action": "released", "at": iso(ctx["now"]), "path": rel,
                                          "sha256": last["sha256"], "archive": last["archive"], "recovered": True})
        act["recovered"] += 1
    for partial in sorted(partials):
        partial = Path(partial)
        match = PARTIAL.fullmatch(partial.name)
        original = partial.parent / match.group(1)
        rel = str(partial.relative_to(work))
        beside = original if match.group(2) == "archive" else original.with_name(original.name + ARCHIVE_SUFFIX)
        try:
            ost = os.lstat(beside)
        except FileNotFoundError:
            ost = None
        if ost is None or not stat.S_ISREG(ost.st_mode):
            conflict(act, "partial-without-original", rel, f"no {beside.name} stands beside it; kept")
            continue
        os.unlink(partial)
        fsync_dir(partial.parent)
        act["partials_cleared"] += 1
    return handled


def refuse(act, reason, rel, detail=""):
    act["refused"][reason] = act["refused"].get(reason, 0) + 1
    if len(act["refused_sample"]) < SAMPLE:
        act["refused_sample"].append({"path": rel, "reason": reason, "detail": detail})


def act_pass(ctx, plan, archives_seen, partials, record, max_reads, proc_root):
    act = {"status": "ran", "archived_files": 0, "logical_bytes": 0, "allocated_bytes_before": 0,
           "archive_allocated_bytes": 0, "saved_allocated_bytes": 0, "budget_bytes": 0,
           "refused": {}, "refused_sample": [], "archived_sample": [], "recovered": 0,
           "partials_cleared": 0, "conflicts": [], "manifest": str(ctx["manifest"]) if ctx["manifest"] else None}
    budgets = {fam["name"]: fam["archive_budget_mb"] << 20 for fam in ctx["acting"]}
    act["budget_bytes"] = sum(budgets.values())
    if ctx["manifest"] is None:
        act.update(status="refused", refusal="no manifest named; nothing is archived without one")
        return act
    claim = open_claim(ctx["manifest"], block=False)
    if claim is None:
        act.update(status="refused", refusal="another lifecycle pass holds the claim")
        return act
    with claim:
        try:
            entries = load_manifest(ctx["manifest"])
        except LifecycleError as error:
            act.update(status="refused", refusal=str(error))
            return act
        if entries is None and archives_seen:
            act.update(status="refused", refusal=f"{archives_seen} archive(s) stand but the manifest "
                                                 f"{ctx['manifest']} is absent; nothing acts until it is found")
            return act
        live = read_liveness(proc_root)
        if not live["complete"]:
            act.update(status="refused", refusal="the process table could not be read whole at act time")
            return act
        ctx["live"] = live
        try:
            handled = recover(ctx, act, entries or [], partials)
        except LifecycleError as error:
            act.update(status="refused", refusal=f"recovery stopped: {error}")
            return act
        units = sorted({unit for _, unit, _, _, _, _ in plan})
        fresh, _ = join_units(units, record, max_reads) if units else ({}, 0)
        spent = dict.fromkeys(budgets, 0)
        for fam, unit, path, planned, size, allocated in plan:
            rel = str(Path(path).relative_to(ctx["work_mount"]))
            if path in handled:
                continue
            now_join = fresh.get(unit, {"reason": "owner-unread"})
            if now_join["reason"] != "candidate":
                refuse(act, f"raced:{now_join['reason']}", rel, now_join.get("why", ""))
                continue
            if spent[fam["name"]] + size > budgets[fam["name"]]:
                refuse(act, "budget-exhausted", rel)
                continue
            try:
                entry = archive_one(ctx, fam, dict(now_join, unit=unit), path, planned)
            except Refused as refused:
                refuse(act, refused.reason, rel, refused.detail)
                continue
            except LifecycleError as error:
                # The manifest itself failed: nothing more acts this pass.
                act.update(status="refused", refusal=str(error))
                break
            except (OSError, EOFError, zlib.error) as error:
                refuse(act, "io-error", rel, str(error))
                continue
            spent[fam["name"]] += size
            act["archived_files"] += 1
            act["logical_bytes"] += entry["logical_bytes"]
            act["allocated_bytes_before"] += entry["allocated_bytes"]
            act["archive_allocated_bytes"] += entry["archive_allocated_bytes"]
            if len(act["archived_sample"]) < SAMPLE:
                act["archived_sample"].append({"path": rel, "sha256": entry["sha256"],
                                               "logical_bytes": entry["logical_bytes"]})
        act["saved_allocated_bytes"] = act["allocated_bytes_before"] - act["archive_allocated_bytes"]
    return act


# ---------------------------------------------------------------------
# THE RESTORE DOOR.
# ---------------------------------------------------------------------

def restore(path, manifest, work_mount, now):
    """Give an archived file back at its literal path, verified."""
    path = Path(os.path.abspath(path))
    work = Path(os.path.abspath(work_mount))
    rel = os.path.relpath(path, work)
    if rel.startswith(".."):
        raise LifecycleError(f"{path} is not under the work mount {work}")
    with open_claim(manifest, block=True):
        if os.path.lexists(path):
            raise LifecycleError(f"a file already stands at {path}; nothing restored over it")
        entries = load_manifest(manifest) or []
        last = latest_by_path(entries).get(rel)
        archived = last_archived(entries, rel)
        if last is None or last["action"] != "released" or archived is None:
            raise LifecycleError(f"no archive recorded for {rel} (its last manifest entry: "
                                 f"{last['action'] if last else 'none'})")
        archive = work / archived["archive"]
        fs = os.statvfs(path.parent)
        if fs.f_bavail * fs.f_frsize < archived["logical_bytes"] + WORK_MARGIN:
            raise LifecycleError(f"too little free space beside {path} to restore {archived['logical_bytes']} bytes")
        tmp = path.parent / f".{path.name}.restore-tmp-{os.getpid()}"
        tfd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            sha, n = hashlib.sha256(), 0
            try:
                afd = os.open(archive, os.O_RDONLY | os.O_NOFOLLOW)
                with os.fdopen(afd, "rb") as raw, gzip.GzipFile(fileobj=raw, mode="rb") as decoded:
                    while True:
                        chunk = decoded.read(CHUNK)
                        if not chunk:
                            break
                        sha.update(chunk)
                        n += len(chunk)
                        # A successful write may still be short (disk/quota
                        # exhaustion or a signal). Only installed bytes count.
                        remaining = memoryview(chunk)
                        while remaining:
                            written = os.write(tfd, remaining)
                            if written <= 0:
                                raise OSError("restore temporary write made no progress")
                            remaining = remaining[written:]
            except (OSError, EOFError, zlib.error, gzip.BadGzipFile) as error:
                raise LifecycleError(f"the archive {archive} does not decode: {error}") from None
            if (sha.hexdigest(), n) != (archived["sha256"], archived["logical_bytes"]):
                raise LifecycleError(f"the archive {archive} decodes to other bytes than the manifest recorded")
            os.fchmod(tfd, archived["mode"])
            if not keep_owner(tfd, archived["uid"], archived["gid"]):
                raise LifecycleError(f"{path} could not be given its original owner {archived['uid']}:{archived['gid']}")
            os.fsync(tfd)
            # The decoder's digest alone says nothing about the file that
            # reached disk. Preserve the sole archive unless that file agrees.
            if file_digest(tmp) != (archived["sha256"], archived["logical_bytes"]):
                raise LifecycleError(f"restore temporary {tmp} differs from the manifest")
            # Readback may update atime. Restore and persist metadata only
            # after verification, before installing the exact original.
            os.utime(tmp, ns=(archived["atime_ns"], archived["mtime_ns"]), follow_symlinks=False)
            os.fsync(tfd)
            try:
                os.link(tmp, path)
            except FileExistsError:
                raise LifecycleError(f"a file appeared at {path}; nothing restored over it") from None
        except BaseException:
            os.close(tfd)
            try:
                os.unlink(tmp)
            except FileNotFoundError:
                pass
            raise
        os.close(tfd)
        os.unlink(tmp)
        fsync_dir(path.parent)
        append_manifest(manifest, {"action": "restored", "at": iso(now), "path": rel,
                                   "sha256": archived["sha256"], "archive": archived["archive"]})
        os.unlink(archive)
        fsync_dir(path.parent)
    return {"path": str(path), "sha256": archived["sha256"], "logical_bytes": archived["logical_bytes"]}


# ---------------------------------------------------------------------
# THE PASS.
# ---------------------------------------------------------------------

def finish_family(out, joins, plan, candidates):
    """Account the files that waited on the join, under its answer."""
    for name, unit in out.pop("_units").items():
        pending = unit.pop("_pending")
        if not pending:
            continue
        join = joins.get((out["name"], name), {"reason": "owner-unread", "why": "not joined"})
        unit["join"] = {k: v for k, v in join.items()}
        unit_candidates = tally()
        for path, planned, size, allocated in pending:
            reason = join["reason"]
            if reason == "candidate":
                if allocated <= BLOCK:
                    reason = "below-archive-floor"
                else:
                    add(unit_candidates, size, allocated)
                    plan.append((out, name, path, planned, size, allocated))
            account(out, unit, reason, size, allocated)
        if unit_candidates["files"]:
            candidates.append({"family": out["name"], "unit": name, "run": join.get("run"), **unit_candidates})


def rank_units(out, units, unit_limit):
    ranked = sorted(units, key=lambda u: (-u["allocated_bytes"], -u["logical_bytes"], u["unit"]))
    for u in ranked:
        u.pop("_pending", None)
        u["writers"].sort(key=lambda w: w["pid"])
        u["newest_change"] = iso(u["newest_change"]) if u["files"] else None
    out["units"] = ranked[:unit_limit]
    for u in ranked[unit_limit:]:
        out["units_omitted"]["units"] += 1
        out["units_omitted"]["files"] += u["files"]
        out["units_omitted"]["allocated_bytes"] += u["allocated_bytes"]


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


def run_inventory(policy, work_mount, proc_root, now, reserve_bytes, unit_limit=40, record=None,
                  manifest=None, act=False, max_reads=200):
    work_mount = Path(work_mount)
    live = read_liveness(proc_root)
    uncertainty = {"liveness_unknown_files": 0, "liveness_unknown_bytes": 0, "unreadable_dirs": 0,
                   "unreadable_sample": [], "vanished": 0, "other_filesystem": 0}
    seen = set()
    families = [walk_family(f, work_mount, live, now, seen, uncertainty) for f in policy["families"]]
    waiting = [(fam["name"], name) for fam in families for name, u in fam["_units"].items() if u["_pending"]]
    joined, reads = join_units({name for _, name in waiting}, record, max_reads) if waiting else ({}, 0)
    joins = {(fam_name, name): joined[name] for fam_name, name in waiting}
    plan, candidates = [], []
    by_name = {f["name"]: f for f in policy["families"]}
    for fam in families:
        units = list(fam["_units"].values())
        finish_family(fam, joins, plan, candidates)
        rank_units(fam, units, unit_limit)
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
    acting = [f for f in policy["families"] if f.get("action") == "archive"]
    policy_id = {"path": policy["path"], "version": policy["version"], "sha256": policy["sha256"]}
    report = {
        "schema": SCHEMA,
        "mode": "dry-run",
        "mutated": False,
        "actions": ("none: " + (INVENTORY_ONLY if policy["version"] == 1 else "no family grants an action")
                    if not acting else "archive: " + ", ".join(f["name"] for f in acting)
                    + ("" if act else " (granted, but this is a dry run)")),
        "observed_at": iso(now),
        "policy": policy_id,
        "mount": mount_reading(work_mount, reserve_bytes),
        "liveness": {
            "table_read": live["table_read"],
            "processes_read": live["processes_read"],
            "unreadable_processes": len(live["unreadable"]),
            "unreadable_sample": live["unreadable"][:SAMPLE],
            "open_regular_files": len(live["open_by"]),
        },
        "families": [{k: v for k, v in fam.items() if not k.startswith("_")} for fam in families],
        "totals": totals,
        "candidates": sorted(candidates, key=lambda c: (c["family"], c["unit"])),
        "candidate_bytes": sum(c["allocated_bytes"] for c in candidates),
        "record_reads": reads,
        "uncertainty": uncertainty,
        "reasons": REASONS,
    }
    if act and acting:
        acting_names = {f["name"] for f in acting}
        ctx = {"work_mount": work_mount, "work_real": os.path.realpath(work_mount), "now": now,
               "manifest": Path(manifest) if manifest else None, "acting": acting, "policy_id": policy_id}
        fam_plan = [(by_name[out["name"]], unit, path, planned, size, allocated)
                    for out, unit, path, planned, size, allocated in sorted(plan, key=lambda p: p[2])
                    if out["name"] in acting_names]
        archives_seen = sum(f["_archives"] for f in families)
        partials = [p for f in families if f["name"] in acting_names for p in f["_partials"]]
        report["mode"] = "act"
        report["act"] = act_pass(ctx, fam_plan, archives_seen, partials, record, max_reads, proc_root)
        a = report["act"]
        report["mutated"] = bool(a["archived_files"] or a["recovered"] or a["partials_cleared"])
    return report


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
    fsync_dir(path.parent)


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
    if "act" in report:
        a = report["act"]
        head = (f"artifact-inventory: archive pass, {a['status']}"
                + (f" ({a['refusal']})" if a.get("refusal") else "")
                + f" — {a['archived_files']} archived, {gib(a['saved_allocated_bytes'])} allocated freed, "
                  f"{sum(a['refused'].values())} refused, {a['recovered']} recovered, "
                  f"{len(a['conflicts'])} conflicts; ")
    else:
        head = "artifact-inventory: dry run, inventory only — "
    return (head + f"{t['files']} files, {gib(t['allocated_bytes'])} allocated "
            f"in {len(report['families'])} families; {len(report['candidates'])} candidates "
            f"({gib(report['candidate_bytes'])}); retained by reason: "
            f"{held or 'nothing'}; liveness unknown for {u['liveness_unknown_files']} files "
            f"({live['unreadable_processes']} processes unreadable, table read: {live['table_read']}); "
            f"{u['unreadable_dirs']} unreadable paths; {m['path']} {free}, reserve {gib(m['reserve_bytes'])}")


def ring_line(report):
    t, m = report["totals"], report["mount"]
    a = report.get("act", {})
    return json.dumps({
        "observed_at": report["observed_at"],
        "policy_sha256": report["policy"]["sha256"],
        "files": t["files"],
        "allocated_bytes": t["allocated_bytes"],
        "free_bytes": m.get("free_bytes"),
        "liveness_unknown_files": report["uncertainty"]["liveness_unknown_files"],
        "candidate_bytes": report["candidate_bytes"],
        "archived_files": a.get("archived_files", 0),
        "saved_allocated_bytes": a.get("saved_allocated_bytes", 0),
        "families": {f["name"]: f["allocated_bytes"] for f in report["families"]},
    }, sort_keys=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description="The artifact lifecycle: account, join, archive, restore.")
    parser.add_argument("--policy")
    parser.add_argument("--work-mount", required=True)
    parser.add_argument("--proc-root", default="/proc")
    parser.add_argument("--reserve-gb", type=int, default=0)
    parser.add_argument("--report")
    parser.add_argument("--ring")
    parser.add_argument("--ring-keep", type=int, default=720)
    parser.add_argument("--unit-limit", type=int, default=40)
    parser.add_argument("--now", type=float)
    parser.add_argument("--manifest", help="the append-only archive manifest; acting and restoring need it")
    parser.add_argument("--dry-run", action="store_true", help="act on nothing, even where the policy grants it")
    parser.add_argument("--max-reads", type=int, default=200, help="record reads one pass may make for owners")
    parser.add_argument("--sor-url", help="the system of record; unnamed, every owner is unread and retained")
    parser.add_argument("--sor-user", default="")
    parser.add_argument("-H", "--sor-header", dest="sor_header",
                        help="a curl -H argument for every record read: the machine token's @file")
    parser.add_argument("--via", dest="curl", default="curl",
                        help="the curl to read the record through (the pass hands its boss-api-curl.sh)")
    parser.add_argument("--restore", metavar="PATH", help="restore one archived file at its plain path")
    args = parser.parse_args(argv)
    now = time.time() if args.now is None else args.now
    if args.restore:
        if not args.manifest:
            print("artifact-inventory: --restore needs --manifest")
            return 64
        try:
            done = restore(args.restore, args.manifest, args.work_mount, now)
        except (LifecycleError, OSError) as error:
            print(f"artifact-inventory: restore refused, nothing installed: {error}")
            return 65
        print(json.dumps(done, sort_keys=True))
        return 0
    if not args.policy:
        print("artifact-inventory: --policy is required")
        return 64
    if args.ring_keep < 1 or args.unit_limit < 1 or args.reserve_gb < 0 or args.max_reads < 0:
        print("artifact-inventory: --ring-keep and --unit-limit must be positive, --reserve-gb and --max-reads not negative")
        return 64
    try:
        policy = load_policy(args.policy)
    except (PolicyError, OSError) as error:
        print(f"artifact-inventory: policy refused, nothing written: {error}")
        return 64
    record = curl_reader(args.curl, args.sor_url, args.sor_user, args.sor_header) if args.sor_url else None
    report = run_inventory(policy, Path(args.work_mount), Path(args.proc_root), now,
                           args.reserve_gb << 30, args.unit_limit, record=record, manifest=args.manifest,
                           act=not args.dry_run, max_reads=args.max_reads)
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
    a = report.get("act")
    if a and (a["status"] != "ran" or a["conflicts"]):
        return 65
    return 0


if __name__ == "__main__":
    sys.exit(main())
