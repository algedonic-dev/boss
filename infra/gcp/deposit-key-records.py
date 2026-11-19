#!/usr/bin/env python3
"""Project public deposit keys from immutable packets and their corrections.

The optional cache is a rebuildable projection, bound to the read source.
Only complete, validated reads can replace it; it never authorises a key.
"""
import datetime
import json
import os
import pathlib
import re
import stat
import subprocess
import sys
import tempfile


def corrected(job, step, field):
    original = step.get("metadata", {}).get(field)
    if not isinstance(original, str):
        return original
    entries = job.get("metadata", {}).get("corrections", [])
    withdrawn = {e["withdraws"] for e in entries if "withdraws" in e}
    replacements = {}
    for index, entry in enumerate(entries):
        if (index not in withdrawn and "withdraws" not in entry
                and entry.get("step") == step.get("id")
                and entry.get("field") == field):
            reads, value = entry.get("reads"), entry.get("should_read")
            if not isinstance(reads, str) or not reads or not isinstance(value, str) or reads not in original:
                raise ValueError(f"packet {job['id']} has an unreadable {field} correction")
            # Corrections quote the stored original, not a previous correction.
            replacements[reads] = value
    spans = []
    for reads, value in replacements.items():
        start = 0
        while (at := original.find(reads, start)) >= 0:
            end = at + len(reads)
            if any(at < hi and end > lo for lo, hi, _ in spans):
                raise ValueError(f"packet {job['id']} has overlapping {field} corrections; "
                                 f"withdraw the conflicting entries with boss correct {job['id']} "
                                 f"--step {step['spec_slug']} --withdraws <index> --why <reason> "
                                 "before correcting the whole field")
            spans.append((at, end, value))
            start = end
    for start, end, value in sorted(spans, reverse=True):
        original = original[:start] + value + original[end:]
    return original


def compact(job):
    return {**{k: job.get(k) for k in ("id", "kind", "subject", "status")},
            "metadata": {"corrections": job.get("metadata", {}).get("corrections", [])},
            "steps": [{**{k: s.get(k) for k in ("id", "spec_slug", "status", "completed_at")},
                       "metadata": {k: v for k, v in s.get("metadata", {}).items()
                                    if k in ("public_key", "output")}}
                      for s in job.get("steps", [])]}


def completion_time(job):
    times = []
    for step in job.get("steps", []):
        if value := step.get("completed_at"):
            stamp = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
            if stamp.tzinfo is None:
                raise ValueError(f"packet {job['id']} records a completion without a timezone")
            times.append(stamp.timestamp())
    return max(times) if times else None


def frontier(rows):
    times = [stamp for job in rows if (stamp := completion_time(job)) is not None]
    if not times:
        raise ValueError("the record has no completion frontier; no cache can be advanced")
    # Inclusive cursor, rounded down with an overlap, from server facts.
    return datetime.datetime.fromtimestamp(int(max(times)) - 1, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load(cache, context, output):
    rows, since = [], ""
    if cache and pathlib.Path(cache).exists():
        fd = os.open(cache, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd) as source:
            mode = os.fstat(source.fileno())
            if not stat.S_ISREG(mode.st_mode) or mode.st_uid != os.geteuid() or mode.st_mode & 0o077:
                raise ValueError(f"cache {cache} is not a private regular file owned by this reader")
            try:
                saved = json.load(source)
            except ValueError as error:
                raise ValueError(f"cache {cache} is unreadable: {error}") from error
        if saved.get("schema") != 1 or saved.get("context") != context:
            raise ValueError(f"cache {cache} belongs to another source or schema; rebuild it before use")
        rows = saved["rows"]
        if not isinstance(rows, list) or len({j["id"] for j in rows}) != len(rows):
            raise ValueError(f"cache {cache} has an unreadable packet set")
        since = frontier(rows)
        if since != saved.get("frontier"):
            raise ValueError(f"cache {cache} does not conserve its completion frontier")
    pathlib.Path(output).write_text(json.dumps(rows))
    print(since)


def save(cache, context, rows):
    state = {"schema": 1, "context": context, "rows": rows, "frontier": frontier(rows)}
    parent = pathlib.Path(cache).parent
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=".deposit-keys-", dir=parent)
    try:
        with os.fdopen(fd, "w") as target:
            json.dump(state, target)
            target.flush()
            os.fsync(target.fileno())
        os.replace(name, cache)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def key_candidates(value, field, forced):
    if not isinstance(value, str):
        return []
    if field == "public_key":
        return [value] if value else []
    candidates = []
    for line in value.splitlines():
        for suffix in line.split(forced + " ")[1:]:
            fields = suffix.split()
            if len(fields) >= 2 and re.fullmatch(r"[a-z0-9@.-]+", fields[0]):
                candidates.append(forced + " " + " ".join(fields[:2]))
    return candidates


def key_identity(value, field):
    # Public comments may be corrected without changing an SSH identity.
    # The break-glass line also binds that identity to its receiver.
    return value if field == "output" else tuple(value.split()[:2])


def project(rows, pairs, bg_kind, bg_subject, forced, key_parser):
    values, support = {}, {}
    bg_found = False
    for job in rows:
        pair = (job["kind"], (job.get("subject") or {}).get("id"))
        if pair not in pairs and pair != (bg_kind, bg_subject):
            continue
        for step in job.get("steps", []):
            slug = step.get("spec_slug")
            field = "output" if pair == (bg_kind, bg_subject) else "public_key"
            if (field == "output" and slug != "run") or (field == "public_key" and slug not in ("issue", "install")):
                continue
            value = corrected(job, step, field)
            remedy = f"boss correct {job['id']} --step {slug} --field {field}"
            if field == "public_key":
                original = step.get("metadata", {}).get(field)
                retracted = value == "" and isinstance(original, str) and bool(original)
                if step.get("status") == "completed" and (not isinstance(value, str) or (not value and not retracted)):
                    raise ValueError(f"packet {job['id']} has no {slug}.public_key; {remedy}")
            candidates = key_candidates(value, field, forced)
            # Review 1249eac7, alternative 2: a correction can retract a
            # bad value, but a new key needs its own broker record. Never
            # turn a generic job-update right into credential authority.
            original = step.get("metadata", {}).get(field)
            recorded = {key_identity(key, field) for key in key_candidates(original, field, forced)}
            for candidate in candidates:
                if key_identity(candidate, field) not in recorded:
                    raise ValueError(f"packet {job['id']} corrected {slug}.{field} would introduce a public identity "
                                     "absent from its original record; withdraw the replacement with "
                                     f"boss correct {job['id']} --step {slug} --withdraws <index> --why <reason>, "
                                     "or retract the malformed value with an empty correction; "
                                     "a new key requires a broker record")
                if "\n" in candidate or "\r" in candidate:
                    raise ValueError(f"packet {job['id']} records a key with a line break; {remedy}")
                values.setdefault(candidate, (job["id"], remedy))
                support.setdefault(candidate, set()).add(job["id"])
                bg_found |= field == "output"
    if not bg_found:
        raise ValueError(f"no {bg_kind} packet records the break-glass deposit key")
    # Validate each unique key once, before argv loses its packet provenance.
    for value, (packet, remedy) in values.items():
        result = subprocess.run([key_parser, "-l", "-E", "sha256", "-f", "-"],
                                input=value + "\n", text=True, capture_output=True, timeout=5)
        if result.returncode or len(result.stdout.splitlines()) != 1:
            raise ValueError(f"packet {packet} records an unparseable public key {json.dumps(value)}; repair with {remedy}")
    # One immutable witness per distinct public value is enough. Keep
    # open and corrected packets whole, plus the completion frontier.
    # If a retained witness gains a correction, the caller reconstructs
    # history before removing it: another old packet may still vouch
    # for the same key. Daily duplicates need not live here forever.
    stamps = {job["id"]: completion_time(job) for job in rows}

    def stamp(identity):
        value = stamps[identity]
        return value if value is not None else float("-inf")

    ranks = {job["id"]: (job.get("status") in ("closed", "cancelled")
                         and not job.get("metadata", {}).get("corrections"),
                         stamp(job["id"]), job["id"])
             for job in rows}
    keep = {max(ids, key=lambda identity: ranks[identity]) for ids in support.values()}
    keep.update(job["id"] for job in rows if job.get("status") not in ("closed", "cancelled")
                or job.get("metadata", {}).get("corrections"))
    if rows:
        keep.add(max(rows, key=lambda job: (stamp(job["id"]), job["id"]))["id"])
    return sorted(values), [job for job in rows if job["id"] in keep]


def main(args):
    command, *args = args
    if command == "load":
        cache, context_file, output = args
        load(cache, pathlib.Path(context_file).read_text(), output)
    elif command == "merge":
        rows = {}
        for filename in args:
            for job in json.loads(pathlib.Path(filename).read_text()):
                rows[job["id"]] = compact(job)
        print(json.dumps(list(rows.values())))
    elif command == "rescan":
        cached = {job["id"]: job for job in json.loads(pathlib.Path(args[0]).read_text())}
        changed = False
        for filename in args[1:]:
            for job in json.loads(pathlib.Path(filename).read_text()):
                prior = cached.get(job["id"])
                if prior and prior.get("status") in ("closed", "cancelled"):
                    changed |= prior.get("metadata", {}).get("corrections", []) != job.get("metadata", {}).get("corrections", [])
        print("yes" if changed else "no")
    elif command == "project":
        source, pairs_file, bg_kind, bg_subject, forced, cache, context_file, key_parser = args
        rows = json.loads(pathlib.Path(source).read_text())
        pairs = {tuple(line.split()) for line in pathlib.Path(pairs_file).read_text().splitlines()}
        keys, retained = project(rows, pairs, bg_kind, bg_subject, forced, key_parser)
        if cache:
            save(cache, pathlib.Path(context_file).read_text(), retained)
        print("\n".join(keys))
    else:
        raise ValueError(f"unknown projection action {command}")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        print(str(error).replace("\n", " "), file=sys.stderr)
        sys.exit(2)
