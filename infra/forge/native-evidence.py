#!/usr/bin/python3 -I
"""Strict native evidence shared by versioned observer contracts (4196a7b1).

No write, credential or domain effect lives here. Transport is supplied by
the existing protected reader; caller-specific meaning stays in observers.
"""
import datetime
import json
import re
import uuid


class Unknown(ValueError):
    pass


def document(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result: raise Unknown("duplicate JSON key")
            result[key] = value
        return result
    def constant(_): raise Unknown("non-JSON constant")
    try:
        if len(raw) > 16 * 1024 * 1024: raise Unknown("whole evidence document exceeds byte bound")
        return json.loads(raw, object_pairs_hook=unique, parse_constant=constant)
    except (ValueError, UnicodeError, TypeError) as error:
        raise Unknown("expected exactly one complete nonempty JSON document") from error


def instant(value):
    if not isinstance(value, str): raise Unknown("server instant missing")
    match = re.fullmatch(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})", value)
    if not match: raise Unknown("server instant must be RFC3339")
    try:
        seconds, fraction, offset = match.groups()
        stamp = datetime.datetime.fromisoformat(seconds + offset.replace("Z", "+00:00"))
        delta = stamp - datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
        return (delta.days * 86400 + delta.seconds) * 10**9 + int((fraction or "").ljust(9, "0"))
    except ValueError as error: raise Unknown("invalid server instant") from error


def cutoff(value):
    if not isinstance(value, str) or not re.fullmatch(r"[1-9][0-9]*", value):
        raise Unknown("positive fixed car convergence epoch required")
    return int(value) * 10**9


def identity(value):
    try:
        if str(uuid.UUID(value)) != value or uuid.UUID(value).int == 0: raise ValueError()
    except (ValueError, TypeError, AttributeError) as error: raise Unknown("native UUID missing or invalid") from error
    return value


def packet(row, kind):
    if not isinstance(row, dict): raise Unknown("packet is not an object")
    name = identity(row.get("id"))
    if (row.get("kind") != kind or row.get("status") not in ("open", "closed")
            or row.get("partition") != "real" or row.get("simulated") is not False
            or not isinstance(row.get("metadata"), dict)
            or (row["metadata"].get("simulated") is not None and row["metadata"].get("simulated") is not False)):
        raise Unknown(name + ": native kind, partition or simulation provenance mismatch")
    instant(row.get("opened_at"))
    return row


def pages(read, path, kind, limit=100):
    rows, ids, total, offset = [], set(), None, 0
    while total is None or offset < total:
        page = document(read(path + "&full=true&limit=" + str(limit) + "&offset=" + str(offset)))
        if (not isinstance(page, dict) or page.get("full") is not True
                or type(page.get("limit")) is not int or page["limit"] != limit
                or type(page.get("offset")) is not int or page["offset"] != offset
                or type(page.get("total")) is not int or not 0 <= page["total"] <= 10000
                or not isinstance(page.get("data"), list)):
            raise Unknown("native page envelope incomplete or over bound")
        if total is None: total = page["total"]
        if total != page["total"] or len(page["data"]) != min(limit, total - offset):
            raise Unknown("native page count changed or is incomplete")
        for row in page["data"]:
            packet(row, kind)
            if row["id"] in ids: raise Unknown(row["id"] + ": duplicate native identity across pages")
            ids.add(row["id"]); rows.append(row)
        offset += len(page["data"])
        if total == 0: break
    return rows


def completion_step(row, slug):
    name = row["id"]
    steps = row.get("steps")
    if not isinstance(steps, list): raise Unknown(name + ": whole native steps unavailable")
    found = [s for s in steps if isinstance(s, dict) and s.get("spec_slug") == slug]
    if len(found) != 1: raise Unknown(name + ": native completion step missing or ambiguous")
    step = found[0]
    try: identity(step.get("id"))
    except Unknown as error: raise Unknown(name + ": native step identity unavailable") from error
    if step.get("job_id") != name: raise Unknown(name + ": native completion step belongs to another packet")
    return step


def execution(row, contract, cut, now):
    packet(row, contract["kind"])
    name = row["id"]
    def fail(message): raise Unknown(name + ": " + message)
    opened = instant(row.get("opened_at"))
    if opened > now: fail("server admission is in the future")
    if row["metadata"].get("host") != contract["host"]: fail("native request host differs")
    if "verb" in contract and row["metadata"].get("verb") != contract["verb"]:
        fail("native request verb differs from declared evidence contract")
    step = completion_step(row, contract["step"]); metadata = step.get("metadata")
    if (row.get("status") != "closed" or step.get("status") != "completed"
            or step.get("completed_by") != contract["completion_actor"] or not isinstance(metadata, dict)
            or metadata.get("runner_host") != contract["host"]
            or metadata.get("disposition") != contract["disposition"]): fail("native completion provenance differs")
    completed = instant(step.get("completed_at"))
    if completed < opened or completed > now: fail("server completion is outside admission/read bounds")
    return step if opened > cut else None


def receipt(row, completion_actors):
    """Conserve the terminal distinction before binding an executed tree.

    Called by a gate observer, not a core workflow-kind switch. Every
    disagreement names its packet so a refusal is already a diagnosis.
    """
    name = identity(row.get("id"))
    def fail(message): raise Unknown(name + ": " + message)
    step = completion_step(row, "record-verdict")
    if step.get("status") != "completed": fail("native verdict is not completed")
    md = step.get("metadata")
    if not isinstance(md, dict): fail("native verdict metadata unreadable")
    verdict = md.get("verdict")
    if row["metadata"].get("outcome") not in (None, "completed" if verdict == "green" else verdict): fail("contradictory terminal")
    if verdict in ("lost", "refused"): return "unknown", step, None
    raw = md.get("receipt")
    try: body = document(raw) if isinstance(raw, str) else raw
    except Unknown as error: fail(str(error))
    if not isinstance(body, dict): fail("native receipt unavailable")
    if verdict == "withdrawn":
        if (body.get("verdict") != verdict or body.get("head") != "" or body.get("mode") != ""
                or body.get("fails") != [] or not isinstance(body.get("withdrawn_because"), str)
                or not body["withdrawn_because"].strip()): fail("contradictory no-compute withdrawal")
        return "no-compute", step, body
    if verdict not in ("green", "failed"): fail("unknown native terminal")
    if not isinstance(completion_actors, list) or not completion_actors or step.get("completed_by") not in completion_actors:
        fail("native execution completion actor differs")
    if (body.get("verdict") != verdict or not isinstance(body.get("head"), str)
            or not re.fullmatch(r"[0-9a-f]{40}", body["head"]) or body["head"] != row["metadata"].get("sha")):
        fail("receipt is not bound to native verdict and head")
    checks, fails = body.get("checks"), body.get("fails")
    if not isinstance(checks, list) or not isinstance(fails, list) or any(not isinstance(v, str) for v in fails): fail("executed checks unavailable")
    if verdict == "green" and (fails or any(not isinstance(c, dict) or c.get("result") == "fail" for c in checks)):
        fail("green receipt contradicts failure evidence")
    return "executed", step, body


def observation(matches, cut, now, coverage=None):
    if matches: return 0, {"observation": "seen", "matches": matches}
    if coverage is None:
        return 75, {"observation": "unknown", "why": "no witnessed event; retained inventory is not durable coverage"}
    if (not isinstance(coverage, dict) or coverage.get("gaps") != []
            or not isinstance(coverage.get("source"), str) or not coverage["source"].strip()
            or instant(coverage.get("begin_at")) > cut or instant(coverage.get("through_at")) != now):
        raise Unknown("native unseen coverage is incomplete")
    return 1, {"observation": "unseen", "coverage": coverage}
