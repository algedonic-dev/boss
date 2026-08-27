#!/usr/bin/env bash
# gate-runner/run.sh — one gate, one Job, self-reporting.
#
# WHY THIS EXISTS. On 2026-08-22/23 gates ran as tmux trees inside the
# boss-dev pod and died four different deaths, none their own fault:
# a converge replaced the pod (twice), container limits made exec
# scopes reap the tmux server, a shared 100Gi target filled and turned
# "No space left on device" into fake code failures, and a cold
# container's first webServer boot mass-failed a mocked suite in 20s.
# Each death was reconstructed from journals after a human asked "how
# are we looking". This script is the other shape: a Kubernetes Job
# with its own clone, its own disk, a database sidecar, and a receipt
# it reports to the gate-run packet itself — so the SoR knows the
# verdict without anyone grepping a pod.
#
# Runs inside the boss-ci image (see gate-runner.yaml). Required env:
#   GATE_BRANCH        branch to gate (fetched from the forge)
#   GATE_RUN_JOB_ID    the gate-run packet this run reports to
# Optional:
#   GATE_MODE          "--auto" for scoped gates, empty for full
#   FORGE_URL          default http://10.20.0.15:3000/david/boss.git
#   JOBS_API           default http://10.20.0.34:7900
set -euo pipefail

FORGE_URL="${FORGE_URL:-http://10.20.0.15:3000/david/boss.git}"
JOBS_API="${JOBS_API:-http://10.20.0.34:7900}"
ACTOR='{"id":"automation:gate-runner","role":"platform-admin","access_tier":"operator"}'

report() { # verdict, note
    local step_id
    step_id=$(curl -sf -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID" \
        | python3 -c 'import sys,json;j=json.load(sys.stdin);print([s["id"] for s in j["steps"] if "Record" in (s.get("title") or "")][0])') || return 1
    python3 - "$1" "$2" <<'PY' > /tmp/verdict.json
import json, sys
print(json.dumps({"status": "completed",
                  "metadata": {"verdict": sys.argv[1], "receipt": sys.argv[2]}}))
PY
    curl -sf -o /dev/null -X PUT -H "x-boss-user: $ACTOR" \
        -H "Content-Type: application/json" -d @/tmp/verdict.json \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/steps/$step_id"
}

# The run itself is guarded so ANY failure below still reports `lost`
# with the reason, rather than leaving the packet to go overdue.
fail_lost() { report lost "runner died before a receipt: $1" || true; exit 1; }
trap 'fail_lost "line $LINENO"' ERR

# One job, one branch, one clean disk. A cold workspace build needs
# ~74G; sharing a warm target between branches is what filled the
# disk mid-run and manufactured failures.
rm -rf /gate-target/target
mkdir -p /gate-target/target
# The disk is a PVC and outlives the Job. The clone below refuses a
# non-empty destination, so a second run on the same disk died with
# "destination path already exists" - the third reason this rig had
# never completed a run. "Wiped per run" has to include the clone.
rm -rf /gate-target/repo
# And the previous run's receipt. It is written only at the END of a
# run, so if this one dies partway the old verdict is still sitting
# there - with a different head - looking exactly like this run's
# result. That nearly credited one branch with another branch's pass
# on 2026-08-25 when w-1 reset mid-gate.
rm -f /gate-target/receipt.json

# Forge auth. The repo is not anonymously clonable: a bare clone dies
# with "could not read Username for http://...", which is the error
# dev-node-checkout.md called the last blocker. The token arrives as a
# FILE (secret forge-read, key token, mounted at /etc/forge) and is
# read by a credential helper rather than interpolated into the URL —
# argv is world-readable to anything sharing the pid namespace, and a
# token in the clone URL also lands in .git/config on the disk.
if [ -r /etc/forge/token ]; then
    git config --global credential.helper \
        '!f() { echo username=x-access-token; echo "password=$(cat /etc/forge/token)"; }; f'
else
    echo "gate-runner: /etc/forge/token missing - the clone will fail" >&2
fi

# Own clone: no dependency on the dev pod's PVC, so this Job schedules
# wherever its nodeSelector says — deliberately NOT the etcd node.
git clone --depth 50 "$FORGE_URL" /gate-target/repo
cd /gate-target/repo
# Explicit refspec. `git fetch origin <branch>` on a shallow clone
# updates FETCH_HEAD but creates no remote-tracking ref, so the
# checkout below died with "origin/<branch> is not a commit" - the
# second reason this rig had never completed a run.
git fetch origin "$GATE_BRANCH:refs/remotes/origin/$GATE_BRANCH"
git checkout -B "$GATE_BRANCH" "origin/$GATE_BRANCH"
HEAD_SHA=$(git rev-parse HEAD)

export CARGO_TARGET_DIR=/gate-target/target
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"

# Warm the web toolchain: the mocked suite's webServer boot on a cold
# container exceeded its timeout three times on 2026-08-23; every spec
# then fails on connect within seconds. (The durable fix is the
# config-side timeout car; this keeps the first boot off the clock.)
(cd apps/web && bun install --frozen-lockfile >/dev/null 2>&1 && bun run build >/dev/null 2>&1) || true

RECEIPT=/gate-target/receipt.json
if BOSS_GATE_RECEIPT="$RECEIPT" ./infra/gate.sh ${GATE_MODE:-} > /gate-target/gate.log 2>&1; then
    VERDICT=green
else
    VERDICT=failed
fi
trap - ERR

SUMMARY=$(python3 - "$RECEIPT" "$HEAD_SHA" <<'PY'
import json, sys
try:
    r = json.load(open(sys.argv[1]))
    fails = [c["name"] for c in r["checks"] if c["result"] != "pass"]
    print(json.dumps({"verdict": r["verdict"], "head": r["head"],
                      "mode": r.get("mode"), "fails": fails}))
except Exception as e:
    print(json.dumps({"verdict": "unreadable", "head": sys.argv[2],
                      "error": str(e)}))
PY
)
# THE VERDICT GOES IN THE LOG BEFORE IT GOES ANYWHERE ELSE.
#
# It used to live in exactly two places, and on 2026-08-25 both were
# lost at once (cf0021ae): the gate passed 30/30 on
# chore/the-build-leaves-the-control-plane, w-1 rebooted before the
# pod finished, and the receipt survived only on the PVC — it had to
# be recovered by mounting the disk in a throwaway pod. The pod log
# is the third copy, it costs one line, and `kubectl logs` reaches
# it without mounting anything.
echo "gate-runner: receipt $SUMMARY"

if ! report "$VERDICT" "$SUMMARY"; then
    # THE OLD FALLBACK CLAIMED AN ALARM THAT CANNOT ALWAYS FIRE.
    #
    # It said "packet will go overdue (the alarm still works)". That
    # holds only while the packet is OPEN. The case that actually
    # burned us is the other one: a gate-run packet reused across
    # relaunches was already TERMINAL, so the step write was refused
    # AND no overdue can ever be raised against a closed packet. Both
    # channels went quiet together and the run looked like it never
    # happened.
    #
    # So the two cases are told apart and only one of them is
    # reassuring. Neither changes the exit status: this is a failure
    # to RECORD the result, not a failure of the gate, and reporting
    # a green gate as red is the confusion cf0021ae exists about.
    state=$(curl -sf -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID" \
        | python3 -c 'import sys,json; print(json.load(sys.stdin).get("status","unknown"))' \
        2>/dev/null || echo unreachable)
    echo "WARN: verdict not recorded on packet $GATE_RUN_JOB_ID (packet status: $state)"
    case "$state" in
        open)
            echo "  The packet is still open, so it will go overdue and the alarm will fire."
            ;;
        unreachable)
            echo "  The jobs API could not be reached, so the packet state is unknown."
            echo "  If it was open it will go overdue; if it was not, this log is the only record."
            ;;
        *)
            echo "  THE PACKET IS ALREADY $state, SO NOTHING WILL GO OVERDUE AND NO ALARM"
            echo "  WILL FIRE. The verdict above is the only surviving record of this run."
            echo "  A terminal packet cannot accept a verdict — file a fresh gate-run packet"
            echo "  rather than reusing one across relaunches (see 64cae7e9)."
            ;;
    esac
fi
tail -5 /gate-target/gate.log || true
echo "gate-runner: $GATE_BRANCH@${HEAD_SHA:0:10} -> $VERDICT"
[ "$VERDICT" = green ]
