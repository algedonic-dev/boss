#!/usr/bin/env bash
#
# publish-github-pr — open the public mirror's pull request BY MACHINE.
#
# The machine half of publish-to-github v6 (design 7b59af2c, David
# 2026-09-08: the mirror at github.com/algedonic-dev/boss is "strictly a
# backup of source protocol"). When David signs the protocol's approve
# step, the dispatcher rule publish-github-pr-on-open-pr-ready files an
# ops-request for the forge host, and the root ops-runner runs THIS
# script with no arguments.
#
# FROM THE ORGANISATION, AS THE APP (backlog d2b7c947, David 2026-09-30:
# "Go with option 2, retire the fork"; "we have made a fundamental
# change to our posture and now are working professionally out of the
# algedonic-dev Github org via the Github App"). Until then this verb
# pushed to a public fork under David's personal GitHub account and
# opened the PR as that account, with a personal token David minted by
# hand. Now the branch lives
# IN the mirror repository and the PR is opened from it by the GitHub
# App's installation on algedonic-dev — GitHub names the App as the
# opener, which is the intended posture — and the snapshot's author and
# committer are the publisher the verb file declares. It:
#
#   1. finds the one open publish-to-github packet whose `open-pr` step
#      is ready (one mirror, one open packet — the daily rule's guard),
#      and reads its APPROVAL off the record: a passkey stamp bound to
#      the approve step's current shape, over the measured tree that
#      step names — refusing before anything is fetched when there is
#      none (backlog 02b65d81; "A run" 1a says exactly what is read);
#   2. fetches forge main from the Forgejo repository ON THIS HOST (a
#      path, no forge credential) and the mirror's main from GitHub
#      (anonymous; the repo is public), and refuses when the approved
#      commit is no longer on forge main's history;
#   3. builds the dated SNAPSHOT commit: `git commit-tree <the approved
#      commit's tree> -p <mirror main>` — the tree that was measured,
#      scanned and signed, never forge main's newer head. The mirror's
#      history is publish snapshots, not the forge's history — a merge of
#      forge main conflicts in hundreds of files (measured 553 on
#      2026-09-08), and a snapshot whose tree IS a forge commit's tree is
#      the honest backup;
#   4. renders the App installation token minted for THIS request, then
#      pushes the snapshot to the forge and to the mirror repository as
#      publish/<date>-<snapshot> — a branch of its own, never forced
#      (backlog 1f0aa60d) — opens the PR from that branch into the
#      mirror's main, and completes the packet's open-pr step with
#      pr_url and snapshot_commit;
#   5. closes each OLDER open publish PR from the mirror's own branches
#      as superseded by this one (backlog d4bfe548) — so there is only
#      ever one PR to merge — where "older" is read off the record: a
#      packet recorded that PR on that branch, from a source this run's
#      contains (backlog 1a2bcf11), and the older packet gets the keys
#      its `superseded` terminal reads (backlog 78f2fbda);
#   6. deletes, forge first and then the mirror, each publish branch a
#      packet recorded whose PR GitHub reads closed or merged (1a2bcf11);
#   7. revokes its token — the Secret's copy marked expired, the token
#      presented to DELETE /installation/token, a read proving it dead —
#      and records how that went on open-pr as `token_revoked`.
#
# ONE RUN AT A TIME: a publish, a --merge and a --measure hold
# $STATE_DIR/publish.lock (flock) from before the packet is read to exit.
#
# A PUBLISH RUN never touches the mirror's main; the only PRs it closes
# are its own older publish snapshots, and the only branches it deletes
# are theirs. MAIN IS WRITTEN BY --merge ALONE (below).
#
# --merge: THE APP MERGES WHAT THE PASSKEY APPROVED (backlog 602fe95f,
# David 2026-09-30: option (ii), "NOBODY pushes or merges main by hand").
# Until then the merge was David's, through GitHub's UI, as an admin
# bypassing a classic rule nothing could satisfy (it required `rust` and
# `web`, which no workflow produces), and a UI squash stamped a personal
# address on the public main (#248). Run by its own verb file
# (infra/ops/verbs/merge-publish-pr.json, the word FIXED in the argv) when
# the packet's `merge` step goes ready, it:
#
#   M1. finds the publish packet its REQUEST was filed for (the request's
#       `metadata.for_publish`, read off the request itself — never the
#       first open packet with a merge ready, since a superseding packet
#       stands open beside the one it supersedes; backlog 16a9c5ae),
#       requires its `merge` step ready, and
#       reads its approval exactly as a publish does (1a) — the passkey
#       signed the TREE, and the merge lands exactly that tree, so the
#       same signature authorises it and no second prompt is asked for —
#       re-running the scan over it (2b);
#   M2. reads what open-pr recorded and holds the snapshot to it: the
#       mirror's publish branch holds that commit, its tree is the
#       approved tree, its one parent is the mirror's main as it stands
#       NOW, and its author and committer are the declared publisher;
#   M3. asks GitHub, anonymously, for the PR (open, the mirror's own
#       branch into main, head = the snapshot, opened by the App — the
#       login and id open-pr recorded, as GitHub answered it with the
#       App's own token when the PR was opened; 4a'), for the
#       rules main carries — refusing when they name no required check,
#       because main's ruleset is what defines green, and when one is not
#       pinned to an App, because a check any App can post defines
#       nothing — and for the head's check-runs, refusing unless no run
#       of a required check is still going and its newest completed
#       `success`;
#   M4. renders this request's own App token and FAST-FORWARDS main to the
#       snapshot with a plain push, never forced. Why a fast-forward and
#       not a squash or a merge commit: the commit on main is then THE
#       approved commit, byte for byte, authored and committed as the
#       publisher; GitHub's merge API mints a new commit (the squash's or
#       the merge's) and stamps it with an identity the caller cannot set.
#       GitHub marks the PR merged when its head reaches the base;
#   M5. proves it — git ls-remote reads main at the snapshot, and GitHub,
#       asked with the token (never a cached anonymous answer), reads the
#       PR merged — revokes the token, writes `pr_state` onto the packet,
#       and completes `merge` with `merged_sha`.
#
# Every refusal happens before the push, names what failed and exits
# non-zero, and the answer rule troubles the step with it. A re-run after
# main already reads the snapshot pushes nothing and proves again.
#
# THE EXIT SAYS WHETHER MAIN MOVED (backlog 16a9c5ae, review 01561b13
# N2; review af3f2996 B1-B3), and it is decided by READING main, first:
# as soon as the packet is known, the mirror's main is fetched and asked
# whether it is, or contains, the snapshot open-pr recorded, before the
# approval or any branch is read; and from the first line of a --merge
# run an early EXIT trap holds any death to 4 (review 89d4d390 C1-C2).
# Exit 2 (refused) or 1 (failed) only after main was read NOT at the
# snapshot and before a push: nothing was merged, and the annotation the
# answer rule writes closes the packet at `merge-refused` — which frees
# the daily rule, whose next publish closes this PR as superseded.
# Exit 3, from the moment main reads the snapshot (found there, pushed,
# or a push that errored but landed — a failed push is read back before
# anything is said): main IS the approved commit and the record has not
# followed, so the packet stays open and troubled, and a merge-publish-pr
# request for it proves it again without pushing or needing the publish
# branch. Exit 4 before main was read: unknown, and left open. The EXIT
# trap holds any exit the run did not choose to its state's own, and
# every write to the system of record after the push is bounded.
#
# THE TOKEN (backlog d2b7c947). A one-hour GitHub App INSTALLATION token,
# minted by the credential broker when this request was filed — rule
# broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-
# filed, narrowed to the mirror repository and contents, pull_requests,
# workflows and metadata, keyed `publish-token-<ops-request id>` in
# Secret boss/github-app-algedonic-dev. This script renders THAT key
# (infra/forge/credential-render.sh --request $OPS_REQUEST_ID) into its
# own slot, checks the slot the way github-act.sh checks its own (a
# root-owned 0600 regular file, rendered for this request, its expiry
# live well past this verb's timeout), copies it into the runner's
# private tmpfs, hands it to git through a credential helper and to gh
# through GH_TOKEN on those child processes, never prints it, and
# revokes it when its GitHub work is done (7 above) or the run ends
# early (the EXIT trap). Nothing here reads a personal token: the
# personal-token path this replaced is gone from the verb.
#
# --check: validate inputs (tools, the publisher, the mint rule, forge
# repository, state dir) with no network and exit 0/1. What the gate
# lint runs. It holds no token. It also
# PERFORMS the forge fetch, into a throwaway repository it deletes,
# because that fetch's permission model differs from a direct read and a
# --check that does not run it passes where the publish fails — which is
# what happened on 2026-09-11 (--check ok at 13:41, publish FAILED at
# 13:43 on the same host, mirror 271 commits behind).
#
# Idempotent: the snapshot is a pure function of what it publishes (its
# tree and its dates are the APPROVED commit's, not forge main's head and
# not the clock — a train landing between two runs changes neither), so
# re-running for
# the same packet builds the SAME commit, re-pushes the same branch as a
# no-op, and reuses the PR already open for it. Nothing is ever forced:
# a branch that already holds a DIFFERENT commit is refused by git, never
# overwritten, and a branch is deleted only leased to the head its closed
# PR names (step 6). A verb killed mid-way leaves at worst a pushed
# publish branch in the mirror repository, and a token that dies within
# the hour it was minted for.
#
# Runs as root under the ops-runner with NO HOME: every path is explicit
# (state dir, GH_CONFIG_DIR) and nothing reads $HOME.
set -euo pipefail

# --MERGE DECIDES ITS EXIT BEFORE ANYTHING CAN FAIL (review 89d4d390 C1).
# For --merge an exit of 1 or 2 is the claim "main was read and nothing
# was merged" (below, at refuse/fail), and the lines from here to the
# argument parse can die — a sourced helper's exit 1, mktemp under a full
# /tmp, any `set -e` status — before finish() is installed. Measured rc=1
# with an unwritable TMPDIR and with a sor.env missing the mirror. So the
# mode is known now, and until finish() replaces it this trap holds any
# failing exit to 4: nothing has been read.
MODE=""
if [ "${1:-}" = --merge ]; then
    MODE=merge
    trap 'early_rc=$?; [ "$early_rc" -eq 0 ] || exit 4' EXIT
fi

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"
# The server's step shape hash — what a passkey stamp is bound to — in
# the one copy ops-runner.sh shares (backlog 02b65d81, "A run" 1a below).
# shellcheck source=infra/lib/step-shape.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/step-shape.sh"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
# THE TOKEN'S DECLARATION AND SLOT (backlog d2b7c947; header, THE TOKEN).
# The rule is named, not searched for: it shares its credential_id with
# the admin rules github-act.sh selects by, so a search by id would find
# theirs. The slot is this verb's own — never github-act.sh's
# algedonic-dev.token, which a per-request render would drop from under
# a GitHub act running at the same moment.
PUBLISH_RULE="$INFRA/dispatcher/rules/broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed.toml"
TOKEN_FILE="${BOSS_PUBLISH_TOKEN_FILE:-/etc/boss-publish/github-app/algedonic-dev-publish.token}"
TOKEN_OWNER_UID="${BOSS_PUBLISH_TOKEN_OWNER_UID:-0}"
RENDER="${BOSS_PUBLISH_RENDER:-$HERE/credential-render.sh}"
RENDER_SLEEP="${BOSS_PUBLISH_RENDER_SLEEP:-7}"
# The slot's expiry must outlast this verb: its ops timeout is 600 s
# (infra/ops/verbs/publish-github-pr.json), and a token that dies
# mid-run leaves a push or a close unproven. Fifteen minutes is that
# with margin; the broker mints a fresh hour at filing.
TOKEN_MARGIN_S=900
STATE_DIR="${BOSS_PUBLISH_STATE_DIR:-/var/lib/boss-publish}"
# WHERE THE FORGE REPOSITORY IS — derived, not asserted, by
# infra/forge/forge-repo-path.sh (sourced below the helpers), which
# reads BOSS_FORGE_REPO_PATH, BOSS_FORGE_COMPOSE, BOSS_FORGE_REPO_SLUG
# and BOSS_FORGE_DATA_FALLBACK.
# THE MIRROR — where it is spelled: infra/estate/estate.toml, rendered
# onto this host as /etc/boss/sor.env (infra/lib/sor.sh), never here.
# Both overrides are taken FIRST and the file is read only when one is
# missing: sourcing sor.sh with BOSS_SOR_ENV named REPLACES what the
# environment carried, and the tests point this verb at fixture
# repositories through exactly these two variables. The clone URL is the
# declared URL plus `.git` — the same string the literal built until
# 2026-09-20, when it was one of four copies owned by nothing (backlog
# f8af6040), one of them inside another script's refusal message.
_mirror_slug="${BOSS_MIRROR_SLUG:-}"
_mirror_url="${BOSS_MIRROR_URL:-}"
if [ -z "$_mirror_slug" ] || [ -z "$_mirror_url" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_MIRROR_SLUG BOSS_MIRROR_URL
fi
MIRROR_SLUG="${_mirror_slug:-$BOSS_MIRROR_SLUG}"
MIRROR_URL="${_mirror_url:-${BOSS_MIRROR_URL}.git}"
# THE BRANCH LIVES IN THE MIRROR ITSELF (backlog d2b7c947). Until
# 2026-09-30 it lived on a public fork under a personal account, because a
# personal token can open a PR only from a repository its user can push
# to; the fork is retired and David deletes it. The App's installation
# holds the mirror, so the head and the base are one repository — the
# owner and name every PR reader below compares against.
MIRROR_OWNER="${MIRROR_SLUG%%/*}"
MIRROR_NAME="${MIRROR_SLUG#*/}"
# THE FORGE PUSH (ce5339d6). The forge repository once carried a push
# mirror to the fork this verb opened PRs from — `git push --mirror` on
# every commit, which PRUNED any branch the forge lacked. PR #238 opened
# at 22:46:59Z on 2026-09-11 and GitHub closed it at 22:49:17Z, head
# deleted, two minutes and one train later; publish/2026-09-08 survived
# because it also existed on the forge. So the snapshot goes to the forge
# FIRST, under the same branch name. Since backlog 21d54f4a (2026-09-26)
# that push mirror is gone (infra/forge/offsite-push.sh replaced it), and
# since backlog a2b58aab nothing carries forge publish/* to GitHub: a
# forge branch NAME vouches for nothing — every holder of a forge write
# credential for user david can push refs/heads/publish/<anything> to the
# forge, which protects main alone — so this verb's secrets scan and
# passkey check are the only way anything reaches the public mirror, and
# it pushes its snapshot there ITSELF (step 4a), with a token only root
# on the forge host reads. The forge-first push stays, so the
# forge holds every snapshot it published. This verb runs as root under the ops-runner, and a root push
# into Forgejo's repository would leave root-owned objects the forge's
# own user cannot collect — so the push runs as the host user whose
# login shell carries the forge credential helper (the converge's own
# arrangement, forge-converge.sh), over Forgejo's HTTP, from a clone
# made readable to it. Empty BOSS_FORGE_PUSH_AS runs the push inline
# (the test harness, whose fixture repo the test's uid owns).
# The forge's clone URL: BOSS_FORGE_PUSH_URL, else the forge base from
# /etc/boss/sor.env (infra/lib/sor.sh) with the product repository's
# path — the same owner every image repo lives under.
# THE CREDENTIAL IS THE CONVERGE'S OWN. The push runs as $FORGE_PUSH_AS
# over Forgejo's HTTP. Measured 2026-09-19 04:55Z on ops-request 3d9d5f58
# (the second approved publish): that user then had NO credential helper
# — `could not read Username for 'http://10.20.0.15:3000'` — and the
# converge's credential rode the checkout's `forgejo` remote URL as
# userinfo, so the push target became that remote's URL, read AS THE
# OWNER. That shape is the one that leaked (design 1c90d183, backlog
# c4cbc6b5: a git error printed the URL into the forge-converge
# journal). Since then the credential is a 0600 file of the owner's
# behind a git credential helper in the owner's GLOBAL config, scoped to
# the forge's URL, and forge-converge's deposit (credential-deposit.sh)
# strips the remote's userinfo once that helper authenticates. So the
# URL read here carries no credential; the push authenticates because
# it runs as the owner, whose helper answers for the forge. The URL is
# still the remote's — the one the converge fetches through — and the
# bare sor.env URL is the fallback only when the checkout has no such
# remote. A remote that still carries userinfo (a host the deposit has
# not converted) is pushed to as it stands and redacted in every message.
FORGE_PUSH_AS="${BOSS_FORGE_PUSH_AS-david}"
FORGE_CHECKOUT="${BOSS_FORGE_CHECKOUT:-/home/david/boss}"
redact_url() { sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'; }
checkout_remote_url() {
    if [ -n "$FORGE_PUSH_AS" ]; then
        runuser -l "$FORGE_PUSH_AS" -c "git -C '$FORGE_CHECKOUT' remote get-url forgejo" 2>/dev/null
    else
        git -C "$FORGE_CHECKOUT" remote get-url forgejo 2>/dev/null
    fi
}
if [ -z "${BOSS_FORGE_PUSH_URL:-}" ]; then
    BOSS_FORGE_PUSH_URL="$(checkout_remote_url || true)"
fi
if [ -z "${BOSS_FORGE_PUSH_URL:-}" ]; then
    # shellcheck source=infra/lib/sor.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
    sor_require BOSS_FORGE_URL
    BOSS_FORGE_PUSH_URL="$BOSS_FORGE_URL/${BOSS_FORGE_OWNER:-david}/boss.git"
fi
FORGE_PUSH_URL="$BOSS_FORGE_PUSH_URL"
FORGE_PUSH_URL_SHOWN="$(printf '%s' "$FORGE_PUSH_URL" | redact_url)"
# THE PUBLISHER (backlog d2b7c947, David 2026-09-30: "We should also use
# david@algedonic.dev as the publisher"). Every snapshot
# is authored AND committed as the one `publisher` infra/ops/verbs/
# publish-github-pr.json declares — the verb's own file, reviewed with
# it beside its `approvers`, and deliberately no env knob: an identity a
# unit could change would be a second spelling. It was a personal
# GitHub handle and its no-reply address, retyped here as defaults,
# until then.
VERB_FILE="$INFRA/ops/verbs/publish-github-pr.json"
DATE="${BOSS_PUBLISH_DATE:-$(date -u +%Y-%m-%d)}"
# A publish branch's name — publish/<date>[-<snapshot>] — for every reader
# that must tell one from any other branch: the supersession sweep, the
# prune, and --merge, which merges only a PR from such a branch.
PUBLISH_BRANCH_RE='^publish/[0-9]{4}-[0-9]{2}-[0-9]{2}(-[0-9a-f]+)?$'
# BRANCH is publish/<date>-<snapshot>, named once the snapshot exists
# (step 3 below) — see there for why the date alone is not a name.
CLONE="${STATE_DIR}/boss.git"
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-${STATE_DIR}/gh}"
ACTOR="${BOSS_OPS_ACTOR:-automation:publish-github-pr}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

me="publish-github-pr"
say() { echo "$me: $*"; }
# Exit 2 is a refusal and exit 1 a failure. For --merge those two are a
# CLAIM that nothing was merged — the protocol's `merge-refused` terminal
# closes the packet on exactly them, and must never close one whose main
# moved (backlog 16a9c5ae; review af3f2996) — so --merge earns them by
# READING the mirror's main:
#   4  before main has been read against the packet's snapshot
#      (main_unread, from the argument on): nothing says it did not move;
#   2/1 once main was read NOT at the snapshot, and until a push
#      (main_still);
#   3  once main reads the snapshot — found there, pushed, or a push that
#      errored but landed (main_moved) — and from then on, whatever the
#      words. finish() holds every exit the run did not choose (a `set -e`
#      death) to the state's own, so only these four ever leave.
REFUSED_EXIT=2 FAILED_EXIT=1 MOVED_NOTE=""
# Once main moved, "nothing was merged" in a message written for the
# earlier state would be false: said as what it is instead.
# Before main was read (exit 4), it would be a claim nobody measured.
said_now() {
    local m="$*"
    if [ "$REFUSED_EXIT" = 3 ]; then
        m=${m//Nothing was merged/Nothing more was pushed}
        m=${m//nothing was merged/nothing more was pushed}
    elif [ "$REFUSED_EXIT" = 4 ]; then
        m=${m//Nothing was merged/Nothing was pushed by this run}
        m=${m//nothing was merged/nothing was pushed by this run}
    fi
    printf '%s' "$m"
}
refuse() { echo "$me: REFUSED — $(said_now "$*")$MOVED_NOTE" >&2; exit "$REFUSED_EXIT"; }
fail() { echo "$me: FAILED — $(said_now "$*")$MOVED_NOTE" >&2; exit "$FAILED_EXIT"; }
main_unread() {
    REFUSED_EXIT=4 FAILED_EXIT=4
    MOVED_NOTE=" (exit 4: the mirror's main was not read against the packet's snapshot, so whether it moved is unknown and the packet stays open — a merge-publish-pr request with for_publish=<its publish packet> reads main first and settles it)"
}
main_still() { REFUSED_EXIT=2 FAILED_EXIT=1 MOVED_NOTE=""; }
main_moved() {
    REFUSED_EXIT=3 FAILED_EXIT=3
    MOVED_NOTE=" (exit 3: $MIRROR_SLUG main reads the approved snapshot, so packet ${job_id:0:8} stays open until its record follows — a merge-publish-pr request with for_publish=$job_id pushes nothing and proves it again)"
}
# From here on refuse and fail give 4 too, until main is read (C1).
[ "$MODE" != merge ] || main_unread

# WHERE THE FORGE REPOSITORY IS — derived from the compose file that
# declares it, in the one definition every reader of the repository by
# path shares (moved out of this file 2026-09-26, backlog 21d54f4a,
# when offsite-push.sh became the second reader). Sets FORGE_REPO and
# FORGE_REPO_FROM; its header carries the history.
# shellcheck source=infra/forge/forge-repo-path.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/forge-repo-path.sh"

# EVERY READ OF THE FORGE REPOSITORY GOES THROUGH THIS ONE CHANNEL,
# --check and the run alike, so --check can never pass on a repository
# the run cannot read. The working directory is created HERE, above
# --check, because the channel is a file and this is where it lives.
workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}$MOVED_NOTE" >&2; exit "$FAILED_EXIT"; }
# finish — every end of a run that loaded its token revokes it (`revoke`,
# below the run's token section), in a subshell so a failure inside
# cannot skip the scratch's removal. The run's own verdict stands: a run
# that got as far as revoking on its happy path did so before open-pr
# completed, and records the outcome there; this is the early-end path,
# and what it could not prove it says.
REVOKE=0 SECRETS=""
finish() {
    local rc=$?
    if [ "$REVOKE" -eq 1 ]; then
        REVOKE=0
        (revoke) || say "the token this run held is NOT proven revoked (above) — it dies at its recorded expiry"
    fi
    [ -z "$SECRETS" ] || rm -rf "$SECRETS"
    rm -rf "$workdir"
    # --merge leaves only with an exit it chose (header of refuse/fail):
    # a death it did not choose takes the state's own failure exit, so a
    # `set -e` status past the push can never read "nothing merged"
    # (review af3f2996 B1).
    if [ "${MODE:-}" = merge ] && [ "$rc" -ne 0 ] \
            && [ "$rc" -ne "$REFUSED_EXIT" ] && [ "$rc" -ne "$FAILED_EXIT" ]; then
        rc=$FAILED_EXIT
    fi
    exit "$rc"
}
trap finish EXIT

# The machine token rides to curl in a 0600 file, never in its argv,
# where every local user reads it in ps (backlog 5f3ad356). Made here,
# in the script's own shell and after the trap above; publish-pr-state.sh
# reads the same MT_HDR.
# shellcheck source=infra/lib/secret-header.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/secret-header.sh"
machine_token_header MT_HDR "${BOSS_JOBS_URL:-}" \
    || { echo "$me: FAILED — the machine token's header file could not be written$MOVED_NOTE" >&2; exit "$FAILED_EXIT"; }

# safe.directory, scoped to this one path. The ops-runner executes verbs
# AS ROOT and this repository belongs to the Forgejo container's
# account, so since git 2.35.2 every command refuses it as "dubious
# ownership" — the same refusal, measured on this host class on
# ops-request c9877f75 (2026-09-10), that made delete-orphan-object's
# read impossible. That script drops to the owner instead; this one
# cannot, because the fetch's DESTINATION is root's own state dir under
# /var/lib, which the owner cannot write. The hazard the drop protects
# against — a root WRITE leaving root-owned objects in somebody else's
# repository — is not reachable here: a fetch only reads the source, and
# nothing in this script writes under $FORGE_REPO. That is also why the
# exemption names $FORGE_REPO exactly and never `*`: it is right for a
# READER and wrong where root writes, which is why
# infra/forge/delete-orphan-object.sh rejects it by name.
#
# WHY A FILE AND NOT `-c safe.directory=…`. Until 2026-09-11 it was the
# `-c` form, and `-c` CANNOT exempt a fetch SOURCE. A local fetch runs
# `git upload-pack` IN THE SOURCE REPOSITORY, and git clears the
# command-line config when it crosses into another repository — git's own
# trace says so, verbatim:
#
#   run_command: unset GIT_CONFIG_PARAMETERS … git-upload-pack '<src>'
#
# so that child runs its ownership check with no exemption at all. A
# DIRECT read in this process does honour `-c`, which is exactly how
# `--check` passed while the publish failed. Measured three ways on git
# 2.39.5, one destination exempted by a protected file so the only
# variable was how the SOURCE was exempted:
#
#   source via `-c` only (the old shape)  exit 128, dubious ownership
#   source via GIT_CONFIG_GLOBAL file     the fetch succeeds
#   source not exempted (control)         exit 128, dubious ownership
#
# GIT_CONFIG_GLOBAL is protected configuration AND is not in the set git
# clears crossing repositories, so the child inherits it. The file lives
# in $workdir — per-run, 0700, removed by the trap — never a fixed path
# under /tmp that another run or another user could have planted.
FORGE_SAFE_CONFIG="$workdir/forge-safe.gitconfig"
printf '[safe]\n\tdirectory = %s\n' "$FORGE_REPO" > "$FORGE_SAFE_CONFIG"
forge_git() { GIT_CONFIG_GLOBAL="$FORGE_SAFE_CONFIG" git "$@"; }

# The private bare clone both a publish and a --measure read through:
# defined here rather than in the run path below because --measure
# exits before it (one definition, not two — CLAUDE.md §9a).
g() { git -C "$CLONE" "$@"; }

# scan_commit <commit> — THE SECRETS SCAN, over a throwaway worktree of
# <commit> in the private clone, run as that tree's OWN
# infra/lint/no-secrets.sh (the lint that ships with what is scanned;
# BOSS_SECRETS_SCAN only for the test harness, whose trees hold none).
# Sets scan_verdict to `clean`, `FAILED` (the lint's redacting report in
# $workdir/scan) or `unrunnable` (why in $workdir/scan-why) — never
# `clean` without a scan having run, because no evidence is not a pass.
# One definition for the two readers: --measure's daily annotation, and
# a publish run, which scans the APPROVED commit itself rather than trust
# a measure step an agent transcribed (adversarial review of 02b65d81).
scan_commit() {
    local tree="$workdir/tree" scan
    scan_verdict=unrunnable
    : > "$workdir/scan"
    if ! g worktree add -q --detach "$tree" "$1" 2>"$workdir/err"; then
        printf 'a worktree of %s could not be created under %s — git said: %s' \
            "$1" "$workdir" "$(head -c 300 "$workdir/err" | tr '\n' ' ')" > "$workdir/scan-why"
        return 0
    fi
    scan="${BOSS_SECRETS_SCAN:-$tree/infra/lint/no-secrets.sh}"
    if [ ! -f "$scan" ]; then
        printf 'no secrets scan at %s' "$scan" > "$workdir/scan-why"
    elif ( cd "$tree" && bash "$scan" ) > "$workdir/scan" 2>&1; then
        scan_verdict=clean
    else
        scan_verdict=FAILED
    fi
    g worktree remove --force "$tree" 2>/dev/null || true
    g worktree prune 2>/dev/null || true
}
set_remote() { g remote get-url "$1" >/dev/null 2>&1 && g remote set-url "$1" "$2" || g remote add "$1" "$2"; }

# ONE RUN AT A TIME (backlog 1a2bcf11). A publish and a --measure share
# the private clone above, and two publishes both read the mirror's open
# PRs before either closes one — so an earlier-forked run could close the
# PR a later run had just opened. The one-open-packet guard is the DAILY
# rule's, and a superseding packet is filed beside the one it supersedes
# (246d597a beside 8d7a3507, 2026-09-27), so nothing else serialises
# them. The lock is taken before the system of record is read, so the
# second run sees what the first recorded. `take_publish_lock <on_fail>
# <var>` waits the seconds named by <var> and then calls <on_fail>,
# naming the file. fd 9 stays open until exit, so the lock is held for
# the whole run.
#
# THE TWO WAITS DIFFER ON PURPOSE (adversarial review of 8ec86b42). A
# publish WAITS — BOSS_PUBLISH_LOCK_WAIT, default 300 s, half the verb's
# 600 s timeout — because its open-pr has nobody to re-file it: a refused
# publish is David signing again by hand. A --measure YIELDS —
# BOSS_PUBLISH_MEASURE_LOCK_WAIT, default 30 s, then not yet (75) —
# because the refresh re-fires on its own cadence, and its verb's 900 s
# timeout must never be spent holding a queue a publish is waiting in.
LOCK_FILE="${STATE_DIR}/publish.lock"
BOSS_PUBLISH_LOCK_WAIT="${BOSS_PUBLISH_LOCK_WAIT:-300}"
BOSS_PUBLISH_MEASURE_LOCK_WAIT="${BOSS_PUBLISH_MEASURE_LOCK_WAIT:-30}"
take_publish_lock() {
    local on_fail="$1" wait_var="$2" wait
    wait="${!wait_var}"
    case "$wait" in ''|*[!0-9]*) "$on_fail" "$wait_var is '$wait', not a whole number of seconds" ;; esac
    command -v flock >/dev/null 2>&1 || "$on_fail" "flock is not on PATH, so this run cannot keep a second publish from overlapping it"
    exec 9>>"$LOCK_FILE" || "$on_fail" "the lock file $LOCK_FILE cannot be opened"
    flock -w "$wait" 9 \
        || "$on_fail" "another publish-github-pr run holds $LOCK_FILE and did not release it within ${wait}s ($wait_var); nothing was read, fetched or pushed"
}

# ---------------------------------------------------------------------
# Preconditions — the same list --check reports on.
# ---------------------------------------------------------------------
# publisher_problem — the verb file must declare the one identity every
# snapshot is authored and committed as (header, THE PUBLISHER): a name
# on one line and an address at the company's domain. Sets PUBLISHER_NAME
# and PUBLISHER_EMAIL when it says nothing.
PUBLISHER_NAME="" PUBLISHER_EMAIL=""
publisher_problem() {
    local who
    who=$(jq -r '.publisher // empty | select(type == "object")
        | [(.name // "" | tostring), (.email // "" | tostring)] | @tsv' "$VERB_FILE" 2>/dev/null) || who=""
    PUBLISHER_NAME="${who%%$'\t'*}"
    PUBLISHER_EMAIL="${who#*$'\t'}"
    [ "$who" != "$PUBLISHER_EMAIL" ] || PUBLISHER_EMAIL=""
    if [ -z "$PUBLISHER_NAME" ] || [ -z "$PUBLISHER_EMAIL" ]; then
        PUBLISHER_NAME="" PUBLISHER_EMAIL=""
        echo "$VERB_FILE declares no publisher {name, email}, so no snapshot has an author to carry; every publish is authored and committed as that one declaration"
    elif ! [[ $PUBLISHER_EMAIL =~ ^[A-Za-z0-9._%+-]+@algedonic\.dev$ ]] || [[ $PUBLISHER_NAME =~ [[:cntrl:]\<\>] ]]; then
        echo "$VERB_FILE declares publisher '$PUBLISHER_NAME' <$PUBLISHER_EMAIL>, which is not a name and a company address (…@algedonic.dev): the public mirror's history names who published it, never a personal address"
        PUBLISHER_NAME="" PUBLISHER_EMAIL=""
    fi
}

# mint_rule_problem — the rule the broker mints this verb's token under
# must be there and be the per-request kind credential-render.sh renders
# with --request; anything else is a verb with no way to a token.
mint_rule_problem() {
    if [ ! -r "$PUBLISH_RULE" ]; then
        echo "the broker rule that mints this verb's token is not readable at $PUBLISH_RULE — nothing would mint an installation token for a publish request"
    elif ! grep -qF 'handler = "credential.rotate.github-app-installation"' "$PUBLISH_RULE" \
        || ! grep -qE '^args = .*request_id = "id"' "$PUBLISH_RULE"; then
        echo "$PUBLISH_RULE is not a per-request GitHub App installation-token rule (its handler and request_id = \"id\"), so no token would be minted for the request this verb answers"
    elif [ ! -x "$RENDER" ]; then
        echo "the render $RENDER is not executable — the token the broker mints could not reach this host's slot"
    fi
}

# The deepest prefix of a path that exists — so "absent" can say how far
# the layout WAS right instead of only that the whole path was wrong.
deepest_existing() {
    local p="$1"
    while [ -n "$p" ] && [ "$p" != "/" ] && [ "$p" != "." ]; do
        if [ -e "$p" ]; then printf '%s' "$p"; return 0; fi
        p=$(dirname "$p")
    done
    printf '/'
}

# FOUR findings, four sentences. Until 2026-09-11 all four reported
# "forge repository not found at <path>" — the same words whether the
# path was absent, was a file, was unreadable by the caller, or was
# readable and refused by git. That is CLAUDE.md §Doors ("a wrong target
# answers instead of erroring") in its most literal form: the first
# reading of the live refusal could not tell a wrong path from a
# permission boundary, so the obvious next step was to guess a second
# path. The verb runs as root through the ops-runner, so "unreadable by
# root" and "unreadable by david" are different findings and the message
# names which user could not read it.
forge_repo_problem() {
    local who anc err
    who="$(id -un 2>/dev/null || id -u 2>/dev/null || printf '?')"
    if [ ! -e "$FORGE_REPO" ]; then
        anc=$(deepest_existing "$FORGE_REPO")
        if [ -r "$anc" ] && [ -x "$anc" ]; then
            echo "no forge repository at $FORGE_REPO — nothing exists there; the deepest path that does exist is $anc. Path came from: $FORGE_REPO_FROM"
        else
            echo "no forge repository at $FORGE_REPO — nothing exists there that this user ($who) can see, and $anc is not searchable by $who, so 'absent' here may mean 'hidden'. Path came from: $FORGE_REPO_FROM"
        fi
    elif [ ! -d "$FORGE_REPO" ]; then
        echo "the forge repository path $FORGE_REPO exists but is not a directory — a bare git repository was expected. Path came from: $FORGE_REPO_FROM"
    elif [ ! -r "$FORGE_REPO" ] || [ ! -x "$FORGE_REPO" ]; then
        echo "the forge repository $FORGE_REPO is a directory but is not readable by this user ($who) — a permission finding, not a wrong path. Path came from: $FORGE_REPO_FROM"
    elif ! err=$(forge_git -C "$FORGE_REPO" rev-parse --git-dir 2>&1 >/dev/null); then
        echo "the forge repository $FORGE_REPO is a readable directory but git refused it as a repository (read as $who). git said: ${err%%$'\n'*}"
    fi
}

# A FIFTH finding, and the one the four above could not reach: the path
# is a git repository this user can read, and the FETCH still fails.
# `git -C <repo> rev-parse` and `git fetch <repo>` do not share a
# permission model — the first reads in this process, the second spawns
# upload-pack inside <repo> — so a check that only does the first passes
# where the run fails. This performs the run's fetch, refspec and all,
# into a bare repository under $workdir that the trap deletes. Measured
# against this tree (188 MB git dir) it costs 0.65 s and 11 MB transient;
# the cost of NOT doing it was 271 commits.
forge_fetch_problem() {
    local probe="$workdir/forge-read-probe.git" err="$workdir/probe-err" who sha
    who="$(id -un 2>/dev/null || id -u 2>/dev/null || printf '?')"
    if ! git init -q --bare "$probe" 2>"$err"; then
        printf 'NOT ATTEMPTED — no probe repository under %s' "$workdir" > "$workdir/fetch-note"
        echo "the fetch probe repository could not be created under $workdir (as $who). git said: $(head -c 200 "$err" | tr '\n' ' ')"
        return 0
    fi
    if ! forge_git -C "$probe" fetch -q "$FORGE_REPO" "+refs/heads/main:refs/boss-publish/probe" 2>"$err"; then
        printf 'FAILED as %s — %s' "$who" "$(head -c 300 "$err" | tr '\n' ' ')" > "$workdir/fetch-note"
        echo "the forge repository $FORGE_REPO is a git repository $who can read, but the FETCH the publish performs fails from it — a different permission model, not a wrong path. git said: $(head -c 300 "$err" | tr '\n' ' '). Path came from: $FORGE_REPO_FROM"
        return 0
    fi
    sha=$(git -C "$probe" rev-parse refs/boss-publish/probe 2>/dev/null || printf '?')
    # Freed here, not only by the trap: the forge host has hit its disk
    # floor before (backlog 99696e43) and the probe's pack is the one
    # thing in this verb measured in megabytes.
    rm -rf "$probe"
    printf 'ok — refs/heads/main is %s, fetched as %s into a throwaway repository' "$sha" "$who" > "$workdir/fetch-note"
}

check_inputs() {
    local rc=0 problem
    for tool in git gh curl jq timeout; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "$me: missing tool on PATH: $tool" >&2; rc=1
        fi
    done
    # Not in a $(…) subshell: it sets the two names the snapshot reads.
    publisher_problem > "$workdir/publisher-problem"
    if [ -s "$workdir/publisher-problem" ]; then echo "$me: $(cat "$workdir/publisher-problem")" >&2; rc=1; fi
    problem=$(mint_rule_problem)
    if [ -n "$problem" ]; then echo "$me: $problem" >&2; rc=1; fi
    problem=$(forge_repo_problem)
    if [ -n "$problem" ]; then
        echo "$me: $problem" >&2; rc=1
    else
        # Only once the path IS a readable repository is the fetch a
        # question worth asking; before that it restates the finding above.
        problem=$(forge_fetch_problem)
        if [ -n "$problem" ]; then echo "$me: $problem" >&2; rc=1; fi
    fi
    if ! mkdir -p "$STATE_DIR" 2>/dev/null || [ ! -w "$STATE_DIR" ]; then
        echo "$me: state dir $STATE_DIR is not writable (BOSS_PUBLISH_STATE_DIR)" >&2; rc=1
    fi
    return $rc
}

if [ "${1:-}" = "--check" ]; then
    # No network, no push, no packet: does this host hold what a run
    # needs, and where does it read each thing from?
    echo "$me --check"
    echo "  forge repo : $FORGE_REPO"
    echo "               ($FORGE_REPO_FROM)"
    echo "  mirror     : $MIRROR_URL (fetched anonymously; publish/* pushed there, the PR opened from it)"
    # Named as NOT covered on purpose: --check holds no token and opens no
    # socket, so whether the broker can mint one — the App's permissions on
    # the installation — is known only when a run renders it.
    echo "  token      : minted per request by $(basename "$PUBLISH_RULE"), rendered to $TOKEN_FILE at run time — not here"
    if check_inputs; then rc=0; else rc=1; fi
    echo "  publisher  : ${PUBLISHER_NAME:-<none>} <${PUBLISHER_EMAIL:-none}> (from $VERB_FILE)"
    echo "  state dir  : $STATE_DIR"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    echo "  forge fetch: $(cat "$workdir/fetch-note" 2>/dev/null || printf 'NOT ATTEMPTED — the forge repository findings above say why')"
    if [ "$rc" -eq 0 ]; then
        echo "$me: --check ok"
        exit 0
    fi
    echo "$me: --check FAILED — see above" >&2
    exit 1
fi

# ---------------------------------------------------------------------
# --measure: RE-MEASURE the drift onto the OPEN publish packet.
#
# WHY (design cb38d806, backlog e1b6ddf7). `publish-to-github-daily`
# fires only on `NOT open_publish_exists("github-mirror")`, so one
# packet held at its approve sign-off retires the cadence for every day
# behind it — the fourth instance of a dedup guard silently retiring a
# cadence. The PII hold of 2026-09-17 -> 09-18 cost a week, and #239
# arrived as a 396-commit / 1319-file snapshot that no reader and no
# CodeQL run can read as a change. The answer decided in that design is
# that a hold must not cost the days behind it: the drift is measured
# again onto the packet already open, so the numbers the sign-off is
# given are today's.
#
# A MEASUREMENT, NOT A PUBLICATION. It makes the same two fetches a
# publish makes — the forge as a path on this host, the mirror
# anonymously — and stops there: no token is read, nothing is pushed,
# no pull request is opened. That is why its own verb file
# (infra/ops/verbs/mirror-drift.json) carries the word as a FIXED argv
# rather than a mode a packet selects: a rule can file the refresh
# without being able to file a publish.
#
# THE SECRETS SCAN RUNS OVER THE TREE THAT WOULD BE PUBLISHED — a
# throwaway worktree of forge main, whose own infra/lint/no-secrets.sh
# is the definition — never over this host's checkout, which is a
# different sha and belongs to another user. A scan that FINDS
# something is recorded as a finding for the reviewer (`FAILED`); a
# scan that cannot RUN answers `not yet` (75) and writes nothing, because
# no evidence is not a pass.
# ---------------------------------------------------------------------
if [ "${1:-}" = "--measure" ]; then
    not_yet() { echo "$me: not yet: $*" >&2; exit 75; }
    [ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
    BASE="${BOSS_JOBS_URL%/}"
    for tool in git curl jq; do
        command -v "$tool" >/dev/null 2>&1 || refuse "missing tool on PATH: $tool"
    done
    problem=$(forge_repo_problem)
    [ -z "$problem" ] || refuse "$problem"
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    [ -w "$STATE_DIR" ] || refuse "state dir $STATE_DIR is not writable (BOSS_PUBLISH_STATE_DIR)"
    # A publish in flight owns the clone; the refresh is not yet, not wrong.
    take_publish_lock not_yet BOSS_PUBLISH_MEASURE_LOCK_WAIT

    # 0. THE PULL REQUESTS' STATE, ASKED OF GITHUB (backlog a5d4322c).
    #    On 2026-09-22 the publish region called #239 open for 86 hours
    #    and itself TROUBLED over it; GitHub said #239 had merged three
    #    days earlier. Nothing in the pipeline had ever asked. It runs
    #    BEFORE the open-packet lookup below because a publish packet
    #    closes at judge-checks, long before its PR merges — the PRs to
    #    ask about are on closed packets. The pass itself lives in
    #    publish-pr-state.sh, one definition shared with
    #    read-publish-checks.sh, which asks it every fifteen minutes
    #    (backlog 663589cd: once a day left a closed PR alarming for 22h).
    GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"
    # shellcheck source=infra/forge/publish-pr-state.sh
    . "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/publish-pr-state.sh"
    publish_pr_states "publish-github-pr --measure"

    # 1. The packet. ANY open publish-to-github packet, at whatever step
    #    it is held — unlike a publish, which needs open-pr ready. One
    #    mirror, one open packet (the daily rule's guard), so the first
    #    open one is the one.
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20" > "$workdir/jobs" 2>"$workdir/err"; then
        fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
    fi
    job_id=$(jq -r '(if type == "object" and has("data") then .data else . end)
        | map(select(.status == "open")) | .[0].id // empty' "$workdir/jobs") \
        || fail "the jobs API answered something this verb cannot read as a packet list"
    if [ -z "$job_id" ]; then
        say "--measure: no open publish-to-github packet — nothing to refresh"
        exit 0
    fi

    # 2. The two refs, in the same private bare clone a publish uses.
    [ -d "$CLONE" ] || git init -q --bare "$CLONE"
    set_remote forge "$FORGE_REPO"
    set_remote mirror "$MIRROR_URL"
    forge_git -C "$CLONE" fetch -q forge "+refs/heads/main:refs/remotes/forge/main" 2>"$workdir/err" \
        || fail "fetching forge main from $FORGE_REPO ($FORGE_REPO_FROM) — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    g fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" 2>"$workdir/err" \
        || fail "fetching mirror main from $MIRROR_URL — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    forge_head=$(g rev-parse refs/remotes/forge/main)
    mirror_head=$(g rev-parse refs/remotes/mirror/main)
    ahead=$(g rev-list --count refs/remotes/mirror/main..refs/remotes/forge/main)
    behind=$(g rev-list --count refs/remotes/forge/main..refs/remotes/mirror/main)
    g diff --name-only refs/remotes/mirror/main refs/remotes/forge/main > "$workdir/changed"
    g diff --name-only --diff-filter=A refs/remotes/mirror/main refs/remotes/forge/main > "$workdir/new-files"
    files=$(grep -c . "$workdir/changed" || true)
    new_count=$(grep -c . "$workdir/new-files" || true)
    # The newly-public files a reviewer is asked to look at by name:
    # runbooks and infra topology describe how to reach and recover the
    # live system, which is a different disclosure from source code.
    grep -E '^(docs/runbooks/|infra/(cluster|caddy|forge)/)' "$workdir/new-files" > "$workdir/sensitive" || true
    sens_count=$(grep -c . "$workdir/sensitive" || true)

    # 3. The secrets scan, over the tree that would be published — the
    #    one scan_commit a publish run also performs over its approved sha.
    scan_commit refs/remotes/forge/main
    [ "$scan_verdict" != unrunnable ] \
        || not_yet "$(cat "$workdir/scan-why"), so what forge main ($forge_head) would publish was never scanned; the drift was NOT written to ${job_id:0:8}"
    secrets="$scan_verdict"

    # 4. The annotation. PATCH /api/jobs/{id}/metadata MERGES top-level
    #    keys, so the refresh lands beside everything the packet already
    #    carries; the job PUT would replace them.
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if [ "$ahead" -gt 0 ]; then has_drift="true"; else has_drift="false"; fi
    jq -n --arg ahead "$ahead" --arg behind "$behind" --arg files "$files" \
          --arg new_count "$new_count" --arg sens "$sens_count" --arg secrets "$secrets" \
          --arg has_drift "$has_drift" --arg ts "$ts" \
          --arg forge_head "$forge_head" --arg mirror_head "$mirror_head" \
          --rawfile sensitive "$workdir/sensitive" '
        {drift_refresh: {
            commits_ahead: $ahead, commits_behind: $behind, files_changed: $files,
            newly_public: $new_count, newly_public_sensitive: $sens,
            newly_public_review: ($sensitive | split("\n") | map(select(. != "")) | .[0:20]),
            secrets_scan: $secrets, has_drift: $has_drift,
            forge_head: $forge_head, mirror_head: $mirror_head,
            measured_at: $ts, measured_by: "publish-github-pr --measure"},
         drift_refreshed_at: $ts}' > "$workdir/refresh" \
        || fail "the refresh could not be rendered as JSON"
    if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$workdir/refresh" \
            "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err"; then
        fail "annotating ${job_id:0:8} with the refreshed drift failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    fi
    # An API's answer is not an API's effect — so READ THE PACKET, not
    # the write call's reply. This used to jq the PATCH's own response
    # body, and that door answers 204 with NO body: there was never
    # anything there to read (backlog b88a13d5). The result depended on
    # the host's jq. On jq-1.6 `jq -e` over an empty document exits 0,
    # so the check passed and verified nothing; elsewhere it exits
    # non-zero, so a daily rule reported FAILED on runs that had done
    # their work (ops-request 9340fd6e). A permanently-red check is one
    # nobody reads; a silently-vacuous one is worse.
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            "$BASE/api/jobs/$job_id" > "$workdir/readback" 2>"$workdir/err"; then
        fail "the refresh for ${job_id:0:8} was accepted but the packet could not be read back — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    fi
    # NO EVIDENCE IS NOT A PASS, and that has to be checked BEFORE the
    # comparison: an empty or unparseable read-back is what made this
    # check vacuous, and `jq -e` alone cannot tell it from a match.
    jq_doc_file "$workdir/readback" && jq -e 'type == "object"' "$workdir/readback" > /dev/null 2>&1 \
        || fail "the refresh for ${job_id:0:8} was accepted but the read-back of the packet answered nothing parseable — the measurement is not on the packet"
    # The line above has already refused a read-back holding no document,
    # so this compare cannot be handed silence.
    jq -e --arg ts "$ts" '((.data // .) | .metadata // {}) | .drift_refreshed_at == $ts' \
        "$workdir/readback" > /dev/null \
        || fail "the jobs API accepted the refresh for ${job_id:0:8} but the packet does not carry $ts — the measurement is not on the packet"

    say "--measure: refreshed ${job_id:0:8} — $ahead commit(s) ahead, $files file(s), $new_count newly public ($sens_count under runbooks/infra), secrets $secrets; forge ${forge_head:0:8} over mirror ${mirror_head:0:8} at $ts"
    exit 0
fi


# ---------------------------------------------------------------------
# A run: a publish (no argument), or --merge (header, M1-M5).
# ---------------------------------------------------------------------
# Any other word is refused, not read as a publish: the verb files admit
# `--check` or nothing (publish-github-pr.json) and `--merge` fixed
# (merge-publish-pr.json), so an unknown word is a hand run's typo.
case "${1:-}" in
    "") MODE=publish RUN_STEP=open-pr ;;
    --merge) MODE=merge RUN_STEP=merge; main_unread ;;
    *) refuse "unknown argument '${1:0:40}' — a run takes none (a publish), --merge, --measure or --check; nothing was read, fetched or pushed" ;;
esac
[ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
BASE="${BOSS_JOBS_URL%/}"
check_inputs || refuse "inputs incomplete (see above); nothing was fetched or pushed"
take_publish_lock refuse BOSS_PUBLISH_LOCK_WAIT

# 1. The packet. One mirror, one open publish packet (149's guard), and
#    the step this run is for — open-pr for a publish, merge for --merge —
#    must be ready or active: a rule fired on readiness.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20&full=true" > "$workdir/jobs" 2> "$workdir/err"; then
    fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
fi
# --merge acts on THE PACKET ITS REQUEST WAS FILED FOR, never the first
# one ready (backlog 16a9c5ae, review 01561b13 N3). A superseding packet
# is filed beside the one it supersedes, so two can stand open at once,
# and the answer rule writes this run's outcome onto the request's
# `for_publish` — which the filing rule (merge-publish-pr-on-merge-ready)
# sets to the packet whose merge went ready. Read off the request itself.
for_publish=""
if [ "$MODE" = merge ]; then
    [[ ${OPS_REQUEST_ID:-} =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "OPS_REQUEST_ID ('${OPS_REQUEST_ID:0:40}') names no ops-request, so which publish this merge was filed for cannot be read; nothing was merged"
    curl -fsS -H "x-boss-user: $BOSS_USER" "$BASE/api/jobs/$OPS_REQUEST_ID" > "$workdir/request" 2> "$workdir/err" \
        || fail "reading request ${OPS_REQUEST_ID:0:8} from $BASE failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was merged"
    for_publish=$(jq -r '(if type == "object" and has("data") then .data else . end)
        | .metadata.for_publish // empty | strings' "$workdir/request" 2>/dev/null) \
        || refuse "request ${OPS_REQUEST_ID:0:8} did not read as a packet — jq failed over $BASE's answer; nothing was merged"
    [[ $for_publish =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "request ${OPS_REQUEST_ID:0:8} names no publish packet (metadata.for_publish '${for_publish:0:40}'), so this run cannot know which PR it was filed to merge; nothing was merged"
fi
target=$(jq -c --arg slug "$RUN_STEP" --arg want "$for_publish" '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open" and ($want == "" or .id == $want)))
    | map({id, title, step: (((.steps // []) | map(select(.spec_slug == $slug)) | .[0])
                            // ((.steps // []) | map(select(.title == $slug)) | .[0]))})
    | map(select(.step != null and (.step.status == "ready" or .step.status == "active")))
    | .[0] // empty' "$workdir/jobs")
if [ -z "$target" ]; then
    [ -z "$for_publish" ] \
        || refuse "request ${OPS_REQUEST_ID:0:8} was filed for publish ${for_publish:0:8}, and no open publish-to-github packet of that id has its merge step ready; nothing was merged"
    say "no open publish-to-github packet has its $RUN_STEP step ready — nothing to do"
    exit 0
fi
job_id=$(printf '%s' "$target" | jq -r '.id')
step_id=$(printf '%s' "$target" | jq -r '.step.id')
say "packet ${job_id:0:8} — $RUN_STEP ready; reading its approval"

# --merge: HAS MAIN MOVED? Read FIRST, before anything else here can fail
# (review af3f2996 B2). The verb's exit is how the protocol tells "nothing
# was merged" (1, 2: merge-refused closes the packet) from "main moved"
# (3: it stays open), and a re-run over a merged main used to fail at the
# approval re-read, or at the fetch of a publish branch GitHub deletes on
# merge, with 1 or 2 — closing a merged packet "Not merged". So main is
# fetched (anonymously) and asked whether it IS or CONTAINS the snapshot
# open-pr recorded (review 89d4d390 C2), and the answer sets the exit
# every later line leaves with: 3 when it does, 2/1 only when the
# snapshot is provably not on main. A main that cannot be read leaves
# the exit at 4: unknown is never "not merged".
if [ "$MODE" = merge ]; then
    recorded_snap=$(jq -r --arg id "$job_id" '(if type == "object" and has("data") then .data else . end)
        | map(select(.id == $id)) | .[0].steps // []
        | map(select(.spec_slug == "open-pr")) | .[0].metadata.snapshot_commit // empty | strings' "$workdir/jobs") \
        || fail "open-pr's snapshot on ${job_id:0:8} could not be read — jq failed over the packet; nothing was merged"
    if [[ $recorded_snap =~ ^[0-9a-f]{40}$ ]]; then
        # EQUALITY IS NOT CONTAINMENT (review 89d4d390 C2): a main that
        # advanced PAST the snapshot — a later publish built on it, merged
        # — carries the approved commit as an ancestor. So main is FETCHED
        # into this verb's own clone (anonymously; the same incremental
        # fetch step 2 makes) and asked whether it contains the snapshot.
        # The snapshot's object is here from the run that built it, or it
        # arrives with main; absent after the fetch, main cannot contain it.
        mkdir -p "$STATE_DIR"
        [ -d "$CLONE" ] || git init -q --bare "$CLONE"
        set_remote mirror "$MIRROR_URL"
        # Anonymous and BOUNDED, like the read it replaced (review 20beab5a):
        # no credential and no prompt, and a hang ends here as a 4 rather
        # than as the runner's 124.
        GIT_TERMINAL_PROMPT=0 timeout "${BOSS_PUBLISH_MAIN_READ_MAX_S:-120}" \
            git -C "$CLONE" -c credential.helper= fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" 2>"$workdir/err" \
            || fail "reading $MIRROR_SLUG main to see whether it already carries $recorded_snap — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
        # A HISTORY THAT CAN LIE IS NO EVIDENCE (review 20beab5a C3). The
        # answers below are sound only over this clone's COMPLETE history:
        # a shallow boundary between main and the snapshot makes merge-base
        # answer "not an ancestor" and can leave the snapshot's object
        # absent, and an info/grafts file rewrites the ancestry it walks —
        # either would read a merged main as not merged. Nothing here makes
        # the clone shallow, so one that is was repaired by hand: 4, and
        # the packet stays open. Replace refs are ignored outright.
        clone_shallow=$(g rev-parse --is-shallow-repository) \
            || fail "asking whether this verb's clone $CLONE is shallow failed, so its history cannot be trusted to say whether main carries $recorded_snap"
        [ "$clone_shallow" = false ] \
            || fail "this verb's clone $CLONE is shallow (git rev-parse --is-shallow-repository: $clone_shallow), so its history cannot say whether $MIRROR_SLUG main carries $recorded_snap — a shallow boundary answers 'not an ancestor'. Unshallow or re-create it (git -C $CLONE fetch --unshallow mirror)"
        [ ! -e "$CLONE/info/grafts" ] \
            || fail "this verb's clone $CLONE carries $CLONE/info/grafts, which rewrites the ancestry git reads, so it cannot say whether $MIRROR_SLUG main carries $recorded_snap. Remove it"
        main_seen=$(g rev-parse refs/remotes/mirror/main)
        if [ "$main_seen" = "$recorded_snap" ]; then
            main_moved
            say "$MIRROR_SLUG main already reads the snapshot $recorded_snap — every exit from here is 3"
        elif ! g --no-replace-objects cat-file -e "${recorded_snap}^{commit}" 2>/dev/null; then
            main_still
        else
            contained=0
            g --no-replace-objects merge-base --is-ancestor "$recorded_snap" refs/remotes/mirror/main || contained=$?
            case "$contained" in
                0)
                    main_moved
                    MOVED_NOTE=" (exit 3: main carries the approved commit, so packet ${job_id:0:8} stays open and troubled rather than closing as not merged)"
                    refuse "$MIRROR_SLUG main is $main_seen, which contains the approved snapshot $recorded_snap as an ancestor: the merge happened and main has moved on past it. Proving a merge main has advanced past is not this verb's yet; a person closes the packet on that evidence"
                    ;;
                1) main_still ;;
                *) fail "asking whether $MIRROR_SLUG main $main_seen contains the snapshot $recorded_snap failed (git merge-base exit $contained)" ;;
            esac
        fi
    else
        # open-pr recorded no snapshot, so no run for this packet has had
        # anything to push: main cannot carry it.
        main_still
    fi
fi

# 1a. THE APPROVAL, READ OFF THE RECORD — never inferred from open-pr
#     being ready (backlog 02b65d81, David 2026-09-27: "Approved the
#     publish, but did not get a passkey check"). Measured on 8d7a3507:
#     approve completed with `sign_offs []`, and this verb, which asked
#     only whether open-pr was ready, pushed a 660-commit snapshot of
#     whatever forge main was at that moment to a PUBLIC repository.
#     Readiness is a flag; a flag is not a signature. publish-to-github
#     registry v6 (live 2026-09-27) makes approve a passkey sign-off over a step that NAMES the
#     measured tree (`source_sha`, and the secrets scan's `scanned_sha`,
#     written there by the measurement before the ceremony), so the
#     passkey — which binds step_shape_hash(title, metadata) — signs one
#     tree. This reads that, the ops-runner's way (verify_approval):
#
#       - approve COMPLETED with decision exactly "approved" (a Reject
#         runs the same ceremony and completes the same step);
#       - it declares presence and a required role, and the NEWEST stamp
#         for each role is presence-assured, carries a nonce, is bound to
#         the step's shape AS IT STANDS NOW and has not been voided — a
#         stamp dies when the content it signed leaves the step;
#       - it names a tree: 40-hex `source_sha` and `scanned_sha`, equal —
#         a scan of another commit vouches for nothing published here;
#       - the MEASURE step recorded the same two shas and a clean scan —
#         the approval is of what was measured, and a tree whose secrets
#         scan did not pass never leaves by machine, signed or not.
#
#     Every check is on server-minted fields of the packet, and every
#     failure refuses BEFORE any fetch, push or PR, naming what failed.
#     (Not here: the ops-runner's named-approver list and ten-minute TTL.
#     Those guard a verb that runs the moment it is signed; this one is
#     single-use by construction — open-pr completes once — and who may
#     stamp is the row's `sign_offs_required` plus `human_only`, which
#     the jobs API enforces at the ceremony.)
job_json=$(jq -c --arg id "$job_id" '(if type == "object" and has("data") then .data else . end)
    | map(select(.id == $id)) | .[0] // empty' "$workdir/jobs")
approve_json=$(printf '%s' "$job_json" | jq -c '((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) // empty')
[ -n "$approve_json" ] \
    || refuse "packet ${job_id:0:8} has no approve step, so nothing on it approved a publish; nothing was fetched or pushed"
approve_shape=$(shape_hash_of "$approve_json") \
    || refuse "the shape hash of ${job_id:0:8}'s approve step could not be computed, so no stamp on it can be verified; nothing was fetched or pushed"
# WHO MAY APPROVE IS A NAMED LIST, NEVER A ROLE (design 03451237 q2, as
# ops-runner's verify_approval reads it; adversarial review of 02b65d81,
# M1). The row's sign_offs_required names a ROLE, so any platform-admin
# with an enrolled passkey could stamp it; the claim this publish rests
# on is DAVID's passkey. The list is this verb's own file — `approvers`
# in infra/ops/verbs/publish-github-pr.json, reviewed with the verb — and
# deliberately no env knob: a list the unit could widen would be a
# bypass with a config file for a key. Unreadable or empty refuses.
approvers=$(jq -c '.approvers // empty
    | select(type == "array" and length > 0 and all(.[]; type == "string" and . != ""))' \
    "$VERB_FILE" 2>/dev/null) || approvers=""
[ -n "$approvers" ] \
    || refuse "$VERB_FILE names no approvers (a non-empty list of employee ids), so no passkey can approve a publish; nothing was fetched or pushed"
approval=$(printf '%s' "$job_json" | jq -c --arg shape "$approve_shape" --argjson approvers "$approvers" '
    def no(msg): {refuse: msg};
    def sha40: type == "string" and test("^[0-9a-f]{40}$");
    ((.steps // []) | map(select(.spec_slug == "approve")) | .[0]) as $a
    | ((.steps // []) | map(select(.spec_slug == "measure")) | .[0]) as $m
    | ($a.metadata // {}) as $am
    | (($m // {}).metadata // {}) as $mm
    | if $a.status != "completed" then
        no("its approve step is \($a.status // "absent"), not completed: open-pr being ready is a flag, not a signature")
      elif $am.decision != "approved" then
        no("its approve step records decision \($am | if has("decision") then .decision | tojson else "none" end), not \"approved\": a passkey that signed any other decision approved nothing")
      elif $a.assurance_required != "presence" then
        no("its approve step declares assurance \($a.assurance_required // "none"), not presence, so completing it proves no passkey signed the publish (a packet admitted before publish-to-github registry v6, 2026-09-27, was never signed)")
      elif (($a.sign_offs_required // []) | length) == 0 then
        no("its approve step requires no sign-off, so nothing on it records who approved the publish (8d7a3507: completed with sign_offs [])")
      elif (($am.source_sha | sha40) and ($am.scanned_sha | sha40)) | not then
        no("its approve step names no tree: source_sha \($am.source_sha // "none" | tostring) and scanned_sha \($am.scanned_sha // "none" | tostring) must be 40-hex shas inside the signed shape, or the passkey signed no particular tree")
      elif $am.scanned_sha != $am.source_sha then
        no("the secrets scan does not cover the published sha: the approve step signs source \($am.source_sha) over a scan of \($am.scanned_sha)")
      elif (($m // {}).status // "absent") != "completed" then
        no("its measure step is \(($m // {}).status // "absent"), so no measurement stands behind the approval")
      elif $mm.source_sha != $am.source_sha or $mm.scanned_sha != $am.scanned_sha then
        no("the approve step signs source \($am.source_sha) / scan \($am.scanned_sha), and the measure step recorded source \($mm.source_sha // "none" | tostring) / scan \($mm.scanned_sha // "none" | tostring): the approval is not of what was measured")
      elif $mm.secrets_scan != "clean" then
        no("the measure step recorded secrets scan \($mm.secrets_scan // "none" | tojson), not \"clean\": a tree whose scan did not pass never leaves by machine, signed or not")
      # EVERY LIVE STAMP IS BY A NAMED APPROVER. Not only the newest one
      # counted below: a live stamp by anyone else on the approval of a
      # public publish is a finding, not a bystander.
      elif ([($a.sign_offs // [])[] | select(.voided_at == null) | (.authority_id // "none" | tostring)]
            - $approvers | length) > 0 then
        no("its approve step carries a live sign-off by \([($a.sign_offs // [])[] | select(.voided_at == null) | (.authority_id // "none" | tostring)] - $approvers | unique | join(", ")), who is not among the approvers infra/ops/verbs/publish-github-pr.json names (\($approvers | join(", "))): who may approve a publish is a named list, never a role")
      else
        [ $a.sign_offs_required[] as $r
          | [($a.sign_offs // [])[] | select(.role == $r)] as $mine
          | if ($mine | length) == 0 then {err: "no \($r) sign-off is stamped on its approve step"}
            else $mine[-1] as $last
            | if $last.assurance != "presence"
                 or (($last.presence_nonce // "") | type) != "string" or ($last.presence_nonce // "") == "" then
                {err: "the newest \($r) sign-off is \($last.assurance // "session")-assured, not presence: no passkey signed it"}
              elif (($last.authority_id // "") | type) != "string" or ($last.authority_id // "") == "" then
                {err: "the newest \($r) sign-off names no authority, so nobody is on the record as approving"}
              elif ([$approvers[] | select(. == $last.authority_id)] | length) == 0 then
                {err: "the newest \($r) sign-off is by \($last.authority_id), who is not among the approvers infra/ops/verbs/publish-github-pr.json names (\($approvers | join(", "))): who may approve a publish is a named list, never a role"}
              elif $last.shape_hash != $shape then
                {err: "the newest \($r) sign-off is bound to shape \($last.shape_hash // "none"), and the approve step now hashes to \($shape): what is on the step is not what was signed"}
              elif $last.voided_at != null then
                {err: "the newest \($r) sign-off was voided at \($last.voided_at | tostring), when the content it signed left the approve step: a stamp dies with its shape and is not revived"}
              else {by: $last.authority_id, at: ($last.stamped_at // "" | tostring)} end
            end ] as $per
        | [$per[] | select(has("err")) | .err] as $errs
        | if ($errs | length) > 0 then no($errs | join("; "))
          else {source_sha: $am.source_sha,
                signed_by: ([$per[] | .by] | unique | join(", ")),
                signed_at: ([$per[] | .at] | max)} end
      end') \
    || refuse "the approval on ${job_id:0:8} could not be read — jq failed over the packet; nothing was fetched or pushed"
why=$(printf '%s' "$approval" | jq -r '.refuse // empty')
[ -z "$why" ] || refuse "packet ${job_id:0:8} is not approved for publication — $why. Nothing was fetched or pushed"
source_sha=$(printf '%s' "$approval" | jq -r '.source_sha')
approved_by=$(printf '%s' "$approval" | jq -r '.signed_by')
approved_at=$(printf '%s' "$approval" | jq -r '.signed_at')
# BRANCH does not exist yet — it is named by the snapshot (step 3), and
# this script runs under `set -u`, so this line spells the pattern.
if [ "$MODE" = merge ]; then
    say "approved by $approved_by with a passkey at $approved_at, over the measured tree $source_sha; merging the PR that carries it — the same signature, over the same tree"
else
    say "approved by $approved_by with a passkey at $approved_at, over the measured tree $source_sha; publishing it as publish/$DATE-<snapshot>"
fi

# THE TOKEN, as functions (backlog 602fe95f): a publish renders it at
# step 4, before its first push, and --merge at M4, before its push to
# main — one definition for both, called where each needs it.
#
# 4. THE TOKEN, before anything leaves (backlog d2b7c947; header, THE
#    TOKEN). A run that cannot hold this request's own live token refuses
#    here, having pushed nothing anywhere — the forge push below included,
#    so a missing credential leaves no half-published branch.
#
#    The request this run answers keys the token (the broker's key
#    publish-token-<request id>). It becomes part of a Secret key, so only
#    a packet id; the ops runner hands every verb the one it runs for.
slot_absent() { [ ! -e "$TOKEN_FILE" ] && [ ! -L "$TOKEN_FILE" ]; }
take_token() {
    REQUEST="${OPS_REQUEST_ID:-}"
    [[ $REQUEST =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "OPS_REQUEST_ID is '${REQUEST:0:80}', not the id of an ops-request — the ops runner hands every verb the packet it answers, and the publish token is minted for that request alone (publish-token-<request id>), so a run that cannot name its request has no token of its own; nothing was pushed"
    # THE SCRATCH lives in the runner's RuntimeDirectory — tmpfs under /run,
    # 0700, removed by systemd when the runner's run stops — exactly as
    # github-act.sh's does (backlog 5f77b205): a SIGKILL or a power loss must
    # not leave the token on disk. No fallback to /tmp.
    [ -n "${RUNTIME_DIRECTORY:-}" ] && [ -d "$RUNTIME_DIRECTORY" ] \
        || refuse "RUNTIME_DIRECTORY is not a directory ('${RUNTIME_DIRECTORY:-unset}') — boss-ops-runner.service's RuntimeDirectory= sets it, and the token is never written anywhere else; nothing was pushed"
    SECRETS="$(mktemp -d -p "$RUNTIME_DIRECTORY" publish-github-pr.XXXXXX)" \
        || fail "cannot make a private scratch directory under $RUNTIME_DIRECTORY; nothing was pushed"
    chmod 700 "$SECRETS" || fail "cannot make $SECRETS private; nothing was pushed"

    # render_slot — credential-render.sh leaves the slot holding this
    # request's live token or nothing (exit 1 is a fault it has named; the
    # slot it left says what stands). The broker mints within seconds of the
    # filing and the runner's pass can come sooner, so an absent slot is
    # rendered again, three tries, before it is a refusal.
    local try=1 rc slot_mode slot_uid held_for expires_s token
    while :; do
        rc=0
        "$RENDER" --rule "$PUBLISH_RULE" --dest "$TOKEN_FILE" --request "$REQUEST" || rc=$?
        case "$rc" in
            0 | 1) ;;
            *) refuse "rendering $TOKEN_FILE from $(basename "$PUBLISH_RULE") refused (exit $rc, above); nothing was pushed" ;;
        esac
        slot_absent || break
        [ "$try" -lt 3 ] || break
        say "no live token for request ${REQUEST:0:8} yet (try $try of 3) — the broker mints one when the request is filed; rendering again in ${RENDER_SLEEP}s"
        sleep "$RENDER_SLEEP"
        try=$((try + 1))
    done
    slot_absent \
        && refuse "no installation token for request ${REQUEST:0:8} at $TOKEN_FILE — the render (credential-render.sh, above) found no live one in the broker's Secret. The broker mints it when a publish-github-pr request is filed ($(basename "$PUBLISH_RULE")), from the App's installation on $MIRROR_OWNER; a mint the App's permissions cannot satisfy is refused on the dispatcher's firing record. Nothing here mints or places a credential, and nothing was pushed"
    [ ! -L "$TOKEN_FILE" ] && [ -f "$TOKEN_FILE" ] \
        || refuse "the token slot $TOKEN_FILE is not a regular file (a link is refused: it could point anywhere); nothing was pushed"
    slot_mode="$(stat -c %a "$TOKEN_FILE")" || refuse "cannot read the mode of $TOKEN_FILE; nothing was pushed"
    case "$slot_mode" in
        600 | 400) ;;
        *) refuse "the token slot $TOKEN_FILE is mode $slot_mode — a token file must be 0600 or 0400; nothing was pushed" ;;
    esac
    # The mode says who may READ it; the owner says who could have WRITTEN it.
    slot_uid="$(stat -c %u "$TOKEN_FILE")" || refuse "cannot read the owner of $TOKEN_FILE; nothing was pushed"
    [ "$slot_uid" = "$TOKEN_OWNER_UID" ] \
        || refuse "the token slot $TOKEN_FILE is owned by uid $slot_uid — it must be owned by uid $TOKEN_OWNER_UID (root); nothing was pushed"
    held_for=""
    if [ ! -L "$TOKEN_FILE.request" ] && [ -f "$TOKEN_FILE.request" ]; then
        IFS= read -r held_for <"$TOKEN_FILE.request" || true
    fi
    [ "$held_for" = "$REQUEST" ] \
        || refuse "the token slot $TOKEN_FILE was rendered for request '${held_for:-none recorded}', not $REQUEST — another request's token is never used; nothing was pushed"
    expires=""
    if [ ! -L "$TOKEN_FILE.expires-at" ] && [ -f "$TOKEN_FILE.expires-at" ]; then
        IFS= read -r expires <"$TOKEN_FILE.expires-at" || true
    fi
    expires_s="$(date -u -d "${expires:-no expiry}" +%s 2>/dev/null)" \
        || refuse "no readable expiry at $TOKEN_FILE.expires-at ('${expires:-none}') — the render writes one beside every token it places; a token without one is not the broker's. Nothing was pushed"
    [ "$expires_s" -gt $(($(date -u +%s) + TOKEN_MARGIN_S)) ] \
        || refuse "the token for request ${REQUEST:0:8} expires at $expires, within $((TOKEN_MARGIN_S / 60)) minutes — too close to outlive this verb; nothing was pushed"
    token=""
    IFS= read -r token <"$TOKEN_FILE" || [ -n "$token" ] || refuse "the token slot $TOKEN_FILE is empty; nothing was pushed"
    [ -n "$token" ] && [[ ! $token =~ [[:space:]] ]] || refuse "the token slot $TOKEN_FILE does not hold one token; nothing was pushed"
    # printf is a builtin: the value reaches the file without an argv. The
    # private copy is what git's helper, gh and the revoke read, so they act
    # with the token validated here even if the slot is replaced mid-run.
    (umask 077 && printf '%s\n' "$token" > "$SECRETS/token") || fail "cannot write the private token copy; nothing was pushed"
    token=""
    REVOKE=1
    say "token rendered for request ${REQUEST:0:8} (App installation on $MIRROR_OWNER, valid until $expires)"
    # The token reaches git through a credential helper that reads the
    # private copy, and gh through GH_TOKEN on that one process; neither is
    # ever echoed.
    helper="!f() { echo username=x-access-token; echo \"password=\$(cat '$SECRETS/token')\"; }; f"
}

# gh_t <gh args…> — gh, with the private copy as GH_TOKEN on that one
# process.
gh_t() { GH_TOKEN="$(cat "$SECRETS/token")" gh "$@"; }

# gh_status <gh api args…> — the HTTP status GitHub answered, read off
# the status line `gh api -i` prints first; empty when there was none.
gh_status() {
    gh_t api -i "$@" > "$workdir/gh-status" 2>&1 || true
    sed -n '1s#^HTTP/[0-9.]* \([0-9][0-9]*\).*#\1#p' "$workdir/gh-status"
}

# revoke — the run's GitHub work is over, and so is its token. The order
# is the broker's own and github-act.sh's: the Secret's copy is marked
# expired FIRST (credential-render.sh --expire), so a death between here
# and the revoke never leaves a dead token that reads as live; then the
# slot goes; then GitHub is asked to end the token, and only a read that
# answers 401 proves it did (a 403 or 404 means GitHub still accepted the
# credential). Sets TOKEN_REVOKED to what it can say; non-zero when any
# step is unproven.
TOKEN_REVOKED="not revoked: the run ended before its token was used"
revoke() {
    local ok=0 erc=0 code said=""
    "$RENDER" --rule "$PUBLISH_RULE" --dest "$TOKEN_FILE" --request "$REQUEST" --expire "$SECRETS/token" || erc=$?
    if [ "$erc" -ne 0 ]; then
        said="the broker's Secret was NOT marked expired (render exit $erc, above); "
        ok=1
    fi
    rm -f -- "$TOKEN_FILE" "$TOKEN_FILE.expires-at" "$TOKEN_FILE.request" || { said="${said}the slot $TOKEN_FILE could not be removed; "; ok=1; }
    code="$(gh_status --method DELETE installation/token)"
    case "${code:-none}" in
        204 | 401) ;;
        *) said="${said}DELETE /installation/token answered ${code:-no status}; "; ok=1 ;;
    esac
    code="$(gh_status "installation/repositories?per_page=1")"
    if [ "$code" = 401 ]; then
        TOKEN_REVOKED="${said}revoked — GET /installation/repositories with the run's token answers 401"
    else
        TOKEN_REVOKED="${said}NOT proven revoked — GET /installation/repositories with the run's token answered ${code:-no status}; it dies at $expires"
        ok=1
    fi
    say "token: $TOKEN_REVOKED"
    return "$ok"
}


# 2. Refs. A private bare clone under the state dir; the forge is read
#    as a path on this host, the mirror anonymously.
mkdir -p "$STATE_DIR" "$GH_CONFIG_DIR"
[ -d "$CLONE" ] || git init -q --bare "$CLONE"
set_remote forge "$FORGE_REPO"
set_remote mirror "$MIRROR_URL"
# A clone made before 2026-09-30 still names the retired fork; nothing
# here pushes to it again (backlog d2b7c947).
g remote remove fork 2>/dev/null || true
# Git's own words on both fetches: a verdict somebody must go re-derive
# is not a verdict (CLAUDE.md §Diagnosis).
forge_git -C "$CLONE" fetch -q forge "+refs/heads/main:refs/remotes/forge/main" 2>"$workdir/err" \
    || fail "fetching forge main from $FORGE_REPO ($FORGE_REPO_FROM) — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
g fetch -q mirror "+refs/heads/main:refs/remotes/mirror/main" 2>"$workdir/err" \
    || fail "fetching mirror main from $MIRROR_URL — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
forge_head=$(g rev-parse refs/remotes/forge/main)
mirror_head=$(g rev-parse refs/remotes/mirror/main)
mirror_tree=$(g rev-parse "refs/remotes/mirror/main^{tree}")

# 2a. THE SIGNED TREE, NOT FORGE MAIN'S HEAD (02b65d81). What leaves is
#     exactly the commit the measurement scanned, the review read and the
#     passkey signed — `source_sha` from 1a — never whatever forge main
#     has become since: trains land every forty minutes, and a head
#     nobody measured is a tree nobody reviewed. It must still be ON
#     forge main's history; a signed sha that is not (a rewritten main,
#     a sha from elsewhere) is a source that moved after measurement, and
#     is refused by name.
if ! g cat-file -e "${source_sha}^{commit}" 2>/dev/null \
        || ! g merge-base --is-ancestor "$source_sha" refs/remotes/forge/main 2>/dev/null; then
    refuse "the approved tree $source_sha is not on forge main (now $forge_head): the source moved after it was measured and signed, so what was approved cannot be published. File a fresh publish packet and approve its measurement; nothing was pushed"
fi
source_tree=$(g rev-parse "${source_sha}^{tree}")

# 2b. THE SCAN, RUN HERE, OVER WHAT LEAVES (adversarial review of
#     02b65d81, H1). The measure step's `secrets_scan: clean` is an
#     agent's transcription of a script's output and nothing signs it,
#     so it is a lead, not evidence. This run scans the approved commit
#     itself — the same scan_commit --measure uses — and refuses unless
#     it comes back clean. A scan that cannot run is a refusal too: no
#     evidence is not a pass. The report is the lint's own, which names
#     the file and redacts the value.
scan_commit "$source_sha"
case "$scan_verdict" in
    clean) say "secrets scan of the approved $source_sha: clean (run here, over the tree that would leave)" ;;
    FAILED) refuse "the secrets scan of the approved $source_sha FAILED when run here, over the tree that would be published — the measure step's word is not evidence, this is. It said: $(head -c 1500 "$workdir/scan" | tr '\n' ' '). Nothing was pushed" ;;
    *) refuse "the secrets scan of the approved $source_sha could not run — $(cat "$workdir/scan-why"). No evidence is not a pass; nothing was pushed" ;;
esac

if [ "$MODE" = merge ]; then
    # =================================================================
    # --merge (header, M1-M5; backlog 602fe95f). The approval (1a), the
    # approved tree on forge main (2a) and the scan over it (2b) are read
    # above exactly as a publish reads them. Nothing below pushes before
    # M4, and every refusal says nothing was merged.
    # =================================================================
    MERGE_READS="${BOSS_PUBLISH_MERGE_READS:-6}"
    # Every write to the system of record below runs after main moved, so
    # it is BOUNDED (review af3f2996 B1): one that hangs to the runner's
    # timeout is recorded as 124, an exit this verb never chose. Bounded,
    # it fails here, with exit 3 and its own words.
    SOR_WRITE_MAX_S="${BOSS_PUBLISH_SOR_WRITE_MAX_S:-60}"
    READBACK_SLEEP="${BOSS_PUBLISH_READBACK_SLEEP:-5}"
    GITHUB_API="${BOSS_GITHUB_API:-https://api.github.com}"

    # M2. WHAT OPEN-PR RECORDED — the one PR this verb may merge. It is
    #     the mirror's own publish branch, a pull request on the mirror,
    #     a 40-hex snapshot, and the SAME source the approval signs.
    recorded=$(printf '%s' "$job_json" | jq -c --arg slug "$MIRROR_SLUG" --arg owner "$MIRROR_OWNER" \
            --arg re "$PUBLISH_BRANCH_RE" --arg src "$source_sha" '
        def no(msg): {refuse: msg};
        ((.steps // []) | map(select(.spec_slug == "open-pr")) | .[0]) as $o
        | (($o // {}).metadata // {}) as $m
        | ("https://github.com/" + ($slug | ascii_downcase) + "/pull/") as $prefix
        | ($m.pr_url // "" | tostring) as $url
        | ($m.head // "" | tostring) as $head
        | ($head | split(":") | .[1:] | join(":")) as $branch
        | if (($o // {}).status // "absent") != "completed" then
            no("its open-pr step is \(($o // {}).status // "absent"), not completed, so no pull request is on the record to merge")
          elif (($url | ascii_downcase | startswith($prefix)) and ($url[($prefix | length):] | test("^[0-9]+$"))) | not then
            no("open-pr recorded pr_url \($url | tojson), which is not a pull request on \($slug)")
          elif (($m.snapshot_commit // "") | tostring | test("^[0-9a-f]{40}$")) | not then
            no("open-pr recorded snapshot_commit \($m.snapshot_commit // "none" | tostring), not a 40-hex sha")
          elif (($head | split(":") | .[0] | ascii_downcase) != ($owner | ascii_downcase)) or ($branch | test($re) | not) then
            no("open-pr recorded head \($head | tojson), which is not a publish/<date>-<snapshot> branch of \($slug) itself — this verb merges only the mirror'"'"'s own publish branches")
          elif ($m.source_sha // "") != $src then
            no("open-pr recorded source \($m.source_sha // "none" | tostring), and the approval signs \($src): the PR is not of the approved tree")
          elif ((($m.opened_by // "") | type) != "string") or ($m.opened_by // "") == ""
               or ((($m.opened_by_id // "") | tostring | test("^[0-9]+$")) | not) then
            no("open-pr recorded no App as the PR'"'"'s opener (opened_by \($m.opened_by // "none" | tostring), opened_by_id \($m.opened_by_id // "none" | tostring)) — the one identity this verb holds the PR to (backlog 16a9c5ae)")
          else {pr_url: $url, number: ($url[($prefix | length):] | tonumber),
                snapshot: $m.snapshot_commit, branch: $branch,
                opened_by: $m.opened_by, opened_by_id: ($m.opened_by_id | tostring)} end') \
        || refuse "what open-pr on ${job_id:0:8} recorded could not be read — jq failed over the packet; nothing was merged"
    why=$(printf '%s' "$recorded" | jq -r '.refuse // empty')
    [ -z "$why" ] || refuse "packet ${job_id:0:8} names no pull request this verb may merge — $why. Nothing was merged"
    pr_url=$(printf '%s' "$recorded" | jq -r '.pr_url')
    pr_n=$(printf '%s' "$recorded" | jq -r '.number')
    snapshot=$(printf '%s' "$recorded" | jq -r '.snapshot')
    pr_branch=$(printf '%s' "$recorded" | jq -r '.branch')
    opened_by=$(printf '%s' "$recorded" | jq -r '.opened_by')
    opened_by_id=$(printf '%s' "$recorded" | jq -r '.opened_by_id')

    #     The snapshot itself, read from the mirror anonymously and held
    #     to the approval: the branch holds it, its tree IS the approved
    #     tree, its one parent is the mirror's main as it stands now (so
    #     the push below is a fast-forward that carries the approved
    #     commit and nothing else), and it names the declared publisher.
    #     When main ALREADY reads the snapshot (a re-run), main itself holds
    #     it, and the branch is not fetched: GitHub may delete a head
    #     branch on merge, and a re-run that needed it could never prove a
    #     merge that happened (review af3f2996 B2).
    already=0
    if [ "$mirror_head" = "$snapshot" ]; then
        already=1
        main_moved
    else
        g fetch -q mirror "+refs/heads/$pr_branch:refs/boss-publish/merge-head" 2>"$workdir/err" \
            || fail "fetching the PR's branch $pr_branch from $MIRROR_URL — git said: $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was merged"
        held=$(g rev-parse refs/boss-publish/merge-head)
        [ "$held" = "$snapshot" ] \
            || refuse "the mirror's $pr_branch holds $held, and open-pr recorded $snapshot: the branch moved after the PR was opened, so what would merge is not what was published. Nothing was merged"
    fi
    snap_tree=$(g rev-parse "${snapshot}^{tree}")
    [ "$snap_tree" = "$source_tree" ] \
        || refuse "the snapshot $snapshot carries tree $snap_tree, and the approved commit $source_sha carries $source_tree: the PR is not of the tree the passkey signed. Nothing was merged"
    read -r -a snap_line <<<"$(g rev-list --parents -n 1 "$snapshot")"
    [ "${#snap_line[@]}" -eq 2 ] \
        || refuse "the snapshot $snapshot has $((${#snap_line[@]} - 1)) parents — a publish snapshot has exactly one, the mirror's main it was built on. Nothing was merged"
    snap_parent="${snap_line[1]}"
    ident=$(g log -1 --format='%an%x09%ae%x09%cn%x09%ce' "$snapshot")
    want_ident="$PUBLISHER_NAME"$'\t'"$PUBLISHER_EMAIL"$'\t'"$PUBLISHER_NAME"$'\t'"$PUBLISHER_EMAIL"
    [ "$ident" = "$want_ident" ] \
        || refuse "the snapshot $snapshot is authored and committed as '$(printf '%s' "$ident" | tr '\t' '/')', not the publisher $VERB_FILE declares ($PUBLISHER_NAME <$PUBLISHER_EMAIL>): the public main names who published it. Nothing was merged"
    if [ "$already" = 0 ] && [ "$snap_parent" != "$mirror_head" ]; then
        # What happens next is the protocol's, and it is said as it is
        # (backlog 16a9c5ae: this line claimed the supersession while the
        # open packet kept the daily rule from spawning the publish that
        # would do it).
        refuse "the mirror's main is $mirror_head now, and the snapshot $snapshot was built on $snap_parent: a fast-forward from here would not land the approved commit on the main it was built for. Nothing was merged. This refusal closes packet ${job_id:0:8} at merge-refused, and the next daily publish, built on main as it stands then, closes $pr_url as superseded"
    fi

    # M3. GITHUB, ASKED — anonymously: the mirror is public, and every
    #     answer here gates a push the git side re-checks (the push below
    #     is a plain fast-forward to this exact sha).
    gh_public() {
        curl -fsS -H "accept: application/vnd.github+json" "$GITHUB_API/repos/$MIRROR_SLUG/$1" > "$2" 2>"$workdir/err"
    }
    gh_public "pulls/$pr_n" "$workdir/pull" \
        || fail "GitHub did not answer for $pr_url — $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was merged"
    jq_doc_file "$workdir/pull" && jq -e 'type == "object"' "$workdir/pull" > /dev/null 2>&1 \
        || fail "GitHub answered $pr_url with nothing parseable; nothing was merged"
    pr_why=$(jq -r --arg slug "$MIRROR_SLUG" --arg b "$pr_branch" --arg snap "$snapshot" \
            --argjson n "$pr_n" --arg already "$already" \
            --arg by "$opened_by" --arg byid "$opened_by_id" '
        def lc: ascii_downcase;
        if .number != $n then "GitHub answered for #\(.number // "none" | tostring), not #\($n)"
        elif (.base.ref // "") != "main" then "its base is \(.base.ref // "none"), not main"
        elif ((.base.repo.full_name // "") | lc) != ($slug | lc) then "its base repository is \(.base.repo.full_name // "none"), not \($slug)"
        elif (.head.ref // "") != $b then "its head branch is \(.head.ref // "none"), not \($b)"
        elif ((.head.repo.full_name // "") | lc) != ($slug | lc) then "its head repository is \(.head.repo.full_name // "none"), not \($slug)"
        elif (.head.sha // "") != $snap then "its head is at \(.head.sha // "none"), not the approved snapshot \($snap)"
        elif (.user.type // "") != "Bot" then "it was opened by \(.user.login // "nobody") (\(.user.type // "no type")), not by the App: a pull request a person opened is never merged by machine"
        elif (.user.login // "") != $by or ((.user.id // "") | tostring) != $byid then "it was opened by \(.user.login // "nobody") (id \(.user.id // "none" | tostring)), and open-pr recorded the App that opened it as \($by) (id \($byid)): any other Bot is not this App"
        elif .state == "open" then empty
        elif $already == "1" and .merged == true then empty
        else "it reads \(.state // "no state"), \(if .merged == true then "merged" else "not merged" end)" end' \
            "$workdir/pull") \
        || fail "GitHub's answer for $pr_url could not be read as a pull request; nothing was merged"
    [ -z "$pr_why" ] || refuse "$pr_url is not the pull request the publish opened, open, on the approved snapshot — $pr_why. Nothing was merged"

    #     WHAT GREEN IS, read off main's own rules: the checks a ruleset
    #     requires there. None named is a refusal — main's ruleset
    #     (github-set-branch-protection, David's passkey) is what defines
    #     green, and backlog 602fe95f's order applies it before any merge.
    gh_public "rules/branches/main?per_page=100" "$workdir/rules" \
        || fail "GitHub did not answer for the rules on $MIRROR_SLUG main — $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was merged"
    jq_doc_file "$workdir/rules" && jq -e 'type == "array" and length < 100' "$workdir/rules" > /dev/null 2>&1 \
        || fail "the rules on $MIRROR_SLUG main did not read as one page of rules: $(head -c 200 "$workdir/rules" | tr '\n' ' '); nothing was merged"
    jq -c '[.[] | select(.type == "required_status_checks")
            | (.parameters.required_status_checks // [])[]
            | {context, integration_id: (.integration_id // null)}] | unique' "$workdir/rules" > "$workdir/required" \
        || fail "the rules on $MIRROR_SLUG main could not be read; nothing was merged"
    [ "$(jq 'length' "$workdir/required")" -gt 0 ] \
        || refuse "$MIRROR_SLUG main requires no status check (GET rules/branches/main names none): main's ruleset is what defines green, and until github-set-branch-protection has applied it nothing is merged by machine. Nothing was merged"
    #     A REQUIRED CHECK IS PINNED TO THE APP THAT RUNS IT (backlog
    #     16a9c5ae, review 01561b13 N1). Unpinned, a check run of that
    #     NAME from any App with checks:write satisfies it — measured on
    #     #248's head, CodeQL's App (57789) can post check runs here — so
    #     a success posted beside a real red Gate would merge. The ruleset
    #     pins one with github-set-branch-protection's `<name>@<App id>`
    #     (the mirror's Gate runs under GitHub Actions, App 15368).
    unpinned=$(jq -r '[.[] | select(.integration_id == null) | .context] | join(", ")' "$workdir/required") \
        || fail "the rules on $MIRROR_SLUG main could not be read; nothing was merged"
    [ -z "$unpinned" ] \
        || refuse "$MIRROR_SLUG main requires $unpinned from ANY App: a check run of that name posted by any App that can write checks would satisfy it, so it defines no green. Apply main's ruleset with the check pinned (github-set-branch-protection, checks <name>@<App id>). Nothing was merged"
    gh_public "commits/$snapshot/check-runs?per_page=100" "$workdir/runs" \
        || fail "GitHub did not answer for the check-runs on $snapshot — $(head -c 300 "$workdir/err" | tr '\n' ' '); nothing was merged"
    jq_doc_file "$workdir/runs" && jq -e '(.check_runs | type) == "array"' "$workdir/runs" > /dev/null 2>&1 \
        || fail "the check-runs on $snapshot did not read as a list; nothing was merged"
    # A limit is not a filter: a run on a second page could be the newer one.
    jq -e '(.total_count // 0) <= (.check_runs | length)' "$workdir/runs" > /dev/null \
        || refuse "$snapshot carries $(jq -r '.total_count' "$workdir/runs") check-runs and one page shows $(jq -r '.check_runs | length' "$workdir/runs"), so the newest run of a required check may be unread. Nothing was merged"
    #     THE NEWEST RUN, and none still going (review 01561b13 N1): a
    #     queued re-run has no started_at yet, and sorted by it an older
    #     success came LAST and won. So any run of a required check that
    #     has not completed is not green, and among the completed ones the
    #     newest — started last, the highest id breaking a tie — decides.
    verdicts=$(jq -c --slurpfile req "$workdir/required" '
        .check_runs as $runs
        | [ $req[0][] as $r
            | [$runs[] | select(.name == $r.context)
                       | select($r.integration_id == null or (.app.id // null) == $r.integration_id)] as $mine
            | ([$mine[] | select(.status != "completed")] | first) as $going
            | ($mine | sort_by(.started_at // "", .id) | last) as $c
            | if $c == null then
                {bad: "no check run named \($r.context | tojson)\(if $r.integration_id != null then " from App \($r.integration_id)" else "" end) on the head"}
              elif $going != null then {bad: "\($r.context) is \($going.status) (\($going.html_url // "no url"))"}
              elif $c.conclusion != "success" then {bad: "\($r.context) concluded \($c.conclusion // "nothing") (\($c.html_url // "no url"))"}
              else {ok: $r.context} end ]' "$workdir/runs") \
        || fail "the check-runs on $snapshot could not be judged; nothing was merged"
    bad=$(printf '%s' "$verdicts" | jq -r '[.[] | .bad // empty] | join("; ")')
    [ -z "$bad" ] || refuse "not every check $MIRROR_SLUG main requires is green on $snapshot — $bad. Nothing was merged"
    green=$(printf '%s' "$verdicts" | jq -c '[.[] | .ok]')
    say "every check main requires is green on ${snapshot:0:12}: $(printf '%s' "$green" | jq -r 'join(", ")')"

    # M4. THE MERGE: a fast-forward of main to the approved snapshot, as
    #     the App, with this request's own token. Plain, never forced: git
    #     refuses anything but a fast-forward, and GitHub refuses the App
    #     while any rule it is not exempt from stands on main.
    take_token
    if [ "$already" = 1 ]; then
        say "the mirror's main already reads $snapshot — nothing to push; proving it"
    else
        if g -c credential.helper= -c "credential.helper=$helper" push -q mirror "$snapshot:refs/heads/main" 2>"$workdir/err"; then
            main_moved
            say "pushed $snapshot to $MIRROR_SLUG main — a fast-forward from $mirror_head"
        else
            # A FAILED PUSH IS A CLAIM TOO (review af3f2996 B3): the
            # connection can drop after the ref moved and before git reads
            # the report. So main is read before anything is said about it.
            push_said=$(head -c 300 "$workdir/err" | tr '\n' ' ')
            if ! g -c credential.helper= ls-remote mirror refs/heads/main > "$workdir/main-after-push" 2>"$workdir/err"; then
                main_unread
                fail "pushing the approved snapshot $snapshot to $MIRROR_SLUG main answered an error (git said: $push_said), and main could not be read back — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
            fi
            if [ "$(awk '$2 == "refs/heads/main" { print $1 }' "$workdir/main-after-push")" = "$snapshot" ]; then
                main_moved
                say "the push answered with an error (git said: $push_said), and $MIRROR_SLUG main reads $snapshot: it landed — proving it"
            else
                fail "pushing the approved snapshot $snapshot to $MIRROR_SLUG main (a fast-forward from $mirror_head, never forced) — git said: $push_said. main is NOT merged: read back, it is not the snapshot. GitHub refuses the App a push to main while a rule it is not exempt from stands there: main's ruleset must name the App among its bypass actors, and no classic rule may stand beside it (github-set-branch-protection removes it)"
            fi
        fi
    fi

    # M5. AN ANSWER IS NOT AN EFFECT: main is read back with git, and the
    #     PR with the token — an authenticated read, never an anonymous
    #     answer a cache may have held since before the push.
    g -c credential.helper= ls-remote mirror refs/heads/main > "$workdir/main-now" 2>"$workdir/err" \
        || fail "the push answered, and $MIRROR_SLUG main could not be read back — git said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    main_now=$(awk '$2 == "refs/heads/main" { print $1 }' "$workdir/main-now")
    [ "$main_now" = "$snapshot" ] \
        || fail "$MIRROR_SLUG main reads ${main_now:-nothing} after the push, not the approved $snapshot — read it before anything else is done"
    reads=0
    until gh_t api "repos/$MIRROR_SLUG/pulls/$pr_n" > "$workdir/pull-after" 2>"$workdir/err" \
            && jq_doc_file "$workdir/pull-after" \
            && jq -e --arg snap "$snapshot" '.state == "closed" and .merged == true and (.head.sha // "") == $snap' \
                "$workdir/pull-after" > /dev/null 2>&1; do
        reads=$((reads + 1))
        [ "$reads" -lt "$MERGE_READS" ] \
            || fail "$MIRROR_SLUG main reads the approved $snapshot, and after $reads reads GitHub still answers $pr_url as $(jq -r '"\(.state // "no state"), merged \(.merged // false)"' "$workdir/pull-after" 2>/dev/null || printf 'nothing readable (%s)' "$(head -c 200 "$workdir/err" | tr '\n' ' ')") — main IS the approved commit; the PR's state has not followed it, so the merge is not recorded on ${job_id:0:8}"
        sleep "$READBACK_SLEEP"
    done
    merged_at=$(jq -r '.merged_at // empty' "$workdir/pull-after")
    say "read back: $MIRROR_SLUG main is $snapshot, and GitHub reads $pr_url merged${merged_at:+ at $merged_at}"

    REVOKE=0
    revoke || say "WARNING — the token this run used is not proven revoked; token_revoked on merge says what stands"

    #     The record: pr_state on the packet — the key and shape
    #     publish-pr-state.sh writes and the publish region reads — then
    #     the step's keys through the merge door, then its status.
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    jq -c --arg url "$pr_url" --arg ts "$ts" \
        '{pr_state: {pr_url: $url, number, state, merged: (.merged == true), merged_at, closed_at,
                     read_at: $ts, read_by: "publish-github-pr --merge"}}' "$workdir/pull-after" > "$workdir/pr-state" \
        || fail "main reads $snapshot and $pr_url is merged, but its state could not be rendered for ${job_id:0:8}"
    curl -fsS --max-time "$SOR_WRITE_MAX_S" -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} --data-binary @"$workdir/pr-state" \
            "$BASE/api/jobs/$job_id/metadata" > /dev/null 2>"$workdir/err" \
        || fail "main reads $snapshot and $pr_url is merged, but writing pr_state onto ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    jq -n -c --arg sha "$snapshot" --arg url "$pr_url" --arg was "$mirror_head" --argjson green "$green" \
            --arg src "$source_sha" --arg by "$approved_by" --arg at "$approved_at" \
            --arg revoked "$TOKEN_REVOKED" --arg merged_at "$merged_at" --arg already "$already" '
        {merged_sha: $sha, pr_url: $url, main_was: $was, method: "fast-forward",
         merged_by: "publish-github-pr --merge", merged_at: $merged_at,
         pushed: ($already != "1"), required_checks_green: $green,
         source_sha: $src, approved_by: $by, approved_at: $at, token_revoked: $revoked}' \
        > "$workdir/payload"
    printf '%s\n' '{"status":"completed"}' > "$workdir/done"
    curl -fsS --max-time "$SOR_WRITE_MAX_S" -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} --data-binary @"$workdir/payload" \
            "$BASE/api/jobs/$job_id/steps/$step_id/metadata" > /dev/null 2>"$workdir/err" \
        || fail "main reads $snapshot and $pr_url is merged, but recording it on merge (${job_id:0:8}) failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    curl -fsS --max-time "$SOR_WRITE_MAX_S" -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} --data-binary @"$workdir/done" \
            "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err" \
        || fail "main reads $snapshot and $pr_url is merged and recorded on merge, but completing merge on ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    # The answer line: the effect this verb's file declares, and what the
    # answer rule copies merged_sha from. Last, and only after the proof.
    say "merged $pr_url — main reads $snapshot"
    exit 0
fi

if [ "$source_tree" = "$mirror_tree" ]; then
    fail "the mirror's main already carries the approved tree ($source_sha) — nothing to publish; open-pr on ${job_id:0:8} stays open for a person to close or supersede"
fi

# 3. The snapshot commit. Tree = the approved commit's tree, parent =
#    the mirror's main. Authored AND committed as the declared publisher
#    (header, THE PUBLISHER); the approved sha rides in the body.
#
#    ITS OWN BRANCH (backlog 1f0aa60d). Until 2026-09-27 the branch was
#    publish/<date> and both pushes were --force, so a second publish on
#    one day landed on the first one's branch: ops-request 9084d6cd
#    (publish 8d7a3507, superseding 1fcbefde) moved open PR #245's head
#    from d459c67a to its own 28554177, then `gh pr create` refused
#    because #245 existed, and the verb exited 1 — having already changed
#    a pull request another packet's record described. So the branch is
#    publish/<date>-<snapshot>: one snapshot, one branch, one PR, one
#    packet. The older PR is then closed by the supersession sweep (4b)
#    like any other, and every push below is a plain push that git
#    refuses rather than let it move a branch to an unrelated commit.
#
#    THE DATES ARE THE APPROVED COMMIT'S, not the clock's and not forge
#    main's head, so the commit — and with it the branch name — is a
#    pure function of what is published: the approved commit, the
#    mirror's main, the packet, the publisher and the day. Nothing that
#    moves when a train lands enters it: not forge main's time, and not
#    forge main's sha in the message (it is said on the run's own output
#    instead). A re-run for the same packet (its own completion failed
#    after the PR opened) rebuilds the identical commit however many
#    trains have landed since, its push is a no-op, and the reuse lookup
#    finds its PR instead of opening a second one.
source_time=$(g log -1 --format=%ct "$source_sha") \
    || fail "reading the approved commit's time ($source_sha)"
snapshot=$(GIT_AUTHOR_NAME="$PUBLISHER_NAME" GIT_AUTHOR_EMAIL="$PUBLISHER_EMAIL" \
           GIT_COMMITTER_NAME="$PUBLISHER_NAME" GIT_COMMITTER_EMAIL="$PUBLISHER_EMAIL" \
           GIT_AUTHOR_DATE="@$source_time +0000" GIT_COMMITTER_DATE="@$source_time +0000" \
           g commit-tree "$source_tree" -p "$mirror_head" \
             -m "publish: $DATE" \
             -m "Snapshot of forge main at $source_sha onto the public mirror — the commit the publish packet measured, scanned and a passkey approved. The mirror is a backup of source: each publish is one commit whose tree is that commit's tree (a merge of the forge history conflicts in hundreds of files). Published by $PUBLISHER_NAME <$PUBLISHER_EMAIL>; opened by machine (BOSS publish-to-github, ops verb publish-github-pr) from packet $job_id; merged into main by the BOSS GitHub App once every check main requires is green.") \
    || fail "git commit-tree"
BRANCH="publish/${DATE}-${snapshot:0:12}"
say "snapshot $snapshot (tree $source_tree of the approved $source_sha, parent mirror $mirror_head; forge main is $forge_head; published by $PUBLISHER_NAME <$PUBLISHER_EMAIL>) — branch $BRANCH"

# 4. THE TOKEN, before anything leaves (take_token, above step 2): a
#    run that cannot hold this request's own live token refuses here,
#    having pushed nothing anywhere — the forge push below included.
take_token

# 4a. The forge first — see FORGE_PUSH_URL. NEVER FORCED (1f0aa60d):
#     the branch is this snapshot's own, so the push either creates it,
#     finds it already at this commit (a re-run: a no-op), or is REFUSED
#     because it holds something else — the one case that would have
#     moved another PR's head. A failure here opens nothing on GitHub.
# Readable to the pushing user: the bare clone only (public source),
# never the state dir's other contents — traversable, not listable.
chmod a+x "$STATE_DIR" 2>/dev/null || true
chmod -R a+rX "$CLONE" 2>/dev/null || true
# The push runs as $FORGE_PUSH_AS over a clone ROOT owns, and git ≥ 2.35.2
# refuses that as "dubious ownership" unless the PUSHING user's config
# exempts it. $FORGE_SAFE_CONFIG above is root's file in a 0700 workdir —
# `runuser -l` neither carries GIT_CONFIG_GLOBAL nor could that user read
# it. Measured 2026-09-18 23:05Z on ops-request c98a782f, the FIRST
# approved publish: `fatal: detected dubious ownership in repository at
# '/var/lib/boss-publish/boss.git'`, and the step sat ready five hours.
# `-c` is right here where the file was right above: a push reads the
# clone in THIS process (pack-objects stays in the same repository, so
# GIT_CONFIG_PARAMETERS survives); the fetch-source caveat does not apply.
forge_push_cmd="git -c 'safe.directory=$CLONE' -C '$CLONE' push -q '$FORGE_PUSH_URL' '$snapshot:refs/heads/$BRANCH'"
if [ -n "$FORGE_PUSH_AS" ]; then
    runuser -l "$FORGE_PUSH_AS" -c "$forge_push_cmd" 2>"$workdir/err" \
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN) as $FORGE_PUSH_AS: $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' '). Nothing was pushed to $MIRROR_SLUG: the forge holds every snapshot this verb publishes"
    say "pushed publish/${BRANCH#publish/} to the forge as $FORGE_PUSH_AS ($FORGE_PUSH_URL_SHOWN) — $MIRROR_SLUG gets it from this verb alone"
else
    bash -c "$forge_push_cmd" 2>"$workdir/err" \
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN): $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' '). Nothing was pushed to $MIRROR_SLUG"
    say "pushed publish/${BRANCH#publish/} to the forge ($FORGE_PUSH_URL_SHOWN) — $MIRROR_SLUG gets it from this verb alone"
fi

# The mirror repository itself, as the App. Never forced — the same three
# outcomes as the forge push above.
g -c "credential.helper=$helper" push -q mirror "$snapshot:refs/heads/$BRANCH" 2>"$workdir/err" \
    || fail "pushing $BRANCH to $MIRROR_URL (not forced: the branch is this snapshot's own, so a refusal means it already holds another commit, which is never overwritten): $(head -c 300 "$workdir/err" | tr '\n' ' ')"
say "pushed $MIRROR_OWNER:$BRANCH to $MIRROR_SLUG"

# THE MIRROR'S OPEN PULL REQUESTS, read once: the reuse question below
# and the supersession sweep (4b) both answer from this listing.
#
# The reuse question used to be `gh pr list --head "<owner>:<branch>"`,
# and gh's own manual says of --head: 'Filter by head branch
# ("<owner>:<branch>" syntax not supported)'. It compares the whole
# string to the bare branch name, so it matched NOTHING, ever — measured
# 2026-09-27, when PR #245 stood open on <fork>:publish/2026-09-27 and the
# lookup answered empty (backlog 1f0aa60d). The listing gh does answer
# carries the head's owner and branch as separate fields, and both are
# compared here. A limit is not a filter: a listing that comes back FULL
# may hide the PR asked about, so it is refused rather than read as
# absence.
#
# FROM THE MIRROR means the owner AND the repository (1a2bcf11, finding
# 3): gh gives the head's owner as `headRepositoryOwner` and its
# repository as `headRepository` ({id, name}); a PR from another
# repository of the same owner — or from any fork, the retired personal
# one among them — is not a branch of the mirror's own. `from_mirror` is that
# one test, for every reader of this listing below.
PR_LIST_LIMIT=200
list_open_prs() {
    gh_t pr list --repo "$MIRROR_SLUG" --state open --limit "$PR_LIST_LIMIT" \
        --json number,url,headRefName,headRepositoryOwner,headRepository > "$1" 2>"$workdir/err" \
        || { printf 'gh said: %s' "$(head -c 300 "$workdir/err" | tr '\n' ' ')" > "$workdir/list-why"; return 1; }
    jq_doc_file "$1" && jq -e 'type == "array"' "$1" > /dev/null 2>&1 \
        || { printf 'gh answered with no list' > "$workdir/list-why"; return 1; }
    [ "$(jq 'length' "$1")" -lt "$PR_LIST_LIMIT" ] \
        || { printf 'the mirror has %s or more open pull requests, so the listing may not hold every one' "$PR_LIST_LIMIT" > "$workdir/list-why"; return 1; }
}
FROM_MIRROR='def from_mirror($owner; $repo):
    ((.headRepositoryOwner.login // "") | ascii_downcase) == ($owner | ascii_downcase)
    and ((.headRepository.name // "") | ascii_downcase) == ($repo | ascii_downcase);'
list_open_prs "$workdir/open-prs" \
    || fail "listing the mirror's open pull requests failed — $(cat "$workdir/list-why"); nothing was read, so no PR was opened, reused or closed; $MIRROR_OWNER:$BRANCH is pushed (snapshot $snapshot), open-pr on ${job_id:0:8} stays ready and a re-run pushes the same commit"
pr_url=$(jq -r --arg owner "$MIRROR_OWNER" --arg repo "$MIRROR_NAME" --arg branch "$BRANCH" "$FROM_MIRROR"'
    first(.[] | select(from_mirror($owner; $repo))
              | select((.headRefName // "") == $branch) | .url) // empty' "$workdir/open-prs") \
    || fail "the open-PR listing could not be read as pull requests"
if [ -n "$pr_url" ]; then
    say "PR already open for $MIRROR_OWNER:$BRANCH — reusing $pr_url at $snapshot"
else
    # The head is the BARE branch: head and base are one repository, and
    # `<owner>:<branch>` is the spelling for a head in ANOTHER repository
    # of the network — which is exactly what this verb no longer opens.
    pr_url=$(gh_t pr create --repo "$MIRROR_SLUG" --base main --head "$BRANCH" \
        --title "publish: $DATE" \
        --body "Backup-of-source publish from the internal forge: snapshot \`$snapshot\` carries forge main at \`$source_sha\` — the commit measured, secrets-scanned and approved with a passkey — as one commit on top of the mirror's \`$mirror_head\`. Published by $PUBLISHER_NAME <$PUBLISHER_EMAIL>; approved with a passkey by $approved_by; opened by machine as the BOSS GitHub App (publish-to-github, packet $job_id), from the branch \`$BRANCH\` of this repository. The same App merges it — a fast-forward of main to exactly this snapshot — once every check main requires is green; nobody merges main by hand." \
        2>"$workdir/err") \
        || fail "gh pr create for $MIRROR_OWNER:$BRANCH — $MIRROR_OWNER:$BRANCH is pushed (snapshot $snapshot) and no PR carries it; gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    say "opened $pr_url at $snapshot"
fi

# 4a'. WHO OPENED IT, as GitHub answers with this run's own token (backlog
#      16a9c5ae, review 01561b13 N4). The PR was opened with the App's
#      installation token, so its author IS the App's bot account — the
#      one identity the tree cannot spell in advance (the App is David's,
#      and nothing here holds its id). Recorded on open-pr as
#      `opened_by` / `opened_by_id`, it is what --merge holds the PR to,
#      where it used to accept any account GitHub types a Bot. Best
#      effort HERE: a read that fails records nothing and the publish
#      stands, because the merge refuses a PR whose opener the record
#      does not name — a missing record fails closed, never open.
opened_by="" opened_by_id="" opened_unread=0
opened_n="${pr_url##*/}"
if [[ $opened_n =~ ^[0-9]+$ ]] \
        && gh_t api "repos/$MIRROR_SLUG/pulls/$opened_n" > "$workdir/opened" 2>"$workdir/err" \
        && jq_doc_file "$workdir/opened"; then
    # A read that fails has its own name, and records nothing.
    opened_by=$(jq -r 'select((.user.type // "") == "Bot") | .user.login // empty | strings' "$workdir/opened" 2>/dev/null) || opened_unread=1
    opened_by_id=$(jq -r 'select((.user.type // "") == "Bot") | .user.id // empty | numbers | tostring' "$workdir/opened" 2>/dev/null) || opened_unread=1
else
    opened_unread=1
fi
if [ "$opened_unread" = 0 ] && [ -n "$opened_by" ] && [[ $opened_by_id =~ ^[0-9]+$ ]]; then
    say "$pr_url was opened by $opened_by (id $opened_by_id), as GitHub reads it with this run's token"
else
    opened_by="" opened_by_id=""
    say "WARNING — GitHub did not name a Bot as the opener of $pr_url, so open-pr records none and --merge will refuse this PR (the next publish supersedes it)"
fi

# 4b. ONE PULL REQUEST AT A TIME (backlog d4bfe548, David 2026-09-24).
#     Every snapshot's parent is the mirror's main, which moves only
#     when David merges, so today's PR CONTAINS every older open one.
#     Measured 04:05Z that day: #242 (packet d2967a9c, closed
#     `pr-opened`) still open on GitHub while #243 carried all of it and
#     more, and #239-#242 had all stood open at once — four PRs to read
#     where one said everything. So each OLDER open publish PR from the
#     mirror's own branches is closed with a comment naming this one,
#     AFTER this one is open.
#
#     WHICH PR IS OLDER IS READ OFF THE RECORD, NOT OFF ITS NAME (backlog
#     1a2bcf11). Until 2026-09-27 "older" meant any open publish/<date>
#     [-<snapshot>] PR from the head's OWNER whose date was not after
#     this run's. The date cannot order two publishes of one day, and a
#     superseding packet is filed beside the one it supersedes, so a run
#     for the older tree could come second and close the newer tree's PR
#     (finding 2); and the owner alone matched a PR a person opened by
#     hand from another repository of theirs (finding 3). So a PR is
#     closed as superseded only when ALL of these hold:
#
#       - it comes from the mirror's own branches: its owner AND its
#         repository (from_mirror);
#       - its branch is publish-shaped — never main, never anything else;
#       - a publish packet other than this one recorded exactly it on its
#         open-pr step: this pr_url, on head `<owner>:<its branch>`;
#       - that packet's source (`source_sha`, or `forge_head` before
#         02b65d81) is this run's approved commit or an ANCESTOR of it on
#         forge main — so this PR carries every commit that one carried,
#         which is exactly what the close comment tells its reader.
#
#     Every other publish-shaped PR from the mirror's owner is KEPT, and the
#     run says which and why. Nothing here closes a PR no packet opened.
#
#     Order, for a re-run: the older packet is annotated FIRST
#     (`pr_superseded`, the intent), then the PR closed, then GitHub
#     read back and ITS answer written as `pr_state` (the effect — the
#     same key and shape `--measure` writes, which the publish region
#     reads) together with the two keys the workflow's `superseded`
#     terminal reads, `superseded_by` and `supersession_translation`
#     (backlog 78f2fbda: written as `pr_superseded` alone, publish
#     8d7a3507 stood open 95 minutes after its PR closed, until a person
#     wrote them by hand). They ride the EFFECT, not the intent, so a
#     close that did not take never closes the older packet. Any failure
#     stops the run before open-pr completes, so a re-run reuses this PR
#     and meets whatever is still open.
#
# THE RECORD, read once for 4b and 4c: every publish packet's open-pr
# but this one's, as {job, url, owner, branch, source}. A limit is not a
# filter — a PR or branch only an unread packet recorded is KEPT, and
# the run says how many packets it did not read.
curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&limit=60&full=true" > "$workdir/published" 2>"$workdir/err" \
    || fail "jobs API unreachable at $BASE while reading which packets recorded which PRs — $(cat "$workdir/err"); the PR is open at $pr_url, nothing older was closed and nothing pruned; open-pr on ${job_id:0:8} stays ready"
jq_doc_file "$workdir/published" \
    || fail "the jobs API answered the publish packets with nothing parseable — the PR is open at $pr_url, nothing older was closed and nothing pruned; open-pr on ${job_id:0:8} stays ready"
published_listed=$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$workdir/published")
published_total=$(jq -r '.total? // empty' "$workdir/published")
case "${published_total:-empty}" in
    empty|*[!0-9]*) ;;
    *) [ "$published_total" -le "$published_listed" ] \
        || say "read $published_listed of $published_total publish packets — a PR or branch only the $((published_total - published_listed)) oldest recorded is kept" ;;
esac
jq -c --arg me "$job_id" '
    (if type == "object" and has("data") then .data else . end)
    | [ .[] | select(.id != $me) | .id as $job
        | ((.steps // []) | map(select(.spec_slug == "open-pr")) | .[0]) as $o
        | select($o != null)
        | ($o.metadata // {}) as $m
        | ($m.head // "") as $h
        | select(($m.pr_url // "") != "" and ($h | contains(":")))
        | {job: $job, url: $m.pr_url, owner: ($h | split(":") | .[0]),
           branch: ($h | split(":") | .[1:] | join(":")),
           source: ($m.source_sha // $m.forge_head // "")} ]' "$workdir/published" > "$workdir/records" \
    || fail "the publish packets could not be read as records — the PR is open at $pr_url, nothing older was closed and nothing pruned; open-pr on ${job_id:0:8} stays ready"

jq -c --arg owner "$MIRROR_OWNER" --arg repo "$MIRROR_NAME" --arg branch "$BRANCH" --arg url "$pr_url" \
      --arg re "$PUBLISH_BRANCH_RE" --slurpfile rec "$workdir/records" "$FROM_MIRROR"'
    .[] | select(((.headRepositoryOwner.login // "") | ascii_downcase) == ($owner | ascii_downcase))
        | select((.headRefName // "") | test($re))
        | select(.headRefName != $branch and .url != $url)
        | . as $pr
        | {number, url, head: .headRefName, repo: (.headRepository.name // "none"),
           ours: from_mirror($owner; $repo),
           rec: ([$rec[0][] | select(.url == $pr.url and .branch == $pr.headRefName
                                     and (.owner | ascii_downcase) == ($owner | ascii_downcase))]
                 | .[0] // {})}' "$workdir/open-prs" > "$workdir/older" \
    || fail "the PR is open at $pr_url, but the open-PR listing could not be read as pull requests — nothing older was closed; open-pr on ${job_id:0:8} stays ready"
: > "$workdir/superseded"
while IFS= read -r row; do
    old_n=$(printf '%s' "$row" | jq -r '.number')
    old_url=$(printf '%s' "$row" | jq -r '.url')
    old_head=$(printf '%s' "$row" | jq -r '.head')
    old_job=$(printf '%s' "$row" | jq -r '.rec.job // empty')
    old_src=$(printf '%s' "$row" | jq -r '.rec.source // empty')
    if [ "$(printf '%s' "$row" | jq -r '.ours')" != true ]; then
        say "kept $old_url ($old_head) — it comes from $MIRROR_OWNER/$(printf '%s' "$row" | jq -r '.repo'), not $MIRROR_SLUG itself, so no publish opened it"
        continue
    fi
    if [ -z "$old_job" ]; then
        say "kept $old_url ($old_head) — no publish packet recorded this pull request on this branch, so no publish opened it; a PR opened by hand is its author's to close"
        continue
    fi
    if ! [[ "$old_src" =~ ^[0-9a-f]{40}$ ]] || ! g cat-file -e "${old_src}^{commit}" 2>/dev/null; then
        say "kept $old_url ($old_head) — packet ${old_job:0:8} recorded no source this clone holds (${old_src:-none}), so that this PR carries it cannot be shown"
        continue
    fi
    if ! g merge-base --is-ancestor "$old_src" "$source_sha" 2>/dev/null; then
        say "kept $old_url ($old_head) — its source ${old_src:0:12} (packet ${old_job:0:8}) is not in this run's ${source_sha:0:12}, so closing it would drop commits only it carries: it is the newer publish, or another line"
        continue
    fi
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    jq -n --arg old "$old_url" --arg new "$pr_url" --arg job "$job_id" --arg ts "$ts" \
        '{pr_superseded: {pr_url: $old, by_pr_url: $new, by_packet: $job, at: $ts,
                          by: "publish-github-pr"}}' > "$workdir/sup"
    curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$workdir/sup" \
            "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
        || fail "recording on ${old_job:0:8} that $pr_url supersedes $old_url failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); $old_url was not closed"
    gh_t pr close "$old_n" --repo "$MIRROR_SLUG" \
            --comment "Superseded by $pr_url — a snapshot of forge main at ${source_sha:0:12}, which contains ${old_src:0:12}, the commit this one ($old_head) published, and every commit since. Closed by machine (BOSS publish-to-github, ops verb publish-github-pr, packet $job_id); the one PR to merge is the newest." \
            > /dev/null 2>"$workdir/err" \
        || fail "closing $old_url as superseded by $pr_url — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    # A close is a claim until GitHub is read saying so.
    gh_t api "repos/$MIRROR_SLUG/pulls/$old_n" > "$workdir/closed" 2>"$workdir/err" \
        || fail "asked GitHub to close $old_url, and it could not be read back — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    jq_doc_file "$workdir/closed" \
        || fail "asked GitHub to close $old_url, and the read-back answered nothing parseable"
    old_state=$(jq -r '.state // "no state"' "$workdir/closed")
    [ "$old_state" = "closed" ] \
        || fail "asked GitHub to close $old_url as superseded by $pr_url, and it still reads $old_state"
    # `unmerged_reason`: a closed-unmerged publish says why (backlog
    # 663589cd), the same key publish-pr-state.sh writes. The two
    # terminal keys beside it close the older packet on `superseded` if
    # it is still open; on a packet already closed they record the same
    # fact where a reader of the packet looks for it.
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    jq -c --arg url "$old_url" --arg new "$pr_url" --arg ts "$ts" --arg job "$job_id" \
          --arg head "$old_head" --arg src "$source_sha" --arg old_src "$old_src" '
        {pr_state: {pr_url: $url, number, state, merged: (.merged == true),
                    merged_at, closed_at, read_at: $ts,
                    read_by: "publish-github-pr (superseded)",
                    unmerged_reason: "superseded by \($new)"},
         superseded_by: $job,
         supersession_translation: "\($url) (\($head)) was closed on GitHub by publish-github-pr at \($ts) as superseded by \($new), which publish \($job) opened from forge commit \($src). That commit contains \($old_src), the commit \($url) published, so \($new) carries every change \($url) carried."}' \
        "$workdir/closed" > "$workdir/pr-state"
    curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
            ${MT_HDR:+-H "$MT_HDR"} \
            --data-binary @"$workdir/pr-state" \
            "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
        || fail "$old_url is closed on GitHub, but writing its state onto ${old_job:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    say "superseded $old_url ($old_head) — closed on GitHub, recorded on ${old_job:0:8} as superseded by ${job_id:0:8}"
    printf '%s\n' "$old_url" >> "$workdir/superseded"
done < "$workdir/older"

# 4c. THE BRANCHES A CLOSED PR LEAVES (backlog 1a2bcf11, finding 1).
#     Since 1f0aa60d every publish pushes a branch of its own to the forge
#     and to GitHub (a personal fork until d2b7c947, the mirror itself since),
#     and nothing removed one: on 2026-09-27 both held seven
#     publish/* heads whose PRs (#239-#245) GitHub read closed, three of
#     them merged, and `boss orient` promised each "stays until GitHub
#     reports the PR merged or closed" with nothing to keep the promise.
#     The archive sweep (sweep-archive-branches.sh) was the named
#     deleter, and cannot be: it judges car branches off an ARCHIVE
#     database it is handed, runs by hand, and holds no GitHub credential,
#     while this verb already holds both pushes. So the verb that creates
#     a publish branch retires it, on each run.
#
#     A branch comes off only when ALL of these hold, else it is KEPT and
#     named with the reason:
#       - a publish packet recorded it as `<mirror owner>:<branch>` on its
#         open-pr, publish-shaped (never main), and it is not this run's;
#       - no OPEN PR from the mirror's branches stands on it — read AGAIN here, after
#         4b's closes, never from the listing taken before them;
#       - the one PR recorded on it reads `closed` (merged or not) when
#         GitHub is asked now, from the mirror's own repository, on this branch;
#       - the branch still holds the head GitHub names for that PR — the
#         delete is leased to that sha (`--force-with-lease=<ref>:<sha>`
#         with `--delete`: a delete that the target REFUSES if the ref
#         moved, never a forced update), and read back after.
#     The FORGE goes first. That order was needed while the off-site push
#     (offsite-push.sh) carried publish/* from the forge to GitHub, so a
#     branch deleted only there came back on its next tick; since backlog
#     a2b58aab it carries nothing, and the order stays as the cheaper one
#     to reason about. A branch the forge still holds is never deleted
#     from the mirror. A branch on neither side is nothing to act on and
#     is not named. A branch a packet recorded on the retired personal
#     fork (`<that account>:<branch>`) is no candidate here: this run holds no
#     credential for that repository, and David deletes it whole.
#
#     A prune failure does not fail the run: this run's PR is open and the
#     older ones are closed, and a stale branch costs nothing a re-try at
#     the next publish does not recover. It is said on the run's output
#     and rides open-pr as `prune_kept`, beside `pruned_branches`.
: > "$workdir/pruned"
: > "$workdir/prune-kept"
prune_kept() { printf '%s: %s\n' "$1" "$2" >> "$workdir/prune-kept"; say "prune: kept $1 — $2"; }
prune_side_cmd() {
    # prune_side_cmd <branch> <sha> — the forge delete, as the forge push runs.
    printf "git -c 'safe.directory=%s' -C '%s' push -q --force-with-lease='refs/heads/%s:%s' '%s' --delete 'refs/heads/%s'" \
        "$CLONE" "$CLONE" "$1" "$2" "$FORGE_PUSH_URL" "$1"
}
jq -c --arg owner "$MIRROR_OWNER" --arg branch "$BRANCH" --arg re "$PUBLISH_BRANCH_RE" '
    [ .[] | select((.owner | ascii_downcase) == ($owner | ascii_downcase))
          | select(.branch | test($re)) | select(.branch != $branch) ]
    | group_by(.branch) | .[] | {branch: .[0].branch, urls: (map(.url) | unique)}' \
    "$workdir/records" > "$workdir/prune-candidates" \
    || { prune_kept "every publish branch" "the records could not be grouped by branch"; : > "$workdir/prune-candidates"; }
if [ ! -s "$workdir/prune-candidates" ]; then
    :
elif ! list_open_prs "$workdir/open-now"; then
    prune_kept "every publish branch" "the open-PR listing could not be read again after the supersession ($(cat "$workdir/list-why")), so no branch can be shown to carry no open PR"
# THE GUARD'S KNOWN POSITIVE (adversarial review of 8ec86b42). The guard
# below counts open PRs from the mirror's branches on each branch, and a listing whose
# shape stopped matching `from_mirror` would count 0 everywhere — a blind
# guard reads exactly like a clear one. This run's own PR is open by now
# (opened or reused above), so the listing must show it, once, on this
# run's branch, or it is trusted to show nothing.
elif [ "$(jq --arg owner "$MIRROR_OWNER" --arg repo "$MIRROR_NAME" --arg b "$BRANCH" "$FROM_MIRROR"'
        [.[] | select(from_mirror($owner; $repo) and .headRefName == $b)] | length' "$workdir/open-now" 2>/dev/null)" != 1 ]; then
    prune_kept "every publish branch" "the fresh listing does not show this run's own open PR on $BRANCH, so it cannot be trusted to show any other"
elif ! forge_git ls-remote "$FORGE_REPO" 'refs/heads/publish/*' > "$workdir/forge-heads" 2>"$workdir/err"; then
    prune_kept "every publish branch" "the forge's publish heads could not be read from $FORGE_REPO — git said: $(head -c 200 "$workdir/err" | tr '\n' ' ')"
elif ! g -c "credential.helper=$helper" ls-remote mirror 'refs/heads/publish/*' > "$workdir/mirror-heads" 2>"$workdir/err"; then
    prune_kept "every publish branch" "the mirror's publish heads could not be read from $MIRROR_URL — git said: $(head -c 200 "$workdir/err" | tr '\n' ' ')"
else
    while IFS= read -r cand; do
        b=$(printf '%s' "$cand" | jq -r '.branch')
        # The shape again, in the shell, before the name goes near git: the
        # records are data the jobs API handed back.
        [[ "$b" =~ $PUBLISH_BRANCH_RE ]] || { prune_kept "$b" "not a publish branch name"; continue; }
        forge_sha=$(awk -v r="refs/heads/$b" '$2 == r { print $1 }' "$workdir/forge-heads")
        mirror_sha=$(awk -v r="refs/heads/$b" '$2 == r { print $1 }' "$workdir/mirror-heads")
        [ -n "$forge_sha$mirror_sha" ] || continue
        # A count, not `jq -e`: an unreadable answer is not zero, so it
        # keeps the branch like an open PR does.
        standing=$(jq --arg owner "$MIRROR_OWNER" --arg repo "$MIRROR_NAME" --arg b "$b" "$FROM_MIRROR"'
                [.[] | select(from_mirror($owner; $repo) and .headRefName == $b)] | length' "$workdir/open-now") \
            || standing="an unreadable number of"
        if [ "$standing" != 0 ]; then
            prune_kept "$b" "$standing open pull request(s) from the mirror's branches stand on it"
            continue
        fi
        if [ "$(printf '%s' "$cand" | jq '.urls | length')" -ne 1 ]; then
            prune_kept "$b" "packets recorded more than one pull request on it ($(printf '%s' "$cand" | jq -r '.urls | join(", ")')), so which head to lease to is not one answer"
            continue
        fi
        url=$(printf '%s' "$cand" | jq -r '.urls[0]')
        n="${url##*/}"
        case "${n:-empty}" in
            empty|*[!0-9]*) prune_kept "$b" "its packet recorded '$url', which is not a pull request url"; continue ;;
        esac
        if ! gh_t api "repos/$MIRROR_SLUG/pulls/$n" > "$workdir/pull" 2>"$workdir/err" || ! jq_doc_file "$workdir/pull"; then
            prune_kept "$b" "GitHub did not answer for $url — $(head -c 200 "$workdir/err" | tr '\n' ' ')"
            continue
        fi
        judged=$(jq -c --arg b "$b" --arg mirror "$MIRROR_SLUG" '
            if .state != "closed" then {why: "GitHub reads \(.state // "no state") for its pull request"}
            elif (.head.ref // "") != $b then {why: "GitHub names head \(.head.ref // "none") for its pull request, not this branch"}
            elif ((.head.repo.full_name // "") | ascii_downcase) != ($mirror | ascii_downcase) then
                {why: "GitHub names repository \(.head.repo.full_name // "none") for its pull request, not \($mirror)"}
            elif ((.head.sha // "") | test("^[0-9a-f]{40}$")) | not then {why: "GitHub names no head sha for its pull request"}
            else {sha: .head.sha, how: (if .merged == true then "merged" else "closed unmerged" end)} end' "$workdir/pull") \
            || { prune_kept "$b" "GitHub's answer for $url could not be read"; continue; }
        why=$(printf '%s' "$judged" | jq -r '.why // empty')
        [ -z "$why" ] || { prune_kept "$b" "$why ($url)"; continue; }
        head_sha=$(printf '%s' "$judged" | jq -r '.sha')
        how=$(printf '%s' "$judged" | jq -r '.how')
        if [ -n "$forge_sha" ]; then
            if [ "$forge_sha" != "$head_sha" ]; then
                prune_kept "$b" "the forge holds ${forge_sha:0:12}, not ${head_sha:0:12}, the head of $url ($how): it moved after the PR closed, so it stays on the forge and the mirror"
                continue
            fi
            if [ -n "$FORGE_PUSH_AS" ]; then
                runuser -l "$FORGE_PUSH_AS" -c "$(prune_side_cmd "$b" "$head_sha")" 2>"$workdir/err"
            else
                bash -c "$(prune_side_cmd "$b" "$head_sha")" 2>"$workdir/err"
            fi || { prune_kept "$b" "PRUNE FAILED on the forge ($FORGE_PUSH_URL_SHOWN): $(head -c 200 "$workdir/err" | redact_url | tr '\n' ' ')"; continue; }
            # An answer is not an effect.
            if [ -n "$(forge_git ls-remote "$FORGE_REPO" "refs/heads/$b" 2>/dev/null)" ]; then
                prune_kept "$b" "PRUNE FAILED: the forge accepted the delete and still lists the branch"
                continue
            fi
        fi
        if [ -n "$mirror_sha" ]; then
            if [ "$mirror_sha" != "$head_sha" ]; then
                prune_kept "$b" "${forge_sha:+deleted from the forge, but }the mirror holds ${mirror_sha:0:12}, not ${head_sha:0:12}, the head of $url ($how), so it stays there"
                continue
            fi
            g -c "credential.helper=$helper" push -q --force-with-lease="refs/heads/$b:$head_sha" mirror --delete "refs/heads/$b" 2>"$workdir/err" \
                || { prune_kept "$b" "PRUNE FAILED on the mirror ${forge_sha:+(deleted from the forge)}: $(head -c 200 "$workdir/err" | tr '\n' ' ')"; continue; }
            if [ -n "$(g -c "credential.helper=$helper" ls-remote mirror "refs/heads/$b" 2>/dev/null)" ]; then
                prune_kept "$b" "PRUNE FAILED: the mirror accepted the delete and still lists the branch"
                continue
            fi
        fi
        printf '%s\n' "$b" >> "$workdir/pruned"
        say "pruned $b — $url is $how; deleted at ${head_sha:0:12} from${forge_sha:+ the forge}${forge_sha:+${mirror_sha:+ and}}${mirror_sha:+ $MIRROR_SLUG}, read back gone"
    done < "$workdir/prune-candidates"
fi

# 4d. THE TOKEN ENDS WITH THE GITHUB WORK (header, THE TOKEN; design
#     76c46869's posture for a per-act token). Everything above that
#     needed it is done, so it is revoked now, before open-pr completes,
#     and how that went rides open-pr as `token_revoked`. A revoke that
#     cannot be proven does not fail the publish — the PR is open and
#     recorded either way, and the token is narrowed to the mirror and
#     dies within its hour — but it is said here and on the record.
REVOKE=0
revoke || say "WARNING — the token this run used is not proven revoked; token_revoked on open-pr says what stands"

# 5. Complete open-pr with pr_url. Merge, never replace — and the
#    SERVER merges: the keys go through the step merge door, then a PUT
#    carries the status alone (backlog e39a9d2a, Stage 2). This used to
#    send the step's read metadata back beside the new keys, because
#    the step PUT swaps metadata wholesale; its end state refuses any
#    metadata body. The keys first: pr_url is what a flip is judged on.
jq -n -c --arg url "$pr_url" --arg snap "$snapshot" \
        --arg fh "$forge_head" --arg mh "$mirror_head" --arg br "$MIRROR_OWNER:$BRANCH" \
        --arg src "$source_sha" --arg by "$approved_by" --arg at "$approved_at" \
        --rawfile sup "$workdir/superseded" --rawfile pruned "$workdir/pruned" \
        --rawfile kept "$workdir/prune-kept" --arg revoked "$TOKEN_REVOKED" \
        --arg publisher "$PUBLISHER_NAME <$PUBLISHER_EMAIL>" \
        --arg ob "$opened_by" --arg obid "$opened_by_id" '
    def lines: split("\n") | map(select(. != ""));
    def orNull: if . == "" then null else . end;
    {pr_url: $url, snapshot_commit: $snap, forge_head: $fh,
     source_sha: $src, approved_by: $by, approved_at: $at,
     opened_by: ($ob | orNull), opened_by_id: ($obid | orNull),
     mirror_head: $mh, head: $br, published_by: "publish-github-pr",
     publisher: $publisher, token_revoked: $revoked,
     superseded_prs: ($sup | lines),
     pruned_branches: ($pruned | lines), prune_kept: ($kept | lines)}' \
    > "$workdir/payload"
printf '%s\n' '{"status":"completed"}' > "$workdir/done"
if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${MT_HDR:+-H "$MT_HDR"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$job_id/steps/$step_id/metadata" > /dev/null 2>"$workdir/err"; then
    fail "the PR is open at $pr_url but recording it on open-pr (${job_id:0:8}) failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); record pr_url on the step by hand"
fi
if ! curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${MT_HDR:+-H "$MT_HDR"} \
        --data-binary @"$workdir/done" \
        "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err"; then
    fail "the PR is open at $pr_url and recorded on open-pr, but completing open-pr on ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); complete the step by hand"
fi

say "done — $pr_url carries $source_sha (open-pr on ${job_id:0:8} completed; the App merges it once every check main requires is green — merge-publish-pr)"
