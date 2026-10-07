"""Fixed image-side harness; branch stdout is data, never a receipt."""
import json
import os
import pathlib
import signal
import subprocess
import sys
import time

head, baseline, budget, identity, checkout = sys.argv[1:]
root = pathlib.Path(checkout)
env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": "/tmp/consist-home",
       "BOSS_MACHINE_TOKEN_DIR": "/tmp/consist-home/no-token",
       "BOSS_SOR_ENV": "/tmp/consist-home/no-sor.env", "GIT_CONFIG_NOSYSTEM": "1"}
pathlib.Path(env["BOSS_MACHINE_TOKEN_DIR"]).mkdir(parents=True, exist_ok=True)
deadline = time.monotonic() + float(budget)


def run(args):
    process = subprocess.Popen(args, cwd=root, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=max(0.001, deadline-time.monotonic()))
        return {"exit": process.returncode, "stdout": stdout.decode("utf-8", "replace"),
                "stderr": stderr.decode("utf-8", "replace")}
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        return {"exit": None, "stdout": stdout.decode("utf-8", "replace"),
                "stderr": stderr.decode("utf-8", "replace"), "unavailable": "consist budget spent"}


def checked_tree():
    # Verification has its own bounded read: spending the lint budget must
    # retain the partial verdict rather than erase it as a missing receipt.
    actual = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, env=env,
                            capture_output=True, text=True, timeout=5)
    trunk = subprocess.run(["git", "rev-parse", "refs/remotes/origin/main"], cwd=root,
                           env=env, capture_output=True, text=True, timeout=5)
    return actual.returncode == 0 and actual.stdout.strip() == head and \
        trunk.returncode == 0 and trunk.stdout.strip() == baseline


receipt = {"version": 1, "identity": identity, "head": head, "baseline": baseline,
           "runs": [], "warnings": []}
if not checked_tree():
    raise RuntimeError("worker checkout is not the exact assembled tree and baseline")
roster = run(["bash", "infra/gate.sh", "--exclusions"])
receipt["exclusions"] = roster
if roster["exit"] != 0:
    receipt["warnings"].append("branch exclusions unavailable: " + roster["stderr"])
else:
    excluded = {pathlib.Path(line.split("\t")[0]).name for line in roster["stdout"].splitlines()}
    scripts = sorted(p for p in (root / "infra/lint").glob("*.sh") if p.name not in excluded)
    if not scripts:
        receipt["warnings"].append("no lint scripts in the assembled tree")
    for script in scripts:
        if time.monotonic() >= deadline:
            receipt["warnings"].append("consist budget spent; remaining scripts not run")
            break
        result = run(["bash", str(script)])
        receipt["runs"].append({"name": script.stem, **result})
        if result["exit"] is None:
            break
if not checked_tree():
    raise RuntimeError("worker tree binding changed or could not be checked")
print(json.dumps(receipt), flush=True)
