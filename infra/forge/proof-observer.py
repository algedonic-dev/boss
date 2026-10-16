#!/usr/bin/python3 -I
"""Registry-owned observers over the existing protected GET reader.

The registry selects a module, never a branch in the generic transport.
This command observes; it cannot publish a proof or change a packet.
"""
import datetime
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys

SOURCE = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


e = load("native_evidence", SOURCE / "infra/forge/native-evidence.py")


def contract(name):
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", name): raise e.Unknown("registered observer name required")
    value = e.document((SOURCE / "infra/platform/proof-observers" / (name + ".json")).read_bytes())
    if (not isinstance(value, dict) or type(value.get("schema")) is not int or value["schema"] != 1
            or type(value.get("version")) is not int or value["version"] < 1
            or not name.endswith("-v" + str(value["version"]))
            or not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", value.get("module", ""))
            or not isinstance(value.get("claim"), str) or not value["claim"].strip()
            or not isinstance(value.get("query"), str) or not value["query"].startswith("/api/jobs?kind=")
            or type(value.get("delay_seconds")) is not int or not 0 <= value["delay_seconds"] <= 86400):
        raise e.Unknown("registered observer contract invalid")
    e.identity(value.get("car"))
    return value


def context(spec, read, env, git):
    cut = e.cutoff(env.get("BOSS_CAR_CONVERGED_AT"))
    merge = env.get("BOSS_CAR_MERGE_REF", "")
    if not re.fullmatch(r"[0-9a-f]{7,40}", merge): raise e.Unknown("fixed car merge required")
    control = e.packet(e.document(read("/api/jobs/" + spec["car"])), "ship-a-change")
    if control["id"] != spec["car"] or control["metadata"].get("branch") != spec["branch"]:
        raise e.Unknown("known car identity or branch differs")
    recorded = control["metadata"].get("merge_ref", "")
    if (not isinstance(recorded, str) or not re.fullmatch(r"[0-9a-f]{7,40}", recorded)
            or git("rev-parse", merge + "^{commit}") != git("rev-parse", recorded + "^{commit}")
            or e.cutoff(git("show", "-s", "--format=%ct", merge, "--")) != cut):
        raise e.Unknown("fixed convergence is not the known car merge")
    git("merge-base", "--is-ancestor", merge, "HEAD")
    return cut + spec["delay_seconds"] * 10**9


def run(spec, read, env, git, now):
    cut = context(spec, read, env, git)
    rows = e.pages(read, spec["query"], spec["kind"])
    # Native completions can occur while pages are read. Sample the
    # caller's clock after that read, not before admitting its evidence.
    observed_at = now() if callable(now) else now
    observer = load("registered_observer", SOURCE / "infra/forge/proof-observers" / (spec["module"] + ".py"))
    code, result = observer.observe(e, rows, cut, observed_at, spec)
    return code, {**result, "schema": 1, "car": spec["car"], "claim": spec["claim"], "retained_rows": len(rows)}


def main(argv):
    if len(argv) != 1: raise e.Unknown("one registered observer name required")
    def invoke(args):
        answer = subprocess.run(args, capture_output=True, timeout=20, check=False)
        if answer.returncode: raise e.Unknown("protected read or fixed merge lookup refused")
        return answer.stdout
    def read(path): return invoke(["bash", str(SOURCE / "infra/forge/probe-bin/boss-sor-read"), path])
    def git(*args): return invoke(["git", *args]).decode().strip()
    def now(): return e.instant(datetime.datetime.now(datetime.timezone.utc).isoformat())
    code, result = run(contract(argv[0]), read, os.environ, git, now)
    print(json.dumps(result, sort_keys=True))
    return code


if __name__ == "__main__":
    try: raise SystemExit(main(sys.argv[1:]))
    except (e.Unknown, OSError, ValueError, TypeError, KeyError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"observation": "unknown", "why": str(error)}, sort_keys=True))
        raise SystemExit(75) from None
