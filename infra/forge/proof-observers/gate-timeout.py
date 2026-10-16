"""A native timeout witness does not judge whether the verdict quotes it."""
import re


def titles(body):
    lines = body.splitlines(); failed = []; reasons = {}; title = None
    for index, line in enumerate(lines):
        match = re.match(r"^\s*\d+\) (\S.*?)[\s─]*$", line)
        if match: title = match.group(1).strip()
        cause = re.match(r"^\s*(Test timeout of \d+ms exceeded\b.*?)\s*$", line)
        if cause and title: reasons[title] = cause.group(1)
        if re.match(r"^\s*\d+ failed\s*$", line):
            indent = len(line) - len(line.lstrip())
            for following in lines[index + 1:]:
                if not following.strip() or len(following) - len(following.lstrip()) <= indent: break
                failed.append(following.strip())
    return [{"spec": title, "timeout": reasons[title]} for title in failed if title in reasons]


def observe(evidence, rows, cut, now, contract):
    found, unknown, excluded = [], [], []
    for row in rows:
        state, step, body = evidence.receipt(row, contract["completion_actors"])
        completed = evidence.instant(step.get("completed_at"))
        opened = evidence.instant(row["opened_at"])
        if not opened <= completed <= now: raise evidence.Unknown(row["id"] + ": server completion outside bounds")
        if state == "unknown": unknown.append(row["id"]); continue
        if state == "no-compute" or completed <= cut or body["verdict"] == "green":
            excluded.append(row["id"]); continue
        launched = evidence.instant(row["metadata"].get("launched_at"))
        if not opened <= launched <= completed: raise evidence.Unknown(row["id"] + ": launch outside server bounds")
        if opened <= cut or launched <= cut: excluded.append(row["id"]); continue
        excerpts = body.get("fails_excerpt")
        if not isinstance(excerpts, dict): raise evidence.Unknown(row["id"] + ": failed-check excerpts unavailable")
        names = {c.get("name") for c in body["checks"] if isinstance(c, dict) and c.get("result") == "fail"}
        for name, excerpt in excerpts.items():
            if not isinstance(excerpt, str): raise evidence.Unknown(row["id"] + ": failed-check excerpt is not text")
            if name in names and any(v.startswith(name + ":") and "playwright verdict line" in v for v in body["fails"]):
                timed = titles(excerpt)
                if timed: found.append({"id": row["id"], "head": body["head"], "check": name, "timed_out_specs": timed})
    code, result = evidence.observation(found, cut, now)
    return code, {**result, "unjudged": unknown, "excluded": excluded}
