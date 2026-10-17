#!/usr/bin/env python3
"""Read-only CI selection; stale tools or missing trusted facts keep image CI."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import types
import re

RECIPE_FILES = ("Dockerfile", "required-tools.txt")


def select(repo, base, head, stamp):
    def git(*args):
        return subprocess.check_output(["git", "-C", repo, *args], stderr=subprocess.PIPE)

    result = None

    def heavy(reason):
        return {"schema": 1, "route": "software", "reason": reason, "obligations": [],
                "requested_base":base,"requested_head":head,"candidate_plan":result}

    try:
        if not all(isinstance(ref,str) and re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}",ref) for ref in (base,head)):
            return heavy("CI requires full frozen base/head identities")
        # The unmerged change cannot change the judgement that grants its skip.
        source = git("show", base + ":infra/forge/delivery-plan.py").decode()
        trusted = types.ModuleType("trusted_base_delivery_plan")
        exec(compile(source, "trusted-base:infra/forge/delivery-plan.py", "exec"), trusted.__dict__)
        result = trusted.git_plan(repo, base, head)
        if result.get("route") != "data":
            return result
        recipe = b"".join(git("show", result["head"] + ":infra/forge/boss-ci/" + name) for name in RECIPE_FILES)
        expected = hashlib.sha256(recipe).hexdigest()
        observed = pathlib.Path(stamp).read_text().strip()
        if observed != expected:
            return heavy("cached runner recipe does not match frozen source: expected " + expected)
        return {**result, "ci_runner_recipe_sha256": expected}
    except Exception as error:
        # A decoder/tool failure is not a claim that the change was validated.
        # Retain its named cause and run the existing heavier CI path.
        stderr = getattr(error,"stderr",b"")
        if isinstance(stderr,bytes):
            stderr = stderr.decode(errors="replace")
        return {**heavy("trusted CI plan unavailable: " + type(error).__name__ + ": " + str(error)),
                "failure":{"kind":type(error).__name__,"message":str(error),"stderr":stderr}}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--stamp", default="/etc/boss-ci-stamp")
    args = parser.parse_args()
    print(json.dumps(select(args.repo, args.base, args.head, args.stamp), sort_keys=True, separators=(",", ":")))
