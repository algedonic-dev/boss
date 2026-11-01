#!/usr/bin/env python3
"""Value-free evidence for the reclaimer's own Git children, not exec authority."""
import json
import fcntl
import os
from pathlib import Path
import re
import stat
import sys
import time
import uuid

MAX_RECORDS = 256
MAX_ROW_BYTES = 4096
OPERATIONS = frozenset(("symbolic-ref", "rev-parse", "merge-base", "rev-list", "merge", "log", "status", "diff", "cherry", "for-each-ref", "worktree", "checkout", "update-ref"))
ACCOUNTING_KEYS = frozenset(("current", "peak", "max", "anon", "file", "kernel", "shmem"))
DEMAND_ACCOUNTING_KEYS = ACCOUNTING_KEYS | frozenset(("slab_reclaimable", "slab_unreclaimable"))


def diagnostic(reason):
    # The wrapper's separate descriptor survives original Git stderr redirection.
    # Only these fixed messages cross it; it never carries arguments or errors.
    message = ("reclaim-git-observer: unknown " + reason + "\n").encode()
    try:
        info = os.fstat(3)
        if stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1 and not info.st_mode & 0o077:
            # One fixed marker, even if thousands of attempts cannot be recorded.
            os.ftruncate(3, 0)
            os.lseek(3, 0, os.SEEK_SET)
            os.write(3, b"unknown\n")
            os.fsync(3)
        else:
            os.write(3, message)
    except OSError:
        sys.stderr.write(message.decode())


def positive_int(value):
    return type(value) is int and value > 0


def valid_accounting(value, schema=1):
    keys = DEMAND_ACCOUNTING_KEYS if schema == 2 else ACCOUNTING_KEYS
    return isinstance(value, dict) and set(value) == keys and all(
        isinstance(item, str) and (item.isdigit() or item == "unknown" or (key == "max" and item == "max"))
        for key, item in value.items())


def valid_begin(row):
    return isinstance(row, dict) and set(row) == {
        "schema", "phase", "operation", "pid", "start_ticks", "boot_id", "cgroup_ns", "at_ns", "accounting_scope", "accounting"
    } and type(row["schema"]) is int and row["schema"] in (1, 2) and all(positive_int(row[k]) for k in ("pid", "start_ticks", "cgroup_ns", "at_ns")) and (
        isinstance(row["phase"], str) and re.fullmatch(r"[a-z_]{1,64}", row["phase"]) is not None
        and isinstance(row["operation"], str) and row["operation"] in OPERATIONS and row["phase"] != "unknown"
        and isinstance(row["boot_id"], str) and re.fullmatch(r"[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}", row["boot_id"]) is not None
        and row["accounting_scope"] == "observer own cgroup" and valid_accounting(row["accounting"], row["schema"]))


def valid_child_usage(value):
    return isinstance(value, dict) and set(value) == {"scope", "max_rss_kib", "user_cpu_us", "system_cpu_us"} and (
        value["scope"] == "wait4 native child" and all(
            type(value[key]) is int and value[key] >= 0
            for key in ("max_rss_kib", "user_cpu_us", "system_cpu_us")))


def valid_end(row, begin):
    keys = {"schema", "pid", "exit_code", "observed", "at_ns", "accounting"}
    if begin.get("schema") == 2:
        keys = keys | {"child_usage"}
    return isinstance(row, dict) and set(row) == keys and (
        type(row["schema"]) is int and row["schema"] in (1, 2) and row["schema"] == begin["schema"]
        and positive_int(row["pid"]) and row["pid"] == begin["pid"]
        and type(row["exit_code"]) is int and 0 <= row["exit_code"] <= 255
        and row["observed"] is True and positive_int(row["at_ns"]) and valid_accounting(row["accounting"], row["schema"])
        and (row["schema"] == 1 or valid_child_usage(row["child_usage"])))


def root_path():
    return Path(os.environ.get("BOSS_RECLAIM_GIT_DIR", os.environ.get("WORK_MOUNT", "/work") + "/telemetry/dev-reclaim-git"))


def valid_retention(row):
    return isinstance(row, dict) and set(row) == {"dropped_complete", "unobserved"} and all(
        type(row[k]) is int and row[k] >= 0 for k in row)


def valid_retirement(row):
    return (isinstance(row, dict) and set(row) == {"entry", "begin", "end", "dropped_complete_before"}
            and isinstance(row["entry"], str) and re.fullmatch(r"[0-9a-f]{32}", row["entry"]) is not None
            and type(row["dropped_complete_before"]) is int and row["dropped_complete_before"] >= 0
            and valid_begin(row["begin"]) and valid_end(row["end"], row["begin"]))


def checked_directory(path, create=False):
    path = Path(path)
    if not path.is_absolute():
        path = Path.cwd() / path
    if ".." in path.parts:
        raise ValueError("storage path is not normalized")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    directory = os.open(path.anchor, flags)
    try:
        # Walk using each opened parent, not a full path checked earlier.
        # The original mkdir(parents=True) followed a linked ancestor and
        # changed its target before refusing it (urgent896, source4f8).
        for component in path.parts[1:]:
            try:
                child = os.open(component, flags, dir_fd=directory)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(component, mode=0o700, dir_fd=directory)
                except FileExistsError:
                    pass  # The following open still refuses a link or file.
                child = os.open(component, flags, dir_fd=directory)
            previous = directory
            directory = child
            os.close(previous)
        info = os.fstat(directory)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
            raise ValueError("storage owner or writable mode is unsafe")
        accepted = directory
        directory = None
        return accepted
    finally:
        if directory is not None:
            os.close(directory)


def start_ticks(pid):
    try:
        # The comm field may contain spaces and parentheses; it is not recorded.
        tail = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(tail[19])
    except (OSError, ValueError, IndexError):
        return None


def live_incarnation(row):
    try:
        tail = Path(f"/proc/{row['pid']}/stat").read_text().rsplit(")", 1)[1].split()
        return (tail[0] not in ("Z", "X", "x") and int(tail[19]) == row["start_ticks"]
                and row["boot_id"] == boot_id()
                and row["cgroup_ns"] == os.stat(f"/proc/{row['pid']}/ns/cgroup").st_ino)
    except (OSError, ValueError, IndexError):
        return False


def boot_id():
    value = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    if not re.fullmatch(r"[0-9a-f-]{36}", value):
        raise ValueError("boot identity unavailable")
    return value


def accounting(schema=1):
    result = {}
    for key in ("current", "peak", "max"):
        try:
            value = Path("/sys/fs/cgroup/memory." + key).read_text().strip()
            result[key] = value if value.isdigit() or value == "max" else "unknown"
        except OSError:
            result[key] = "unknown"
    try:
        rows = Path("/sys/fs/cgroup/memory.stat").read_text().splitlines()
    except OSError:
        rows = []
    keys = DEMAND_ACCOUNTING_KEYS if schema == 2 else ACCOUNTING_KEYS
    for key in sorted(keys - {"current", "peak", "max"}):
        matches = [r.split() for r in rows if r.split() and r.split()[0] == key]
        result[key] = matches[0][1] if len(matches) == 1 and len(matches[0]) == 2 and matches[0][1].isdigit() else "unknown"
    return result


def write_record(path, name, row):
    data = (json.dumps(row, separators=(",", ":"), sort_keys=True) + "\n").encode()
    if len(data) > MAX_ROW_BYTES:
        raise ValueError("record bound exceeded")
    directory = checked_directory(path)
    try:
        fd = os.open(name + ".writing", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.rename(name + ".writing", name, src_dir_fd=directory, dst_dir_fd=directory)
            os.fsync(directory)
        finally:
            try:
                os.unlink(name + ".writing", dir_fd=directory)
            except FileNotFoundError:
                pass
    finally:
        os.close(directory)


def read_record(path, name):
    directory = os.dup(path) if type(path) is int else checked_directory(path)
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
                raise ValueError("unsafe record")
            data = stream.read(MAX_ROW_BYTES + 1)
            if len(data) > MAX_ROW_BYTES:
                raise ValueError("record bound exceeded")
            return json.loads(data)
    finally:
        os.close(directory)


def records():
    root = root_path()
    fd = checked_directory(root)
    try:
        lock = os.open(".lock", os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
        try:
            info = os.fstat(lock)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
                raise ValueError("unsafe ledger lock")
        finally:
            os.close(lock)
    finally:
        os.close(fd)
    entries = []
    for item in root.iterdir():
        if item.name in (".lock", "retention.json", "retirement.json"):
            continue
        if not re.fullmatch(r"[0-9a-f]{32}", item.name):
            raise ValueError("unexpected ledger entry")
        fd = checked_directory(item)
        os.close(fd)
        entries.append(item)
    if len(entries) > MAX_RECORDS:
        raise ValueError("ledger bound exceeded")
    return entries


def reserve():
    root = root_path()
    directory = checked_directory(root, create=True)
    lock = os.open(".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=directory)
    try:
        info = os.fstat(lock)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
            raise ValueError("unsafe ledger lock")
        fcntl.flock(lock, fcntl.LOCK_EX)
        if (root / "retirement.json").exists():
            raise ValueError("unfinished retirement held for diagnosis")
        entries = records()
        try:
            retention = read_record(root, "retention.json")
        except FileNotFoundError:
            retention = {"dropped_complete": 0, "unobserved": 0}
        if not valid_retention(retention):
            raise ValueError("malformed retention counters")
        if len(entries) >= MAX_RECORDS:
            completed = []
            for entry in entries:
                selected = checked_directory(entry)
                try:
                    begin = read_record(selected, "begin.json")
                    end = read_record(selected, "end.json")
                    if valid_begin(begin) and valid_end(end, begin):
                        if sorted(os.listdir(selected)) == ["begin.json", "end.json"]:
                            identity = os.fstat(selected)
                            completed.append((begin.get("at_ns", 0), entry.name, entry,
                                              identity.st_dev, identity.st_ino, begin, end))
                except FileNotFoundError:
                    pass  # Interrupted or live evidence is never retired for space.
                finally:
                    os.close(selected)
            if not completed:
                retention["unobserved"] += 1
                write_record(root, "retention.json", retention)
                raise ValueError("bound held by unfinished evidence")
            _, _, oldest, device, inode, begin, end = min(completed)
            # Refuse extra entries; retire only this observer's complete record.
            # Keep the original before removing any byte. An interrupted counter
            # commit remains visibly UNKNOWN and its original is still archived.
            retired = os.open(oldest.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            try:
                info = os.fstat(retired)
                if info.st_uid != os.getuid() or info.st_mode & 0o022:
                    raise ValueError("retirement owner or writable mode is unsafe")
                if (info.st_dev, info.st_ino) != (device, inode):
                    raise ValueError("retirement directory changed after selection")
                if read_record(retired, "begin.json") != begin or read_record(retired, "end.json") != end:
                    raise ValueError("retirement records changed after selection")
                write_record(root, "retirement.json", {"entry": oldest.name,
                    "begin": begin, "end": end,
                    "dropped_complete_before": retention["dropped_complete"]})
                # The archive and removal use the selected incarnation. A second
                # same-UID private directory is no more ours than a linked one.
                current = os.stat(oldest.name, dir_fd=directory, follow_symlinks=False)
                if (current.st_dev, current.st_ino) != (device, inode):
                    raise ValueError("retirement directory changed after archive")
                if read_record(retired, "begin.json") != begin or read_record(retired, "end.json") != end:
                    raise ValueError("retirement records changed after archive")
                os.unlink("end.json", dir_fd=retired)
                os.unlink("begin.json", dir_fd=retired)
                os.fsync(retired)
                current = os.stat(oldest.name, dir_fd=directory, follow_symlinks=False)
                if (current.st_dev, current.st_ino) != (device, inode):
                    raise ValueError("retirement directory changed before removal")
                os.rmdir(oldest.name, dir_fd=directory)
            finally:
                os.close(retired)
            retention["dropped_complete"] += 1
            write_record(root, "retention.json", retention)
            os.unlink("retirement.json", dir_fd=directory)
            os.fsync(directory)
        entry = root / uuid.uuid4().hex
        os.mkdir(entry.name, mode=0o700, dir_fd=directory)
        return entry
    finally:
        os.close(lock)
        os.close(directory)


def operation(arguments):
    index = 0
    while index < len(arguments):
        token = arguments[index]
        if token in ("-C", "-c", "--git-dir", "--work-tree"):
            index += 2
        elif token.startswith("-"):
            index += 1
        else:
            return token if token in OPERATIONS else "unknown"
    return "unknown"


def state():
    try:
        entries = records()
        unknown = 0
        active = 0
        for entry in entries:
            begin = read_record(entry, "begin.json")
            if not valid_begin(begin):
                unknown += 1
                continue
            try:
                end = read_record(entry, "end.json")
                if not valid_end(end, begin):
                    unknown += 1
            except FileNotFoundError:
                if live_incarnation(begin):
                    active += 1
                else:
                    unknown += 1
        try:
            retention = read_record(root_path(), "retention.json")
        except FileNotFoundError:
            retention = {"dropped_complete": 0, "unobserved": 0}
        if not valid_retention(retention):
            raise ValueError("malformed retention counters")
        unknown += retention["unobserved"]
        try:
            pending = read_record(root_path(), "retirement.json")
            if not valid_retirement(pending):
                raise ValueError("malformed retirement")
            unknown += 1
        except FileNotFoundError:
            pass
        return {"state": "unknown" if unknown else "observed", "scope": "validated_retained_records", "records": len(entries), "active": active, "unknown": unknown,
                "dropped_complete": retention["dropped_complete"], "record_limit": MAX_RECORDS}
    except (OSError, ValueError, json.JSONDecodeError):
        return {"state": "unknown", "reason": "ledger unavailable or malformed"}


def snapshot(limit):
    """Only validated technical fields; never copy arbitrary stored JSON."""
    result = state()
    try:
        phases = []
        current_boot = boot_id()
        current_namespace = os.stat("/proc/self/ns/cgroup").st_ino
        for entry in records():
            begin = read_record(entry, "begin.json")
            if not valid_begin(begin):
                continue
            terminal = None
            try:
                terminal = read_record(entry, "end.json")
                status = "completed" if valid_end(terminal, begin) else "unknown_terminal"
            except FileNotFoundError:
                status = "active" if live_incarnation(begin) else "interrupted_or_replaced"
            binding = ("current_observer_namespace" if begin["boot_id"] == current_boot
                       and begin["cgroup_ns"] == current_namespace else "historical_other_namespace")
            row = {**begin, "status": status, "binding": binding}
            if terminal is not None and valid_end(terminal, begin):
                row["terminal"] = terminal
            phases.append(row)
        # Interrupted/live evidence stays ahead of old completed observations.
        phases.sort(key=lambda row: (row["status"] == "completed", -row["at_ns"]))
        result.update({"schema": 1, "observer_boot_id": current_boot,
                       "observer_cgroup_ns": current_namespace,
                       "omitted": max(0, len(phases) - limit), "phases": phases[:limit]})
        try:
            pending = read_record(root_path(), "retirement.json")
            if not valid_retirement(pending):
                raise ValueError("malformed retirement")
            result["unfinished_retirement"] = pending
        except FileNotFoundError:
            pass
    except (OSError, ValueError, json.JSONDecodeError):
        result = {"state": "unknown", "reason": "ledger unavailable or malformed", "phases": []}
    return result


def run(phase, arguments):
    # Missing telemetry cannot alter a pre-existing Git/cleanup decision. It is
    # a separately visible UNKNOWN, never an observer PASS. No argument is logged.
    entry = None
    try:
        entry = reserve()
    except (OSError, ValueError):
        diagnostic("storage or retention bound")
    pid = os.fork()
    if pid == 0:
        try:
            if entry is not None:
                ticks = start_ticks(os.getpid())
                if ticks is None:
                    raise ValueError("child incarnation unavailable")
                write_record(entry, "begin.json", {
                    "schema": 2, "phase": phase if re.fullmatch(r"[a-z_]{1,64}", phase) else "unknown",
                    "operation": operation(arguments), "pid": os.getpid(), "start_ticks": ticks,
                    "boot_id": boot_id(), "cgroup_ns": os.stat("/proc/self/ns/cgroup").st_ino,
                    "at_ns": time.time_ns(), "accounting_scope": "observer own cgroup", "accounting": accounting(2),
                })
        except (OSError, ValueError):
            diagnostic("pre-exec evidence")
        try:
            for descriptor in (3, 9):
                try:
                    os.close(descriptor)
                except OSError:
                    pass
            os.execvp("git", ["git", *arguments])
        except OSError:
            os._exit(127)
    # Linux wait4 retains this child's high-water after it exits. The shared
    # cgroup boundaries cannot attribute file/slab or short-lived heap demand
    # to a Git child (urgent896); record the two facts separately, never a limit.
    waited_pid, status, usage = os.wait4(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    code = code if code >= 0 else 128 - code
    try:
        if entry is not None:
            begin = read_record(entry, "begin.json")
            write_record(entry, "end.json", {"schema": 2, "pid": waited_pid, "exit_code": code,
                "observed": begin.get("pid") == waited_pid, "at_ns": time.time_ns(), "accounting": accounting(2),
                "child_usage": {"scope": "wait4 native child", "max_rss_kib": usage.ru_maxrss,
                    "user_cpu_us": round(usage.ru_utime * 1_000_000), "system_cpu_us": round(usage.ru_stime * 1_000_000)}})
    except (OSError, ValueError, json.JSONDecodeError):
        diagnostic("terminal evidence")
    return code


if __name__ == "__main__":
    if sys.argv[1:] == ["--state"]:
        print(json.dumps(state(), separators=(",", ":")))
    elif sys.argv[1:] == ["--snapshot"]:
        print(json.dumps(snapshot(32), separators=(",", ":")))
    elif sys.argv[1:] == ["--archive"]:
        print(json.dumps(snapshot(MAX_RECORDS), separators=(",", ":")))
    elif len(sys.argv) >= 4 and sys.argv[1] == "--run" and sys.argv[3] == "--":
        sys.exit(run(sys.argv[2], sys.argv[4:]))
    else:
        print("reclaim-git-observer: unsupported operation", file=sys.stderr)
        sys.exit(64)
