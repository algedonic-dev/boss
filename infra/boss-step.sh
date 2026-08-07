#!/usr/bin/env bash
#
# boss-step — close a step on the open Job of a given Workflow.
#
#   ./infra/boss-step.sh <workflow> <step-title> [key=value ...]
#   ./infra/boss-step.sh regenerate-deployment build source_ref=abc1234
#
# The point: work a machine did should be recorded by the machine that
# did it. A regen's `build` step closing because the build script
# finished is a fact; the same step closing because someone typed
# afterwards is a claim. The reason to model an operation as a Job is
# that its state stays true without anyone maintaining it.
#
# ## Behaviour
#
# Finds the single OPEN Job of `<workflow>`, finds the step by title,
# merges the key=value pairs into its metadata, completes it.
#
# - No open Job → NO-OP, exit 0. These scripts run outside regens all
#   the time; a build is not required to belong to one, and failing a
#   build because no Job is open would be the tail wagging the dog.
# - More than one open Job → exit 1. Guessing which to close is worse
#   than stopping.
# - Step already terminal → NO-OP, exit 0. Re-running a deploy inside
#   one regen must not fail: idempotence is the contract of a step's
#   status.
#
# Metadata is MERGED, never replaced. `PUT /api/jobs/{id}/steps/{id}`
# has PATCH semantics for top-level fields but swaps `metadata`
# wholesale, so sending only new keys silently wipes the rest —
# including `authority_role`, which is what keeps a gated step gated.
#
# Talks to jobs-api directly with an actor header, like
# feedback-queue.sh: the gateway is the browser edge and strips
# inbound `x-boss-*`, so terminal tooling cannot present itself there.
#
# The python runs via `-c "$PYCODE"` with the payload on stdin rather
# than embedded in a double-quoted string. feedback-queue.sh was
# written the other way and a literal double quote silently truncated
# the program.
set -euo pipefail

if [ $# -lt 2 ]; then
    sed -n '2,12p' "$0" >&2
    exit 2
fi

WORKFLOW="$1"; shift
STEP_TITLE="$1"; shift

BASE="${BOSS_JOBS_URL:-http://127.0.0.1:7900}"

# An automated close should read as automation in the audit trail, not
# as whichever human happened to be logged in.
ACTOR="${BOSS_STEP_ACTOR:-automation:boss-step}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

if ! jobs_json=$(curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=$WORKFLOW&status=open&limit=50" 2>/dev/null); then
    echo "boss-step: jobs-api unreachable at $BASE — '$STEP_TITLE' not recorded" >&2
    exit 1
fi

PYCODE=$(cat <<'PY'
import json, subprocess, sys

workflow, step_title, base, boss_user = sys.argv[1:5]
pairs = sys.argv[5:]

body = json.load(sys.stdin)
rows = body["data"] if isinstance(body, dict) and "data" in body else body
rows = [j for j in rows if j.get("status") == "open"]

if not rows:
    print("boss-step: no open %s Job — nothing to record" % workflow, file=sys.stderr)
    sys.exit(0)
if len(rows) > 1:
    ids = ", ".join(j["id"][:8] for j in rows)
    print("boss-step: %d open %s Jobs (%s) — refusing to guess"
          % (len(rows), workflow, ids), file=sys.stderr)
    sys.exit(1)

job = rows[0]
step = None
for s in job.get("steps", []):
    if s.get("title") == step_title:
        step = s
        break
if step is None:
    titles = ", ".join(s.get("title", "?") for s in job.get("steps", []))
    print("boss-step: no step %r on %s (has: %s)"
          % (step_title, job["id"][:8], titles), file=sys.stderr)
    sys.exit(1)

if step.get("status") in ("completed", "skipped"):
    print("boss-step: %s already %s — no-op" % (step_title, step["status"]),
          file=sys.stderr)
    sys.exit(0)

merged = dict(step.get("metadata") or {})
for p in pairs:
    if "=" not in p:
        print("boss-step: %r is not key=value" % p, file=sys.stderr)
        sys.exit(2)
    k, v = p.split("=", 1)
    merged[k] = v

payload = json.dumps({"status": "completed", "metadata": merged})
url = "%s/api/jobs/%s/steps/%s" % (base, job["id"], step["id"])
r = subprocess.run(
    ["curl", "-fsS", "-X", "PUT", "-H", "content-type: application/json",
     "-H", "x-boss-user: " + boss_user, "-d", payload, url],
    capture_output=True, text=True,
)
if r.returncode != 0:
    print("boss-step: PUT failed — %s" % r.stderr.strip(), file=sys.stderr)
    sys.exit(1)
print("boss-step: closed %s/%s on %s" % (workflow, step_title, job["id"][:8]))
PY
)

printf '%s' "$jobs_json" | python3 -c "$PYCODE" \
    "$WORKFLOW" "$STEP_TITLE" "$BASE" "$BOSS_USER" "$@"
