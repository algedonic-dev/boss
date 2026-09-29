#!/usr/bin/env bash
#
# forge-converge — the forge host adopts its OWN units from forge main,
# the way cluster-deploy-runner rolls the CLUSTER onto forge main.
#
# WHY THIS EXISTS. The cluster converges every ten minutes; the forge
# host had no equivalent loop for its own infra/forge + infra/ops
# units, so every unit authored after the last hand-run of install.sh
# sat inert on main: reap-dead-ci-jobs through the 2026-08-17 fill,
# disk-floor-sweep through the 2026-09-03 fill that blocked every
# train. "Landed but never installed" was a recurring, expensive class
# of defect, and its single cause was that nothing ran install.sh. This
# closes the class: main moves, the next converge installs whatever is
# new. (docs/design/the-build-plane-manages-itself.md, car 4 / keystone.)
#
# It runs install.sh, which is idempotent — installs+enables every unit
# in its UNITS list and restarts only what changed. forge-converge is
# itself in that list, so after the ONE bootstrap `sudo install.sh` it
# reinstalls itself and no unit ever again needs a hand-install.
#
# Install (forge host — the one surviving hand action, the bootstrap
# that installs the loop that ends the bootstraps):
#   sudo cp infra/forge/forge-converge.{service,timer} /etc/systemd/system/
#   sudo systemctl daemon-reload && sudo systemctl enable --now forge-converge.timer
# or simply `sudo infra/forge/install.sh`, which now covers it.
set -euo pipefail

# SNAPSHOT-EXEC before touching git — the identical hazard
# cluster-deploy-runner.sh documents at length: the git checkout below
# rewrites THIS file's bytes while bash is still reading it by offset,
# so it resumes mid-token in the new contents — a silent, unrepeatable
# failure. exec into a copy first, so the bytes bash executes are
# unreachable from the repo git is about to move.
if [ -z "${BOSS_CONVERGE_SNAPSHOT:-}" ]; then
    snap="$(mktemp -t forge-converge.XXXXXX)"
    cat "$0" > "$snap"
    # Where this script's own library lives, captured while $0 still
    # points into the checkout: after the exec it points at the snapshot
    # in /tmp, so `dirname $0` finds nothing of ours.
    BOSS_FORGE_CONVERGE_INFRA="${BOSS_FORGE_CONVERGE_INFRA:-$(cd "$(dirname "$0")/.." && pwd)}" \
        BOSS_CONVERGE_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi
trap 'rm -f "$BOSS_CONVERGE_SNAPSHOT"' EXIT

REPO="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
OWNER="${BOSS_FORGE_REPO_OWNER:-david}"

# WHAT THIS RUN LEAVES FOR ITS OWN PACKET. maintenance-forge-converge
# closed `result=ok` carrying nothing else — the same silence as its
# boss-gcp sibling, which on 2026-09-11 was enough to support a wrong
# conclusion about what a converge had installed (infra/run-summary.sh
# carries the measurement). This host does have a readable journal door,
# so the stakes are lower; it is also the host whose door once answered
# 200 with a seven-hour-stale journal, and two converges that report
# differently about the same question are the §9a shape. install.sh below
# records what it installed; this records which commit from.
#
# CLEARED FIRST, BEFORE ANYTHING CAN FAIL. boss-step.sh reads the file
# from ExecStopPost — a different process — so a run that dies early must
# leave nothing, or the last run's success is read as this run's.
# shellcheck source=infra/run-summary.sh
. "${BOSS_FORGE_CONVERGE_INFRA:-$(dirname "$0")/..}/run-summary.sh"
run_summary_reset

# THE CHECKOUT'S FORGE CREDENTIAL, FIRST (design 1c90d183, David
# 2026-09-26; backlog c4cbc6b5). It lived in the userinfo of the
# `forgejo` remote, and a git error that printed the URL printed it into
# this unit's journal. It now lives in a 0600 file of the owner's behind
# a git credential helper scoped to the forge's URL, and the broker
# rotates it: credential-deposit.sh reads the Secret the broker rule
# declares through the forge's admin kubeconfig, proves a new value by
# effect before it replaces the file, strips the remote's userinfo once
# the helper authenticates, reports the held token's last eight on this
# run's packet, and records delivery on the rotation packet so the
# broker may revoke the old token. BEFORE the fetch, because it owes
# nothing to the forge token: a revoked or broken one cannot stop the
# step that repairs it. Its exit is carried like install.sh's, so the
# rest of the converge still runs.
INFRA="${BOSS_FORGE_CONVERGE_INFRA:-$REPO/infra}"
FORGE_TOKEN_FILE="${BOSS_FORGE_TOKEN_FILE:-/home/$OWNER/.config/boss/forge-checkout.token}"
deposit_rc=0
"$INFRA/forge/credential-deposit.sh" \
    --rule "$INFRA/dispatcher/rules/broker-rotates-the-forge-host-checkout-token.toml" \
    --checkout "$REPO" --dest "$FORGE_TOKEN_FILE" --owner "$OWNER" || deposit_rc=$?

# Fetch and check out forge main as the checkout's OWNER, never as root
# — a root `git` in a david-owned clone leaves root-owned objects that
# break the owner's later pulls. `-l` gives the owner's login env so the
# credential helper that carries the forge token is found. Detached at
# the sha (like cluster-deploy-runner) rather than a tracking branch, so
# the host holds no branch state of its own to diverge. --ff-only is
# implicit in a detached checkout of the fetched sha: no merge is made.
#
# Through the checkout's ONE lock (checkout-lock.sh, sourced inside the
# owner's shell so the lock file is the owner's): cluster-deploy-runner
# fetches this same checkout on this same tick, and its merge-triggered
# start lands a second after any merge. On 2026-09-07 22:01 this fetch
# held `refs/remotes/forgejo/main` while the runner's fetch died on it
# (backlog d66f92b2). The helper is read whole at source time, so the
# checkout it then performs cannot rewrite the code running it.
runuser -l "$OWNER" -c "cd '$REPO' && . infra/forge/checkout-lock.sh && checkout_git '$REPO' fetch -q forgejo main && checkout_git '$REPO' checkout -qf \"\$(git rev-parse forgejo/main)\""

# WHICH COMMIT THIS HOST'S UNITS NOW COME FROM, read as the checkout's
# owner for the same reason every git call above is: root cannot even READ
# a clone it does not own. Handed to install.sh as BOSS_CONVERGE_SHA:
# the CLI it installs for the cluster-operator role is taken out of the
# cluster image built for exactly this commit (infra/estate/
# install-cli-from-image.sh, backlog 9f00a805), and install.sh, running
# as root, cannot read the sha off the owner's clone itself.
BOSS_CONVERGE_SHA="$(runuser -l "$OWNER" -c "git -C '$REPO' rev-parse HEAD")"
export BOSS_CONVERGE_SHA
run_summary_field converge_sha "$BOSS_CONVERGE_SHA"

# WHAT THIS HOST IS FOR, read off the system of record the same way
# boss-gcp reads it (infra/estate/node-roles.sh, one definition). The
# forge's id is declared on the unit (BOSS_NODE_ID=forge), never guessed
# from a hostname. install.sh inherits BOSS_NODE_ROLES and installs what
# the roles bring — today `cluster-operator` brings talosctl and the
# credential check (design 1bc4b4ed).
NODE_ID="${BOSS_NODE_ID:-forge}"
. "${BOSS_FORGE_CONVERGE_INFRA:-$(dirname "$0")/..}/estate/node-roles.sh"
BOSS_CONVERGE_NAME="forge-converge" read_node_roles "$NODE_ID"
run_summary_field node_id "$NODE_ID"
run_summary_field node_roles "${BOSS_NODE_ROLES:-}"

# install.sh needs root (writes /etc/systemd/system). This script runs
# as root; git already finished above, so install.sh's bytes are stable
# for the duration of its run and it needs no snapshot of its own. Its
# exit is carried rather than fatal, so main's protection below is
# converged on every tick even when a unit or the CLI step reds it.
install_rc=0
"$REPO/infra/forge/install.sh" || install_rc=$?

# MAIN'S PROTECTION, as the tree declares it (backlog f9256445, car 4 of
# design d812f1b7; David answered Q2 "yes" on 2026-09-25: direct push and
# force push off, so only a PR merge moves forge main — declared here,
# applied by this converge, never set by hand in the Forgejo UI).
# infra/forge/protect-main.sh is the one definition and carries the
# reasoning, including the writer this rule CANNOT stop: the rewind of
# 2026-09-25 was Forgejo's own push-mirror sync (`update by push` as
# Gitea <gitea@fake.local>), which never passes the pre-receive hook
# where protection lives, so the conductor's ancestry arm is the only
# guard against it.
#
# THE CREDENTIAL IS THE CHECKOUT'S OWN: the token file the deposit
# above keeps (the same value the fetch's helper read), copied by root
# into a root-only header file with a builtin — made in the unit's
# RUNTIME_DIRECTORY (never written without one; see below) — and deleted on exit. It
# never reaches an argv or the journal. The deposit runs first on every
# pass, so the pass that cuts the checkout over already reads the file
# here; a pass with no file (the deposit refused, and said why on this
# packet) leaves the header empty and protect-main's own exit 4 names it.
# NOT the remote URL's userinfo, which forge-auth-header.sh read from
# #697 until this car deleted it (backlog 85f8a614): the deposit strips
# that userinfo, after which the reader had nothing to read and would
# have exited 3 on every run. And NOT `git credential fill`, whose
# prompt-disabled error named the userinfo in this journal (backlog
# 164f38c7, 37497977). Whether it may administer the repository is
# MEASURED by the first write — a 401/403 is named on this run's packet —
# rather than assumed; nothing here mints or places a credential
# (CLAUDE.md §Doors, the credential broker). Run after install.sh, which
# renders the /etc/boss/sor.env that carries BOSS_FORGE_URL.
#
# THE HEADER LIVES IN THE UNIT'S RUNTIME DIRECTORY, OR IS NOT WRITTEN
# (backlog 359a811c, 2026-09-28). forge-converge.service declares
# RuntimeDirectory=: tmpfs under /run, 0700, removed by systemd when the
# oneshot run stops WHATEVER the exit — so after the SIGKILL
# TimeoutStartSec sends second too — and a power loss clears it. Until
# then the header was a `mktemp -t` file under the host's shared /tmp,
# which the EXIT trap removes on every end but those two, and there the
# token stayed until tmp cleanup (the ops runner's same defect: 5f77b205).
# The first of a colon list, should the unit ever declare two.
#
# No fallback to /tmp. Without the directory, NO header is written and the
# two readers of it — protect-main and offsite-push — are skipped, the
# run ending 78 (REFUSED) with `refused` on its packet. Everything ABOVE
# still ran — the deposit, the fetch, install.sh — and the DR render below
# still runs: none of them touches this header, and install.sh is the step
# that installs the unit carrying the directive. That is load-bearing, and
# the first draft of this car got it wrong (adversarial review, 2026-09-28):
# cluster-deploy-runner checks this same checkout out at the new sha about
# a minute after every train merge, so the timer's next tick runs THIS
# script under the OLD unit, which has no directory. Refusing the whole
# run there would never install the new unit and would refuse every tick
# after — a wedge only a hand `sudo install.sh` clears. Refusing only the
# header, that tick installs the unit and the next one runs clean.
rtdir="${RUNTIME_DIRECTORY:-}"
rtdir="${rtdir%%:*}"
protect_rc=0
offsite_rc=0
auth_hdr=""
if [ -z "$rtdir" ] || [ ! -d "$rtdir" ]; then
    echo "forge-converge: REFUSED the forge header — RUNTIME_DIRECTORY is not a directory ('${RUNTIME_DIRECTORY:-unset}'); forge-converge.service's RuntimeDirectory= sets it, and the token is written nowhere else. protect-main and offsite-push are skipped this run; install.sh ran, so the next tick runs under a unit that has it." >&2
    run_summary_field refused "RUNTIME_DIRECTORY is not a directory ('${RUNTIME_DIRECTORY:-unset}'): no forge header, protect-main and offsite-push skipped"
    protect_rc=78
    offsite_rc=78
else
    # STALE HEADERS FROM BEFORE THIS CAR. A run SIGKILLed or cut by a power
    # loss while the header lived in /tmp left it there; nothing writes
    # that glob any more, and a oneshot never runs beside itself, so every
    # such file older than this run's directory is a leftover. Bounded to
    # the top level of /tmp and files this account owns (root, on the host).
    stale="$(find "${BOSS_FORGE_STALE_HEADER_DIR:-/tmp}" -maxdepth 1 -type f -uid "$(id -u)" \
        -name 'forge-auth.*' ! -newer "$rtdir" -delete -print 2>/dev/null | wc -l)" || stale="unknown"
    run_summary_field stale_headers_removed "$stale"

    auth_hdr="$(mktemp -p "$rtdir" forge-auth.XXXXXX)"
    chmod 600 "$auth_hdr"
    trap 'rm -f "$BOSS_CONVERGE_SNAPSHOT" "$auth_hdr"' EXIT
    # The token file is the OWNER's to write and this script runs as root,
    # so root never reads it (review A1 of the re-review of 5ef6db0b,
    # 2026-09-26): a symlink planted there to /etc/boss-ops/kubeconfig was
    # read as root and sent to the forge in this header. A symlink gets no
    # header, which protect-main names; the file is read as the owner, who
    # cannot read what they could not already.
    if [ -L "$FORGE_TOKEN_FILE" ]; then
        echo "forge-converge: $FORGE_TOKEN_FILE is a symlink, not the deposit's own file; protect-main gets no header" >&2
    elif [ -s "$FORGE_TOKEN_FILE" ] \
        && FORGE_TOKEN="$(runuser -u "$OWNER" -- cat -- "$FORGE_TOKEN_FILE")"; then
        printf 'Authorization: token %s\n' "$FORGE_TOKEN" >"$auth_hdr"
        unset FORGE_TOKEN
    fi
    BOSS_FORGE_AUTH_HEADER_FILE="$auth_hdr" "$REPO/infra/forge/protect-main.sh" || protect_rc=$?
fi

# THE DR COPY'S GITHUB TOKEN, RENDERED ON EVERY TICK AND BEFORE THE PUSH
# (design 76155676, backlog 81eb6d4d). It is a GitHub App installation
# token that lives ONE HOUR; the credential broker re-mints it into
# Secret boss/github-dr-push-token before it expires, and this copies the
# live value into the root-only file the off-site push reads for
# algedonic-dev/boss-dr — or removes the file when the Secret holds
# nothing live, so the push refuses on an empty slot instead of pushing
# with a dead token. The destination is the DR target's `token_file` in
# offsite-push.json (backlog 761bc8a9); credential_render_sh.rs pins the
# two equal.
render_rc=0
"$INFRA/forge/credential-render.sh" \
    --rule "$INFRA/dispatcher/rules/broker-rotates-the-github-dr-push-token.toml" \
    --dest "${BOSS_GITHUB_DR_TOKEN_FILE:-/etc/boss-publish/github-dr.token}" || render_rc=$?

# THE OFF-SITE COPY, pushed by us and not by Forgejo (backlog 21d54f4a,
# decided 2026-09-26: no mirror can wipe what it mirrors). Forgejo's push
# mirror is `git push -f --mirror` whatever its filter, and it was the
# writer that rewound main on 2026-09-25; infra/forge/offsite-push.sh is
# the one definition and carries the reasoning. A plain push of what
# offsite-push.json declares — a non-fast-forward is refused and named,
# never overwritten — read back, and only then the Forgejo push mirror
# deleted. Main goes, alone, to the PRIVATE DR copy algedonic-dev/boss-dr
# (backlog 761bc8a9), read with the token rendered just above. It
# declares NOTHING for the public fork dauld/boss-mirror: not main
# (backlog 67931115), not publish/* (backlog a2b58aab) — the fork gets
# publish branches only from publish-github-pr.sh, scanned and approved.
# Same forge header as protect-main (deleting a mirror is the same
# repository administration); each GitHub token is its target's own
# file, read by git's credential helper, never here — and an empty DR
# slot is a refusal on this packet, not a skip.
if [ "$offsite_rc" -eq 0 ]; then
    BOSS_FORGE_AUTH_HEADER_FILE="$auth_hdr" "$REPO/infra/forge/offsite-push.sh" || offsite_rc=$?
fi

# install.sh's verdict first (it is the older and wider one), then the
# protection's, then the off-site push's, then the deposit's: any reds
# this run and puts the packet on `failed`.
[ "$install_rc" -eq 0 ] || exit "$install_rc"
[ "$protect_rc" -eq 0 ] || exit "$protect_rc"
[ "$offsite_rc" -eq 0 ] || exit "$offsite_rc"
[ "$render_rc" -eq 0 ] || exit "$render_rc"
exit "$deposit_rc"
