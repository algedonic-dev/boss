#!/usr/bin/env bash
#
# offsite-push — push forge main to the PRIVATE disaster-recovery copy
# (algedonic-dev/boss-dr) with its OWN credential slot, with a PLAIN push
# (no force, no --mirror, no prune), read it back, and only then remove
# Forgejo's own push mirror. The public fork (dauld/boss-mirror) is
# declared with NO branch (see NOTHING DECLARED below): nothing reaches
# it from here.
#
#   offsite-push.sh
#
# Run by forge-converge.sh on every tick, after protect-main.sh and after
# the render of the DR token. What it pushes, where, and with which token
# file is declared in infra/forge/offsite-push.json — a list of targets,
# each {remote, branches, credential, token_file}.
#
# NEVER MAIN ON THE FORK (backlog 67931115, design 1f35a3e8, David
# 2026-09-27). dauld/boss-mirror is a fork of the public
# algedonic-dev/boss, so it is public and cannot be made private: until
# that date this push put forge main — every train, unsigned, never
# secrets-scanned — on public GitHub within one tick, while the publish
# flow's scan and passkey gated only the PR. Car 1 of that item took
# main off the list; until the DR copy below landed, forge main had no
# off-site copy.
#
# MAIN RIDES ALONE TO A PRIVATE DR COPY (backlog 761bc8a9, car 2; design
# 76155676, David 2026-09-27: algedonic-dev/boss-dr — org-owned, private,
# not a fork). Main goes to the private repository outside the fork
# network. The declaration is refused (exit 2) unless main is declared on
# exactly one target and that target declares nothing else, so main
# cannot come back to the fork by a one-word edit; and no two targets may
# share a remote or a token file, so each target has its own credential
# slot. The tests the_public_fork_receives_nothing_from_the_converge and
# the_dr_copy_is_declared_main_only pin which remote carries which
# branches.
#
# WHY IT EXISTS (backlog 21d54f4a, parent f9256445, design d812f1b7)
# ------------------------------------------------------------------
# On 2026-09-25 at 20:10:42Z forge main went back from c85941b4 (train
# #687's merge, one second old) to 777a5888, written by Forgejo itself
# (`Gitea <gitea@fake.local> update by push`). The writer was the forge's
# push mirror to github.com/dauld/boss-fork: Forgejo 16.0.2 adds a push
# mirror with `git remote add --mirror` (services/mirror/mirror_push.go),
# whose fetch refspec `+refs/*:refs/*` makes every sync write the values
# it just pushed back onto the forge's OWN refs/heads/* — so a sync that
# read main before the merge rewound main after it. Branch protection
# cannot stop it (protect-main.sh says why: the pre-receive hook never
# runs for it).
#
# A branch filter does not fix it. Triage a53e92a1 read v16.0.2's source
# and reproduced both halves: a mirror created WITH a filter is added
# without --mirror, so the write-back goes; but every sync is still
# `git push -f --mirror` (modules/git/repo.go Push), which force-pushes
# and PRUNES every branch on the target that the forge lacks, filter or
# no filter. David, 2026-09-26: "I just want to make sure we can't
# accidentally wipe ourselves" — no mirror may wipe what it mirrors. So
# the decided fix (packet 21d54f4a, decided_2026-09-26) is to REMOVE the
# Forgejo push mirror and replace it with this: our own push, declared
# in the tree, that can neither overwrite nor delete anything off-site.
#
# WHAT THE PUSH IS
# ----------------
# `git push <remote> refs/heads/<b>:refs/heads/<b>...` for each declared
# branch — no leading `+`, no --force, no --mirror, no --prune. So:
#   * a fast-forward lands; a branch new to the target is created;
#   * a NON-fast-forward (forge main rewound, or a target that moved on
#     its own) is REFUSED by git, named here, recorded on the converge's
#     packet, and exit 1 — the target keeps what it had, never
#     overwritten. The next tick asks again; a person decides;
#   * a branch the target holds and the forge does not is left alone.
# It pushes from a PRIVATE bare clone ($STATE_DIR/boss.git, root's) that
# FETCHES the declared branches from the forge's repository by path.
# The forge repository is only ever read — nothing here adds a remote to
# it or runs a push inside it, which is the shape that rewound main — and
# root never writes into a repository the Forgejo account owns
# (publish-github-pr.sh carries the reasoning; the path is derived by
# forge-repo-path.sh, the one definition both share).
#
# WHAT READS IT: algedonic-dev/boss-dr is the disaster-recovery copy of
# main.
# dauld/boss-mirror (canonical name; GitHub 301-redirects boss-fork
# there) is the fork publish-github-pr.sh opens its PRs from
# (measured_2026-09-26_mirror_consumers on packet 21d54f4a). Each publish
# branch, publish/<date>-<snapshot> since backlog 1f0aa60d, is pushed to
# the forge FIRST by that verb (ce5339d6) and to the fork by the verb
# itself. Until backlog a2b58aab this push carried publish/* too, which
# was the only thing it added: an exit to the public fork for any forge
# branch so named, scanned or not. It carries nothing there now. The verb
# also DELETES a publish branch once its PR reads closed — forge first,
# then fork (backlog 1a2bcf11).
#
# NOTHING DECLARED FOR THE FORK (backlog a2b58aab). The fork's target
# declares an empty branch list, and
# the_public_fork_receives_nothing_from_the_converge (offsite_push_sh.rs)
# refuses any branch in it. A forge branch NAME vouches for nothing:
# every holder of a forge write credential for user david can push
# refs/heads/publish/<anything>, and the forge protects main alone, so
# carrying publish/* put any such branch on the public fork within one
# tick, never scanned, never approved. publish-github-pr.sh pushes each
# snapshot to the fork itself, after its secrets scan and the passkey
# check, so the only way onto the fork is that verb. A target that
# declares nothing needs no token — a converge must not go red for a
# credential it does not use — and is named in the verdict as receiving
# nothing. An empty list never reaches `git push`, which with no refspec
# falls back to push.default rather than refusing.
#
# NO REFUSAL IS EXPECTED. The only branch pushed is main, to the DR copy,
# and forge main only ever moves forward (protect-main.sh). A
# non-fast-forward there is forge main REWOUND — the 2026-09-25 shape —
# or a DR copy that moved on its own: a real disagreement, never timing.
# Read both before anything moves.
#
# ORDER: PUSH, READ BACK, THEN REMOVE THE MIRROR
# ----------------------------------------------
# The Forgejo push mirror is deleted (DELETE /repos/{repo}/push_mirrors/
# {name}, every one listed — any Forgejo push mirror is `-f --mirror`)
# on EVERY run that can reach the forge's API — whatever happened to the
# push. A refusal (a non-fast-forward, named), a ref that does not read
# back, an empty or loose credential slot, a fetch that fails, a push or
# read-back that fails: each is recorded, nothing more is pushed, and the
# mirror half STILL runs; the run then exits with the worse of the two
# codes and a verdict naming both (adversarial review of fcf6042f, F1).
# That mirror is `git push -f --mirror`: aimed at the public fork it is
# the exposure 67931115 removed, aimed at boss-dr it is the wipe
# 21d54f4a removed, so keeping it is never the safe side of a failure.
# Until this car a failed DR push kept it — a rule left over from when
# the mirror WAS the off-site copy. Only two things skip the removal,
# because they are the removal's own inputs: no forge header, and a
# mirror list that cannot be read. Measured 2026-09-27: the live
# forge already carries no push mirror, every converge answering "the
# forge carries no push mirror". The list is read back after the delete:
# an answer is not an effect.
#
# What the API delete removes, read off v16.0.2 (d7471ea4,
# routers/api/v1/repo/mirror.go DeletePushMirrorByRemoteName): the push
# mirror's DATABASE ROW only. Every sync starts from that row
# (services/mirror/mirror_push.go SyncPushMirror returns at "!exist"), so
# with the row gone nothing pushes and nothing writes back — the writer
# is removed. The web UI's delete also runs `git remote rm` in the
# repository (routers/web/repo/setting/setting.go, push-mirror-remove);
# the API's does not, so the old remote's config section stays in the
# forge repository, inert: no row, no sync, no push through it. It is not
# removed from here, because that is a write into the Forgejo account's
# repository, which this script never makes.
#
# CREDENTIALS — all read from files, none ever on an argv or printed
#   GitHub: ONE token file per target, named by the target's
#     `token_file` (and its registry id by `credential`). Each target that
#     declares a branch needs its file to exist, be non-empty, 0600/0400,
#     and owned by root; a target that declares none (the fork, today) is
#     never pushed, so its file is not read or checked. Each reaches git through
#     its own credential helper that reads the file when git asks, and
#     answers only https://github.com AND only its own target's path
#     (credential.useHttpPath) — so the DR token can never be handed to
#     the fork and the fork's token can never write the DR copy. Nothing
#     here mints or places a credential.
#     * dauld-github-token, /etc/boss-publish/github.token — the fork's,
#       the same file publish-github-pr.sh reads (pinned equal by
#       offsite_push_sh.rs). Unused while the fork declares no branch.
#     * github-dr-push-token, /etc/boss-publish/github-dr.token — the DR
#       copy's: a one-hour INSTALLATION TOKEN of the GitHub App installed
#       on the algedonic-dev organisation (design 76155676). The App's
#       private key, App id and installation id are the root material,
#       placed once by David; the installation token is minted FROM them,
#       so it is never placed by hand and never a personal access token
#       (CLAUDE.md §Doors, "the boundary is derivability"). The credential
#       broker's `credential.rotate.github-app-installation` handler mints
#       it into ONE named Secret, boss/github-dr-push-token (the rule
#       infra/dispatcher/rules/broker-rotates-the-github-dr-push-token.toml),
#       and forge-converge.sh renders that Secret into this file through
#       credential-render.sh, root:root 0600, on every tick before this
#       script runs — or removes the file when the Secret holds nothing
#       live. credential_render_sh.rs pins the render's destination equal
#       to this target's token_file. This script reads only the FILE, and
#       owes nothing to how it was filled. The id is issuer-neutral, as
#       the registry's other consumer credentials are
#       (forge-host-checkout-token, boss-dev-forge-token): it names what
#       the credential is for, not who minted it. Git sends it as username
#       x-access-token, which GitHub accepts for an installation token as
#       for a personal one. A tick whose render found nothing live leaves
#       the slot empty, and so that tick's off-site copy of main — which
#       the next paragraph makes loud.
#     AN EMPTY OR ABSENT SLOT IS A REFUSAL, NOT A SKIP: exit 4, the slot
#     and its credential named on the packet, and NOTHING pushed to any
#     target — a tick that quietly pushed the rest would read green while
#     no off-site copy of main was being kept.
#   Forgejo: the header file forge-converge.sh fills from the checkout's
#     own forge credential ($BOSS_FORGE_AUTH_HEADER_FILE), as for
#     protect-main.sh. Whether it may delete a push mirror is MEASURED by
#     the first delete — a 401/403 is named on the packet.
#
# EXIT
#   0  every declared branch is on its target at the forge's value (pushed
#      or already so), read back, and the forge carries no push mirror
#   1  the push was refused (non-fast-forward, named) or failed, a pushed
#      ref does not read back, the forge lists a mirror with no name, or a
#      mirror delete was refused or did not take — stderr says which, and
#      what was left as it was. A push-side failure still lets the mirror
#      go; the exit is the worse of the two halves (4 > 2 > 1).
#   2  the declaration is unreadable, names a branch that could force,
#      rename or match more than a branch pattern should, declares main on
#      no target or on two, lets main ride with another branch, or has two
#      targets share a remote or a token file — nothing was fetched or
#      pushed, and the Forgejo push mirror was still removed (a failure of
#      that removal is exit 1 or 4, as below, and the verdict still names
#      the refused declaration)
#   4  cannot answer: a credential slot empty, missing or loose, the forge
#      repository unreadable or its fetch failed — nothing was pushed, and
#      the Forgejo push mirror was still removed — or the forge header
#      missing or the forge API unreachable, where no mirror was removed.
#   Any other stop (a `set -u` abort, sor.sh refusing an unset
#   BOSS_FORGE_URL) still writes offsite_push, beginning REFUSED:, from
#   the EXIT trap: silence is the one outcome this never has.
#
# ENV
#   BOSS_OFFSITE_PUSH_DECL       the declaration (default beside this file)
#   BOSS_OFFSITE_TOKEN_OWNER_UID the uid that must own each token file (0
#                                — the seam the tests use, as they do not
#                                run as root)
#   BOSS_OFFSITE_STATE_DIR       the private clone's home (/var/lib/boss-offsite)
#   BOSS_FORGE_REPO_PATH …       see forge-repo-path.sh
#   BOSS_FORGE_URL               Forgejo's base (from /etc/boss/sor.env)
#   BOSS_FORGE_AUTH_HEADER_FILE  the forge header file (required)
#   BOSS_OFFSITE_CURL            the curl command — the test seam
#   BOSS_OFFSITE_GITHUB_BASE     a local stand-in for https://github.com/ —
#                                the test seam for the push and the read-
#                                back only; unset on the forge. A value
#                                that is not a local directory is exit 4,
#                                and a verdict made through it names it
#   GIT_*                        every one is UNSET before git runs; this
#                                script sets its own
#   BOSS_RUN_SUMMARY_FILE        offsite_push lands on the packet: for
#                                each target, its declared branches,
#                                what was pushed, and every ref read
#                                back, by name, targets joined by " | "
# Tested in crates/core/boss-testing/tests/offsite_push_sh.rs against real
# git repositories and a stub curl.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/run-summary.sh
. "$HERE/../run-summary.sh"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/forge/forge-repo-path.sh
. "$HERE/forge-repo-path.sh"

ME="offsite-push"
DECL="${BOSS_OFFSITE_PUSH_DECL:-$HERE/offsite-push.json}"
STATE_DIR="${BOSS_OFFSITE_STATE_DIR:-/var/lib/boss-offsite}"
CURL="${BOSS_OFFSITE_CURL:-curl}"
TOKEN_OWNER_UID="${BOSS_OFFSITE_TOKEN_OWNER_UID:-0}"
CLONE="$STATE_DIR/boss.git"
# A stall is a failure this script names, in seconds, rather than a hang
# the unit's ten-minute timeout kills before offsite_push is written
# (b176fd60 S4): below LOW_SPEED_LIMIT bytes/s for LOW_SPEED_TIME
# seconds, git gives up with its own words.
LOW_SPEED_LIMIT=1000
LOW_SPEED_TIME=60

say() { echo "$ME: $*" >&2; }
verdict() { run_summary_field offsite_push "$1"; }
# The one DR copy (backlog 761bc8a9, design 76155676): the only remote
# main may be declared for, normalised as the judgement below compares.
DR_REMOTE="https://github.com/algedonic-dev/boss-dr"

# WHAT WENT WRONG BEFORE THE MIRROR HALF, carried to it (F1). A refused
# declaration and a push-side failure each push nothing more and still
# let the mirror half run; `pending_note` holds their verdict words until
# `pushed` carries them, and every exit takes the worst code seen.
decl_refused=""
held=""
held_rc=0
pending_note=""
add_note() { pending_note="${pending_note:+$pending_note; and }$1"; }
# worst <rc> — the worse of <rc>, a held push failure, and a refused
# declaration (2): 4 > 2 > 1 > 0.
worst() {
    local w="$1"
    [ "$held_rc" -le "$w" ] || w="$held_rc"
    [ -z "$decl_refused" ] || [ "$w" -ge 2 ] || w=2
    echo "$w"
}
cannot_answer() {
    say "CANNOT ANSWER — $*"
    verdict "cannot answer: $*${pending_note:+; and $pending_note}"
    exit "$(worst 4)"
}
failed() {
    say "FAILED — $*"
    verdict "FAILED: $*${pending_note:+; and $pending_note}"
    exit "$(worst 1)"
}
# A REFUSED declaration pushes nothing and does NOT stop the run (the
# review of 590d3384): the mirror half below still removes any Forgejo
# push mirror, and the run then exits 2. The first refusal is recorded.
refuse_decl() {
    [ -z "$decl_refused" ] || return 0
    say "REFUSED — $DECL: $*; nothing is fetched or pushed, and the Forgejo push mirror is still removed"
    decl_refused="the declaration $DECL: $*, so nothing was fetched or pushed"
    add_note "REFUSED: $decl_refused"
}
# hold_push <rc> <verdict words> — a push-side failure (a slot, the
# stand-in, the forge repository, the fetch): nothing more is pushed, the
# mirror half still runs, and the run exits no better than <rc> (F1).
hold_push() {
    say "$2 — nothing more is pushed, and the Forgejo push mirror is still removed"
    held="${held:-$2}"
    [ "$1" -le "$held_rc" ] || held_rc="$1"
    add_note "$2"
}
# NO SILENT EXIT (F2). Whatever stops this script — a `set -u` abort, a
# refusal in a sourced helper — the packet gets an offsite_push verdict:
# if none was written, the trap writes one that begins REFUSED:.
on_exit() {
    local rc=$?
    [ -z "${WORK:-}" ] || rm -rf "$WORK"
    if [ -n "${BOSS_RUN_SUMMARY_FILE:-}" ] \
        && ! { jq_doc_file "$BOSS_RUN_SUMMARY_FILE" && jq -e 'has("offsite_push")' "$BOSS_RUN_SUMMARY_FILE" >/dev/null 2>&1; }; then
        run_summary_field offsite_push "REFUSED: offsite-push.sh stopped (exit $rc) before it wrote a verdict — its stderr on this run names where; nothing after that point ran"
    fi
}
WORK=""
trap on_exit EXIT

# --- the declaration -------------------------------------------------------
# A list of targets, each {remote, branches, credential, token_file}.
# Each branch is a plain name or a name ending in ONE `/*` — the only
# wildcard a refspec carries both sides of. No `+` (force), no `:`
# (rename or delete), nothing git would read as a second pattern. Every
# remote is a GitHub repository over https, `owner/name` and nothing
# more — no userinfo, no port, no query, no other host — so a declaration
# cannot aim a token anywhere the helper would not already refuse. The
# remote and the token file are written into the credential helper's
# text, so they are held to characters no shell reads as syntax. Every
# pattern ends at `\z`, never `$`: jq 1.6's `$` matches before a final
# newline, so "main\n" and "/etc/x\n" passed (review of fcf6042f, F2),
# and a value with a newline is two lines to anything that reads lines.
if ! jq_doc_file "$DECL" \
    || ! jq -e '
        type == "object"
        and (.targets | type == "array" and length > 0
             and all(type == "object"
                 and (.remote | type == "string" and test("^https://github\\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(\\.git)?\\z"))
                 and (.credential | type == "string" and test("^[a-z0-9-]+\\z"))
                 and (.token_file | type == "string" and test("^/[A-Za-z0-9._/-]+\\z"))
                 and (.branches | type == "array"
                      and all(type == "string"
                              and test("^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*(/\\*)?\\z")))))' \
        "$DECL" >/dev/null 2>&1; then
    refuse_decl "not an object with a non-empty list of targets, each naming a remote, a credential id, an absolute token file, and a list of plain branch names (a name, or a name ending in /*)"
fi
# Main has exactly one off-site home and rides there alone (761bc8a9):
# the fork is public, so the shapes that would put main back on it — the
# old single target carrying main and publish/*, or main added to the
# fork beside the DR copy — are refused here, before anything is read.
# So are two targets sharing a token file (each has its own credential
# slot) or a remote — compared NORMALISED, because GitHub answers
# Algedonic-Dev/Boss-DR, boss-dr and boss-dr.git as one repository, and a
# path with a doubled or trailing slash is the same file: two spellings of
# one slot or one remote are one slot or one remote. One judgement that
# must answer exactly `ok`: any other answer, silence included, is the
# refusal.
T_REMOTE=()
T_CRED=()
T_TOKEN=()
T_BRANCHES=()
ALL_BRANCHES=()
if [ -z "$decl_refused" ]; then
    why="$(jq -r --arg dr "$DR_REMOTE" '
        def norm: ascii_downcase | gsub("/+"; "/") | sub("/$"; "") | sub("\\.git$"; "");
        [.targets[] | select(any(.branches[]; . == "main"))] as $m
        | if ($m | length) != 1 then
            "main is declared on \($m | length) targets — it must be on exactly one target, the private DR copy (backlog 761bc8a9)"
          elif ($m[0].remote | norm) != ($dr | norm) then
            "main is declared for \($m[0].remote) — main goes to the private DR copy \($dr) and nowhere else (backlog 761bc8a9)"
          elif $m[0].branches != ["main"] then
            "the target carrying main (\($m[0].remote)) declares other branches too — main rides alone to the DR copy, so a remote that takes another branch is never where main goes (backlog 761bc8a9)"
          elif ([.targets[].token_file | norm] | length) != ([.targets[].token_file | norm] | unique | length) then
            "two targets name one token file — each off-site target has its own credential slot"
          elif ([.targets[].remote | norm] | length) != ([.targets[].remote | norm] | unique | length) then
            "two targets share a remote"
          else "ok" end' "$DECL" 2>&1)"
    [ "$why" = ok ] || refuse_decl "${why:-the declaration could not be judged}"
fi
# Each target is read as ONE record — one @tsv line, its branches last
# (the only field that may be empty) — so its remote, credential, slot
# and branches cannot shift against each other's lists (F2), and the
# record count must equal the target count.
if [ -z "$decl_refused" ]; then
    while IFS=$'\t' read -r r c t b; do
        T_REMOTE+=("$r")
        T_CRED+=("$c")
        T_TOKEN+=("$t")
        T_BRANCHES+=("$b")
    done < <(jq -r '.targets[] | [.remote, .credential, .token_file, (.branches | join(" "))] | @tsv' "$DECL")
    [ "${#T_REMOTE[@]}" = "$(jq -r '.targets | length' "$DECL")" ] \
        || refuse_decl "its targets did not read as one record each"
fi
if [ -z "$decl_refused" ]; then
    for tb_line in "${T_BRANCHES[@]}"; do
        for b in $tb_line; do
            case " ${ALL_BRANCHES[*]} " in *" $b "*) ;; *) ALL_BRANCHES+=("$b") ;; esac
        done
    done
else
    T_REMOTE=()
    T_CRED=()
    T_TOKEN=()
    T_BRANCHES=()
fi
fetch_specs=()
list_patterns=()
for b in "${ALL_BRANCHES[@]}"; do
    fetch_specs+=("+refs/heads/$b:refs/heads/$b")
    list_patterns+=("refs/heads/$b")
done

# --- the mirror half's own inputs: the only failures that keep the mirror --
AUTH="${BOSS_FORGE_AUTH_HEADER_FILE:-}"
if [ -z "$AUTH" ] || [ ! -s "$AUTH" ]; then
    cannot_answer "no forge credential: BOSS_FORGE_AUTH_HEADER_FILE names no non-empty header file — nothing was pushed and no mirror was removed"
fi
sor_require BOSS_FORGE_URL
API="${BOSS_FORGE_URL%/}/api/v1/repos/$FORGE_REPO_SLUG/push_mirrors"
WORK="$(mktemp -d -t offsite-push.XXXXXX)" || cannot_answer "mktemp failed"

# --- every push precondition, before anything is pushed ----------------------
# Every target's credential slot is checked before ANY target is pushed:
# an empty slot is a refusal, not a skip. A tick that pushed the fork and
# quietly passed over an empty DR slot would read green while no off-site
# copy of main was being kept (761bc8a9). A target that declares no
# branch is never pushed, so its slot is not read: a converge must not go
# red for a credential it does not use (backlog a2b58aab). A failure here
# pushes nothing and still lets the mirror half run (F1).
#
# The test seam first: it is a LOCAL stand-in and nothing else. Set on
# the live unit to a remote — an anonymous-write host, a lookalike of
# github.com — the push and the read-back would go there while every
# message and the verdict still named the declared GitHub remote: a
# packet reading "pushed main … read back" with no off-site copy kept
# (delta review of bf3ee4a3, 761bc8a9).
if [ -n "${BOSS_OFFSITE_GITHUB_BASE:-}" ] && [ ! -d "$BOSS_OFFSITE_GITHUB_BASE" ]; then
    hold_push 4 "cannot answer: BOSS_OFFSITE_GITHUB_BASE is set but is not a local directory — it is the test seam and nothing else; nothing was pushed to any target"
fi
for i in "${!T_REMOTE[@]}"; do
    [ -z "$held" ] || break
    [ -n "${T_BRANCHES[$i]}" ] || continue
    f="${T_TOKEN[$i]}"
    slot="the credential slot $f (${T_CRED[$i]}, for ${T_BRANCHES[$i]} to ${T_REMOTE[$i]})"
    hint="infra/forge/offsite-push.sh CREDENTIALS says what fills this slot and what the token must be allowed to do"
    mode="$(stat -c %a "$f" 2>/dev/null || echo '?')"
    # The mode says who may READ it; the owner says who could have
    # WRITTEN it. A file another account owns can be swapped for a token
    # of that account's choosing, and root would push with it (backlog
    # b176fd60 S5).
    owner="$(stat -c %u "$f" 2>/dev/null || echo '?')"
    if [ ! -e "$f" ]; then
        hold_push 4 "cannot answer: $slot is absent — an empty credential slot is a refusal, not a skip, so nothing was pushed to any target; $hint"
    elif [ ! -r "$f" ]; then
        hold_push 4 "cannot answer: $slot is unreadable — nothing was pushed to any target"
    elif [ ! -s "$f" ]; then
        hold_push 4 "cannot answer: $slot is empty — an empty credential slot is a refusal, not a skip, so nothing was pushed to any target; $hint"
    elif [ "$mode" != 600 ] && [ "$mode" != 400 ]; then
        hold_push 4 "cannot answer: $slot is mode $mode — a token file must be 0600 or 0400; nothing was pushed to any target"
    elif [ "$owner" != "$TOKEN_OWNER_UID" ]; then
        hold_push 4 "cannot answer: $slot is owned by uid $owner — it must be owned by uid $TOKEN_OWNER_UID (root); nothing was pushed to any target"
    fi
done
if [ -z "$held" ] && [ "${#ALL_BRANCHES[@]}" -gt 0 ] && { [ ! -d "$FORGE_REPO" ] || [ ! -r "$FORGE_REPO" ]; }; then
    hold_push 4 "cannot answer: the forge repository $FORGE_REPO is not a readable directory (path came from: $FORGE_REPO_FROM); nothing was pushed to any target"
fi

# The forge repository belongs to the Forgejo account and this runs as
# root, so git needs it exempted from the ownership check — through a
# config FILE, because a local fetch runs upload-pack inside the source
# and git clears `-c` crossing into it (publish-github-pr.sh measured
# this, 2026-09-11). The file is per-run, in a 0700 dir the trap removes.
printf '[safe]\n\tdirectory = %s\n\tdirectory = %s\n' "$FORGE_REPO" "$CLONE" >"$WORK/safe.gitconfig"
# No git setting reaches this run from its environment, before this
# script sets its own (delta review of bf3ee4a3). GIT_CONFIG_COUNT /
# GIT_CONFIG_KEY_n / GIT_CONFIG_PARAMETERS carry config past both the
# system and the global file — a url.*.pushInsteadOf that sends the push
# elsewhere, a core.hooksPath that runs a hook as root, an http.proxy
# with sslVerify off — and GIT_DIR, GIT_EXEC_PATH, GIT_SSH_COMMAND and
# GIT_ASKPASS each change what runs or where. The whole class goes.
while IFS= read -r v; do unset "$v"; done < <(compgen -e GIT_)
export GIT_CONFIG_GLOBAL="$WORK/safe.gitconfig"
# And no system config: a root unit reads /etc/gitconfig, where a
# `pushInsteadOf` would send this push somewhere the declaration does not
# name and a `core.hooksPath` would run a hook as root. The tests always
# set this; the script did not (b176fd60 S3).
export GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0
g() { git -C "$CLONE" "$@"; }

# push_targets — fetch the declared branches and push each target; sets
# `pushed` (the per-target words) and `refusal`, or holds a failure that
# stops it (F1). Run only on a declaration that stands with nothing held.
refusal=""
push_targets() {
    # --- read the forge -------------------------------------------------------
    if [ ! -d "$CLONE" ]; then
        mkdir -p "$STATE_DIR" && chmod 700 "$STATE_DIR" \
            && git init -q --bare "$CLONE" 2>"$WORK/err" \
            || { hold_push 4 "cannot answer: no private clone at $CLONE: $(tr '\n' ' ' <"$WORK/err"); nothing was pushed to any target"; return; }
    fi
    # Forced and pruned HERE only: the private clone is a copy of what the
    # forge holds now, so a rewound forge main is a rewound local main — and
    # the push below, which is neither, refuses it off-site.
    g fetch -q --prune "$FORGE_REPO" "${fetch_specs[@]}" 2>"$WORK/err" \
        || { hold_push 4 "cannot answer: fetching ${ALL_BRANCHES[*]} from $FORGE_REPO: $(head -c 300 "$WORK/err" | tr '\n' ' '); nothing was pushed to any target"; return; }
    # Only the DECLARED refs: a fetch prunes inside its own refspecs and
    # nowhere else, so a branch a past declaration fetched stays in the
    # clone, and a listing of every head would read it back and report a ref
    # this run never pushed (67931115).
    g for-each-ref --format='%(objectname) %(refname)' "${list_patterns[@]}" >"$WORK/local"
    [ -s "$WORK/local" ] || { hold_push 4 "cannot answer: the forge holds none of ${ALL_BRANCHES[*]}; nothing was pushed to any target"; return; }


    # --- push each target: plain, so git refuses what is not a fast-forward
    # helper_for <token file> <path> — sets HELPER, a credential helper that
    # reads the token file when git asks; the argv carries the file's path,
    # never its contents. It answers ONLY https://github.com and ONLY the one
    # repository path it was made for — git hands it protocol, host and (with
    # credential.useHttpPath) path on stdin — so a redirect, an insteadOf or
    # a changed declaration can never carry a token to another host
    # (b176fd60 S5), and the DR token and the fork's can never be handed to
    # each other's repository (761bc8a9).
    helper_for() {
        local tf="$1" hp="$2"
        HELPER="!f() { test \"\$1\" = get || exit 0; p=; h=; pa=; while IFS= read -r l && [ -n \"\$l\" ]; do case \"\$l\" in protocol=*) p=\${l#protocol=} ;; host=*) h=\${l#host=} ;; path=*) pa=\${l#path=} ;; esac; done; test \"\$p\" = https && test \"\$h\" = github.com && test \"\$pa\" = '$hp' || exit 0; echo username=x-access-token; echo \"password=\$(cat '$tf')\"; }; f"
    }
    SEGS=()
    any_refused=""
    any_unread=""
    unreached=""
    for i in "${!T_REMOTE[@]}"; do
        remote="${T_REMOTE[$i]}"
        # Where git goes for it. The test seam BOSS_OFFSITE_GITHUB_BASE maps
        # https://github.com/ to a local base; unset — as on the forge — it is
        # the declared remote itself. The helper, the verdict and every
        # message keep the DECLARED remote, and the helper answers only
        # github.com, so a seam aimed elsewhere carries no token.
        url="$remote"
        [ -z "${BOSS_OFFSITE_GITHUB_BASE:-}" ] \
            || url="${BOSS_OFFSITE_GITHUB_BASE%/}/${remote#https://github.com/}"
        # A target that declares nothing (the public fork, backlog a2b58aab)
        # is named and never pushed: an empty list must not reach `git push`.
        if [ -z "${T_BRANCHES[$i]}" ]; then
            seg="nothing declared for $remote, so nothing fetched or pushed to it — the public fork receives publish branches only from publish-github-pr.sh, after its secrets scan and passkey check (backlog a2b58aab)"
            echo "$ME: $seg"
            SEGS+=("$seg")
            continue
        fi
        read -r -a tb <<<"${T_BRANCHES[$i]}"
        # The forge's refs THIS target declares: a name exactly, a `name/*`
        # by prefix (a refspec's `*` spans slashes).
        : >"$WORK/local.$i"
        while read -r sha ref; do
            for b in "${tb[@]}"; do
                case "$b" in
                    */\*) [[ $ref == "refs/heads/${b%\*}"* ]] || continue ;;
                    *) [ "$ref" = "refs/heads/$b" ] || continue ;;
                esac
                echo "$sha $ref" >>"$WORK/local.$i"
                break
            done
        done <"$WORK/local"
        if [ ! -s "$WORK/local.$i" ]; then
            if [ "${T_BRANCHES[$i]}" = main ]; then
                say "FAILED — the forge holds no main to push to $remote"
                SEGS+=("main to $remote: the forge holds no main")
                unreached=1
            else
                SEGS+=("${tb[*]} to $remote: the forge holds none, nothing to push")
            fi
            continue
        fi
        specs=()
        for b in "${tb[@]}"; do
            specs+=("refs/heads/$b:refs/heads/$b")
        done
        helper_for "${T_TOKEN[$i]}" "${remote#https://github.com/}"
        net=(-c credential.helper= -c "credential.helper=$HELPER" -c credential.useHttpPath=true
             -c "http.lowSpeedLimit=$LOW_SPEED_LIMIT" -c "http.lowSpeedTime=$LOW_SPEED_TIME")
        g "${net[@]}" push --porcelain "$url" "${specs[@]}" \
            >"$WORK/push.out" 2>"$WORK/push.err"
        push_rc=$?
        # Porcelain: `<flag>\t<from>:<to>\t<summary>`; `!` is a rejection.
        rejected="$(awk -F'\t' '$1 == "!" { sub(/^[^:]*:/, "", $2); printf "%s %s; ", $2, $3 }' "$WORK/push.out")"
        rejected_refs="$(awk -F'\t' '$1 == "!" { sub(/^[^:]*:/, "", $2); print $2 }' "$WORK/push.out")"
        if [ -z "$rejected" ] && [ "$push_rc" -ne 0 ]; then
            words="pushing ${tb[*]} to $remote: git exit $push_rc (a transfer below $LOW_SPEED_LIMIT B/s for ${LOW_SPEED_TIME}s is given up here): $(head -c 300 "$WORK/push.err" | tr '\n' ' ')"
            say "FAILED — $words"
            SEGS+=("$words")
            unreached=1
            continue
        fi
        updated="$(awk -F'\t' '$1 == " " || $1 == "*" { sub(/:.*/, "", $2); sub(/^refs\/heads\//, "", $2); printf "%s ", $2 }' "$WORK/push.out")"

        # --- read back: an answer is not an effect ---------------------------
        # Read back even when a ref was refused: git updates the refs it accepts.
        if ! g "${net[@]}" ls-remote --heads "$url" >"$WORK/remote" 2>"$WORK/err"; then
            words="reading $remote back failed: $(head -c 300 "$WORK/err" | tr '\n' ' ')${rejected:+; and it refused: ${rejected%; }}"
            say "FAILED — $words"
            SEGS+=("$words")
            unreached=1
            continue
        fi
        missing=""
        back=""
        while read -r sha ref; do
            if grep -qxF "$(printf '%s\t%s' "$sha" "$ref")" "$WORK/remote"; then
                back="$back ${ref#refs/heads/}"
            elif ! grep -qxF "$ref" <<<"$rejected_refs"; then
                missing="$missing $ref"
            fi
        done <"$WORK/local.$i"
        # What this target did, by name — its declaration, each ref it moved,
        # and each ref it read back — so the converge's packet says which
        # branches reach which repository rather than only how many (67931115).
        did="up to date"
        [ -z "$updated" ] || did="pushed ${updated% }"
        seg="${tb[*]} to $remote: $did; read back at the forge's value:${back:- none}"
        # A record made through the stand-in says so, and so can never read
        # as a copy kept on GitHub.
        [ "$url" = "$remote" ] || seg="$seg (via stand-in $url)"
        echo "$ME: $seg"
        if [ -n "$rejected" ]; then
            say "REFUSED — $remote would not take: ${rejected%; }"
            say "nothing of it was overwritten: $remote keeps what it had. A non-fast-forward means the forge and the off-site copy disagree about history — read both before anything moves."
            seg="$remote would not take ${rejected%; }; $seg"
            any_refused=1
        fi
        if [ -n "$missing" ]; then
            say "FAILED —$missing do not read back on $remote at the forge's value"
            seg="${missing# } do not read back on $remote; $seg"
            any_unread=1
        fi
        SEGS+=("$seg")
    done
    pushed=""
    for s in "${SEGS[@]}"; do
        pushed="${pushed:+$pushed | }$s"
    done
    # The verdict's first word is the worst outcome of any target, so a
    # reader of the packet line sees it before any detail.
    head=""
    [ -z "$any_refused" ] || head="REFUSED"
    [ -z "$any_unread$unreached" ] || head="${head:+$head, }FAILED"
    pushed="${head:+$head: }$pushed"
    refusal="$any_refused$any_unread"

    # --- a target not reached still lets the mirror go (F1) ------------------
    # A push or read-back that FAILED is named, exit 1 at least, and the
    # mirror half still runs: that mirror force-pushes every forge branch,
    # main among them, to the public fork.
    if [ -n "$unreached" ]; then
        say "a target was not reached — nothing more is pushed, and the Forgejo push mirror is still removed"
        [ "$held_rc" -ge 1 ] || held_rc=1
    fi
}

[ -n "$decl_refused" ] || [ -n "$held" ] || push_targets
# A refused declaration or a held failure is the verdict's first words,
# so no reader of the success words can mistake this run for one.
if [ -n "$decl_refused" ] || [ -n "$held" ]; then
    pushed="$pending_note"
fi
pending_note=""

# finish <words> — the mirror half's outcome; exits with the worst of a
# held push failure (4 or 1), a refused declaration (2), and a ref above
# refused or not read back (1), else 0.
finish() {
    echo "$ME: $1"
    verdict "$pushed; $1"
    local r=0
    [ -z "$refusal" ] || r=1
    exit "$(worst "$r")"
}

# --- the Forgejo push mirror, removed on every run that reaches its API -----
# call <METHOD> <url> — sets CODE, body in $WORK/out.
call() {
    local rc=0
    : >"$WORK/out"
    CODE="$("$CURL" -sS -m 20 -o "$WORK/out" -w '%{http_code}' -H "@$AUTH" -X "$1" "$2" 2>"$WORK/err")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        CURL_WORDS="curl exit $rc: $(tr '\n' ' ' <"$WORK/err")"
        return 1
    fi
}
forge_words() { jq -r '.message // empty' "$WORK/out" 2>/dev/null | tr '\n' ' ' | cut -c1-200; }
# list_mirrors — the push mirrors' remote_names, one per line, or exit.
list_mirrors() {
    call GET "$API" || cannot_answer "$pushed; but listing the forge's push mirrors failed: $CURL_WORDS — none was removed"
    [ "$CODE" = 200 ] || cannot_answer "$pushed; but listing the forge's push mirrors answered HTTP $CODE ($(forge_words)) — none was removed"
    # jq_doc_file first: an empty 200 body must not read as "no mirrors".
    jq_doc_file "$WORK/out" && jq -e 'type == "array"' "$WORK/out" >/dev/null 2>&1 \
        || cannot_answer "$pushed; but the forge answered its push-mirror list with no list — none was removed"
    # A row with no remote_name is a mirror that is still THERE, and one
    # this cannot delete by name. `// empty` used to drop it and report
    # "no Forgejo push mirror" (b176fd60 S5).
    jq -e 'all(.[]; (.remote_name | type) == "string" and (.remote_name | length) > 0)' "$WORK/out" >/dev/null 2>&1 \
        || failed "$pushed; but the forge lists a push mirror with no remote_name (to $(jq -r '[.[] | select((.remote_name | type) != "string" or (.remote_name | length) == 0) | .remote_address // "?"] | join(", ")' "$WORK/out")) — it cannot be deleted by name and may still be pushing; none was removed"
    jq -r '.[].remote_name' "$WORK/out"
}

mirrors="$(list_mirrors)" || exit "$(worst $?)"
if [ -z "$mirrors" ]; then
    finish "the forge carries no push mirror"
fi
while read -r name; do
    case "$name" in
        ''|*[!A-Za-z0-9._-]*) failed "$pushed; but the forge lists a push mirror named '$name', which this will not put in a URL" ;;
    esac
    call DELETE "$API/$name" || failed "$pushed; but deleting push mirror $name failed: $CURL_WORDS"
    case "$CODE" in
        200|204) ;;
        *) failed "$pushed; but deleting push mirror $name answered HTTP $CODE ($(forge_words)) — a 401/403 means the converge's forge credential cannot administer the repository" ;;
    esac
done <<<"$mirrors"
left="$(list_mirrors)" || exit "$(worst $?)"
[ -z "$left" ] || failed "$pushed; deleted push mirror(s) $(echo "$mirrors" | paste -sd, -), but the forge still lists $(echo "$left" | paste -sd, -) — does not read back"
finish "removed Forgejo push mirror(s) $(echo "$mirrors" | paste -sd, -) — read back: none left"
