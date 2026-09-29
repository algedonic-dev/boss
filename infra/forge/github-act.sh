#!/usr/bin/env bash
set +x # FIRST: an inherited SHELLOPTS=xtrace would trace the token (review L4)
#
# github-act — three bounded GitHub acts, each a rendered plan and a
# passkey-approved write, authenticated as the GitHub App's INSTALLATION
# on the owner the request names.
#
#   github-act.sh create-repository     --plan <owner> <name> <visibility>
#   github-act.sh create-repository            <owner> <name> <visibility> <plan-sha256>
#   github-act.sh set-branch-protection --plan <owner> <repo> <pattern> <push-allow> <checks>
#   github-act.sh set-branch-protection        <owner> <repo> <pattern> <push-allow> <checks> <plan-sha256>
#   github-act.sh delete-refs           --plan <owner> <repo> <refs> <reason>
#   github-act.sh delete-refs                  <owner> <repo> <refs> <reason> <plan-sha256>
#
# WHY IT EXISTS (design 76155676 decision 4, David 2026-09-27; backlog
# 6a8ff89f). Every GitHub act BOSS needed was a click or a personal token
# David minted by hand: the disaster-recovery repository could not be
# created, `publish/*` could not be protected behind an identity, and the
# public fork's stale branches could only be deleted in the UI. The design
# makes a GitHub App the one root and these three acts bounded verbs in
# the ops-runner shape (design 17835005): a READ-ONLY plan verb renders
# what the act would do from what GitHub says NOW, David's passkey signs
# those bytes, and the write re-renders and acts only while today's plan
# still hashes to the signed one — then proves the act by reading GitHub
# back. David, 2026-09-27: "we may end up creating a repo for each
# customer … the app is nice for customers that want to use their own
# repo" — so create-repository is the rehearsal for a hosting flow, and
# the installation is DATA, never a constant.
#
# WHICH CREDENTIAL. One App, many installations (an organisation each:
# algedonic-dev today, a hosting customer's org later). The owner the
# request names selects the installation's token SLOT:
#     $BOSS_GITHUB_APP_TOKEN_DIR/<owner>.token   (default dir
#     /etc/boss-publish/github-app), with <owner>.token.expires-at beside it
# — the two files infra/forge/credential-render.sh writes from the broker
# Secret `github-app-<owner>`. The token is a one-hour installation token
# the credential broker mints (`credential.rotate.github-app-installation`);
# nothing here mints one. A slot that is absent, not a root-owned 0600
# regular file, empty, or whose recorded expiry is not live is a REFUSAL
# naming the path, before GitHub is called.
#
# MINTED PER ACT, RENDERED HERE, REVOKED HERE (design 76c46869 Q1-Q2,
# David 2026-09-28; backlog bf8726c9). The org-admin token is never kept
# alive: two broker rules mint it when a GitHub request is filed and
# again when its plan is approved. So before it reads the slot this runs
# credential-render.sh itself, for the broker rule whose credential_id is
# `github-app-<owner>` — no wait of up to ten minutes for a converge pass
# — and an owner no rule declares is refused: nothing mints its token.
# After a WRITE, however it ends (done, refused or failed), the act is
# over, and so is its token: the Secret's copy is marked expired first
# (`credential-render.sh --expire`, as the broker's own revoke does, so a
# revoked token never reads as live to the approval mint's 40-minute
# window), then the token is presented to DELETE /installation/token,
# proven dead by a read that answers 401, and the slot removed. A plan
# keeps it: the approved write that follows needs it. A revoke that does
# not take is exit 1 — after an act that stands, which the output says.
# Only a 401 on the verify read proves the revoke: a 403 or a 404 means
# GitHub accepted the credential (the review of car 57a2c56e, finding 3).
#
# THE TOKEN IS THE REQUEST'S OWN (backlog 4ce4ec55; the same review,
# finding 1). With one Secret key per owner, two writes approved before
# the first ran shared one token: write 1 revoked it, and write 2's plan
# re-run (the runner's, before the claim) found the key expired and
# refused — its single-use approval spent. So the broker keys each token
# `token-<ops-request id>`, and this renders, marks expired and revokes
# the key of the request it answers: the OPS_REQUEST_ID the ops runner
# hands every verb (plan and write alike). A run without one, or with
# anything but a lowercase packet uuid, is refused before any render or
# call — the runner always sets it, so this refuses nothing it runs.
# The slot FILE is still one per owner and a plan keeps it, so a slot
# that does not name this request beside its token (`<slot>.request`, the
# render writes it) is another request's, and is refused rather than
# acted with and revoked (the review of c21401da, F1).
# The DR push slot
# (/etc/boss-publish/github-dr.token) is NOT this slot and cannot serve:
# it is narrowed to boss-dr with contents:write — no repository creation,
# no rulesets — and cannot even be minted until boss-dr exists.
#
# THE TOKEN NEVER LEAVES A FILE OR A VARIABLE. curl reads it from a 0600
# header file in a private temp directory (`-H @file`), git from this
# script's own `credential-helper` word, which reads a private copy of the
# validated token and answers ONLY https://github.com, exactly. No argv
# carries it, no message prints it, xtrace is off from the first line and
# not inherited, git and curl traces are unset, no redirect is followed,
# and no core is dumped (adversarial review of 78959555).
#
# THE THREE ACTS, each bounded by construction:
#
#   create-repository <owner> <name> <private|public>
#     POST /orgs/<owner>/repos — an ORGANISATION's repository (an
#     installation token cannot create a user's), empty (auto_init false).
#     Never a fork: this script makes no /forks call, and a repository
#     that already exists AS a fork is refused, as is one with the other
#     visibility, an archived one, or one that answers under another
#     name. One that exists exactly as declared is a plan with no act
#     (idempotent). Proof: GET /repos/<owner>/<name> reads back as
#     <owner>/<name>, that visibility, fork false.
#
#   set-branch-protection <owner> <repo> <pattern> <push-allow> <checks>
#     A repository RULESET, not classic branch protection: the classic
#     endpoint takes one branch NAME and cannot carry `publish/**`, and a
#     ruleset can. The verb owns exactly one ruleset per pattern, named
#     `boss: refs/heads/<pattern>`, created or replaced whole; it never
#     touches a ruleset it did not name and never deletes one. Its rules
#     are FIXED here — creation, update (restrict updates), deletion,
#     non_fast_forward, plus required_status_checks when checks are named
#     — so the request chooses only WHO may push (`app:<App id>` or
#     `team:<team id>` bypass actors, comma-separated, or `none`: nobody)
#     and WHICH checks (context names, comma-separated, or `none`). A
#     rulesets API has no user actor, so an identity is an App or a team.
#     Proof: the ruleset reads back, normalised, equal to the plan.
#
#   delete-refs <owner> <repo> <refs> <reason>
#     Each ref named in full (`heads/<branch>` or `tags/<tag>`,
#     comma-separated, at most 200; no pattern). The plan reads every
#     ref's sha with `git ls-remote`, and the write is ONE atomic push of
#     deletes, each LEASED to the sha the plan names — a ref that moved
#     since the render voids the plan; one that moves between the
#     re-render and the push refuses the whole push. The repository's
#     DEFAULT branch is always refused (GitHub refuses it too; changing
#     the default is not this verb's), and `heads/main` or `heads/master`
#     only with a reason (a slug, since an ops argument carries no
#     whitespace). A named ref already absent is listed and left.
#     Proof: `git ls-remote` lists none of the deleted refs.
#
# THE BYTES ARE DETERMINISTIC: no clock, no token digits, no temp path.
# Two renders of one true state are byte-identical; the sha256 goes on
# stderr as `plan-sha256: <hex>`. A write whose re-render hashes
# otherwise is refused, exit 78, naming both hashes — which also makes a
# second run of an applied plan a refusal (its state has moved), so no
# approval is spent twice.
#
# EXIT
#   0   the plan (with --plan), or the act done and read back
#   78  refused: the request, the slot or GitHub's state rules it out;
#       nothing was changed on GitHub
#   1   failed: GitHub could not be read, answered an error (any 4xx or
#       5xx not named above as an answer), or an act did not read back —
#       stderr says which, and what stands
#
# ENV
#   OPS_REQUEST_ID   the ops-request this run answers, set by the runner
#                    from the packet id it read — not packet-supplied
#                    text — and required (above)
# and the test seams, honoured ONLY when BOSS_GITHUB_ACT_SEAMS=test and
# unset otherwise (the ops runner hands a verb no packet-supplied
# environment, only the argv the allowlist builds):
#   BOSS_GITHUB_APP_TOKEN_DIR    the slots' directory
#   BOSS_GITHUB_TOKEN_OWNER_UID  the uid that must own a slot (0)
#   BOSS_GITHUB_API              https://api.github.com
#   BOSS_GITHUB_GIT_BASE         https://github.com
#   BOSS_GITHUB_CURL             the curl command
#   BOSS_GITHUB_READBACK_SLEEP   seconds between read-back attempts (2), and five
#                                times it between renders of an absent slot
#   BOSS_GITHUB_RENDER           the render (infra/forge/credential-render.sh)
# and, dropped likewise so the render reads the one Secret it declares:
#   BOSS_RENDER_KUBECTL, BOSS_OPS_DIR   credential-render.sh's kubectl
# Tested in crates/core/boss-testing/tests/github_act_sh.rs against a
# stub GitHub API and real git repositories.

set -uo pipefail

# THE ENVIRONMENT IS NOT TRUSTED TO BE QUIET (adversarial review of
# 78959555, L4/L5). Nothing inherited may trace, log or dump what this
# process holds: no child inherits xtrace, no git or curl trace or key
# log is honoured, no inherited git config is applied, no core file is
# written. SHELLOPTS is readonly, so it is un-exported rather than unset.
export -n SHELLOPTS
unset BASH_XTRACEFD GIT_TRACE GIT_TRACE_CURL GIT_TRACE_CURL_NO_DATA GIT_TRACE_PACKET \
    GIT_TRACE_REDACT GIT_CURL_VERBOSE GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS \
    GIT_ASKPASS SSH_ASKPASS SSLKEYLOGFILE CURL_HOME
ulimit -c 0
# The test seams are honoured only under the tests' marker: a runner unit
# that happened to carry BOSS_GITHUB_API cannot send the token elsewhere.
if [ "${BOSS_GITHUB_ACT_SEAMS:-}" != test ]; then
    unset BOSS_GITHUB_APP_TOKEN_DIR BOSS_GITHUB_TOKEN_OWNER_UID BOSS_GITHUB_API \
        BOSS_GITHUB_GIT_BASE BOSS_GITHUB_CURL BOSS_GITHUB_READBACK_SLEEP \
        BOSS_GITHUB_RENDER BOSS_RENDER_KUBECTL BOSS_OPS_DIR
fi

ME=github-act
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
HERE="$(dirname "$SELF")"

# --- the credential helper git runs (delete-refs) -----------------------------
# `github-act.sh credential-helper <token copy> get` — git hands the
# protocol and host on stdin; the token is answered ONLY for
# https://github.com, exactly, and only on `get`. No verb file can name
# this word (the-controls-are-bounded-verbs holds each GitHub verb to one
# of the three acts), so it is reachable only through git's helper config
# this script writes below.
if [ "${1-}" = credential-helper ]; then
    [ $# -eq 3 ] && [ "$3" = get ] || exit 0
    p="" h=""
    while IFS= read -r l && [ -n "$l" ]; do
        case "$l" in
            protocol=*) p="${l#protocol=}" ;;
            host=*) h="${l#host=}" ;;
        esac
    done
    [ "$p" = https ] && [ "$h" = github.com ] || exit 0
    [ -f "$2" ] && [ ! -L "$2" ] || exit 0
    t=""
    IFS= read -r t <"$2" || [ -n "$t" ] || exit 0
    [ -n "$t" ] || exit 0
    printf 'username=x-access-token\npassword=%s\n' "$t"
    exit 0
fi
# jq_doc_file / jq_doc_text: on jq-1.6 `jq -e` over NO document exits 0,
# so every guard below first asks whether GitHub sent one (d96e38ab).
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
TOKEN_DIR="${BOSS_GITHUB_APP_TOKEN_DIR:-/etc/boss-publish/github-app}"
TOKEN_OWNER_UID="${BOSS_GITHUB_TOKEN_OWNER_UID:-0}"
API="${BOSS_GITHUB_API:-https://api.github.com}"
GIT_BASE="${BOSS_GITHUB_GIT_BASE:-https://github.com}"
CURL="${BOSS_GITHUB_CURL:-curl}"
READBACK_SLEEP="${BOSS_GITHUB_READBACK_SLEEP:-2}"
RENDER="${BOSS_GITHUB_RENDER:-$HERE/credential-render.sh}"
RULES_DIR="$(cd "$HERE/../dispatcher/rules" 2>/dev/null && pwd)" || RULES_DIR="$HERE/../dispatcher/rules"
MAX_REFS=200

say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  nothing was changed on GitHub."; exit 78; }
fail() { say "FAILED — $*"; exit 1; }

usage() {
    say "usage: $ME create-repository [--plan] <owner> <name> <private|public> [<plan-sha256>]"
    say "       $ME set-branch-protection [--plan] <owner> <repo> <pattern> <push-allow> <checks> [<plan-sha256>]"
    say "       $ME delete-refs [--plan] <owner> <repo> <refs> <reason> [<plan-sha256>]"
    say "  --plan renders the plan on stdout and plan-sha256 on stderr and changes nothing;"
    say "  without it the last argument is the SIGNED plan's hash, and the act runs only while today's plan hashes to it"
}

# --- the arguments -----------------------------------------------------------
[ $# -ge 1 ] || { usage; refuse "no act named"; }
ACT="$1"
shift
PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
case "$ACT" in
    create-repository) WANT=3 ;;
    set-branch-protection) WANT=5 ;;
    delete-refs) WANT=4 ;;
    *) usage; refuse "unknown act '$ACT' — the acts are create-repository, set-branch-protection and delete-refs" ;;
esac
APPROVED=""
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq "$WANT" ] || { usage; refuse "$ACT --plan takes $WANT arguments, got $#"; }
else
    [ $# -eq $((WANT + 1)) ] || { usage; refuse "$ACT takes $WANT arguments and the approved plan's hash, got $# — render a plan with --plan; a write acts only on a signed one"; }
    APPROVED="${*: -1}"
    [[ $APPROVED =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$APPROVED'"
    set -- "${@:1:$WANT}"
fi

# Every argument is re-checked here even though the allowlist's patterns
# admitted it: the script is the bound, and the runner is one caller.
OWNER="$1"
[[ $OWNER =~ ^[A-Za-z0-9]([A-Za-z0-9]|-[A-Za-z0-9]){0,38}$ ]] \
    || refuse "'$OWNER' is not a GitHub account name (letters, digits and single inner hyphens, at most 39)"
# The request this run answers keys its token (header, THE TOKEN IS THE
# REQUEST'S OWN). It becomes part of a Secret key, so only a packet id.
REQUEST="${OPS_REQUEST_ID:-}"
[[ $REQUEST =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || refuse "OPS_REQUEST_ID is '${REQUEST:0:80}', not the id of an ops-request — the ops runner hands every verb the packet it answers, and the admin token is minted per request (the broker's key token-<request id>), so a run that cannot name its request has no token of its own to act with"
# repo_name <what> <value> — a repository name GitHub would keep as given.
repo_name() {
    [[ $2 =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}$ ]] || refuse "$1 '$2' is not a repository name (letters, digits, . _ -, not leading . or -, at most 100)"
    case "$2" in
        *.git) refuse "$1 '$2' ends in .git, which GitHub strips — name the repository without it" ;;
    esac
}

for tool in jq sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is not on PATH — GitHub's answers cannot be read, and no evidence is not a plan"
done

# THE SCRATCH — the token's header file and git's copy — LIVES IN THE
# UNIT'S RUNTIME DIRECTORY (backlog 5f77b205, 2026-09-28): tmpfs under
# /run, 0700, which systemd removes when the runner's oneshot run stops
# and a power loss clears. Under the host's shared /tmp, where it lived
# until then, a SIGKILL, an OOM kill or a power loss left the token on
# disk; the trap below covers every other end. No fallback to /tmp: a
# run without the directory is REFUSED (78) — nothing has reached GitHub.
[ -n "${RUNTIME_DIRECTORY:-}" ] && [ -d "$RUNTIME_DIRECTORY" ] \
    || refuse "RUNTIME_DIRECTORY is not a directory ('${RUNTIME_DIRECTORY:-unset}') — boss-ops-runner.service's RuntimeDirectory= sets it, and the token is never written anywhere else"
WORK="$(mktemp -d -p "${RUNTIME_DIRECTORY:?}" github-act.XXXXXX)" || fail "cannot make a private scratch directory"
chmod 700 "$WORK" || fail "cannot make $WORK private"
# finish — every end of a WRITE that loaded its token revokes it (below,
# `revoke`), in a subshell so a failure inside cannot skip the scratch's
# removal; the act's own verdict stands unless the revoke failed, which
# is exit 1 whatever the act did — a live admin token after its act is a
# fault someone must read.
REVOKE=0
finish() {
    local rc=$? r=0
    if [ "$REVOKE" -eq 1 ]; then
        REVOKE=0
        (revoke) || r=$?
        if [ "$r" -ne 0 ]; then
            say "FAILED — the token this act used is not proven revoked (above)$([ "$rc" -eq 0 ] && echo '; the act itself is done and read back')"
            rc=1
        fi
    fi
    rm -rf "$WORK"
    exit "$rc"
}
trap finish EXIT

# --- the installation's token slot -------------------------------------------
TOKEN_FILE="$TOKEN_DIR/$OWNER.token"
RULE_FILE=""
# render_slot — the broker rule declaring `github-app-<owner>` (the first
# in the directory; a per-act pair carries one declaration, pinned equal
# by github_app_installation_token_rules.rs) names the Secret, and
# credential-render.sh leaves the slot holding its live token or nothing.
# Its report goes to stderr: stdout is the plan's bytes. Exit 1 is a fault
# it has named, and the slot it left says what stands; anything else is
# a refusal of how it was run.
render_slot() {
    local f rc=0
    if [ -z "$RULE_FILE" ]; then
        for f in "$RULES_DIR"/*.toml; do
            [ -f "$f" ] || continue
            if grep -qF "credential_id = \"\\\"github-app-$OWNER\\\"\"" "$f"; then
                RULE_FILE="$f"
                break
            fi
        done
    fi
    [ -n "$RULE_FILE" ] \
        || refuse "no broker rule in $RULES_DIR declares the credential github-app-$OWNER, so nothing mints an installation token for $OWNER and this verb has none to act with (design 76c46869: the credential is named after the owner)"
    "$RENDER" --rule "$RULE_FILE" --dest "$TOKEN_FILE" --request "$REQUEST" >&2 || rc=$?
    case "$rc" in
        0 | 1) ;;
        *) refuse "rendering $TOKEN_FILE from $(basename "$RULE_FILE") refused (exit $rc, above)" ;;
    esac
}
slot_absent() { [ ! -e "$TOKEN_FILE" ] && [ ! -L "$TOKEN_FILE" ]; }
load_token() {
    local mode uid expires at now token="" try
    # THE MINT MAY STILL BE IN FLIGHT. The filing rule mints within seconds
    # of the request, and the runner's pass can come sooner; a plan that
    # refused on that race would abort a request David filed. So an
    # absent slot is rendered again, three tries over ~20 s, inside the
    # verb's 120 s.
    for try in 1 2 3; do
        render_slot
        slot_absent || break
        [ "$try" -eq 3 ] && break
        say "no live token for $OWNER yet (try $try of 3) — the broker mints one when the request is filed; rendering again in $((READBACK_SLEEP * 5)) s"
        sleep $((READBACK_SLEEP * 5))
    done
    if slot_absent; then
        refuse "no installation token for $OWNER at $TOKEN_FILE — the render (credential-render.sh, above) found no live one in the broker's Secret: the broker mints it when a GitHub request naming $OWNER is filed and when its plan is approved, from the App's installation on $OWNER (installation_id). Nothing here mints or places a credential"
    fi
    [ ! -L "$TOKEN_FILE" ] && [ -f "$TOKEN_FILE" ] \
        || refuse "the token slot $TOKEN_FILE is not a regular file (a link is refused: it could point anywhere)"
    mode="$(stat -c %a "$TOKEN_FILE")" || refuse "cannot read the mode of $TOKEN_FILE"
    case "$mode" in
        600 | 400) ;;
        *) refuse "the token slot $TOKEN_FILE is mode $mode — a token file must be 0600 or 0400" ;;
    esac
    # The mode says who may READ it; the owner says who could have WRITTEN
    # it (offsite-push.sh, backlog b176fd60 S5).
    uid="$(stat -c %u "$TOKEN_FILE")" || refuse "cannot read the owner of $TOKEN_FILE"
    [ "$uid" = "$TOKEN_OWNER_UID" ] \
        || refuse "the token slot $TOKEN_FILE is owned by uid $uid — it must be owned by uid $TOKEN_OWNER_UID (root)"
    # The slot is per OWNER and a plan keeps it: a token this request's
    # render did not place is another request's, and acting with it would
    # end it under that request (the review of c21401da, F1). The render
    # names the request beside every token it places.
    local held_for=""
    if [ ! -L "$TOKEN_FILE.request" ] && [ -f "$TOKEN_FILE.request" ]; then
        IFS= read -r held_for <"$TOKEN_FILE.request" || true
    fi
    [ "$held_for" = "$REQUEST" ] \
        || refuse "the token slot $TOKEN_FILE was rendered for request '${held_for:-none recorded}', not $REQUEST — another request's token is never acted with (the render names the request at $TOKEN_FILE.request)"
    # The broker's expiry, as the render recorded it. A token without one
    # was not minted by the broker, and a dead one would answer 401.
    [ ! -L "$TOKEN_FILE.expires-at" ] && [ -f "$TOKEN_FILE.expires-at" ] \
        || refuse "no recorded expiry at $TOKEN_FILE.expires-at — the render writes one beside every token it places; a token without one is not the broker's"
    IFS= read -r expires <"$TOKEN_FILE.expires-at" || [ -n "${expires:-}" ] \
        || refuse "cannot read the expiry at $TOKEN_FILE.expires-at"
    at="$(date -u -d "$expires" +%s 2>/dev/null)" || refuse "the recorded expiry '$expires' at $TOKEN_FILE.expires-at is not a time"
    now="$(date -u +%s)" || fail "cannot read the clock"
    [ "$at" -gt "$now" ] \
        || refuse "the token for $OWNER expired at $expires (recorded at $TOKEN_FILE.expires-at) — the broker's refresh has stopped, or the render has not run since"
    # Five minutes of margin: the verb may run 120 s and read back after
    # its act, and a token that dies mid-act leaves an act unproven
    # (review L3). The broker re-mints with forty minutes left.
    [ "$at" -gt $((now + 300)) ] \
        || refuse "the token for $OWNER expires within five minutes ($expires, recorded at $TOKEN_FILE.expires-at) — too close to outlive this verb; the broker's next refresh replaces it"
    IFS= read -r token <"$TOKEN_FILE" || [ -n "$token" ] || refuse "the token slot $TOKEN_FILE is empty"
    [ -n "$token" ] || refuse "the token slot $TOKEN_FILE is empty"
    [[ $token =~ [[:space:]] ]] && refuse "the token slot $TOKEN_FILE does not hold one token"
    # printf is a builtin: the value reaches the files without an argv.
    # $WORK/token is the private copy git's credential helper reads, so the
    # helper answers with the token validated here even if the slot is
    # replaced mid-run (review L6).
    (umask 077 && printf 'Authorization: Bearer %s\n' "$token" >"$WORK/auth" \
        && printf '%s\n' "$token" >"$WORK/token") \
        || fail "cannot write the private credential files"
    token=""
    TOKEN_EXPIRES="$expires"
    # A write is the act, and the token ends with it (finish → revoke).
    [ "$PLAN" -eq 1 ] || REVOKE=1
}

# revoke — the act is over, so is its token. The order is the broker's
# own (credential_rotate_github_app.rs): the Secret's copy is marked
# expired FIRST, so a death between here and the revoke never leaves a
# dead token that reads as live; then the slot goes; then GitHub is asked
# to end the token and a read proves it did. Runs in finish's subshell:
# any step that cannot be proven makes it non-zero.
revoke() {
    local ok=0 erc=0
    "$RENDER" --rule "$RULE_FILE" --dest "$TOKEN_FILE" --request "$REQUEST" --expire "$WORK/token" >&2 || erc=$?
    if [ "$erc" -ne 0 ]; then
        say "the broker's Secret was NOT marked expired (render exit $erc, above) — until it is re-minted it records a live expiry for the token revoked below"
        ok=1
    fi
    rm -f -- "$TOKEN_FILE" "$TOKEN_FILE.expires-at" "$TOKEN_FILE.request" || { say "cannot remove the slot $TOKEN_FILE"; ok=1; }
    api DELETE /installation/token
    case "$CODE" in
        204) say "DELETE /installation/token answered 204" ;;
        401) say "DELETE /installation/token answered 401 — the token was already dead" ;;
        *) say "DELETE /installation/token answered HTTP $CODE: $(said)"; ok=1 ;;
    esac
    api GET "/installation/repositories?per_page=1"
    if [ "$CODE" = 401 ]; then
        say "revoked — GET /installation/repositories with the act's token answers 401"
    else
        say "the act's token STILL AUTHENTICATES after the revoke (GET /installation/repositories answered HTTP $CODE) — it lives until $TOKEN_EXPIRES"
        ok=1
    fi
    return "$ok"
}

# --- the REST half -------------------------------------------------------------
# api <METHOD> <path> [<json body file>] — sets CODE; the body is in
# $WORK/out. `-q` first so no .curlrc is read; no redirect is followed
# (a moved repository answers 301 and is refused by name, and the token
# goes nowhere GitHub did not first answer from).
api() {
    local rc=0 data=()
    [ -z "${3:-}" ] || data=(-H 'Content-Type: application/json' --data-binary "@$3")
    : >"$WORK/out"
    CODE="$("$CURL" -q -sS -m 30 --max-redirs 0 -o "$WORK/out" -w '%{http_code}' \
        -H "@$WORK/auth" -H 'Accept: application/vnd.github+json' \
        -H 'X-GitHub-Api-Version: 2022-11-28' \
        -X "$1" ${data[@]+"${data[@]}"} "$API$2" 2>"$WORK/curl.err")" || rc=$?
    [ "$rc" -eq 0 ] || fail "$1 $2: curl exit $rc ($(head -c 300 "$WORK/curl.err" | tr '\n' ' ')) — GitHub did not answer, so there is nothing to act on"
    case "${CODE:-empty}" in
        empty | *[!0-9]*) fail "$1 $2: curl printed no HTTP status ('${CODE:-}')" ;;
    esac
}
# said — GitHub's own words for the last answer, or what it looked like.
said() {
    local m
    if m="$(jq -r '[.message // empty, (.errors // [] | .[] | (.message? // .code? // tostring))] | join("; ")' "$WORK/out" 2>/dev/null)" && [ -n "$m" ]; then
        printf '%s' "$m" | tr '\n' ' ' | cut -c1-300
    else
        printf 'a body that is not GitHub JSON: %s' "$(head -c 200 "$WORK/out" | tr '\n' ' ')"
    fi
}
# unexpected <what> — the last answer was none this act names.
unexpected() { fail "$1 answered HTTP $CODE: $(said)"; }

# --- the plan ------------------------------------------------------------------
# seal — hash the rendered plan; --plan prints it and exits; a write
# goes on only while the bytes hash to the approved one.
seal() {
    HASH="$(sha256sum "$WORK/plan" | cut -c1-64)" || fail "cannot hash the plan"
    if [ "$PLAN" -eq 1 ]; then
        cat "$WORK/plan"
        echo "plan-sha256: $HASH" >&2
        exit 0
    fi
    if [ "$HASH" != "$APPROVED" ]; then
        cat "$WORK/plan" >&2
        refuse "the plan as it stands (above) hashes to $HASH, not the approved $APPROVED — GitHub's state moved since it was signed, or this is not the request that was approved. Render and approve it again"
    fi
    # The capture before the act: the approved plan, on stdout, first.
    cat "$WORK/plan"
    echo
    echo "$ME: plan $APPROVED still holds"
}

# readback <check function> — the check reads GitHub and returns 0 when
# the act stands; a few tries, because an answer is not an effect and a
# read right after a write may lag.
readback() {
    local try
    for try in 1 2 3; do
        "$1" && return 0
        [ "$try" -eq 3 ] || sleep "$READBACK_SLEEP"
    done
    return 1
}

# =============================================================================
# create-repository
# =============================================================================
create_repository() {
    NAME="$2"
    VIS="$3"
    repo_name "the repository" "$NAME"
    case "$VIS" in
        private) PRIVATE=true ;;
        public) PRIVATE=false ;;
        *) refuse "visibility must be private or public, got '$VIS'" ;;
    esac
    load_token

    api GET "/orgs/$OWNER"
    case "$CODE" in
        200) ;;
        404) refuse "$OWNER is not an organisation this installation can see (GET /orgs/$OWNER answered 404) — an installation token creates repositories only in an organisation, POST /orgs/{org}/repos" ;;
        *) unexpected "GET /orgs/$OWNER" ;;
    esac
    jq_doc_file "$WORK/out" || fail "GET /orgs/$OWNER answered 200 with no JSON document"
    ORG_ID="$(jq -er '.id | numbers' "$WORK/out")" || fail "GET /orgs/$OWNER answered 200 without a numeric id: $(said)"
    ORG_LOGIN="$(jq -er '.login | strings' "$WORK/out")" || fail "GET /orgs/$OWNER answered 200 without a login"
    [ "${ORG_LOGIN,,}" = "${OWNER,,}" ] || refuse "GET /orgs/$OWNER answered as $ORG_LOGIN"

    api GET "/repos/$OWNER/$NAME"
    case "$CODE" in
        404) STATE=absent ;;
        200) STATE=exists ;;
        301) refuse "$OWNER/$NAME has moved (GitHub answers 301: renamed or transferred) — name the repository as it is now" ;;
        *) unexpected "GET /repos/$OWNER/$NAME" ;;
    esac
    if [ "$STATE" = exists ]; then
        jq_doc_file "$WORK/out" && jq -e 'type == "object" and (.full_name | type == "string") and (.private | type == "boolean") and (.fork | type == "boolean")' "$WORK/out" >/dev/null 2>&1 \
            || fail "GET /repos/$OWNER/$NAME answered 200 without full_name, private and fork: $(said)"
        local full fork priv archived id
        full="$(jq -r '.full_name' "$WORK/out")" || fail "cannot read full_name"
        fork="$(jq -r '.fork' "$WORK/out")" || fail "cannot read fork"
        priv="$(jq -r '.private' "$WORK/out")" || fail "cannot read private"
        archived="$(jq -r '.archived == true' "$WORK/out")" || fail "cannot read archived"
        id="$(jq -r '.id // "none"' "$WORK/out")" || fail "cannot read id"
        [ "${full,,}" = "${OWNER,,}/${NAME,,}" ] || refuse "GET /repos/$OWNER/$NAME answered as $full"
        [ "$fork" = false ] || refuse "$full already exists and is a FORK (of $(jq -r '.parent.full_name // "an unnamed parent"' "$WORK/out")) — this verb never makes or adopts a fork"
        [ "$archived" = false ] || refuse "$full already exists and is archived"
        [ "$priv" = "$PRIVATE" ] || refuse "$full already exists with private=$priv — this verb creates a repository; it never changes one's visibility"
        # `private` is two-valued and visibility is three (internal): a
        # private-true internal repository is not the one asked for (L8).
        vis_now="$(jq -r '.visibility // "not stated"' "$WORK/out")" || fail "cannot read visibility"
        [ "$vis_now" = "$VIS" ] || refuse "$full already exists with visibility $vis_now — this verb never changes one's visibility"
        EXISTING="id $id, visibility $VIS, private $priv, fork false, not archived"
    fi

    jq -n -c --arg name "$NAME" --arg vis "$VIS" --argjson private "$PRIVATE" \
        '{name: $name, private: $private, visibility: $vis, auto_init: false}' >"$WORK/body.json" \
        || fail "cannot build the request body"
    {
        echo "plan: github-create-repository"
        echo "repository: $OWNER/$NAME"
        echo "organisation: $ORG_LOGIN (id $ORG_ID)"
        echo "visibility: $VIS"
        echo "credential: the GitHub App installation on $OWNER, token slot $TOKEN_FILE"
        if [ "$STATE" = absent ]; then
            echo "state: absent — GET /repos/$OWNER/$NAME answers 404"
            echo "act: POST /orgs/$OWNER/repos $(cat "$WORK/body.json")"
        else
            echo "state: exists as declared — $EXISTING"
            echo "act: none — nothing to create; the write reads it back and changes nothing"
        fi
        echo "proof: GET /repos/$OWNER/$NAME answers 200 as $OWNER/$NAME, visibility $VIS, private $PRIVATE, fork false, not archived"
        echo "never: a fork, a visibility change, an archive or a delete, a repository outside $OWNER"
    } >"$WORK/plan"
    seal

    if [ "$STATE" = absent ]; then
        api POST "/orgs/$OWNER/repos" "$WORK/body.json"
        [ "$CODE" = 201 ] || fail "POST /orgs/$OWNER/repos answered HTTP $CODE: $(said) — GitHub did not create $OWNER/$NAME"
        echo "$ME: POST /orgs/$OWNER/repos answered 201"
    fi
    repo_reads_back() {
        api GET "/repos/$OWNER/$NAME"
        [ "$CODE" = 200 ] || { say "read back: GET /repos/$OWNER/$NAME answered HTTP $CODE: $(said)"; return 1; }
        jq_doc_file "$WORK/out" && jq -e --arg full "${OWNER,,}/${NAME,,}" --argjson private "$PRIVATE" --arg vis "$VIS" \
            '(.full_name | ascii_downcase) == $full and .private == $private and .visibility == $vis
             and .fork == false and .archived == false' \
            "$WORK/out" >/dev/null 2>&1 && return 0
        say "read back: $OWNER/$NAME answers $(jq -c '{full_name, visibility, private, fork, archived}' "$WORK/out" 2>/dev/null)"
        return 1
    }
    readback repo_reads_back \
        || fail "$OWNER/$NAME does not read back as $OWNER/$NAME, visibility $VIS, fork false, not archived (above)$([ "$STATE" = absent ] && echo ' — the POST answered 201, so what GitHub holds must be read before anything else is done')"
    echo "$ME: proven — GET /repos/$OWNER/$NAME reads back as $OWNER/$NAME, private $PRIVATE, fork false$([ "$STATE" = exists ] && echo '; nothing was created, it already stood as declared')"
}

# =============================================================================
# set-branch-protection
# =============================================================================
# norm — a ruleset reduced to what this verb declares, so GitHub's own
# additions (ids, timestamps, links, parameter defaults) and its ordering
# do not read as drift. Used on the desired body and on every read-back.
NORM='def norm: {
    name, target, enforcement,
    include: ((.conditions.ref_name.include // []) | sort),
    exclude: ((.conditions.ref_name.exclude // []) | sort),
    bypass: ([(.bypass_actors // [])[] | "\(.actor_type):\(.actor_id):\(.bypass_mode)"] | sort),
    rules: ([(.rules // [])[]
        | if .type == "required_status_checks" then
            # Each check with the App it is pinned to (`any` when none): a
            # check satisfied only by another App is a different rule (L2).
            "required_status_checks " + ([(.parameters.required_status_checks // [])[]
                | "\(.context)@\(.integration_id // "any")"] | sort | join(","))
              + " strict=" + ((.parameters.strict_required_status_checks_policy // false) | tostring)
              + " do_not_enforce_on_create=" + ((.parameters.do_not_enforce_on_create // false) | tostring)
          elif .type == "update" then
            "update fetch_and_merge=" + ((.parameters.update_allows_fetch_and_merge // false) | tostring)
          else .type end] | sort)
};'
norm_lines() {
    jq -r "$NORM"' norm
        | "  name: \(.name)", "  target: \(.target)", "  enforcement: \(.enforcement)",
          "  include: \(.include | join(" "))",
          "  exclude: \(if (.exclude | length) == 0 then "none" else (.exclude | join(" ")) end)",
          "  bypass: \(if (.bypass | length) == 0 then "none" else (.bypass | join(" ")) end)",
          (.rules[] | "  rule: \(.)")' "$1"
}

set_branch_protection() {
    REPO="$2"
    PATTERN="$3"
    ALLOW="$4"
    CHECKS="$5"
    repo_name "the repository" "$REPO"
    [[ $PATTERN =~ ^[A-Za-z0-9_*][A-Za-z0-9._/*-]{0,199}$ ]] \
        || refuse "the branch pattern '$PATTERN' is not one (letters, digits, . _ - / *, not leading . - or /)"
    case "$PATTERN" in
        *//* | *..* | */ | *.lock | */.* | .*) refuse "the branch pattern '$PATTERN' names no branch git allows" ;;
    esac
    [[ $ALLOW =~ ^(none|(app|team):[0-9]{1,12}(,(app|team):[0-9]{1,12}){0,9})$ ]] \
        || refuse "the push allowlist '$ALLOW' is not 'none' or up to ten app:<App id> / team:<team id>, comma-separated (a ruleset's bypass actors are Apps and teams, never a user)"
    [[ $CHECKS =~ ^(none|[A-Za-z0-9][A-Za-z0-9._/:-]{0,99}(,[A-Za-z0-9][A-Za-z0-9._/:-]{0,99}){0,19})$ ]] \
        || refuse "the required checks '$CHECKS' are not 'none' or up to twenty check names, comma-separated, without whitespace"
    BYPASS="$(jq -n -c --arg a "$ALLOW" 'if $a == "none" then [] else ($a | split(",") | map(split(":")
        | {actor_id: (.[1] | tonumber), actor_type: (if .[0] == "app" then "Integration" else "Team" end), bypass_mode: "always"})) end')" \
        || fail "cannot build the bypass list"
    jq_doc_text "$BYPASS" || fail "the bypass list came out empty"
    jq -e 'map("\(.actor_type):\(.actor_id)") | length == (unique | length)' <<<"$BYPASS" >/dev/null \
        || refuse "the push allowlist '$ALLOW' names an actor twice"
    CHECKS_JSON="$(jq -n -c --arg c "$CHECKS" 'if $c == "none" then [] else ($c | split(",")) end')" \
        || fail "cannot build the check list"
    jq_doc_text "$CHECKS_JSON" || fail "the check list came out empty"
    jq -e 'length == (unique | length)' <<<"$CHECKS_JSON" >/dev/null \
        || refuse "the required checks '$CHECKS' name a check twice"
    RS_NAME="boss: refs/heads/$PATTERN"
    load_token

    api GET "/repos/$OWNER/$REPO"
    case "$CODE" in
        200) ;;
        404) refuse "$OWNER/$REPO does not exist or this installation cannot see it (GET answered 404)" ;;
        301) refuse "$OWNER/$REPO has moved (GitHub answers 301) — name the repository as it is now" ;;
        *) unexpected "GET /repos/$OWNER/$REPO" ;;
    esac
    jq_doc_file "$WORK/out" || fail "GET /repos/$OWNER/$REPO answered 200 with no JSON document"
    jq -e --arg full "${OWNER,,}/${REPO,,}" '(.full_name // "" | ascii_downcase) == $full' "$WORK/out" >/dev/null 2>&1 \
        || refuse "GET /repos/$OWNER/$REPO answered as $(jq -r '.full_name // "no full_name"' "$WORK/out" 2>/dev/null)"
    jq -e '.archived != true' "$WORK/out" >/dev/null 2>&1 || refuse "$OWNER/$REPO is archived"

    api GET "/repos/$OWNER/$REPO/rulesets?includes_parents=false&per_page=100"
    [ "$CODE" = 200 ] || unexpected "GET /repos/$OWNER/$REPO/rulesets"
    jq_doc_file "$WORK/out" || fail "the rulesets of $OWNER/$REPO answered 200 with no JSON document"
    jq -e 'type == "array"' "$WORK/out" >/dev/null 2>&1 || fail "the rulesets of $OWNER/$REPO answered 200 with no list: $(said)"
    # A limit is not a filter: a full page may hide a namesake.
    jq -e 'length < 100' "$WORK/out" >/dev/null || refuse "$OWNER/$REPO lists 100 rulesets on one page — this verb reads one page and cannot be sure it saw every ruleset named $RS_NAME"
    local ids n
    ids="$(jq -r --arg n "$RS_NAME" '[.[] | select(.name == $n) | .id | tostring] | join(" ")' "$WORK/out")" \
        || fail "cannot read the rulesets of $OWNER/$REPO"
    n="$(wc -w <<<"$ids")"
    case "$n" in
        0) RS_ID="" ;;
        1) RS_ID="$ids" ;;
        *) refuse "$OWNER/$REPO carries $n rulesets named '$RS_NAME' (ids $ids) — this verb owns one per pattern and will not guess which" ;;
    esac
    [[ -z $RS_ID || $RS_ID =~ ^[0-9]+$ ]] || fail "the ruleset named '$RS_NAME' has no numeric id ('$RS_ID')"

    jq -n --arg name "$RS_NAME" --arg inc "refs/heads/$PATTERN" \
        --argjson bypass "$BYPASS" --argjson checks "$CHECKS_JSON" '{
            name: $name, target: "branch", enforcement: "active",
            conditions: {ref_name: {include: [$inc], exclude: []}},
            bypass_actors: $bypass,
            rules: ([{type: "creation"},
                     {type: "update", parameters: {update_allows_fetch_and_merge: false}},
                     {type: "deletion"},
                     {type: "non_fast_forward"}]
                    + (if ($checks | length) > 0 then
                         [{type: "required_status_checks", parameters: {
                             strict_required_status_checks_policy: false,
                             do_not_enforce_on_create: false,
                             required_status_checks: ($checks | map({context: .}))}}]
                       else [] end))}' >"$WORK/desired.json" || fail "cannot build the ruleset"
    DESIRED="$(jq -S -c "$NORM"' norm' "$WORK/desired.json")" || fail "cannot normalise the ruleset"

    # is_the_ruleset — the last answer IS ruleset $RS_ID under the verb's
    # name. An empty or `null` 200, or another ruleset's body, normalises
    # to an empty rule set and rendered a signable PUT (review M1).
    is_the_ruleset() {
        jq_doc_file "$WORK/out" && jq -e --argjson id "$RS_ID" --arg n "$RS_NAME" \
            'type == "object" and .id == $id and .name == $n' "$WORK/out" >/dev/null 2>&1
    }
    CURRENT=""
    if [ -n "$RS_ID" ]; then
        api GET "/repos/$OWNER/$REPO/rulesets/$RS_ID"
        [ "$CODE" = 200 ] || unexpected "GET /repos/$OWNER/$REPO/rulesets/$RS_ID"
        is_the_ruleset || fail "GET /repos/$OWNER/$REPO/rulesets/$RS_ID answered 200 with a body that is not ruleset $RS_ID named '$RS_NAME': $(head -c 200 "$WORK/out" | tr '\n' ' ')"
        cp "$WORK/out" "$WORK/current.json" || fail "cannot keep the current ruleset"
        CURRENT="$(jq -S -c "$NORM"' norm' "$WORK/current.json")" || fail "the ruleset $RS_ID of $OWNER/$REPO does not read as a ruleset: $(said)"
    fi

    if [ -z "$RS_ID" ]; then
        ACT_LINE="POST /repos/$OWNER/$REPO/rulesets"
    elif [ "$CURRENT" = "$DESIRED" ]; then
        ACT_LINE=""
    else
        ACT_LINE="PUT /repos/$OWNER/$REPO/rulesets/$RS_ID"
    fi
    {
        echo "plan: github-set-branch-protection"
        echo "repository: $OWNER/$REPO"
        echo "ruleset: $RS_NAME ($([ -n "$RS_ID" ] && echo "id $RS_ID" || echo absent))"
        echo "branches: refs/heads/$PATTERN"
        echo "push allowlist: $([ "$ALLOW" = none ] && echo 'none — nobody may create, update or delete a matching branch' || echo "$ALLOW (bypass actors: they alone may create, update or delete a matching branch)")"
        echo "required checks: $CHECKS"
        echo "credential: the GitHub App installation on $OWNER, token slot $TOKEN_FILE"
        echo "current:"
        if [ -n "$RS_ID" ]; then norm_lines "$WORK/current.json"; else echo "  absent"; fi
        echo "desired:"
        norm_lines "$WORK/desired.json"
        if [ -n "$ACT_LINE" ]; then
            echo "act: $ACT_LINE $(jq -c . "$WORK/desired.json")"
        else
            echo "act: none — the ruleset already reads as desired; the write reads it back and changes nothing"
        fi
        echo "proof: the ruleset reads back, normalised, as desired (above)"
        echo "never: a ruleset this verb did not name, a delete of any ruleset, a rule outside the fixed set above"
    } >"$WORK/plan"
    seal

    if [ -n "$ACT_LINE" ]; then
        if [ -z "$RS_ID" ]; then
            api POST "/repos/$OWNER/$REPO/rulesets" "$WORK/desired.json"
            [ "$CODE" = 201 ] || fail "POST /repos/$OWNER/$REPO/rulesets answered HTTP $CODE: $(said) — no ruleset was created"
            jq_doc_file "$WORK/out" && RS_ID="$(jq -er '.id | numbers' "$WORK/out")" && [ -n "$RS_ID" ] \
                || fail "POST /repos/$OWNER/$REPO/rulesets answered 201 without a numeric id — read the repository's rulesets before anything else is done"
        else
            api PUT "/repos/$OWNER/$REPO/rulesets/$RS_ID" "$WORK/desired.json"
            [ "$CODE" = 200 ] || fail "PUT /repos/$OWNER/$REPO/rulesets/$RS_ID answered HTTP $CODE: $(said) — the ruleset was not replaced"
        fi
        echo "$ME: $ACT_LINE answered $CODE (ruleset id $RS_ID)"
    fi
    ruleset_reads_back() {
        local got
        api GET "/repos/$OWNER/$REPO/rulesets/$RS_ID"
        [ "$CODE" = 200 ] || { say "read back: GET /repos/$OWNER/$REPO/rulesets/$RS_ID answered HTTP $CODE: $(said)"; return 1; }
        is_the_ruleset || { say "read back: GET /repos/$OWNER/$REPO/rulesets/$RS_ID answered with a body that is not ruleset $RS_ID named '$RS_NAME'"; return 1; }
        got="$(jq -S -c "$NORM"' norm' "$WORK/out" 2>/dev/null)" || { say "read back: the ruleset does not read as one"; return 1; }
        [ "$got" = "$DESIRED" ] && return 0
        say "read back: ruleset $RS_ID reads $got"
        return 1
    }
    readback ruleset_reads_back \
        || fail "ruleset $RS_ID on $OWNER/$REPO does not read back as desired (above; wanted $DESIRED)"
    echo "$ME: proven — ruleset $RS_ID ($RS_NAME) on $OWNER/$REPO reads back as desired$([ -z "$ACT_LINE" ] && echo '; nothing was written, it already stood as declared')"
}

# =============================================================================
# delete-refs
# =============================================================================
delete_refs() {
    REPO="$2"
    REFS_ARG="$3"
    REASON="$4"
    repo_name "the repository" "$REPO"
    [[ $REASON =~ ^(none|[a-z0-9][a-z0-9-]{7,79})$ ]] \
        || refuse "the reason '$REASON' is not 'none' or a slug of 8 to 80 lowercase letters, digits and hyphens"
    [[ $REFS_ARG =~ ^(heads|tags)/[A-Za-z0-9_][A-Za-z0-9._/-]*(,(heads|tags)/[A-Za-z0-9_][A-Za-z0-9._/-]*)*$ ]] \
        || refuse "the refs '$REFS_ARG' are not heads/<branch> or tags/<tag>, each named in full, comma-separated"
    command -v git >/dev/null 2>&1 || fail "git is not on PATH"
    local r dups
    IFS=, read -r -a WANTED <<<"$REFS_ARG"
    [ "${#WANTED[@]}" -le "$MAX_REFS" ] || refuse "${#WANTED[@]} refs named — at most $MAX_REFS in one request"
    for r in "${WANTED[@]}"; do
        git check-ref-format "refs/$r" || refuse "refs/$r is not a ref name git allows"
    done
    dups="$(printf '%s\n' "${WANTED[@]}" | sort | uniq -d | paste -sd, -)" || fail "cannot compare the refs named"
    [ -z "$dups" ] || refuse "the refs name $dups more than once"
    load_token

    # git sees no system or user config (a root unit reads /etc/gitconfig,
    # where an insteadOf would send this elsewhere) and never prompts.
    : >"$WORK/gitconfig"
    export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$WORK/gitconfig" GIT_TERMINAL_PROMPT=0
    git init -q --bare "$WORK/local.git" 2>"$WORK/git.err" || fail "cannot make a scratch repository: $(head -c 300 "$WORK/git.err" | tr '\n' ' ')"
    URL="$GIT_BASE/$OWNER/$REPO.git"
    # The helper reads the slot when git asks, and answers ONLY
    # https://github.com (offsite-push.sh, b176fd60 S5): the argv carries
    # the slot's path, never its contents.
    # The helper is this script's own `credential-helper` word, reading the
    # private copy in $WORK (review L6, L7). No redirect is followed, so the
    # credential is only ever presented to the host git first asked (L1).
    NET=(-c credential.helper= -c "credential.helper=!bash '$SELF' credential-helper '$WORK/token'"
        -c http.followRedirects=false -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60)
    g() { git -C "$WORK/local.git" "${NET[@]}" "$@"; }

    # read_remote — every ref and the default branch, as GitHub has them now.
    read_remote() {
        g ls-remote "$URL" >"$WORK/remote" 2>"$WORK/git.err" \
            || fail "reading $OWNER/$REPO with git ls-remote failed: $(head -c 300 "$WORK/git.err" | tr '\n' ' ')"
        g ls-remote --symref "$URL" HEAD >"$WORK/head" 2>"$WORK/git.err" \
            || fail "reading $OWNER/$REPO's default branch failed: $(head -c 300 "$WORK/git.err" | tr '\n' ' ')"
    }
    read_remote
    DEFAULT="$(awk -F'\t' '$1 ~ /^ref: refs\/heads\// && $2 == "HEAD" { sub(/^ref: refs\/heads\//, "", $1); print $1; exit }' "$WORK/head")" \
        || fail "cannot read the default branch from the listing"
    # A repository holding refs always advertises HEAD; one that does not
    # cannot say which branch is the default, and the default is the one
    # ref this verb must never plan (review L9). Empty is an empty repo.
    if [ -z "$DEFAULT" ] && [ -s "$WORK/remote" ]; then
        refuse "$OWNER/$REPO lists refs but advertises no HEAD, so its default branch cannot be read — and the default branch is never deleted"
    fi

    : >"$WORK/delete"
    : >"$WORK/absent"
    local sha
    for r in "${WANTED[@]}"; do
        if [ -n "$DEFAULT" ] && [ "$r" = "heads/$DEFAULT" ]; then
            refuse "refs/heads/$DEFAULT is $OWNER/$REPO's DEFAULT branch — GitHub refuses to delete it, and changing the default is not this verb's"
        fi
        case "$r" in
            heads/main | heads/master)
                [ "$REASON" != none ] || refuse "refs/$r is named with no reason — this verb deletes main or master only when the request carries one (its last argument before the hash)"
                ;;
        esac
        sha="$(awk -F'\t' -v ref="refs/$r" '$2 == ref { print $1; exit }' "$WORK/remote")" \
            || fail "cannot read the listing"
        if [ -n "$sha" ]; then
            [[ $sha =~ ^[0-9a-f]{40}$ ]] || fail "refs/$r lists as '$sha', which is not a sha"
            printf '%s refs/%s\n' "$sha" "$r" >>"$WORK/delete"
        else
            printf 'refs/%s\n' "$r" >>"$WORK/absent"
        fi
    done
    sort -k2 -o "$WORK/delete" "$WORK/delete" || fail "cannot sort"
    sort -o "$WORK/absent" "$WORK/absent" || fail "cannot sort"
    local nd na
    nd="$(wc -l <"$WORK/delete")" || fail "cannot count"
    na="$(wc -l <"$WORK/absent")" || fail "cannot count"
    {
        echo "plan: github-delete-refs"
        echo "repository: $OWNER/$REPO"
        echo "default branch: ${DEFAULT:-none advertised}"
        echo "reason: $REASON"
        echo "credential: the GitHub App installation on $OWNER, token slot $TOKEN_FILE"
        echo "refs named: ${#WANTED[@]} — to delete: $nd, already absent: $na"
        while read -r sha r; do echo "delete $r at $sha"; done <"$WORK/delete"
        while read -r r; do echo "absent $r — nothing to delete"; done <"$WORK/absent"
        if [ "$nd" -gt 0 ]; then
            echo "act: one atomic git push of $nd delete(s), each leased to the sha above (--force-with-lease=<ref>:<sha>) — all of them or none"
        else
            echo "act: none — every named ref is already absent"
        fi
        echo "proof: git ls-remote lists none of the deleted refs"
        echo "never: the default branch, a ref not named above, a ref at a sha other than the one above, a push that writes anything"
    } >"$WORK/plan"
    seal

    if [ "$nd" -gt 0 ]; then
        local leases=() specs=() rc=0
        while read -r sha r; do
            leases+=("--force-with-lease=$r:$sha")
            specs+=(":$r")
        done <"$WORK/delete"
        g push --porcelain --atomic "${leases[@]}" "$URL" "${specs[@]}" >"$WORK/push.out" 2>"$WORK/push.err" || rc=$?
        if [ "$rc" -ne 0 ]; then
            say "the push was refused or failed (git exit $rc): $(awk -F'\t' '$1 == "!" { printf "%s %s; ", $2, $3 }' "$WORK/push.out")$(head -c 300 "$WORK/push.err" | tr '\n' ' ')"
            say "an atomic push lands all of its deletes or none; reading $OWNER/$REPO back to say which"
        else
            echo "$ME: the atomic push answered: $(awk -F'\t' '$1 == "-" { printf "%s ", $2 }' "$WORK/push.out")"
        fi
    fi
    refs_read_back() {
        local left="" s ref
        g ls-remote "$URL" >"$WORK/remote" 2>"$WORK/git.err" \
            || { say "read back: git ls-remote failed: $(head -c 300 "$WORK/git.err" | tr '\n' ' ')"; return 1; }
        while read -r s ref; do
            if awk -F'\t' -v ref="$ref" '$2 == ref { found = 1 } END { exit !found }' "$WORK/remote"; then
                left="$left $ref"
            fi
        done <"$WORK/delete"
        [ -z "$left" ] && return 0
        say "read back: $OWNER/$REPO still lists$left"
        return 1
    }
    readback refs_read_back \
        || fail "$OWNER/$REPO still holds ref(s) this plan deletes (above)$([ "${rc:-0}" -ne 0 ] && echo " — the push was refused (git exit $rc), so none of them was deleted")"
    [ "${rc:-0}" -eq 0 ] || fail "the push exited $rc, yet none of the planned refs lists any more — read $OWNER/$REPO before anything else is done"
    if [ "$nd" -gt 0 ]; then
        echo "$ME: proven — $nd ref(s) deleted from $OWNER/$REPO, and git ls-remote lists none of them"
    else
        echo "$ME: proven — every named ref is absent from $OWNER/$REPO; nothing was pushed"
    fi
}

case "$ACT" in
    create-repository) create_repository "$@" ;;
    set-branch-protection) set_branch_protection "$@" ;;
    delete-refs) delete_refs "$@" ;;
esac
exit 0
