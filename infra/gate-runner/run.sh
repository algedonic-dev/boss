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

# --- report-back (begin) ---
# THE REPORT RETRIES, because the system of record rolls.
#
# This used to be one call. A gate ends 10-40 minutes after it starts,
# the SoR Recreate-rolls for 30-90s on every train converge, and every
# merge now converges within a minute of landing - so any gate whose
# last minute overlapped a roll made its single report into a dark API,
# printed a WARN, and exited 0. Four greens were lost that way on the
# night of 2026-09-07 (backlog 23188cc5), each re-proven by hand at ~10
# minutes of cluster time. The verdict existed the whole time, in this
# pod's log; nothing was retrying the one call that mattered.
#
# So: `report` is a loop over `report_once`, sleeping GATE_REPORT_BACKOFF
# between attempts (~5 minutes in all, several times a roll), and it
# tells the two failure shapes apart. NOBODY ANSWERED (connection
# refused, timeout, empty reply, 5xx) is a roll and is retried. A REFUSAL
# (any other 4xx, or a step-selection disagreement) is about the write
# itself - a frozen step, a packet this runner does not understand - and
# retrying it for five minutes would only delay the terminal-packet
# branch below. Every attempt is logged, so a reader of the Job log sees
# the roll the runner rode out.
#
# The block is bracketed so boss-testing's gate_runner_report_retry test
# can lift it verbatim and run it against a stub SoR; the pod receives
# exactly one file, so the code cannot live anywhere else.
GATE_REPORT_BACKOFF="${GATE_REPORT_BACKOFF:-5 10 20 30 45 60 60 60}"

# THE REPORT-BACK RECORDS ITS OWN STORY, on the receipt the packet keeps.
#
# Everything the loop below knows about the roll it rode out - how many
# attempts, how many seconds, whether the system of record went dark at
# all - it prints to the pod log, and the pod log is reaped with the Job.
# The packet, which is the record, said only "green". During train #282's
# converge two gates reported cleanly across a dark SoR and there is no
# record anywhere that it happened.
#
# §Diagnosis: "an alarm that reports through its subject dies with it".
# The runner cannot report an outage THROUGH the API that is out - but
# it can carry its own account inside the payload it finally lands, so
# the outage is visible afterwards instead of only while it is happening.
# These three are set by `report` and read by `report_once` at the moment
# of the write, because the story is only true as of the attempt making
# it.
REPORT_ATTEMPT=1
REPORT_WAITED=0
REPORT_UNREACHABLE=false

# Did curl fail because nobody answered? (6 resolve, 7 refused, 28 timed
# out, 35 TLS, 52 empty reply, 55 send, 56 recv.) Those are what a roll
# looks like from a client.
report_transient_curl() { case "$1" in 6|7|28|35|52|55|56) return 0 ;; *) return 1 ;; esac; }

# One attempt. Exit 0 recorded; 75 nobody answered (retry); 1 refused.
report_once() { # verdict, note
    local rc out code body step_id
    # The reporting step is selected by its spec KEY, never its
    # rendered title: the title is prose a registry edit may change
    # on purpose, and matching it kept one fact in two places with
    # nothing holding them together (48bed517 - the old selector
    # grepped for "Record"). `spec_slug` is the same key advancement
    # pairs steps by, exposed on every materialized step row.
    # Exactly one match or refuse LOUDLY: zero means the protocol and
    # this runner disagree, two means the report would land somewhere
    # arbitrary - either way the disagreement goes to the Job log and
    # the packet goes overdue, which is the alarm this rig already
    # defines; silence is the only wrong answer.
    rc=0
    out=$(curl -s --max-time 20 -w '\n%{http_code}' -H "x-boss-user: $ACTOR" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID") || rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "gate-runner: report: GET packet failed (curl exit $rc)"
        report_transient_curl "$rc" && return 75
        return 1
    fi
    code=${out##*$'\n'}; body=${out%$'\n'*}
    case "$code" in
        2??) ;;
        5??|000) echo "gate-runner: report: GET packet answered HTTP $code"; return 75 ;;
        *) echo "gate-runner: report: GET packet answered HTTP $code"; return 1 ;;
    esac
    local picked step_status stored_verdict
    picked=$(printf '%s' "$body" | python3 -c '
import sys, json
j = json.load(sys.stdin)
j = j.get("data", j)
hits = [s for s in j["steps"] if s.get("spec_slug") == "record-verdict"]
if len(hits) != 1:
    sys.stderr.write(
        "gate-runner: expected exactly one record-verdict step, found %d"
        " (slugs: %s) - the gate-run protocol and this runner disagree\n"
        % (len(hits), [s.get("spec_slug") for s in j["steps"]]))
    sys.exit(1)
s = hits[0]
v = (s.get("metadata") or {}).get("verdict")
print("%s %s %s" % (s["id"], s.get("status") or "-", v if isinstance(v, str) and v else "-"))') || return 1
    read -r step_id step_status stored_verdict <<<"$picked"
    # A RETRY AFTER A WRITE THAT LANDED. A timeout can hide a completion
    # the SoR did record; the next attempt then finds the step terminal,
    # and the merge door below refuses a terminal step outright (the PUT
    # had an idempotent re-send carve-out; the merge door has none). If
    # the step already carries THIS verdict the report is done, and
    # saying so is the truth. A terminal step carrying anything else
    # falls through to the merge, whose 409 is the loud refusal a frozen
    # step is owed (cf0021ae).
    case "$step_status" in
        completed|skipped)
            if [ "$stored_verdict" = "$1" ]; then
                echo "gate-runner: report: record-verdict step is already $step_status and already carries verdict $1 - nothing to write"
                return 0
            fi ;;
    esac
    # A PER-INVOCATION file, not a fixed /tmp path: the block is lifted
    # verbatim by boss-testing's gate_runner_report_retry and run
    # concurrently there, where a shared name is a race that empties one
    # test's payload under another.
    local payload
    payload=$(mktemp) || return 1
    python3 - "$1" "$2" "$REPORT_ATTEMPT" "$REPORT_WAITED" "$REPORT_UNREACHABLE" <<'PY' > "$payload"
import json, sys
verdict, receipt, attempt, waited, unreachable = sys.argv[1:6]
# The receipt is a JSON STRING on the step (the encoding every reader
# already parses). Annotate it as an object and re-serialize, so the
# report-back's own story rides WITH the gate's findings rather than
# beside them - one document, one parse, and a reader that only wants
# the verdict is unaffected. A receipt that will not parse is passed
# through untouched: an unreadable receipt is evidence too, and wrapping
# it would destroy the only copy.
try:
    body = json.loads(receipt)
    if not isinstance(body, dict):
        raise ValueError("receipt is not an object")
    body["report"] = {"attempts": int(attempt), "waited_s": int(waited),
                      "sor_unreachable": unreachable == "true"}
    receipt = json.dumps(body, separators=(",", ":"))
except Exception:
    pass
print(json.dumps({"verdict": verdict, "receipt": receipt}))
PY
    # TWO WRITES: THE KEYS THROUGH THE MERGE DOOR, THEN THE STATUS ALONE.
    #
    # This was one PUT of {status, metadata: {verdict, receipt}} with no
    # read before it. The step PUT REPLACES metadata wholesale, and the
    # registry materializes keys onto every step at admission
    # (metadata_defaults, authority_role, station, audience, claimable),
    # so that body silently shed every key the runner did not think to
    # send - on every gate verdict (backlog e39a9d2a, correction
    # 2026-09-23). The server is to REFUSE such a body in that item's
    # last car, and a runner still sending it then would stop every gate
    # from reporting. PATCH .../steps/{id}/metadata merges against the
    # row as it stands, in one transaction, so it cannot race; the PUT
    # that follows carries nothing to drop.
    #
    # ORDER IS LOAD-BEARING: a step's required-at-done fields are
    # validated when it flips to completed, so the verdict must be on
    # the row before the PUT arrives. A merge that lands followed by a
    # PUT that meets a roll is retried whole by `report`: the second
    # merge rewrites the same two keys, carrying the later attempt's
    # story, which is the one that is true.
    report_write PATCH "$payload" \
        "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/steps/$step_id/metadata" "merge verdict" \
        || { rc=$?; rm -f "$payload"; return "$rc"; }
    printf '%s' '{"status":"completed"}' > "$payload"
    # A COMPLETION THAT LOST THE STEP RACE IS SENT ONCE MORE (backlog
    # 2a6d0b86). The step PUT refuses, 409 code STEP_CHANGED_CODE, a write
    # whose read another write moved before it landed (car 88123ae0) -
    # where it used to answer success and erase that write. The refusal
    # means nothing was written, and this body is status-only, so it
    # carries nothing that can be stale: the same body goes once more,
    # as boss dispatch's briefed completion does. A second loss, or any
    # other refusal, stands - refused by name, never looped. Before
    # this, the one 409 refused the report outright and the conductor
    # closed the gate-run `lost` over a real green or red.
    local url="$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/steps/$step_id"
    rc=0
    report_write PUT "$payload" "$url" "PUT completion" || rc=$?
    if [ "$rc" -eq 76 ]; then
        echo "gate-runner: report: PUT completion lost the step race to another write - nothing was written; sending the same status-only body once more"
        rc=0
        report_write PUT "$payload" "$url" "PUT completion (resent)" || rc=$?
        if [ "$rc" -eq 76 ]; then
            echo "gate-runner: report: PUT completion lost the step race twice - refused in the server's words above, not resent again"
            rc=1
        fi
    fi
    rm -f "$payload"
    return "$rc"
}

# The `code` the step PUT's 409 carries when the row moved between the
# handler's read and its write (car 88123ae0) - the one refusal a resend
# answers (above). The runner matches the CODE, never the `error` words,
# which are a person's and free to move (backlog 2aa2b19e: the words
# were copied here byte for byte and went stale when the compare widened
# to the whole row). Its definition is
# boss_jobs::step_metadata_write::STEP_CHANGED_CODE; boss-testing's
# the_step_race_is_matched_by_its_code holds this line to it by name.
STEP_CHANGED_CODE='step_changed'

# DELETE AFTER ONE RELEASE (the review of car 24eb9471). The words a
# server from before the code answered the same race with, and nothing
# else: while a jobs API that predates the code can still be what this
# runner reports to, those exact words are the race too. Its definition
# is boss_jobs::step_metadata_write::STEP_CHANGED_ERROR_BEFORE_THE_CODE,
# which carries the same deletion note; the same pin holds this line to
# it by name.
STEP_CHANGED_WORDS_BEFORE_THE_CODE='step changed while this write was computed — its metadata is no longer what the write read, so writing it would erase the other write'

# Is this response body the step-race refusal - an object whose `code`
# is STEP_CHANGED_CODE, or (for one release) whose `error` is exactly
# the old words? The words are compared as BYTES: os.fsencode undoes
# whatever the locale did to argv, so the em dash cannot make an exact
# match miss.
report_step_changed() { # response body file
    python3 - "$1" "$STEP_CHANGED_CODE" "$STEP_CHANGED_WORDS_BEFORE_THE_CODE" <<'PY'
import json, os, sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        said = json.load(f)
except Exception:
    sys.exit(1)
if not isinstance(said, dict):
    sys.exit(1)
code, err = said.get("code"), said.get("error")
old = isinstance(err, str) and err.encode("utf-8") == os.fsencode(sys.argv[3])
sys.exit(0 if code == sys.argv[2] or old else 1)
PY
}

# One report write, classified the way every report write is: 0 landed,
# 75 nobody answered (a roll - retry), 76 the step PUT lost the race to
# another write and wrote nothing (resend once), 1 refused (about the
# write itself). A refusal prints the server's words beside its status:
# a bare "HTTP 409" sent the reader of the Job log to the API to find
# out which refusal it was.
report_write() { # method, body file, url, label
    local rc=0 out said
    said=$(mktemp) || return 1
    out=$(curl -s --max-time 20 -o "$said" -w '%{http_code}' -X "$1" \
        -H "x-boss-user: $ACTOR" -H "Content-Type: application/json" \
        -d @"$2" "$3") || rc=$?
    if [ "$rc" -ne 0 ]; then
        rm -f "$said"
        echo "gate-runner: report: $4 failed (curl exit $rc)"
        report_transient_curl "$rc" && return 75
        return 1
    fi
    case "$out" in
        2??) rm -f "$said"; return 0 ;;
        5??|000) rm -f "$said"; echo "gate-runner: report: $4 answered HTTP $out"; return 75 ;;
    esac
    echo "gate-runner: report: $4 answered HTTP $out: $(head -c 2000 "$said" | tr '\n' ' ')"
    rc=1
    if [ "$out" = 409 ] && report_step_changed "$said"; then rc=76; fi
    rm -f "$said"
    return "$rc"
}

report() { # verdict, note
    local attempt=0 rc delay t0=$SECONDS
    REPORT_UNREACHABLE=false
    for delay in $GATE_REPORT_BACKOFF end; do
        attempt=$((attempt + 1))
        REPORT_ATTEMPT=$attempt
        REPORT_WAITED=$((SECONDS - t0))
        rc=0
        report_once "$1" "$2" || rc=$?
        if [ "$rc" -eq 0 ]; then
            echo "gate-runner: verdict $1 recorded on packet $GATE_RUN_JOB_ID (attempt $attempt, $((SECONDS - t0))s)"
            return 0
        fi
        if [ "$rc" -ne 75 ]; then
            echo "gate-runner: report attempt $attempt refused outright - not a roll, not retrying"
            return 1
        fi
        # 75 is "nobody answered", which is the roll. Latch it: the
        # attempt that eventually LANDS is the one carrying the story,
        # and what it has to say is that the SoR was dark for a while -
        # not that it was reachable at the moment of the write.
        REPORT_UNREACHABLE=true
        if [ "$delay" = end ]; then
            echo "gate-runner: report attempt $attempt could not reach the system of record - out of retries after $((SECONDS - t0))s"
            return 1
        fi
        echo "gate-runner: report attempt $attempt could not reach the system of record - retrying in ${delay}s (a deploy rolls it for ~30-90s)"
        sleep "$delay"
    done
}
# --- report-back (end) ---

# The run itself is guarded so ANY failure below still reports `lost`
# with the reason, rather than leaving the packet to go overdue.
fail_lost() { report lost "runner died before a receipt: $1" || true; exit 1; }
trap 'fail_lost "line $LINENO"' ERR

# One job, one branch, one PRIVATE disk. /gate-target is a per-run
# emptyDir now: born empty with the pod, dead with it. The wipe
# discipline that used to live here as three rm -rf lines — stale
# target (disk-filling, manufactured failures), stale clone
# ("destination path already exists"), stale receipt (nearly credited
# one branch with another's pass on 2026-08-25) — is structural: there
# is nothing from a previous run to wipe, and no other run can ever
# see this workspace. That structural isolation is what makes
# CONCURRENT gates safe (packet 28de3845); the 2026-08-24 crossed
# receipts needed a shared disk to happen on.
#
# The receipt now dies with the pod, deliberately. Its surviving
# copies are the packet (the record) and this pod's stdout — the
# `gate-runner: receipt` line below, which `kubectl logs` serves for
# ttlSecondsAfterFinished after the Job ends.
#
# /gate-seed is the one shared surface left: the warm target snapshot
# + the crate cache, on the PVC that used to BE the workspace. Reads
# and writes of it are flock-disciplined below.
SEED=/gate-seed
SEED_LOCK="$SEED/.seed.lock"
mkdir -p /gate-target

# SKEW GUARD, the other direction: under an OLD manifest (no /gate-seed
# mount) this pod's /gate-target is still the shared PVC, which
# persists between runs. For exactly that case the old wipe-per-run
# discipline comes back — without it the clone refuses a non-empty
# destination and cross-branch targets overfill the 120Gi volume (the
# three incidents the old rm lines were written for). /gate-target/cargo
# is deliberately NOT wiped: on the old shape it is the persistent
# crate cache, and wiping it would resurrect the crc32fast class.
if [ ! -d "$SEED" ]; then
    rm -rf /gate-target/target /gate-target/repo
    rm -f /gate-target/receipt.json
fi

# SEED THE TARGET from the warm snapshot. The math this replaces: a
# cold workspace build writes ~74G of target/ and costs 20+ minutes of
# compile (measured; boss-dev.yaml Q1). The seed copy moves the same
# bytes at disk speed — minutes, not tens of minutes — and cargo then
# rebuilds only the workspace crates, which is the ~14-minute warm
# gate this rig is known for. The copy runs under a SHARED flock:
# many seeding readers may overlap freely, but none may overlap the
# refresher rewriting the snapshot (exclusive lock, end of this
# script) — a half-rewritten seed under a reader is how you get
# corrupt rlibs beneath fresh-looking fingerprints, a red that is
# nobody's code. On any failure or a 15-minute lock timeout, fall
# back to a cold build: slow and correct.
#
# THE COPY RUNS BEFORE THE CLONE, and the order is the correctness
# property, not tidiness (backlog 646c0ade). `cp -a` keeps the seed's
# mtimes, and cargo trusts an artifact that is not older than its
# sources — so a seeded artifact NEWER than a cloned file is reused, and
# the run links the seeding run's build of that crate (main's tree or a
# train's, not this branch's): a false green, the gate-side twin of the
# wt-cargo wrong binary (1150c518). The copy used to run after the
# clone, and the order held only by timing: a reader that cloned and
# then waited on the shared lock while a refresh held the exclusive one
# copied the NEW seed, whose newest artifact was one svelte-check older
# than the refresh. Triage run 4814939a read 60 pods on 2026-09-28 and
# found every run clone-newer — by 2.6 s at the worst pairing of the
# measured extremes (svelte-check 14 s at its fastest; clone to lock
# 11.4 s at its slowest). Here, every seeded artifact was written
# before the seed was installed, which is before this copy, which is
# before every file the clone writes: clone-newer by construction,
# whatever any duration does. seed_target reads nothing the clone
# makes. `seed_order_json` below measures the order after the checkout
# and the receipt carries it, so it is read on every gate-run packet
# rather than believed.
#
# THE SEED IS LOOKED FOR INSIDE THE LOCK (backlog ae39a328). The
# refresher deletes $SEED/target under its exclusive lock and renames
# the new one into place before letting go, so a `-d` test made before
# taking the shared lock can land in that window, see no seed, and
# build cold. Triage run c221e11e read 236 gate pods (2026-09-27/28): 2
# went cold, exactly the two whose start fell inside a refresh. Tested
# inside the lock, a reader blocked behind a refresh copies the seed it
# just installed; `.seed-head` is read in the same hold, so the head
# reported is the head copied. The cold cost in the message is that
# triage's measurement, one branch against itself: 11.3 min cold, 7.7
# warm.
mkdir -p /gate-target/target
seed_target() { # <destination target dir>
    local t0=$SECONDS rc=0 head
    # --reflink=auto: on one filesystem (the seed is a local PV on the
    # build node's xfs since 5b3dabb5) this shares extents and writes
    # only metadata; anywhere else it falls back to a plain copy. The
    # timing line below is the measurement either way. Exit 3 is "no
    # seed", distinct from flock's timeout and cp's failure (both 1).
    head=$( ( flock -s -w 900 9 || exit 1
              [ -d "$SEED/target" ] || exit 3
              cp -a --reflink=auto "$SEED/target/." "$1/" || exit 1
              cat "$SEED/.seed-head" 2>/dev/null || echo '<unrecorded>'
            ) 9>>"$SEED_LOCK" ) || rc=$?
    case $rc in
        0) echo "gate-runner: target seeded from head $head in $((SECONDS - t0))s" ;;
        3) echo "gate-runner: no warm seed at $SEED/target — cold build (~4 min extra, measured 2026-09-28)" ;;
        *) echo "gate-runner: seed copy failed or lock timed out after $((SECONDS - t0))s — cold build instead"
           rm -rf "$1"
           mkdir -p "$1" ;;
    esac
}
if [ -d "$SEED" ]; then seed_target /gate-target/target; fi

# THE ORDER, MEASURED (646c0ade). One line of JSON for the receipt: the
# newest seeded artifact's mtime, the oldest cloned source's (.git is
# not a source), the margin in seconds, and `order` — `clone-newer`
# (margin > 0), `SEED-NEWER` (a tie counts: cargo reads an artifact no
# older than its source as fresh), or `cold` (nothing was seeded, so
# nothing can be trusted wrongly). Worst pair against worst pair, so a
# positive margin covers every artifact against every source. awk keeps
# find's own text for the winner: its default print format would round
# an epoch to six digits.
seed_order_json() { # <target dir> <repo dir>
    local newest oldest
    newest=$(find "$1" -type f -printf '%T@\n' 2>/dev/null \
        | awk 'NR == 1 || $1 + 0 > m { m = $1 + 0; s = $1 } END { if (NR) print s }') || newest=""
    oldest=$(find "$2" -path "$2/.git" -prune -o -type f -printf '%T@\n' 2>/dev/null \
        | awk 'NR == 1 || $1 + 0 < m { m = $1 + 0; s = $1 } END { if (NR) print s }') || oldest=""
    python3 - "$newest" "$oldest" <<'PY'
import datetime, json, sys
newest, oldest = sys.argv[1], sys.argv[2]
def iso(t):
    if not t:
        return None
    return datetime.datetime.fromtimestamp(float(t), datetime.timezone.utc).isoformat(timespec="milliseconds")
out = {"seed_newest_artifact": iso(newest), "clone_oldest_source": iso(oldest)}
if newest and oldest:
    margin = float(oldest) - float(newest)
    out["margin_s"] = round(margin, 3)
    out["order"] = "clone-newer" if margin > 0 else "SEED-NEWER"
else:
    out["margin_s"] = None
    out["order"] = "cold" if not newest else "no-sources"
print(json.dumps(out, separators=(",", ":")))
PY
}

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
# The checkout is the last write to the sources and nothing has built
# yet, so this is the order cargo will judge. It rides the receipt as
# `seed_order` (the summary block below); a failed measurement is an
# absent field, never a failed gate.
t_order=$SECONDS
SEED_ORDER=$(seed_order_json /gate-target/target /gate-target/repo) || SEED_ORDER=""
echo "gate-runner: seed order ${SEED_ORDER:-<unmeasured>} (measured in $((SECONDS - t_order))s)"

export CARGO_TARGET_DIR=/gate-target/target
# THE CRATE CACHE SURVIVES THE RUN, and it is a correctness fix before
# it is a speed one. CARGO_HOME was unset once, so it defaulted inside
# the container and died with the pod — meaning every gate re-downloaded
# every dependency from static.crates.io, and every gate was therefore
# betting its verdict on several hundred consecutive successful fetches
# over a link that is measurably not that reliable.
#
# That bet lost on 2026-08-27 05:51Z: `clippy` was recorded as FAILED on
# fix/a-dropped-lookup-does-not-red-a-train when the real log said
#   error: failed to download from `https://static.crates.io/.../crc32fast`
#   Caused by: [7] Could not connect to server
# Every other check on that branch passed. A green branch was called red
# by the network, which is the same class of fault as the kaniko DNS
# break that cancelled a six-car train — and it is the likeliest
# explanation for the unexplainable clippy/fixture reds in backlog
# 9c7ed804, none of which reproduced by hand.
#
# So CARGO_HOME lives on the seed volume, which outlives every pod.
# Concurrent gates share it SAFELY without our lock: cargo has locked
# its package cache against concurrent processes since forever — this
# is the same arrangement as N developer builds sharing one ~/.cargo.
# The registry cache is content-addressed by version + checksum, so a
# stale entry cannot produce a wrong build — only a faster one.
#
# The [ -d ] probe is a SKEW guard: a session gating from a stale
# checkout renders the old manifest, which mounts no /gate-seed. That
# run must cost speed, not the gate — cold and loud beats dead.
if [ -d "$SEED" ]; then
    export CARGO_HOME="$SEED/cargo"
else
    echo "gate-runner: /gate-seed is not mounted (manifest older than this script?) — running cold"
    export CARGO_HOME=/gate-target/cargo
fi
mkdir -p "$CARGO_HOME"

# Build parallelism follows the CPU the container was actually GIVEN.
# It was pinned at 4, so raising the gate ceiling from 6 CPU to 20 in
# the build-node car bought nothing measurable: the gate was never
# bound at 20, it was bound at 4, and sixteen cores sat idle. A 20-CPU
# run on w-1 took 47 minutes against 40 on a 6-CPU control plane,
# which is what sent me looking.
#
# Read the CGROUP QUOTA, not nproc: inside a container nproc reports
# the node's core count (32 on w-1), not the slice the container may
# use, so it would oversubscribe by 60% here.
gate_cpus() {
    local q p
    if [ -r /sys/fs/cgroup/cpu.max ]; then
        read -r q p < /sys/fs/cgroup/cpu.max
        if [ "$q" != "max" ] && [ -n "$p" ] && [ "$p" -gt 0 ] 2>/dev/null; then
            echo $(( (q + p - 1) / p ))
            return
        fi
    fi
    nproc 2>/dev/null || echo 4
}
CPUS=$(gate_cpus)
[ "${CPUS:-0}" -ge 1 ] 2>/dev/null || CPUS=4

# RUST_TEST_THREADS stays at 2 ON PURPOSE. The suites share one
# postgres sidecar, and parallel DB-backed tests are what produced the
# /dev/shm exhaustion and the schema-load race this rig has already
# been bitten by. Raising both at once would also make the result
# unattributable. Widen it as a separate, measured change.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$CPUS}" RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
echo "gate-runner: building ${CARGO_BUILD_JOBS}-wide (cgroup quota), tests 2-wide"

# THIS IS A LOADED GATE, SAID TO THE WEB SUITES (backlog ebb750cd). On
# 2026-09-28 two trains went red in the web suite on tests that pass in
# well under a second on the dev pod — a mocked paint at 27.6 s against
# a 15 s expect, a unit file at 30.2 s against 30 s — while three gates
# each built cargo 20-wide on w-1's one NVMe. Their budgets are stated
# for a quiet run; under this, apps/web/src/dev-load.ts scales every one
# by its GATE_LOAD_SCALE. Only this runner sets it — a developer's run,
# wt-web and forge CI stay on the quiet budgets — and the name is pinned
# to the web tree's by the_gate_states_its_web_load.rs. No retry: what
# each test took goes on the receipt as `web_timings` (infra/gate.sh).
export BOSS_WEB_GATE_LOAD=1
echo "gate-runner: web budgets scaled for gate load (BOSS_WEB_GATE_LOAD=1; the factor is GATE_LOAD_SCALE in apps/web/src/dev-load.ts)"

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
# THE VERDICT COMES FROM THE RECEIPT, not from a second reading of the
# exit status (backlog c67bdbae). gate.sh already decided and wrote it
# down; deriving it again here made the fact live twice, and the two
# copies disagreed IN THE SAME WRITE — gate-run d14b0768 uploaded a
# receipt saying `refused` while reporting `failed` beside it, so a
# branch took a strike because the system of record was mid-roll and a
# live-reading lint declined to guess.
#
# AN INFRASTRUCTURE REFUSAL IS NOT A CONSIST FAILURE (CLAUDE.md
# §Diagnosis). gate.sh exits 2 for one and says in its own output that
# the run "judged nothing about the branch". The if above stays as the
# FALLBACK — a receipt that does not exist or will not parse leaves the
# exit-status reading in place, which is the conservative answer, and
# `failed` is the right conservative answer for a run whose record is
# unreadable.
#
# Only the words the protocol's `verdict` enum declares are accepted
# here. A receipt naming anything else is ignored rather than reported:
# an unknown value would be refused by the field validator, and a
# refused report is a `lost` gate-run nobody can read.
if [ -s "$RECEIPT" ]; then
    from_receipt=$(jq -r '.verdict // empty' "$RECEIPT" 2>/dev/null || true)
    case "$from_receipt" in
        green|failed|lost|refused) VERDICT="$from_receipt" ;;
        *) : ;;
    esac
fi
trap - ERR

# --- failure detail (begin) ---
# WHAT FAILED, IN THE RECORD THAT OUTLIVES THE POD.
#
# The 2026-09-09 work made the receipt name the failing CHECK: 62 entries
# with a result and a duration each, which is what turned one diagnosis
# into a six-minute read. It stopped one level short. Gate-run 9dc722d2
# recorded verdict=failed with `test` correctly marked the single failure
# and nothing whatsoever about WHICH test - and the check name is not
# what a reader acts on. The failing test name was one grep away in this
# pod's log, and the pod log is REAPED: the receipt is the durable record
# (backlog 4a4d1227).
#
# So the sections this script already pulls out of gate.log for the
# stdout replay are also parsed for the part that has to survive - the
# failing test names, and the panic line with its file:line - and merged
# into the receipt as `fails` before it is reported.
#
# ONE PARSER. The replay and `fails` answer the same question ("what did
# the failed checks say"), so gate.log is read once and the replay text
# is written to a file the block below merely prints (CLAUDE.md §9a: a
# fact that would otherwise live twice). That also moves the old
# `|| tail -200` shell fallback INSIDE the parser, where it can say why
# it fired instead of firing silently.
#
# `fails` IS ALWAYS PRESENT, `[]` when nothing failed. The field was
# absent from every receipt gate.sh writes - so a reader saw `null`,
# while the pre-2026-09-09 four-field digests sitting beside it on older
# packets carried `[]`. "Nothing failed" and "nobody wrote the field"
# must not look the same.
#
# `fails_excerpt` IS THE REPLAY, ON THE RECORD (backlog 5708cbd5). Train
# #361's red gate (2026-09-14) took this one level further down: `fails`
# named the test and said "no panic line for it in this check's output",
# and the assertion text was in the replay below - which only `kubectl
# logs` serves, and the forge, the yard and `boss orient` cannot run it.
# So the SAME lines the replay prints for each failed check are written
# to the receipt as `fails_excerpt: {"<check>": "..."}`, bounded, so the
# gate-run packet's record-verdict step carries WHY and the red-train
# alert can attach it the way it attaches a forge check's log. It is
# `{}` when nothing failed, for the reason `fails` is `[]`.
#
# EVERY CAP BELOW IS DELIBERATE AND STATES ITSELF. A suite failing 200
# tests must not write a receipt nobody can read; a cap that silently
# drops the remainder is the 778 KB-log-tailed-to-16 KB defect wearing a
# different hat, so each one carries the count of what it left out. The
# excerpt's own caps are sized to the transport: the receipt rides the
# step's metadata as one JSON string, and `report_once` hands it to
# python as ONE argv string - 128 KB per argument on Linux - so ~24 KB of
# excerpt in all keeps the whole receipt well inside that with the rest
# of gate.sh's account beside it.
python3 - "$RECEIPT" /gate-target/gate.log /gate-target/failed-checks.txt <<'PY' || echo "gate-runner: failure-detail extractor crashed - the receipt keeps whatever gate.sh wrote"
import json, os, re, sys
from collections import deque

receipt_path, log_path, replay_path = sys.argv[1], sys.argv[2], sys.argv[3]

REPLAY_TAIL = 300      # lines per failed check replayed to the pod log
# Lines per failed check this parser reads. It was 2 000 while cargo
# stopped at the first failing binary, which put every failure at the
# end of the check. Since `--no-fail-fast` (backlog 3bef4198) an early
# binary's failure is followed by every later binary passing - tens of
# thousands of lines on a full gate - so the whole check is read; this
# bound only stops a runaway log from taking the pod's memory.
PARSE_TAIL = 400000
REPLAY_BLOCKS = 600    # lines of earlier failure blocks replayed ahead of the tail
RAW_TAIL = 200         # lines of gate.log when nothing can be parsed
PER_CHECK = 5          # named failures per check on the receipt
TOTAL_ENTRIES = 40     # entries on the whole receipt
ENTRY_CHARS = 400      # characters per entry
MESSAGE_LINES = 12     # lines of one panic message kept, before the entry's own clip
QUOTE_LINES = 3        # raw lines quoted for a check this cannot parse
EXCERPT_CHARS = 6000   # characters of `fails_excerpt` per failed check
EXCERPT_TOTAL = 24000  # characters of `fails_excerpt` on the whole receipt
EXCERPT_FLOOR = 400    # below this much budget left, a check's excerpt is omitted, stated
EXCERPT_CONTEXT = 40   # lines kept above the first failure marker

try:
    with open(log_path, errors="replace") as fh:
        LOG = [line.rstrip("\n") for line in fh]
except OSError:
    LOG = []


def raw_tail():
    """gate.log's own last words, for the cases nothing can be parsed."""
    if not LOG:
        return ["(gate.log is missing or empty - there is nothing to fall back to)"]
    return ["last %d of %d line(s) of gate.log:" % (min(RAW_TAIL, len(LOG)), len(LOG))] \
        + LOG[-RAW_TAIL:]


def sections(names):
    """The ::group:: block each named check wrote.

    gate.sh brackets every check with `::group::gate: <name>` /
    `::endgroup::`, so the failed sections can be lifted exactly and
    nothing else - a full gate.log is mostly successful build chatter.
    Returns name -> (last PARSE_TAIL lines, TRUE line count), because a
    reduction that cannot say what it dropped is the defect this whole
    block is about. An UNTERMINATED group is kept: that is a timeout, an
    OOM kill, a node reset mid-gate - one of the cases most worth
    explaining.
    """
    wanted = {"::group::gate: %s" % n: n for n in names}
    out, cur, buf, seen = {}, None, None, 0
    for line in LOG:
        if cur is None:
            name = wanted.get(line)
            if name is not None:
                cur, buf, seen = name, deque(maxlen=PARSE_TAIL), 0
        elif line == "::endgroup::":
            out[cur] = (list(buf), seen)
            cur, buf = None, None
        else:
            buf.append(line)
            seen += 1
    if cur is not None:
        out[cur] = (list(buf), seen)
    return out


RE_FAILED = re.compile(r"^test (\S+) \.\.\. FAILED")
RE_STDOUT = re.compile(r"^---- (\S+) stdout ----")
RE_LISTED = re.compile(r"^ {4}(\S+)$")
# `(<tid>)` between the thread's name and `panicked` is optional: rustc
# began printing the thread id there, and the pattern that did not allow
# it made gate-run a1664c7e report "no panic line for it in this check's
# output" over a panic sitting in the same receipt's `fails_excerpt`
# (backlog 2dc742c1, 2026-09-22). Both formats carry it.
RE_PANIC_OLD = re.compile(r"^thread '([^']*)'(?: \(\d+\))? panicked at '(.*)', (\S+)$")
RE_PANIC_NEW = re.compile(r"^thread '([^']*)'(?: \(\d+\))? panicked at (\S+):$")
# Where a panic's message stops: cargo's hint, a captured backtrace, or
# the `failures:` roll-call that follows the last block.
RE_MESSAGE_END = re.compile(r"^(note: run with `RUST_BACKTRACE|stack backtrace:$|failures:$)")
RE_ERROR = re.compile(r"^\s*(error(\[E\d{4}\])?|Error|ERROR)\b[: ]")
RE_ARROW = re.compile(r"^\s*--> (\S+)")


def failing_tests(body):
    """Every test cargo said failed, in the order it said so.

    Three statements of the same fact, all real: the per-test
    `test X ... FAILED` line, the `---- X stdout ----` header above each
    panic, and the `failures:` list cargo prints at the end. The list is
    the only one certain to be within PARSE_TAIL of a long suite, so all
    three are read and deduped.
    """
    names, seen, in_list = [], set(), False

    def add(n):
        if n not in seen:
            seen.add(n)
            names.append(n)

    for line in body:
        if line.strip() == "failures:":
            in_list = True
            continue
        m = RE_FAILED.match(line) or RE_STDOUT.match(line)
        if m:
            add(m.group(1))
            in_list = False
            continue
        if in_list:
            m = RE_LISTED.match(line)
            if m:
                add(m.group(1))
            elif line.strip():
                in_list = False
    return names


def panics(body):
    """(test or None, location, message) for each panic cargo printed.

    Attributed to the `---- <test> stdout ----` block it appeared in,
    falling back to the panicking thread's name - which for a plain
    `#[test]` IS the test name. Both cargo panic formats are read: the
    current two-line one (location, then the message) and the older
    single-line `panicked at 'msg', location`.
    """
    out, block = [], None
    for i, line in enumerate(body):
        m = RE_STDOUT.match(line)
        if m:
            block = m.group(1)
            continue
        m = RE_PANIC_OLD.match(line)
        if m:
            out.append((block or m.group(1) or None, m.group(3), m.group(2)))
            continue
        m = RE_PANIC_NEW.match(line)
        if m:
            out.append((block or m.group(1) or None, m.group(2), message_after(body, i)))
    return out


def message_after(body, i):
    """A current-format panic's message: EVERY line after the header up
    to cargo's own `note:` (or a backtrace, the next block, the roll-call),
    not the first one. Gate-run 2510ac65 (2026-09-24, backlog b53dca8c)
    recorded an assertion's header - "... and that relaunching is safe:"
    - and dropped the lines it introduced, which held the cause ("refused
    every connection for 0s"); only the excerpt had them. Bounded to
    MESSAGE_LINES, saying what it left; `clip` bounds the entry itself."""
    lines = []
    for nxt in body[i + 1:]:
        if RE_MESSAGE_END.match(nxt) or RE_STDOUT.match(nxt) \
                or RE_PANIC_OLD.match(nxt) or RE_PANIC_NEW.match(nxt):
            break
        if nxt.strip():
            lines.append(nxt.strip())
    if len(lines) > MESSAGE_LINES:
        lines = lines[:MESSAGE_LINES] + ["(+%d more message line(s); the excerpt has them)" % (
            len(lines) - MESSAGE_LINES)]
    return " ".join(lines)


# Playwright's own verdict lines, in the order a reader acts on them:
# the per-spec `✘` marker (the spec's name), the run's `N failed`
# roll-up, its `Error:` line, and the Expected/Received diff - whose
# `+`/`-` lines hold the received value (the 3 s click timeout the
# operator had to pull the Job log for).
RE_SPEC_FAIL = re.compile(r"^\s*✘\s")
RE_SPEC_SUMMARY = re.compile(r"^\s*\d+ failed\b")
RE_SPEC_DIFF_HEAD = re.compile(r"^\s*[-+] (Expected|Received)\b")
RE_SPEC_DIFF_LINE = re.compile(r"^\s*[-+] ")
RE_SPEC_DIFF_END = re.compile(r"^\s*>?\s*\d+ \||^\s*\d+\) ")   # the code frame, or the next failure
DIFF_LINES = 8         # `+`/`-` lines of one Expected/Received diff kept in its entry
DIFF_SCAN = 60         # lines read past a diff header before giving up on its end


def error_lines(body):
    """Error lines with their `-->` location, for checks that are not
    cargo-test-shaped: a compile error, clippy, svelte-check. This is as
    far as the evidence goes - no test name is invented from them."""
    out = []
    for i, line in enumerate(body):
        if RE_ERROR.match(line):
            where = ""
            for nxt in body[i + 1:i + 3]:
                m = RE_ARROW.match(nxt)
                if m:
                    where = " (%s)" % m.group(1)
                    break
            out.append(line.strip() + where)
    return out


# A `✘` is printed for every test whose status is not `passed` - and
# that includes a `test.fail()` spec that failed exactly as told, which
# Playwright counts PASSED. The `✘` line alone cannot say which it was
# (the list reporter only colours the two differently, and the gate's
# log has no colour). Red gate-run 5b5a04d8 (2026-09-28, backlog
# 42981848) quoted four such specs as its first four verdict lines and
# cut one of the two that really timed out. Playwright's own roll-up
# CAN say: its `N failed` block lists exactly the tests whose outcome
# was unexpected, in the same `[project] › file:line:col › title` the
# `✘` line carries before its `(retry #n)` and `(duration)`. So a `✘` is
# quoted as a verdict when the roll-up counts its test failed, and the
# ones it does not are counted and said - never dropped in silence.
RE_SPEC_CROSS = re.compile(r"^\s*✘\s+(?:\d+\s+)?(.*)$")
RE_SPEC_LOCATION = re.compile(r"(\S+:\d+:\d+)")


def failed_by_rollup(body):
    """The test titles Playwright's `N failed` roll-up lists (each line
    indented under it), or `None` when the output holds no roll-up that
    names any - a run killed before Playwright's epilogue."""
    titles = []
    for i, line in enumerate(body):
        if not RE_SPEC_SUMMARY.match(line):
            continue
        indent = len(line) - len(line.lstrip())
        for nxt in body[i + 1:]:
            if not nxt.strip() or len(nxt) - len(nxt.lstrip()) <= indent:
                break
            titles.append(nxt.strip())
    return titles or None


def counted_failed(cross, titles):
    """Is this `✘` line's test one the roll-up counts failed?"""
    m = RE_SPEC_CROSS.match(cross)
    rest = m.group(1).strip() if m else cross.strip()
    return any(rest == t or rest.startswith(t + " (") for t in titles)


def unexpected_crosses(crosses, body):
    """(the `✘` lines to quote as verdicts, a note saying what the choice
    rests on - or None when every `✘` was a failure and the roll-up says
    so)."""
    if not crosses:
        return crosses, None
    titles = failed_by_rollup(body)
    if titles is None:
        return crosses, ("no Playwright roll-up naming the failed specs in this output (the "
                         "run ended before its summary), so an expected failure (test.fail) "
                         "cannot be told from a real one - every ✘ line is quoted")
    unmatched = [t for t in titles if not any(counted_failed(c, [t]) for c in crosses)]
    if unmatched:
        return crosses, ("Playwright's roll-up names %d failed spec(s) no ✘ line matches (first: "
                         "%s), so the ✘ lines cannot be sorted by it - every ✘ line is quoted"
                         % (len(unmatched), unmatched[0]))
    kept = [c for c in crosses if counted_failed(c, titles)]
    if len(kept) == len(crosses):
        return kept, None
    return kept, ("%d ✘ line(s) not quoted: Playwright's roll-up counts %d spec(s) failed and "
                  "these are not among them - an expected failure (test.fail, counted "
                  "passed) or a flaky spec's failed attempt (counted flaky)" % (
                      len(crosses) - len(kept), len(titles)))


# A TIMEOUT'S CAUSE, BESIDE ITS `✘` (backlog a766e20d). Red gate-run
# 5b5a04d8 (2026-09-28) failed on two specs, each `(4.0m)`, and the line
# that said why - `Test timeout of 240000ms exceeded while setting up
# "page".`, under each one's numbered `N) [project] › file:line:col ›
# title` block - matched no verdict pattern, so `fails` never quoted it,
# on the kind of red that made 8 same-head reds in 7 days. The same run
# quoted each block's `Error Context: test-results/…` line as an error:
# RE_ERROR takes `Error` and a space, and that line is the PATH of the
# page snapshot gate.sh keeps (see `context`), not a verdict. Only the
# Playwright reading refuses it; cargo and svelte still need the space.
RE_SPEC_NUMBERED = re.compile(r"^\s*\d+\) (\S.*?)[\s─]*$")
RE_SPEC_REASON = re.compile(r"^\s*(Test timeout of \d+ms exceeded\b.*?)\s*$")
RE_SPEC_CONTEXT_PATH = re.compile(r"^\s*Error Context: ")


def spec_reasons(body):
    """{numbered block's title: the timeout line under it}, first one
    per title (a retried test prints the same reason per attempt)."""
    out, title = {}, None
    for line in body:
        m = RE_SPEC_NUMBERED.match(line)
        if m:
            title = m.group(1)
            continue
        m = RE_SPEC_REASON.match(line)
        if m and title is not None:
            out.setdefault(title, m.group(1))
    return out


def with_reason(cross, reasons):
    """The `✘` line, and the reason its numbered block gives, if any."""
    for title, why in reasons.items():
        if counted_failed(cross, [title]):
            return "%s - %s" % (cross, why)
    return cross


def spec_verdicts(body):
    """Playwright's verdict lines, ranked - or `[]` for a check that is
    not Playwright-shaped (no `✘` marker and no `N failed` roll-up), so
    an `Error:` line from svelte-check is never called a failing spec -
    and a note, or None, saying which `✘` lines were set aside and why
    (see `unexpected_crosses`).
    Each Expected/Received diff is ONE entry: its two headers and up to
    DIFF_LINES of its `+`/`-` lines, saying how many more there were.
    (Backlog 3a6f61d6, 2026-09-18: until this ranking existed the alert
    quoted five of the mocked runner's connect-noise lines over the
    verdict beside them; the noise itself was deleted at its source on
    2026-09-19, 82b87a09, so nothing here filters it any more.)"""
    fails, summary, errors, diffs = [], [], [], []
    i = 0
    while i < len(body):
        line = body[i]
        if RE_SPEC_FAIL.match(line):
            fails.append(line.strip())
        elif RE_SPEC_SUMMARY.match(line):
            summary.append(line.strip())
        elif RE_SPEC_DIFF_HEAD.match(line):
            head, rows, scanned = [], [], 0
            while i < len(body) and RE_SPEC_DIFF_HEAD.match(body[i]):
                head.append(body[i].strip())
                i += 1
            # The diff's rows sit among unmarked context lines (`Array [`,
            # `]`) and end at the code frame or the next numbered failure.
            while i < len(body) and scanned < DIFF_SCAN \
                    and not RE_SPEC_DIFF_END.match(body[i]) \
                    and not RE_SPEC_FAIL.match(body[i]) \
                    and not RE_SPEC_SUMMARY.match(body[i]):
                if RE_SPEC_DIFF_LINE.match(body[i]):
                    rows.append(body[i].strip())
                i += 1
                scanned += 1
            kept = rows[:DIFF_LINES]
            if len(rows) > DIFF_LINES:
                kept.append("(+%d more diff line(s); the excerpt has them)" % (
                    len(rows) - DIFF_LINES))
            diffs.append(" / ".join(head + kept))
            continue
        elif RE_SPEC_CONTEXT_PATH.match(line):
            pass
        elif RE_ERROR.match(line) and line.lstrip().startswith("Error"):
            errors.append(line.strip())
        i += 1
    if not fails and not summary:
        return [], None
    fails, note = unexpected_crosses(fails, body)
    reasons = spec_reasons(body)
    fails = [with_reason(c, reasons) for c in fails]
    return fails + summary + errors + diffs, note


# bun's unit-test verdicts (backlog 4d928d0a). The 00:01Z train of
# 2026-09-28 was disassembled on web-suite and `fails` said only
# `error: script "test:unit" exited with code 1`, while the excerpt
# beside it named the file and the test that timed out. The web unit
# suites run one file per `bun test` process
# (apps/web/scripts/each-test-file-alone.ts), which prints a failing
# file's whole output under `===== <file> — exit N, run alone =====`.
# bun 1.3 prints an assertion's or a throw's `error:` line (and its `at`
# location) ABOVE the `(fail) <test> [ms]` line, and a timeout's reason
# (`^ this test timed out after …`) BELOW it.
RE_BUN_ALONE = re.compile(r"^===== (\S+) — exit (\d+), run alone =====$")
RE_BUN_FILE = re.compile(r"^(\S+[._](?:test|spec)\.[cm]?[jt]sx?):$")
RE_BUN_OUTCOME = re.compile(r"^\((pass|fail|skip|todo)\) (.*)$")
RE_BUN_FAIL = re.compile(r"^\(fail\) ")
RE_BUN_ERROR = re.compile(r"^(error: .+|[A-Z]\w*(?:Error|Exception)\b.*)$")
RE_BUN_AT = re.compile(r"^\s+at .*?\(?(\S+:\d+:\d+)\)?$")
RE_BUN_FRAME = re.compile(r"^\s*\d+ \|")
RE_BUN_REASON = re.compile(r"^\s+\^ (this test .+)$")


def bun_reason(body, since, i):
    """Why the `(fail)` at `i` failed: the timeout line below it, or the
    first error line bun printed since the previous outcome (with the
    lines that continue it, up to its `at` location, and the location)."""
    if i + 1 < len(body):
        m = RE_BUN_REASON.match(body[i + 1])
        if m:
            return m.group(1)
    return bun_error(body[since:i]) or "no error line above it in this check's output"


def bun_error(lines):
    """The first error bun printed in `lines`: its line, the non-blank
    lines continuing it up to its `at` location (MESSAGE_LINES at most,
    saying what it left), and that location."""
    start = next((k for k, l in enumerate(lines) if RE_BUN_ERROR.match(l)), None)
    if start is None:
        return ""
    msg, where = [lines[start].strip()], ""
    for nxt in lines[start + 1:]:
        m = RE_BUN_AT.match(nxt)
        if m:
            where = " (at %s)" % m.group(1)
            break
        if RE_BUN_FRAME.match(nxt) or RE_BUN_OUTCOME.match(nxt) or nxt.startswith("-----"):
            break
        if nxt.strip():
            msg.append(nxt.strip())
    if len(msg) > MESSAGE_LINES:
        msg = msg[:MESSAGE_LINES] + ["(+%d more message line(s); the excerpt has them)" % (
            len(msg) - MESSAGE_LINES)]
    return " ".join(msg) + where


def bun_failures(body):
    """(file, test or None, why) for each unit test bun said failed, in
    order - and for each file the per-file runner said failed with NO
    `(fail)` line (an import that threw, an unhandled error between
    tests), one entry naming the file and its first error."""
    out, file_, since = [], None, 0
    alone = []      # (file, exit code, header index)
    failed_in = set()
    for i, line in enumerate(body):
        m = RE_BUN_ALONE.match(line)
        if m:
            file_, since = m.group(1), i + 1
            alone.append((file_, m.group(2), i))
            continue
        m = RE_BUN_FILE.match(line)
        if m:
            if file_ is None or not file_.endswith(m.group(1)):
                file_ = m.group(1)
            since = i + 1
            continue
        m = RE_BUN_OUTCOME.match(line)
        if m:
            if m.group(1) == "fail":
                out.append((file_, m.group(2), bun_reason(body, since, i)))
                failed_in.add(file_)
            since = i + 1
    for n, (name, code, at) in enumerate(alone):
        if name in failed_in:
            continue
        end = alone[n + 1][2] if n + 1 < len(alone) else len(body)
        why = bun_error(body[at + 1:end]) or "no error line in its output"
        out.append((name, None, "exit %s, run alone, with no (fail) line: %s" % (code, why)))
    return out


RE_UNREACHABLE = re.compile(
    r"Unable to connect|Could not resolve host|Temporary failure in name resolution|"
    r"failed to lookup address|Connection timed out|Network is unreachable|"
    r"ConnectionRefused|ECONNREFUSED|ETIMEDOUT|EAI_AGAIN|"
    r"Couldn't connect to server|Could not connect to|error sending request for url")
# Two ways to say "I could not judge the tree" that must NOT be
# mistaken for "the tree is bad". A local backend refused (127.0.0.1,
# localhost) is the CAR's failure — a mocked suite reaching for a
# service it was never given — and stays a red (478347ad, corrected
# 2026-09-11 after exactly that misreading).
RE_LOCAL = re.compile(r"127\.0\.0\.1|localhost|\[::1\]")


def network_refusal(name, body):
    """`None`, or the reason a failed check is a REFUSAL rather than a
    verdict: its failure lines are connect/resolve errors against
    somewhere off this machine, and NOTHING judged the tree — no failing
    test, no panic, no compile error line that is not itself about the
    network. Gate-run 7522c115 (2026-09-11): 232 bun "Unable to connect"
    lines against registry.npmjs.org, verdict=failed against a branch
    that was never judged; the registry answered two minutes later.
    The receipt's own `fails` said "no cargo test failure" over the
    connect errors — the diagnosis was on the record and nothing acted.
    """
    hits = [l for l in body if RE_UNREACHABLE.search(l) and not RE_LOCAL.search(l)]
    if len(hits) < 3:
        return None
    if failing_tests(body) or panics(body) or spec_verdicts(body)[0] or bun_failures(body):
        return None
    judged = [e for e in error_lines(body) if not RE_UNREACHABLE.search(e)
              and not re.search(r"InstallFailed|install failed|network", e)]
    if judged:
        return None
    return ("network unreachable during %s: %d connect/resolve error line(s) and no test, "
            "panic or compile failure - the run could not judge the tree; re-gate when the "
            "network answers (first: %s)" % (name, len(hits), hits[0].strip()[:160]))


def clip(entry):
    """One entry, one line, bounded - and saying by how much."""
    entry = " ".join(entry.split())
    if len(entry) <= ENTRY_CHARS:
        return entry
    return entry[:ENTRY_CHARS] + "... (+%d char(s); the Job log replay has the full text)" % (
        len(entry) - ENTRY_CHARS)


RE_RESULT_LINE = re.compile(r"^test result: ")


def failure_blocks(body, before):
    """libtest's failure block for each binary that failed, among
    `body[:before]`: from its first `failures:` line through its
    `test result:` line, and the `error: test failed, to rerun pass
    `-p <crate> --test <binary>`` line after it, which names the binary.

    With `--no-fail-fast` (backlog 3bef4198) these are what a replay of
    the tail alone would lose: every later binary's passing output comes
    after them."""
    out, start = [], None
    for i, line in enumerate(body[:before]):
        if start is None and line.strip() == "failures:":
            start = i
        elif start is not None and RE_RESULT_LINE.match(line):
            end = i + 1
            while end < min(len(body), i + 3) and (not body[end].strip()
                                                   or body[end].startswith("error: test failed")):
                end += 1
            out.append((start, end))
            start = None
    return out


def selection(body, total):
    """What the replay prints for one check (and so what its excerpt is
    cut from): the last REPLAY_TAIL lines, preceded by every failure
    block that sits above them, bounded, with each omission stated.
    Returns (header, lines)."""
    tail_from = max(len(body) - REPLAY_TAIL, 0)
    tail = body[tail_from:]
    blocks = failure_blocks(body, tail_from)
    if not blocks:
        return "last %d of %d line(s):" % (len(tail), total), tail
    early = []
    for start, end in blocks:
        early += body[start:end]
    dropped = 0
    if len(early) > REPLAY_BLOCKS:
        dropped = len(early) - REPLAY_BLOCKS
        early = early[:REPLAY_BLOCKS]
    lines = early
    if dropped:
        lines.append("... (+%d line(s) of earlier failure blocks omitted - this replay keeps %d)"
                     % (dropped, REPLAY_BLOCKS))
    gap = tail_from - blocks[-1][1]
    lines.append("... (%d line(s) between the last failure block and the check's last %d "
                 "omitted - no failure block in them)" % (max(gap, 0), len(tail)))
    header = ("%d failure block(s) from earlier in the check, then its last %d of %d line(s):"
              % (len(blocks), len(tail), total))
    return header, lines + tail


RE_MARK = (RE_STDOUT, RE_PANIC_OLD, RE_PANIC_NEW, RE_ERROR, RE_SPEC_FAIL, RE_BUN_ALONE,
           RE_BUN_FAIL)


def marks_a_failure(line):
    """Does this line MARK a failure, for the excerpt's window?"""
    return any(r.match(line) for r in RE_MARK)


def excerpt(lines, budget):
    """One failed check's `fails_excerpt`: the replay's own lines,
    bounded to the smaller of EXCERPT_CHARS and what is left of
    EXCERPT_TOTAL - and saying what the bound removed.

    The selection is the replay's (`lines` IS what it prints), not a
    new one. The bound spends its budget where a reader acts: from
    EXCERPT_CONTEXT lines above the first line that MARKS the failure -
    a `---- <test> stdout ----` header, a panic, an error line - keeping
    the head from there, because cargo prints the panic first and the
    `failures:` roll-call last. With no marker the tail is kept: a check
    that said nothing this parser recognises is best explained by its
    last words.
    """
    cap = min(EXCERPT_CHARS, budget)
    text = "\n".join(lines)
    if len(text) <= cap:
        return text
    mark = next((i for i, l in enumerate(lines) if marks_a_failure(l)), None)
    budget_note = "this check's excerpt budget was %d char(s): %d per check, %d in all" % (
        cap, EXCERPT_CHARS, EXCERPT_TOTAL)
    if mark is None:
        kept = text[-cap:]
        return "... (%d char(s) before this omitted - %s; the Job log replay has them)\n%s" % (
            len(text) - cap, budget_note, kept)

    def from_line(start):
        note = ""
        if start:
            note = "... (%d line(s) before the first failure marker omitted; the Job log " \
                   "replay has them)\n" % start
        return note, "\n".join(lines[start:])

    note, text = from_line(max(mark - EXCERPT_CONTEXT, 0))
    if len(note) + len(text) <= cap:
        return note + text
    # TIGHT: the budget goes to the marker and the check's last words,
    # and the context above the marker is the first thing to go
    # (4077889a). For a web-suite red the marker is the per-spec `✘`
    # deep in Playwright's list, the 40 lines above it are passing
    # specs, and the `Error:`, the Expected/Received diff and the
    # `N failed` roll-up are at the END - a head-only cut from 40 lines
    # above held the chatter and lost the verdict. Cargo reads the same
    # way: the panic block at the marker, the `failures:` roll-call last.
    note, text = from_line(mark)
    if len(note) + len(text) <= cap:
        return note + text
    room = max(cap - len(note), 0)
    mid = "\n... (%%d char(s) omitted between the first failure marker and the check's last " \
          "words - %s; the Job log replay has the full text)\n" % budget_note
    share = room - len(mid % 0)
    if share < 2 * EXCERPT_FLOOR:
        return note + text[:room] + "\n... (+%d char(s) omitted - %s; the Job log replay " \
               "has the full text)" % (len(text) - room, budget_note)
    head = share // 2
    tail = share - head
    return note + text[:head] + mid % (len(text) - head - tail) + text[-tail:]


def detail(name, got):
    """What `fails` says about one failed check.

    A precedence ladder, and every rung names which one it is standing
    on, so a reader never has to guess whether a line was parsed or
    merely quoted.
    """
    if got is None:
        return ["%s: no ::group:: block in gate.log - it failed before it ran, or gate.sh "
                "changed its grouping and this extractor needs updating" % name]
    body, total = got
    if not body:
        return ["%s: the check produced no output at all" % name]

    out = []
    tests = failing_tests(body)
    found = panics(body)
    where = {}
    for test, loc, msg in found:
        if test is not None and test not in where:
            where[test] = (loc, msg)

    if tests:
        for test in tests[:PER_CHECK]:
            loc, msg = where.get(test, (None, None))
            if loc is None:
                out.append("%s: %s - FAILED, with no panic line for it in this check's "
                           "output" % (name, test))
            else:
                out.append("%s: %s - panicked at %s: %s" % (name, test, loc, msg))
        if len(tests) > PER_CHECK:
            out.append("%s: + %d more failing test(s) not named here (%d failed in all) - the "
                       "Job log replay lists them" % (name, len(tests) - PER_CHECK, len(tests)))
        return out

    unit = bun_failures(body)
    if unit:
        for file_, test, why in unit[:PER_CHECK]:
            if test is None:
                out.append("%s: %s - %s" % (name, file_, why))
            else:
                out.append("%s: %s - (fail) %s - %s" % (name, file_ or "(file not named)",
                                                         test, why))
        if len(unit) > PER_CHECK:
            out.append("%s: + %d more failing unit test(s) not named here (%d in all) - the "
                       "excerpt and the Job log replay list them" % (
                           name, len(unit) - PER_CHECK, len(unit)))
        return out

    loose = [(loc, msg) for _, loc, msg in found]
    if loose:
        for loc, msg in loose[:PER_CHECK]:
            out.append("%s: panicked at %s: %s (no failing test name in this check's "
                       "output)" % (name, loc, msg))
        if len(loose) > PER_CHECK:
            out.append("%s: + %d more panic(s) (%d in all)" % (
                name, len(loose) - PER_CHECK, len(loose)))
        return out

    specs, set_aside = spec_verdicts(body)
    if specs:
        out.append("%s: no cargo test failure in this check's output; %d playwright verdict "
                   "line(s), first %d:" % (name, len(specs), min(PER_CHECK, len(specs))))
        if set_aside:
            out.append("%s: %s" % (name, set_aside))
        out += ["%s: | %s" % (name, s) for s in specs[:PER_CHECK]]
        if len(specs) > PER_CHECK:
            out.append("%s: + %d more verdict line(s) not quoted here - the excerpt has "
                       "them" % (name, len(specs) - PER_CHECK))
        return out

    errs = error_lines(body)
    if errs:
        out.append("%s: no cargo test failure in this check's output; %d error line(s), "
                   "first %d:" % (name, len(errs), min(PER_CHECK, len(errs))))
        out += ["%s: | %s" % (name, e) for e in errs[:PER_CHECK]]
        return out

    said = [line for line in body if line.strip()]
    quoted = said[-QUOTE_LINES:]
    out.append("%s: nothing this parser recognises - no failing test, no panic, no error "
               "line; last %d of %d line(s) quoted verbatim:" % (name, len(quoted), total))
    out += ["%s: | %s" % (name, line) for line in quoted]
    return out


# WHAT PLAYWRIGHT SAW, ON THE RECORD (backlog 4d928d0a). gate.sh keeps
# every error context a failed web suite wrote - the page snapshot at the
# moment a spec failed - whole, in a file beside the receipt, and names
# it under `evidence`. That file dies with this pod, so it is copied onto
# the receipt as `fails_context: {"<check>": "..."}` and into the replay.
# Bounded like the excerpt, for the same transport, per spec so a
# mass-fail still shows more than one page, and every cut states itself.
CONTEXT_PER_SPEC = 2000   # characters of one spec's context on the receipt
CONTEXT_CHARS = 6000      # characters of `fails_context` per check
CONTEXT_TOTAL = 12000     # characters of `fails_context` on the whole receipt
CONTEXT_REPLAY = 20000    # lines of a check's kept evidence replayed to the pod log


def spec_contexts(text):
    """The kept file, split on the `===== <path> =====` header gate.sh
    writes above each spec's context."""
    blocks = []
    for line in text.split("\n"):
        if (line.startswith("===== ") and line.endswith(" =====")) or not blocks:
            blocks.append([line])
        else:
            blocks[-1].append(line)
    return ["\n".join(b).rstrip("\n") for b in blocks if any(l.strip() for l in b)]


RE_CONTEXT_LOCATION = re.compile(r"^- Location: (\S+)$", re.M)


def context(text, budget, failed=None):
    """One check's `fails_context`: each spec's context up to
    CONTEXT_PER_SPEC, as many as fit the smaller of CONTEXT_CHARS and
    what is left of CONTEXT_TOTAL, then a line saying how many there
    were and what was left out.

    Playwright writes a context for an expected failure (`test.fail`)
    too, so the contexts of the specs its roll-up counts `failed` are
    placed first: gate-run 5b5a04d8 spent its budget on four expected
    failures and omitted `it-map-routes.mocked.spec.ts:163`'s page, one of
    the two that timed out (backlog 42981848). Nothing is dropped for
    it - the order changes, the count and the replay do not. Since
    a766e20d gate.sh keeps only the contexts its roll-up lists, so this
    order matters only when it could not read one."""
    specs = spec_contexts(text)
    at = {m.group(1) for t in (failed or []) for m in [RE_SPEC_LOCATION.search(t)] if m}
    first = [s for s in specs if any(m in at for m in RE_CONTEXT_LOCATION.findall(s))]
    specs = first + [s for s in specs if s not in first]
    cap = min(CONTEXT_CHARS, budget)
    out, used = [], 0
    for spec in specs:
        piece = spec
        if len(piece) > CONTEXT_PER_SPEC:
            piece = piece[:CONTEXT_PER_SPEC] + (
                "\n... (+%d char(s) of this context omitted - the receipt keeps %d per spec; "
                "the Job log replay has it whole)" % (len(spec) - CONTEXT_PER_SPEC,
                                                     CONTEXT_PER_SPEC))
        if used + len(piece) + 1 > cap - 400:
            break
        out.append(piece)
        used += len(piece) + 1
    note = "(%d spec context(s) in all" % len(specs)
    if first and len(first) < len(specs):
        note += ("; the %d of the spec(s) Playwright's roll-up counts failed come first, the "
                 "rest (an expected failure writes one too) after" % len(first))
    if len(out) < len(specs):
        note += ("; %d omitted here - the receipt keeps %d char(s) of them per check, %d in "
                 "all; the Job log replay has them whole" % (len(specs) - len(out), CONTEXT_CHARS,
                                                             CONTEXT_TOTAL))
    return "\n".join(out + [note + ")"])


def read_evidence(path):
    """(text, None) or (None, why it could not be read)."""
    try:
        with open(path, errors="replace") as fh:
            return fh.read(), None
    except (OSError, TypeError) as exc:
        return None, "(the evidence file %s could not be read: %s)" % (path, exc)


def write(entries, replay, excerpts=None, contexts=None):
    with open(replay_path, "w") as fh:
        fh.write("\n".join(replay) + ("\n" if replay else ""))
    if entries is None:
        return
    RECEIPT["fails_excerpt"] = excerpts or {}
    RECEIPT["fails_context"] = contexts or {}
    if len(entries) > TOTAL_ENTRIES:
        dropped = len(entries) - TOTAL_ENTRIES + 1
        entries = entries[:TOTAL_ENTRIES - 1] + [
            "+ %d more entr%s omitted - this receipt caps `fails` at %d so it stays readable; "
            "the Job log replay has the rest" % (dropped, "y" if dropped == 1 else "ies",
                                                 TOTAL_ENTRIES)]
    RECEIPT["fails"] = [clip(e) for e in entries]
    tmp = receipt_path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(RECEIPT, fh, indent=2)
    os.replace(tmp, receipt_path)


# An unreadable receipt is evidence too, and rewriting it would destroy
# the only copy. Hand over the log's last words and leave the file alone;
# the summary block below reports `verdict: unreadable` on its own.
try:
    with open(receipt_path) as fh:
        RECEIPT = json.load(fh)
    if not isinstance(RECEIPT, dict):
        raise ValueError("receipt is not an object")
except Exception as exc:
    write(None, ["gate-runner: receipt unreadable (%s); falling back to the raw tail" % exc]
          + raw_tail())
    raise SystemExit(0)

checks = RECEIPT.get("checks")
failed = [c.get("name") for c in checks if isinstance(c, dict) and c.get("result") != "pass"] \
    if isinstance(checks, list) else []
failed = [n for n in failed if isinstance(n, str)]

if not failed:
    if RECEIPT.get("verdict") == "green":
        write([], [])
        raise SystemExit(0)
    # A verdict with no failed check means the run ended OUTSIDE a check:
    # the headroom guard refusing, or a crash before the receipt. That is
    # not a consist failure and the record has to be able to say so.
    note = ("no check is marked failed - the run ended outside a check (a headroom refusal, "
            "or a crash before the receipt)")
    if RECEIPT.get("refused_because"):
        note += "; see `refused_because` on this receipt"
    write([note], ["gate-runner: verdict is %s but %s" % (RECEIPT.get("verdict"), note)]
          + raw_tail())
    raise SystemExit(0)

found = sections(failed)
# A run whose EVERY failed check could not reach the network judged
# nothing: the receipt becomes a refusal in the shape gate.sh writes for
# its own disk floor (`verdict: refused`, `refused_because`), so the
# strike rule, the yard and `red_verdict_detail` read it as the
# infrastructure's failure, not the branch's. One judged failure among
# the failed checks keeps the red: a test that failed is a verdict.
refusals = []
for name in failed:
    got = found.get(name)
    why = network_refusal(name, got[0]) if got and got[0] else None
    if why is None:
        refusals = []
        break
    refusals.append(why)
if refusals:
    RECEIPT["verdict"] = "refused"
    RECEIPT["refused_because"] = "; ".join(refusals)
entries, replay, excerpts = [], [], {}
for name in failed:
    got = found.get(name)
    entries += detail(name, got)
    replay.append("")
    replay.append("----- FAILED: %s -----" % name)
    if got is None:
        replay.append("  (no ::group:: block for this check in gate.log - it failed before it")
        replay.append("   ran, or gate.sh changed its grouping and this extractor needs updating)")
        excerpts[name] = "(no ::group:: block for this check in gate.log)"
        continue
    body, total = got
    if not body:
        replay.append("  (the check produced no output at all)")
        excerpts[name] = "(the check produced no output at all)"
        continue
    header, tail = selection(body, total)
    replay.append("  " + header)
    replay += ["  " + line for line in tail]
    # The receipt's copy of the same lines, within what the total cap
    # has left. A check the total cannot fit still gets an entry that
    # says so: an absent key would read as "nothing to say".
    left = EXCERPT_TOTAL - sum(len(e) for e in excerpts.values())
    if left < EXCERPT_FLOOR:
        excerpts[name] = "(excerpt omitted - this receipt caps `fails_excerpt` at %d char(s) " \
                         "in all and %d check(s) before this one used it; the Job log replay " \
                         "has it)" % (EXCERPT_TOTAL, len(excerpts))
    else:
        excerpts[name] = excerpt(tail, left)

# The evidence gate.sh kept for a failed check, onto the record and into
# the replay - see "WHAT PLAYWRIGHT SAW" above.
contexts = {}
evidence = RECEIPT.get("evidence")
for name, path in (evidence.items() if isinstance(evidence, dict) else []):
    text, why = read_evidence(path)
    replay.append("")
    replay.append("----- EVIDENCE: %s (%s) -----" % (name, path))
    if text is None:
        contexts[name] = why
        replay.append("  " + why)
        continue
    lines = text.split("\n")
    kept = lines[:CONTEXT_REPLAY]
    replay += ["  " + line for line in kept]
    if len(lines) > CONTEXT_REPLAY:
        replay.append("  ... (+%d line(s) omitted - this replay keeps %d, and the file itself "
                      "died with this pod)" % (len(lines) - CONTEXT_REPLAY,
                                                                   CONTEXT_REPLAY))
    left = CONTEXT_TOTAL - sum(len(c) for c in contexts.values())
    got = found.get(name)
    failed_specs = failed_by_rollup(got[0]) if got and got[0] else None
    contexts[name] = context(text, left, failed_specs) if left >= EXCERPT_FLOOR else (
        "(context omitted - this receipt caps `fails_context` at %d char(s) in all; the Job "
        "log replay has it)" % CONTEXT_TOTAL)
write(entries, replay, excerpts, contexts)
PY
# --- failure detail (end) ---

# --- receipt summary (begin) ---
# THE PACKET KEEPS THE WHOLE RECEIPT.
#
# `infra/gate.sh` writes a rich account of the run — mode, scope, head,
# dirty, host, ci, free_gb, unverifiable[], every check with its result
# and its duration, and `refused_because` when it declined. Each of
# those fields exists because some reader once had to go and re-derive
# it, and its own comment says so.
#
# This block used to REDUCE that to {verdict, head, mode, fails} before
# reporting, and the rest died on /gate-target with gate.log when the
# Job was reaped. Measured on a live gate-run packet (86553d4f): the
# receipt the packet kept was 101 characters. So `boss-jobs`' yard,
# which reads `receipt.checks` to name a red gate's failing check, found
# nothing to read; `dirty`, which train.rs refuses to board on, never
# arrived; and "which check was slow" was unanswerable from the record.
#
# §Diagnosis: a verdict someone must go re-derive is not a verdict. The
# whole receipt costs a few hundred bytes on the packet, which is the
# cheapest evidence in this pipeline.
#
# COMPACT, one line: `gate-runner: receipt $SUMMARY` below is the third
# copy of the verdict — the one `kubectl logs` serves when the PVC and
# the packet do not (cf0021ae) — and a greppable line has to stay one
# line. `fails` is not re-derived here either — the block above already
# merged it into the receipt this reads, and it is NOT a copy of `checks`:
# `checks` says which check failed, `fails` says which TEST and where it
# panicked, which is the part `checks` cannot carry and a reader acts on.
# A `fails` that merely restated the check names would be a fact living
# twice inside one document with nothing holding the two equal
# (CLAUDE.md §9a), which is why it does not.
#
# `seed_order` is the runner's own measurement, not gate.sh's: whether
# every cloned source is newer than every seeded artifact (646c0ade). It
# is added to an unreadable receipt too — a run that died is exactly
# the one whose build a reader may want to rule a stale artifact out of.
SUMMARY=$(python3 - "$RECEIPT" "$HEAD_SHA" "${SEED_ORDER:-}" <<'PY'
import json, sys
def with_order(body):
    if isinstance(body, dict) and sys.argv[3]:
        try:
            body["seed_order"] = json.loads(sys.argv[3])
        except ValueError:
            body["seed_order"] = {"unparsed": sys.argv[3]}
    return body
try:
    print(json.dumps(with_order(json.load(open(sys.argv[1]))), separators=(",", ":")))
except Exception as e:
    # gate.sh died before writing a receipt, or wrote something
    # unparseable. That is its own verdict, never a silent green.
    print(json.dumps(with_order({"verdict": "unreadable", "head": sys.argv[2],
                                 "error": str(e)}), separators=(",", ":")))
PY
)
# --- receipt summary (end) ---
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

REPORTED=0
if report "$VERDICT" "$SUMMARY"; then
    REPORTED=1
else
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
            echo "  WILL FIRE."
            echo "  A terminal packet cannot accept a verdict on a STEP — file a fresh"
            echo "  gate-run packet rather than reusing one across relaunches (64cae7e9)."
            # BUT IT STILL ACCEPTS METADATA, so the verdict does not have
            # to die with this pod.
            #
            # The lines above have existed since 2026-08-27 and a green
            # run still evaporated on 2026-08-29 (1826ec9f), because a
            # pod log is not a record: the Job is reaped, `kubectl logs`
            # goes with it, and the packet says `lost` for a branch that
            # gated green. The step API refuses a frozen step and names
            # the job-metadata PATCH as the way to annotate instead —
            # verified 2026-08-30 that a CLOSED packet accepts it and
            # that other keys survive the merge.
            #
            # This records; it does not reopen. Reviving a terminal
            # packet would fight the freeze that makes receipts
            # trustworthy, which is why the packet asked for the verdict
            # to be RECOVERABLE rather than automatically re-applied.
            ORPHAN=$(python3 - "$SUMMARY" "$RECEIPT" "$(hostname)" <<'PY'
import json, sys
print(json.dumps({"orphaned_verdict": {
    "receipt": json.loads(sys.argv[1]),
    "receipt_path": sys.argv[2],
    "pod": sys.argv[3],
    "note": "the gate ran to a verdict, but its packet was already terminal "
            "so no step could take it. Recorded here rather than lost with "
            "the pod. The packet's own status is NOT evidence about this run.",
}}))
PY
            )
            if curl -sf -X PATCH -H "x-boss-user: $ACTOR" \
                 -H 'content-type: application/json' -d "$ORPHAN" \
                 "$JOBS_API/api/jobs/$GATE_RUN_JOB_ID/metadata" >/dev/null 2>&1; then
                echo "  RECOVERED: verdict written to the packet as metadata.orphaned_verdict."
            else
                echo "  AND THE METADATA WRITE FAILED TOO — this log is the only record."
            fi
            ;;
    esac
fi

# WHY THE FAILING OUTPUT IS REPLAYED TO STDOUT. gate.log is written to
# /gate-target, which only the gate container mounts — not the postgres
# sidecar, not any later Job. When this container exits, the last reader
# of that file is gone. So a failure that cost an hour to produce left a
# receipt naming WHICH check failed and no way whatsoever to learn WHY.
#
# That is not hypothetical: three branches were called red by this rig
# and then passed every one of those same checks run by hand, and no
# theory could be tested because the evidence died with the pod
# (backlog 9c7ed804). A verdict nobody can explain is barely better
# than no verdict — it teaches the reader to distrust the gate.
#
# stdout is the one surface that outlives the container: `kubectl logs`
# serves a terminated pod for as long as the Job exists. gate.sh already
# brackets every check with `::group::gate: <name>` / `::endgroup::`, so
# the failure-detail block above replays exactly the sections that failed
# and nothing else. That distinction is the whole point — a full gate.log
# is mostly successful build chatter and dumping it whole would bury the
# three lines that matter.
#
# THIS IS A PRINTER NOW, not a second parser. The extraction moved up to
# the one block that also writes the receipt's `fails`, because "what did
# the failed checks say" is one fact and it was about to live twice
# (CLAUDE.md §9a). Its `|| tail` belt remains for the case where that
# block did not run at all; every case it DID handle — an unreadable
# receipt, a verdict with no failed check — writes its own explanation
# plus the raw tail into the file, so the fallback says why it fired.
if [ "$VERDICT" != green ]; then
    echo "=== gate-runner: replaying failed checks from gate.log ==="
    cat /gate-target/failed-checks.txt 2>/dev/null || tail -200 /gate-target/gate.log || true
    echo "=== end of failed-check replay ==="
fi

# REFRESH THE SEED — the housekeeping that keeps parallel gates warm.
# Runs AFTER the verdict is reported (a refresh must never delay a
# `--wait`), and only from a run whose target is worth inheriting:
#
#   - GREEN only. A red run's target is usually fine (test failures
#     still compile), but a compile-error red would seed broken
#     workspace artifacts, and telling the cases apart buys nothing:
#     green near-tip runs happen many times a day.
#   - AT/NEAR main's tip, measured not felt: every car gates as
#     main + one change, so `rev-list --count HEAD..origin/main` is 0
#     in the common case and small when a train merged mid-gate. Past
#     2 the branch is stale-based and its target would seed the
#     distance to main into every later gate.
#   - EXCLUSIVE, NON-BLOCKING lock. Readers hold the lock shared while
#     copying; a second refresher just skips (-n) — best-effort
#     housekeeping does not queue.
#   - STAGE THEN RENAME. The copy lands in target.partial and is
#     mv-ed into place; a pod that dies mid-refresh (w-1 has reset
#     mid-gate before) leaves a MISSING seed — next gate cold, slow,
#     correct — never a torn one under a fresh-looking marker. The
#     old seed is removed first because two targets (~74G each) do
#     not fit the 120Gi volume; the cold window is the price of
#     fitting, and it only opens on a mid-refresh death.
# PRUNE THE STALE BINARIES BEFORE THE SEED IS RENAMED INTO PLACE.
# Measured 2026-09-17 17:58Z, the day the seed grew past the workspace:
# /gate-seed/target was 154G, 153G of it debug/deps, and 3,226 of those
# files were TEST AND BIN EXECUTABLES — 138 builds of `boss`, 195 of
# `boss_dispatcher`, 126 of `rebuild_e2e`, each 50–110 MB, one per
# gate that ever relinked them. Cargo names an executable
# <stem>-<16 hex> and never deletes a superseded one, so the seed
# grows by every relink and a reflink copy of it is measured by the
# kubelet at full size: the train gate for #424 was evicted two minutes
# in ("Usage of EmptyDir volume gate-workspace exceeds the limit
# 160Gi") before a single check ran, and settled LOST. An executable is
# never reused across source changes — the next gate relinks it in
# seconds — so the seed keeps ONE per stem (the newest) and drops the
# rest. Libraries (.rlib/.rmeta/.so/.d) are untouched: those ARE the
# warmth. Pure over its argument; the refresh calls it on the staged
# copy, so a torn prune can only ever touch target.partial.
prune_seed_binaries() { # <deps dir> — prints "pruned N binaries, M MiB"
    local deps="$1" n=0 bytes=0 f stem
    [ -d "$deps" ] || { echo "pruned 0 binaries, 0 MiB"; return 0; }
    # newest first, so the first of each stem is the keeper
    while IFS= read -r f; do
        case "$f" in *.*) continue ;; esac
        [ -f "$deps/$f" ] && [ -x "$deps/$f" ] || continue
        stem=$(printf '%s' "$f" | sed -E 's/-[0-9a-f]{16}$//')
        [ "$stem" = "$f" ] && continue
        case " $KEPT " in *" $stem "*)
            bytes=$((bytes + $(stat -c %s "$deps/$f")))
            rm -f -- "$deps/$f" "$deps/$f.d" && n=$((n + 1)) ;;
        *) KEPT="$KEPT $stem" ;;
        esac
    done < <(ls -t "$deps" 2>/dev/null)
    echo "pruned $n binaries, $((bytes / 1048576)) MiB"
}

refresh_seed() {
    if [ ! -d "$SEED" ]; then return 0; fi
    if ! [ "$VERDICT" = "green" ]; then return 0; fi
    git fetch --depth 50 origin "+main:refs/remotes/origin/main" >/dev/null 2>&1 || {
        echo "gate-runner: seed not refreshed — could not re-fetch origin/main"; return 0; }
    local behind
    # rev-list prints a count or fails (shallow clone, no merge base
    # within depth) — 999 makes "cannot measure" read as "too far".
    behind=$(git rev-list --count HEAD..origin/main 2>/dev/null || echo 999)
    if [ "$behind" -gt 2 ]; then
        echo "gate-runner: seed not refreshed — HEAD is $behind commit(s) behind origin/main"
        return 0
    fi
    if [ "$(cat "$SEED/.seed-head" 2>/dev/null || true)" = "$HEAD_SHA" ]; then
        echo "gate-runner: seed already at $HEAD_SHA — not refreshed"
        return 0
    fi
    local t0=$SECONDS
    if ( flock -x -n 9 &&
         rm -f "$SEED/.seed-head" &&
         rm -rf "$SEED/target" "$SEED/target.partial" &&
         cp -a --reflink=auto /gate-target/target "$SEED/target.partial" &&
         { KEPT=""; prune_seed_binaries "$SEED/target.partial/debug/deps"; } &&
         mv "$SEED/target.partial" "$SEED/target" &&
         echo "$HEAD_SHA" > "$SEED/.seed-head"
       ) 9>>"$SEED_LOCK"; then
        echo "gate-runner: seed refreshed to $HEAD_SHA in $((SECONDS - t0))s"
    else
        echo "gate-runner: seed refresh skipped (another writer holds the lock) or failed after $((SECONDS - t0))s — the previous seed stands"
    fi
}
refresh_seed || true

tail -5 /gate-target/gate.log || true
echo "gate-runner: $GATE_BRANCH@${HEAD_SHA:0:10} -> $VERDICT"
# AN UNREPORTED VERDICT IS A FAILED RUN. This used to exit on the gate
# verdict alone, so a green gate whose report never landed left a Job
# reading Complete beside a packet that never closed - the exact shape
# a reader mistakes for "nothing happened here" (2026-09-07: four such
# Jobs, each hand-re-gated). The Job status is a claim about the RUN,
# and a run that could not record its result did not finish its job.
# Exit 75 (EX_TEMPFAIL), distinct from a red gate's 1, and one greppable
# line carrying everything the packet should have received. `boss gate
# --wait` already reads a failed Job with a silent packet as "read the
# log, this is NOT a red gate" - so the verdict is not mistaken for red,
# it is found.
if [ "$REPORTED" != 1 ]; then
    echo "gate-runner: UNREPORTED verdict=$VERDICT packet=$GATE_RUN_JOB_ID head=$HEAD_SHA receipt $SUMMARY"
    exit 75
fi
[ "$VERDICT" = green ]
