#!/usr/bin/env bash
#
# publish-drift — publish EVERY platform workflow kind the tree moved
# ahead of, as one act, from the converged checkout on boss-gcp; list
# the ones it must not touch, field by field; answer one table.
#
# WHY IT EXISTS (backlog a2f97942, ratified by retro 27fad542)
# ---------------------------------------------------------------------
# `publish-workflow` (the sibling in this directory) is ONE kind per
# request by design — bounded, with the refusals that make it safe to
# hand to a button. On 2026-09-18 13:4x that bound became the operator's
# loop: after the maintenance-audience car landed, 23 publish-workflow
# ops-requests were hand-scripted one per kind (26 in the retro's
# window), preceded by a hand `until --check ok` wait for boss-gcp's
# checkout to carry the train. The gap is one level up: after a train
# lands, every kind the tree moved ahead of should be published as one
# act — the Drift tab already computes the set (protocol-drift.sh
# daily; /api/workflows drift) and its approve fires one publish — and
# the wait should be a verdict, not a loop an operator runs.
#
# THE RULE, INHERITED WHOLE
# ---------------------------------------------------------------------
# This verb re-derives NOTHING. The drift set is `publish-workflow.sh
# <kind> --check` per kind in the bundle, classified by that verb's
# documented exit codes (its EXIT block is the contract; the phrases
# below are pinned to its text by boss-testing/tests/publish_drift_sh.rs):
#
#   0  TREE-AHEAD  live is a row the tree once said, now behind — publish
#   5  EQUAL       nothing to publish — skip
#   6  REFUSED     live carries what the tree NEVER SAID — LISTED, field
#                  by field, copied from the sub-verb; NEVER published
#                  by this verb. There is no --force-tree here: that
#                  decision is the operator's, one kind at a time, at the
#                  Drift tab's approve (publish-workflow --force-tree).
#   4  REFUSED     the tree's row does not lint — listed
#   8  no live row — the seed admits it; listed as pending, skipped
#   9  HELD        the kind is held out of every unattended publish and
#                  the tree is ahead — listed as held, never published;
#                  OR the server judges its live row passkey-protected
#                  (the sub-verb's PASSKEY-PROTECTED line, backlog
#                  5af64799) — held the same way, with the server's
#                  reason, and --file-held files its packet
#  75/78           the sub-verb could not answer / is misconfigured —
#                  THIS RUN STOPS with the same code: half a drift set
#                  judged from half a registry is the confident wrong
#                  answer (CLAUDE.md §Doors)
#
# In --for-real, each TREE-AHEAD kind is published in bundle order
# through `publish-workflow.sh <kind>` itself — the CLI's checked
# sequence, the read-back, the actor rule, all its own — and each row
# of the table reports what that verb confirmed. A publish it did NOT
# confirm (7) is reported on its row and makes this verb's exit 1; the
# kinds after it still run, because each kind's publish is independent
# and a stopped loop would hand the operator the loop back.
#
# A HELD KIND IS NEVER PUBLISHED HERE (backlog 083d240e)
# ---------------------------------------------------------------------
# A row that turns on a REFUSAL goes live at a deliberate registry
# publish — Pacific hours, David present, a positive control straight
# after, a named rollback (design b08725c2; design 09618594 question
# `signer`). This verb is the opposite of that: a rule files its
# --for-real on any clean check, at any hour. Found 2026-10-06 by
# builder run 9f9bb3da: the signer half of row E would have published
# itself on the first clean check after its merge — and AGAIN after a
# rollback, because republishing the old row leaves the tree ahead of
# live. That day only ten unrelated refusals held it off (ops-request
# 090bcfc9: would publish 1, skipped 56 equal, refused 10).
#
# So the tree says which kinds this verb must leave alone, and
# infra/gcp/workflow-holds.py is the ONE reader of it (its header is the
# contract): every kind whose row declares a field `writer` or a step
# `executor` is held BY DEFAULT, and infra/platform/workflow-holds/
# <kind>.toml overrides in both directions (`held` with a why and what
# lifts it, `released` to hand a default-held row back).
#
#   - A held kind is asked `--check` like any other (so its row states
#     what the registry holds: ahead, equal, never said) and is then
#     HELD whatever the answer: counted as `held`, in neither `would
#     publish` nor `refused`, so it never stops the other kinds.
#   - It is named every run, in its table row and on a line of its own
#     (`publish-drift: held <kind> (<source>) — <why> — lifts: <...>`),
#     and the verdict line states `held H` even when H is 0.
#   - --for-real reads the holds AGAIN before every publish, and
#     compares the checkout's HEAD to the one line 1 named: the check
#     that filed this run may predate a hold, and a checkout that moved
#     mid-run was judged as another tree. A hold that appeared is
#     honoured; a moved checkout stops the remaining publishes (exit 1).
#   - FAIL CLOSED: holds that cannot be read (see the reader) refuse
#     the whole run in BOTH modes, exit 78, before any kind is asked —
#     no table and no verdict line, so the rule that reads a clean
#     check finds nothing to act on.
#
# THE SUB-VERB HOLDS TOO, and that is the layer that binds (review R1
# of the signer car, run 7cee49b9): publish-workflow.sh asks the same
# reader and refuses a held kind itself (exit 9), because the
# `publish-workflow` verb is a second machine road that needs no
# approval. So this verb's own look is what makes the answer LOUD (the
# row, the line, the count) and the sub-verb's is what makes it TRUE:
# a 9 from `--check` for a kind this run did not know as held re-reads
# the holds and holds it, and a 9 at the publish itself is a held row,
# not a failed publish.
#
# The one door that still publishes a held kind is the deliberate one:
# `boss workflow publish <kind> <file>`, run by a person from a
# checkout. It does not lift the hold; a car that changes the tree does.
#
# THE CHECKOUT IS NAMED FIRST, AND A STALE ONE IS `not yet`
# ---------------------------------------------------------------------
# The first line states the sha of the checkout read, so a publish from
# a stale checkout is visible on the packet. Then the newest converged
# train is read off the system of record (the newest CLOSED pr-train
# whose `merged` step carries a `merge_ref`, the way
# infra/forge/landed-train-shas.lib.sh reads it) and must be in this
# checkout's history; otherwise `not yet: checkout at X, main at Y`,
# exit 75, and nothing is asked — the operator's `until --check ok` loop
# becomes a verdict a rule can re-file on the next converge.
#
# --file-held: A HELD KIND'S PUBLISH IS FILED ON ITS OWNER (design
# b1f319bc, David 2026-11-05; backlog 7955a488)
# ---------------------------------------------------------------------
# A held kind whose tree moved ahead of live is an obligation on a
# person, and until this mode it ended as a `held` line on a check
# nobody was asked to act on: on 2026-11-05 the held ops-request row's
# publish lived only in a handoff note. `--file-held` checks every kind,
# including kinds whose hold was lifted: their outstanding publish
# obligations can now be satisfied by the unattended road. For
# each that answers 9 with the tree ahead, files one publish-a-protocol
# packet carrying what the owner's passkey will sign: the kind, its
# file, the file's sha256 at this checkout, the live version, the
# checkout sha, the sub-verb's dry-run lint line and its rendered diff
# (its indented DIFF lines, copied, never re-derived). It publishes
# nothing; the publish runs on boss-gcp after the owner signs (rule
# publish-a-protocol-files-its-machine-publish).
#
#   - ONE PACKET PER SPEC, EVER. Every publish-a-protocol packet is read
#     (every page, every status) and a packet for the same kind and
#     sha256 — open, published, declined — files nothing: a declined
#     spec is an answer, and asking it again on every converge (~28 a
#     day) would be a passkey prompt nobody wants. A new edit is a new
#     sha256 and a new question.
#   - A NEWER EDIT SUPERSEDES. An OPEN packet for the kind with another
#     sha256 is marked `superseded_by` the new one through the metadata
#     merge door, which readies its `superseded` terminal: its signature
#     could no longer publish anything (the script refuses other bytes).
#   - A DARK READ IS NOT AN EMPTY LIST. A known answer is asked first —
#     the ops-request this run answers exists, so that listing is never
#     empty — and a list whose rows do not reach its total files nothing
#     (75): a duplicate per converge is worse than one converge's
#     silence, and the next converge re-derives the finding.
#
# USAGE
#   publish-drift.sh [--check | --for-real | --file-held]   (default: --check)
#
# ENV
#   BOSS_JOBS_URL                (required) the system of record
#   BOSS_PUBLISH_WORKFLOW_REPO   the checkout whose bundle is read — the
#                                SAME variable the sub-verb reads, so the
#                                two cannot read different trees
#                                (default: the one this script is in)
#   BOSS_PUBLISH_WORKFLOW_SH     the sub-verb (default: publish-workflow.sh
#                                beside this script; a test plants one)
#   BOSS_ACTOR                   who the publishes sign as (default
#                                automation:ops-runner, the account
#                                RUNNING it); also signs the train read
#   OPS_REQUEST_ID               the packet, from the runner; printed
#
# EXIT
#   0  the verdict was reached: every tree-ahead kind published and
#      confirmed (or, with --check, listed) — refusals listed count as
#      the operator's decision, not this verb's failure
#   1  at least one publish was NOT confirmed, a sub-verb run ended
#      outside its documented codes, or the checkout moved (or its
#      holds stopped being readable) while the publishes ran; under
#      --file-held, a packet that should have been filed was not
#   2  usage: a mode outside --check / --for-real / --file-held
#  75  cannot answer: the checkout is behind the newest converged train
#      (not yet), the system of record could not be read, or a kind's
#      registry row could not
#  78  configuration: no BOSS_JOBS_URL, no jq/curl/git/python3, no
#      sub-verb, no bundle in the checkout, no hold reader — or the
#      tree's holds cannot be read (REFUSED: nothing asked, nothing
#      published)
#
# Runs as root under the ops-runner with NO HOME; every git read drops
# to the checkout's owner, as the sibling does.
set -uo pipefail

NAME="publish-drift"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '%s: %s\n' "$NAME" "$*"; }
usage() {
    sed -n '/^# USAGE/,/^# EXIT/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
}

# --- arguments ------------------------------------------------------------
MODE="${1:---check}"
if [ $# -gt 1 ]; then
    echo "$NAME: usage: $(basename "$0") [--check | --for-real | --file-held]" >&2
    usage; exit 2
fi
case "$MODE" in
    --check|--for-real|--file-held) ;;
    *)
        echo "$NAME: usage: mode '$MODE' is not one of --check, --for-real, --file-held (there is no --force-tree here: a live row the tree never said is the operator's decision at the Drift tab, one kind at a time)" >&2
        exit 2 ;;
esac

# --- configuration --------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$NAME: BOSS_JOBS_URL is not set, and there is no safe default — a publish against the wrong instance answers instead of erroring (CLAUDE.md §Doors). The ops-runner's unit pins it." >&2
    exit 78
fi
for tool in jq curl git python3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$NAME: no $tool on PATH — nothing compared, nothing published" >&2; exit 78; }
done
# The hold reader is TOML read by tomllib; without it the holds cannot
# be read, and unread holds are not absent holds.
python3 -c 'import tomllib' 2>/dev/null || { echo "$NAME: python3 has no tomllib (3.11+) — the tree's holds cannot be read, so nothing compared, nothing published" >&2; exit 78; }
HOLDS_PY="$SELF_DIR/workflow-holds.py"
[ -f "$HOLDS_PY" ] || { echo "$NAME: the hold reader $HOLDS_PY is missing — which kinds are held cannot be read, so nothing compared, nothing published" >&2; exit 78; }

REPO="${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..}"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || { echo "$NAME: checkout ${BOSS_PUBLISH_WORKFLOW_REPO:-$SELF_DIR/../..} is not a directory this process can enter" >&2; exit 78; }
# The sub-verb reads the same variable: one checkout, named once.
export BOSS_PUBLISH_WORKFLOW_REPO="$REPO"
BUNDLE_REL="infra/platform/workflows"
BUNDLE="$REPO/$BUNDLE_REL"
[ -d "$BUNDLE" ] || { echo "$NAME: the checkout $REPO has no $BUNDLE_REL directory — there is no bundle to compare" >&2; exit 78; }

SUB="${BOSS_PUBLISH_WORKFLOW_SH:-$SELF_DIR/publish-workflow.sh}"
if [ ! -x "$SUB" ]; then
    echo "$NAME: the sub-verb $SUB is not an executable file — the comparison and the publish are ITS, and this verb re-derives neither" >&2
    exit 78
fi

# The account running it signs the train read and, through the
# sub-verb, every publish — the identity rule every `boss` verb applies.
export BOSS_ACTOR="${BOSS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$BOSS_ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# The machine token rides to curl in a 0600 file, never in its argv,
# where every local user reads it in ps (backlog 5f3ad356). After the
# trap above, so the lib's cleanup is chained in front of it.
# shellcheck source=infra/lib/secret-header.sh
. "$SELF_DIR/../lib/secret-header.sh" || { echo "$NAME: $SELF_DIR/../lib/secret-header.sh is missing — nothing compared, nothing published" >&2; exit 78; }
machine_token_header MT_HDR "$BOSS_JOBS_URL" || exit 78

# --- git, as the checkout's owner -------------------------------------------
# The one definition of that bound, infra/lib/git-owner.sh (backlog 5ff118da).
# shellcheck source=infra/lib/git-owner.sh
. "$SELF_DIR/../lib/git-owner.sh" || { echo "$NAME: $SELF_DIR/../lib/git-owner.sh is missing — nothing compared, nothing published" >&2; exit 78; }
if ! git_owner_of "$REPO"; then
    echo "$NAME: cannot resolve the owner of $REPO (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — no passwd entry, so there is no account to read git as" >&2
    exit 78
fi

# --- the first line: which checkout, at what sha ------------------------------
HEAD="$(as_owner "git -C '$REPO' rev-parse HEAD" 2>"$TMP/git.err")"
if [ -z "$HEAD" ]; then
    echo "$NAME: cannot read $REPO as a git checkout (as '$OWNER'): $(tr '\n' ' ' <"$TMP/git.err")" >&2
    exit 78
fi
say "checkout $REPO at ${HEAD:0:12} — $MODE (packet ${OPS_REQUEST_ID:-none}, as $BOSS_ACTOR)"

# --- freshness: the newest converged train must be in this history --------------
# Signed: an unauthenticated read is handed a smaller world (total: 0)
# rather than an error, so an empty listing below is a failed read and
# never "no train has landed".
TRAINS_URL="$BOSS_JOBS_URL/api/jobs?kind=pr-train&limit=40&full=true"
if ! curl -q -fsS --max-time 20 -H "x-boss-user: $BOSS_USER" \
        ${MT_HDR:+-H "$MT_HDR"} \
        "$TRAINS_URL" > "$TMP/trains.json" 2>"$TMP/curl.err"; then
    say "cannot answer: the system of record did not answer the pr-train read ($TRAINS_URL): $(tr '\n' ' ' <"$TMP/curl.err")"
    say "whether this checkout carries the newest train cannot be judged, so nothing was compared or published"
    exit 75
fi
NEWEST="$(jq -c '
    (if type == "object" and has("data") then .data else . end)
    | map(. as $t
          | ((.steps // []) | map(select(.spec_slug == "merged" and .status == "completed")) | .[0]) as $m
          | select($m != null and (($m.metadata.merge_ref // "") | length) >= 7)
          | {id: $t.id, at: ($m.completed_at // ""), ref: $m.metadata.merge_ref})
    | sort_by(.at) | last // empty' "$TMP/trains.json" 2>/dev/null)"
if [ -z "$NEWEST" ]; then
    n="$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$TMP/trains.json" 2>/dev/null || echo '?')"
    say "cannot answer: $TRAINS_URL listed no closed pr-train with a merge_ref ($n packet(s) read, signed as $BOSS_ACTOR) — a read that answers no trains is a denied scope as often as an empty world, and neither is a fact this verb may publish on"
    exit 75
fi
TRAIN_ID="$(jq -r .id <<<"$NEWEST")"
TRAIN_AT="$(jq -r .at <<<"$NEWEST")"
MAIN_SHA="$(jq -r .ref <<<"$NEWEST")"
if ! as_owner "git -C '$REPO' cat-file -e '$MAIN_SHA^{commit}' && git -C '$REPO' merge-base --is-ancestor '$MAIN_SHA' HEAD" >/dev/null 2>&1; then
    say "not yet: checkout at ${HEAD:0:8}, main at ${MAIN_SHA:0:8} — the newest converged train ${TRAIN_ID:0:8} merged ${MAIN_SHA:0:8} at ${TRAIN_AT:-?} and this checkout does not carry it yet (boss-gcp-converge fast-forwards it on its next tick); nothing compared, nothing published"
    exit 75
fi
say "fresh: the newest converged train ${TRAIN_ID:0:8} (merged ${MAIN_SHA:0:8} at ${TRAIN_AT:-?}) is in this checkout's history"

# --- the bundle, in its order --------------------------------------------------
# The seed loader reads every *.toml directly in the bundle directory,
# sorted by file name (boss_jobs::seed_loader); the kind IS the file
# name, because the sub-verb looks the kind's file up by that name.
KINDS=()
while IFS= read -r f; do
    [ -n "$f" ] || continue
    k="$(basename "$f" .toml)"
    KINDS+=("$k")
done <<LIST
$(LC_ALL=C ls "$BUNDLE"/*.toml 2>/dev/null)
LIST
if [ "${#KINDS[@]}" -eq 0 ]; then
    echo "$NAME: $BUNDLE_REL holds no *.toml — there is no bundle to compare" >&2
    exit 78
fi

# --- the holds: which kinds this verb must leave alone ---------------------------
# Read by the one reader, whole or not at all. `read_holds <out>` answers
# 0 with the rows written, non-zero with the reader's own words in
# $TMP/holds.problems — and the caller decides what a failure stops.
HOLDS_REL="infra/platform/workflow-holds"
read_holds() { # <out file>
    local rc
    python3 "$HOLDS_PY" "$REPO" > "$1" 2>"$TMP/holds.err"; rc=$?
    if [ "$rc" -ne 0 ]; then
        {
            awk -F'\t' '$1 == "problem" { print $2 }' "$1"
            cat "$TMP/holds.err"
        } > "$TMP/holds.problems"
        [ -s "$TMP/holds.problems" ] || echo "the hold reader exited $rc and said nothing" > "$TMP/holds.problems"
        return 1
    fi
}
hold_field() { # <holds file> <state> <kind> <column 3..5>: empty when the kind is not in that state
    awk -F'\t' -v s="$2" -v k="$3" -v c="$4" '$1 == s && $2 == k { print $c; exit }' "$1"
}
HOLDS="$TMP/holds.tsv"
if ! read_holds "$HOLDS"; then
    say "REFUSED — the tree's holds cannot be read, and a hold that cannot be read is not a hold that is absent:"
    sed 's/^/  /' "$TMP/holds.problems"
    say "nothing asked, nothing published, in either mode: which kinds are held out of this verb is declared under $HOLDS_REL and derived from each row's writers, executors and bills ($HOLDS_PY), and until that reads whole this verb cannot tell a refusal row from any other. Fix the file named above in a car (packet ${OPS_REQUEST_ID:-none})."
    exit 78
fi
n_declared_held="$(awk -F'\t' '$1 == "held" { n++ } END { print n + 0 }' "$HOLDS")"
say "holds: ${n_declared_held:-0} kind(s) held out of this verb, read from $HOLDS_REL and each row's writers, executors and bills"
say "bundle: ${#KINDS[@]} kind(s) under $BUNDLE_REL; asking $SUB --check for each"

# --- the table -----------------------------------------------------------------
# Every row is `kind  from  to  result`; the result column carries the
# sub-verb's verdict in its words. A version the phrase did not yield
# reads `?` rather than a number this verb made up.
ROWS="$TMP/rows.tsv"
: > "$ROWS"
row() { printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "$ROWS"; }
version_after() { # <prefix> <one line>: the digits right after <prefix>
    local v
    v="$(sed -n "s/.*$1\([0-9]\{1,\}\).*/\1/p" <<<"$2")"
    printf '%s' "${v:-?}"
}
first_line_with() { # <phrase> <text>
    grep -m1 -F -- "$1" <<<"$2"
}

AHEAD=()          # kinds to publish, in bundle order
HELD_LINES="$TMP/held.lines"   # one `held <kind> ...` line per held kind
: > "$HELD_LINES"
held_line() { # <holds file> <kind>: the line that names a held kind, outside the table
    printf '%s: held %s (%s) — %s — lifts: %s\n' "$NAME" "$2" \
        "$(hold_field "$1" held "$2" 3)" "$(hold_field "$1" held "$2" 4)" "$(hold_field "$1" held "$2" 5)"
}
HAND="published only by the deliberate door: boss workflow publish <kind> $BUNDLE_REL/<kind>.toml, run by a person from a checkout — which does not lift the hold (the publish-workflow verb refuses a held kind too)"
n_equal=0; n_refused=0; n_pending=0; n_other=0; n_held=0; n_open=0
HELD_AHEAD=()     # --file-held: held kinds the tree moved ahead of
EQUAL_KINDS=()    # exact live-version / frozen-spec witnesses, never publishes
for kind in "${KINDS[@]}"; do
    if ! grep -Eqx -- '^[a-z][a-z0-9-]{1,60}$' <<<"$kind"; then
        row "$kind" "-" "-" "skipped: file name outside the kind pattern ^[a-z][a-z0-9-]{1,60}$"
        n_other=$((n_other + 1))
        continue
    fi
    # A lifted hold must not hide an obsolete publish obligation.
    if [ "$MODE" = "--file-held" ] && [ -z "$(hold_field "$HOLDS" held "$kind" 3)" ]; then
        n_open=$((n_open + 1))
    fi
    out="$("$SUB" "$kind" --check 2>&1)"; rc=$?
    if [ "$MODE" = "--file-held" ] && [ "$rc" = 5 ]; then
        # Only the sub-verb's explicit equality witness authorizes a
        # withdrawal. Exit 5 or a human sentence alone is insufficient.
        witness="$(sed -n "s/^publish-workflow: EQUAL: $kind v\([1-9][0-9]*\); spec sha256 \([0-9a-f]\{64\}\)$/\1 \2/p" <<<"$out")"
        if [ -z "$witness" ] || [ "$(wc -l <<<"$witness")" -ne 1 ]; then
            say "cannot answer: $kind exit 5 carried no single live-version and frozen-spec equality witness — nothing superseded"
            exit 75
        fi
        printf '%s\n' "$witness" > "$TMP/equal.$kind"
        EQUAL_KINDS+=("$kind")
    fi
    # A HELD kind is asked like any other — its row states what the
    # registry holds, and a registry that cannot be read still stops the
    # run below — and is then held WHATEVER the sub-verb answered: in
    # neither `would publish` nor `refused`, so it never reaches AHEAD
    # and never stops the kinds that are not held.
    held_source="$(hold_field "$HOLDS" held "$kind" 3)"
    held_why="$(hold_field "$HOLDS" held "$kind" 4)"
    held_lifts="$(hold_field "$HOLDS" held "$kind" 5)"
    hand="${HAND//<kind>/$kind}"
    # Only a VERDICT is folded into `held`: 75/78 stop the run and an
    # undocumented exit is still an error, held or not.
    #
    # THE SUB-VERB HOLDS TOO (review R1 of the signer car): it reads the
    # same reader and answers 9 where a held kind would have been
    # `--check ok`, and refuses 9 to publish one. Two layers, one
    # reader, so they agree unless the tree changed between the two
    # reads — and then the LATER read wins: a 9 for a kind this run did
    # not know as held re-reads the holds and holds it.
    #
    # A PASSKEY-PROTECTED KIND IS HELD THE SAME WAY (backlog 5af64799).
    # The sub-verb also answers 9 for a kind the SERVER judges
    # passkey-protected (GET /api/workflows/<kind>/passkey) and no hold
    # names, and says so on its `PASSKEY-PROTECTED (...)` line with the
    # server's reason, copied here, never re-derived. Until that line, a
    # protected kind came back `--check ok`, went down --for-real, and the
    # server refused it unsigned with a draft created and left (ops-request
    # 489680c9, publish-to-github v18). Its road is the owner's passkey:
    # held below, so --for-real names it and skips it, and --file-held
    # files its publish-a-protocol packet as for any held kind.
    passkey_why=""
    if [ "$rc" = 9 ] && [ -z "$held_source" ]; then
        passkey_why="$(sed -n '/^publish-workflow: PASSKEY-PROTECTED (/{s/^publish-workflow: PASSKEY-PROTECTED ([^)]*): //p;q;}' <<<"$out")"
    fi
    if [ -n "$passkey_why" ]; then
        held_source="passkey-protected, judged by the server"
        held_why="$passkey_why"
        held_lifts="a publish-a-protocol packet its owner signs with a passkey (--file-held files it)"
        hand="published only with its owner's passkey: the server refuses an unsigned publish of a passkey-protected row, so neither this verb nor boss workflow publish without a signed packet can land it"
    elif [ "$rc" = 9 ] && [ -z "$held_source" ]; then
        if ! read_holds "$HOLDS"; then
            say "REFUSED — the sub-verb says '$kind' is held and the tree's holds can no longer be read:"
            sed 's/^/  /' "$TMP/holds.problems"
            say "nothing published (packet ${OPS_REQUEST_ID:-none})"
            exit 78
        fi
        held_source="$(hold_field "$HOLDS" held "$kind" 3)"
        held_why="$(hold_field "$HOLDS" held "$kind" 4)"
        held_lifts="$(hold_field "$HOLDS" held "$kind" 5)"
        if [ -z "$held_source" ]; then
            # The sub-verb held it and the reader, asked again, does not:
            # the tree is moving under this run. Not an answer.
            say "cannot answer: publish-workflow.sh says '$kind' is held and $HOLDS_PY, read again, does not — the checkout is changing under this run; nothing published"
            exit 75
        fi
    fi
    case "$rc" in 0|4|5|6|8|9) ;; *) held_source="" ;; esac
    if [ -n "$held_source" ]; then
        live="-"
        case "$rc" in
            9) live="v$(version_after 'over live v' "$(first_line_with '--check HELD: would NOT publish' "$out")")"
               state="the tree is ahead of live $live, and this verb would have published it" ;;
            0) live="v$(version_after 'over live v' "$(first_line_with '--check ok: would publish' "$out")")"
               state="the tree is ahead of live $live, and the sub-verb did not say HELD (it read the holds before this hold was there, or is older than the hold)" ;;
            5) live="v$(version_after "the live $kind v" "$(first_line_with 'already says what' "$out")")"
               state="live $live is equal to the tree — nothing is waiting" ;;
            6) live="v$(version_after "live $kind v" "$(first_line_with 'carries what the tree never said' "$out")")"
               state="live $live carries what the tree never said" ;;
            4) state="the tree's row does not lint clean (or the registry holds a stale draft)" ;;
            8) state="no live row yet (the seed admits a new kind)" ;;
        esac
        row "$kind" "$live" "-" "HELD out of this verb ($held_source): $held_why — lifts: $held_lifts. State: $state. $hand"
        printf '%s: held %s (%s) — %s — lifts: %s\n' "$NAME" "$kind" "$held_source" "$held_why" "$held_lifts" >> "$HELD_LINES"
        n_held=$((n_held + 1))
        # The sub-verb's own answer, kept whole for the packet: its
        # dry-run line and its indented DIFF lines are what gets signed.
        if [ "$MODE" = "--file-held" ] && [ "$rc" = 9 ]; then
            printf '%s\n' "$out" > "$TMP/held-ahead.$kind"
            HELD_AHEAD+=("$kind")
        fi
        continue
    fi
    case "$rc" in
        0)
            live="$(version_after 'over live v' "$(first_line_with '--check ok: would publish' "$out")")"
            row "$kind" "v$live" "tree" "would publish (the tree moved ahead; live v$live is a row the tree once said)"
            AHEAD+=("$kind")
            ;;
        5)
            live="$(version_after "the live $kind v" "$(first_line_with 'already says what' "$out")")"
            row "$kind" "v$live" "v$live" "equal — nothing to publish"
            n_equal=$((n_equal + 1))
            ;;
        6)
            live="$(version_after "live $kind v" "$(first_line_with 'carries what the tree never said' "$out")")"
            row "$kind" "v$live" "-" "REFUSED: live v$live carries what the tree never said — never published by this verb; the fields, from the sub-verb:"
            # The drift, field by field, copied not retyped: the
            # sub-verb prints one two-space-indented line per field.
            grep -E '^  ' <<<"$out" | sed 's/^/\t\t\t    /' >> "$ROWS"
            printf '\t\t\t    (write the live edit into %s/%s.toml and land that car, or approve --force-tree for this one kind at the Drift tab)\n' "$BUNDLE_REL" "$kind" >> "$ROWS"
            n_refused=$((n_refused + 1))
            ;;
        4)
            row "$kind" "-" "-" "REFUSED: the tree's row does not lint clean (or the registry holds a stale draft) — not published; the sub-verb's output:"
            grep -E '^  ' <<<"$out" | sed 's/^/\t\t\t    /' >> "$ROWS"
            n_refused=$((n_refused + 1))
            ;;
        8)
            row "$kind" "-" "-" "pending: no live row yet (the seed admits a new kind; this verb republishes existing ones)"
            n_pending=$((n_pending + 1))
            ;;
        75|78)
            say "$kind: the sub-verb could not answer (exit $rc); its output, whole:"
            sed 's/^/  /' <<<"$out"
            say "cannot answer: a drift set judged from a registry that could not be read for '$kind' would be the confident wrong answer — nothing published"
            exit "$rc"
            ;;
        *)
            row "$kind" "-" "-" "ERROR: publish-workflow.sh --check exited $rc, outside its documented codes: $(tail -n1 <<<"$out")"
            n_other=$((n_other + 1))
            ;;
    esac
done

# --- --for-real: each tree-ahead kind, in order, through the sub-verb ---------------
n_published=0; n_unconfirmed=0
if [ "$MODE" = "--for-real" ] && [ "${#AHEAD[@]}" -gt 0 ]; then
    say "publishing ${#AHEAD[@]} tree-ahead kind(s) in bundle order, each through $SUB <kind> (its own lint, publish, read-back)"
    PUB="$TMP/pub.tsv"
    : > "$PUB"
    STOPPED=""    # why the remaining kinds are not published, once set
    for kind in "${AHEAD[@]}"; do
        # LOOK AGAIN, before every publish (backlog 083d240e). The table
        # above was judged once; since then a publish has run for every
        # kind before this one. Two things can have changed under it, and
        # neither is assumed not to have:
        #   - the checkout moved (boss-gcp-converge fast-forwards it), so
        #     this kind was judged as a row of ANOTHER tree — stop;
        #   - a hold landed, or stopped being readable — honour it, or
        #     stop.
        if [ -z "$STOPPED" ]; then
            now="$(as_owner "git -C '$REPO' rev-parse HEAD" 2>/dev/null)"
            if [ "$now" != "$HEAD" ]; then
                STOPPED="the checkout moved from ${HEAD:0:8} to ${now:0:8} while this run published, so this kind was judged against another tree; the next check judges it"
            elif ! read_holds "$TMP/holds.now"; then
                STOPPED="the tree's holds stopped being readable while this run published ($(tr '\n' ' ' <"$TMP/holds.problems")), and an unreadable hold is not an absent one"
            fi
        fi
        if [ -n "$STOPPED" ]; then
            printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "NOT PUBLISHED: $STOPPED" >> "$PUB"
            n_unconfirmed=$((n_unconfirmed + 1))
            continue
        fi
        late_source="$(hold_field "$TMP/holds.now" held "$kind" 3)"
        if [ -n "$late_source" ]; then
            printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "HELD out of this verb ($late_source), by a hold that was not there when this run's table was judged: $(hold_field "$TMP/holds.now" held "$kind" 4) — lifts: $(hold_field "$TMP/holds.now" held "$kind" 5). ${HAND//<kind>/$kind}" >> "$PUB"
            held_line "$TMP/holds.now" "$kind" >> "$HELD_LINES"
            n_held=$((n_held + 1))
            continue
        fi
        out="$("$SUB" "$kind" 2>&1)"; rc=$?
        sed "s/^/  [$kind] /" <<<"$out"
        case "$rc" in
            0)
                # "<kind> vA -> vB live at <url> — confirmed ..."
                line="$(first_line_with ' live at ' "$out")"
                from="$(sed -n 's/.*[^0-9]v\([0-9]\{1,\}\) -> v[0-9]\{1,\} live at.*/\1/p' <<<"$line")"
                to="$(sed -n 's/.*[^0-9]v[0-9]\{1,\} -> v\([0-9]\{1,\}\) live at.*/\1/p' <<<"$line")"
                printf '%s\t%s\t%s\t%s\n' "$kind" "v${from:-?}" "v${to:-?}" "published — confirmed by the sub-verb's read-back" >> "$PUB"
                n_published=$((n_published + 1))
                ;;
            9)
                # The sub-verb REFUSED it as held: a hold that landed
                # after this run's own look a moment ago. Held, not a
                # failed publish — and the other kinds go on.
                printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "HELD out of this verb, refused by the sub-verb at the publish itself: $(first_line_with 'REFUSED' "$out" | sed 's/^publish-workflow: REFUSED — //')" >> "$PUB"
                printf '%s: held %s (refused by publish-workflow.sh at the publish) — see its row\n' "$NAME" "$kind" >> "$HELD_LINES"
                n_held=$((n_held + 1))
                ;;
            7)
                # The sub-verb ran the publish and the registry did not
                # bear it out (or could not be read back): its last line
                # says which; a draft may be armed for this ONE kind.
                printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "NOT CONFIRMED: $(tail -n1 <<<"$out")" >> "$PUB"
                n_unconfirmed=$((n_unconfirmed + 1))
                ;;
            *)
                printf '%s\t%s\t%s\t%s\n' "$kind" "-" "-" "NOT CONFIRMED (publish-workflow.sh exited $rc, a verdict --check did not reach): $(tail -n1 <<<"$out")" >> "$PUB"
                n_unconfirmed=$((n_unconfirmed + 1))
                ;;
        esac
    done
    # The published rows replace their `would publish` rows, in place.
    while IFS=$'\t' read -r k f t r; do
        [ -n "$k" ] || continue
        awk -v k="$k" -v f="$f" -v t="$t" -v r="$r" 'BEGIN{FS=OFS="\t"} $1==k && $4 ~ /^would publish/ {print k, f, t, r; next} {print}' "$ROWS" > "$ROWS.new" \
            && mv "$ROWS.new" "$ROWS"
    done < "$PUB"
fi

# --- --file-held: one publish-a-protocol packet per held spec ---------------------
n_filed=0; n_asked=0; n_superseded=0; n_unfiled=0
if [ "$MODE" = "--file-held" ] && { [ "${#HELD_AHEAD[@]}" -gt 0 ] || [ "${#EQUAL_KINDS[@]}" -gt 0 ]; }; then
    api_get() { # <path> <out>: 0 with a 200 body written
        local code
        code=$(curl -q -sS --max-time 20 -o "$2" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} "$BOSS_JOBS_URL$1" 2>>"$TMP/api.err")
        [ "$code" = "200" ]
    }
    # The known answer: the request this run answers is an ops-request,
    # so a listing that says there are none is a dark read.
    # Read as a number, not asked as a `jq -e` guard: an empty body
    # yields no total at all, and the case below refuses it.
    control=""
    api_get "/api/jobs?kind=ops-request&limit=1" "$TMP/control.json" \
        && control="$(jq -r '.total // empty' "$TMP/control.json" 2>/dev/null)"
    case "${control:-empty}" in empty|*[!0-9]*|0) control="" ;; esac
    if [ -z "$control" ]; then
        say "cannot answer: the known-answer read (GET /api/jobs?kind=ops-request) did not show the request this run answers, so a listing of publish-a-protocol packets could not be trusted to be whole — nothing filed (packet ${OPS_REQUEST_ID:-none}). $(head -c 300 "$TMP/api.err" | tr '\n' ' ')"
        exit 75
    fi
    # Every publish-a-protocol packet, every status, every page.
    : > "$TMP/asked.tsv"
    offset=0
    while :; do
        if ! api_get "/api/jobs?kind=publish-a-protocol&limit=500&offset=$offset" "$TMP/page.json"; then
            say "cannot answer: the publish-a-protocol listing failed at offset $offset — nothing filed (packet ${OPS_REQUEST_ID:-none}). $(head -c 300 "$TMP/api.err" | tr '\n' ' ')"
            exit 75
        fi
        # A page is rows only when `data` IS an array of jobs, each an
        # object with an id (review R2 of car 214ae147, Codex run
        # 753b6bd7): `.data // []` read a 200 carrying `data: null` as
        # an empty world, the extraction's error went unchecked, and a
        # second packet was filed for a declined spec, exit 0. A page
        # that is not rows proves nothing about what is already asked.
        rows=""; total=""
        read -r rows total < <(jq -r '
            select((.data | type) == "array"
                   and all(.data[]; type == "object" and (.id | type) == "string" and .id != ""))
            | [(.data | length), (.total // "x")] | @tsv' "$TMP/page.json" 2>/dev/null)
        case "${rows:-x}:${total:-x}" in
            *[!0-9:]*|:*|*:) say "cannot answer: the publish-a-protocol listing answered a page at offset $offset that is not rows (no data array of jobs with ids, or no total) — nothing filed (packet ${OPS_REQUEST_ID:-none})"; exit 75 ;;
        esac
        if ! jq -r '.data[] | select((.metadata.registry // "workflow") == "workflow") | [.id, (.status // ""), (.metadata.protocol_kind // ""), (.metadata.spec_sha256 // "")] | @tsv' \
            "$TMP/page.json" >> "$TMP/asked.tsv" 2>"$TMP/page.err"; then
            say "cannot answer: the publish-a-protocol page at offset $offset could not be read as rows ($(head -c 200 "$TMP/page.err" | tr '\n' ' ')) — nothing filed (packet ${OPS_REQUEST_ID:-none})"
            exit 75
        fi
        offset=$((offset + rows))
        [ "$offset" -lt "$total" ] || break
        [ "$rows" -gt 0 ] || { say "cannot answer: the publish-a-protocol listing stopped at $offset of $total — nothing filed"; exit 75; }
    done
    # Equality satisfies the old obligation; it is not a publish by
    # this packet. Retain the observed version and the compared bytes
    # as the witness for the existing superseded terminal. A later edit
    # is a new obligation, not a retroactive change to this observation.
    for kind in "${EQUAL_KINDS[@]}"; do
        read -r live sha < "$TMP/equal.$kind"
        file_rel="$BUNDLE_REL/$kind.toml"
        while IFS=$'\t' read -r old_id old_status old_kind old_sha; do
            [ "$old_kind" = "$kind" ] && [ "$old_status" = "open" ] || continue
            # Bind the witness to this checkout before each metadata write.
            current_head="$(git -C "$REPO" rev-parse HEAD 2>/dev/null)"
            current_sha="$(sha256sum "$REPO/$file_rel" 2>/dev/null | cut -c1-64)"
            if [ "$current_head" != "$HEAD" ] || [ "$current_sha" != "$sha" ]; then
                say "cannot answer: $kind checkout changed after its equality read — no withdrawal of $old_id"
                exit 75
            fi
            jq -n --arg kind "$kind" --arg live "$live" --arg sha "$sha" \
                --arg head "$HEAD" --arg path "$file_rel" --arg req "${OPS_REQUEST_ID:-none}" '{
                  superseded_by: "workflow:\($kind):v\($live)",
                  supersession_translation: "Live \($kind) v\($live) compared equal to \($path) sha256 \($sha) at checkout \($head); no publish was needed by this packet.",
                  superseded_live: {kind: $kind, version: $live, spec_sha256: $sha, tree_sha: $head, for_check: $req}
                }' > "$TMP/supersede.json"
            code=$(curl -q -sS --max-time 20 -X PATCH -o /dev/null -w '%{http_code}' \
                -H 'content-type: application/json' -H "x-boss-user: $BOSS_USER" ${MT_HDR:+-H "$MT_HDR"} \
                --data-binary @"$TMP/supersede.json" "$BOSS_JOBS_URL/api/jobs/$old_id/metadata" 2>>"$TMP/api.err")
            case "$code" in
                200|204) say "file-held: $kind — open publish-a-protocol $old_id superseded by equal live v$live"
                         n_superseded=$((n_superseded + 1)) ;;
                *) say "file-held: $kind — $old_id NOT superseded (PATCH answered HTTP ${code:-000})"
                   n_unfiled=$((n_unfiled + 1)) ;;
            esac
        done < "$TMP/asked.tsv"
    done
    for kind in "${HELD_AHEAD[@]}"; do
        out="$(cat "$TMP/held-ahead.$kind")"
        file_rel="$BUNDLE_REL/$kind.toml"
        held_line_text="$(first_line_with '--check HELD: would NOT publish' "$out")"
        live="$(version_after 'over live v' "$held_line_text")"
        # The sha the sub-verb printed for the ONE copy its diff and lint
        # read (publish-workflow.sh 1'), never a fresh hash of a checkout
        # that may have moved since: the owner signs one file's hash
        # beside that same file's diff (the source of review R1, car
        # 214ae147). A check that prints none files nothing.
        sha="$(sed -n 's/.*; spec sha256 \([0-9a-f]\{64\}\)$/\1/p' <<<"$held_line_text")"
        if [ -z "$sha" ]; then
            say "file-held: $kind NOT filed — its --check HELD line names no spec sha256, so which bytes its diff describes cannot be signed"
            n_unfiled=$((n_unfiled + 1))
            continue
        fi
        asked="$(awk -F'\t' -v k="$kind" -v s="$sha" '$3 == k && $4 == s { print $1 " (" $2 ")"; exit }' "$TMP/asked.tsv")"
        if [ -n "$asked" ]; then
            say "file-held: $kind at sha256 ${sha:0:12} was already asked as publish-a-protocol $asked — not filed again"
            n_asked=$((n_asked + 1))
            continue
        fi
        dry_run="$(first_line_with 'lints clean' "$out" | sed 's/^publish-workflow: //')"
        diff_text="$(grep -E '^  ' <<<"$out" | sed 's/^  //')"
        jq -n --arg kind "$kind" --arg path "$file_rel" --arg sha "$sha" --arg live "$live" \
            --arg head "$HEAD" --arg dry "${dry_run:-the sub-verb printed no lint line}" \
            --arg diff "${diff_text:-the sub-verb printed no diff}" --arg actor "$BOSS_ACTOR" \
            --arg req "${OPS_REQUEST_ID:-none}" '{
              kind: "publish-a-protocol", status: "open", owner_id: $actor, priority: "standard", tags: [],
              subject: {subject_kind: "custom", id: $kind},
              title: "Publish \($kind) over live v\($live)",
              metadata: {registry: "workflow", row_name: $kind,
                         protocol_kind: $kind, spec_path: $path, spec_sha256: $sha, live_version: $live,
                         tree_sha: $head, dry_run: $dry, diff: $diff, filed_by: $actor, for_check: $req,
                         design: "b1f319bc"}}' > "$TMP/packet.json"
        code=$(curl -q -sS --max-time 20 -X POST -o "$TMP/created.json" -w '%{http_code}' \
            -H 'content-type: application/json' -H "x-boss-user: $BOSS_USER" ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$TMP/packet.json" "$BOSS_JOBS_URL/api/jobs" 2>>"$TMP/api.err")
        new_id="$(jq -r '.id // empty' "$TMP/created.json" 2>/dev/null)"
        if [ "$code" != "201" ] || [ -z "$new_id" ]; then
            say "file-held: $kind NOT filed — POST /api/jobs answered HTTP ${code:-000}: $(head -c 300 "$TMP/created.json" 2>/dev/null | tr '\n' ' ')"
            n_unfiled=$((n_unfiled + 1))
            continue
        fi
        say "file-held: $kind filed as publish-a-protocol $new_id — $file_rel sha256 ${sha:0:12} over live v$live, for its owner's passkey"
        n_filed=$((n_filed + 1))
        printf '%s\topen\t%s\t%s\n' "$new_id" "$kind" "$sha" >> "$TMP/asked.tsv"
        # Each OPEN packet for the kind that signs other bytes is
        # superseded: its signature could publish nothing now.
        while IFS=$'\t' read -r old_id old_status old_kind old_sha; do
            [ "$old_kind" = "$kind" ] && [ "$old_status" = "open" ] && [ "$old_id" != "$new_id" ] && [ "$old_sha" != "$sha" ] || continue
            jq -n --arg new "$new_id" --arg sha "$sha" '{superseded_by: $new,
                supersession_translation: "a newer edit of the same kind (sha256 \($sha[0:12])) was filed as \($new[0:8]); this packet signs bytes the tree no longer holds, so its publish would be refused"}' \
                > "$TMP/supersede.json"
            code=$(curl -q -sS --max-time 20 -X PATCH -o /dev/null -w '%{http_code}' \
                -H 'content-type: application/json' -H "x-boss-user: $BOSS_USER" ${MT_HDR:+-H "$MT_HDR"} \
                --data-binary @"$TMP/supersede.json" "$BOSS_JOBS_URL/api/jobs/$old_id/metadata" 2>>"$TMP/api.err")
            case "$code" in
                200|204) say "file-held: $kind — open publish-a-protocol $old_id superseded by $new_id"
                         n_superseded=$((n_superseded + 1)) ;;
                *) say "file-held: $kind — open publish-a-protocol $old_id NOT marked superseded (PATCH answered HTTP ${code:-000}); its signature would be refused for other bytes anyway" ;;
            esac
        done < <(cat "$TMP/asked.tsv")
    done
fi

# --- the answer ------------------------------------------------------------------
echo
printf 'kind\tfrom\tto\tresult\n'
cat "$ROWS"
echo
n_ahead="${#AHEAD[@]}"
# Every held kind and every released one, by name, every run — outside
# the table, one line each, so a reader (or a probe) greps one shape.
cat "$HELD_LINES"
awk -F'\t' -v n="$NAME" '$1 == "released" { printf "%s: released %s (%s) — %s\n", n, $2, $3, $4 }' "$HOLDS"
# `held H` follows `refused K` so the three counts a rule reads keep
# their places (publish-drift-for-real-on-clean-check's verdict_pattern),
# and it is stated when it is 0: a count that is absent cannot be told
# from holds that were never read.
verdict=""
if [ "$MODE" = "--check" ]; then
    verdict="$NAME: would publish $n_ahead, skipped $n_equal equal, refused $n_refused, held $n_held"
elif [ "$MODE" = "--file-held" ]; then
    # Never `would publish` / `published`: the for-real rule's pattern
    # must not read a filing as a check.
    verdict="$NAME: filed $n_filed publish-a-protocol packet(s), already asked $n_asked, superseded $n_superseded, not filed $n_unfiled, held $n_held ahead ${#HELD_AHEAD[@]}, not held $n_open"
else
    verdict="$NAME: published $n_published, skipped $n_equal equal, refused $n_refused, held $n_held"
    [ "$n_unconfirmed" -eq 0 ] || verdict="$verdict, not confirmed $n_unconfirmed"
fi
[ "$n_pending" -eq 0 ] || verdict="$verdict, pending $n_pending"
[ "$n_other" -eq 0 ] || verdict="$verdict, errored $n_other"
verdict="$verdict (checkout ${HEAD:0:8}, ${#KINDS[@]} kind(s), packet ${OPS_REQUEST_ID:-none})"
echo "$verdict"
if [ "$n_unconfirmed" -gt 0 ] || [ "$n_other" -gt 0 ] || [ "$n_unfiled" -gt 0 ]; then
    exit 1
fi
exit 0
