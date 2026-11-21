# github-job-log.sh — read ONE GitHub Actions job log of the public
# mirror with the broker's read-only checks token, and parse it: the
# tests it says FAILED and the gate's receipt. Sourced, never run.
#
# WHY ONE FILE (backlog 8f266452, David 2026-11-20). The checks token and
# the log read were written for read-publish-checks.sh (backlog
# b7a74610), which reads only the OPEN publish packet's failing checks.
# PR #261's Gate failed on a packet already closed and nothing could read
# its log (job 135639960947): the door was missing, not the permission.
# read-github-job-log.sh is that door, and it reads with the SAME token,
# the SAME slot, the SAME render and the SAME parse — so they live here,
# once, and both verbs source this file. A second copy of the slot checks
# or of the parse would be CLAUDE.md §9a's pair, two readers drifting on
# what a live token or a failed test is.
#
# THE CALLER PROVIDES, before sourcing: `me`, `say`, `workdir` (its
# private scratch), MIRROR_SLUG and GITHUB_API, RECEIPT_STEP_RUN (the
# receipt step's run line, held equal to .github/workflows/ci.yml by each
# caller's own pin), and infra/lib/secret-header.sh and infra/lib/jq.sh
# sourced. And it sets its EXIT trap FIRST, removing the slot when
# `checks_token_taken` is 1 — before secret_header first arms, so that
# cleanup chains in front of it (lib/secret-header.sh, ORDER).
#
# Nothing here mints, places or prints a credential: the broker mints the
# token into its Secret (the rule named below), credential-render.sh
# places it in this host's slot, and it reaches curl only through a 0600
# header file.

# THE CHECKS TOKEN (backlog b7a74610, 2026-11-19). Publish PR #260's Gate
# failed its `test` check and nothing in BOSS could say which test: the
# annotations quoted a prose line (54278a24), and the job log that names
# it answers 403 to an anonymous reader. David opened it in a browser and
# pasted the name. So a failing Actions check's log is read with a
# GitHub App installation token narrowed to actions:read + metadata:read
# on the mirror, which the broker mints into Secret
# boss/github-app-algedonic-dev under one standing key when a reader's
# request is filed. The rule is NAMED, not searched for: it shares its
# credential_id with the admin rules github-act.sh selects by. The token
# reaches this host only through infra/forge/credential-render.sh, into
# the readers' one slot, and only on a run that has a log to read; it
# reaches curl only through a 0600 header file (lib/secret-header.sh).
_github_job_log_forge="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECKS_RULE="$_github_job_log_forge/../dispatcher/rules/broker-mints-the-algedonic-dev-checks-token-when-a-checks-read-is-filed.toml"
CHECKS_TOKEN_FILE="${BOSS_CHECKS_TOKEN_FILE:-/etc/boss-publish/github-app/algedonic-dev-checks.token}"
CHECKS_TOKEN_OWNER_UID="${BOSS_CHECKS_TOKEN_OWNER_UID:-0}"
CHECKS_RENDER="${BOSS_CHECKS_RENDER:-$_github_job_log_forge/credential-render.sh}"
# The token must outlive the log read: the readers' own timeout, 120 s
# (infra/ops/verbs/read-publish-checks.json, read-github-job-log.json).
CHECKS_TOKEN_MARGIN_S=120
# What one run reads, bounded, because the forge's runner is serial and
# what is recorded is budgeted. At most LOGS_PER_RUN logs (the mirror runs
# one Gate job; any further one is stated not read), each at most
# LOG_MAX_BYTES within LOG_READ_SECONDS. Recorded per log: the first
# FAILED_TESTS_MAX failed tests (the rest counted), and the receipt whole
# when it encodes within RECEIPT_ALLOWANCE bytes — over it, stated not
# recorded with its size, its failed checks still named. The log itself
# is never stored: GitHub keeps it, and the reading says where.
LOGS_PER_RUN=2
LOG_MAX_BYTES=67108864
LOG_READ_SECONDS=40
FAILED_TESTS_MAX=50
RECEIPT_ALLOWANCE=32768

# THE CHECKS SLOT ON EVERY EXIT PATH (review f5a9b7f8, B1). The slot is
# on the forge's persistent disk, under /etc, so a run that renders it
# must remove it however it ends: the caller's EXIT trap calls
# drop_checks_slot once `checks_token_taken` is 1, which take_checks_token
# sets before it renders, and HUP / INT / TERM exit through that trap by
# name. A SIGKILL runs no trap: the next run removes what it left
# (drop_stale_checks_slot).
checks_token_taken=0
drop_checks_slot() { rm -f -- "$CHECKS_TOKEN_FILE" "$CHECKS_TOKEN_FILE.expires-at" "$CHECKS_TOKEN_FILE.request"; }

# drop_stale_checks_slot — a slot an earlier run left (one killed
# outright, which runs no trap) is removed at the start of a run, whether
# or not this run has a log to read: the forge's runner is serial, so no
# other run is between its render and its read. --check touches nothing.
drop_stale_checks_slot() {
    if [ -e "$CHECKS_TOKEN_FILE" ] || [ -L "$CHECKS_TOKEN_FILE" ] \
        || [ -e "$CHECKS_TOKEN_FILE.expires-at" ] || [ -e "$CHECKS_TOKEN_FILE.request" ]; then
        if drop_checks_slot 2> "$workdir/err"; then
            say "checks token: removed the slot an earlier run left at $CHECKS_TOKEN_FILE"
        else
            echo "$me: checks token: the slot an earlier run left at $CHECKS_TOKEN_FILE could not be removed — $(cat "$workdir/err")" >&2
        fi
    fi
}

# checks_token_problem — the rule the broker mints the checks token under
# must be there and be the STANDING kind (no request_id) credential-render
# renders without --request, and the render must be runnable; anything
# else is a reader with no way to a log.
checks_token_problem() {
    if [ ! -r "$CHECKS_RULE" ]; then
        echo "the broker rule that mints the checks token is not readable at $CHECKS_RULE"
    elif ! grep -qF 'handler = "credential.rotate.github-app-installation"' "$CHECKS_RULE" \
        || grep -qE '^args = .*request_id = ' "$CHECKS_RULE"; then
        echo "$CHECKS_RULE is not a standing GitHub App installation-token rule (its handler, and no request_id)"
    elif [ ! -x "$CHECKS_RENDER" ]; then
        echo "the render $CHECKS_RENDER is not executable — the token the broker mints could not reach this host's slot"
    fi
}

# checks_token_repository — the owner/repo the checks token is minted
# for, off the rule's own `verify_repo` (the token is narrowed to that
# repository and proved against it), or nothing when it cannot be read.
# boss-dispatcher's github_app_installation_token_rules holds it equal to
# the estate's declared mirror, so a reader comparing its mirror to this
# reads the one repository the token can read, spelled nowhere else.
checks_token_repository() {
    [ -r "$CHECKS_RULE" ] || return 0
    sed -n 's/.*verify_repo = "\\"\([A-Za-z0-9._-]*\/[A-Za-z0-9._-]*\)\\"".*/\1/p' "$CHECKS_RULE" \
        | awk 'NR == 1'
}

CHECKS_HDR="" CHECKS_WHY=""
# take_checks_token — in the script's own shell (secret_header's first
# call may not be in a subshell): CHECKS_HDR is the header file, or
# CHECKS_WHY says why there is none. The token is validated as
# publish-github-pr.sh validates its own: a regular file, root's, 0600 or
# 0400, its expiry beside it and live past the verb's timeout.
take_checks_token() {
    local out="" rc=0 line word mode uid expires="" expires_s token=""
    checks_token_taken=1
    CHECKS_WHY="$(checks_token_problem)"
    [ -z "$CHECKS_WHY" ] || return 0
    "$CHECKS_RENDER" --rule "$CHECKS_RULE" --dest "$CHECKS_TOKEN_FILE" > "$workdir/render" 2>&1 || rc=$?
    # The render's words, made in this script's own file: awk reads it
    # whole (no early-exiting reader), and a read that fails stops here.
    out="$(awk '{ printf "%s ", $0 }' "$workdir/render")" \
        || { CHECKS_WHY="the render's output could not be read back from $workdir/render"; return 0; }
    [ -z "$out" ] || say "checks token: ${out:0:600}"
    case "$rc" in
        0 | 1) ;;
        *) CHECKS_WHY="the render refused (exit $rc): ${out:0:300}"; return 0 ;;
    esac
    if [ ! -e "$CHECKS_TOKEN_FILE" ] && [ ! -L "$CHECKS_TOKEN_FILE" ]; then
        # The render's one line names what it found: `<file>: <word>: …`.
        line="$(awk -v k="$CHECKS_TOKEN_FILE: " 'index($0, k) { l = $0 } END { print l }' "$workdir/render")" \
            || { CHECKS_WHY="no live checks token at $CHECKS_TOKEN_FILE, and the render's output could not be read back"; return 0; }
        line="${line##*"$CHECKS_TOKEN_FILE: "}"
        word="${line%%:*}"
        [[ $word =~ ^[a-z][a-z\ ]{0,30}$ ]] || word="unstated"
        CHECKS_WHY="no live checks token at $CHECKS_TOKEN_FILE — the render found the broker's Secret '$word'. The broker mints it when a $me request is filed ($(basename "$CHECKS_RULE")), from the App's installation on the mirror's owner, which must hold Actions: Read; a mint GitHub refuses is on the dispatcher's firing record"
        return 0
    fi
    if [ -L "$CHECKS_TOKEN_FILE" ] || [ ! -f "$CHECKS_TOKEN_FILE" ]; then
        CHECKS_WHY="the token slot $CHECKS_TOKEN_FILE is not a regular file (a link is refused: it could point anywhere)"; return 0
    fi
    mode="$(stat -c %a "$CHECKS_TOKEN_FILE")" \
        || { CHECKS_WHY="cannot read the mode of the token slot $CHECKS_TOKEN_FILE"; return 0; }
    case "$mode" in
        600 | 400) ;;
        *) CHECKS_WHY="the token slot $CHECKS_TOKEN_FILE is mode '${mode}' — a token file must be 0600 or 0400"; return 0 ;;
    esac
    # The mode says who may READ it; the owner says who could have WRITTEN it.
    uid="$(stat -c %u "$CHECKS_TOKEN_FILE")" \
        || { CHECKS_WHY="cannot read the owner of the token slot $CHECKS_TOKEN_FILE"; return 0; }
    if [ "$uid" != "$CHECKS_TOKEN_OWNER_UID" ]; then
        CHECKS_WHY="the token slot $CHECKS_TOKEN_FILE is owned by uid '${uid}', not $CHECKS_TOKEN_OWNER_UID (root)"; return 0
    fi
    if [ ! -L "$CHECKS_TOKEN_FILE.expires-at" ] && [ -f "$CHECKS_TOKEN_FILE.expires-at" ]; then
        IFS= read -r expires < "$CHECKS_TOKEN_FILE.expires-at" || true
    fi
    if ! expires_s="$(date -u -d "${expires:-no expiry}" +%s 2>/dev/null)"; then
        CHECKS_WHY="no readable expiry at $CHECKS_TOKEN_FILE.expires-at ('${expires:-none}') — the render writes one beside every token it places"; return 0
    fi
    if [ "$expires_s" -le $(($(date -u +%s) + CHECKS_TOKEN_MARGIN_S)) ]; then
        CHECKS_WHY="the checks token expires at $expires, within ${CHECKS_TOKEN_MARGIN_S}s — too close to outlive this read"; return 0
    fi
    IFS= read -r token < "$CHECKS_TOKEN_FILE" || [ -n "$token" ] || true
    if [ -z "$token" ] || [[ $token =~ [[:space:]] ]]; then
        token=""
        CHECKS_WHY="the token slot $CHECKS_TOKEN_FILE does not hold one token"; return 0
    fi
    # printf inside secret_header is a builtin: the value reaches the 0600
    # file without an argv, and nothing here echoes it.
    if ! secret_header CHECKS_HDR "Authorization: Bearer $token"; then
        token="" CHECKS_HDR=""
        CHECKS_WHY="the token's header file could not be written (lib/secret-header.sh, above)"; return 0
    fi
    token=""
    say "checks token rendered (valid until $expires)"
}

# read_job_log <job id> <out> — GET the job's log with the checks token
# (take_checks_token first; CHECKS_HDR must be set) and write it to <out>
# with GitHub's framing off. 0 when read; 1 with JOB_LOG_WHY (the
# reading's words, after `not read: `) and JOB_LOG_SAID (the run's own
# line) when it was not. The raw answer is kept at <out>.raw.
#
# --proto-redir =https: GitHub answers the logs endpoint with a 302 to its
# blob store over https, and a downgrade redirect is refused by the verb
# rather than by whichever curl the forge carries (review f5a9b7f8, N2;
# curl drops a -H header on a cross-host hop, as it has since 7.58).
JOB_LOG_WHY="" JOB_LOG_SAID=""
read_job_log() {
    local id="$1" out="$2" path code crc=0 msg=""
    path="/repos/$MIRROR_SLUG/actions/jobs/$id/logs"
    JOB_LOG_WHY="" JOB_LOG_SAID=""
    : > "$out.raw"
    code=$(curl -q -sS -L --proto-redir =https --max-time "$LOG_READ_SECONDS" --max-filesize "$LOG_MAX_BYTES" \
            -o "$out.raw" -w '%{http_code}' \
            -H "accept: application/vnd.github+json" -H "$CHECKS_HDR" \
            "$GITHUB_API$path" 2>"$workdir/err") || crc=$?
    if [ "$crc" -ne 0 ]; then
        JOB_LOG_WHY="GET $path did not complete (curl exit $crc: $(head -c 200 "$workdir/err" | tr '\n' ' '))"
        JOB_LOG_SAID="its job log could not be read (curl exit $crc)"
        return 1
    fi
    if [ "$code" != 200 ]; then
        # GitHub's refusal is a JSON body naming why; anything else is
        # quoted as it came.
        if jq_doc_file "$out.raw"; then
            msg="$(jq -r '(objects | .message // empty) | tostring' "$out.raw")" \
                || msg="(GitHub's answer could not be read back)"
        fi
        msg="${msg:-$(head -c 200 "$out.raw" | tr '\n' ' ')}"
        JOB_LOG_WHY="GitHub answered $code to GET $path — $msg"
        JOB_LOG_SAID="GitHub answered $code to its job log — $msg"
        return 1
    fi
    # GitHub's framing off: the BOM, a CR, the timestamp on every line.
    LC_ALL=C sed -E -e '1s/^\xEF\xBB\xBF//' -e 's/\r$//' -e 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z ?//' \
        "$out.raw" > "$out"
}

# The failed tests of a job log already stripped of GitHub's framing, as
# TSV: test, the binary cargo was Running, the rerun selector.
failed_tests_tsv() {
    awk '
        function flush(r,   i) { for (i = 1; i <= n; i++) printf "%s\t%s\t%s\n", t[i], b[i], r; n = 0 }
        /^[[:space:]]*Running / { flush(""); bin = $0; sub(/^[[:space:]]*Running /, "", bin); sub(/ \(.*$/, "", bin); next }
        /^[[:space:]]*Doc-tests / { flush(""); bin = "doctests"; next }
        /^test .* \.\.\. FAILED$/ { s = $0; sub(/^test /, "", s); sub(/ \.\.\. FAILED$/, "", s); n++; t[n] = s; b[n] = bin; next }
        /^error: test failed, to rerun pass `/ { r = $0; sub(/^error: test failed, to rerun pass `/, "", r); sub(/`.*$/, "", r); flush(r); next }
        END { flush("") }' "$1"
}
# The receipt step's output: after its `##[group]Run …` header and that
# group's `##[endgroup]`, up to the next `##[` line or the runner's own
# post-job lines. Exit 3 when the log holds no such step.
receipt_text() {
    awk -v want="##[group]Run $RECEIPT_STEP_RUN" '
        !found && index($0, want) == 1 { found = 1; hdr = 1; next }
        hdr { if ($0 == "##[endgroup]") { hdr = 0; body = 1 }; next }
        body && (/^##\[/ || /^Post job cleanup\.$/ || /^Cleaning up orphan processes$/) { body = 0; done = 1 }
        body && !done { print }
        END { if (!found) exit 3 }' "$1"
}

# job_log_reading <stripped log> <log path> — the reading of one log as
# one JSON object on stdout: `failed_tests` (each {test, in, rerun}, the
# same test named twice once, at most FAILED_TESTS_MAX, the rest counted
# as `failed_tests_more`), and the receipt — `receipt_failed_checks` and
# `receipt` (or `receipt_unrecorded` over RECEIPT_ALLOWANCE), or
# `receipt_unread` saying why there is none.
job_log_reading() {
    local log="$1" path="$2" rrc=0 receipt='null' receipt_why="" unparsed=0 objects=""
    failed_tests_tsv "$log" > "$workdir/failed-tests.tsv"
    receipt_text "$log" > "$workdir/receipt.txt" || rrc=$?
    # The receipt is ONE JSON object. Two are not a receipt (review
    # f5a9b7f8, N1): handed to `--argjson r` below they ended the run under
    # `set -e` with no reading landed and only jq's words to say why, so
    # the count is taken first and anything but one is stated, not taken.
    # Output that is not JSON at all is named as such (unparsed), never
    # read as a count of nothing.
    objects="$(jq -s '[.[] | objects] | length' "$workdir/receipt.txt" 2>/dev/null)" || unparsed=1
    if [ "$rrc" -eq 3 ]; then
        receipt_why="the log holds no step headed Run $RECEIPT_STEP_RUN"
    elif [ "$unparsed" -eq 0 ] && [ "$objects" = 1 ] \
        && receipt="$(jq -c -s 'first(.[] | objects)' "$workdir/receipt.txt" 2>/dev/null)"; then
        :
    else
        receipt='null'
        case "$unparsed:${objects:-empty}" in
            0:[2-9] | 0:[1-9][0-9]*) receipt_why="the receipt step printed $objects JSON objects, not one — none is taken as the gate's receipt" ;;
            1:*) receipt_why="the receipt step printed no receipt (its output is not JSON): $(head -c 200 "$workdir/receipt.txt" | tr '\n' ' ')" ;;
            *) receipt_why="the receipt step printed no receipt: $(head -c 200 "$workdir/receipt.txt" | tr '\n' ' ')" ;;
        esac
    fi
    jq -R -s -c --argjson max "$FAILED_TESTS_MAX" --argjson r "$receipt" --arg why "$receipt_why" \
            --argjson allow "$RECEIPT_ALLOWANCE" --arg p "$path" '
        [split("\n")[] | select(length > 0) | split("\t")
         | {test: .[0]} + (if (.[1] // "") != "" then {in: .[1]} else {} end)
                        + (if (.[2] // "") != "" then {rerun: .[2]} else {} end)] as $all
        # The same test named twice (a binary re-run) is one entry.
        | [range($all | length) as $i | select([$all[:$i][] == $all[$i]] | any | not) | $all[$i]] as $t
        | {failed_tests: $t[:$max]}
        + (if ($t | length) > $max then {failed_tests_more: (($t | length) - $max)} else {} end)
        + (if $r == null then {receipt_unread: $why}
           else {receipt_failed_checks: [$r.checks[]? | select(.result == "fail") | .name]}
                + (($r | tojson | utf8bytelength) as $n
                   | if $n <= $allow then {receipt: $r}
                     else {receipt_unrecorded: "not recorded: \($n) bytes, over the allowance of \($allow); GET \($p) holds it"} end)
           end)' "$workdir/failed-tests.tsv"
}
