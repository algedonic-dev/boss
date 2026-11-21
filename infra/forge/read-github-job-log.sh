#!/usr/bin/env bash
#
# read-github-job-log — read the public mirror's GitHub Actions job log
# and say which tests failed, onto the ops-request that asked.
#
#   read-github-job-log.sh job <job id>   one job's log
#   read-github-job-log.sh run <run id>   the run's jobs, and the logs of
#                                         at most two that did not pass
#   read-github-job-log.sh --check        tools and addresses, no network
#
# THE GAP (backlog 8f266452, David 2026-11-20: can we see GitHub failures
# better?). PR #261 (publish fbe2e977) failed its mirror Gate — check run
# and job 135639960947, run 44546952203 — and nothing in BOSS could name
# the failing test: its annotations quote only a prose line (54278a24),
# and its job log answers 403 to an anonymous reader. The algedonic-dev
# App already holds Actions: read, so no permission was missing; a DOOR
# was. The one reader holding a token, read-publish-checks, reads only
# the OPEN publish packet's failing checks, so a closed packet's failure,
# or any other run, had no path in. This is that path.
#
# NO NEW CREDENTIAL. It reads with the broker's read-only checks token
# (actions:read + metadata:read on the mirror alone), the same Secret
# key, minted by the same rule when this verb's request is filed, and
# rendered, checked and parsed by the same definition read-publish-checks
# sources: infra/forge/lib/github-job-log.sh. The token reaches curl only
# through a 0600 header file, only for a log read, and the slot it is
# rendered into is removed on every way the run ends. Nothing here mints,
# places or prints a credential.
#
# ONE REPOSITORY. There is no repository parameter: the repository is the
# mirror this host declares (BOSS_MIRROR_SLUG, rendered from
# infra/estate/estate.toml into /etc/boss/sor.env), and the verb REFUSES
# (exit 2) before any read when that is not the repository the checks
# token is minted for — the rule's own verify_repo, which boss-dispatcher's
# github_app_installation_token_rules holds equal to the declared mirror.
# The token's `repositories` narrowing is the second wall: it cannot read
# another repository's logs whatever a request names. An id is digits
# (^[0-9]{1,20}$), checked by the runner's pattern and again here.
#
# WHAT IT ANSWERS, on the ops-request and nowhere else, bounded: per job
# its id, name and conclusion; every test the log says FAILED, at most
# FAILED_TESTS_MAX (the rest counted), with its binary and the selector
# that reruns it; the checks the gate's receipt names failed; and the log
# around the FIRST failure — TAIL_BEFORE lines before the first `… FAILED`
# / `error:` / `##[error]` line through TAIL_AFTER after, held to
# TAIL_MAX_BYTES. Whatever could not be had is stated with why. A run's
# job list is read anonymously (the mirror is public); only a log read
# carries the token, and a run with nothing failing never renders it.
#
# EXIT. 0 when every log it set out to read was read; 1 when one was not
# (no live token, a refused or failed read), each stated above the last
# line; 2 a refusal, nothing read. A cold filing can reach the forge's
# serial runner before its mint lands: it then says it held no live token,
# and a refile reads the log (the rule's 40-minute refresh).
#
# Runs as root under the ops-runner with NO HOME: nothing reads $HOME.
set -euo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"

# THE MIRROR — from the one place it is spelled, as read-publish-checks
# reads it: an explicit BOSS_MIRROR_SLUG first (how the tests point the
# verb at a fixture), else infra/lib/sor.sh.
MIRROR_SLUG="${BOSS_MIRROR_SLUG:-}"
if [ -z "$MIRROR_SLUG" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_SLUG
    MIRROR_SLUG="$BOSS_MIRROR_SLUG"
fi
GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"
# The receipt step, as GitHub heads it in a job log: `##[group]Run ` and
# the step's `run:` line in .github/workflows/ci.yml, held equal by
# read_github_job_log_sh::the_receipt_step_the_verb_reads_is_the_one_the_mirror_runs.
RECEIPT_STEP_RUN='cat .gate-receipt.json || echo "no receipt — the gate stopped before it could write one"'
# The tail around the first failure: lines before it, lines after it, and
# the bytes the whole may weigh. Leading lines go first when it is over,
# then trailing ones; the failure line stays.
TAIL_BEFORE=40
TAIL_AFTER=20
TAIL_MAX_BYTES=8192
# How many failed tests the last line names; every one is named above it.
LAST_LINE_TESTS=5

me="read-github-job-log"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

# The checks token, its slot, the log read and the parse (one definition
# with read-publish-checks). Sets checks_token_taken=0.
# shellcheck source=infra/forge/lib/github-job-log.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/github-job-log.sh"

workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}" >&2; exit 1; }
# The slot on every exit path (lib/github-job-log.sh): set BEFORE
# secret_header first arms, so its cleanup chains in front of this one.
trap 'rm -rf "$workdir"; [ "$checks_token_taken" != 1 ] || drop_checks_slot' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# shellcheck source=infra/lib/secret-header.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/secret-header.sh"

# The arguments, judged before anything else is read.
mode="${1:-}"
case "$mode" in
    --check)
        [ $# -eq 1 ] || refuse "--check takes no id (got $# arguments); nothing was read" ;;
    job | run)
        [ $# -eq 2 ] || refuse "$mode takes exactly one id (got $(($# - 1))); nothing was read"
        [[ $2 =~ ^[0-9]{1,20}$ ]] \
            || refuse "the $mode id is not a GitHub id (it must match ^[0-9]{1,20}\$); nothing was read" ;;
    *)
        refuse "the mode is job <id>, run <id> or --check; nothing was read" ;;
esac
id="${2:-}"

# THE ONE REPOSITORY: the mirror this host declares must be the one the
# checks token is minted for.
token_repo="$(checks_token_repository)"
mirror_problem=""
if [ -z "$token_repo" ]; then
    mirror_problem="the repository the checks token is minted for could not be read (verify_repo in $(basename "$CHECKS_RULE"))"
elif [ "$MIRROR_SLUG" != "$token_repo" ]; then
    mirror_problem="the mirror this host declares ('$MIRROR_SLUG', BOSS_MIRROR_SLUG) is not the repository the checks token is minted for ('$token_repo', verify_repo in $(basename "$CHECKS_RULE")); this verb reads that one repository and no other"
fi

if [ "$mode" = "--check" ]; then
    echo "$me --check"
    echo "  mirror  : $MIRROR_SLUG via $GITHUB_API — the one repository read; no repository is a parameter"
    echo "  reads   : job <id>, one log; run <id>, the run's jobs (anonymous) and the logs of at most ${LOGS_PER_RUN} that did not pass"
    echo "  token   : the read-only checks token minted by $(basename "$CHECKS_RULE"), rendered to $CHECKS_TOKEN_FILE at run time — not here"
    echo "  answers : per job its id, name and conclusion, up to ${FAILED_TESTS_MAX} failed tests, the receipt's failed checks, and ${TAIL_BEFORE} lines before the first failure through ${TAIL_AFTER} after, at most ${TAIL_MAX_BYTES} bytes — never the token"
    rc=0
    for tool in curl jq awk sed; do
        command -v "$tool" >/dev/null 2>&1 || { echo "$me: missing tool on PATH: $tool" >&2; rc=1; }
    done
    if [ -n "$mirror_problem" ]; then echo "$me: $mirror_problem" >&2; rc=1; fi
    problem="$(checks_token_problem)"
    if [ -n "$problem" ]; then echo "$me: $problem" >&2; rc=1; fi
    if [ "$rc" -eq 0 ]; then
        echo "$me: --check ok"
        exit 0
    fi
    echo "$me: --check FAILED — see above" >&2
    exit 1
fi
[ -z "$mirror_problem" ] || refuse "$mirror_problem — nothing was read"

# ---------------------------------------------------------------------
# A run.
# ---------------------------------------------------------------------
drop_stale_checks_slot

# One anonymous GET against the mirror: the body to stdout, curl's words
# to $workdir/err. The job list and a job's record are public; only a
# log read (read_job_log) carries the token, and it never comes here.
gh_get() {
    curl -q -fsS --max-time 20 -H "accept: application/vnd.github+json" \
        "$GITHUB_API/repos/$MIRROR_SLUG/$1" 2>"$workdir/err"
}
err_text() { head -c 200 "$workdir/err" | tr '\n' ' '; }

# job_log_tail <stripped log> — the lines around the FIRST failure, with
# a header line, each line control-free and the whole within
# TAIL_MAX_BYTES. A log with no failure line gives its last lines.
job_log_tail() {
    local log="$1" first total from to
    read -r first total < <(LC_ALL=C awk '
        f == 0 && (/ \.\.\. FAILED$/ || /^error(\[[A-Za-z0-9]+\])?: / || /^##\[error\]/) { f = NR }
        END { print f + 0, NR + 0 }' "$log")
    if [ "$total" -eq 0 ]; then
        echo "  tail: the log is empty"
        return 0
    fi
    if [ "$first" -gt 0 ]; then
        from=$((first > TAIL_BEFORE ? first - TAIL_BEFORE : 1))
        to=$((first + TAIL_AFTER < total ? first + TAIL_AFTER : total))
    else
        from=$((total > TAIL_BEFORE + TAIL_AFTER ? total - TAIL_BEFORE - TAIL_AFTER : 1))
        to=$total
    fi
    sed -n "${from},${to}p" "$log" > "$workdir/tail.txt"
    jq -R -s -r --argjson from "$from" --argjson first "$first" --argjson total "$total" \
        --argjson cap "$TAIL_MAX_BYTES" '
        def bytes: map(utf8bytelength + 1) | add // 0;
        # Terminal escapes and control characters do not reach the record.
        [rtrimstr("\n") | split("\n")[]
         | gsub("\u001b\\[[0-9;?]*[ -/]*[@-~]"; "") | gsub("[\\x00-\\x08\\x0b-\\x1f\\x7f]"; "")] as $l
        # The anchor: the failure line, or the last line when there is none.
        | (if $first > 0 then $first - $from else ($l | length) - 1 end) as $a
        | {l: $l, a: $a, lo: $from}
        | until((.l | bytes) <= $cap or (.l | length) <= 1;
            if .a > 0 then {l: .l[1:], a: (.a - 1), lo: (.lo + 1)}
            elif .a < (.l | length) - 1 then {l: .l[:-1], a: .a, lo: .lo}
            else . end)
        | .l |= map(until(utf8bytelength <= $cap - 1; .[0:(length * 9 / 10 | floor)]))
        | "  tail: log lines \(.lo)-\(.lo + (.l | length) - 1) of \($total)"
          + (if $first > 0 then ", the first failure at line \($first)"
             else " — no FAILED or error: line in the log, so its last lines" end)
          + " (at most \($cap) bytes)",
          (.l[] | "  | " + .)' "$workdir/tail.txt"
}

# The answer, gathered as it goes: how many logs were read and how many
# were meant to be and were not (with the first why), and every failed
# test named.
logs_read=0
unread=0
unread_why=""
: > "$workdir/named"

# read_one <job id> <header> — one job: its header, then its log's
# reading, or why it was not read.
read_one() {
    local job="$1" header="$2" path
    path="/repos/$MIRROR_SLUG/actions/jobs/$job/logs"
    echo "$header"
    if [ "$logs_read" -ge "$LOGS_PER_RUN" ]; then
        echo "  not read: this run reads at most $LOGS_PER_RUN job logs (the forge's runner is serial) — GET $path holds it; file job $job to read it"
        return 0
    fi
    [ "$checks_token_taken" -eq 1 ] || take_checks_token
    if [ -z "$CHECKS_HDR" ]; then
        echo "  not read: $CHECKS_WHY — file it again once the broker has minted it"
        unread=$((unread + 1)); unread_why="${unread_why:-job $job: $CHECKS_WHY}"
        return 0
    fi
    logs_read=$((logs_read + 1))
    if ! read_job_log "$job" "$workdir/log-$job.txt"; then
        echo "  not read: $JOB_LOG_WHY"
        unread=$((unread + 1)); unread_why="${unread_why:-job $job: $JOB_LOG_WHY}"
        return 0
    fi
    job_log_reading "$workdir/log-$job.txt" "$path" > "$workdir/reading-$job.json"
    jq -r '
        def clean: gsub("[\\x00-\\x1f\\x7f]"; " ");
        (if (.failed_tests | length) == 0 then "  failed tests: none named (no `test … FAILED` line in the log)"
         else (.failed_tests[]
               | "  failed test: \(.test | clean)"
                 + (if .in then " — in \(.in | clean)" else "" end)
                 + (if .rerun then " — rerun: cargo test \(.rerun | clean) \(.test | clean)" else "" end))
         end),
        (if .failed_tests_more then "  failed tests not listed: \(.failed_tests_more) more" else empty end),
        (if .receipt_unread then "  receipt: unread — \(.receipt_unread | clean)"
         else "  receipt: failed checks: \(if (.receipt_failed_checks | length) == 0 then "none" else (.receipt_failed_checks | map(clean) | join(", ")) end)"
         end)' "$workdir/reading-$job.json"
    jq -r '.failed_tests[].test | gsub("[\\x00-\\x1f\\x7f]"; " ")' "$workdir/reading-$job.json" >> "$workdir/named"
    job_log_tail "$workdir/log-$job.txt"
}

# The failed tests named, for the last line: the first LAST_LINE_TESTS,
# and how many more. A test two jobs both name is counted once.
named_line() {
    jq -R -s -r --argjson n "$LAST_LINE_TESTS" '
        reduce (split("\n")[] | select(length > 0)) as $x ([]; if any(.[]; . == $x) then . else . + [$x] end)
        | . as $t
        | if ($t | length) == 0 then "no failed test named in what was read"
          else "\($t | length) failed test(s): \($t[:$n] | join(", "))"
               + (if ($t | length) > $n then ", and \(($t | length) - $n) more above" else "" end)
          end' "$workdir/named"
}

if [ "$mode" = "job" ]; then
    say "mirror $MIRROR_SLUG — job $id"
    # The job's own record is public; one that cannot be read leaves its
    # name and conclusion unread, never the log.
    if gh_get "actions/jobs/$id" > "$workdir/job.json" \
        && jq_doc_file "$workdir/job.json" \
        && jq -e 'type == "object"' "$workdir/job.json" > /dev/null 2>&1; then
        header="$(jq -r --arg id "$id" '
            def clean: gsub("[\\x00-\\x1f\\x7f]"; " ");
            "job \($id): \((.name // "unnamed") | tostring | clean) — \((.conclusion // .status // "unstated") | tostring | clean)"
            + (if .run_id then " (run \(.run_id))" else "" end)' "$workdir/job.json")"
    else
        header="job $id: its name and conclusion unread — GET actions/jobs/$id: $(err_text)"
    fi
    read_one "$id" "$header"
    [ "$unread" -eq 0 ] || fail "job $id: its log was not read — $unread_why"
    say "job $id read — $(named_line)"
    exit 0
fi

# run <id>: the run's jobs, anonymously; the logs of the ones that did
# not pass, at most LOGS_PER_RUN.
say "mirror $MIRROR_SLUG — run $id"
gh_get "actions/runs/$id/jobs?per_page=100" > "$workdir/jobs.json" \
    || fail "GET actions/runs/$id/jobs — $(err_text); nothing was read"
# An empty 200 is no job list; `jq -e` alone reads silence as a pass
# (d96e38ab).
jq_doc_file "$workdir/jobs.json" && jq -e '.jobs | type == "array"' "$workdir/jobs.json" > /dev/null 2>&1 \
    || fail "GET actions/runs/$id/jobs answered something that is not a job list: $(head -c 200 "$workdir/jobs.json" | tr '\n' ' ')"
# Not passed: completed, and not success / neutral / skipped — the
# predicate read-publish-checks judges a failing check by.
jq -r '
    def clean: gsub("[\\x00-\\x1f\\x7f]"; " ");
    .jobs[] | select(.status == "completed"
                     and ((.conclusion // "none") | IN("success", "neutral", "skipped") | not))
    | [(.id | tostring), ((.name // "unnamed") | tostring | clean), ((.conclusion // "none") | tostring | clean)]
    | @tsv' "$workdir/jobs.json" > "$workdir/failing.tsv"
listed=$(jq '.jobs | length' "$workdir/jobs.json")
total=$(jq --argjson l "$listed" '.total_count // $l' "$workdir/jobs.json")
running=$(jq '[.jobs[] | select(.status != "completed")] | length' "$workdir/jobs.json")
not_passed=$(awk 'END { print NR }' "$workdir/failing.tsv")
echo "run $id: $total job(s), $not_passed not passed$([ "$running" -eq 0 ] || echo ", $running still running")$([ "$listed" -ge "$total" ] || echo " (the first $listed listed)")"
while IFS=$'\t' read -r job name conclusion; do
    [[ $job =~ ^[0-9]{1,20}$ ]] || continue
    read_one "$job" "job $job: $name — $conclusion"
done < "$workdir/failing.tsv"
[ "$unread" -eq 0 ] || fail "run $id: $unread job log(s) not read — $unread_why"
if [ "$not_passed" -eq 0 ]; then
    say "run $id — $total job(s), none failed; no log to read"
else
    say "run $id — $not_passed job(s) not passed, $logs_read log(s) read — $(named_line)"
fi
