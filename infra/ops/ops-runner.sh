#!/bin/sh
# ops-runner — answer ops-request packets filed against this host.
#
# The PULL half of host observability (packet 729329c6). David: "I am
# really tired of the copy pasting after sshing." A debug read is a
# packet: somebody files an `ops-request` Job naming a host and an
# allowlisted read-only verb; this runner, on that host under a
# ~1-minute systemd timer, polls the system of record, executes the
# verb, and completes the packet's `execute` step with stdout/stderr
# and the exit code. The operator stops being the transport.
#
# ## Security posture (phase 1)
#
# - READ-ONLY. Every verb in the allowlist is a read; mutating verbs
#   are phase 2, behind per-verb policy, and are NOT in this script's
#   world at all.
# - THE ALLOWLIST IS THE AUTHORITY: infra/ops/verbs/<name>.json, one
#   file per verb, in-tree, reviewed, versioned (the verb's name IS
#   the file name; infra/ops/verbs-allowlist.sh assembles the
#   directory — 5086842d, after two verb cars collided on the one file
#   it used to be). A packet carries only a verb NAME and args; the
#   command words come from the file. The runner never executes a
#   packet-supplied string.
# - NO SHELL INTERPOLATION OF ARGS, EVER. The runner builds an argv
#   ARRAY (`set -- word word ...`) and execs it directly — no sh -c,
#   no eval, nothing packet-supplied ever becomes program text. Args
#   are validated against strict per-param patterns first (no
#   whitespace, no leading '-'), which is also what makes the
#   newline-split of jq's argv output below exact rather than hopeful.
# - A VERB SERVES NAMED HOSTS, and a host serves only the verbs that
#   name it: every allowlist entry declares `hosts` (estate node ids),
#   and a verb whose `hosts` does not list this runner's HOST_ID is
#   REFUSED — absent `hosts` included, so a new verb cannot reach a
#   host by forgetting to say. Not a privilege boundary (every runner
#   reads the same file) but a door that tells the truth about what it
#   opens: 11 of the 16 verbs name a script under the FORGE's checkout,
#   so when boss-gcp got a runner (2026-09-11) the unscoped allowlist
#   would have advertised a vocabulary of which 11 could only fail on
#   ENOENT. An exec failure is not a verdict; a refusal naming the
#   verb, this host and the hosts that verb does serve is.
# - Anything else — unknown verb, wrong arg shape, pattern miss —
#   drives the packet to its `refused` terminal with the reason in
#   `output` AND, named, in `reason` — the same text the journal line
#   carries, so a refusal never has to be re-derived from the host's
#   journal (6964f9e8; CLAUDE.md §Diagnosis: a verdict must name what
#   failed). Refusing loudly in the SoR beats guessing.
# - A param may be a LITERAL LIST (`one_of`) instead of a pattern: the
#   packet selects one of the reviewed words in the allowlist, and
#   nothing packet-supplied reaches the argv — so a word there may
#   lead with '-' (publish-github-pr's `--check`). With `optional`,
#   an absent arg DROPS its placeholder word from the argv rather
#   than passing an empty one.
# - A VERB THAT DECLARES `requires_approval` RUNS ONLY UNDER A PASSKEY
#   APPROVAL OF A RENDERED PLAN (design 17835005, decided by David
#   2026-09-21; backlog fd7090cc). Until 2026-09-24 this runner refused
#   every such verb, because nothing issued an approval. Now, in order:
#   the verb names a read-only `plan_verb`, which this runner renders
#   on the host and writes onto the request's `approve` step; David's
#   passkey signs that step, and a presence stamp binds
#   `step_shape_hash(title, metadata)` — the plan bytes; `execute`
#   runs only once the approve step is COMPLETED with a presence stamp
#   for every required role, bound to the step's CURRENT shape, signed
#   within APPROVAL_TTL_S; `execute` is claimed `active` before the
#   argv runs, and an `active` one is never run again; and the write
#   gets sha256 of the SIGNED plan as its last param, `plan_sha256`, so
#   its own script re-renders and refuses a state that moved since the
#   signature. q2 as David ruled it: the runner trusts the system of
#   record's sign-off record and nothing else — a metadata flag, a
#   packet-supplied hash, a completed step with no stamp are all
#   refusals, each named on the request.
#   Tightened after the car's security review (2026-09-24): the stamp
#   must be by an employee the verb file names in `approvers` (design
#   03451237 q2 — a named list, not a role); the plan is signed with
#   the verb, host and args it was rendered for and the hash of the
#   bytes this runner rendered, and the request must still match them;
#   the plan verb is re-run before the claim and the signed plan must be
#   its bytes exactly; the record is re-read immediately before a claim
#   that is a compare-and-set through the claim door; an execute left
#   `active` records "claimed, outcome unknown" and is never called
#   refused; and an arg carrying a control character is refused.
#   And after its re-review (2026-09-25): only an UNHELD execute is
#   claimed — ops-request declares execute `claimable`, so the
#   dispatcher no longer nominates it to the agent executor, whose hold
#   the claim door would refuse this runner on — a held one is refused
#   by name, and "claimed, outcome unknown" names only a holder shaped
#   like one of this runner's own passes.
#   And a plan that names NOTHING TO DO asks nobody (backlog b2f78bb9):
#   a plan verb may declare `nothing_to_do`, the regex its whole plan
#   matches when it names no change, and a matching plan closes the
#   request through its `nothing-to-do` terminal with the plan on it,
#   never reaching the approve step (see `render_plan`).
#
# ## Behaviour
#
# - Polls open ops-request Jobs whose metadata.host equals HOST_ID,
#   exactly. A packet for a host with no runner is nobody's to guess
#   at; it sits open and visibly unanswered.
# - READS ITS OWN QUEUE before it walks it — depth and the oldest
#   packet's wait — on one journal line per run, and writes what each
#   answered request waited onto that request. The walk is serial and
#   has no per-verb fairness, so this depth is the only real bound on
#   raising any probe cadence further (backlog 1ffb3305). The depth is
#   the server's `total` for this host, and a reading that could not
#   see the whole queue prints `>=` and `truncated:` instead of passing
#   a page off as the count (2cfb4562).
# - Executes with a wall-clock timeout (OPS_TIMEOUT, default 30s) and
#   an output cap (OPS_OUTPUT_CAP, default 100KB); both truncations
#   are LOUD — a marker line in the recorded output says what was cut.
# - Completes a step in two writes, sending only what it changes: its
#   keys through the step merge door (`PATCH .../steps/{id}/metadata`,
#   merged against the row as it stands), THEN the status alone
#   (`PUT .../steps/{id}`). The PUT refuses any metadata body now
#   (e39a9d2a, Stage 2's last car); when it swapped `metadata`
#   wholesale, a metadata body had to carry every key — it used to be
#   the keys READ at poll time plus the runner's own, a copy that could
#   be stale by the time it landed (the boss-step.sh lesson; backlog
#   2aa2b19e, the gate runner's shape since e39a9d2a). The status-only
#   PUT carries nothing to go stale, so a lost step race is answered by
#   sending it once more. An execute that already carries THIS host's
#   answer — its keys landed, its status did not — is finished with the
#   status alone on the next pass and its verb is never run again
#   (backlog 1434e6e3). A write the server answers with a 5xx is a
#   refusal and is recorded; only no answer at all goes unrecorded.
# - Records the verb's exit ONCE, as `exit_code` on that step.
#   `answered` means the verb ran; `exit_code` says how it went, and
#   every reader — `boss ops --wait`, the answered-ops-request judges,
#   the yard — takes it from there (50fede8b; see the merge door below
#   for the request-level copy this replaced).
# - Judges an exit 0 against the verb's declared `effect` and records
#   the verdict beside the exit — `effect`, `effect_unproven` or
#   `effect_unread` — because an exit 0 is not a proof that the verb
#   changed what it exists to change (backlog fdbb447e part 1; see the
#   judgement below and infra/ops/verbs/README.md §Effect).
# - Records how long the verb RAN, as `duration_ms` beside that exit
#   (b7bfe821). The request's own stamps span this runner's poll
#   latency — up to a minute — so they cannot cost a verb; this is
#   taken around the exec. A refusal ran nothing and records neither.
# - A per-packet problem (refusal, missing step) never kills the loop;
#   a transport failure to the SoR fails the unit loudly, systemd
#   records it red, and the same loud-local-failure posture as the
#   estate observers applies (3ddd8333: silent-on-curl-failure does
#   not get a second landing).
# - A completion the SERVER REFUSES is written onto the request as
#   `completion_refused` (status, the server's words, first/last,
#   count, whether the verb ran), so a jammed packet names its own
#   cause (post-mortem 3c3b202c: two hours of 409s that only a journal
#   ever saw, and only as a number).
# - No maintenance-wrap packet pair, deliberately: this fires every
#   minute, and a packet per firing would drown the board. Its
#   product IS packets — the ops-requests it answers — and its
#   failure modes are a red systemd unit plus a filed packet aging
#   visibly unanswered.
#
# ## Env
#
#   HOST_ID        (required) estate node id this runner answers for
#   BOSS_JOBS_URL  (required, no default — see below) the SoR
#   OPS_VERBS_DIR  (default: verbs/ beside this script) — one file per verb
#   OPS_TIMEOUT    (default 30) seconds before a verb is killed, unless
#                  the verb's allowlist entry declares its own `timeout`
#   OPS_OUTPUT_CAP (default 102400) bytes of output kept
#   BOSS_MACHINE_TOKEN_DIR (default /etc/boss/machine-token) the mounted
#     machine token; its `current` slot rides as x-boss-machine-token in
#     a 0600 header file, never in argv, and only to an estate host —
#     loopback or BOSS_MACHINE_TOKEN_HOSTS (infra/lib/secret-header.sh
#     machine_token_header; design 6805c764 car 4)
#   BOSS_RUNNER_CREDENTIAL_FILE (default /etc/boss/ops-runner.credential)
#     this host's runner credential, presented as x-boss-runner-credential
#     the same way; absent, none is sent (design f623e425; see below)
#
# All JSON parsing is jq with payloads on stdin or via --arg/--argjson/
# --rawfile, never spliced into the program text (the boss-step.sh /
# feedback-queue.sh rule).
set -u

: "${HOST_ID:?HOST_ID is required and must match the estate node id}"

# WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION — same
# refusal as boss-step.sh. Defaulting to 127.0.0.1 is how nightly
# maintenance packets spent weeks landing on a non-authoritative
# instance (2026-08-17). A runner with no system of record configured
# refuses, loudly, and systemd records a failed unit — which is a
# state somebody notices.
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$(basename "$0"): BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    Defaulting to 127.0.0.1 is how nightly maintenance packets spent weeks" >&2
    echo "    landing on a non-authoritative instance (2026-08-17). Name the system of" >&2
    echo "    record explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://<jobs-api-host>:<port> $(basename "$0")" >&2
    echo "    The installed unit pins it on the Exec line (see boss-ops-runner.service)." >&2
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="$BOSS_JOBS_URL"

VERBS_DIR="${OPS_VERBS_DIR:-$(dirname "$0")/verbs}"
# THE CHECKOUT THIS RUNNER IS PART OF. A verb's script is named in its
# verb file RELATIVE to the repo (infra/forge/disk-report.sh) and resolved
# here, against the checkout the runner itself runs from — never an
# absolute path baked into the allowlist. Until 2026-09-12 eleven of
# sixteen verbs carried /home/david/boss/…, the FORGE's checkout path,
# so none could run on boss-gcp (/opt/boss) and every reader of the
# file — two lints, the test harness — substituted that prefix for its
# own: one assumption in four places (66077f9c, CLAUDE.md §9a). A bare
# command (systemctl, df) stays a bare command, resolved on PATH.
OPS_REPO_ROOT="${OPS_REPO_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
OPS_TIMEOUT="${OPS_TIMEOUT:-30}"
OPS_OUTPUT_CAP="${OPS_OUTPUT_CAP:-102400}"
# A STEP WRITE THE SYSTEM OF RECORD DID NOT ANSWER IS TRIED AGAIN (the
# review of car 24eb9471). A completion is two writes (see
# `complete_step`), so a jobs-API roll between them — ~30-90s — used to
# strand the keys on an open step with no status. Each step write is
# bounded by OPS_WRITE_MAX_TIME seconds and, on no answer or a 5xx,
# retried after each OPS_WRITE_BACKOFF delay in turn: the gate
# runner's report schedule, cut to what one runner pass can afford.
OPS_WRITE_MAX_TIME="${OPS_WRITE_MAX_TIME:-20}"
OPS_WRITE_BACKOFF="${OPS_WRITE_BACKOFF:-5 10 20 30 45}"
# ...within ONE budget for the whole PASS, not one per write (the
# re-review of car 59a6ade9): a dark system of record met by a pass with
# many requests would otherwise stretch the pass past APPROVAL_TTL_S and
# let approvals it has not reached yet expire behind it. Seconds spent
# retrying (the failed tries and the waits); once spent, a write that
# gets no answer or a 5xx is taken as it stands. It stays below
# APPROVAL_TTL_S, which clamps it just after its definition; pinned by
# ops_runner_sh.rs.
OPS_WRITE_RETRY_BUDGET="${OPS_WRITE_RETRY_BUDGET:-240}"
retry_spent=0

workdir=$(mktemp -d) || exit 1
trap 'rm -rf "$workdir"' EXIT

# THE ALLOWLIST IS ASSEMBLED ONCE PER RUN from the directory, by the one
# script every sh/python reader shares, into a file the per-packet jq
# below reads. A directory that cannot be loaded — missing, empty, a
# file that is not one verb — is a refusal to run at all (EX_CONFIG,
# the fault named by the assembler on stderr), never a partial
# allowlist that refuses every packet as "unknown verb".
VERBS_FILE="$workdir/allowlist.json"
if ! sh "$(dirname "$0")/verbs-allowlist.sh" "$VERBS_DIR" > "$VERBS_FILE"; then
    echo "ops-runner: allowlist directory $VERBS_DIR could not be loaded — refusing to run" >&2
    exit 78
fi

# An automated answer should read as automation in the audit trail.
ACTOR="${BOSS_OPS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# ---- the approval channel (design 17835005; backlog fd7090cc) -------
#
# HOW LONG A PASSKEY APPROVAL STAYS GOOD, counted from the presence
# stamp's `stamped_at` — the moment the passkey signed. q4 asked for an
# approval that is single-use AND time-boxed, "minutes". Ten: the runner
# polls about once a minute, so a signature is normally acted on inside
# two, and ten leaves room for a runner that missed a pass or two while
# still refusing a signature left lying from an earlier sitting. A
# constant here, deliberately not an env knob: a window the unit could
# widen would be a bypass with a config file for a key. Pinned by
# ops_runner_approval_sh.rs (`expires after 600s`).
APPROVAL_TTL_S=600
# The write retry budget of one pass stays below the approval window
# (see OPS_WRITE_RETRY_BUDGET): a value that is not a count, or one at or
# past the window, is cut to half the window.
case ${OPS_WRITE_RETRY_BUDGET:-empty} in
    empty|*[!0-9]*) OPS_WRITE_RETRY_BUDGET=$((APPROVAL_TTL_S / 2)) ;;
esac
if [ "$OPS_WRITE_RETRY_BUDGET" -ge "$APPROVAL_TTL_S" ]; then
    OPS_WRITE_RETRY_BUDGET=$((APPROVAL_TTL_S / 2))
fi
# A stamp dated this far ahead of the host's clock is a clock nobody can
# reconcile, and an approval that cannot be dated cannot be time-boxed.
APPROVAL_SKEW_S=60

# THE SERVER'S SHAPE HASH, IN jq — `shape_hash_of <step json>`. The
# presence stamp records `boss_core::job::step_shape_hash` of the step
# it signed; recomputing it over the approve step as it stands NOW is
# what makes "the plan on the step is the plan that was signed" a check
# this runner performs rather than a belief it holds. The definition,
# its canonical form and why a disagreement refuses rather than accepts
# live in infra/lib/step-shape.sh — one copy since 2026-09-27, when the
# public-mirror publish became the second verb acting on a passkey
# (backlog 02b65d81). Pinned equal to the Rust function by the happy
# path in ops_runner_approval_sh.rs (§9a). A runner that cannot load it
# cannot verify any approval, so it refuses to run at all.
# Checked BEFORE the `.`: under /bin/sh (dash) a `.` of a missing file
# is a special-builtin error that ends the shell with its own message,
# not this one.
SHAPE_LIB="$(dirname "$0")/../lib/step-shape.sh"
if [ ! -r "$SHAPE_LIB" ]; then
    echo "ops-runner: $SHAPE_LIB is missing — without the shape hash no approval can be verified; refusing to run" >&2
    exit 78
fi
# shellcheck source=infra/lib/step-shape.sh
. "$SHAPE_LIB"

# THE MACHINE TOKEN RIDES TO curl IN A 0600 FILE, never in its argv,
# where every local user of the host reads it in ps and
# /proc/<pid>/cmdline — seven of this runner's writes did until
# 2026-09-28 (backlog 5f3ad356). Under the unit's RuntimeDirectory= the
# file lives in tmpfs systemd removes when the run stops; elsewhere in a
# private mktemp directory. Made once, here in the runner's own shell and
# after the EXIT trap above (the helper chains its cleanup in front of
# it), because the writes below run inside $(…). Checked before the `.`
# for the reason SHAPE_LIB is.
SECRET_LIB="$(dirname "$0")/../lib/secret-header.sh"
if [ ! -r "$SECRET_LIB" ]; then
    echo "ops-runner: $SECRET_LIB is missing — the machine token would have to ride in curl's command line; refusing to run" >&2
    exit 78
fi
# shellcheck source=infra/lib/secret-header.sh
. "$SECRET_LIB"
if ! machine_token_header MT_HDR "$BASE"; then
    echo "ops-runner: the machine token's header file could not be written — refusing to run" >&2
    exit 78
fi

# THIS RUNNER'S OWN CREDENTIAL (design f623e425 Q1, option A, decided by
# David 2026-09-25; backlog 6c9183de). The jobs API knows the runner by a
# credential it PRESENTS in x-boss-runner-credential — resolved by the
# server to runner:ops for THIS host — never by the id in x-boss-user,
# which any holder of the machine token can type. It rides beside the
# machine token on every request to the system of record, in its own
# 0600 header file, never in argv. The value is read from a root-only
# file the credential broker delivers (BOSS_RUNNER_CREDENTIAL_FILE); it
# never reaches a log line, and the shell variable is emptied once the
# header file holds it.
#
# IT NEVER STOPS AN ANSWER (DR rule 62dac114: no refusal on David's path).
# No file is today's state on every host — the pass runs exactly as
# before, sending none. A file that cannot be one header is said here and
# the pass goes on without it: the credential adds identity, and until a
# protocol declares a writer, identity is all it adds.
RUNNER_CREDENTIAL_FILE="${BOSS_RUNNER_CREDENTIAL_FILE:-/etc/boss/ops-runner.credential}"
RC_HDR=""
if [ -e "$RUNNER_CREDENTIAL_FILE" ]; then
    rc_value=""
    rc_why=""
    if [ ! -r "$RUNNER_CREDENTIAL_FILE" ]; then
        rc_why="this runner cannot read it"
    elif ! rc_value=$(cat "$RUNNER_CREDENTIAL_FILE" 2>"$workdir/rc-err"); then
        rc_value=""
        rc_why="reading it failed: $(cat "$workdir/rc-err")"
    elif [ -z "$rc_value" ]; then
        rc_why="it is empty"
    fi
    if [ -n "$rc_why" ]; then
        echo "ops-runner: the runner credential at $RUNNER_CREDENTIAL_FILE is NOT presented ($rc_why); this pass answers without it" >&2
    elif ! secret_header RC_HDR "x-boss-runner-credential: $rc_value" 2>"$workdir/rc-err"; then
        RC_HDR=""
        echo "ops-runner: the runner credential at $RUNNER_CREDENTIAL_FILE is NOT presented ($(cat "$workdir/rc-err")); this pass answers without it" >&2
    fi
    rc_value=""
fi

# THE CLAIMANT A PASS SIGNS ITS CLAIM AS, and the test for one — side by
# side, so the shape is written once. The claim door is idempotent for
# its holder, so each pass is a different claimant: this runner's
# account, its host, the moment and the pid. `is_runner_pass` answers
# "yes" only for exactly that shape on THIS host — the only holder whose
# claim can have run a write here, and so the only one a "claimed,
# outcome unknown" record may name (security re-review, 2026-09-25).
# `\z`, not `$`: Oniguruma reads `$` as "end, or before a final newline".
mint_claimant() {
    printf '%s:%s:%s-%s' "$ACTOR" "$HOST_ID" "$(date -u +%Y%m%dT%H%M%SZ)" "$$"
}
is_runner_pass() {
    jq -rn --arg id "$1" --arg p "$ACTOR:$HOST_ID:" '
        if ($id | startswith($p)) and ($id[($p | length):] | test("^[0-9]{8}T[0-9]{6}Z-[0-9]+\\z"))
        then "yes" else "no" end'
}

# decide <verb> <args json> <approved: true|false> — one jq pass over the
# ALLOWLIST: either {refuse: reason} or {argv, timeout}. The packet's
# verb and args enter only as --arg/--argjson values — data, never
# program text. `approved` is this runner's own verdict, reached below
# from the system of record; a verb that requires an approval is refused
# here without it, so a path that forgot to verify still refuses.
decide() {
    jq -c --arg verb "$1" --arg host "$HOST_ID" --argjson args "$2" --argjson approved "$3" '
        def refuse(msg): {refuse: msg};
        def hex4: [(. / 4096 | floor) % 16, (. / 256 | floor) % 16, (. / 16 | floor) % 16, . % 16]
                  | map("0123456789ABCDEF"[.:. + 1]) | join("");
        def controls: [explode[] | select(. < 32 or . == 127) | "U+" + hex4] | unique;
        .verbs[$verb] as $spec
        | if $verb == "" then refuse("metadata.verb is missing")
          elif $spec == null then
            refuse("verb \($verb) is not in the allowlist (infra/ops/verbs/); verbs: "
                   + (.verbs | keys | join(", ")))
          elif (($spec.hosts // []) | index($host)) == null then
            refuse("verb \($verb) does not serve host \($host) — infra/ops/verbs/\($verb).json scopes it to "
                   + (if (($spec.hosts // []) | length) == 0
                      then "no host (a verb that declares no `hosts` is refused everywhere)"
                      else ($spec.hosts | join(", ")) end)
                   + "; verbs this host serves: "
                   + ([.verbs | to_entries[] | select((.value.hosts // []) | index($host)) | .key] | join(", ")))
          elif ($spec.requires_approval // false) != false and $approved != true then
            refuse("verb \($verb) declares requires_approval, and this request carries no approval "
                   + "this runner verified (design 17835005: a rendered plan, signed with a passkey, "
                   + "single-use, verified before the argv is built)")
          elif ($args | type) != "array" or any($args[]; type != "string") then
            refuse("metadata.args must be a JSON array of strings")
          elif ($spec | has("capture")) and $spec.capture != "separate-streams" then
            refuse("verb \($verb) declares an unsupported capture format")
          elif ($args | length) > ($spec.params | length) then
            refuse("verb \($verb) takes at most \($spec.params | length) arg(s), got \($args | length)")
          else
            [ $spec.params | to_entries[] | .key as $i | .value as $p
              | $args[$i] as $raw
              | if $raw == null then
                  (if $p | has("default") then {ok: $p.default}
                   elif ($p.optional // false) == true then {omit: true}
                   else {err: "missing required arg \($p.name)"} end)
                # AN ARG IS ONE WORD (security review, 2026-09-24). Oniguruma
                # in jq reads `$` as "end, or before a final newline", so
                # `"target-a\n"` passed `^[a-z-]{1,20}$` (measured, jq 1.6)
                # and the line-per-word argv below grew an extra empty word,
                # moving every later placeholder — on an approval verb, the
                # one the signed plan hash rides. So no control character
                # reaches a pattern at all, and none reaches the argv.
                elif ($raw | controls | length) > 0 then
                  {err: "arg \($p.name) value \($raw | tojson) carries a control character (\($raw | controls | join(", "))); an arg is one word on one line"}
                elif $p | has("one_of") then
                  (if any($p.one_of[]; . == $raw) then {ok: $raw}
                   else {err: "arg \($p.name) value \($raw) is not one of \($p.one_of | join(", "))"} end)
                elif ($raw | test($p.pattern)) | not then
                  {err: "arg \($p.name) value \($raw) does not match \($p.pattern)"}
                elif ($p | has("max")) and (($raw | tonumber) > $p.max) then
                  {err: "arg \($p.name) value \($raw) exceeds max \($p.max)"}
                else {ok: $raw} end
            ] as $vals
            | [ $vals[] | select(has("err")) | .err ] as $errs
            | if ($errs | length) > 0 then refuse($errs | join("; "))
              else {argv: [ $spec.argv[]
                            | if test("^\\{[0-9]+\\}$")
                              then . as $ph
                                   | $vals[($ph | ltrimstr("{") | rtrimstr("}") | tonumber) - 1]
                                   | if has("omit") then empty else .ok end
                              else . end ],
                    timeout: ($spec.timeout // null),
                    capture: ($spec.capture // null),
                    effect: ($spec.effect // null),
                    effect_unread: ($spec.effect_unread // null)}
              end
          end' "$VERBS_FILE"
}

# A byte record, not a parsed proposal (f3a09e07). Base64 preserves binary
# and trailing newlines; completeness describes retained bytes, not success.
# A consumer must also read the exit before interpreting a complete stream.
stream_record() {
    sr_file="$1"
    sr_bytes=$(wc -c < "$sr_file") || return 1
    sr_hash=$(sha256sum "$sr_file") || return 1
    sr_hash=${sr_hash%% *}
    head -c "$OPS_OUTPUT_CAP" "$sr_file" > "$workdir/stream-kept" || return 1
    sr_kept=$(wc -c < "$workdir/stream-kept") || return 1
    # Separate commands conserve the encoder's exit in POSIX sh: a pipe
    # to tr would hide a failed base64 behind tr's successful empty read.
    base64 "$workdir/stream-kept" > "$workdir/stream-encoded" || return 1
    tr -d '\n' < "$workdir/stream-encoded" > "$workdir/stream-base64" || return 1
    jq -cn --rawfile encoded "$workdir/stream-base64" \
        --argjson bytes "$sr_bytes" --argjson kept "$sr_kept" --arg hash "$sr_hash" \
        '{base64:$encoded, bytes:$bytes, retained_bytes:$kept,
          sha256:$hash, complete:($bytes == $kept)}'
}

# approval_contract <verb> — {plan_verb: name} when the verb can carry
# an approval, else {refuse: reason}. Judged before any plan is rendered
# and again before anything runs, off the allowlist alone:
#   - it names a `plan_verb`: q1, a passkey signs a rendered plan, never
#     a verb call, so a verb with no plan has nothing to sign;
#   - that verb is in the allowlist, needs no approval itself, and
#     serves this host — the plan is rendered where the write will run;
#   - its LAST param is a required `plan_sha256`: the write's script
#     re-renders and refuses bytes that no longer hash to it (q4, drift
#     voids the approval). A write that cannot do that could act on a
#     state the signature never saw, so it stays inert — which is where
#     commission-a-disk was until its script took the hash (b2d5b546);
#   - the plan verb takes exactly the write's other params, by name, so
#     the plan is rendered from the args the write will act on;
#   - it names its `approvers`, employee ids (design 03451237 q2, David
#     2026-09-22: a named list, not a role — a role is registry data, so
#     a role gate hangs the approval on whoever can write a policy row).
#     Only a presence stamp whose `authority_id` is on it approves.
approval_contract() {
    jq -c --arg v "$1" --arg h "$HOST_ID" '
        .verbs as $all | $all[$v] as $s | ($s.params // []) as $ap
        | ($s.plan_verb // null) as $pv
        | if ($pv | type) != "string" then
            {refuse: "verb \($v) declares requires_approval and names no plan_verb, so there is no rendered plan for a passkey to sign (design 17835005 q1: the signature binds a plan, never a verb call)"}
          elif $all[$pv] == null then
            {refuse: "verb \($v) names plan_verb \($pv), which is not in the allowlist (infra/ops/verbs/)"}
          elif ($all[$pv].requires_approval // false) != false then
            {refuse: "verb \($v) names plan_verb \($pv), which itself requires an approval — a plan must be renderable before anything is signed"}
          elif (($all[$pv].hosts // []) | index($h)) == null then
            {refuse: "verb \($v) names plan_verb \($pv), which does not serve host \($h), so the plan could not be rendered where the write runs"}
          elif ($ap | length) == 0 or $ap[-1].name != "plan_sha256"
               or $ap[-1].pattern != "^[0-9a-f]{64}$"
               or ($ap[-1].optional // false) == true or ($ap[-1] | has("default")) then
            {refuse: "verb \($v) does not take a required plan_sha256 as its last param, so its script cannot re-render the plan and refuse one that moved since it was signed (design 17835005 q4: drift voids an approval). A write that cannot void a stale approval is refused"}
          elif [$ap[:-1][].name] != [($all[$pv].params // [])[].name] then
            {refuse: "plan_verb \($pv) takes (\([($all[$pv].params // [])[].name] | join(", "))) and verb \($v) takes (\([$ap[].name] | join(", "))): the plan must be rendered from exactly the args the write acts on, less plan_sha256"}
          elif ($s.approvers | type) != "array" or ($s.approvers | length) == 0
               or any($s.approvers[]; type != "string" or . == "") then
            {refuse: "verb \($v) names no approvers (a non-empty list of employee ids in infra/ops/verbs/\($v).json), so no passkey can approve it (design 03451237 q2: who may approve is a named list, never a role)"}
          else {plan_verb: $pv, approvers: $s.approvers,
                nothing_to_do: ($all[$pv].nothing_to_do // null)} end' "$VERBS_FILE"
}

# The job metadata key that readies ops-request's `nothing-to-do`
# terminal. Written by this runner and nothing else; boss-testing's
# the_nothing_to_do_terminal_waits_on_the_marker_the_runner_writes holds
# ops-request.toml's `ready_when` to this line by name (CLAUDE.md §9a).
NOTHING_TO_DO_MARKER='plan_names_nothing'

# close_nothing_to_do <job json> <plan verb> <args json> <write verb>
# <pattern> — close the request through its `nothing-to-do` terminal,
# carrying the plan ($workdir/plan, hash $rv_sha) and everything it was
# rendered for. Three writes, IN THIS ORDER: the record onto the
# terminal through the step merge door (it is pending, and the merge
# door takes a pending step, as `abort_request`'s does); then the job
# marker its `ready_when` waits on, so the step is never ready without
# its record; then the status alone (`finish_step`; the dispatcher's
# marker rule may complete it first, and a status-only re-send to a
# completed step answers 204). Returns 0 when closed, 2 when the request
# has no such terminal — pinned to an ops-request version from before
# it, which has nowhere to record this — and 1 when a write was refused,
# having said why on stderr.
close_nothing_to_do() {
    nd_id=$(printf '%s' "$1" | jq -r '.id')
    nd_short=$(printf '%s' "$nd_id" | cut -c1-8)
    nd_sid=$(printf '%s' "$1" | jq -r '((.steps // []) | map(select(.spec_slug == "nothing-to-do")) | .[0].id) // empty')
    [ -n "$nd_sid" ] || return 2
    jq -cn --rawfile p "$workdir/plan" --arg v "$4" --arg h "$HOST_ID" --argjson a "$3" \
        --arg s "$rv_sha" --arg pv "$2" --arg re "$5" \
        '{plan: $p, verb: $v, host: $h, args: $a, rendered_plan_sha256: $s,
          plan_verb: $pv, nothing_to_do: $re, runner_host: $h}' > "$workdir/nothing"
    step_write PATCH "$workdir/nothing" "$workdir/nothing-body" \
        "$BASE/api/jobs/$nd_id/steps/$nd_sid/metadata"
    case "${sw_code:-000}" in
        2??) ;;
        *) echo "ops-runner: could not record nothing to do on $nd_short — merge HTTP ${sw_code:-none}: $(head -c 2000 "$workdir/nothing-body" | tr '\n' ' ')$(cat "$workdir/put-err")" >&2
           return 1 ;;
    esac
    jq -cn --arg k "$NOTHING_TO_DO_MARKER" '{($k): true}' > "$workdir/nothing-marker"
    step_write PATCH "$workdir/nothing-marker" "$workdir/nothing-body" \
        "$BASE/api/jobs/$nd_id/metadata"
    case "${sw_code:-000}" in
        2??) ;;
        *) echo "ops-runner: could not mark $nd_short as naming nothing to do — job merge HTTP ${sw_code:-none}: $(head -c 2000 "$workdir/nothing-body" | tr '\n' ' ')$(cat "$workdir/put-err")" >&2
           return 1 ;;
    esac
    finish_step "$workdir/nothing-body" "$BASE/api/jobs/$nd_id/steps/$nd_sid" \
        "$nd_short's nothing-to-do step"
    case "${cs_code:-000}" in
        2??) return 0 ;;
    esac
    echo "ops-runner: could not close $nd_short as nothing to do — PUT HTTP ${cs_code:-none}: $(head -c 2000 "$workdir/nothing-body" | tr '\n' ' ')$(cat "$workdir/put-err")" >&2
    return 1
}

# abort_request <job json> <reason> — close the request through its
# `refused` terminal, with the reason on that step. The one refusal door
# when `execute` is still PENDING behind the approve step: a step
# completed out of order is refused by the server, but a terminal whose
# `outcome_kind` is `aborted` completes from any open state (fd0f92ae),
# and closing marks the rest skipped. Returns non-zero, having said why
# on stderr, when the server did not take it.
#
# Its two writes are `complete_step`'s, the execute completion's own
# (backlog 2aa2b19e): the reason through the merge door, then the
# status alone, sent once more when it loses the step race. It was one
# PUT carrying the refused step's metadata as READ plus the reason, with
# no resend, so the one 409 that says nothing was written left the
# request open over a refusal the runner had already decided.
abort_request() {
    ab_id=$(printf '%s' "$1" | jq -r '.id')
    ab_short=$(printf '%s' "$ab_id" | cut -c1-8)
    ab_step=$(printf '%s' "$1" | jq -c '((.steps // []) | map(select(.spec_slug == "refused")) | .[0]) // empty')
    if [ -z "$ab_step" ]; then
        echo "ops-runner: $ab_short has no refused step to close it by — $2" >&2
        return 1
    fi
    ab_sid=$(printf '%s' "$ab_step" | jq -r '.id')
    jq -cn --arg r "$2" --arg h "$HOST_ID" '{reason: $r, runner_host: $h}' > "$workdir/abort"
    complete_step "$workdir/abort" "$workdir/abort-body" \
        "$BASE/api/jobs/$ab_id/steps/$ab_sid" "$ab_short's refused step"
    case "${cs_code:-000}" in
        2??) return 0 ;;
    esac
    echo "ops-runner: could not close $ab_short refused — $cs_write HTTP ${cs_code:-none}: $(head -c 2000 "$workdir/abort-body" | tr '\n' ' ')$(cat "$workdir/put-err") — the refusal was: $2" >&2
    return 1
}

# The `code` the step PUT's 409 carries when the row moved between the
# handler's read and its write (car 88123ae0) — the one refused
# completion that is sent again (see `finish_step`). The runner matches
# the CODE, never the current `error` words, which are a person's and
# free to move (backlog 2aa2b19e: the words were copied here byte for
# byte and went stale when the compare widened to the whole row). Its
# definition is boss_jobs::step_metadata_write::STEP_CHANGED_CODE;
# boss-testing's the_step_race_is_matched_by_its_code holds this line to
# it by name.
STEP_CHANGED_CODE='step_changed'

# DELETE AFTER ONE RELEASE (the review of car 24eb9471). The words a
# server from before the code answered the same race with, and nothing
# else: while a jobs API that predates the code can still be what this
# runner reaches, those exact words are the race too. Its definition is
# boss_jobs::step_metadata_write::STEP_CHANGED_ERROR_BEFORE_THE_CODE,
# which carries the same deletion note; the same pin holds this line to
# it by name.
STEP_CHANGED_WORDS_BEFORE_THE_CODE='step changed while this write was computed — its metadata is no longer what the write read, so writing it would erase the other write'

# step_write <method> <payload file> <response body file> <url> — one
# step write, bounded by OPS_WRITE_MAX_TIME and tried again after each
# OPS_WRITE_BACKOFF delay while the system of record gives no answer or
# a 5xx: a roll, which the gate runner's report rides out the same way.
# A retry is safe on both doors — a merge re-sent unchanged and a
# status-only PUT re-sent to a completed step each answer success. The
# retries of a whole pass draw on one OPS_WRITE_RETRY_BUDGET, counted in
# `retry_spent` — which is why this runs in the caller's shell and sets
# `sw_code`, the last HTTP status (empty when curl never got an answer),
# rather than printing it into a subshell that would forget the spend.
# The server's words land in the body file, curl's in $workdir/put-err.
step_write() {
    sw_code=""
    for sw_delay in $OPS_WRITE_BACKOFF end; do
        : > "$3"
        sw_t0=$(date -u +%s)
        sw_code=$(curl -sS --max-time "$OPS_WRITE_MAX_TIME" -o "$3" -w '%{http_code}' -X "$1" \
            -H "content-type: application/json" \
            -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
            --data-binary @"$2" \
            "$4" 2>"$workdir/put-err") || sw_code=""
        case "${sw_code:-000}" in
            000|5??) ;;
            *) break ;;
        esac
        [ "$sw_delay" = end ] && break
        retry_spent=$((retry_spent + $(date -u +%s) - sw_t0))
        if [ $((retry_spent + sw_delay)) -gt "$OPS_WRITE_RETRY_BUDGET" ]; then
            echo "ops-runner: $1 ${4#"$BASE"} answered ${sw_code:-nothing} — this pass's retry budget (${OPS_WRITE_RETRY_BUDGET}s) is spent; taking the answer as it stands" >&2
            break
        fi
        echo "ops-runner: $1 ${4#"$BASE"} answered ${sw_code:-nothing} — the system of record may be rolling; trying again in ${sw_delay}s" >&2
        sleep "$sw_delay"
        retry_spent=$((retry_spent + sw_delay))
    done
}

# lost_the_step_race <status> <response body file> — is this the 409 of
# the step race: its `code` is STEP_CHANGED_CODE, or (for one release)
# its `error` is exactly the words a server from before the code sent?
lost_the_step_race() {
    [ "$1" = 409 ] || return 1
    [ "$(jq -r --arg c "$STEP_CHANGED_CODE" --arg w "$STEP_CHANGED_WORDS_BEFORE_THE_CODE" '
        if (.code? == $c) or (.error? == $w) then "yes" else "no" end' "$2" 2>/dev/null)" = yes ]
}

# finish_step <response body file> <step url> <label> — the status alone,
# sent once more when it loses the step race (backlog 2a6d0b86). The
# step PUT refuses, 409 code STEP_CHANGED_CODE, a write whose read
# another write moved before it landed (car 88123ae0), where it used to
# answer success and erase that write. The refusal means NOTHING was
# written, and the body is the status alone, so it carries nothing a
# lost race can make stale: the same body goes once more. A second loss,
# or any other refusal, stands — never a loop. Sets `cs_code` and
# `cs_write`=PUT, as `complete_step` does.
finish_step() {
    cs_write=PUT
    printf '%s' '{"status":"completed"}' > "$workdir/status-only"
    step_write PUT "$workdir/status-only" "$1" "$2"; cs_code=$sw_code
    if lost_the_step_race "$cs_code" "$1"; then
        echo "ops-runner: PUT on $3 lost the step race to another write — nothing was written; sending the same completion once more" >&2
        step_write PUT "$workdir/status-only" "$1" "$2"; cs_code=$sw_code
        if lost_the_step_race "$cs_code" "$1"; then
            echo "ops-runner: PUT on $3 lost the step race twice — not resent again" >&2
        fi
    fi
    return 0
}

# complete_step <keys file> <response body file> <step url> <label> —
# complete a step sending only what this runner changes (backlog
# 2aa2b19e, the gate runner's shape since e39a9d2a). TWO WRITES, IN
# THIS ORDER: the keys through the step merge door, which merges them
# against the row as it stands, in one transaction, so it cannot race
# and sheds nothing it was not sent; THEN the status alone
# (`finish_step`), because a step's required-at-done fields are judged
# when it flips to completed, so they must be on the row first. A
# refused merge sends no status. Keys that landed under a status that
# did not are no answer to any reader (`boss ops --wait` and the
# approval surface read only a COMPLETED execute), and an execute left
# that way carrying this host's answer is finished by the next pass,
# never run again (see "AN ANSWER THIS HOST ALREADY RECORDED").
#
# Sets `cs_code`, the last write's HTTP status (empty when curl never got
# an answer), and `cs_write`, which write that was (`merge` or `PUT`);
# the server's words are in the body file, curl's in $workdir/put-err.
complete_step() {
    cs_write=merge
    step_write PATCH "$1" "$2" "$3/metadata"; cs_code=$sw_code
    case "${cs_code:-000}" in
        2??) ;;
        *) return 0 ;;
    esac
    finish_step "$2" "$3" "$4"
}

# record_refused_completion <http status> <response body file> <verb ran:
# true|false> — a completion the SERVER refused, written onto the
# request ($job, $job_id, $short) through the job metadata door. It
# accumulates from what the request already carries: how long a request
# has been jammed, and how often its verb re-ran, is the number a reader
# wants. `verb_ran` because a refused answer whose verb ran is HELD for
# a human rather than run again (865d37df).
record_refused_completion() {
    printf '%s' "$job" | jq -c --arg code "$1" --rawfile body "$2" \
        --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg ran "$3" '
        (.metadata.completion_refused // {}) as $prev
        | {completion_refused: {
            http: ($code | tonumber),
            reason: ($body | .[0:2000]),
            first_at: ($prev.first_at // $at),
            last_at: $at,
            count: (($prev.count // 0) + 1),
            verb_ran: ($ran == "true")}}' > "$workdir/refused"
    if ! rr_err=$(curl -fsS -X PATCH -H "content-type: application/json" \
            -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
            --data-binary @"$workdir/refused" \
            "$BASE/api/jobs/$job_id/metadata" 2>&1 >/dev/null); then
        echo "ops-runner: could not record the refusal on $short — $rr_err" >&2
    fi
}

# run_plan_verb <request id> <plan verb> <args json> — run the plan verb
# on this host. On success `rv_why` is empty, the plan's bytes are in
# $workdir/plan and `rv_sha` is their sha256; otherwise `rv_why` names
# why there is no plan. Used twice, and deliberately the same both times:
# to RENDER the plan onto the approve step, and to render it AGAIN just
# before the write, so the runner itself can refuse a signed plan that
# is not what the plan verb produces here now. Its own positional
# parameters are the plan verb's argv, so the caller's are untouched.
run_plan_verb() {
    rv_id=$1; rv_verb=$2; rv_args=$3
    rv_why=""; rv_sha=""
    : > "$workdir/plan"
    rv_dec=$(decide "$rv_verb" "$rv_args" false)
    rv_why=$(printf '%s' "$rv_dec" | jq -r '.refuse // empty')
    if [ -z "$rv_why" ]; then
        set --
        while IFS= read -r w; do
            set -- "$@" "$w"
        done <<ARGV
$(printf '%s' "$rv_dec" | jq -r '.argv[]')
ARGV
        rv_script=""
        case "$1" in
            /*) rv_script="$1" ;;
            */*) rv_script="$OPS_REPO_ROOT/$1" ;;
        esac
        if [ -n "$rv_script" ]; then
            if [ -x "$rv_script" ]; then
                shift; set -- "$rv_script" "$@"
            else
                rv_why="it names a script not in this checkout: $rv_script"
            fi
        fi
    fi
    if [ -n "$rv_why" ]; then
        rv_why="the plan verb $rv_verb cannot render a plan for this request: $rv_why"
        return 0
    fi
    rv_t=$(printf '%s' "$rv_dec" | jq -r '.timeout // empty')
    # stdout and stderr APART: the plan is stdout, byte for byte, and
    # its `plan-sha256:` line rides stderr because a hash cannot be
    # inside what it hashes.
    OPS_REQUEST_ID="$rv_id" BOSS_ACTOR="${BOSS_ACTOR:-$ACTOR}" \
        timeout "${rv_t:-$OPS_TIMEOUT}" "$@" > "$workdir/plan" 2> "$workdir/plan-err" < /dev/null
    rv_rc=$?
    rv_n=$(wc -c < "$workdir/plan")
    rv_sha=$(sha256sum "$workdir/plan" | cut -c1-64)
    rv_said=$(sed -n 's/^plan-sha256: \([0-9a-f]\{64\}\)$/\1/p' "$workdir/plan-err" | tail -n 1)
    if [ "$rv_rc" -ne 0 ]; then
        rv_why="the plan verb $rv_verb refused (exit $rv_rc): $(head -c 2000 "$workdir/plan-err")$(head -c 2000 "$workdir/plan")"
    elif [ "$rv_n" -eq 0 ]; then
        rv_why="the plan verb $rv_verb rendered an empty plan, and an empty plan names nothing to approve"
    elif [ "$rv_n" -gt "$OPS_OUTPUT_CAP" ]; then
        rv_why="the plan verb $rv_verb rendered $rv_n bytes, over OPS_OUTPUT_CAP=$OPS_OUTPUT_CAP; a truncated plan cannot be signed"
    elif [ "$rv_said" != "$rv_sha" ]; then
        rv_why="the plan verb $rv_verb printed plan-sha256 '${rv_said:-none}' on stderr, and its stdout hashes to $rv_sha; a plan that does not name its own hash cannot be held to its bytes"
    else
        # The bytes must survive being a JSON string, or the hash the
        # write is handed later would not be the hash it re-renders: a
        # plan that is not UTF-8 is refused here, not at the write.
        rv_back=$(jq -jn --rawfile p "$workdir/plan" '$p' | sha256sum | cut -c1-64)
        if [ "$rv_back" != "$rv_sha" ]; then
            rv_why="the plan verb $rv_verb rendered bytes that do not survive a JSON string (not UTF-8?), so the stored plan would not hash to what the write re-renders"
        fi
    fi
    [ -z "$rv_why" ] || rv_sha=""
}

# render_plan <job json> <plan verb> <args json> <approve step json>
# <verb> [<nothing_to_do>] — render the plan and write it onto the
# approve step, or, when the plan verb's declared `nothing_to_do`
# matches the plan, close the request with nothing asked. Sets
# `plan_outcome` to planned | nothing | refused | failed.
#
# A PLAN THAT NAMES NOTHING TO DO ASKS NOBODY (backlog b2f78bb9, car 3
# of 3df309bf). The machine files a remedy while a finding persists, and
# a remedy that already ran renders a plan with nothing in it — which
# reached the approve step as a passkey prompt for a no-op. A plan verb
# may declare in its file the regex (jq's engine) its WHOLE plan matches
# when it names no change; a match closes the request through its
# `nothing-to-do` terminal with the plan on it, and the approve step is
# never written. Every doubt asks instead: no declaration, a regex jq
# cannot judge, or a request pinned to a version with no such terminal
# renders the plan for the passkey exactly as before — a no-op signed
# is a no-op run, while a request closed on a guess is a remedy nobody
# looked at.
#
# WHAT THE PASSKEY SIGNS IS THE WHOLE REQUEST (security review,
# 2026-09-24). The plan rides with the `verb`, `host` and `args` it was
# rendered for and `rendered_plan_sha256`, the hash of the bytes THIS
# runner produced — one write, so all of it is inside the step's shape
# hash. The request's own metadata stays writable after the signature;
# these do not, and execute compares the request against them before it
# builds an argv.
render_plan() {
    rp_job=$1; rp_verb=$2; rp_args=$3; rp_approve=$4; rp_write=$5; rp_nothing=${6:-}
    rp_id=$(printf '%s' "$rp_job" | jq -r '.id')
    rp_short=$(printf '%s' "$rp_id" | cut -c1-8)
    rp_sid=$(printf '%s' "$rp_approve" | jq -r '.id')
    plan_outcome=failed
    run_plan_verb "$rp_id" "$rp_verb" "$rp_args"
    rp_why=""
    if [ -z "$rv_why" ] && [ -n "$rp_nothing" ]; then
        rp_none=$(jq -rn --rawfile p "$workdir/plan" --arg re "$rp_nothing" \
            'if ($p | test($re)) then "yes" else "no" end' 2> "$workdir/nothing-err") || rp_none=error
        case "$rp_none" in
            yes)
                close_nothing_to_do "$rp_job" "$rp_verb" "$rp_args" "$rp_write" "$rp_nothing"
                case $? in
                    0)
                        echo "ops-runner: $rp_short — the plan $rp_verb rendered (plan-sha256 $rv_sha) names nothing to do; closed nothing-to-do, no passkey asked"
                        plan_outcome=nothing
                        return 0 ;;
                    2)
                        echo "ops-runner: $rp_short — the plan $rp_verb rendered names nothing to do, but the request has no nothing-to-do terminal (filed under an ops-request version from before it); rendering it for a passkey as before" >&2 ;;
                    # A CLOSE THE SERVER DID NOT TAKE IS A DOUBT, AND A
                    # DOUBT ASKS (backlog aa816dd4). The terminal's record
                    # is the runner account's alone (`written_by`), so a
                    # runner signing under another BOSS_OPS_ACTOR is
                    # refused it on every pass; returning here left such a
                    # request open forever with nobody asked. The plan
                    # goes onto the approve step instead — a no-op signed
                    # is a no-op run. The reason is already on stderr.
                    *)
                        echo "ops-runner: $rp_short — the plan $rp_verb rendered names nothing to do, but the request could not be closed that way (above); rendering it for a passkey" >&2 ;;
                esac ;;
            no) ;;
            *)
                echo "ops-runner: $rp_short — $rp_verb's nothing_to_do could not be judged ($(head -c 300 "$workdir/nothing-err" | tr '\n' ' ')); rendering the plan for a passkey" >&2 ;;
        esac
    fi
    if [ -n "$rv_why" ]; then
        rp_why="$rv_why — so there is nothing to sign"
    else
        rp_sha=$rv_sha
        jq -cn --rawfile p "$workdir/plan" --arg v "$rp_write" --arg h "$HOST_ID" \
            --argjson a "$rp_args" --arg s "$rp_sha" \
            '{plan: $p, verb: $v, host: $h, args: $a, rendered_plan_sha256: $s}' > "$workdir/plan-patch"
    fi
    if [ -n "$rp_why" ]; then
        if abort_request "$rp_job" "$rp_why"; then
            echo "ops-runner: refused $rp_short — $rp_why"
            plan_outcome=refused
        fi
        return 0
    fi
    if ! rp_err=$(curl -fsS -X PATCH -H "content-type: application/json" \
            -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
            --data-binary @"$workdir/plan-patch" \
            "$BASE/api/jobs/$rp_id/steps/$rp_sid/metadata" 2>&1 >/dev/null); then
        echo "ops-runner: could not write the plan onto $rp_short's approve step — $rp_err" >&2
        return 0
    fi
    echo "ops-runner: rendered the plan for $rp_verb on $rp_short (plan-sha256 $rp_sha) — it waits for a passkey approval on its approve step"
    plan_outcome=planned
}

# verify_approval <job json> <verb> <execute status> — {plan_sha256,
# signed_at} when the system of record holds a passkey approval of this
# request's plan that this runner may act on NOW, else {refuse: reason}
# naming what failed. It reads only the approve step's server-minted
# record: its status, its decision (inside the signed shape), its
# assurance declaration, its required roles and the sign-off stamps the
# server wrote. Nothing the filer wrote counts.
verify_approval() {
    va_verb=$2
    if [ "$3" != ready ]; then
        jq -cn --arg v "$va_verb" --arg s "$3" '{refuse: "execute is already \($s): an earlier pass claimed this approval to run \($v), and a single-use approval is never run twice (design 17835005 q4). Whether that pass finished is in this host'"'"'s journal; to run \($v) again, file a new request and approve its fresh plan"}'
        return 0
    fi
    va_c=$(approval_contract "$va_verb")
    if [ -n "$(printf '%s' "$va_c" | jq -r '.refuse // empty')" ]; then
        printf '%s' "$va_c"
        return 0
    fi
    va_a=$(printf '%s' "$1" | jq -c '((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) // empty')
    va_shape=""; va_sha=""
    if [ -n "$va_a" ]; then
        va_shape=$(shape_hash_of "$va_a")
        va_sha=$(printf '%s' "$va_a" | jq -j '.metadata.plan // "" | strings' | sha256sum | cut -c1-64)
    fi
    va_verdict=$(printf '%s' "$1" | jq -c --arg v "$va_verb" --arg shape "$va_shape" \
        --arg host "$HOST_ID" --arg plansha "$va_sha" \
        --argjson approvers "$(printf '%s' "$va_c" | jq -c '.approvers')" \
        --argjson now "$(date -u +%s)" --argjson ttl "$APPROVAL_TTL_S" --argjson skew "$APPROVAL_SKEW_S" \
        --slurpfile verbs "$VERBS_FILE" '
        def ts: try (sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z") | fromdateiso8601) catch null;
        def no(msg): {refuse: ("verb \($v) requires a passkey approval, and " + msg)};
        def request(m): {verb: m.verb, host: m.host, args: m.args};
        (.metadata // {}) as $jm
        | ($verbs[0].verbs[$v].params | length) as $np
        | ((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) as $a
        # A MISSING args is named as missing (backlog 3df309bf review): for
        # a verb whose only param is the hash this used to read "carries 0
        # arg(s) where it takes 0", a refusal that names no defect.
        | if ($jm.args | type) != "array" then
            no("this request carries no args list (args is \($jm.args | type)): it must carry an array of exactly \($np - 1) arg(s) before plan_sha256 — an empty list when the verb takes none — and the hash is appended by this runner from the SIGNED plan, never supplied by the filer")
          elif ($jm.args | length) != ($np - 1) then
            no("this request carries \($jm.args | length) arg(s) where it takes \($np - 1) before plan_sha256: the hash is appended by this runner from the SIGNED plan, never supplied by the filer")
          elif $a == null then
            no("this request has no approve step to carry one (filed before ops-request v2); file it again with boss ops")
          elif $a.status != "completed" then
            no("its approve step is \($a.status), not completed"
               + (if $jm.requires_approval != true
                  then " — the request was filed without requires_approval, so its approve step could never become ready, and no field on the packet stands in for a signature. File it with boss ops, which reads requires_approval off the verb"
                  else "" end))
          # A REJECTION IS NOT AN APPROVAL (adversarial re-review,
          # 2026-09-25). Reject runs the same ceremony as Approve on both
          # surfaces — the decision saved, a presence stamp bound to the
          # shape that carries it, the step completed — so everything
          # below holds for a rejected plan too, and before this line the
          # write RAN on one (reproduced with this runner). The decision
          # is inside the signed shape, so this reads what was signed;
          # only the exact string "approved" is an approval.
          elif $a.metadata.decision != "approved" then
            no("its approve step records decision \($a.metadata // {} | if has("decision") then .decision | tojson else "none" end), not \"approved\": a passkey that signed any other decision approved nothing")
          elif $a.assurance_required != "presence" then
            no("its approve step declares assurance \($a.assurance_required // "none"), not presence, so completing it proves no passkey signed the plan")
          elif (($a.sign_offs_required // []) | length) == 0 then
            no("its approve step requires no sign-off, so nothing on it records who approved the plan (the d5efbb3c shape: completed with sign_offs [])")
          elif ($a.metadata.plan | type) != "string" or $a.metadata.plan == "" then
            no("its approve step carries no plan, so there is nothing a signature could have bound")
          # THE SIGNED REQUEST, compared before any argv exists: the verb,
          # host and args this runner rendered the plan for rode onto the
          # approve step with it, inside the signed shape. The request
          # itself stays writable, so it must still ask for exactly that.
          elif request($a.metadata) != {verb: $v, host: $host, args: $jm.args} then
            no("its approve step signs \(request($a.metadata) | tojson), and this request asks for \({verb: $v, host: $host, args: $jm.args} | tojson): the request is not the one that was signed")
          # A PLAN THIS RUNNER WROTE carries the hash of the bytes it
          # rendered; the plan-verb re-render before the claim is the
          # check a forger cannot satisfy by writing both fields.
          elif ($a.metadata.rendered_plan_sha256 // null) != $plansha then
            no("its approve step carries rendered_plan_sha256 \($a.metadata.rendered_plan_sha256 // "none" | tostring), and its plan hashes to \($plansha): the plan on the step is not the plan this runner rendered")
          else
            [ $a.sign_offs_required[] as $r
              | [($a.sign_offs // [])[] | select(.role == $r)] as $mine
              | [$mine[] | select(.assurance == "presence"
                                  and ((.presence_nonce // "") | type) == "string" and (.presence_nonce // "") != ""
                                  and ((.authority_id // "") | type) == "string" and (.authority_id // "") != "")] as $pres
              | [$pres[] | select(.authority_id as $who | any($approvers[]; . == $who))] as $named
              | [$named[] | select(.shape_hash == $shape)] as $onshape
              # A STAMP DIES WHEN THE SHAPE IT SIGNED LEAVES THE STEP
              # (backlog c085256d, design 87329a13). The server marks it
              # voided_at at the edit that moved the shape, and a dead
              # stamp is no approval even on the shape it signed: content
              # put back byte for byte (A-B-A) must not revive a withdrawn
              # one. Only a stamp with no void at all counts — a
              # voided_at of any value, false included, is dead.
              | [$onshape[] | select(.voided_at == null)] as $bound
              # AND THE NEWEST WORD OF AN APPROVER IS THE ONE THAT COUNTS: each
              # named approver, the last stamp for the role, in the order the
              # server appended them and of any assurance, must be on the
              # shape being run. A rejection is signed too, so on a record
              # that carries no void (written before the server voided
              # anything) the newer stamp on another shape is what says
              # the older one was withdrawn. Read, as everything here is,
              # only from fields the server minted.
              | [$approvers[] as $who | [$mine[] | select(.authority_id == $who)]
                 | select(length > 0) | .[-1]] as $newest
              | [$newest[] | select(.shape_hash != $shape)] as $withdrawn
              | if ($mine | length) == 0 then {err: "no \($r) sign-off is stamped on its approve step"}
                elif ($pres | length) == 0 then {err: "the \($r) sign-off is \($mine[-1].assurance // "session")-assured, not presence: no passkey signed it"}
                elif ($named | length) == 0 then {err: "the \($r) presence sign-off is by \([$pres[] | .authority_id] | unique | join(", ")), who is not among the approvers infra/ops/verbs/\($v).json names (\($approvers | join(", "))): who may approve is a named list, never a role (design 03451237 q2)"}
                elif ($onshape | length) == 0 then {err: "the \($r) presence sign-off is bound to shape \($named[-1].shape_hash), and the approve step now hashes to \($shape): the plan on the step is not the plan that was signed"}
                elif ($withdrawn | length) > 0 then {err: "the newest \($r) sign-off by \($withdrawn[0].authority_id) is bound to shape \($withdrawn[0].shape_hash), and the approve step now hashes to \($shape): an approver'"'"'s newest word on the step is the one that counts, so an older stamp on this shape was withdrawn (design 87329a13)"}
                elif ($bound | length) == 0 then {err: "the \($r) presence sign-off bound to this shape was voided at \($onshape[-1].voided_at | tostring), when the content it signed left the approve step: a stamp dies when the shape it signed leaves the step, and putting the content back does not revive it (design 87329a13). Approve the plan again"}
                else ([$bound[] | .stamped_at | ts]) as $t
                  | if any($t[]; . == null) then {err: "the \($r) sign-off carries a stamped_at this runner cannot read: \([$bound[] | .stamped_at | tostring] | join(", "))"}
                    else {at: ($t | max)} end
                end ] as $per
            | [$per[] | select(has("err")) | .err] as $errs
            | if ($errs | length) > 0 then no($errs | join("; "))
              else ([$per[] | .at] | min) as $at
              | if $at > $now + $skew then
                  no("it was signed at \($at | todate), in this host'"'"'s future (now \($now | todate)); an approval that cannot be dated cannot be time-boxed")
                elif $now - $at > $ttl then
                  no("it was signed at \($at | todate), \($now - $at)s ago, and an approval expires after \($ttl)s (design 17835005 q4: single-use and time-boxed). File the request again and approve its fresh plan")
                else {ok: true, signed_at: ($at | todate)} end
              end
          end')
    if [ -n "$(printf '%s' "$va_verdict" | jq -r '.refuse // empty')" ]; then
        printf '%s' "$va_verdict"
        return 0
    fi
    printf '%s' "$va_verdict" | jq -c --arg h "$va_sha" --argjson c "$va_c" \
        '{plan_sha256: $h, signed_at: .signed_at, plan_verb: $c.plan_verb}'
}

# A LIMIT IS NOT A FILTER (backlog 2cfb4562). This read was
# `?kind=ops-request&status=open&limit=100` — every host's open
# requests, one page, with the `total` the API answers beside the rows
# thrown away — so above 100 the walk silently skipped the tail and the
# gauge below printed the page as the whole queue: a queue stuck at 400
# read 100 forever, and no threshold above 100 could ever be crossed.
# Now the SERVER narrows to this host (`metadata` containment, the
# host url-encoded by jq, never spliced), the page is the API's own
# ceiling (MAX_LIMIT in crates/core/boss-jobs/src/http/mod.rs — if the
# two ever disagree, the comparison below says so loudly rather than
# the page passing for the queue), and the rows are held against
# `total` before anything reads them as a count.
#
# `full=true`: the runner reads each listed request's approve and
# execute step METADATA (the plan, the decision, the stamp's shape, the
# exit it merges into), and the list's default is going slim — step
# metadata only when asked (backlog 9b473d4a; a_listed_packet_carries_its_steps.rs).
host_doc=$(jq -rn --arg h "$HOST_ID" '{host: $h} | tojson | @uri')
QUEUE_PAGE=1000
if ! jobs_json=$(curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=ops-request&status=open&metadata=$host_doc&limit=$QUEUE_PAGE&full=true" 2>&1); then
    echo "ops-runner: jobs-api unreachable at $BASE — $jobs_json" >&2
    exit 1
fi

# Envelope ({"data": [...]}) or bare array; keep open rows for THIS
# host only — the server was asked to narrow, and this still checks.
mine=$(printf '%s' "$jobs_json" | jq -c --arg h "$HOST_ID" '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open" and (.metadata.host // "") == $h))')
n=$(printf '%s' "$mine" | jq 'length')

# THE DEPTH IS EXACT ONLY WHEN THE SERVER'S COUNT VOUCHES FOR IT. Three
# ways it cannot: no numeric `total` (a bare array, an older shape), a
# page that held fewer rows than `total`, or a row on the page for
# ANOTHER host — the evidence the containment filter was not applied,
# and then `total` is every host's (a wrong target answers instead of
# erroring, CLAUDE.md §Doors). The first and third leave only a lower
# bound; the second knows the depth but read part of it, and the list
# is newest first, so the oldest packets are exactly the ones unread.
# `truncated` names which, and the gauge prints `>=` in place of `=` so
# no reader parsing `depth=` can take a lower bound for the count.
depth="$n"; depth_exact=true; truncated=""
total=$(printf '%s' "$jobs_json" | jq -r '
    if type == "object" and (.total | type) == "number" then .total else "" end')
rows=$(printf '%s' "$jobs_json" | jq '
    (if type == "object" and has("data") then .data else . end) | length')
if [ "$rows" -ne "$n" ]; then
    depth_exact=false
    truncated="the list was not narrowed to host $HOST_ID ($rows rows, $n for it) — its total is not this host's"
else
    case ${total:-empty} in
        empty | *[!0-9]*)
            depth_exact=false
            truncated="the list carried no total, so how much it did not hold is unknown" ;;
        *)
            depth="$total"
            [ "$n" -ge "$total" ] || truncated="read $n of $total open — the walk takes the rest on later runs" ;;
    esac
fi

# THE READING, TAKEN BEFORE THE WALK (backlog 1ffb3305). This loop is
# serial and has no per-verb fairness: a latency-sensitive verb waits
# behind whatever is ahead of it in the same run, so a converge — the
# verb that deploys a fix — waits behind however many run-car-probes
# are in front of it. Measured 2026-09-19: run-car-probe was 171 of
# the last 300 ops-requests against 42 converges, and its cadence went
# daily to hourly the same day. Today's volumes are comfortable and
# that is the point of measuring now: the depth is the only real bound
# on raising a probe cadence further, and a queue whose depth nobody
# reads is one that gets discovered at its worst moment, by a converge
# that did not deploy when it should have. Per-verb fairness or a
# priority lane is the larger change and waits for this reading to say
# it is needed.
#
# One wait per packet, computed once here and carried into the walk —
# a request's own wait rides its metadata below, so the series is a
# jobs-API query rather than an ssh to read this journal. `opened_at`
# is what the filer stamps; a packet without one has NO age and says
# so, because `date -d ''` answers midnight rather than erroring and
# would report a fresh packet as a decades-old wait
# (crates/core/boss-jobs/src/probe.rs carries the same trap).
now_s=$(date -u +%s)
waitsf="$workdir/waits"
: > "$waitsf"
oldest_wait="-"
while IFS= read -r ts; do
    [ -n "$ts" ] || continue           # jq emits "-" for an absent stamp,
    w="-"                              # so an empty line is only the
    if [ "$ts" != "-" ]; then          # heredoc's own trailing newline.
        t=$(date -u -d "$ts" +%s 2>/dev/null) || t=""
        case ${t:-empty} in
            empty | *[!0-9]*) ;;
            *)
                w=$((now_s - t))
                [ "$w" -ge 0 ] || w=0  # a clock skew is not a negative wait
                if [ "$oldest_wait" = "-" ] || [ "$w" -gt "$oldest_wait" ]; then
                    oldest_wait="$w"
                fi
                ;;
        esac
    fi
    printf '%s\n' "$w" >> "$waitsf"
done <<TS
$(printf '%s' "$mine" | jq -r '.[] | .metadata.opened_at // "-"')
TS
# EVERY run, depth zero included: a gauge that appears only when it is
# non-zero cannot be told apart from a runner that stopped. A reading
# that could not see the whole queue says so on the same line (above).
if [ -z "$truncated" ]; then
    echo "ops-runner: queue host=$HOST_ID depth=$depth oldest_wait_s=$oldest_wait"
elif [ "$depth_exact" = true ]; then
    echo "ops-runner: queue host=$HOST_ID depth=$depth oldest_wait_s>=$oldest_wait truncated: $truncated"
else
    echo "ops-runner: queue host=$HOST_ID depth>=$depth oldest_wait_s>=$oldest_wait truncated: $truncated"
fi

if [ "$n" -eq 0 ]; then
    echo "ops-runner: no open ops-request for $HOST_ID"
    exit 0
fi

answered=0; refused=0; skipped=0; failed=0; held=0; planned=0; nothing=0; waiting=0
i=0
while [ "$i" -lt "$n" ]; do
    job=$(printf '%s' "$mine" | jq -c ".[$i]")
    i=$((i + 1))
    # This packet's own wait, from the pass above — one line per
    # packet, in order, so the index IS the line number. "-" when the
    # packet carries no `opened_at`.
    wait_s=$(sed -n "${i}p" "$waitsf")
    job_id=$(printf '%s' "$job" | jq -r '.id')
    short=$(printf '%s' "$job_id" | cut -c1-8)

    # Slug first, title as fallback — the boss-step.sh idiom.
    step=$(printf '%s' "$job" | jq -c '
        ((.steps // []) | map(select(.spec_slug == "execute")) | .[0])
        // ((.steps // []) | map(select(.title == "execute")) | .[0])
        // empty')
    if [ -z "$step" ]; then
        echo "ops-runner: $short has no execute step — skipping" >&2
        skipped=$((skipped + 1))
        continue
    fi
    step_status=$(printf '%s' "$step" | jq -r '.status // ""')
    step_id=$(printf '%s' "$step" | jq -r '.id')

    # HELD AFTER A REFUSED COMPLETION (backlog 865d37df, post-mortem
    # 3c3b202c). The verb runs BEFORE its completion PUT, so a refused
    # PUT used to leave the step ready and the next pass ran the verb
    # again — every open verb ~120 times on 2026-09-22. Harmless for a
    # read; not for a destructive verb, which is what ops-request v2
    # exists to carry. So a request whose refusal says the verb RAN is
    # never run again by this loop: at most once matters more than
    # eventually completed. Clearing `completion_refused` on the request
    # (a PATCH setting it to null) is the explicit act that releases it.
    # A refusal that ran nothing (a refused verb's own answer) stays
    # retryable, because retrying it repeats nothing.
    held_why=$(printf '%s' "$job" | jq -r '
        .metadata.completion_refused // empty
        | select(.verb_ran == true)
        | "HTTP \(.http // "?"): \((.reason // "") | .[0:300])"')
    if [ -n "$held_why" ]; then
        echo "ops-runner: $short held after a refused completion — its verb already ran and will not run again until completion_refused is cleared: $held_why" >&2
        held=$((held + 1))
        continue
    fi

    verb=$(printf '%s' "$job" | jq -r '.metadata.verb // ""')
    args=$(printf '%s' "$job" | jq -c '.metadata.args // []')

    # WHETHER THIS REQUEST NEEDS AN APPROVAL IS THE VERB'S TO SAY, read
    # off the reviewed allowlist — never off the packet, whose
    # `requires_approval` is only what makes the approve step ready. A
    # verb that does not serve this host is left to the refusal below.
    # Anything but an absent or literal `false` declaration reads as
    # REQUIRED: a typo in a reviewed file (`"true"`, a string) must not
    # quietly turn the gate off.
    needs_approval=$(jq -r --arg v "$verb" --arg h "$HOST_ID" '
        (.verbs[$v] // {}) as $s
        | if (($s.hosts // []) | index($h)) != null and ($s.requires_approval // false) != false
          then "yes" else "no" end' "$VERBS_FILE")

    # THE PLAN STAGE (design 17835005 q1). An approval verb's execute
    # waits, pending, behind the approve step. While it does, this
    # runner's job is to put the plan on that step for a passkey to
    # sign — once: a plan already there is the one being reviewed, and
    # re-rendering it would move the bytes under the reviewer. A request
    # whose verb cannot carry an approval, or whose plan verb refuses,
    # is closed refused here, because nothing it could wait for exists.
    if [ "$needs_approval" = yes ] && [ "$step_status" = pending ]; then
        approve=$(printf '%s' "$job" | jq -c '((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) // empty')
        a_status=$(printf '%s' "$approve" | jq -r '.status // "absent"' 2>/dev/null)
        a_plan=$(printf '%s' "$approve" | jq -r '(.metadata.plan // "") | length' 2>/dev/null)
        case "${a_status:-absent}" in
            ready|active) ;;
            *)
                echo "ops-runner: $short execute is pending and its approve step is '${a_status:-absent}' — skipping this cycle" >&2
                skipped=$((skipped + 1))
                continue
                ;;
        esac
        if [ "${a_plan:-0}" -gt 0 ]; then
            echo "ops-runner: $short waits for a passkey approval of its plan on its approve step ($verb)"
            waiting=$((waiting + 1))
            continue
        fi
        contract=$(approval_contract "$verb")
        why=$(printf '%s' "$contract" | jq -r '.refuse // empty')
        if [ -n "$why" ]; then
            if abort_request "$job" "$why"; then
                echo "ops-runner: refused $short — $why"
                refused=$((refused + 1))
            else
                failed=$((failed + 1))
            fi
            continue
        fi
        render_plan "$job" "$(printf '%s' "$contract" | jq -r '.plan_verb')" "$args" "$approve" "$verb" \
            "$(printf '%s' "$contract" | jq -r '.nothing_to_do // empty | strings')"
        case "$plan_outcome" in
            planned) planned=$((planned + 1)) ;;
            nothing) nothing=$((nothing + 1)) ;;
            refused) refused=$((refused + 1)) ;;
            *) failed=$((failed + 1)) ;;
        esac
        continue
    fi

    case "$step_status" in
        ready|active) ;;
        *)
            # pending (predicate not yet true) waits for the next
            # poll; completed/skipped is a race with the outcome step.
            echo "ops-runner: $short execute is '$step_status' — skipping this cycle" >&2
            skipped=$((skipped + 1))
            continue
            ;;
    esac

    # WHO HOLDS EXECUTE, and whether it is one of this runner's passes.
    # An approved write runs only under a claim this runner took, signed
    # as a claimant it mints below — so the holder is how the rest of the
    # walk tells "a pass of mine may have run this" from "nothing of mine
    # touched it" (security re-review, 2026-09-25).
    exec_holder=""; runner_pass=no
    if [ "$needs_approval" = yes ]; then
        exec_holder=$(printf '%s' "$step" | jq -r '.assignee_id // "" | strings')
        runner_pass=$(is_runner_pass "$exec_holder")
    fi

    # AN ANSWER THIS HOST ALREADY RECORDED IS FINISHED, NEVER RE-RUN (the
    # reviews of cars 24eb9471 and 59a6ade9; backlog 1434e6e3). A
    # completion is two writes (`complete_step`): a pass whose keys landed
    # and whose status did not — no answer, a status the server refused
    # and a person has since cleared, or a pass that died between them —
    # leaves this host's answer on an execute that is still ready (or,
    # for an approval verb, active under that pass's own claim). Running
    # the verb again to answer it a second time is the 3c3b202c failure:
    # rollback-to, delete-orphan-object, prune-registry-versions, the
    # retire-* verbs and converge are not reads. So an execute carrying a
    # disposition this host wrote gets the status alone and runs NOTHING.
    # For an approval verb the answer must also be signed `claimed_as`
    # the pass of this runner that holds the step — anyone else's is
    # "outcome unknown" below. To run a verb again on purpose, file a new
    # request.
    own_disp=$(printf '%s' "$step" | jq -r --arg h "$HOST_ID" --arg st "$step_status" \
        --arg na "$needs_approval" --arg rp "$runner_pass" '
        (.metadata // {}) as $m
        | if (($m.disposition | type) == "string") and $m.disposition != ""
             and $m.runner_host == $h
             and ($st == "ready"
                  or ($st == "active"
                      and ($na == "no"
                           or ($rp == "yes" and $m.claimed_as == (.assignee_id // null)))))
          then $m.disposition else "" end')
    if [ -n "$own_disp" ]; then
        putbodyf="$workdir/put-body"
        finish_step "$putbodyf" "$BASE/api/jobs/$job_id/steps/$step_id" "$short"
        case "${cs_code:-000}" in
            2??)
                if [ "$own_disp" = answered ]; then
                    echo "ops-runner: answered $verb on $short — this host's answer was already on the step and its status was not; sent the status alone, ran nothing"
                    answered=$((answered + 1))
                else
                    echo "ops-runner: refused $short — this host's refusal was already on the step and its status was not; sent the status alone, ran nothing"
                    refused=$((refused + 1))
                fi
                ;;
            000)
                echo "ops-runner: PUT got no answer on $short finishing this host's answer — $(cat "$workdir/put-err")" >&2
                failed=$((failed + 1))
                ;;
            *)
                # A 5xx is the server answering, and a deterministic one
                # must say so on the request rather than loop every pass.
                echo "ops-runner: PUT refused on $short finishing this host's answer — HTTP $cs_code: $(head -c 2000 "$putbodyf" | tr '\n' ' ')" >&2
                if [ "$own_disp" = answered ]; then
                    record_refused_completion "$cs_code" "$putbodyf" true
                else
                    record_refused_completion "$cs_code" "$putbodyf" false
                fi
                failed=$((failed + 1))
                ;;
        esac
        continue
    fi

    # CLAIMED, THEN SILENCE (security review, 2026-09-24). An approval
    # verb's execute is `active` under a PASS OF THIS RUNNER only because
    # that pass claimed it under a single-use approval and never
    # answered — it died, or its host did, between the claim and the
    # completion. Whether the write ran is NOT KNOWN, so this pass neither
    # runs it again (the claim was the one use) nor closes it `refused`,
    # which would tell every reader that nothing ran. It says exactly what
    # it knows, once, on the request, and leaves the step active so the
    # packet looks as troubled as it is. The claimant's id names the pass;
    # that host's journal says how far it got. Resolving it is a person's
    # act. An execute held by ANYONE ELSE is not this record: no pass of
    # this runner AS CONFIGURED NOW claimed it, and it is refused by name
    # below (re-review, 2026-09-25) — saying the outcome is unknown, never
    # that nothing ran, because an unrecognised holder may be a pass
    # signed under an earlier BOSS_OPS_ACTOR (adversarial re-review).
    if [ "$needs_approval" = yes ] && [ "$step_status" = active ] && [ "$runner_pass" = yes ]; then
        claimed_by=$(printf '%s' "$step" | jq -r '.assignee_id // "an unnamed claimant"')
        noted=$(printf '%s' "$job" | jq -r 'if .metadata.execute_outcome_unknown != null then "yes" else "no" end')
        if [ "$noted" = yes ]; then
            echo "ops-runner: $short held — claimed, outcome unknown (recorded on the request): $verb was claimed by $claimed_by and never answered" >&2
            held=$((held + 1))
            continue
        fi
        jq -cn --arg by "$claimed_by" --arg v "$verb" --arg h "$HOST_ID" \
            --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
            {execute_outcome_unknown: {
                state: "claimed, outcome unknown",
                claimed_by: $by, runner_host: $h, seen_at: $at,
                reason: "execute was claimed by \($by) under a single-use passkey approval to run \($v), and no answer from this claimant was recorded: whether \($v) ran is unknown. It will not be run again (design 17835005 q4), and the request is NOT closed refused, because that would say nothing ran. The journal on \($h) says how far that pass got; to run \($v) again, file a new request and approve its fresh plan"}}' \
            > "$workdir/unknown"
        if ! patch_err=$(curl -fsS -X PATCH -H "content-type: application/json" \
                -H "x-boss-user: $BOSS_USER" \
                ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
                --data-binary @"$workdir/unknown" \
                "$BASE/api/jobs/$job_id/metadata" 2>&1 >/dev/null); then
            echo "ops-runner: could not record claimed, outcome unknown on $short — $patch_err" >&2
            failed=$((failed + 1))
            continue
        fi
        echo "ops-runner: $short held — claimed, outcome unknown: $verb was claimed by $claimed_by and never answered; recorded on the request, never run again" >&2
        held=$((held + 1))
        continue
    fi

    # THE APPROVAL, VERIFIED BEFORE THE ARGV IS BUILT. `approved` is
    # true only when the system of record holds a presence-assured,
    # shape-bound, in-window sign-off on a completed approve step, and
    # execute has not been claimed; the write then gets the SIGNED
    # plan's hash as its last arg, appended here — the filer never
    # supplies it. Anything short of that is a refusal on the step.
    approved=false; approval_why=""; plan_sha=""; signed_at=""; claimant=""
    # A HELD EXECUTE IS REFUSED BY NAME, before any approval is judged
    # (security re-review, 2026-09-25). The claim door admits a READY
    # step only unheld or to its holder, so an execute somebody was
    # assigned is one this runner's per-pass claim can never take — the
    # dispatcher nominated every ops-request execute to the agent
    # executor until ops-request declared the step `claimable`, and each
    # approved write would have failed its claim every minute until the
    # approval expired. An ACTIVE one held by anyone but a pass of this
    # runner was taken by a claim this runner does not recognise, so what
    # ran under it is unknown here. Both are said once, on the request,
    # naming the holder.
    if [ "$needs_approval" = yes ] && [ "$step_status" = active ]; then
        approval_why="verb $verb requires a passkey approval, and its execute step is active, held by ${exec_holder:-no recorded claimant} — not a pass of this runner as it is configured now ($ACTOR on $HOST_ID). This pass ran nothing under that claim, but whether anything ran under it is unknown here: a pass signed under an earlier BOSS_OPS_ACTOR, a runner on another host, or another actor could hold it, and that holder's own record says. A step another actor took is not one this runner may run (design 17835005 q4: single-use, claimed by the pass that runs it). File the request again and approve its fresh plan"
    elif [ "$needs_approval" = yes ] && [ -n "$exec_holder" ]; then
        approval_why="verb $verb requires a passkey approval, and its execute step is assigned to $exec_holder: the claim door admits a ready step only unheld or to its holder, so this runner's single-use claim would be refused — only an unheld execute is claimed, which is why ops-request declares execute claimable. Nothing ran on $HOST_ID. File the request again and approve its fresh plan"
    elif [ "$needs_approval" = yes ]; then
        verdict=$(verify_approval "$job" "$verb" "$step_status")
        approval_why=$(printf '%s' "$verdict" | jq -r '.refuse // empty' 2>/dev/null)
        if [ -z "$approval_why" ]; then
            plan_sha=$(printf '%s' "$verdict" | jq -r '.plan_sha256 // empty' 2>/dev/null)
            signed_at=$(printf '%s' "$verdict" | jq -r '.signed_at // empty' 2>/dev/null)
            case "${plan_sha:-empty}" in
                empty | *[!0-9a-f]*)
                    approval_why="verb $verb requires a passkey approval, and this runner could not hash the approved plan (verdict: ${verdict:-none})" ;;
                *)
                    if [ "${#plan_sha}" -ne 64 ]; then
                        approval_why="verb $verb requires a passkey approval, and the approved plan's hash is not 64 hex digits: $plan_sha"
                    fi ;;
            esac
        fi
        # A PLAN THIS RUNNER DID NOT WRITE IS REFUSED BY THIS RUNNER
        # (security review, 2026-09-24). Everything above reads fields on
        # the approve step, and anyone who can write its plan can write a
        # matching rendered_plan_sha256 beside it. What they cannot do is
        # make the plan verb produce their bytes: so it runs again, here,
        # from the signed args, and the signed plan must be exactly what
        # it renders now. A plan typed onto the step by anyone else — or a
        # state that moved since the signature — is refused before the
        # claim, naming both hashes, rather than left to the write's own
        # re-render to catch.
        if [ -z "$approval_why" ]; then
            plan_verb=$(printf '%s' "$verdict" | jq -r '.plan_verb // empty')
            run_plan_verb "$job_id" "$plan_verb" "$args"
            if [ -n "$rv_why" ]; then
                approval_why="verb $verb requires a passkey approval, and the signed plan could not be checked against a fresh render: $rv_why"
            elif [ "$rv_sha" != "$plan_sha" ]; then
                approval_why="verb $verb requires a passkey approval, and the signed plan hashes to $plan_sha while the plan verb $plan_verb renders $rv_sha on $HOST_ID now: the plan on the approve step is not what this runner renders here (written by someone else, or the state moved since it was signed). Nothing runs on it; file the request again and approve its fresh plan"
            else
                approved=true
                args=$(printf '%s' "$args" | jq -c --arg h "$plan_sha" '. + [$h]')
            fi
        fi
    fi

    # One jq pass over the ALLOWLIST decides: either a refusal reason
    # or a fully resolved argv (see `decide` above).
    if [ -n "$approval_why" ]; then
        decision=$(jq -cn --arg r "$approval_why" '{refuse: $r}')
    else
        decision=$(decide "$verb" "$args" "$approved")
    fi

    outf="$workdir/out"
    disp=""; rc_str=""; script=""; dur_ms=""
    eff_line=""; eff_unproven=""; eff_unread=""
    streamsf="$workdir/streams"
    printf 'null\n' > "$streamsf"
    streams_unread=""
    reason=$(printf '%s' "$decision" | jq -r '.refuse // empty')
    if [ -n "$reason" ]; then
        disp="refused"; rc_str=""
        printf '%s' "$reason" > "$outf"
    else
        # Build the argv as positional parameters. Newline-split is
        # EXACT here, not hopeful: fixed argv words are reviewed file
        # content and substituted args have passed patterns that admit
        # no whitespace.
        set --
        while IFS= read -r w; do
            set -- "$@" "$w"
        done <<ARGV
$(printf '%s' "$decision" | jq -r '.argv[]')
ARGV
        # argv[0]: a repo-relative script resolves against this checkout
        # and must exist there — an absent script is a REFUSAL naming the
        # path, not an exec error dressed up as an answer. A word with no
        # slash is a bare command for PATH; an absolute path is left as
        # the allowlist wrote it (the lint refuses those).
        script=""
        case "$1" in
            /*) script="$1" ;;
            */*) script="$OPS_REPO_ROOT/$1" ;;
        esac
        if [ -n "$script" ] && [ ! -x "$script" ]; then
            reason="verb $verb names a script not in this checkout: $1 (resolved to $script under OPS_REPO_ROOT=$OPS_REPO_ROOT)"
            disp="refused"; rc_str=""
            printf '%s' "$reason" > "$outf"
        fi
    fi
    # SINGLE-USE: THE CLAIM IS THE USE (design 17835005 q4). An approved
    # execute is claimed `active` BEFORE its argv runs, and an `active`
    # one is never run again (it is "claimed, outcome unknown" above), so
    # a second pass — this runner's next minute, or a second runner —
    # can never run the same approval twice, whether or not this one
    # finishes. A claim the server does not take runs nothing: the next
    # pass sees a ready step and judges the approval again, window
    # included.
    #
    # Two things make that true rather than hoped (security review,
    # 2026-09-24):
    #
    # - THE RECORD IS READ AGAIN IMMEDIATELY BEFORE THE CLAIM. The list
    #   this pass judged was read at its top, and the plan re-render
    #   above takes seconds; another pass may have claimed execute, or
    #   the sign-off may have been withdrawn, since. The one job is read
    #   again and judged again, and a record that moved is left for the
    #   next pass with nothing written — a stale read decides nothing.
    # - THE CLAIM IS A COMPARE-AND-SET. It was a PUT of
    #   {"status":"active"}, which the server overlays on whatever the
    #   step is, so two passes that both read `ready` both "claimed". It
    #   goes through the claim door, whose WHERE clause admits exactly
    #   one ready->active. That door is idempotent for its HOLDER, so the
    #   claim is signed as a claimant unique to this pass — the runner's
    #   account, its host, the pass — and a second pass is a second
    #   claimant, which the door refuses 409 naming the holder.
    if [ "$disp" != "refused" ] && [ "$approved" = true ]; then
        fresh=""
        if ! fresh=$(curl -fsS -H "x-boss-user: $BOSS_USER" \
                ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
                "$BASE/api/jobs/$job_id" 2>&1); then
            echo "ops-runner: could not re-read $short before claiming its execute — $fresh — $verb NOT run" >&2
            failed=$((failed + 1))
            continue
        fi
        fresh_status=$(printf '%s' "$fresh" | jq -r '
            ((.steps // []) | map(select(.spec_slug == "execute")) | .[0].status) // "absent"' 2>/dev/null)
        fresh_holder=$(printf '%s' "$fresh" | jq -r '
            ((.steps // []) | map(select(.spec_slug == "execute")) | .[0].assignee_id) // "" | strings' 2>/dev/null)
        moved=""
        if [ "$fresh_status" != ready ]; then
            moved="execute is now '${fresh_status:-unreadable}'"
        elif [ -n "$fresh_holder" ]; then
            moved="execute is now held by $fresh_holder, and the claim door admits only an unheld one"
        elif [ "$(printf '%s' "$fresh" | jq -c '.metadata.args // []' 2>/dev/null)" != "$(printf '%s' "$job" | jq -c '.metadata.args // []')" ]; then
            moved="the request's args changed"
        else
            again=$(verify_approval "$fresh" "$verb" ready)
            again_why=$(printf '%s' "$again" | jq -r '.refuse // empty' 2>/dev/null)
            if [ -n "$again_why" ]; then
                moved="the approval no longer holds: $again_why"
            elif [ "$(printf '%s' "$again" | jq -r '.plan_sha256 // empty')" != "$plan_sha" ]; then
                moved="the signed plan is no longer the one checked"
            fi
        fi
        if [ -n "$moved" ]; then
            echo "ops-runner: $short moved between its read and its claim ($moved) — nothing claimed or written; the next pass judges it afresh" >&2
            skipped=$((skipped + 1))
            continue
        fi
        claimant=$(mint_claimant)
        claim_user=$(jq -cn --arg id "$claimant" '{id: $id, role: "platform-admin", access_tier: "operator",
            territory_account_ids: [], direct_report_ids: [], department: "platform"}')
        : > "$workdir/claim-body"
        claim_code=$(curl -sS -o "$workdir/claim-body" -w '%{http_code}' -X POST \
                -H "x-boss-user: $claim_user" \
                ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
                "$BASE/api/jobs/$job_id/steps/$step_id/claim" 2>"$workdir/claim-err") || claim_code=""
        case "${claim_code:-000}" in
            2??) ;;
            *)
                echo "ops-runner: could not claim $short's approved execute as $claimant (HTTP ${claim_code:-none}: $(head -c 2000 "$workdir/claim-body" | tr '\n' ' ')$(cat "$workdir/claim-err")) — $verb NOT run" >&2
                failed=$((failed + 1))
                continue
                ;;
        esac
        echo "ops-runner: claimed $short's execute as $claimant under a passkey approval signed at $signed_at (plan-sha256 $plan_sha)"
    fi
    if [ "$disp" != "refused" ]; then
        if [ -n "$script" ]; then
            shift; set -- "$script" "$@"
        fi
        # A verb may declare its own `timeout` in the allowlist (a
        # reviewed number, like its argv); otherwise the runner's
        # default applies. publish-github-pr's first push of the whole
        # tree is minutes, not seconds.
        verb_timeout=$(printf '%s' "$decision" | jq -r '.timeout // empty')
        rawf="$workdir/raw"
        # The packet's id rides in the verb's ENVIRONMENT, never its
        # argv: a verb that starts a longer-running unit (converge-now)
        # can leave the unit a note saying which packet asked, so the
        # unit's outcome lands back on that packet instead of the
        # `answered` that `systemctl start --no-block` earns by merely
        # being accepted (backlog d66f92b2). Data, not program text.
        #
        # SO DOES THE ACTOR. A verb whose argv is the tree's own CLI
        # (`boss prove … --unattended`, run-car-probe since backlog
        # 9f00a805 car 2) signs its jobs-API writes as BOSS_ACTOR and
        # REFUSES a write unnamed (CLAUDE.md §Doors); this runner's own
        # account is the one it should sign as, the same identity the
        # step completion below carries. A unit that set BOSS_ACTOR
        # itself wins — the drop-in is the operator's say.
        # HOW LONG IT TOOK, taken around the exec and recorded beside
        # the exit (backlog b7bfe821). opened_at-to-closed_at is the
        # only duration the record used to carry and it is dominated
        # by up to a minute of this runner's poll latency, so costing
        # run-car-probe on 2026-09-19 meant inferring per-probe time
        # from the spread within a simultaneous batch. The number is
        # here, free, at the moment the exit is written. Milliseconds
        # because a probe is seconds. A `date` without GNU's %N leaves
        # a non-digit in the stamp; the guard below then records no
        # duration rather than a nonsense one.
        #
        # WHAT IS RUNNING, SAID WHEN IT STARTS (backlog b81ff4ca). This
        # runner is serial, and until 2026-09-28 it named a request only
        # when it ENDED: the forge journal read `answered
        # publish-github-pr` at 02:28:59 and then nothing for thirteen
        # minutes while eight requests queued, so the item was filed as
        # a silent runner with three wrong hypotheses — and the runner
        # had been running read-publish-checks 444d0f22 the whole time,
        # which only its 02:42:28 `answered` line said. This line makes
        # the silence attributable from the journal as it happens: the
        # verb, the request, how long it queued and how long it may hold
        # the runner.
        case ${wait_s:-empty} in
            empty|*[!0-9]*) queued="queued unknown" ;;
            *) queued="queued ${wait_s}s" ;;
        esac
        echo "ops-runner: running $verb on $short ($queued, timeout ${verb_timeout:-$OPS_TIMEOUT}s)"
        t0=$(date -u +%s%3N 2>/dev/null)
        if [ "$(printf '%s' "$decision" | jq -r '.capture // empty')" = separate-streams ]; then
            OPS_REQUEST_ID="$job_id" BOSS_ACTOR="${BOSS_ACTOR:-$ACTOR}" \
                timeout "${verb_timeout:-$OPS_TIMEOUT}" "$@" \
                > "$workdir/stdout" 2> "$workdir/stderr" < /dev/null
            rc=$?
            t1=$(date -u +%s%3N 2>/dev/null)
            # Opt-in diagnostic output retains both streams, stdout first;
            # ordinary verbs keep their existing interleaved output below.
            cat "$workdir/stdout" "$workdir/stderr" > "$rawf"
            if stream_record "$workdir/stdout" > "$workdir/stdout-record" \
                && stream_record "$workdir/stderr" > "$workdir/stderr-record" \
                && jq -cn --slurpfile out "$workdir/stdout-record" \
                    --slurpfile err "$workdir/stderr-record" '{stdout:$out[0],stderr:$err[0]}' > "$streamsf"; then
                :
            else
                printf 'null\n' > "$streamsf"
                streams_unread="Separate stream capture failed; diagnostic output and the verb exit are retained, but no structured result is proven"
            fi
        else
            OPS_REQUEST_ID="$job_id" BOSS_ACTOR="${BOSS_ACTOR:-$ACTOR}" \
                timeout "${verb_timeout:-$OPS_TIMEOUT}" "$@" > "$rawf" 2>&1 < /dev/null
            rc=$?
            t1=$(date -u +%s%3N 2>/dev/null)
        fi
        dur_ms=""
        case ${t0:-empty}${t1:-empty} in
            *[!0-9]*) ;;
            *)
                dur_ms=$((t1 - t0))
                [ "$dur_ms" -ge 0 ] || dur_ms=0   # a clock step is not a negative run
                ;;
        esac
        size=$(wc -c < "$rawf")
        if [ "$size" -gt "$OPS_OUTPUT_CAP" ]; then
            head -c "$OPS_OUTPUT_CAP" "$rawf" > "$outf"
            printf '\n[ops-runner: output truncated — kept %s of %s bytes]\n' \
                "$OPS_OUTPUT_CAP" "$size" >> "$outf"
        else
            cat "$rawf" > "$outf"
        fi
        if [ "$rc" -eq 124 ]; then
            printf '\n[ops-runner: command killed at %ss timeout]\n' "${verb_timeout:-$OPS_TIMEOUT}" >> "$outf"
        fi
        disp="answered"; rc_str="$rc"

        # AN EXIT 0 IS NOT AN EFFECT (backlog fdbb447e part 1, design
        # 3036296f mechanism B, David 2026-09-27). `answered` says the
        # verb ran and `exit_code` says it did not fail; neither says it
        # changed what it exists to change. The daily prune's proof read
        # the outcome alone and counted a REFUSED run as proof; the data
        # move's proof could not tell the old copy from the new. So a
        # verb file declares its EFFECT: the regex of the line its script
        # prints only after it has read back what it changed (a re-list,
        # a re-stat, a re-query) — or, for a dry run, the line saying it
        # changed nothing. The judgement is made ONCE, here, where the
        # verb file and the whole output are both in hand, and recorded
        # on the step beside the exit, so every reader — `boss ops
        # --wait`, `verb_failure` (the answered-ops-request judges), a
        # car's recorded probe — reads one verdict instead of re-deriving
        # it:
        #   effect           the last line matching the declared regex;
        #   effect_unproven  exit 0 and NO line matched (or the regex
        #                    could not be judged) — the run did not show
        #                    its effect, and every reader fails it;
        #   effect_unread    the verb declares no read-back yet — its
        #                    file's own `effect_unread` reason, copied so
        #                    the run says out loud that exit 0 is all it
        #                    proves.
        # Only an exit 0 is judged: a failed run is already a failure,
        # and a not-yet (75) or a kill (124) claims no effect at all.
        # Nothing is refused here — the verb has already run; this is the
        # record of what the run showed (DR rule 62dac114: no new
        # refusal on a human path).
        #
        # The WHOLE output is judged ("$rawf"), never the recorded copy:
        # "$outf" is cut at OPS_OUTPUT_CAP, and a verb says its result
        # LAST — the adversarial review of this car found every
        # prune-registry-versions run and sweep-archive-branches 54547b33
        # (184 KB) past the cap, so judging the cut copy would have written
        # a false EFFECT NOT SHOWN into the record for exactly the verbs
        # that did the most. Only the one line kept is capped (2,000 bytes).
        if [ "$rc" -eq 0 ]; then
            eff_re=$(printf '%s' "$decision" | jq -r '.effect // empty')
            if [ -n "$eff_re" ]; then
                if eff_found=$(jq -nRr --arg re "$eff_re" \
                        '[inputs | select(test($re))] | last // empty | .[0:2000]' \
                        < "$rawf" 2> "$workdir/effect-err"); then
                    if [ -n "$eff_found" ]; then
                        eff_line="$eff_found"
                    else
                        eff_unproven="exit 0, and no line of the output matches the effect this verb declares (${eff_re}) — the run did not show that it changed what it exists to change"
                    fi
                else
                    eff_unproven="exit 0, and the effect this verb declares (${eff_re}) could not be judged: $(head -c 500 "$workdir/effect-err")"
                fi
            else
                eff_unread=$(printf '%s' "$decision" | jq -r '.effect_unread // empty')
            fi
        fi
    fi

    # Only the keys this answer records (see header): they go through the
    # step merge door, so nothing the runner merely READ rides back over
    # the row (backlog 2aa2b19e). The output rides --rawfile so
    # arbitrary command output stays data. A refusal also writes the
    # reason under its own name: `output` on a refusal IS the reason
    # (one source — the same string the journal line below prints),
    # and a reader of the packet should not have to know that.
    # An approved run names the approval it ran under: the SIGNED plan's
    # hash — the argument the write's script re-rendered against — and
    # when the passkey signed it — and the claimant that took it, which
    # is the pass a "claimed, outcome unknown" would have named.
    # Every key an answer MAY carry and this one does not goes as null,
    # which the merge door reads as a delete: through that door an omitted
    # key keeps whatever an earlier pass left, and one record must not
    # mix two passes' answers (the re-review of car 59a6ade9).
    payloadf="$workdir/payload"
    jq -cn --rawfile out "$outf" --slurpfile streams "$streamsf" --arg su "$streams_unread" \
        --arg d "$disp" --arg rc "$rc_str" --arg h "$HOST_ID" --arg ms "${dur_ms:-}" \
        --arg ps "$plan_sha" --arg sa "$signed_at" --arg ap "$approved" --arg ca "$claimant" \
        --arg el "$eff_line" --arg eu "$eff_unproven" --arg er "$eff_unread" '
        {disposition: $d, output: $out, runner_host: $h, streams:$streams[0],
         streams_unread:(if $su == "" then null else $su end),
         exit_code: (if $rc == "" then null else $rc end),
         duration_ms: (if $ms == "" then null else ($ms | tonumber) end),
         reason: (if $d == "refused" then $out else null end),
         claimed_as: (if $ca == "" then null else $ca end),
         effect: (if $el == "" then null else $el end),
         effect_unproven: (if $eu == "" then null else $eu end),
         effect_unread: (if $er == "" then null else $er end)}
        + (if $ap == "true" and $d == "answered"
           then {approved_plan_sha256: $ps, approval_signed_at: $sa}
           else {approved_plan_sha256: null, approval_signed_at: null} end)' \
        > "$payloadf"

    # THE QUEUE READING RIDES THE REQUEST (1ffb3305): the depth this
    # run faced, and what THIS request waited before the runner reached
    # it. Both are facts about the queue, not about the verb, so they
    # have no home on the execute step — and riding the merge door
    # makes the depth a question the jobs API answers, which the
    # journal line alone does not. This is the wait BEFORE the verb,
    # not the run: the run's own `duration_ms` rides the execute step
    # with the exit it belongs to (b7bfe821).
    #
    # THE VERB'S EXIT DOES NOT RIDE HERE (backlog 50fede8b). It used to:
    # f47861a5 added `exit` beside the step's `exit_code` so a reader of
    # the close would see it where the outcome is. Nothing ever read the
    # copy — `boss ops --wait`, `verb_failure` (the whole answered-ops-
    # request judge family) and the yard's shed and signals all read
    # `exit_code` off the execute step, and a list read carries each
    # row's steps, so a request-level reader never had to fetch them
    # separately. Two spellings of one fact, written by one act and held
    # equal by nothing, is what CLAUDE.md §9a refuses: the first writer
    # to move one without the other (a retry, a hand correction, a
    # second runner) hands a reader a stale exit. One spelling now:
    # `exit_code`, on the step, written by the PUT below.
    #
    # A refusal ran nothing and waited in no queue this run answered,
    # so it writes nothing here. A failed merge does not withhold the
    # answer — the step completion below still lands — but it is
    # counted, and the unit goes red for it.
    if [ "$disp" = "answered" ]; then
        exitf="$workdir/exit"
        # A lower bound rides under its own key, never as `queue_depth`
        # — a reader of that key takes it as the count (2cfb4562).
        jq -cn --arg w "$wait_s" --argjson d "$depth" --arg exact "$depth_exact" '
            (if $exact == "true" then {queue_depth: $d} else {queue_depth_at_least: $d} end)
            + (if $w == "-" then {} else {queued_s: ($w | tonumber)} end)' > "$exitf"
        if ! patch_err=$(curl -fsS -X PATCH -H "content-type: application/json" \
                -H "x-boss-user: $BOSS_USER" \
                ${MT_HDR:+-H "$MT_HDR"} ${RC_HDR:+-H "$RC_HDR"} \
                --data-binary @"$exitf" \
                "$BASE/api/jobs/$job_id/metadata" 2>&1 >/dev/null); then
            echo "ops-runner: PATCH queue_depth=$depth failed on $short — $patch_err" >&2
            failed=$((failed + 1))
        fi
    fi

    # A REFUSED COMPLETION SAYS WHY, ON THE REQUEST (post-mortem
    # 3c3b202c). This PUT was `curl -f`, which throws the response body
    # away. On 2026-09-22 00:50-02:54 UTC the server refused every
    # completion on both hosts with a 409 whose body named the blocker
    # (`step has unresolved blockers`, the ops-request v2 `approve`
    # step; fixed in bd0f3369). The journal held "409" and nothing else,
    # the red unit had no reader, and the verbs re-ran once a minute for
    # two hours. So the status and the server's words are kept. A
    # refusal (the server answered, and not 2xx) is also written onto
    # the request through the metadata door, which kept working all
    # night: the packet then says why it is stuck. A transport failure
    # (no answer at all) has no words to keep and no door to write
    # through, so it stays a journal line and a red unit, as before.
    #
    # The two writes are `complete_step`'s: the keys through the merge
    # door, then the status alone, which is sent once more when it loses
    # the step race (backlog 2a6d0b86) — before that the one 409 recorded
    # `completion_refused` and, the verb having run, HELD the request for
    # a human over a refusal whose own words say nothing was written. A
    # refused merge, a second lost race, or any other refusal takes the
    # refusal path below with the server's words — never a loop.
    putbodyf="$workdir/put-body"
    complete_step "$payloadf" "$putbodyf" "$BASE/api/jobs/$job_id/steps/$step_id" "$short"
    put_code=$cs_code
    # NO ANSWER AT ALL, after `step_write` rode out its retries, is the
    # one outcome recorded nowhere: a system of record that does not
    # answer cannot be written to. The next pass finds this host's answer
    # on the step if the keys landed, and finishes it without running the
    # verb again. A 5xx is the SERVER ANSWERING — a failed blocker check,
    # a failed protocol read, a repository error — so it takes the refusal
    # path below, recorded with `verb_ran`, and the held check stops a
    # re-run, as on main (the re-review of car 59a6ade9: read as an outage
    # it recorded nothing, and every later pass re-ran the verb).
    case "${put_code:-000}" in
        2??) ;;
        000)
            echo "ops-runner: $cs_write got no answer on $short — $(cat "$workdir/put-err")" >&2
            failed=$((failed + 1))
            continue
            ;;
        *)
            said=$(head -c 2000 "$putbodyf" | tr '\n' ' ')
            echo "ops-runner: $cs_write refused on $short — HTTP $put_code: $said" >&2
            if [ "$disp" = answered ]; then
                record_refused_completion "$put_code" "$putbodyf" true
            else
                record_refused_completion "$put_code" "$putbodyf" false
            fi
            failed=$((failed + 1))
            continue
            ;;
    esac

    if [ "$disp" = "answered" ]; then
        echo "ops-runner: answered $verb on $short (exit $rc_str, ${size}B, ${dur_ms:--}ms)"
        answered=$((answered + 1))
    else
        echo "ops-runner: refused $short — $reason"
        refused=$((refused + 1))
    fi
done

echo "ops-runner: $HOST_ID answered=$answered refused=$refused skipped=$skipped failed=$failed held=$held planned=$planned nothing=$nothing waiting=$waiting"
[ "$failed" -eq 0 ] || exit 1
