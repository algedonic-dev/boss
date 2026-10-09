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
    #
    # `pwd -P`, the PHYSICAL directory (backlog a604a35b): the unit starts
    # this through $TREE/current, a name root-tree.sh below MOVES to the
    # next generation in the middle of this run. Every library and every
    # deposit this run reads comes from the generation it started in.
    BOSS_FORGE_CONVERGE_INFRA="${BOSS_FORGE_CONVERGE_INFRA:-$(cd "$(dirname "$0")/.." && pwd -P)}" \
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

# WHICH MACHINE THIS IS, BEFORE THE FIRST DEPOSIT (backlog 62b09c57, N7).
# Everything below reads the cluster's Secrets, moves a checkout and a
# tree, and runs install.sh — on the machine it is started on. Until this
# line nothing asked which machine that was: on 2026-10-07 23:12Z the
# launcher, started in a fixture tree on the dev pod with a stub for the
# legacy converge only, ran THIS file from the fixture's generation as
# root for 1.5 s, and install.sh put an address file, a launcher, a
# downloaded kubectl and a `boss-probe` account on the pod (review
# aa901496). infra/lib/host-check.sh is the one definition and carries
# the rule: each line here names one place this run would touch the
# machine's own system; with every one redirected the run is a test and
# is asked nothing, and with any one real the machine must hold the
# address infra/estate/estate.toml declares for the forge, or this exits
# 78 with nothing read, fetched or written, and says so on its packet
# (`host_check`). install.sh, root-tree.sh, the two deposits further down
# and the ops runner's installer ask again for themselves, so a hand run
# of any one of them is held to the same answer.
#
# IT CAN STOP THIS LOOP, AND THAT IS SAID HERE. A forge that does not
# hold the declared address — a rebuild brought up elsewhere, an estate
# file edited ahead of the machine — refuses every tick, and a refused
# tick fetches nothing, so the fix does not arrive by merge. By hand, as
# root, once: BOSS_FIRST_INSTALL_AS=forge
# /usr/local/libexec/boss/forge-converge-launch. The line it prints, and
# `host_check` on that run's packet, say it was overridden.
# shellcheck source=infra/lib/host-check.sh
. "${BOSS_FORGE_CONVERGE_INFRA:-$(dirname "$0")/..}/lib/host-check.sh"
host_seam BOSS_FORGE_REPO_DIR /home/david/boss
host_seam BOSS_FORGE_TOKEN_FILE
host_seam BOSS_MACHINE_TOKEN_DIR /etc/boss/machine-token
host_seam BOSS_PROBE_READER_CREDENTIAL /etc/boss/probe-reader.credential
host_seam BOSS_GITHUB_DR_TOKEN_FILE /etc/boss-publish/github-dr.token
host_seam BOSS_RUNNER_CREDENTIAL_FILE /etc/boss/ops-runner.credential
host_seam BOSS_FORGE_STALE_HEADER_DIR /tmp
host_check forge forge-converge
[ -z "$HOST_CHECK_VERDICT" ] || echo "forge-converge: host check — $HOST_CHECK_VERDICT"

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

# THE ESTATE MACHINE TOKEN, ALL THREE SLOTS, ROOT-ONLY (design-doc
# 20058482, question forge-token, David 2026-10-06; backlog 88379df3).
# Every shell caller on this host already asks infra/lib/secret-header.sh
# for the token and found no file; machine-token-deposit.sh reads Secret
# boss/boss-machine-token through the admin kubeconfig this host already
# holds, makes /etc/boss/machine-token equal to it (root:root, 0700 and
# 0600), and proves it by the gate's own answer to a stamped read.
#
# HERE, BEFORE THE FETCH: the fetch is the first step `set -e` can end
# this run on, and a rotation's drain (1440 minutes, the rule's line)
# is counted against how often this runs, not how often the forge
# token works. Ten minutes a tick is 144 ticks inside one drain.
#
# ITS EXIT IS RECORDED AND NEVER CARRIED. Every other step's exit below
# reds this run; this one's does not, on purpose. The converge installs
# this host's repairs and owes the token nothing — a host with no token
# is a host whose callers send without one, which a gate in `report`
# admits and tallies (CLAUDE.md §Diagnosis: an arm that needs the patient
# is not an arm). The script names its own fault on stderr and on this
# run's packet (machine_token_secret / _action / _effect), and the
# status rides beside them.
#
# ITS TIME IS BOUNDED TOO (adversarial review 9a1e289b, B1). An exit that
# is not carried is half of owing nothing: the first draft ran this in
# the foreground with no bound, and a sleeping kubectl held the converge
# at this line until the unit's TimeoutStartSec killed it — before the
# fetch, before install.sh, on every tick for as long as the cluster API
# or the docker daemon stalled. The script bounds each of its own waits;
# this bounds the script, so a wait nobody thought of costs
# FORGE_DEPOSIT_BOUND_S and never a tick. A stop reads 124 (or 137 when
# TERM was not enough) in machine_token_deposit_status, the action line
# says what that means, and the converge goes on. Nothing alarms on a
# non-zero status yet: it rides a green packet, and until something reads
# it the gates' own tally of this host's misses is the alarm.
FORGE_DEPOSIT_BOUND_S="${BOSS_FORGE_DEPOSIT_BOUND_S:-180}"
machine_token_rc=0
timeout -k 5 "$FORGE_DEPOSIT_BOUND_S" "$INFRA/forge/machine-token-deposit.sh" \
    --rule "$INFRA/dispatcher/rules/broker-rotates-the-machine-token.toml" \
    --dest "${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}" || machine_token_rc=$?
run_summary_field machine_token_deposit_status "$machine_token_rc"
case "$machine_token_rc" in
    124 | 137)
        echo "forge-converge: the machine token deposit did not finish inside $FORGE_DEPOSIT_BOUND_S seconds and was stopped; the converge goes on, and the next tick deposits again" >&2
        run_summary_field machine_token_action "STOPPED: the deposit did not finish inside $FORGE_DEPOSIT_BOUND_S seconds (a stalled cluster API or docker daemon); any slot it had written stands, and the next tick follows the Secret again"
        ;;
esac

# THE PROBE READER'S CLIENT FILE, `current` ONLY, ROOT-ONLY (design
# b35c22b4; backlog d26515c5, unit 7; review f09db7f0, F4). A recorded
# probe runs here as boss-probe and holds no machine token; `boss prove`,
# as root, opens a reader door for it with the credential in
# /etc/boss/probe-reader.credential — which nothing delivered until this
# line. probe-reader-deposit.sh reads the `current` key of Secret
# boss/boss-probe-reader and no other, presents that value ALONE to
# /api/machine-gate/accepts on every gated port of the table the door
# itself forwards to, and writes the file only when every one answers
# `reader.current`. A gate that names it by a BARE slot is saying it
# equals an estate token: refused, loud, never deposited. An empty
# Secret — which it is until the first rotation is scoped — is a not-yet
# on this packet (probe_reader_effect), and nothing is ever removed.
#
# THE PORT TABLE IS THE ROOT-OWNED PROBE VIEW'S, NOT THIS CHECKOUT'S
# (adversarial review 4d39f4dc, N6). "Every gate" is as strong as the
# list of gates: a table shortened to one deposited at 1 of 1. The door
# takes its port list only from a file root alone can write (review
# 991bb439, N3), and that file exists here — infra/forge/probe-account.sh
# keeps a root-owned clone at this path (its VIEW, by its own variable
# and default) and points the ops runner's BOSS_PROBE_DIR at it; uid 0,
# mode 0644, measured by probe-door-check on 2026-10-07. The deposit
# refuses a table that is not its account's alone, so on a host where
# the view does not exist yet it deposits nothing and says so. The view
# is refreshed by install.sh further down, so this reads the table of
# the PREVIOUS tick's commit — the one the door is reading too.
#
# The destination is the ONE name the door reads, by the door's own
# variable (boss-cli probe_reader.rs CREDENTIAL_ENV, DEFAULT_CREDENTIAL;
# pinned equal by probe_reader_deposit_sh.rs).
#
# HERE, BESIDE THE ESTATE TOKEN'S, AND FOR ITS REASONS: before the fetch,
# so a broken forge token cannot stop the file following a promotion
# inside the rotation's drain; its exit RECORDED and never carried; its
# time bounded, so a stalled read or a silent gate costs this bound and
# never a tick. The two deposits' bounds together stay well inside the
# unit's TimeoutStartSec.
FORGE_READER_DEPOSIT_BOUND_S="${BOSS_FORGE_READER_DEPOSIT_BOUND_S:-90}"
probe_reader_rc=0
timeout -k 5 "$FORGE_READER_DEPOSIT_BOUND_S" "$INFRA/forge/probe-reader-deposit.sh" \
    --rule "$INFRA/dispatcher/rules/broker-rotates-the-probe-reader.toml" \
    --ports "${BOSS_PROBE_VIEW:-/var/lib/boss/probe-view}/infra/forge/sor-ports.env" \
    --dest "${BOSS_PROBE_READER_CREDENTIAL:-/etc/boss/probe-reader.credential}" || probe_reader_rc=$?
run_summary_field probe_reader_deposit_status "$probe_reader_rc"
case "$probe_reader_rc" in
    124 | 137)
        echo "forge-converge: the probe reader's deposit did not finish inside $FORGE_READER_DEPOSIT_BOUND_S seconds and was stopped; the converge goes on, and the next tick deposits again" >&2
        run_summary_field probe_reader_action "STOPPED: the deposit did not finish inside $FORGE_READER_DEPOSIT_BOUND_S seconds (a stalled cluster API, docker daemon or gate); a file it had already renamed into place stands, and the next tick asks again"
        run_summary_field probe_reader_effect "UNVERIFIED: the deposit was stopped at its bound before it recorded what the gates said"
        ;;
esac

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
#
# ITS EXIT IS CARRIED, NO LONGER FATAL (backlog a604a35b). Until root had
# a tree of its own this checkout was what the rest of the run executed,
# so a fetch that failed had to end the run. It no longer is: the units
# and install.sh come from root's tree below, and the checkout is kept
# current only for what still reads it — the ops runner's verbs and the
# cluster converge, until each moves. A broken checkout or a revoked
# checkout token reds this run and stops nothing.
checkout_rc=0
runuser -l "$OWNER" -c "cd '$REPO' && . infra/forge/checkout-lock.sh && checkout_git '$REPO' fetch -q forgejo main && checkout_git '$REPO' checkout -qf \"\$(git rev-parse forgejo/main)\"" || checkout_rc=$?
[ "$checkout_rc" -eq 0 ] || run_summary_field checkout "FAILED (exit $checkout_rc): the owner's fetch or checkout of $REPO — root's tree below does not depend on it"

# THE TREE ROOT EXECUTES FROM, BROUGHT TO THE FORGE'S MAIN (backlog
# a604a35b; decided by David on design-doc c98c79aa, question root-tree,
# and as position B, 2026-10-07). infra/forge/root-tree.sh is the one
# definition and carries the reasoning: root fetches refs/heads/main from
# the forge's own repository on local disk, served as that repository's
# owner, accepts it only as a fast-forward of what it last accepted,
# exports it whole and then moves one name. The checkout above is not in
# its path. A refresh that fails or refuses leaves the previous tree in
# place, says so on this packet (root_tree) and reds this run — and
# install.sh below still runs, from the tree as it stands.
#
# AFTER IT, THIS RUN MARKS ITS OWN GENERATION GOOD. A refresh that
# returned 0 is the proof that running THIS script can bring in the next
# fix, which is all forge-converge-launch.sh needs to know to prefer it
# over a newer generation that has not shown as much. Only a script that
# was started from a generation can be marked; under the old unit, from
# the checkout, there is nothing to mark and the launcher's bootstrap
# arm stays open until the first tick from the tree.
tree_rc=0
tree_line="$("$INFRA/forge/root-tree.sh" refresh 2>&1)" || tree_rc=$?
printf '%s\n' "$tree_line"
run_summary_field root_tree "$(printf '%s' "$tree_line" | tail -n 1 | cut -c1-600)"
# `path` exits 1 when `current` names no whole generation: that is "no
# tree", by name, and never an empty answer taken for one.
no_tree=""
TREE_NOW="$("$INFRA/forge/root-tree.sh" path 2>/dev/null)" || no_tree=1
[ -z "$no_tree" ] || TREE_NOW=""
if [ "$tree_rc" -eq 0 ]; then
    "$INFRA/forge/root-tree.sh" mark-good "$INFRA/.." >/dev/null 2>&1 || true
fi
# WHERE THE REST OF THIS RUN EXECUTES FROM: the generation the refresh
# just made current — so a train's install.sh runs on the tick that
# fetched it and its converge script on the next, as before — or, on a
# host with no tree yet, the directory this script started in. Never
# $REPO: the checkout is the owner's to write.
RUN_FROM="${TREE_NOW:+$TREE_NOW/infra}"
RUN_FROM="${RUN_FROM:-$INFRA}"
run_summary_field runs_from "$RUN_FROM"

# WHICH COMMIT THIS HOST'S UNITS NOW COME FROM, read as the checkout's
# owner for the same reason every git call above is: root cannot even READ
# a clone it does not own. Handed to install.sh as BOSS_CONVERGE_SHA:
# the CLI it installs for the cluster-operator role is taken out of the
# cluster image built for exactly this commit (infra/estate/
# install-cli-from-image.sh, backlog 9f00a805), and install.sh, running
# as root, cannot read the sha off the owner's clone itself.
#
# FROM ROOT'S TREE WHEN THERE IS ONE (backlog a604a35b): the commit
# install.sh is about to run from is the generation's own stamp, and the
# CLI it installs must be that commit's. The owner's answer is the
# fallback for a host with no tree yet, and is data from another account
# either way — a commit name or nothing.
# A read that failed is said (converge_sha_unread) and leaves NO sha:
# install.sh then installs no CLI and says so, rather than one for a
# commit nobody named.
BOSS_CONVERGE_SHA=""
sha_unread=""
if [ -n "$TREE_NOW" ]; then
    BOSS_CONVERGE_SHA="$("$INFRA/forge/root-tree.sh" head 2>/dev/null)" || sha_unread="root's tree at $TREE_NOW did not name its commit"
else
    BOSS_CONVERGE_SHA="$(runuser -l "$OWNER" -c "git -C '$REPO' rev-parse HEAD")" || sha_unread="the owner's git could not read HEAD of $REPO"
fi
if [ -z "$sha_unread" ] && ! [[ "$BOSS_CONVERGE_SHA" =~ ^[0-9a-f]{40,64}$ ]]; then
    sha_unread="what was read is not a commit name"
fi
if [ -n "$sha_unread" ]; then
    echo "forge-converge: no converged sha — $sha_unread; install.sh installs no CLI this tick" >&2
    run_summary_field converge_sha_unread "$sha_unread"
    BOSS_CONVERGE_SHA=""
fi
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
"$RUN_FROM/forge/install.sh" || install_rc=$?

# AFTER A PRE-CAR GENERATION'S INSTALLER, THE RUNNER IS POINTED BACK AT
# THE CHECKOUT (review 5f3736a2, F2). A generation with no root-tree.sh
# is a commit from before root had a tree — in practice a merged REVERT
# of that car, which root-tree.sh accepts on purpose so the host rolls
# back by itself. Its install.sh is the old one, and the old
# install-ops-runner.sh writes the runner's ExecStart as "the tree I am
# in": run from root's tree that is /var/lib/boss/tree/gen/<revert>, an
# export with no .git, where the publish, merge and tag verbs fail — and
# it stood for a tick, until the reinstalled checkout converge repointed
# it. That installer cannot be changed; this script, still the car's on
# the tick that fetches the revert, runs ITS OWN generation's runner
# installer right after, naming the checkout — the same line the car's
# install.sh passes. What remains is the seconds between the two
# daemon-reloads. Its exit is carried: the runner is a door, and a door
# pointed at an export is a red run.
repoint_rc=0
if [ -n "$TREE_NOW" ] && [ ! -e "$TREE_NOW/infra/forge/root-tree.sh" ]; then
    echo "forge-converge: the generation just installed from ($TREE_NOW) carries no root-tree.sh — a commit from before root's tree, so its installer pointed the ops runner at that export; pointing it back at $REPO"
    INSTALL_OPS_RUNNER_REPO="$REPO" bash "$INFRA/ops/install-ops-runner.sh" "$NODE_ID" || repoint_rc=$?
    run_summary_field ops_runner_repointed "a pre-car generation installed (no root-tree.sh in $TREE_NOW): the runner's ExecStart named again as $REPO/infra/ops/ops-runner.sh (exit $repoint_rc)"
fi

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
    BOSS_FORGE_AUTH_HEADER_FILE="$auth_hdr" "$RUN_FROM/forge/protect-main.sh" || protect_rc=$?
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

# THIS HOST'S OPS RUNNER CREDENTIAL (design f623e425 Q1; backlog 1e50e66b).
# The broker stages the forge's new value in Secret
# boss/ops-runner-credential (key forge.next); runner-credential-deposit.sh
# proves it through the jobs API's credential door, installs it into the
# root-only file infra/ops/ops-runner.sh presents (the same
# BOSS_RUNNER_CREDENTIAL_FILE, one default in both — pinned by
# runner_credential_deposit_sh.rs), and records delivery on the rotation
# packet so the broker may promote it. It never stops the runner: no file
# is a runner that answers without a credential, as every runner did
# before the door. Its exit is carried like the render's.
runner_rc=0
"$INFRA/forge/runner-credential-deposit.sh" \
    --rule "$INFRA/dispatcher/rules/broker-rotates-the-forge-ops-runner-credential.toml" \
    --dest "${BOSS_RUNNER_CREDENTIAL_FILE:-/etc/boss/ops-runner.credential}" || runner_rc=$?

# THE OFF-SITE COPY, pushed by us and not by Forgejo (backlog 21d54f4a,
# decided 2026-09-26: no mirror can wipe what it mirrors). Forgejo's push
# mirror is `git push -f --mirror` whatever its filter, and it was the
# writer that rewound main on 2026-09-25; infra/forge/offsite-push.sh is
# the one definition and carries the reasoning. A plain push of what
# offsite-push.json declares — a non-fast-forward is refused and named,
# never overwritten — read back, and only then the Forgejo push mirror
# deleted. Main goes, alone, to the PRIVATE DR copy algedonic-dev/boss-dr
# (backlog 761bc8a9), read with the token rendered just above, and it is
# the one target: every target is the algedonic-dev organisation's
# (backlog d2b7c947). The public mirror gets publish branches only from
# publish-github-pr.sh, scanned and approved — never main (backlog
# 67931115) and never a forge branch by its name alone (backlog a2b58aab).
# Same forge header as protect-main (deleting a mirror is the same
# repository administration); each GitHub token is its target's own
# file, read by git's credential helper, never here — and an empty DR
# slot is a refusal on this packet, not a skip.
if [ "$offsite_rc" -eq 0 ]; then
    BOSS_FORGE_AUTH_HEADER_FILE="$auth_hdr" "$RUN_FROM/forge/offsite-push.sh" || offsite_rc=$?
fi

# install.sh's verdict first (it is the older and wider one), then the
# protection's, then the off-site push's, the DR render's, the runner
# credential's, then the checkout deposit's: any reds this run and puts
# the packet on `failed`.
[ "$install_rc" -eq 0 ] || exit "$install_rc"
[ "$protect_rc" -eq 0 ] || exit "$protect_rc"
[ "$offsite_rc" -eq 0 ] || exit "$offsite_rc"
[ "$render_rc" -eq 0 ] || exit "$render_rc"
[ "$runner_rc" -eq 0 ] || exit "$runner_rc"
[ "$repoint_rc" -eq 0 ] || exit "$repoint_rc"
# Root's tree, then the owner's checkout: each already said why on this
# packet (root_tree, checkout), and neither stopped anything above.
[ "$tree_rc" -eq 0 ] || exit "$tree_rc"
[ "$checkout_rc" -eq 0 ] || exit "$checkout_rc"
exit "$deposit_rc"
