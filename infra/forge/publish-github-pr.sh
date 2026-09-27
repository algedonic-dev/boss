#!/usr/bin/env bash
#
# publish-github-pr — open the public mirror's pull request BY MACHINE.
#
# The machine half of publish-to-github v6 (design 7b59af2c, David
# 2026-09-08: the mirror at github.com/algedonic-dev/boss is "strictly a
# backup of source protocol" and the PR "should open from our dauld
# GitHub account"). When David signs the protocol's approve step, the
# dispatcher rule publish-github-pr-on-open-pr-ready files an
# ops-request for the forge host, and the root ops-runner runs THIS
# script with no arguments. It:
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
#   4. pushes it to the dauld fork as publish/<date>-<snapshot> —
#      a branch of its own, never forced (backlog 1f0aa60d) — recreating
#      the fork once if it is gone, opens the PR against
#      algedonic-dev/boss as dauld, and completes the packet's open-pr
#      step with pr_url and snapshot_commit;
#   5. closes each OLDER open publish/<date>[-<snapshot>] PR from the
#      fork as superseded by this one, which contains it (backlog
#      d4bfe548) — so there is only ever one PR to merge.
#
# THE MERGE ON GITHUB STAYS DAVID'S — the second gate. Nothing here
# touches the mirror's main; the only PRs it closes are its own older
# publish snapshots.
#
# THE TOKEN. dauld's GitHub token is provisioned by David's token admin
# at $BOSS_GITHUB_TOKEN_FILE (default /etc/boss-publish/github.token,
# root:root 0600, one line) and declared in the credentials registry
# (id dauld-github-token). This script only ever READS it, hands it to
# git through a credential helper and to gh through GH_TOKEN on those
# two child processes, and never prints it. A missing or world-readable
# token file is a loud refusal naming the path — never a silent skip.
#
# --check: validate inputs (tools, token file, forge repository, state
# dir) with no network and exit 0/1. What the gate lint runs. It also
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
# overwritten. A verb killed mid-way leaves at worst a pushed branch on
# our own fork.
#
# Runs as root under the ops-runner with NO HOME: every path is explicit
# (state dir, GH_CONFIG_DIR) and nothing reads $HOME.
set -euo pipefail

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/jq.sh"
# The server's step shape hash — what a passkey stamp is bound to — in
# the one copy ops-runner.sh shares (backlog 02b65d81, "A run" 1a below).
# shellcheck source=infra/lib/step-shape.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/step-shape.sh"

TOKEN_FILE="${BOSS_GITHUB_TOKEN_FILE:-/etc/boss-publish/github.token}"
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
# THE FORK — dauld/boss-mirror, which is the one repository in
# algedonic-dev/boss's fork network that dauld owns. Until 2026-09-11 this
# default read `dauld/boss`, and NOTHING in the tree ever set
# BOSS_FORK_SLUG (it appeared exactly once, here), so the default was
# always what ran. Measured against GitHub's REST API that day, with a
# control on the same connection:
#
#   repos/dauld/boss          -> 404 (David's unrelated PRIVATE repository)
#   repos/algedonic-dev/boss  -> 200  fork=false parent=none
#   repos/dauld/boss-mirror   -> 200  fork=true  parent=algedonic-dev/boss
#
# The push to dauld/boss SUCCEEDED — the token authenticates for it — and
# `gh pr create` then failed with four GraphQL errors at once ("Head sha
# can't be blank", "Base sha can't be blank", "No commits between
# algedonic-dev:main and dauld:publish/2026-09-11", "Head ref must be a
# branch"), none of which names the cause: GitHub opens a pull request
# only between two repositories in ONE fork network, and dauld/boss is
# not in it. The slug was the symptom; the check below is the defect.
# THE FORGE PUSH (ce5339d6). The forge repository carries a push mirror
# to the SAME fork this verb opens PRs from — `git push --mirror` on
# every commit, which PRUNES any branch the forge lacks. PR #238 opened
# at 22:46:59Z on 2026-09-11 and GitHub closed it at 22:49:17Z, head
# deleted, two minutes and one train later; publish/2026-09-08 survived
# because it also existed on the forge. So the snapshot goes to the forge
# FIRST, under the same branch name, and the mirror carries it from
# there. Since backlog 21d54f4a (2026-09-26) that mirror is gone: the
# forge converge's infra/forge/offsite-push.sh pushes main and publish/*
# to the same fork with a plain push that neither forces nor prunes, and
# deleted the Forgejo push mirror. The forge-first push stays, so the
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
FORK_SLUG="${BOSS_FORK_SLUG:-dauld/boss-mirror}"
FORK_URL="${BOSS_FORK_URL:-https://github.com/${FORK_SLUG}.git}"
FORK_OWNER="${FORK_SLUG%%/*}"
AUTHOR_NAME="${BOSS_PUBLISH_AUTHOR_NAME:-dauld}"
AUTHOR_EMAIL="${BOSS_PUBLISH_AUTHOR_EMAIL:-dauld@users.noreply.github.com}"
DATE="${BOSS_PUBLISH_DATE:-$(date -u +%Y-%m-%d)}"
# BRANCH is publish/<date>-<snapshot>, named once the snapshot exists
# (step 3 below) — see there for why the date alone is not a name.
CLONE="${STATE_DIR}/boss.git"
export GH_CONFIG_DIR="${GH_CONFIG_DIR:-${STATE_DIR}/gh}"
ACTOR="${BOSS_OPS_ACTOR:-automation:publish-github-pr}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

me="publish-github-pr"
say() { echo "$me: $*"; }
refuse() { echo "$me: REFUSED — $*" >&2; exit 2; }
fail() { echo "$me: FAILED — $*" >&2; exit 1; }

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
workdir=$(mktemp -d) || { echo "$me: FAILED — no working directory under ${TMPDIR:-/tmp}" >&2; exit 1; }
trap 'rm -rf "$workdir"' EXIT

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

# ---------------------------------------------------------------------
# Preconditions — the same list --check reports on.
# ---------------------------------------------------------------------
token_problem() {
    if [ ! -e "$TOKEN_FILE" ]; then
        echo "the dauld GitHub token is not at $TOKEN_FILE — David's token admin provisions it (root:root 0600, the token on one line); set BOSS_GITHUB_TOKEN_FILE if it lives elsewhere"
    elif [ ! -r "$TOKEN_FILE" ]; then
        echo "$TOKEN_FILE exists but is not readable by this user"
    elif [ ! -s "$TOKEN_FILE" ]; then
        echo "$TOKEN_FILE is empty"
    else
        local mode
        mode=$(stat -c %a "$TOKEN_FILE" 2>/dev/null || echo "?")
        case "$mode" in
            600|400) ;;
            *) echo "$TOKEN_FILE is mode $mode — a token file must be 0600 or 0400 (chmod 600 $TOKEN_FILE)" ;;
        esac
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
    for tool in git gh curl jq; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "$me: missing tool on PATH: $tool" >&2; rc=1
        fi
    done
    problem=$(token_problem)
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
    echo "  token file : $TOKEN_FILE"
    echo "  forge repo : $FORGE_REPO"
    echo "               ($FORGE_REPO_FROM)"
    echo "  mirror     : $MIRROR_URL (anonymous fetch)"
    # Named as NOT covered on purpose: --check holds no token and opens no
    # socket, so it cannot ask GitHub whether this is a fork of the mirror
    # — the question that a green --check preceded three failures on
    # without ever asking (2026-09-11). Saying so beats implying it.
    echo "  fork       : $FORK_URL (push as $FORK_OWNER; that it is a fork of $MIRROR_SLUG is checked at run time, with the token — not here)"
    echo "  state dir  : $STATE_DIR"
    echo "  jobs api   : ${BOSS_JOBS_URL:-<unset — the ops-runner pins it on its Exec line>}"
    if check_inputs; then rc=0; else rc=1; fi
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
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
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
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
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
# A run.
# ---------------------------------------------------------------------
[ -n "${BOSS_JOBS_URL:-}" ] || refuse "BOSS_JOBS_URL is not set; the ops-runner pins it on its Exec line and a hand run must name the system of record"
BASE="${BOSS_JOBS_URL%/}"
check_inputs || refuse "inputs incomplete (see above); nothing was fetched or pushed"

# 1. The packet. One mirror, one open publish packet (149's guard), and
#    its open-pr must be ready or active — a rule fired on readiness.
if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=publish-to-github&status=open&limit=20&full=true" > "$workdir/jobs" 2> "$workdir/err"; then
    fail "jobs API unreachable at $BASE — $(cat "$workdir/err")"
fi
target=$(jq -c '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open"))
    | map({id, title, step: (((.steps // []) | map(select(.spec_slug == "open-pr")) | .[0])
                            // ((.steps // []) | map(select(.title == "open-pr")) | .[0]))})
    | map(select(.step != null and (.step.status == "ready" or .step.status == "active")))
    | .[0] // empty' "$workdir/jobs")
if [ -z "$target" ]; then
    say "no open publish-to-github packet has its open-pr step ready — nothing to do"
    exit 0
fi
job_id=$(printf '%s' "$target" | jq -r '.id')
step_id=$(printf '%s' "$target" | jq -r '.step.id')
say "packet ${job_id:0:8} — open-pr ready; reading its approval"

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
VERB_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ops/verbs/publish-github-pr.json"
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
say "approved by $approved_by with a passkey at $approved_at, over the measured tree $source_sha; publishing it as publish/$DATE-<snapshot>"

# 2. Refs. A private bare clone under the state dir; the forge is read
#    as a path on this host, the mirror anonymously.
mkdir -p "$STATE_DIR" "$GH_CONFIG_DIR"
[ -d "$CLONE" ] || git init -q --bare "$CLONE"
set_remote forge "$FORGE_REPO"
set_remote mirror "$MIRROR_URL"
set_remote fork "$FORK_URL"
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

if [ "$source_tree" = "$mirror_tree" ]; then
    fail "the mirror's main already carries the approved tree ($source_sha) — nothing to publish; open-pr on ${job_id:0:8} stays open for a person to close or supersede"
fi

# 3. The snapshot commit. Tree = the approved commit's tree, parent =
#    the mirror's main. Authored as dauld; the approved sha rides in the
#    body.
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
#    mirror's main, the packet and the day. Nothing that moves when a
#    train lands enters it: not forge main's time, and not forge main's
#    sha in the message (it is said on the run's own output instead). A
#    re-run for the same packet (its own completion failed after the PR
#    opened) rebuilds the identical commit however many trains have
#    landed since, its push is a no-op, and the reuse lookup finds its PR
#    instead of opening a second one.
source_time=$(g log -1 --format=%ct "$source_sha") \
    || fail "reading the approved commit's time ($source_sha)"
snapshot=$(GIT_AUTHOR_NAME="$AUTHOR_NAME" GIT_AUTHOR_EMAIL="$AUTHOR_EMAIL" \
           GIT_COMMITTER_NAME="$AUTHOR_NAME" GIT_COMMITTER_EMAIL="$AUTHOR_EMAIL" \
           GIT_AUTHOR_DATE="@$source_time +0000" GIT_COMMITTER_DATE="@$source_time +0000" \
           g commit-tree "$source_tree" -p "$mirror_head" \
             -m "publish: $DATE" \
             -m "Snapshot of forge main at $source_sha onto the public mirror — the commit the publish packet measured, scanned and a passkey approved. The mirror is a backup of source: each publish is one commit whose tree is that commit's tree (a merge of the forge history conflicts in hundreds of files). Opened by machine (BOSS publish-to-github, ops verb publish-github-pr) from packet $job_id; merged by a person.") \
    || fail "git commit-tree"
BRANCH="publish/${DATE}-${snapshot:0:12}"
say "snapshot $snapshot (tree $source_tree of the approved $source_sha, parent mirror $mirror_head; forge main is $forge_head) — branch $BRANCH"

# 4. The fork, the push, the PR — the only three steps that need the
#    token. It reaches git through a credential helper that reads the
#    FILE (the gate-runner's idiom) and gh through GH_TOKEN on that one
#    process; neither is ever echoed.
helper="!f() { echo username=x-access-token; echo \"password=\$(cat '$TOKEN_FILE')\"; }; f"
gh_t() { GH_TOKEN="$(cat "$TOKEN_FILE")" gh "$@"; }

# A SIXTH REFUSAL — the fork must be a fork OF THE MIRROR, not merely a
# name that resolves. Until 2026-09-11 this asked `gh repo view
# "$FORK_SLUG" --json name`, which proves only that SOMETHING answers to
# that name. On 2026-09-11 something did: dauld/boss, David's unrelated
# private repository, outside algedonic-dev/boss's fork network entirely.
# So the auto-fork below was skipped (the repository "existed"), the
# snapshot was pushed into the wrong repository, and the failure surfaced
# one step later as four GraphQL errors that name no cause. CLAUDE.md
# §Doors, "a wrong target answers instead of erroring", and its corollary:
# before concluding something exists, ask a question whose answer
# distinguishes it from its namesake.
#
# The question is therefore the RELATIONSHIP. GitHub's REST repository
# object answers it in two fields — `parent.full_name` is what a fork was
# forked FROM, `source.full_name` is the root of its whole network, so a
# fork of a fork of the mirror still answers the mirror — and `gh api` is
# the stable way to read them. Three outcomes, three different things to
# do, and each says which one it took:
#
#   absent (404)       the auto-fork's case, and the only one it was ever
#                      written for. It forks UNDER $FORK_SLUG's own name
#                      (--fork-name): `gh repo fork` otherwise names the
#                      new fork after the upstream, which on this account
#                      is dauld/boss — the repository that caused this.
#   present, unrelated REFUSED, naming both slugs and what the repository
#                      says about itself. Never a push: the push is the
#                      one step here that puts our commit somewhere we did
#                      not choose, and it cannot be taken back from here.
#   present, a fork of the mirror — proceed, saying so.
fork_of_mirror() {
    # 0 = $1 is a fork whose parent or source is $MIRROR_SLUG; 1 = it is
    # not; 2 = there is no such repository (gh could not read it at all).
    # Either way $workdir/fork-says holds the repository's own words, so
    # the refusal quotes GitHub rather than paraphrasing it.
    local slug="$1" meta="$workdir/fork.json"
    if ! gh_t api "repos/$slug" > "$meta" 2>"$workdir/err"; then
        return 2
    fi
    # gh exited 0, which is not the same as gh having ANSWERED: an empty
    # body reaches `jq -r` as no document, so fork-says would be blank
    # and `jq -e` below would exit 0 — "yes, this is the fork, push to
    # it" on no evidence at all (d96e38ab). 1, not 2: 2 goes on to FORK
    # the repository, and a write is the wrong answer to a read that did
    # not happen. 1 refuses with these words and pushes nothing.
    if ! jq_doc_file "$meta"; then
        printf 'nothing readable — gh answered with no repository object' > "$workdir/fork-says"
        return 1
    fi
    jq -r '"fork=\(.fork // false) parent=\(.parent.full_name // "none") source=\(.source.full_name // "none") private=\(.private // "?")"' \
        "$meta" > "$workdir/fork-says" 2>/dev/null \
        || printf 'a repository object jq could not read' > "$workdir/fork-says"
    jq -e --arg m "$MIRROR_SLUG" '
        (.fork == true)
        and ((((.parent.full_name // "") | ascii_downcase) == ($m | ascii_downcase))
             or (((.source.full_name // "") | ascii_downcase) == ($m | ascii_downcase)))' \
        "$meta" >/dev/null 2>&1
}

fork_rc=0
fork_of_mirror "$FORK_SLUG" || fork_rc=$?
if [ "$fork_rc" -eq 0 ]; then
    say "fork $FORK_SLUG confirmed in $MIRROR_SLUG's network ($(cat "$workdir/fork-says"))"
elif [ "$fork_rc" -eq 2 ]; then
    say "fork $FORK_SLUG not found ($(head -c 200 "$workdir/err" | tr '\n' ' ')) — forking $MIRROR_SLUG once as ${FORK_SLUG#*/}"
    gh_t repo fork "$MIRROR_SLUG" --clone=false --fork-name "${FORK_SLUG#*/}" >/dev/null 2>"$workdir/err" \
        || fail "gh repo fork $MIRROR_SLUG --fork-name ${FORK_SLUG#*/}: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    # Re-read rather than assume. The repository we push to must be the
    # fork we just made, and "the command exited 0" is not that — it is
    # the same standard of evidence that failed today.
    fork_of_mirror "$FORK_SLUG" \
        || refuse "forked $MIRROR_SLUG as $FORK_SLUG, but GitHub does not then report $FORK_SLUG as a fork of $MIRROR_SLUG ($(cat "$workdir/fork-says" 2>/dev/null || printf 'it is still not there')) — nothing was pushed"
    say "forked $MIRROR_SLUG as $FORK_SLUG ($(cat "$workdir/fork-says"))"
else
    refuse "$FORK_SLUG exists on GitHub but is NOT a fork of $MIRROR_SLUG — it says $(cat "$workdir/fork-says"). A pull request can only be opened between two repositories in one fork network, so pushing $BRANCH there would succeed and then fail at gh pr create with errors that name no cause (measured 2026-09-11 against dauld/boss, a namesake outside the network). Point BOSS_FORK_SLUG at a fork of $MIRROR_SLUG, or rename $FORK_SLUG so this verb forks it itself. Nothing was pushed"
fi

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
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN) as $FORGE_PUSH_AS: $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' '). Without it on the forge, the off-site push (offsite-push.sh) cannot carry it"
    say "pushed publish/${BRANCH#publish/} to the forge as $FORGE_PUSH_AS ($FORGE_PUSH_URL_SHOWN) — the off-site push carries it too"
else
    bash -c "$forge_push_cmd" 2>"$workdir/err" \
        || fail "pushing $BRANCH to the forge ($FORGE_PUSH_URL_SHOWN): $(head -c 300 "$workdir/err" | redact_url | tr '\n' ' ')"
    say "pushed publish/${BRANCH#publish/} to the forge ($FORGE_PUSH_URL_SHOWN) — the off-site push carries it too"
fi

# Never forced — the same three outcomes as the forge push above.
g -c "credential.helper=$helper" push -q fork "$snapshot:refs/heads/$BRANCH" 2>"$workdir/err" \
    || fail "pushing $BRANCH to $FORK_URL (not forced: the branch is this snapshot's own, so a refusal means it already holds another commit, which is never overwritten): $(head -c 300 "$workdir/err" | tr '\n' ' ')"
say "pushed $FORK_OWNER:$BRANCH"

# THE MIRROR'S OPEN PULL REQUESTS, read once: the reuse question below
# and the supersession sweep (4b) both answer from this listing.
#
# The reuse question used to be `gh pr list --head "<owner>:<branch>"`,
# and gh's own manual says of --head: 'Filter by head branch
# ("<owner>:<branch>" syntax not supported)'. It compares the whole
# string to the bare branch name, so it matched NOTHING, ever — measured
# 2026-09-27, when PR #245 stood open on dauld:publish/2026-09-27 and the
# lookup answered empty (backlog 1f0aa60d). The listing gh does answer
# carries the head's owner and branch as separate fields, and both are
# compared here. A limit is not a filter: a listing that comes back FULL
# may hide the PR asked about, so it is refused rather than read as
# absence.
PR_LIST_LIMIT=200
gh_t pr list --repo "$MIRROR_SLUG" --state open --limit "$PR_LIST_LIMIT" \
        --json number,url,headRefName,headRepositoryOwner > "$workdir/open-prs" 2>"$workdir/err" \
    || fail "listing the mirror's open pull requests failed — gh said: $(head -c 300 "$workdir/err" | tr '\n' ' '); $FORK_OWNER:$BRANCH is pushed (snapshot $snapshot), no PR was opened or reused, open-pr on ${job_id:0:8} stays ready and a re-run pushes the same commit"
jq_doc_file "$workdir/open-prs" && jq -e 'type == "array"' "$workdir/open-prs" > /dev/null 2>&1 \
    || fail "gh answered the open-PR listing with no list — nothing was read, so no PR was opened, reused or closed; $FORK_OWNER:$BRANCH is pushed (snapshot $snapshot) and open-pr on ${job_id:0:8} stays ready"
[ "$(jq 'length' "$workdir/open-prs")" -lt "$PR_LIST_LIMIT" ] \
    || fail "the mirror has $PR_LIST_LIMIT or more open pull requests, so the listing may not hold the one for $FORK_OWNER:$BRANCH — nothing was opened, reused or closed; $FORK_OWNER:$BRANCH is pushed (snapshot $snapshot)"
pr_url=$(jq -r --arg owner "$FORK_OWNER" --arg branch "$BRANCH" '
    first(.[] | select(((.headRepositoryOwner.login // "") | ascii_downcase) == ($owner | ascii_downcase))
              | select((.headRefName // "") == $branch) | .url) // empty' "$workdir/open-prs") \
    || fail "the open-PR listing could not be read as pull requests"
if [ -n "$pr_url" ]; then
    say "PR already open for $FORK_OWNER:$BRANCH — reusing $pr_url at $snapshot"
else
    pr_url=$(gh_t pr create --repo "$MIRROR_SLUG" --base main --head "$FORK_OWNER:$BRANCH" \
        --title "publish: $DATE" \
        --body "Backup-of-source publish from the internal forge: snapshot \`$snapshot\` carries forge main at \`$source_sha\` — the commit measured, secrets-scanned and approved with a passkey — as one commit on top of the mirror's \`$mirror_head\`. Opened by machine (BOSS publish-to-github, packet $job_id); the merge is a person's." \
        2>"$workdir/err") \
        || fail "gh pr create for $FORK_OWNER:$BRANCH — $FORK_OWNER:$BRANCH is pushed (snapshot $snapshot) and no PR carries it; gh said: $(head -c 300 "$workdir/err" | tr '\n' ' ')"
    say "opened $pr_url at $snapshot"
fi

# 4b. ONE PULL REQUEST AT A TIME (backlog d4bfe548, David 2026-09-24).
#     Every snapshot's parent is the mirror's main, which moves only
#     when David merges, so today's PR CONTAINS every older open one.
#     Measured 04:05Z that day: #242 (packet d2967a9c, closed
#     `pr-opened`) still open on GitHub while #243 carried all of it and
#     more, and #239-#242 had all stood open at once — four PRs to read
#     where one said everything. So each OLDER open publish PR from our
#     fork is closed with a comment naming this one, AFTER this one is
#     open. Only our fork's heads, only `publish/<date>` or
#     `publish/<date>-<snapshot>` whose date is not after this run's:
#     a newer-dated PR, somebody else's branch, or a non-publish PR from
#     the fork is never ours to close. A SAME-day publish on another
#     branch is older than this one (1f0aa60d: #245 on the date-only
#     publish/2026-09-27 is exactly that case), because this snapshot is
#     cut from forge main now, on top of the same mirror main.
#
#     Order, for a re-run: the older packet is annotated FIRST
#     (`pr_superseded`, the intent), then the PR closed, then GitHub
#     read back and ITS answer written as `pr_state` (the effect — the
#     same key and shape `--measure` writes, which the publish region
#     reads). Any failure stops the run before open-pr completes, so a
#     re-run reuses this PR and meets whatever is still open. A PR no
#     packet recorded is closed all the same and said so by URL.
jq -c --arg owner "$FORK_OWNER" --arg branch "$BRANCH" --arg url "$pr_url" --arg date "$DATE" '
    .[] | select(((.headRepositoryOwner.login // "") | ascii_downcase) == ($owner | ascii_downcase))
        | select((.headRefName // "") | test("^publish/[0-9]{4}-[0-9]{2}-[0-9]{2}(-[0-9a-f]+)?$"))
        | select(.headRefName[8:18] <= $date and .headRefName != $branch and .url != $url)
        | {number, url, head: .headRefName}' "$workdir/open-prs" > "$workdir/older" \
    || fail "the PR is open at $pr_url, but the open-PR listing could not be read as pull requests — nothing older was closed; open-pr on ${job_id:0:8} stays ready"
: > "$workdir/superseded"
if [ -s "$workdir/older" ]; then
    curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&limit=60&full=true" > "$workdir/published" 2>"$workdir/err" \
        || fail "jobs API unreachable at $BASE while recording superseded PRs — $(cat "$workdir/err"); nothing older was closed"
    while IFS= read -r row; do
        old_n=$(printf '%s' "$row" | jq -r '.number')
        old_url=$(printf '%s' "$row" | jq -r '.url')
        old_head=$(printf '%s' "$row" | jq -r '.head')
        old_job=$(jq -r --arg url "$old_url" 'first((if type == "object" and has("data") then .data else . end)
            | .[] | select(any((.steps // [])[]; .spec_slug == "open-pr" and (.metadata.pr_url // "") == $url))
            | .id) // empty' "$workdir/published")
        ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        if [ -n "$old_job" ]; then
            jq -n --arg old "$old_url" --arg new "$pr_url" --arg job "$job_id" --arg ts "$ts" \
                '{pr_superseded: {pr_url: $old, by_pr_url: $new, by_packet: $job, at: $ts,
                                  by: "publish-github-pr"}}' > "$workdir/sup"
            curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                    --data-binary @"$workdir/sup" \
                    "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
                || fail "recording on ${old_job:0:8} that $pr_url supersedes $old_url failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); $old_url was not closed"
        fi
        gh_t pr close "$old_n" --repo "$MIRROR_SLUG" \
                --comment "Superseded by $pr_url — today's snapshot of forge main, which carries everything in this one ($old_head) and every commit since. Closed by machine (BOSS publish-to-github, ops verb publish-github-pr, packet $job_id); the one PR to merge is the newest." \
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
        if [ -n "$old_job" ]; then
            # `unmerged_reason`: a closed-unmerged publish says why
            # (backlog 663589cd), the same key publish-pr-state.sh writes.
            jq -c --arg url "$old_url" --arg new "$pr_url" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
                {pr_state: {pr_url: $url, number, state, merged: (.merged == true),
                            merged_at, closed_at, read_at: $ts,
                            read_by: "publish-github-pr (superseded)",
                            unmerged_reason: "superseded by \($new)"}}' \
                "$workdir/closed" > "$workdir/pr-state"
            curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
                    --data-binary @"$workdir/pr-state" \
                    "$BASE/api/jobs/$old_job/metadata" > /dev/null 2>"$workdir/err" \
                || fail "$old_url is closed on GitHub, but writing its state onto ${old_job:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' ')"
            say "superseded $old_url ($old_head) — closed on GitHub, recorded on ${old_job:0:8}"
        else
            say "superseded $old_url ($old_head) — closed on GitHub; no publish packet recorded it, so the close comment is its only record"
        fi
        printf '%s\n' "$old_url" >> "$workdir/superseded"
    done < "$workdir/older"
fi

# 5. Complete open-pr with pr_url. Merge, never replace (the
#    ops-runner's rule): PUT swaps metadata wholesale.
printf '%s' "$target" | jq -c --arg url "$pr_url" --arg snap "$snapshot" \
        --arg fh "$forge_head" --arg mh "$mirror_head" --arg br "$FORK_OWNER:$BRANCH" \
        --arg src "$source_sha" --arg by "$approved_by" --arg at "$approved_at" \
        --rawfile sup "$workdir/superseded" '
    {status: "completed",
     metadata: ((.step.metadata // {})
                + {pr_url: $url, snapshot_commit: $snap, forge_head: $fh,
                   source_sha: $src, approved_by: $by, approved_at: $at,
                   mirror_head: $mh, head: $br, published_by: "publish-github-pr",
                   superseded_prs: ($sup | split("\n") | map(select(. != "")))})}' \
    > "$workdir/payload"
if ! curl -fsS -X PUT -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        --data-binary @"$workdir/payload" \
        "$BASE/api/jobs/$job_id/steps/$step_id" > /dev/null 2>"$workdir/err"; then
    fail "the PR is open at $pr_url but completing open-pr on ${job_id:0:8} failed — $(head -c 300 "$workdir/err" | tr '\n' ' '); record pr_url on the step by hand"
fi

say "done — $pr_url carries $source_sha (open-pr on ${job_id:0:8} completed; the merge is David's)"
