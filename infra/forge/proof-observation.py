#!/usr/bin/python3 -I
"""A registered observation is separate evidence, never a replacement proof.

The caller is the existing protected probe account. This module has only
GET and fixed git reads; the native ops executor owns its immutable result.
"""
import datetime
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys

SOURCE = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("proof_observers", SOURCE / "infra/forge/proof-observer.py")
observers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observers)
e = observers.e
# Leave room for the executor's prefix under its existing102400-byte cap.
MAX_OUTPUT = 90000


def encoded(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(",", ":")).encode()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def timeout_diagnostic(error):
    # TimeoutExpired.output/stderr are the bytes communicate() captured
    # before its deadline. str(error) contains neither: keep those exact
    # bytes, including invalid UTF-8, apart from the command/deadline facts.
    streams = {}
    for name, captured in (("stdout", error.output), ("stderr", error.stderr)):
        raw = captured.encode() if isinstance(captured, str) else captured or b""
        streams[name] = {"bytes": len(raw), "sha256": digest(raw),
                         "base64": base64.b64encode(raw).decode("ascii"),
                         "retained_complete": True}
    return {"kind": "subprocess-timeout", "command": error.cmd,
            "timeout_seconds": error.timeout, "complete": True,
            "process_output_complete": False, "streams": streams}


def observe(name, read, git, now):
    result = {"schema": 1, "kind": "registered-proof-observation", "observation": "unknown"}
    try:
        contract = observers.contract(name)
        raw_contract = (SOURCE / "infra/platform/proof-observers" / (name + ".json")).read_bytes()
        result["contract"] = {"name": name, "sha256": digest(raw_contract), "body": contract}
        original_raw = read("/api/jobs/" + contract["car"])
        original = e.packet(e.document(original_raw), "ship-a-change")
        md = original["metadata"]
        if (original["id"] != contract["car"] or md.get("branch") != contract["branch"]
                or md.get("merged") != "true" or any(md.get(key) in (True, "true") for key in ("unlanded", "abandoned", "disproved"))):
            raise e.Unknown("registered original is not the bound landed car")
        merge = md.get("merge_ref")
        if not isinstance(merge, str) or not re.fullmatch(r"[0-9a-f]{7,40}", merge):
            raise e.Unknown("original fixed merge unavailable")
        resolved = git("rev-parse", merge + "^{commit}")
        head = git("rev-parse", "HEAD")
        if any(not re.fullmatch(r"[0-9a-f]{40}", value) for value in (resolved, head)):
            raise e.Unknown("native merge or source artifact identity unavailable")
        epoch = git("show", "-s", "--format=%ct", resolved, "--")
        e.cutoff(epoch)
        # Reference complete immutable histories instead of silently replacing
        # or reprinting them into a carrier smaller than those histories.
        history = {"steps": original.get("steps"), "proof_attempt": md.get("proof_attempt"),
                   "reproof": md.get("reproof"), "owed_proof": md.get("owed_proof")}
        result["original"] = {"car": original["id"], "branch": md["branch"], "status": original["status"],
                              "merge_ref": resolved, "converged_epoch": epoch,
                              "whole_get_sha256": digest(original_raw), "whole_get_bytes": len(original_raw),
                              "proof_history_sha256": digest(encoded(history)),
                              "proof_expect": md.get("proof_expect")}
        result["source_head"] = head
        pages, rows = [], {}
        def captured(path):
            if path == "/api/jobs/" + contract["car"]:
                return original_raw
            raw = read(path)
            body = e.document(raw)
            pages.append({"path": path, "bytes": len(raw), "sha256": digest(raw)})
            if isinstance(body, dict) and isinstance(body.get("data"), list):
                for row in body["data"]:
                    if isinstance(row, dict) and isinstance(row.get("id"), str): rows[row["id"]] = row
            return raw
        clock = []
        def observed_now():
            value = now() if callable(now) else now
            clock.append(value)
            return value
        code, observed = observers.run(contract, captured,
                                       {"BOSS_CAR_CONVERGED_AT": epoch, "BOSS_CAR_MERGE_REF": resolved}, git, observed_now)
        allowed = {"schema", "car", "claim", "retained_rows", "observation", "matches", "coverage", "why", "unjudged", "excluded"}
        if (not isinstance(observed, dict) or set(observed) - allowed or type(observed.get("schema")) is not int
                or observed["schema"] != 1 or observed.get("car") != contract["car"] or observed.get("claim") != contract["claim"]
                or type(observed.get("retained_rows")) is not int or observed["retained_rows"] != len(rows)
                or type(code) is not int or len(clock) != 1
                or {"seen": 0, "unseen": 1, "unknown": 75}.get(observed.get("observation")) != code):
            raise e.Unknown("registered observation envelope, claim or exit differs")
        if code in (0, 1):
            matches = observed.get("matches", [])
            if not isinstance(matches, list) or (code == 0 and not matches):
                raise e.Unknown("complete native witness roster unavailable")
            expected, _ = e.observation(matches, e.cutoff(epoch) + contract["delay_seconds"] * 10**9, clock[0], observed.get("coverage"))
            if expected != code: raise e.Unknown("observation lacks its declared witness or complete coverage")
        # The versioned observer validates whole native inputs. Keep complete
        # matching packets, including actor/host/server bounds, for its witness.
        witnesses = [rows[identity] for identity in dict.fromkeys(match["id"] for match in observed.get("matches", []))]
        result.update(observed)
        result["witnesses"] = witnesses
        result["query_pages"] = pages
        result["retention"] = {
            "query_pages": "references only; complete selected witness packets retained",
            "original": "exact GET and proof-history hashes; unchanged originals remain at the exact car resource",
            "failure": "UNKNOWN and complete diagnostic; never a cropped positive witness",
        }
        if len(encoded(result)) + 1 > MAX_OUTPUT:
            raise e.Unknown("whole native observation exceeds existing output carrier bound; no witness is conserved")
        return code, result
    except (e.Unknown, OSError, ValueError, TypeError, KeyError, subprocess.TimeoutExpired) as error:
        # Do not crop a required original into apparent evidence. Keep the
        # precise refusal; contract/original references remain independently readable.
        result.pop("witnesses", None)
        result.pop("matches", None)
        result.pop("query_pages", None)
        result["observation"] = "unknown"
        result["why"] = str(error)
        result["retention"] = {"failure": "complete diagnostic; required witness not conserved"}
        if isinstance(error, subprocess.TimeoutExpired):
            diagnostic = timeout_diagnostic(error)
            result["diagnostic"] = diagnostic
            result["retention"]["failure"] = "complete captured timeout diagnostic; process output incomplete at deadline; required witness not conserved"
            if len(encoded(result)) + 1 > MAX_OUTPUT:
                # Prefer captured originals over repeating large metadata
                # already available at its immutable car resource.
                prior = result
                result = {"schema": 1, "kind": "registered-proof-observation", "observation": "unknown",
                          "why": str(error), "diagnostic": diagnostic, "retention": {
                              "failure": prior["retention"]["failure"],
                              "original": "references only; unchanged complete original remains at the exact car resource"}}
                for key in ("source_head",):
                    if key in prior: result[key] = prior[key]
                if "contract" in prior:
                    result["contract_reference"] = {key: prior["contract"][key] for key in ("name", "sha256")}
                if "original" in prior:
                    result["original_reference"] = {key: prior["original"][key] for key in
                        ("car", "whole_get_sha256", "whole_get_bytes", "proof_history_sha256")}
            if len(encoded(result)) + 1 > MAX_OUTPUT:
                raw = encoded(diagnostic)
                diagnostic.update(complete=False, bytes=len(raw), sha256=digest(raw))
                for stream in diagnostic["streams"].values():
                    stream.pop("base64")
                    stream["retained_complete"] = False
                result["retention"]["failure"] = "captured timeout streams exceed carrier; bytes and hashes retained, diagnostic incomplete; required witness not conserved"
            if len(encoded(result)) + 1 > MAX_OUTPUT:
                # Original-car metadata can itself fill the carrier. Keep
                # the bounded timeout facts, never discard its stream hashes
                # merely to keep those independently readable references.
                result = {"schema": 1, "kind": "registered-proof-observation", "observation": "unknown",
                          "why": "whole timeout diagnostic exceeds existing carrier; original diagnostic is not conserved",
                          "diagnostic": diagnostic, "retention": {"failure": "timeout diagnostic incomplete"}}
            if len(encoded(result)) + 1 > MAX_OUTPUT:
                command = encoded(diagnostic["command"])
                diagnostic["command"] = {"retained_complete": False, "bytes": len(command), "sha256": digest(command)}
                diagnostic["complete"] = False
            return 75, result
        if len(encoded(result)) + 1 > MAX_OUTPUT:
            diagnostic = str(error).encode()
            result = {"schema": 1, "kind": "registered-proof-observation", "observation": "unknown",
                      "why": "whole failure diagnostic exceeds existing carrier; original diagnostic is not conserved",
                      "diagnostic": {"complete": False, "bytes": len(diagnostic), "sha256": digest(diagnostic)}}
        return 75, result


def main(argv):
    if len(argv) != 1:
        print(json.dumps({"schema": 1, "kind": "registered-proof-observation", "observation": "unknown", "why": "one registered contract name required"}))
        return 75
    def invoke(args):
        answer = subprocess.run(args, capture_output=True, timeout=20, check=False)
        if answer.returncode:
            raise e.Unknown("protected read or fixed source lookup refused: " + json.dumps({
                "exit": answer.returncode, "stdout": answer.stdout.decode(errors="replace"),
                "stderr": answer.stderr.decode(errors="replace")}))
        return answer.stdout
    def read(path): return invoke(["bash", str(SOURCE / "infra/forge/probe-bin/boss-sor-read"), path])
    def git(*args): return invoke(["git", *args]).decode().strip()
    def now(): return e.instant(datetime.datetime.now(datetime.timezone.utc).isoformat())
    code, result = observe(argv[0], read, git, now)
    print(encoded(result).decode())
    return code


if __name__ == "__main__": raise SystemExit(main(sys.argv[1:]))
