#!/usr/bin/env python3
"""Weekly source-diff readings for the IT retro (8235e12a).

No store, branch lookup or train-total division. The first-parent main log
defines landings; existing native car/train records bind frozen source heads.
Binary line counts and missing evidence remain unknown. Overlapping source
diffs are per-car work, not an allocation of the assembled train's net delta.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import sys
import uuid


def git(repo, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    result = subprocess.run(["git", "--no-pager", "-C", repo, *args],
                            env=env, capture_output=True)
    if result.returncode:
        raise ValueError("Git read failed: " + result.stderr.decode(errors="replace"))
    return result.stdout


def full_sha(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value) is not None


def job_id(value):
    try:
        return isinstance(value, str) and str(uuid.UUID(value)) == value
    except ValueError:
        return False


def unwrap(value):
    return value.get("data", value) if isinstance(value, dict) else value


def main_landings(repo, ref, since, until):
    start, end = dt.date.fromisoformat(since), dt.date.fromisoformat(until)
    if end - start != dt.timedelta(days=7):
        raise ValueError("read exactly one seven-day window; dates are half-open UTC")
    if git(repo, "rev-parse", "--is-shallow-repository").strip() != b"false":
        raise ValueError("shallow or unknown Git history cannot prove an empty week")
    previous = start - dt.timedelta(days=7)
    head = git(repo, "rev-parse", "--verify", "--end-of-options", ref + "^{commit}").decode().strip()
    records = git(repo, "log", "--first-parent", "--format=%H%x00%ct%x00%B%x00", head).split(b"\0")
    landings = []
    for i in range(0, len(records) - 2, 3):
        sha, epoch, body = records[i].strip().decode(), records[i + 1].decode(), records[i + 2].decode()
        date = dt.datetime.fromtimestamp(int(epoch), dt.timezone.utc).date()
        if not previous <= date < end:
            continue
        title = body.splitlines()[0] if body.splitlines() else ""
        if not title.startswith("train:"):
            continue
        count = re.search(r"\((\d+) changes\)", title)
        ids = re.findall(r"\[ship-a-change ([0-9a-f]{8})(?:,|\])", body)
        clean = bool(count) and len(ids) == int(count.group(1)) and len(set(ids)) == len(ids)
        landings.append({"merge": sha, "date": date.isoformat(), "short_ids": ids,
                         "expected": int(count.group(1)) if count else None,
                         "body_complete": clean})
    return head, landings


def listing(pages):
    if not isinstance(pages, list) or not pages:
        raise ValueError("native listing unavailable")
    rows, total = [], None
    for page in pages:
        if not isinstance(page, dict) or type(page.get("total")) is not int or page["total"] < 0:
            raise ValueError("native total missing or malformed")
        if total is not None and page["total"] != total:
            raise ValueError("native total changed between pages")
        total = page["total"]
        if not isinstance(page.get("data"), list):
            raise ValueError("native rows missing or malformed")
        rows.extend(page["data"])
    ids = [row.get("id") for row in rows if isinstance(row, dict)]
    if len(ids) != len(rows) or not all(job_id(i) for i in ids):
        raise ValueError("native row identity malformed")
    if any(not isinstance(row.get("metadata"), dict) for row in rows):
        raise ValueError("native row metadata malformed")
    if len(ids) != len(set(ids)) or len(rows) != total:
        raise ValueError("native listing incomplete or duplicated")
    return {row["id"]: row for row in rows}


def index(records):
    result = {}
    if not isinstance(records, list):
        raise ValueError("native full records missing")
    for record in records:
        record = unwrap(record)
        if not isinstance(record, dict) or not job_id(record.get("id")) or record["id"] in result:
            raise ValueError("native full record identity malformed or duplicated")
        steps = record.get("steps")
        if not isinstance(record.get("metadata"), dict) or not isinstance(steps, list) or any(
            not isinstance(step, dict) or not isinstance(step.get("metadata"), dict) for step in steps
        ):
            raise ValueError("native full record metadata or steps malformed")
        result[record["id"]] = record
    return result


def commit_matches(repo, short, commit):
    if not isinstance(short, str) or re.fullmatch(r"[0-9a-f]{12,40}", short) is None:
        return False
    return git(repo, "rev-parse", "--verify", "--end-of-options", short + "^{commit}").decode().strip() == commit


def car_diff(repo, car, listed, train, landing, all_ids):
    md = car.get("metadata", {})
    if car.get("kind") != "ship-a-change" or md.get("outcome") != "merged":
        raise ValueError("car has no merged outcome")
    if any(md.get(k) != listed.get("metadata", {}).get(k)
           for k in ["boarded_head", "train", "merge_ref", "outcome"]):
        raise ValueError("car binding changed since listing")
    head = md.get("boarded_head")
    if not full_sha(head):
        raise ValueError("boarded head is missing or malformed")
    receipt = md.get("regate_receipt")
    if receipt is None:
        gates = [s for s in car.get("steps", []) if s.get("spec_slug") == "gate"]
        if len(gates) != 1 or gates[0].get("status") != "completed":
            raise ValueError("authoritative completed gate missing")
        receipt = gates[0].get("metadata", {}).get("receipt")
    try:
        receipt = json.loads(receipt)
    except (TypeError, ValueError) as error:
        raise ValueError("authoritative receipt malformed") from error
    if not isinstance(receipt, dict) or receipt.get("verdict") != "green" or receipt.get("head") != head:
        raise ValueError("authoritative GREEN receipt does not bind boarded head")
    if not isinstance(train, dict) or train.get("kind") != "pr-train" or train.get("id") != md.get("train"):
        raise ValueError("landed train binding missing")
    aboard = train.get("metadata", {}).get("boarded_jobs")
    if not isinstance(aboard, list) or len(aboard) != len(set(aboard)) or set(aboard) != set(all_ids):
        raise ValueError("train consist differs from the actual squash body")
    merged = [s for s in train.get("steps", []) if s.get("spec_slug") == "merged"]
    if len(merged) != 1 or merged[0].get("status") != "completed":
        raise ValueError("train merge is not completed")
    merge = landing["merge"]
    if not commit_matches(repo, md.get("merge_ref"), merge) or not commit_matches(repo, merged[0].get("metadata", {}).get("merge_ref"), merge):
        raise ValueError("car/train merge differs from frozen main landing")
    git(repo, "cat-file", "-e", head + "^{commit}")
    bases = git(repo, "merge-base", "--all", merge + "^", head).decode().splitlines()
    if len(bases) != 1 or not full_sha(bases[0]):
        raise ValueError("source baseline is absent or ambiguous")
    base = bases[0]
    fields = git(repo, "diff", "--no-ext-diff", "--no-textconv", "--find-renames", "--numstat", "-z", base, head, "--").split(b"\0")
    files, adds, removes, binary, core, pos = 0, 0, 0, 0, False, 0
    while pos < len(fields) and fields[pos]:
        added, removed, path = fields[pos].split(b"\t", 2)
        pos += 1
        paths = [path]
        if not path:  # -z rename shape carries both paths separately.
            if pos + 1 >= len(fields):
                raise ValueError("rename diff row incomplete")
            paths = fields[pos:pos + 2]
            pos += 2
        files += 1
        core = core or any(p.startswith(b"crates/core/") for p in paths)
        if added == b"-" and removed == b"-":
            binary += 1
        elif added.isdigit() and removed.isdigit():
            adds += int(added)
            removes += int(removed)
        else:
            raise ValueError("line counts malformed")
    return {"id": car["id"], "merge": merge, "head": head, "base": base,
            "files": files, "adds": adds if not binary else None,
            "removes": removes if not binary else None, "binary_files": binary, "core": core,
            "state": "measured"}


def metric(values, coverage_unknown=False, operation=statistics.median):
    known = [v for v in values if v is not None]
    if coverage_unknown or len(known) != len(values):
        return {"state": "unknown", "value": None, "known_samples": len(known)}
    if not known:
        return {"state": "undefined", "value": None, "known_samples": 0}
    return {"state": "measured", "value": operation(known), "known_samples": len(known)}


def measure(repo, ref, since, until, bundle):
    head, landings = main_landings(repo, ref, since, until)
    native_error = None
    try:
        rows, cars, trains = listing(bundle.get("pages")), index(bundle.get("cars")), index(bundle.get("trains"))
    except (ValueError, AttributeError) as error:
        native_error, rows, cars, trains = str(error), {}, {}, {}
    start = dt.date.fromisoformat(since)
    def week(begin, end):
        selected = [l for l in landings if begin.isoformat() <= l["date"] < end.isoformat()]
        count = sum(l["expected"] or 0 for l in selected)
        incomplete = any(not l["body_complete"] for l in selected)
        results, seen = [], set()
        for landing in selected:
            ids = []
            for short in landing["short_ids"]:
                matches = [cid for cid in rows if cid.startswith(short)]
                ids.append(matches[0] if len(matches) == 1 else None)
            for cid, short in zip(ids, landing["short_ids"]):
                try:
                    if native_error: raise ValueError(native_error)
                    if incomplete or cid is None or None in ids: raise ValueError("squash car identities incomplete or ambiguous")
                    if cid in seen: raise ValueError("car appears in more than one landing")
                    seen.add(cid)
                    car = cars.get(cid)
                    if not isinstance(car, dict): raise ValueError("full landed car unavailable")
                    results.append(car_diff(repo, car, rows[cid], trains.get(car.get("metadata", {}).get("train")), landing, ids))
                except (ValueError, TypeError, KeyError, UnicodeError) as error:
                    results.append({"id": cid or short, "merge": landing["merge"], "state": "unknown", "reason": str(error)})
        good = [r for r in results if r["state"] == "measured"]
        unknown = bool(native_error) or incomplete or len(good) != count
        ratio = metric([r.get("adds") for r in good], unknown, sum)
        if ratio["state"] == "measured":
            if any(r["removes"] is None for r in good): ratio = {"state": "unknown", "value": None}
            elif ratio["value"] == 0: ratio = {"state": "undefined", "value": None, "reason": "nothing added"}
            else: ratio["value"] = sum(r["removes"] for r in good) / ratio["value"]
        return {"since": begin.isoformat(), "until": end.isoformat(),
                "cars_landed": {"state": "unknown" if incomplete else "measured", "value": None if incomplete else count},
                "median_files": metric([r["files"] for r in good], unknown),
                "median_adds": metric([r["adds"] for r in good], unknown),
                "median_removes": metric([r["removes"] for r in good], unknown),
                "removes_adds_ratio": ratio,
                "core_share": metric([int(r["core"]) for r in good], unknown, lambda xs: sum(xs) / len(xs)),
                "coverage": {"measured_cars": len(good), "unknown_cars": max(0, count - len(good)),
                             "native_error": native_error, "squash_body_complete": not incomplete}, "cars": results}
    current = week(start, dt.date.fromisoformat(until))
    previous = week(start - dt.timedelta(days=7), start)
    trend = {}
    for key in ["cars_landed", "median_files", "median_adds", "median_removes", "removes_adds_ratio", "core_share"]:
        now, before = current[key], previous[key]
        state = "measured" if now["state"] == before["state"] == "measured" else ("unknown" if "unknown" in [now["state"], before["state"]] else "undefined")
        trend[key] = {"state": state, "delta": now["value"] - before["value"] if state == "measured" else None}
    return {"version": 1, "ref": ref, "main_head": head,
            "method": "per-car frozen source diff from its unique Git merge-base with the main landing parent; UTC seven-day windows; ratio is summed removals/adds; core share is fraction of cars touching crates/core/",
            "limits": "Source diffs can overlap and do not allocate the assembled train net diff. Binary line counts remain unknown. Native reads are not an atomic snapshot; changed totals/bindings are refused. Git failure is never an empty week. No runtime or simplification judgement follows from these readings alone.",
            "current": current, "previous": previous, "trend": trend}


def api_read(path):
    result = subprocess.run(["boss-api", "GET", path], capture_output=True, text=True)
    if result.returncode:
        raise ValueError("native read refused: " + result.stderr)
    try: return json.loads(result.stdout)
    except ValueError as error: raise ValueError("native read is not JSON") from error


def collect(repo, ref, since, until, read=api_read):
    _, landings = main_landings(repo, ref, since, until)
    pages, offset = [], 0
    while True:
        page = read(f"/api/jobs?kind=ship-a-change&limit=1000&offset={offset}")
        pages.append(page)
        if not isinstance(page, dict) or not isinstance(page.get("data"), list) or type(page.get("total")) is not int:
            raise ValueError("native listing malformed")
        offset += len(page["data"])
        if offset >= page["total"]: break
        if not page["data"]: raise ValueError("native listing ended before total")
        if len(pages) > 1 and page["total"] != pages[0]["total"]:
            raise ValueError("native total changed during collection")
    rows = listing(pages)
    shorts = {s for l in landings for s in l["short_ids"]}
    selected = [cid for cid in rows if cid[:8] in shorts]
    cars = list(index([unwrap(read("/api/jobs/" + cid)) for cid in selected]).values())
    tids = {c.get("metadata", {}).get("train") for c in cars if isinstance(c, dict)}
    trains = [unwrap(read("/api/jobs/" + tid)) for tid in sorted(t for t in tids if job_id(t))]
    return {"pages": pages, "cars": cars, "trains": trains}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent))
    parser.add_argument("--ref", required=True, help="protected main ref or its captured full SHA")
    parser.add_argument("--since", required=True)
    parser.add_argument("--until", required=True)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--input", help="copied native pages/cars/trains JSON; never a new store")
    inputs.add_argument("--collect", action="store_true", help="read existing native records through boss-api GET")
    args = parser.parse_args()
    try:
        bundle = collect(args.repo, args.ref, args.since, args.until) if args.collect else json.loads(Path(args.input).read_text())
        print(json.dumps(measure(args.repo, args.ref, args.since, args.until, bundle), indent=2))
    except (ValueError, OSError, TypeError) as error:
        print(json.dumps({"state": "unknown", "reason": str(error)}))
        return 75
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
