#!/usr/bin/env python3
"""Bounded passive gate observations; absent evidence is never a zero."""
import datetime
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time

RAW_BYTES = 2048
JOURNAL_BYTES = 2 * 1024 * 1024
RECEIPT_BYTES = 64 * 1024
RECEIPT_FILE_BYTES = 256 * 1024
RECEIPT_SAMPLES = 32
JOURNAL_ROWS = 512
ERRORS = 16
FILES = ("cpu.max", "cpuset.cpus.effective", "cpu.stat", "cpu.pressure",
         "memory.pressure", "io.pressure", "memory.current", "memory.peak", "memory.events")
IDENTITY = ("GATE_RUN_JOB_ID", "BOSS_AGENT_RUN", "BOSS_GATE_SOURCE_HEAD", "POD_NAME",
            "POD_UID", "NODE_NAME", "NATIVE_JOB_NAME", "NATIVE_JOB_UID")
RESOURCES = ("GATE_CPU_REQUEST", "GATE_CPU_LIMIT", "GATE_MEMORY_REQUEST", "GATE_MEMORY_LIMIT")
# THE GATE POSTGRES DATA VOLUME (backlog 9cd74fd5). gate-runner.yaml keeps it
# in memory with a sizeLimit and mounts it read-only where this variable says,
# so every receipt carries how full it got. Readings go to a side journal
# beside the samples: the receipt's byte bound drops the samples first
# (measured 2026-10-07: 40 of 40 receipts carried none of their ~220), so a
# number that has to reach the receipt is a summary of its own.
PGDATA_ENV = "BOSS_GATE_PGDATA"
PGDATA_BYTES = 256 * 1024
PGDATA_SERIES = 16
# At this share of its size the receipt marks the volume `full`. EVIDENCE,
# NEVER A VERDICT (review bc011e92): a reading over a threshold says nothing
# about WHICH check failed or why, and a car whose own tests fill the
# database earned its red. The mark is there for the person reading a red.
PGDATA_FULL = 0.95
# What the watcher says about itself, read back onto the receipt: one line
# when it starts, one for its first failed reading, one if it dies. Never a
# line per reading.
WATCH_BYTES = 4096


def text(value, bound=RAW_BYTES):
    return isinstance(value, str) and 0 < len(value) <= bound


def validate_browser(value):
    if not isinstance(value, dict):
        raise ValueError("browser observation must be an object")
    state = value.get("state")
    if state in ("unavailable", "invalid"):
        if set(value) == {"state", "reason"} and text(value.get("reason")) and value["reason"].strip():
            return value
    elif (state == "measured" and set(value) == {"state", "tests", "workers", "method", "source"}
          and type(value.get("tests")) is int and value["tests"] >= 0
          and type(value.get("workers")) is int and value["workers"] > 0
          and text(value.get("method")) and value["method"].strip()
          and text(value.get("source")) and value["source"].strip()):
        return value
    raise ValueError("malformed browser observation variant")


def observation(value, kind):
    if not isinstance(value, dict):
        return False
    state = value.get("state")
    if state == "unavailable":
        return set(value) == {"state", "reason"} and text(value.get("reason"))
    if kind == "reading":
        return set(value) == {"state", "raw"} and state in ("measured", "truncated") and isinstance(value.get("raw"), str) and len(value["raw"]) <= RAW_BYTES
    if kind == "control" and state == "invalid":
        return set(value) == {"state", "raw"} and isinstance(value.get("raw"), str) and len(value["raw"]) <= RAW_BYTES
    return (kind in ("control", "resource") and state == "configured"
            and set(value) == ({"state", "value", "method", "unit"} if kind == "resource" else {"state", "value", "method"})
            and type(value.get("value")) is int and value["value"] > 0
            and text(value.get("method")))


def validate_sample(row):
    """JSON syntax alone proves none of the observation's required shape."""
    if not isinstance(row, dict):
        raise ValueError("sample must be an object")
    required = ("label", "at", "monotonic_ns", "identity", "lifetime", "method",
                "workers", "resources", "files", "overlap", "child_runtime_workers")
    if set(row) != set(required):
        raise ValueError("sample fields disagree with declared shape")
    if not text(row["label"], 200) or not text(row["method"]):
        raise ValueError("invalid sample label or method")
    if not text(row["at"], 100):
        raise ValueError("invalid sample timestamp")
    try:
        instant = datetime.datetime.fromisoformat(row["at"])
        if instant.utcoffset() != datetime.timedelta(0):
            raise ValueError("timestamp must be UTC")
    except ValueError as error:
        raise ValueError("invalid sample timestamp") from error
    if type(row["monotonic_ns"]) is not int or row["monotonic_ns"] < 0:
        raise ValueError("invalid monotonic clock")
    identity = row["identity"]
    if not isinstance(identity, dict) or set(identity) != set(IDENTITY) or any(
            value is not None and not text(value) for value in identity.values()):
        raise ValueError("invalid sample identity")
    lifetime = row["lifetime"]
    if lifetime is not None and (not isinstance(lifetime, list) or len(lifetime) != 3
            or any(type(value) is not int or value < 0 for value in lifetime[:2])
            or not observation(lifetime[2], "reading")):
        raise ValueError("invalid cgroup lifetime")
    for field, keys, kind in (("files", FILES, "reading"),
                              ("workers", ("cargo", "rust_tests"), "control"),
                              ("resources", RESOURCES, "resource")):
        values = row[field]
        if not isinstance(values, dict) or set(values) != set(keys) or any(
                not observation(value, kind) for value in values.values()):
            raise ValueError("invalid " + field + " observations")
    for name, value in row["resources"].items():
        if value["state"] == "configured" and value.get("unit") != ("millicpu" if "CPU" in name else "bytes"):
            raise ValueError("invalid resource unit")
    for field in ("overlap", "child_runtime_workers"):
        if not observation(row[field], "unavailable"):
            raise ValueError("invalid " + field + " observation")
    return row


def reading(path):
    try:
        with open(path, "rb") as stream:
            raw = stream.read(RAW_BYTES + 1)
        return {"state": "truncated" if len(raw) > RAW_BYTES else "measured",
                "raw": raw[:RAW_BYTES].decode("utf-8", errors="replace")}
    except OSError as error:
        return {"state": "unavailable", "reason": str(error)}


def control(name):
    raw = os.environ.get(name)
    if raw is None:
        return {"state": "unavailable", "reason": "no explicit process control"}
    if not re.fullmatch(r"[1-9][0-9]*", raw):
        return {"state": "invalid", "raw": raw[:RAW_BYTES]}
    return {"state": "configured", "value": int(raw), "method": "effective exported process environment"}


def sample(label):
    root = Path(os.environ.get("BOSS_RUNTIME_CGROUP_ROOT", "/sys/fs/cgroup"))
    try:
        stat = root.stat()
        lifetime = [stat.st_dev, stat.st_ino, reading("/proc/sys/kernel/random/boot_id")]
    except OSError:
        lifetime = None
    identity = {name: os.environ.get(name) or None for name in IDENTITY}
    return {"label": label[:200], "at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "monotonic_ns": time.monotonic_ns(), "identity": identity, "lifetime": lifetime,
            "method": "passive cgroup v2 files and exported process controls",
            "workers": {"cargo": control("CARGO_BUILD_JOBS"), "rust_tests": control("RUST_TEST_THREADS")},
            "resources": {name: {"state": "configured", "value": int(os.environ[name]),
                                 "unit": "millicpu" if "CPU" in name else "bytes",
                                 "method": "Kubernetes resourceFieldRef"}
                          if re.fullmatch(r"[1-9][0-9]*", os.environ.get(name, ""))
                          else {"state": "unavailable", "reason": "missing or malformed declared quantity"} for name in
                          RESOURCES},
            "files": {name: reading(root / name) for name in FILES},
            "overlap": {"state": "unavailable", "reason": "no authenticated overlap observation supplied"},
            "child_runtime_workers": {"state": "unavailable", "reason": "child runtime does not publish counts"}}


def counters(row):
    source = row["files"]["cpu.stat"]
    if source["state"] != "measured":
        raise ValueError("CPU counters unavailable or truncated")
    result = {}
    for line in source["raw"].splitlines():
        key, value = line.split()
        if key in result or not value.isdigit():
            raise ValueError("malformed CPU counter")
        result[key] = int(value)
    return result


def known_lifetime(row):
    lifetime = row["lifetime"]
    return (lifetime is not None and lifetime[2]["state"] == "measured"
            and bool(lifetime[2]["raw"].strip()))


def interval(before, after):
    result = {"from": before["label"], "to": after["label"], "state": "invalid"}
    try:
        if (not known_lifetime(before) or not known_lifetime(after)
                or before["lifetime"] != after["lifetime"] or before["identity"] != after["identity"]):
            raise ValueError("lifetime or identity mismatch")
        elapsed = after["monotonic_ns"] - before["monotonic_ns"]
        if elapsed <= 0:
            raise ValueError("nonpositive monotonic interval")
        left, right = counters(before), counters(after)
        if left.keys() != right.keys() or any(right[key] < left[key] for key in left):
            raise ValueError("counter reset or changed counter set")
        if "usage_usec" not in left:
            raise ValueError("CPU usage counter absent")
        result.update(state="measured", elapsed_ns=elapsed,
                      cpu_usage_usec=right["usage_usec"] - left["usage_usec"],
                      counter_deltas={key: right[key] - left[key] for key in left})
    except (KeyError, TypeError, ValueError) as error:
        result["reason"] = str(error)
    return result


def append(path, row):
    encoded = json.dumps(row, separators=(",", ":"))
    # A bound is visible both in the retained log and in the final receipt.
    if (path.stat().st_size if path.exists() else 0) + len((encoded + "\n").encode()) > JOURNAL_BYTES:
        path.with_suffix(path.suffix + ".truncated").write_text("journal byte bound reached")
        print("gate-runtime-evidence: collection truncated at declared journal bound", file=sys.stderr)
        return
    with path.open("a") as stream:
        stream.write(encoded + "\n")
    print("gate-runtime-evidence: " + encoded)


def pgdata_journal(path):
    return path.with_suffix(path.suffix + ".pgdata")


def note_pgdata(path, label):
    """One statvfs of the data volume, appended; False when none is declared.

    A statvfs of a tmpfs is its pages in use, which is what the pod's memory
    is charged for. It reads no file of the database: the directory is 0700
    to the postgres uid, and the gate container is not that uid.
    """
    mount = os.environ.get(PGDATA_ENV)
    if not mount:
        return False
    row = {"label": label[:200], "at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
    try:
        stat = os.statvfs(mount)
        row.update(used_bytes=(stat.f_blocks - stat.f_bfree) * stat.f_frsize,
                   capacity_bytes=stat.f_blocks * stat.f_frsize)
    except OSError as error:
        row["error"] = str(error)[:200]
    side = pgdata_journal(path)
    encoded = (json.dumps(row, separators=(",", ":")) + "\n").encode()
    # One O_APPEND write per reading, so the watcher and a check boundary
    # never interleave inside a line. A full side journal stops taking
    # readings and the report says so; it never drops the earlier ones.
    # A reading that cannot be WRITTEN costs that reading and is said on
    # stderr; it never fails the sample it rides beside.
    try:
        if (side.stat().st_size if side.exists() else 0) + len(encoded) > PGDATA_BYTES:
            return True
        descriptor = os.open(side, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
        try:
            os.write(descriptor, encoded)
        finally:
            os.close(descriptor)
    except OSError as error:
        print("gate-runtime-evidence: data volume reading not recorded: " + str(error)[:200], file=sys.stderr)
    return True


def watch_log(path):
    return path.with_suffix(path.suffix + ".watch")


def last_reading_failed(path):
    """The OS's words when the newest reading could not be taken, else None."""
    try:
        with pgdata_journal(path).open("rb") as stream:
            stream.seek(max(0, stream.seek(0, os.SEEK_END) - RAW_BYTES))
            row = json.loads(stream.read().splitlines()[-1])
        return row.get("error") if isinstance(row, dict) else None
    except (OSError, ValueError, IndexError):
        return None


def watch(path, interval, ticks):
    """Read the data volume between check boundaries until the parent is gone.

    The test check is ONE boundary pair thirty minutes apart, and scratch
    databases are dropped again, so the boundaries alone can miss the peak.
    Ends by itself: on its tick bound, or within a second of its parent's
    exit (the gate image has no `kill`, and nothing should need one).

    IT IS HEARD (review bc011e92, F5). run.sh starts it in the background
    with its output inherited, and it moves its own stdout and stderr onto
    the side file the report reads back, so the receipt tells a watcher
    that never started from one that started and died, in the watcher's own
    words. Three lines at most: the start, the FIRST failed reading, and a
    death. A collector that predates this action says so on the pod log,
    once, because nothing redirected it.
    """
    side = os.open(watch_log(path), os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.dup2(side, 1)
    os.dup2(side, 2)
    os.close(side)
    mount = os.environ.get(PGDATA_ENV)
    if not mount:
        print("gate-runtime-evidence: watcher not started: " + PGDATA_ENV + " is unset", flush=True)
        return
    print("gate-runtime-evidence: watcher started: pid %d, every %gs, %s"
          % (os.getpid(), interval, mount[:200]), flush=True)
    parent = os.getppid()
    done, said_failure = 0, False
    while ticks is None or done < ticks:
        note_pgdata(path, "watch")
        failure = None if said_failure else last_reading_failed(path)
        if failure:
            said_failure = True
            print("gate-runtime-evidence: watcher reading %d failed (said once; every failed "
                  "reading is counted in pgdata.errors): %s" % (done + 1, str(failure)[:200]), flush=True)
        done += 1
        waited = 0.0
        while waited < interval:
            time.sleep(min(1.0, interval - waited))
            waited += 1.0
            if os.getppid() != parent:
                return
        if os.getppid() != parent:
            return


def pgdata_report(path):
    """The data volume's peak over every reading; absent is never a zero."""
    side = pgdata_journal(path)
    try:
        with side.open("rb") as stream:
            raw = stream.read(PGDATA_BYTES + 1)
    except OSError:
        return {"state": "unavailable", "watcher": watcher_report(path),
                "reason": "no reading of the gate Postgres data volume: " + PGDATA_ENV
                          + " was unset for every sample (a runner manifest older than the volume)"}
    valid, errors, first_error = [], 0, None
    for line in raw[:PGDATA_BYTES].splitlines():
        try:
            row = json.loads(line)
        except (ValueError, RecursionError):
            row = None
        used, capacity = (row.get("used_bytes"), row.get("capacity_bytes")) if isinstance(row, dict) else (None, None)
        if (type(used) is int and type(capacity) is int and 0 <= used and 0 < capacity
                and text(row.get("label"), 200) and text(row.get("at"), 100)):
            valid.append((row["label"], row["at"], used, capacity))
            continue
        errors += 1
        if first_error is None:
            # A reading that could not be TAKEN carries the OS's own words.
            said = row.get("error") if isinstance(row, dict) else None
            first_error = said[:200] if text(said) else "malformed reading"
    watcher = watcher_report(path)
    if not valid:
        return {"state": "unavailable", "errors": errors, "watcher": watcher,
                "reason": "the gate Postgres data volume could not be read: "
                          + (first_error or "no valid reading")}
    peak = max(valid, key=lambda row: row[2])
    watched = sum(1 for row in valid if row[0] == "watch")
    capacity = valid[-1][3]
    stride = max(1, -(-len(valid) // PGDATA_SERIES))
    output = {"state": "measured" if watched else "partial",
              "method": "statvfs of the data volume's mount (tmpfs pages in use) at every check "
                        "boundary and from the runner's watcher between them",
              "mount": os.environ.get(PGDATA_ENV), "capacity_bytes": capacity,
              "peak_bytes": peak[2], "peak_label": peak[0], "peak_at": peak[1],
              "last_bytes": valid[-1][2], "readings": len(valid), "watch_readings": watched,
              "errors": errors, "full": peak[2] >= PGDATA_FULL * peak[3],
              "watcher": watcher,
              "series": [[row[1], row[2]] for row in valid[::stride]][:PGDATA_SERIES]}
    if not watched:
        # WHY it is partial, in the watcher's own words when it had any.
        output["note"] = ("check boundaries only: the peak is a lower bound, the test check is "
                          "one boundary pair. " + (
                              "The watcher started and recorded no reading; it said: "
                              + " | ".join(watcher["said"]) if watcher["state"] == "started"
                              else "No watcher started: " + watcher["reason"]))
    if len(raw) + 256 > PGDATA_BYTES:
        output.update(state="partial", truncated="side journal byte bound reached; later readings were not taken")
    return output


def watcher_report(path):
    """Whether run.sh's watcher started, and what it said; bounded."""
    try:
        with watch_log(path).open("rb") as stream:
            raw = stream.read(WATCH_BYTES + 1)
    except OSError:
        return {"state": "not started",
                "reason": "the watcher left no file (" + watch_log(path).name + "): run.sh did not "
                          "start it, or its collector predates the watch action (a runner script "
                          "older than the volume; the pod log then carries its refusal)"}
    said = [line[:300] for line in raw[:WATCH_BYTES].decode("utf-8", errors="replace").splitlines() if line.strip()]
    started = any("watcher started:" in line for line in said)
    output = {"state": "started" if started else "not started", "said": said[:8]}
    if not started:
        output["reason"] = "the watcher's file holds no start line"
    if len(raw) > WATCH_BYTES or len(said) > 8:
        output["truncated"] = True
    return output


def integer(reading_):
    raw = reading_.get("raw", "").strip() if reading_.get("state") == "measured" else ""
    return int(raw) if raw.isdigit() else None


def oom_kills(row):
    source = row["files"]["memory.events"]
    if source.get("state") != "measured":
        return None
    for line in source["raw"].splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0] == "oom_kill" and parts[1].isdigit():
            return int(parts[1])
    return None


def memory_report(rows):
    """The gate container's memory over EVERY sample, before any is dropped.

    Its own cgroup only: the postgres sidecar's, where the data volume's
    pages are charged, is not visible from this container - the data volume
    reading above is that number.
    """
    peaks = [value for value in (integer(row["files"]["memory.peak"]) for row in rows) if value is not None]
    if not peaks:
        return {"state": "unavailable", "reason": "no sample read the cgroup's memory.peak"}
    output = {"state": "measured", "peak_bytes": max(peaks), "limit_bytes": None, "oom_kill": None,
              "method": "cgroup v2 memory.peak (file cache included) and the memory.events "
                        "oom_kill delta of the gate container, first to last sample; evidence "
                        "only, it changes no verdict"}
    limit = rows[-1]["resources"]["GATE_MEMORY_LIMIT"]
    if limit.get("state") == "configured":
        output["limit_bytes"] = limit["value"]
    first, last = oom_kills(rows[0]), oom_kills(rows[-1])
    if (len(rows) > 1 and first is not None and last is not None and last >= first
            and known_lifetime(rows[0]) and rows[0]["lifetime"] == rows[-1]["lifetime"]):
        output["oom_kill"] = last - first
    return output


def report(path):
    rows, errors, intervals, omitted_rows, invalid_rows = [], [], [], 0, 0
    previous = None
    try:
        with path.open("rb") as stream:
            raw = stream.read(JOURNAL_BYTES + 1)
        if len(raw) > JOURNAL_BYTES:
            errors.append("journal exceeds declared bound")
        lines = raw[:JOURNAL_BYTES].splitlines()
        omitted_rows = max(0, len(lines) - JOURNAL_ROWS)
        for index, line in enumerate(lines[:JOURNAL_ROWS], 1):
            try:
                row = validate_sample(json.loads(line))
                if previous is not None:
                    intervals.append(interval(previous, row))
                rows.append(row)
                previous = row
            except (ValueError, RecursionError) as error:
                invalid_rows += 1
                previous = None
                if len(errors) < ERRORS:
                    errors.append("unreadable journal row " + str(index) + ": " + str(error)[:200])
        if omitted_rows:
            errors.append("journal row bound reached; original journal retained")
    except OSError as error:
        errors.append(str(error))
    if path.with_suffix(path.suffix + ".truncated").exists():
        errors.append("collection truncated at journal bound")
    if path.with_suffix(path.suffix + ".failed").exists():
        errors.append("one or more collection calls failed")
    kept = rows[:RECEIPT_SAMPLES // 2] + rows[-RECEIPT_SAMPLES // 2:] if len(rows) > RECEIPT_SAMPLES else rows
    output = {"version": 1, "state": "partial" if errors else "measured",
              "bounds": {"raw_file_bytes": RAW_BYTES, "journal_bytes": JOURNAL_BYTES,
                         "receipt_bytes": RECEIPT_BYTES, "receipt_samples": RECEIPT_SAMPLES,
                         "journal_rows": JOURNAL_ROWS, "diagnostic_errors": ERRORS},
              "raw_record": str(path), "collection_errors": errors[:ERRORS], "sample_count": len(rows),
              "invalid_rows": invalid_rows,
              "omitted_rows": omitted_rows,
              "samples": kept, "intervals": intervals,
              # Summaries over EVERY row and reading, computed before the
              # reductions below, which keep neither.
              "pgdata": pgdata_report(path), "memory": memory_report(rows),
              "browser": {"state": "unavailable", "reason": "no observed browser runtime record"},
              "browser_policy": {"state": "unavailable", "reason": "no resolved browser configuration"}}
    # The runner copies raw bytes independently of verdict before cleanup;
    # receipt reductions name their omissions rather than claiming completeness.
    if len(kept) != len(rows):
        output.update(state="partial", omitted_samples=len(rows) - len(kept))
    while len(json.dumps(output).encode()) > RECEIPT_BYTES - 2 * RAW_BYTES and output["samples"]:
        output["samples"].pop(-2 if len(output["samples"]) > 1 else -1)
        output.update(state="partial", omitted_samples=len(rows) - len(output["samples"]))
    interval_count = len(intervals)
    while len(json.dumps(output).encode()) > RECEIPT_BYTES - 2 * RAW_BYTES and output["intervals"]:
        output["intervals"].pop()
        output.update(state="partial", omitted_intervals=interval_count - len(output["intervals"]))
    if any(any(v.get("state") != "measured" for v in row.get("files", {}).values())
           or any(value is None for value in row["identity"].values())
           or not known_lifetime(row) for row in rows):
        output["state"] = "partial"
    if not rows:
        output["state"] = "unavailable"
    try:
        with path.with_suffix(path.suffix + ".browser").open("rb") as stream:
            raw = stream.read(RAW_BYTES + 1)
        if len(raw) > RAW_BYTES:
            raise ValueError("browser observation exceeds declared byte bound")
        output["browser"] = validate_browser(json.loads(raw))
    except OSError as error:
        output["browser"]["reason"] = str(error)[:RAW_BYTES]
    except (ValueError, RecursionError) as error:
        output["browser"] = {"state": "invalid", "reason": str(error)[:RAW_BYTES]}
    try:
        with path.with_suffix(path.suffix + ".browser-config").open("rb") as stream:
            raw = stream.read(RAW_BYTES + 1)
        if len(raw) > RAW_BYTES:
            raise ValueError("resolved browser configuration exceeds declared byte bound")
        policy = json.loads(raw)
        if (not isinstance(policy, dict) or set(policy) != {"workers", "retries"}
                or type(policy.get("workers")) is not int or policy["workers"] < 1
                or type(policy.get("retries")) is not int or policy["retries"] < 0):
            raise ValueError("malformed resolved browser configuration")
        output["browser_policy"] = {"state": "configured", "workers": policy["workers"],
                                    "retries": policy["retries"], "method": "loaded Playwright configuration"}
    except OSError:
        output["browser_policy"]["reason"] = "resolved browser configuration unavailable"
    except (ValueError, RecursionError):
        output["browser_policy"]["reason"] = (
            "resolved browser configuration exceeds declared byte bound" if len(raw) > RAW_BYTES
            else "malformed resolved browser configuration")
    return output


def browser(path):
    try:
        with path.open("rb") as stream:
            raw = stream.read(JOURNAL_BYTES + 1)
        observations = re.findall(r"Running ([0-9]+) tests? using ([0-9]+) workers?", raw[:JOURNAL_BYTES].decode(errors="replace"))
        if len(observations) != 1 or len(raw) > JOURNAL_BYTES:
            return {"state": "unavailable", "reason": "missing, ambiguous or truncated browser runtime output"}
        tests, workers = map(int, observations[0])
        if workers < 1:
            return {"state": "invalid", "reason": "nonpositive worker observation"}
        return {"state": "measured", "tests": tests, "workers": workers,
                "method": "Playwright list reporter runtime announcement", "source": str(path)}
    except OSError as error:
        return {"state": "unavailable", "reason": str(error)}


def retain(path, receipt):
    """Copy original bounded bytes to runner stdout before its workspace dies."""
    diagnostics, raw = [], None
    try:
        with path.open("rb") as stream:
            raw = stream.read(JOURNAL_BYTES + 1)
        if len(raw) > JOURNAL_BYTES:
            diagnostics.append("journal exceeds declared bound; retained prefix only")
            raw = raw[:JOURNAL_BYTES]
    except OSError as error:
        diagnostics.append("journal unavailable: " + str(error)[:200])
    for suffix, reason in ((".truncated", "collection truncated at journal bound"),
                           (".failed", "one or more collection calls failed")):
        if path.with_suffix(path.suffix + suffix).exists():
            diagnostics.append(reason)
    identity = {name: (os.environ[name][:RAW_BYTES] or None) if name in os.environ else None for name in IDENTITY}
    if any(len(os.environ.get(name, "")) > RAW_BYTES for name in IDENTITY):
        diagnostics.append("identity exceeds declared field bound; retained prefix only")
    record = {"version": 1, "state": "unavailable" if raw is None else
              "partial" if diagnostics else "retained", "method": "runner stdout base64 frames",
              "source": str(path)[:RAW_BYTES], "identity": identity,
              "bytes": len(raw) if raw is not None else 0, "bound_bytes": JOURNAL_BYTES,
              "sha256": hashlib.sha256(raw).hexdigest() if raw is not None else None,
              "diagnostics": diagnostics,
              "lifetime": "runner stdout; availability follows pod log retention, not permanent archival"}
    # Prepare the binding without claiming bytes that stdout has not accepted.
    # A full/broken output sink must never leave a retained stamp behind.
    body = None
    try:
        with receipt.open("rb") as stream:
            receipt_raw = stream.read(RECEIPT_FILE_BYTES + 1)
        if len(receipt_raw) > RECEIPT_FILE_BYTES:
            raise ValueError("receipt exceeds declared retention reader byte bound")
        candidate = json.loads(receipt_raw)
        if not isinstance(candidate, dict) or not isinstance(candidate.get("runtime_evidence"), dict):
            raise ValueError("receipt has no runtime evidence object")
        candidate["runtime_evidence"]["raw_retention"] = record
        if len(json.dumps(candidate["runtime_evidence"]).encode()) > RECEIPT_BYTES:
            raise ValueError("retention binding exceeds receipt evidence bound")
        body = candidate
        binding = {"state": "prepared", "method": "receipt written only after stdout end frame is flushed"}
    except (OSError, ValueError, RecursionError) as error:
        binding = {"state": "unavailable", "reason": str(error)[:200]}
    output_error = None
    try:
        if raw is not None:
            for index, offset in enumerate(range(0, len(raw), 3072)):
                print("gate-runtime-raw: " + json.dumps({"kind": "chunk", "index": index,
                      "data": base64.b64encode(raw[offset:offset + 3072]).decode("ascii")}, separators=(",", ":")))
        print("gate-runtime-raw: " + json.dumps(dict(record, kind="end", receipt_binding=binding), separators=(",", ":")), flush=True)
    except OSError as error:
        output_error = error
        record = dict(record, state="unavailable", diagnostics=diagnostics +
                      ["stdout retention failed: " + str(error)[:200]])
    if body is not None:
        body["runtime_evidence"]["raw_retention"] = record
        temporary = receipt.with_suffix(receipt.suffix + ".retention-tmp")
        temporary.write_text(json.dumps(body, separators=(",", ":")))
        temporary.replace(receipt)
    if output_error is not None:
        raise SystemExit("raw journal was not retained: stdout output failed")


if __name__ == "__main__":
    action, path = sys.argv[1], Path(sys.argv[2])
    if action == "sample":
        append(path, sample(sys.argv[3]))
        note_pgdata(path, sys.argv[3])
    elif action == "watch":
        watch(path, float(sys.argv[3]) if len(sys.argv) > 3 else 15.0,
              int(sys.argv[4]) if len(sys.argv) > 4 else None)
    elif action == "report":
        print(json.dumps(report(path), separators=(",", ":")))
    elif action == "browser":
        print(json.dumps(browser(path), separators=(",", ":")))
    elif action == "retain":
        retain(path, Path(sys.argv[3]))
    elif action == "merge":
        receipt = Path(sys.argv[3])
        body = json.loads(receipt.read_text())
        if not isinstance(body, dict):
            raise SystemExit("receipt is not an object; original preserved")
        # EVIDENCE BESIDE THE VERDICT, NEVER INSTEAD OF IT (review bc011e92,
        # F1). gate.sh wrote the verdict from the checks; this adds what the
        # run's own resources did (runtime_evidence.pgdata.full,
        # runtime_evidence.memory.oom_kill) and touches nothing else. A full
        # volume or a kill beside a red is CO-OCCURRENCE: this collector does
        # not know which check failed or why, a car whose own tests fill the
        # database or eat the gate's memory earned its red, and `refused` is
        # relaunched without bound and strikes nobody. Pinned by
        # gate_pgdata_evidence.rs.
        body["runtime_evidence"] = report(path)
        temporary = receipt.with_suffix(receipt.suffix + ".runtime-tmp")
        temporary.write_text(json.dumps(body, separators=(",", ":")))
        temporary.replace(receipt)
    else:
        raise SystemExit("unknown runtime evidence action")
