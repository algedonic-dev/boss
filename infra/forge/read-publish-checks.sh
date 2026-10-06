#!/usr/bin/env bash
#
# read-publish-checks — read the mirror pull request's check results
# BACK onto the publish packet, so the merge follows a judged reading
# instead of a red badge.
#
# THE GAP (backlog 321f1409, David 2026-09-19). The public mirror runs
# CodeQL on every publish PR, and its result never returned to the
# system of record: the check runs AFTER the publish packet's sign-off,
# on a surface only David sees, at merge time. PR #238 (publish/
# 2026-09-11) was red — "64 new alerts including 18 critical" — and was
# merged over the red by hand on 2026-09-12; PR #239 (publish/2026-09-19,
# 1319 files) stalled on "109 new alerts including 10 critical". Read
# from the pod over the public API, of the 100 annotations GitHub
# exposes for #239: 81 rust/cleartext-logging where the "sensitive"
# value is account_id / employee_id / current_uid (a name heuristic), 9
# hard-coded-cryptographic-value on #[cfg(test)] constants, 4 cleartext-
# transmission on in-cluster http URLs, 2 uncontrolled-path, 2
# uncontrolled-allocation, 1 SSRF, 1 sanitization — zero real findings,
# and the check's own footnote says a snapshot this large reads as
# all-new code. A check whose result never returns to the record is a
# check nobody reads (CLAUDE.md §Diagnosis).
#
# WHAT THIS DOES. publish-to-github v7's `read-checks` step is a MACHINE
# step that becomes ready when `open-pr` completes; the dispatcher rule
# read-publish-checks-on-read-checks-ready files an ops-request for the
# forge, and the root ops-runner runs THIS script with no arguments. It:
#
#   1. finds the one open publish-to-github packet whose `read-checks`
#      step is ready, and takes the PR's head sha and url off its
#      completed `open-pr` step — the sha publish-github-pr RECORDED
#      when it pushed (`snapshot_commit`), never re-derived;
#   2. reads GET /repos/<mirror>/commits/<head>/check-runs ONCE — the
#      mirror is public, so no credential — and reads the scan when the
#      code-scanning checks have completed: the check named $SCAN_CHECK
#      and its `Analyze (…)` jobs (Analyze (rust) took 13 min on both
#      #238 and #239). A head with no check-runs yet, or with those
#      still running, is NOT YET: the partial reading goes on the
#      packet, the step stays open, and the verb exits 75 at once
#      (backlog b81ff4ca — see THE RUNNER IS NEVER HELD below). A
#      GitHub that cannot be read is a FAILED line, never a not-yet;
#   2b. BUT THE STEP COMPLETES ONLY ON EVERY CHECK (backlog c6cb678b,
#      2026-09-24). d167e7d7 reasoned that `judge-checks` reads nothing
#      but the scan, and so completed the step with the gate still
#      running — which made a slow check's verdict one nobody read. PR
#      #243: the mirror's Gate concluded failure at 04:18Z, the reading
#      had recorded it in_progress minutes earlier, `judge-checks`
#      judged CodeQL's three rules, and the packet closed `pr-opened`
#      at 04:41Z; the publish region turned red 25 hours later on the
#      PR's AGE. So a check still running once the scan is read is
#      NOT YET: the partial reading goes on the packet, the step stays
#      open, and this verb exits 75 at once — the runner is not held
#      for the gate. The rule `reread-publish-pr-every-15-minutes`
#      files this verb again until every check has completed, and the
#      step's `conclusion` is then the verdict over ALL of them: the
#      scan's own when it did not pass, else `failure` when any other
#      check failed, named in `failing`. A check still running
#      $CEILING seconds after the PR opened completes the step as
#      `unfinished` with what was still running named, so the judge
#      step reads it rather than the packet waiting forever;
#   3. reads the code-scanning check's annotations (every page the API
#      exposes; GitHub caps a check-run's exposed annotations, and the
#      reading records the declared count beside the read count);
#   3b. reads every OTHER failing check's annotations too, for the
#      failed checks infra/gate.sh names there on Actions — the Gate's
#      job log is admin-only, its annotations are public (backlog
#      29c36336) — and puts them on that check's `failing` entry;
#   4. PATCHes the reading onto the packet's metadata as
#      `code_scanning` — one entry per check with its conclusion, and
#      for the scanning check the counts by rule, by file and by level
#      — and completes `read-checks` with `conclusion`, `alerts` and
#      `rules`. The protocol's `judge-checks` step then asks the agent
#      for a disposition per rule; the merge on GitHub stays David's.
#
# THE RUNNER IS NEVER HELD (backlog b81ff4ca, 2026-09-28). The forge's
# ops-runner is a one-minute oneshot that answers requests serially, so
# for as long as this verb runs no other forge request is answered.
# Until this change the verb WAITED in-verb for the scan — polling every
# 60 s up to 1500 s — under a header that called the wait a stated cost
# "to revisit if the runner's queue shows the delay". It showed it:
# read-publish-checks 444d0f22 held the runner 786 s, waited out
# Analyze (rust), then exited 75 on the mirror's Gate anyway, while
# eight requests queued behind it (depth 8, oldest wait 748 s — a train
# converge and six car probes among them) and ops.queue.alarm 2163a4c5
# fired on a runner that was busy, not silent. The reason the wait was
# kept — that a cadence rule re-filing the request would leave a no-op
# packet per firing — had already gone: the re-reads below file this
# verb every fifteen minutes anyway. So the scan is waited for the way
# every other check is: one reading, not yet, and the next re-read. The
# cost, stated: the scan's verdict reaches the packet up to fifteen
# minutes after it concludes, once per publish, against a runner held
# thirteen minutes for every other forge request.
#
# The re-reads (2b) are that cadence: unconditional, with the
# idempotence here — a firing with no packet waiting answers `nothing
# to do`, as mirror-drift does on a quiet day. Hourly until backlog
# 663589cd, and every fifteen minutes since, because the same firing
# now asks what became of each publish PR (step 0) and a close must not
# alarm for an hour. Its cost is stated: 96 ops-requests a day, almost
# all of them that answer.
#
# Idempotent: re-running for the same packet re-reads and re-PATCHes
# the same keys; a step already completed is nothing to do.
#
# --check: tools and addresses, no network, exit 0/1 — what the gate
# lint runs, and the one argument the allowlist admits.
#
# Runs as root under the ops-runner with NO HOME: nothing reads $HOME.
set -euo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

# THE MIRROR — owner/repo, from the one place it is spelled:
# infra/estate/estate.toml, rendered onto this host as /etc/boss/sor.env
# (infra/lib/sor.sh). An explicit BOSS_MIRROR_SLUG still wins and is
# taken FIRST, because sourcing sor.sh with BOSS_SOR_ENV named REPLACES
# what the environment carried — that is how the tests point this verb
# at a fixture. Until 2026-09-20 the default here was the slug spelled
# out, one of four copies of it owned by nothing (backlog f8af6040).
MIRROR_SLUG="${BOSS_MIRROR_SLUG:-}"
if [ -z "$MIRROR_SLUG" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_SLUG
    MIRROR_SLUG="$BOSS_MIRROR_SLUG"
fi
GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"
# The check whose annotations are the alerts. GitHub's code-scanning
# roll-up posts as one check-run named for the tool.
SCAN_CHECK="${BOSS_CODE_SCANNING_CHECK:-CodeQL}"
# Its per-language jobs, by name prefix — GitHub's CodeQL setup names
# them `Analyze (rust)`, `Analyze (javascript-typescript)`. Until these
# and $SCAN_CHECK have completed the reading is not yet (backlog
# d167e7d7).
SCAN_JOBS="${BOSS_CODE_SCANNING_JOBS:-Analyze (}"
# THE CEILING on a check still running after the scan is read, counted
# from the instant `open-pr` completed (backlog c6cb678b). Four hours:
# the mirror gate's own `timeout-minutes: 180` in .github/workflows/
# ci.yml, plus a re-read to see it end. Past it the step
# completes `unfinished`, naming what never finished.
CEILING="${BOSS_CHECKS_CEILING_SECONDS:-14400}"
# GitHub pages a check-run's annotations 100 at a time; ten pages is
# well past what it exposes for one run.
MAX_PAGES=10
ACTOR="${BOSS_OPS_ACTOR:-automation:read-publish-checks}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

me="read-publish-checks"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}" >&2; exit 1; }
trap 'rm -rf "$workdir"' EXIT

# The machine token rides to curl in a 0600 file, never in its argv,
# where every local user reads it in ps (backlog 5f3ad356). Made here,
# in the script's own shell and after the trap above, because the reads
# below run inside $(…).
# shellcheck source=infra/lib/secret-header.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/secret-header.sh"
machine_token_header MT_HDR "${BOSS_JOBS_URL:-}" \
    || fail "the machine token's header file could not be written"

check_inputs() {
    local rc=0
    for tool in curl jq; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "$me: missing tool on PATH: $tool" >&2; rc=1
        fi
    done
    case "$MIRROR_SLUG" in
        */*) ;;
        *) echo "$me: BOSS_MIRROR_SLUG '$MIRROR_SLUG' is not owner/repo" >&2; rc=1 ;;
    esac
    case "${CEILING:-empty}" in
        empty|*[!0-9]*) echo "$me: BOSS_CHECKS_CEILING_SECONDS ($CEILING) must be whole seconds" >&2; rc=1 ;;
    esac
    return $rc
}

if [ "${1:-}" = "--check" ]; then
    echo "$me --check"
    echo "  mirror     : $MIRROR_SLUG via $GITHUB_API (public, unauthenticated)"
    echo "  scan check : $SCAN_CHECK, with its jobs named '$SCAN_JOBS…'"
    echo "  reading    : ONE per run, never a wait; any check still running is not yet (exit 75), re-read every 15 minutes, which also re-reads every standing publish PR's state"
    echo "  ceiling    : ${CEILING}s after the PR opened, a check still running completes the step as unfinished"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    if check_inputs; then
        echo "$me: --check ok"
        exit 0
    fi
    echo "$me: --check FAILED — see above" >&2
    exit 1
fi
[ $# -eq 0 ] || refuse "the only argument admitted is --check (got: $*)"

# ---------------------------------------------------------------------
# A run.
# ---------------------------------------------------------------------
[ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
BASE="${BOSS_JOBS_URL%/}"
check_inputs || refuse "inputs incomplete (see above); nothing was read"

# 0. WHAT BECAME OF EACH PUBLISH PULL REQUEST (backlog 663589cd). Every
#    run, before and whatever the reading below finds: the re-read rule
#    files this verb every fifteen minutes, so a PR David closes or
#    merges — or one whose checks turn red after its reading was taken —
#    is on its packet as `pr_state` within the quarter hour, where the
#    daily --measure used to be the only reader and #244 alarmed 22 hours
#    past its close. The pass is publish-pr-state.sh, the one definition
#    --measure shares.
# shellcheck source=infra/forge/publish-pr-state.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/publish-pr-state.sh"
publish_pr_states "read-publish-checks"

# 1. The packet: the open publish whose read-checks is ready or active
#    (a rule fired on readiness), and what its open-pr recorded.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20&full=true" > "$workdir/jobs" 2> "$workdir/err"; then
    fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
fi
target=$(jq -c '
    # Slug first, title as fallback — the boss-step.sh idiom.
    def step($slug): (((.steps // []) | map(select(.spec_slug == $slug)) | .[0])
                      // ((.steps // []) | map(select(.title == $slug)) | .[0]));
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open"))
    | map({id, title, read: step("read-checks"), open: step("open-pr")})
    | map(select(.read != null and (.read.status == "ready" or .read.status == "active")))
    | .[0] // empty' "$workdir/jobs" 2>/dev/null || true)
if [ -z "$target" ]; then
    # The jq above is written once; a shape it cannot read is a
    # failure to say so, not "nothing to do".
    # NOTHING READ is not "nothing to do", and that is the input
    # `jq -e` alone calls a pass (d96e38ab).
    if ! jq_doc_file "$workdir/jobs" \
        || ! jq -e '(if type == "object" and has("data") then .data else . end) | type == "array"' "$workdir/jobs" >/dev/null 2>&1; then
        fail "the jobs API answered something that is not a packet list: $(head -c 200 "$workdir/jobs" | tr '\n' ' ')"
    fi
    say "no open publish-to-github packet has its read-checks step ready — nothing to do"
    exit 0
fi
job_id=$(printf '%s' "$target" | jq -r '.id')

# 1a. THE REQUEST NAMES THE PACKET IT READ (backlog fd808d90, measured
#     2026-09-27). The answer rule complete-publish-read-checks-on-read-
#     publish-checks-answered follows `for_publish` from the closed
#     request to the publish — and a request `reread-publish-pr-every-
#     15-minutes` files from a timer names no packet, because a clock
#     firing has none to name. So eight re-reads of publish 8d7a3507
#     exited 2 on the refusal below between 14:00Z and 15:45Z, each
#     closed `answered`, and not one reached the read-checks step or
#     filed an alert. This verb is the one party that knows which packet
#     it read, so it writes the edge onto its own request (the runner's
#     OPS_REQUEST_ID) HERE — before any refusal, and before the reading
#     — and every answer, a refusal included, then reaches the step the
#     same way the step-ready rule's requests do. It also corrects that
#     rule's edge when this run read a different packet than the one
#     whose step went ready: the answer is about the packet read.
#     A hand run has no request, and writes nothing here.
if [[ "${OPS_REQUEST_ID:-}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
    jq -n -c --arg p "$job_id" '{for_publish: $p}' > "$workdir/for-publish"
    if curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$workdir/for-publish" "$BASE/api/jobs/$OPS_REQUEST_ID/metadata" > /dev/null 2>"$workdir/err"; then
        say "request ${OPS_REQUEST_ID:0:8} names publish $job_id (for_publish)"
    else
        say "request ${OPS_REQUEST_ID:0:8} could not be written for_publish=$job_id — $(head -c 200 "$workdir/err" | tr '\n' ' '); a failure below reaches no step"
    fi
fi
step_id=$(printf '%s' "$target" | jq -r '.read.id')
open_status=$(printf '%s' "$target" | jq -r '.open.status // "absent"')
head=$(printf '%s' "$target" | jq -r '.open.metadata.snapshot_commit // ""')
pr_url=$(printf '%s' "$target" | jq -r '.open.metadata.pr_url // ""')
# When the PR opened: the server's stamp on open-pr, the ceiling's zero.
pr_opened_at=$(printf '%s' "$target" | jq -r '.open.completed_at // ""')
[ "$open_status" = "completed" ] \
    || refuse "packet ${job_id:0:8}: read-checks is ready but its open-pr step is '$open_status' — the PR is not on the record yet"
case "$head" in
    *[!0-9a-f]* | "")
        refuse "packet ${job_id:0:8}: the open-pr step recorded no head sha (metadata.snapshot_commit is '${head:-empty}') — publish-github-pr writes it when it pushes; nothing was read" ;;
esac
[ -n "$pr_url" ] || refuse "packet ${job_id:0:8}: the open-pr step recorded no pr_url; nothing was read"
say "packet ${job_id:0:8} — reading the checks on $pr_url (head ${head:0:12})"

# 2. The check-runs: ONE reading, never a wait (backlog b81ff4ca).
gh_get() {
    # One GET against the public API; the body to stdout, curl's words
    # to $workdir/err. No token: the mirror is public, and this verb
    # holds none.
    curl -fsS -H "accept: application/vnd.github+json" "$GITHUB_API/repos/$MIRROR_SLUG/$1" 2>"$workdir/err"
}
checks="$workdir/checks.json"
# A GitHub that cannot be read is a FAILURE, not a not-yet: nothing was
# measured, and the answer rule troubles the step and alerts on it. An
# empty 200 is no check list either — `jq` alone reads silence as a
# pass (d96e38ab).
if ! gh_get "commits/$head/check-runs?per_page=100" > "$checks"; then
    fail "GET commits/${head:0:12}/check-runs — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
fi
jq_doc_file "$checks" \
    || fail "GET commits/${head:0:12}/check-runs answered something that is not JSON: $(head -c 200 "$checks" | tr '\n' ' ')"
total=$(jq -r '.total_count // 0' "$checks")
running=$(jq -r '[.check_runs[]? | select(.status != "completed") | .name] | join(", ")' "$checks")
# The scan is read when its check has COMPLETED and none of its jobs is
# still running. A head where only a non-scanning check has registered
# is not yet — the gate can start before CodeQL.
scan_state=$(jq -r --arg n "$SCAN_CHECK" --arg p "$SCAN_JOBS" '
    [.check_runs[]?] as $r
    | if ([$r[] | select(.name == $n or (.name | startswith($p)))
                | select(.status != "completed")] | length) > 0 then "running"
      elif ([$r[] | select(.name == $n)] | length) == 0 then "absent"
      else "done" end' "$checks")
note=""
case "${total:-0}" in
    0) note="no check-runs registered on ${head:0:12} yet" ;;
    *) if [ -n "$running" ] && [ "$scan_state" != "done" ]; then
           case "$scan_state" in
               absent) note="$total check-runs on ${head:0:12}, no $SCAN_CHECK check-run yet, still running: $running" ;;
               *) note="$total check-runs on ${head:0:12}, still running: $running" ;;
           esac
       fi ;;
esac
if [ -n "$note" ]; then
    # NOT YET: what was seen goes on the record as a PARTIAL reading and
    # the step stays open — a scan still running is not a conclusion,
    # and this runner is not the place to wait for one.
    jq -c --arg head "$head" --arg pr "$pr_url" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg note "$note" '
        {code_scanning: {
            read_at: $at, head: $head, pr_url: $pr, complete: false, note: $note,
            checks: [.check_runs[]? | {name, status, conclusion, title: .output.title,
                                      annotations: .output.annotations_count, url: .html_url,
                                      app: .app.slug}],
            still_running: [.check_runs[]? | select(.status != "completed") | .name]}}' "$checks" > "$workdir/partial"
    curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${MT_HDR:+-H "$MT_HDR"} \
        --data-binary @"$workdir/partial" "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
        || fail "writing the partial reading onto ${job_id:0:8} (PATCH /api/jobs/$job_id/metadata) — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    echo "$me: not yet: $note — the partial reading is on ${job_id:0:8} (code_scanning.complete=false), read-checks stays open, and reread-publish-pr-every-15-minutes reads it again"
    exit 75
fi
if [ -z "$running" ]; then
    say "$total check-runs on ${head:0:12}, all completed"
else
    say "$total check-runs on ${head:0:12}, the code-scanning checks completed; still running: $running"
fi

# 3. The scanning check's annotations, every page.
scan_id=$(jq -r --arg n "$SCAN_CHECK" '[.check_runs[] | select(.name == $n)] | .[0].id // empty' "$checks")
ann="$workdir/annotations.json"
echo '[]' > "$ann"
if [ -n "$scan_id" ]; then
    page=1
    while [ "$page" -le "$MAX_PAGES" ]; do
        gh_get "check-runs/$scan_id/annotations?per_page=100&page=$page" > "$workdir/page" \
            || fail "GET check-runs/$scan_id/annotations page $page — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
        n=$(jq 'length' "$workdir/page")
        jq -s '.[0] + .[1]' "$ann" "$workdir/page" > "$ann.new" && mv "$ann.new" "$ann"
        [ "$n" -eq 100 ] || break
        page=$((page + 1))
    done
    say "$SCAN_CHECK check-run $scan_id: $(jq 'length' "$ann") annotations read"
else
    say "no check-run named $SCAN_CHECK on ${head:0:12} — the reading says so"
fi

# 3b. WHICH CHECK FAILED, for every other check that did not pass
#     (backlog 29c36336). PR #245's Gate exited 1 and this reading named
#     only `Gate (infra/gate.sh, full): failure`: the gate names its
#     failed check in the job log, and GitHub serves a job log only to a
#     repository admin (403 to anyone else, this verb included). Its
#     ANNOTATIONS are public, and on Actions infra/gate.sh prints one per
#     failed check — `GATE FAIL: <name>` with the check's first useful
#     line, at most five, then one `GATE FAIL (N more)` naming the rest a
#     line each, and `GATE REFUSED` with the reason on a refusal. They
#     are read here, one page (a check-run exposes at most 50), and step
#     4 copies them onto that check's `failing` entry. An annotations
#     read GitHub refuses is recorded as unread, by its error — the
#     check's failure stands either way, so the reading does not fail
#     on it.
gate_ann="$workdir/gate-annotations.json"
echo '{}' > "$gate_ann"
while read -r run_id; do
    [ -n "$run_id" ] || continue
    # An empty 200 is no annotation list, and `jq -e` alone reads
    # silence as a pass (d96e38ab).
    if gh_get "check-runs/$run_id/annotations?per_page=100" > "$workdir/page" \
        && jq_doc_file "$workdir/page" \
        && jq -e 'type == "array"' "$workdir/page" > /dev/null 2>&1; then
        jq --arg id "$run_id" --slurpfile a "$workdir/page" '. + {($id): {annotations: $a[0]}}' \
            "$gate_ann" > "$gate_ann.new" && mv "$gate_ann.new" "$gate_ann"
    else
        err="$(head -c 200 "$workdir/err" | tr '\n' ' ')"
        jq --arg id "$run_id" --arg e "${err:-the answer was not an annotation list}" '. + {($id): {error: $e}}' \
            "$gate_ann" > "$gate_ann.new" && mv "$gate_ann.new" "$gate_ann"
        say "check-run $run_id: its annotations could not be read — ${err:-the answer was not an annotation list}"
    fi
done < <(jq -r --arg n "$SCAN_CHECK" --arg p "$SCAN_JOBS" '
    .check_runs[]? | select(.status == "completed"
                            and ((.conclusion // "none") | IN("success", "neutral", "skipped") | not)
                            and .name != $n and (.name | startswith($p) | not))
    | .id' "$checks")

# 4. The reading, and the writes: the packet's metadata first, then the
#    step, so a step that reads done always has its reading beside it.
jq -n -c --slurpfile checks "$checks" --slurpfile ann "$ann" --slurpfile gann "$gate_ann" \
      --arg head "$head" --arg pr "$pr_url" --arg scan "$SCAN_CHECK" \
      --arg capped "past the gate's annotation cap; its words are in the job log" \
      --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    # What the annotations of a check-run say failed (3b): each
    # `GATE FAIL: <name>` with its message, in log order; each name the
    # `GATE FAIL (N more)` summary lists after its first line; and a
    # `GATE REFUSED` reason. Only failure-level annotations, and only
    # these titles — the exit-code line and the deliberate browser
    # fixtures of the web suite are not failed checks.
    def gate_reading($r):
      if $r == null then {}
      elif $r.error then {annotations_unread: $r.error}
      else ([$r.annotations[] | select(.annotation_level == "failure")]
            | sort_by(.start_line // 0)) as $f
        | {failed_checks: [$f[] | (.title // "") as $t
                           | if ($t | startswith("GATE FAIL: ")) then
                               {name: ($t | ltrimstr("GATE FAIL: ")), message: (.message // "")}
                             elif ($t | test("^GATE FAIL \\([0-9]+ more\\)$")) then
                               ((.message // "") | split("\n") | .[1:][] | select(length > 0)
                                | {name: ., message: $capped})
                             else empty end]}
          + ([$f[] | select(.title == "GATE REFUSED") | .message] as $why
             | if ($why | length) > 0 then {refused: $why[0]} else {} end)
      end;
    ($checks[0].check_runs) as $runs
    | ($ann[0]) as $a
    | ($gann[0]) as $g
    | ([$runs[] | select(.name == $scan)] | .[0]) as $s
    # A check that ran and did not pass. `neutral` and `skipped` are
    # GitHub saying the check had nothing to object to.
    | [$runs[] | select(.status == "completed"
                        and ((.conclusion // "none") | IN("success", "neutral", "skipped") | not))
               | {name, conclusion: (.conclusion // "none")} + gate_reading($g[(.id | tostring)])] as $failing
    | ([$runs[] | select(.status != "completed") | .name]) as $running
    | {code_scanning: {
        read_at: $at, head: $head, pr_url: $pr, complete: ($running | length == 0),
        # THE VERDICT OVER EVERY CHECK (backlog c6cb678b): the scan when
        # it did not pass, else `failure` when any other check failed —
        # a Gate red on the PR is a red reading, never a clean one.
        conclusion: (if $s == null then "absent"
                     elif ($s.conclusion // "none") != "success" then ($s.conclusion // "none")
                     elif ([$failing[] | select(.name != $scan)] | length) > 0 then "failure"
                     else "success" end),
        failing: $failing,
        checks: [$runs[] | {name, status, conclusion, title: .output.title,
                            annotations: .output.annotations_count, url: .html_url, app: .app.slug}],
        # A check here is still running and has no conclusion on this
        # record; the step does not complete on it (backlog c6cb678b).
        still_running: $running,
        alerts: (if $s == null then {check: $scan, conclusion: "absent", read: 0, by_rule: [], by_file: [], by_level: {}}
                 else {
                    check: $scan, conclusion: ($s.conclusion // "none"), title: $s.output.title,
                    url: $s.html_url,
                    # The check-run summary footnote: alerts not introduced by the PR may be
                    # reported because the change was too large — true of every week-scale snapshot.
                    caveat: (($s.output.summary // "") | test("too large")),
                    declared: ($s.output.annotations_count // 0),
                    read: ($a | length),
                    by_level: ($a | group_by(.annotation_level) | map({(.[0].annotation_level): length}) | add // {}),
                    by_rule: ($a | group_by(.title) | map({rule: .[0].title, count: length,
                                                          files: ([.[].path] | unique | length)})
                              | sort_by(-.count, .rule)),
                    by_file: ($a | group_by(.path) | map({path: .[0].path, count: length})
                              | sort_by(-.count, .path))
                 } end)}}' > "$workdir/reading"
alerts=$(jq -r '.code_scanning.alerts.read' "$workdir/reading")
rules=$(jq -r '.code_scanning.alerts.by_rule | length' "$workdir/reading")
files=$(jq -r '.code_scanning.alerts.by_file | length' "$workdir/reading")

# A CHECK STILL RUNNING is not yet — or, past the ceiling, unfinished.
if [ -n "$running" ]; then
    opened_s=""
    [ -n "$pr_opened_at" ] && opened_s=$(date -u -d "$pr_opened_at" +%s 2>/dev/null || true)
    case "${opened_s:-empty}" in
        empty|*[!0-9]*) past=0; age="unknown (open-pr carries no readable completed_at '${pr_opened_at}')" ;;
        *) age=$(( $(date -u +%s) - opened_s ))
           if [ "$age" -ge "$CEILING" ]; then past=1; else past=0; fi
           age="${age}s" ;;
    esac
    if [ "$past" -eq 0 ]; then
        # The partial reading goes on the packet — the scan's counts are
        # final, and what is still running is named — and nothing else.
        curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$workdir/reading" "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
            || fail "writing the partial reading onto ${job_id:0:8} (PATCH /api/jobs/$job_id/metadata) — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
        echo "$me: not yet: $running still running on ${head:0:12}, ${age} after the PR opened (ceiling ${CEILING}s) — the partial reading is on ${job_id:0:8} (code_scanning.complete=false), read-checks stays open, and reread-publish-pr-every-15-minutes reads it again"
        exit 75
    fi
    jq -c --arg r "$running" --arg age "$age" \
        '.code_scanning.conclusion = "unfinished"
         | .code_scanning.note = "\($r) still running \($age) after the PR opened — past the ceiling, read as unfinished"' \
        "$workdir/reading" > "$workdir/reading.new" && mv "$workdir/reading.new" "$workdir/reading"
    say "$running still running ${age} after the PR opened — past the ${CEILING}s ceiling; the step completes unfinished so the judge reads it"
fi
conclusion=$(jq -r '.code_scanning.conclusion' "$workdir/reading")
# Each failing check with what its annotations named (3b) — the string
# the judge step and the publish region read.
failing=$(jq -r '[.code_scanning.failing[]
    | "\(.name): \(.conclusion)"
      + (if (.failed_checks // []) != [] then " (failed: \([.failed_checks[].name] | join(", ")))" else "" end)
      + (if .refused then " (refused: \(.refused))" else "" end)
      + (if .annotations_unread then " (its annotations could not be read)" else "" end)]
    | join("; ")' "$workdir/reading")

curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
    ${MT_HDR:+-H "$MT_HDR"} \
    --data-binary @"$workdir/reading" "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
    || fail "writing the reading onto ${job_id:0:8} (PATCH /api/jobs/$job_id/metadata) — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
say "reading written onto ${job_id:0:8} as code_scanning"

# Merge, never replace — and the SERVER merges: the verb's keys go
# through the step merge door, then a PUT carries the status alone
# (backlog e39a9d2a, Stage 2). This used to send the step's read
# metadata back beside them, because the step PUT swaps metadata
# wholesale; its end state refuses any metadata body. The keys first:
# conclusion is required at done, and the flip is where that is judged.
jq -n -c --arg c "$conclusion" --arg a "$alerts" --arg r "$rules" --arg h "$head" \
        --arg f "$failing" --arg s "$running" '
    {conclusion: $c, alerts: $a, rules: $r, head: $h, read_by: "read-publish-checks",
     failing: $f, still_running: $s}' \
    > "$workdir/payload"
printf '%s\n' '{"status":"completed"}' > "$workdir/done"
curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
    ${MT_HDR:+-H "$MT_HDR"} \
    --data-binary @"$workdir/payload" "$BASE/api/jobs/$job_id/steps/$step_id/metadata" > /dev/null 2>"$workdir/err" \
    || fail "the reading is on ${job_id:0:8} but writing it onto read-checks failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); complete the step by hand with conclusion=$conclusion alerts=$alerts rules=$rules"
curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
    ${MT_HDR:+-H "$MT_HDR"} \
    --data-binary @"$workdir/done" "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err" \
    || fail "the reading is on ${job_id:0:8} and on read-checks, but completing read-checks failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); complete the step by hand"

# What failed, by name, before the answer line — the verdict names it.
[ -z "$failing" ] || say "failing: $failing"

# The answer line, LAST: what the dispatcher rule
# complete-publish-read-checks-on-read-publish-checks-answered reads.
say "read $conclusion — $alerts alerts in $rules rules over $files files on ${head:0:12} ($pr_url)"
