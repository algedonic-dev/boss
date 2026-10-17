#!/usr/bin/env bash
#
# cluster-deploy-runner — converge the cluster onto forge main.
#
# The deployment half of the forge shipping protocol (directive
# 27ab7680): the conductor merges CI-green trains into forge main;
# this runner, on the forge host, notices main moved, builds the
# all-in-one image, pushes it to the forge registry, applies the
# tree's cluster manifests (infra/cluster/manifests/), and rolls the
# cluster deployment. Derived state reconverging on intent
# (deployment-as-network) — no ssh from the conductor, no shared
# credentials: each side touches only what it owns. Cluster config
# converges on forge main exactly like code does; hand-applied
# changes are drift (see infra/cluster/manifests/README.md).
#
# Install (forge host): install.sh's UNITS list, which forge-converge
# runs from root's tree on every tick.
#
# Expects: root, started from root's tree (/var/lib/boss/tree/current;
# infra/forge/root-tree.sh); the SYSTEM docker daemon; the registry push
# login the forge converge deposits (REGISTRY_PUSH_DIR, forge-defaults.sh);
# the admin kubeconfig David places (ops_kubeconfig,
# infra/estate/ops-credentials.sh); kubectl via the alpine/k8s container.
set -euo pipefail

# ROOT, FROM ROOT'S TREE, ON THE SYSTEM DAEMON (backlog 683d7fe9, car 2;
# decided by David 2026-10-07 on design-doc c8502e17, question
# image-build, and as position B of the forge sudo decision). Until this
# car the converge ran as david: it fetched and checked out his clone,
# built and pushed on his ROOTLESS docker daemon with the registry login
# he once typed into his ~/.docker/config.json, read his ~/kc.yaml, kept
# its stamps in his home — and reached root anyway through a passwordless
# `sudo docker` for every kubectl it ran. So the loop that builds the
# image every pod runs was one edit of a file his account owns away from
# running anything, and its calls went out without the machine token,
# which only root can read. Now:
#   - it runs as root, and executes and BUILDS root's own tree: a
#     generation root fetched from the forge's repository and verified
#     (root-tree.sh carries why that, and not a checkout);
#   - it builds and pushes on the system daemon, the one the CI jobs
#     already run in, with the token the credential broker mints and the
#     forge converge deposits root-only (registry-token-deposit.sh);
#   - its kubectl reads the admin kubeconfig David placed, and its state
#     lives under CONVERGE_STATE (forge-defaults.sh), root's.
# The operator's rootless daemon is used by no unattended code after it.
# Its images and build cache stay where they are until he removes them
# (infra/forge/OPERATIONS.md says how much and the one command), and the
# FIRST build on the system daemon is cold — the compiler's cache mounts
# live in the daemon that built them — inside the unit's 90 minutes.
#
# A RUN THAT IS NOT ROOT IS THE UNIT BEFORE THIS CAR. On the train that
# carries this file the installed unit is still david's: it checks out
# the new commit and hands over to this copy as david, which can neither
# refresh root's tree nor read the push login. That run refuses, saying
# so, and forge-converge installs the root unit on its next tick; the
# run after that builds. Pointed at a tree of its caller's choosing
# (BOSS_ROOT_TREE) this is a test, and is not asked.
ROOT_TREE="${BOSS_ROOT_TREE:-/var/lib/boss/tree}"
if [ -z "${BOSS_ROOT_TREE:-}" ] && [ "$(id -u)" -ne 0 ]; then
    echo "cluster-deploy-runner: REFUSED — this converge runs as root from root's tree since backlog 683d7fe9, and this run is $(id -un 2>/dev/null || id -u): the unit installed on this host predates that car. forge-converge installs the root unit on its next tick, and the run after it builds; nothing was built or rolled" >&2
    exit 1
fi
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"

# THE RUN ENDS BY SAYING HOW IT ENDED — to the ops-request that started
# it, when one did (cluster-deploy-lib.sh answer_converge_requests;
# backlog d66f92b2). STAGE names where the run is, so a death anywhere
# below reads `converge_failed: <stage> (exit N)` on the packet that
# asked, instead of the clean `answered` that `systemctl start
# --no-block` returned before the unit had done anything. OUTCOME is set
# by the exits that are not failures (unchanged, held, converged). exec
# replaces the process, so the request is answered by the run that
# actually exits.
STAGE="start"
OUTCOME=""
# The image repo this converge pushes (REGISTRY), its stamp (STAMP_FILE
# — the last build rolled and verified), the quarantine stamp for a head
# whose BOOT failed (FAILED_FILE — "proven unbootable; do not re-roll
# it"), the directory all of them live in (CONVERGE_STATE) and the
# registry push login (REGISTRY_PUSH_DIR) are forge-defaults.sh's: one
# definition for this runner, the watchdog, the rollback and the sweeps,
# off the registry host in /etc/boss/sor.env (backlog 5222163e). Before
# the trap, which answers requests out of CONVERGE_STATE.
. "$REPO/infra/forge/forge-defaults.sh"
# THE ALERT SPOOL IS THIS CONVERGE'S OWN, HOWEVER IT WAS STARTED (review
# 1ba2873d, B2). Every tick replays what the spool holds to the jobs API
# as root, with the machine token (replay_kept_alerts, below). Only the
# unit used to name it, so a run started without the unit's environment
# replayed alert-lib.sh's fallback, /var/tmp/boss-alert-spool — sticky and
# world-writable: any account on the forge could plant a body there for
# root to file as platform-admin. The default is under CONVERGE_STATE,
# root's and 0700; the unit names the same path (forge-defaults.sh says
# why there are two spellings and which test holds them equal).
ALERT_SPOOL="${ALERT_SPOOL:-$CONVERGE_ALERT_SPOOL}"

# WHERE THE MINUTES WENT, on the packet. Every converge closed its
# maintenance packet `result=ok` and nothing else; how long the image
# build took — the second-longest stage of a car's life, 10–20 min
# measured 2026-09-12 — lived only in this journal, 200 lines at a time
# through an ops-request. run-summary.sh is the one definition of how a
# unit leaves facts for its own ExecStopPost (the unit declares
# BOSS_RUN_SUMMARY_FILE); each stage below stamps its seconds as soon as
# it knows them, and a run that dies keeps what it had recorded.
. "$REPO/infra/run-summary.sh"
run_summary_reset
_stage_started=$(date +%s)
_stage_done() { # <field>  — seconds since the previous stage boundary
    local now; now=$(date +%s)
    run_summary_field "$1" "$(( now - _stage_started ))"
    _stage_started=$now
}
_finish() {
    local rc=$?
    if [ -n "$OUTCOME" ]; then
        answer_converge_requests "$CONVERGE_STATE" "${OUTCOME%%=*}" "${OUTCOME#*=}" || true
    elif [ "$rc" -ne 0 ]; then
        answer_converge_requests "$CONVERGE_STATE" converge_failed "$STAGE (exit $rc)" || true
    fi
    # THE MAINTENANCE PACKET NAMES THE STAGE TOO (backlog 07d7549c,
    # 2026-09-16). The ops-request above has read `converge_failed:
    # <stage>` since d66f92b2, but a run the TIMER started answers no
    # request, and its own packet closed `result: exit-code` — the
    # stage that died was in this journal alone. A verdict must name
    # what failed (CLAUDE.md §Diagnosis), on the packet that carries it.
    if [ "$rc" -ne 0 ]; then
        run_summary_field failed_stage "$STAGE"
    fi
    exit "$rc"
}
trap _finish EXIT
# Sourced from the generation this run started in, and again from the
# view it hands over to below; both carry the functions the trap uses,
# which is why they live in the lib and not in this file.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
take_converge_requests "$CONVERGE_STATE" || true
# What an earlier tick's alert door kept, filed now (the lib says why
# this is the converge's own job since backlog a604a35b).
replay_kept_alerts || true
forge_need REGISTRY

# FORGE MAIN, AS ROOT'S TREE HOLDS IT (backlog 683d7fe9). The converge no
# longer fetches a checkout: root-tree.sh refresh brings the forge's own
# main into a new verified generation and moves `current` — a
# fast-forward of what root last accepted, or a refusal, never a rewound
# main. forge-converge refreshes on its own tick too; one lock serialises
# the two. A refresh that FAILED or was REFUSED fails this run here, as
# the fetch did, and `current` stays what it was.
#
# ONCE PER TICK (review 1ba2873d, N3): by the stage that hands over to
# the view below. The view's own stage runs the generation the refresh
# just named, so asking again only took the lock a second time.
if [ -z "${BOSS_RUNNER_VIEW:-}" ]; then
    STAGE="fetch"
    tree_rc=0
    tree_line="$(BOSS_ROOT_TREE="$ROOT_TREE" bash "$REPO/infra/forge/root-tree.sh" refresh 2>&1)" || tree_rc=$?
    printf 'cluster-deploy-runner: %s\n' "$tree_line"
    if [ "$tree_rc" -ne 0 ]; then
        echo "cluster-deploy-runner: root's tree could not be brought to the forge's main (root-tree.sh refresh exited $tree_rc, the line above says why) — nothing built or rolled" >&2
        exit 1
    fi
fi

# RUN FROM A VIEW OF THE GENERATION THAT IS MAIN, AND ONLY FROM THAT.
# Two things, in one hop. The script that runs is HEAD's own (David
# accepted (a) on d0b5efd4): this process may have started from an older
# generation — the unit's `current` moved under it, or the refresh above
# just moved it — and a change to this script must run on its own train's
# converge, not the next one's. And it runs from a VIEW: a converge is up
# to ninety minutes, root-tree.sh keeps only the generations `current`,
# `previous` and `good` name, and two merges in one cold build would
# delete the tree this run reads its manifests, renderers and checks
# from after the build. The view is the generation hard-linked under
# CONVERGE_STATE — the same files, which nothing writes, so it costs no
# space and no copy — and it outlives any prune. BOSS_RUNNER_VIEW is the
# guard against a second hop.
#
# EACH RUN'S VIEW IS ITS OWN (review 1ba2873d, N2): `run-tree.<pid>`.
# One fixed `run-tree` let a run started by hand and a timer's run each
# remove the tree the other was executing from. A run removes only the
# views of runs that are no longer alive (no /proc entry for the pid; exec
# keeps the pid, so the view's stage is the same process), and never its
# own.
#
# AND IT CARRIES THE GENERATION'S HISTORY (review 1ba2873d, B1). A
# generation is an export with no .git, and two stages below read git in
# the tree they run from: the orphan check walks every manifest the
# history deleted, and registry-drift reads which side is ahead off the
# history and HEAD. In a bare export the first walked nothing and
# recorded `orphans_check: ok`, and the second filed every kind it could
# not place as "side not measured" on a "shallow" checkout. So the view
# gets a .git of its own — beside the hard links, never in the generation
# or the store: its objects are root's store (repo.git, a full fetch, by
# alternates — no copy), its HEAD is the generation's sha, its index is
# that commit's tree. Root made every part of it.
if [ -z "${BOSS_RUNNER_VIEW:-}" ]; then
    STAGE="tree view"
    gen="$(BOSS_ROOT_TREE="$ROOT_TREE" bash "$REPO/infra/forge/root-tree.sh" path)"
    gen_sha="$(cat -- "$gen/.boss-tree-sha")"
    [[ "$gen_sha" =~ ^[0-9a-f]{40,64}$ ]] || { echo "cluster-deploy-runner: $gen/.boss-tree-sha does not name a commit — not a generation of root's tree; nothing built or rolled" >&2; exit 1; }
    install -d -m 0700 -- "$CONVERGE_STATE"
    for old in "$CONVERGE_STATE"/run-tree.*; do
        [ -e "$old" ] || continue
        pid="${old##*/run-tree.}"
        pid="${pid%.new}"
        [[ "$pid" =~ ^[0-9]+$ ]] || continue
        [ "$pid" != "$$" ] && [ ! -d "/proc/$pid" ] && rm -rf -- "$old"
    done
    view="$CONVERGE_STATE/run-tree.$$"
    rm -rf -- "$view.new" "$view"
    cp -al -- "$gen" "$view.new"
    view_git() { GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null -C "$view.new" "$@"; }
    view_git init -q --template= \
        && install -d -m 0755 -- "$view.new/.git/objects/info" \
        && printf '%s\n' "$ROOT_TREE/repo.git/objects" > "$view.new/.git/objects/info/alternates" \
        && view_git update-ref --no-deref HEAD "$gen_sha" \
        && view_git read-tree HEAD \
        || { echo "cluster-deploy-runner: the view of ${gen_sha:0:12} could not be given its history from $ROOT_TREE/repo.git — the orphan check and the drift phase would read git in a tree with none; nothing built or rolled" >&2; exit 1; }
    view_git update-index -q --refresh >/dev/null 2>&1 || true
    mv -T -- "$view.new" "$view"
    BOSS_RUNNER_VIEW="$view" exec bash "$view/infra/forge/cluster-deploy-runner.sh" "$@"
fi
cd "$REPO"
# The image tag is a NAME, seven characters of the full sha — not
# `git rev-parse --short`, whose length git picks from the object count
# (core.abbrev auto) and which grew to eight on this history on
# 2026-09-17: the cluster ran boss:3b73d515 while the CLI installer on
# boss-gcp pulled boss:3b73d51 (${SHA:0:7}, install-cli-from-image.sh)
# and every converge there failed its CLI step (backlog 7f4a5a3c). A
# tag has no ambiguity to resolve; a test pins the two expressions. The
# full sha is the one the generation says it is (root-tree.sh writes
# .boss-tree-sha into every generation before it is named).
STAGE="read head"
HEAD_FULL=$(cat .boss-tree-sha)
[[ "$HEAD_FULL" =~ ^[0-9a-f]{40,64}$ ]] || { echo "cluster-deploy-runner: $REPO/.boss-tree-sha does not name a commit — not a generation of root's tree; nothing built or rolled" >&2; exit 1; }
HEAD=${HEAD_FULL:0:7}
# WHAT THIS RUN EXECUTED, AND AS WHOM, on every tick's packet: the
# generation of root's tree and the account (forge-converge records its
# own `runs_from` the same way). A reader off the host tells a root run
# from the tree apart from the unit before this car by this line alone.
run_summary_field runs_from "root's tree $HEAD_FULL as $(id -un 2>/dev/null || id -u)"
# The converge's own stamps came from david's home until this car; its
# first run as root carries them across, once (the lib says why it is
# this run and not the installer).
carry_converge_state
LAST=$(cat "$STAMP_FILE" 2>/dev/null || echo none)

# THE ADMIN KUBECONFIG DAVID PLACED (backlog fb444bbb; ops-credentials.sh
# ops_kubeconfig), not ~/kc.yaml, a second copy nothing checked. Root can
# ask whether it is there, so it asks before the first `-v` would mount
# it: docker makes an absent `-v` source into a root-owned DIRECTORY,
# which would stand in the way of the file David places next.
. "$REPO/infra/estate/ops-credentials.sh"
KUBECONFIG_PATH="${BOSS_FORGE_KUBECONFIG:-$(ops_kubeconfig)}"
require_cluster_credentials() {
    if [ ! -f "$KUBECONFIG_PATH" ] || [ -L "$KUBECONFIG_PATH" ]; then
        STAGE="kubeconfig"
        echo "cluster-deploy-runner: the admin kubeconfig $KUBECONFIG_PATH is not a file of its own (root material, placed by David; design 835c0c9c) — the cluster cannot be read or written; nothing built or rolled" >&2
        return 1
    fi
}
# THE SYSTEM DAEMON, EXPLICITLY: whatever DOCKER_HOST the environment
# carried, every docker below is root's own daemon.
unset DOCKER_HOST
# kubectl, through the alpine/k8s container with the admin kubeconfig.
# Defined before the unchanged check below because the no-op tick
# READS the cluster too (0b7804f3); kubectl_seeing DIR is the same
# kubectl with a rendered directory mounted read-only at /manifests —
# the apply directory on a deploy, a throwaway render on a no-op tick.
# Every kubectl container here runs with `--log-driver=none`: a Secret
# it reads (the shared-Secret copy in cluster-deploy-lib.sh) reaches this
# script through the attach and never the docker daemon's log (backlog
# aa700921, a_kubectl_container_keeps_no_log.rs).
K="docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
kubectl_seeing() { # DIR
    echo "docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro -v $1:/manifests:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
}

if [ "$HEAD" = "$LAST" ]; then
    require_cluster_credentials
    echo "cluster-deploy-runner: forge main unchanged ($HEAD)"
    run_summary_field unchanged "$HEAD"
    # THE NO-OP TICK OBSERVES THE CONNECTOR TOO (backlog 0b7804f3;
    # measured 2026-09-16 13:38Z). `cloudflared` and `tunnel_ingress`
    # were recorded only by the `tunnel connector` stage below, so the
    # newest packet that carried them was the last DEPLOY, and on a
    # quiet morning retire-cloudflared refused David's --for-real over
    # a 194-minute-old observation with no way to refresh it but a
    # train (ops-request 7d05cb04). A liveness fact is observed on
    # every tick. The same lib function records the same fields
    # (cluster-deploy-lib.sh observe_connector), from the same inputs
    # the deploying tick has: the tree rendered to a throwaway
    # directory (the tree here IS the converged one — the generation
    # the stamp names), and the skipped set the secret gate decides
    # now, read-only.
    # An observation tick writes nothing to the cluster: no apply, no
    # roll, no declared-secrets create, no ConfigMap patch — kubectl
    # reads only (get, a client dry run, rollout status), and a test
    # holds the block to that. A gate read the credential cannot make
    # FAILS the tick, exactly as it fails the deploying tick's apply
    # loop: no skipped set is knowable, so no ingress map is claimed
    # and nothing is recorded — a guessed map would be evidence the
    # retire verb reads. The connector read itself never fails a tick
    # (connector_status): the tunnel is not what a converge delivers.
    observe_deployed_surfaces
    # And the tenant site, for the same reason (backlog e114238a): the
    # tenant's publish loop closes on the newest converge's record of
    # what the gateway serves, and a fact that exists only when a train
    # ships is the wrong shape for it. `get svc` is the one kubectl
    # read; the page itself is read over HTTP, never through the tunnel.
    OUTCOME="converged=$HEAD (unchanged)"
    exit 0
fi
_stage_started=$(date +%s)

# A head that bricked its boot stays quarantined until main moves.
# Without this, the 2026-09-02 shape loops forever: rollout fails,
# set -e kills the run before the stamp, and the next tick rebuilds
# and re-rolls the SAME brick every ten minutes — which is exactly
# what the runner spent the 20:15-21:10 outage doing. Exit NONZERO:
# a held converge is a failed unit somebody can see, not a quiet pass.
if [ "$HEAD" = "$(cat "$FAILED_FILE" 2>/dev/null || echo none)" ]; then
    echo "cluster-deploy-runner: $HEAD bricked its boot on a previous converge — holding until main moves (rm $FAILED_FILE to retry it)" >&2
    STAGE="quarantined $HEAD"
    exit 1
fi

# An operator's hold (converge-hold.sh, the hold-converge ops verb)
# stops the roll before the build: main has moved, and a human said not
# yet. Loud on every tick, exit 0 — a hold is a decision, not a failure.
# HOLD_FILE is forge-defaults.sh's, sourced above: this line respelled
# the default until backlog d94d287e, so moving the hold would have left
# the runner reading the old path while the verb wrote the new one.
#
# WHAT A HOLD STOPS, SAID ON THE TICK (delta review aa901496, N1;
# backlog 62b09c57): the build and the roll — the cluster stays on the
# build it is on. Root's tree follows main whatever the hold says (the
# refresh above, and forge-converge's), so the tree is never what a hold
# keeps back. converge_hold_report (the lib) prints the HELD line, names
# the build the cluster stays on and the main it was not rolled to, puts
# the hold and its age on this tick's packet, and raises one alert for a
# hold left standing past its bound.
if reason=$(converge_held "$HOLD_FILE"); then
    converge_hold_report "$HOLD_FILE" "$reason" "$HEAD_FULL" "$LAST"
    OUTCOME="converge_held=$reason"
    exit 0
fi
# Complete an eligible repository/registry delivery where it actually goes live.
# This is before the image login/build/roll and after quarantine and operator
# holds. Missing qualification retains the existing software route.
STAGE="delivery channel"
. "$REPO/infra/forge/delivery-stage.sh"
. "$REPO/infra/estate/cli-compatible.sh"
delivery_rc=0
delivery_channel_stage "$REPO" "$HEAD_FULL" "$LAST" "$(cli_host_store)" \
    "$REPO/infra/forge/root-tree.sh" || delivery_rc=$?
case "$delivery_rc" in
    0|75)
        # Agent images retain their own input-equality publication protocol;
        # skipping a core image does not drop its per-generation witness.
        . "$REPO/infra/agent/publication-stage.sh"
        STAGE="agent image publication"
        agent_image_publication_stage "$REPO/infra/agent/publish-image.sh" "$HEAD_FULL" "$CONVERGE_STATE"
        STAGE="data side observations"
        observe_data_surfaces "$CONVERGE_STATE"
        if [ "$delivery_rc" -eq 0 ]; then
            OUTCOME="delivery_arrived=$HEAD_FULL (data; actual image remains $LAST)"
        else
            OUTCOME="delivery_pending=$HEAD_FULL (native publication/equality; actual image remains $LAST)"
        fi
        exit 0
        ;;
esac
require_cluster_credentials
echo "cluster-deploy-runner: forge main moved $LAST -> $HEAD; building"

# THE PUSH LOGIN IS THE BROKER'S, AND IT IS THERE BEFORE ANYTHING IS
# BUILT (backlog 683d7fe9). The forge converge deposits it root-only from
# Secret boss/forge-registry-push-token and proves it by a login the
# registry accepts on every pass (registry-token-deposit.sh; its packet
# reads registry_token_effect). Absent, the push would fail an hour into
# a cold build; so the run stops here and says which file and who fills
# it. It rides only the commands that talk to the registry — the mirror,
# the push, the tag verification — as DOCKER_CONFIG, never the whole
# run: a build's buildx state does not belong in the deposit's directory.
STAGE="registry login"
REGISTRY_LOGIN="$REGISTRY_PUSH_DIR/config.json"
if [ ! -f "$REGISTRY_LOGIN" ] || [ -L "$REGISTRY_LOGIN" ]; then
    echo "cluster-deploy-runner: no registry push login at $REGISTRY_LOGIN — the forge converge deposits it from the broker's Secret boss/forge-registry-push-token (registry-token-deposit.sh; a rotate-a-credential packet of forge-registry-push-token mints it), so $HEAD is not built or rolled until that has happened" >&2
    exit 1
fi
# The FULL commit rides into the image's environment (the runtime
# stage's ENV; Capabilities.commit reads it at process start) so the
# conductor's `converged` step can verify the running pod serves this
# exact merge — the short tag stays the image name, the full sha is
# the attestation (prefix-compared, so either length matches). It is
# NOT compiled in: that made every train recompile every crate.
STAGE="build $HEAD"
# A FAILED CONVERGE SAYS WHY.
#
# This ran `docker build -q`. On 2026-09-09 train 283 merged at 03:40
# and its converge failed four times — 03:46, 03:55, 04:04, 04:14 —
# and the record held the Dockerfile context dump, a one-line ERROR,
# and NOTHING the compiler said. Establishing even the exit code meant
# reading an untruncated journal line by hand; ruling out a compile
# error and a full disk took a workspace check on another box and a
# disk-report ops-request, and neither question should have needed
# asking. The build knew the answer and was told not to speak
# (backlog ddb0f7bd).
#
# `-q` suppresses OUTPUT, not work: it saves no build time, so the
# only thing it was buying was a quiet journal. Capturing to a file
# buys that too, and keeps the evidence. So the build always runs
# verbose into a log, the log is printed ONLY on failure, and the
# first failure is readable instead of the second.
#
# The stamp is the other half. The runner is stateless across timer
# firings, so without it a head that fails four times looks like four
# unrelated failures; with it the log says which attempt this is. It
# is deliberately NOT a quarantine — unlike FAILED_FILE, which holds
# an unbootable head out, a build that failed is retried, because the
# common causes here (a registry fetch losing a DNS race on this LAN,
# memory contention with a concurrent CI job) are transient and a
# retry that PASSES is itself the finding.
# MIRROR WHAT THE TREE LISTS BEFORE BUILDING. The Dockerfile's
# `COPY --from=10.20.0.15:3000/david/…` and the manifests' images are
# forge tags the mirror list declares; the registry holding them is a
# separate fact, and on 2026-09-13 the gap cost two hours of unconverged
# trains (see mirror-base-images.sh --missing). Best-effort: a mirror
# that cannot complete is said, and the build runs anyway — a base the
# registry still lacks names itself in the build log.
if [ -x infra/forge/mirror-base-images.sh ]; then
    if ! infra/forge/mirror-base-images.sh --missing 2>&1 | sed 's/^/cluster-deploy-runner: /'; then
        echo "cluster-deploy-runner: mirror --missing did not complete — building anyway; a base the registry lacks will name itself in the build log"
    fi
fi
BUILD_FAILED_FILE="${BOSS_FORGE_LAST_BUILD_FAILED:-$CONVERGE_STATE/last-build-failed}"
BUILD_ATTEMPT="first attempt"
if [ "$HEAD" = "$(cat "$BUILD_FAILED_FILE" 2>/dev/null || echo none)" ]; then
    BUILD_ATTEMPT="RETRY — this head already failed to build at least once"
fi
BUILD_LOG="$(mktemp -t boss-converge-build.XXXXXX)"
echo "cluster-deploy-runner: building $HEAD ($BUILD_ATTEMPT)"
_build_started=$(date +%s)
if docker build -f infra/oss-quickstart/Dockerfile \
    --build-arg BOSS_BUILD_COMMIT="$HEAD_FULL" \
    -t "$REGISTRY:$HEAD" . > "$BUILD_LOG" 2>&1; then
    rm -f "$BUILD_FAILED_FILE" "$BUILD_LOG"
    # Stamped here, in the block the build-log test lifts verbatim, with
    # the lib's own function rather than a helper the block cannot see.
    run_summary_field build_s "$(( $(date +%s) - _build_started ))"
    run_summary_field build_head "$HEAD"
    _stage_started=$(date +%s)
else
    build_rc=$?
    echo "$HEAD" > "$BUILD_FAILED_FILE"
    # THE FAILURE, NOT THE TAIL. Capturing the log was half the fix and
    # the half I got wrong first: BuildKit prints its epilogue — the
    # whole failing RUN's Dockerfile context, some 35 lines — AFTER the
    # step's own output, so `tail` showed the recipe and hid the
    # compiler. Measured 2026-09-09 05:12, on the first failure this
    # capture ever saw. That is precisely the defect CLAUDE.md's
    # Diagnosis section names, committed inside the fix for it.
    #
    # So scan for what a failure actually prints and show the window
    # around the FIRST one, falling back to the tail only when nothing
    # matches — and say which of the two is on screen, because a tail
    # presented as a diagnosis is how this started.
    lines="${BOSS_BUILD_LOG_TAIL:-80}"
    marker=$(grep -nEm1 'error\[E|^error:|error: could not compile|panicked at|cannot find|No space left|Killed|signal: 9|FATAL:' \
                 "$BUILD_LOG" 2>/dev/null | cut -d: -f1 || true)
    if [ -n "$marker" ]; then
        start=$(( marker > 20 ? marker - 20 : 1 ))
        echo "cluster-deploy-runner: BUILD FAILED for $HEAD (exit $build_rc, $BUILD_ATTEMPT) — the failure, with context, from line $marker of the build log" >&2
        sed -n "${start},$(( start + lines ))p" "$BUILD_LOG" >&2
    else
        echo "cluster-deploy-runner: BUILD FAILED for $HEAD (exit $build_rc, $BUILD_ATTEMPT) — nothing in the log named a failure, so this is the last $lines lines, not a diagnosis" >&2
        tail -n "$lines" "$BUILD_LOG" >&2
    fi
    echo "cluster-deploy-runner: (full build log kept at $BUILD_LOG)" >&2
    exit "$build_rc"
fi
STAGE="push $HEAD"
DOCKER_CONFIG="$REGISTRY_PUSH_DIR" docker push "$REGISTRY:$HEAD"
_stage_done push_s

# The image proves it can boot before it goes anywhere near the cluster
# (cluster-deploy-lib.sh image_boots): its own launcher checks that
# every file it sources is beside it. A head that fails here is
# quarantined like a head that failed on the cluster — with no dark
# window at all, because nothing was applied.
. "$REPO/infra/forge/cluster-deploy-lib.sh"
STAGE="boot check $HEAD"
if ! image_boots docker "$REGISTRY:$HEAD"; then
    echo "$HEAD" > "$FAILED_FILE"
    echo "cluster-deploy-runner: $HEAD fails its own boot check — not rolling it; quarantined (rm $FAILED_FILE to retry it)" >&2
    exit 1
fi

# THE CONVERGE CLEANS UP AFTER ITSELF.
#
# Every run builds a 1.07 GB image and pushes it, and nothing ever
# removed the local copy. MEASURED 2026-08-29 on the minipc: 42 boss
# tags resident, ~45 GB, on a 228 G disk that had reached 93% full —
# the host that also serves the forge, the OCI registry and the CI
# runner, so image growth competes with the registry it feeds. The
# daily disk-headroom and stale-build-cache sweeps had each been
# opening a packet about it for three days (`0e62f404`, `b99e9627`).
#
# AFTER THE PUSH, DELIBERATELY. The push is what makes a local copy
# redundant, so cleanup that ran before it could delete the only copy
# of something. `set -e` means a failed push never reaches this.
#
# The deletion loop itself — keep the N newest, VERIFY a candidate is
# present in the registry before rmi, never touch `latest` — is the
# SHARED definition in prune-registry-tags.lib.sh, sourced from the
# generation this run builds (we cd'd to $REPO above). disk-floor-sweep.sh runs
# the identical loop below the disk floor; two copies of a loop that
# deletes images is the drifting pair §9a bans, so the rationale for
# why verification is load-bearing lives in the lib's header now.
# Checked while extracting this: all 42 resident tags were in the
# registry, so this is hygiene rather than recovery from a divergence.
# The verification asks the registry with the push login, as the push did.
. "$REPO/infra/forge/prune-registry-tags.lib.sh"
STAGE="prune images"
DOCKER_CONFIG="$REGISTRY_PUSH_DIR" prune_registry_verified_tags "$REGISTRY" "${BOSS_RUNNER_KEEP_IMAGES:-5}" cluster-deploy-runner

# Build cache is regenerable by definition, so the only cost of being
# wrong here is a slower next build. Age-filtered rather than emptied:
# a week keeps the layers a rebuild actually reuses and drops the rest.
# `--keep-storage` is gone in Docker 29; `--filter until=` is the
# supported spelling.
docker builder prune -f --filter until=168h >/dev/null 2>&1 || true
echo "cluster-deploy-runner: build cache pruned (older than 168h)"

# Cluster config converges with the code: apply the tree's manifests
# idempotently (secrets are referenced by name and stay out-of-tree).
# Apply comes BEFORE the image roll so the tag built above — not the
# placeholder tag committed in boss.yaml — is what the cluster ends
# on. A failed apply aborts here (set -e): no stamp is written, the
# next timer run retries.
# ONE SOURCE, RENDERED PER INSTANCE (backlog 07d7549c; design ffc83387,
# David 2026-09-16: the playground is a second namespace on this
# cluster, and both it and prod converge on every train). The
# manifests directory is written for prod; infra/cluster/instances.toml
# names every instance and its four parameters, and render-instance.sh
# emits each one into its own directory — prod's byte-identical to the
# tree (a test holds it so), every other with namespace, tenant, sim
# flag, hostname and the in-cluster DNS names substituted. The roster
# infra/cluster/instance-manifests.txt says which manifests exist per
# instance and which exist once for the pipeline; a manifest it does
# not classify REFUSES the render, so a partial apply never reads as a
# full one. The image-sha substitution below is unchanged and runs on
# every rendered directory.
STAGE="render instances"
RENDER_DIR="$(mktemp -d -t cluster-deploy-render.XXXXXX)"
APPLY_DIR="$(mktemp -d -t cluster-deploy-manifests.XXXXXX)"
"$REPO/infra/cluster/render-instance.sh" --all "$RENDER_DIR"
INSTANCES=$("$REPO/infra/cluster/render-instance.sh" --instances)
SOURCE_NS=$("$REPO/infra/cluster/render-instance.sh" --source)
# The source instance's row — its name and its tenant source — read
# off the same list every other instance is read from.
SOURCE_NAME=""; SOURCE_TENANT_REPO=""; SOURCE_TENANT_REF=""; SOURCE_TENANT_DIR=""; SOURCE_SITE=""
# 31d76e37: dev work growth belongs to its exact-plan approval, never to
# converge. Preserve the current bounded request in the staged PVC only;
# every Deployment/template byte is unchanged. A dark/denied read aborts,
# an absent claim keeps the reviewed 40Gi bootstrap, and >64Gi refuses.
STAGE="preserve dev work volume request"
$K get pvc boss-dev-work -n boss-dev --ignore-not-found=true -o json --request-timeout=20s \
    > "$APPLY_DIR/dev-work-claim.json"
python3 "$REPO/infra/forge/dev-work-volume.py" --render-converge \
    "$RENDER_DIR/$SOURCE_NS/boss-dev.yaml" "$APPLY_DIR/dev-work-claim.json"
if [ -s "$APPLY_DIR/dev-work-claim.json" ]; then
    run_summary_json dev_work_claim "$(cat "$APPLY_DIR/dev-work-claim.json")"
else
    run_summary_json dev_work_claim '{"absent":true,"bootstrap":"40Gi"}'
fi
# The deposit template is generic; the deployment's existing ConfigMap
# and CronJob are its authoritative nonsecret inputs (877ad378). Resolve
# both before image staging or ANY apply. Record immutable read identities
# and complete-body hashes through this converge's existing durable door.
STAGE="deposit configuration"
deposit_config_render "$K" "$SOURCE_NS" \
    "$RENDER_DIR/$SOURCE_NS/boss-break-glass-deposit.yaml" \
    "$RENDER_DIR/$SOURCE_NS/boss-break-glass-deposit.yaml" \
    "$REPO/infra/cluster/render-deposit-config.sh" "$APPLY_DIR/deposit-config.json"
run_summary_json deposit_config "$(cat "$APPLY_DIR/deposit-config.json")"
# The applied copy carries the build that is ALREADY converged (the
# stamp), not the manifest's placeholder tag: the apply must never
# change what runs. Rolling to $HEAD is roll_deployment's job below.
while IFS="$IFS_ROW" read -r iname ins_ns tdir _s _h _share trepo tref site; do
    manifests_with_image "$RENDER_DIR/$ins_ns" "$APPLY_DIR/$ins_ns" "$REGISTRY" "$LAST"
    if [ "$ins_ns" = "$SOURCE_NS" ]; then
        SOURCE_NAME="$iname"; SOURCE_TENANT_DIR="$tdir"; SOURCE_TENANT_REPO="$trepo"; SOURCE_TENANT_REF="$tref"; SOURCE_SITE="$site"
    fi
done <<< "$(instance_rows "$INSTANCES")"
rm -rf "$RENDER_DIR"
KM=$(kubectl_seeing "$APPLY_DIR")
# Client-only conversion of the image-pinned, reviewed source. No live
# Secret values are read, and this is not an API create. Both config
# writes then use explicit UID/resourceVersion tests. Ordinary apply
# receives neither document after success; a conflict stops beforehand.
KAPPLY="docker run --rm --log-driver=none -i --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
$KM create --dry-run=client --validate=false -f "/manifests/$SOURCE_NS/boss-break-glass-deposit.yaml" -o json > "$APPLY_DIR/deposit-template-documents.json"
deposit_config_source "$APPLY_DIR/$SOURCE_NS/boss-break-glass-deposit.yaml" \
    "$APPLY_DIR/deposit-template-documents.json" "$APPLY_DIR/deposit-template.json"
deposit_apply_rc=0
deposit_config_apply "$KAPPLY" "$SOURCE_NS" "$APPLY_DIR/$SOURCE_NS/boss-break-glass-deposit.yaml" \
    "$APPLY_DIR/deposit-template.json" "$APPLY_DIR/deposit-config.json" || deposit_apply_rc=$?
run_summary_json deposit_config "$(cat "$APPLY_DIR/deposit-config.json")"
[ "$deposit_apply_rc" -eq 0 ] || exit "$deposit_apply_rc"
# apply_instance NAMESPACE — the rendered directory for one instance.
# The files that declare a Namespace go FIRST: `kubectl apply -f DIR`
# walks files alphabetically, so on a namespace's first converge every
# CronJob sorted before boss.yaml would meet "namespace not found",
# the apply would exit 1 after creating the namespace, and only the
# NEXT converge would succeed. A second apply of an unchanged file is
# a no-op, so prod pays nothing for the same ordering.
# apply_namespaces NAMESPACE — the Namespace files alone. Also what
# provisioning runs before it creates an instance's Secrets IN that
# namespace (dc1bc724): the one definition of "the Namespace first".
apply_namespaces() {
    local ns="$1" f
    for f in $(grep -l '^kind: Namespace$' "$APPLY_DIR/$ns"/*.yaml); do
        $KM apply -f "/manifests/$ns/${f##*/}"
    done
}
apply_instance() {
    local ns="$1"
    apply_namespaces "$ns"
    $KM apply -f "/manifests/$ns"
}
# A generic image ships an explicit empty declaration. This estate's
# declaration is delivered separately, without baking it into the SPA.
# The whole-directory mount observes ConfigMap updates; never subPath.
converge_dev_door() {
    local ns="$1" stage="$APPLY_DIR/dev-door-$1"
    bash "$REPO/infra/estate/render-dev-door-config.sh" \
        "$REPO/infra/estate/dev-door.json" "$stage" "$ns"
    $KAPPLY apply -f - < "$stage/configmap.json"
}
# `-i`, and that single flag is the whole bug this replaces. The first
# version piped the generated ConfigMap into `$K apply -f -`, but $K is
# `docker run --rm` with no `-i`, so the container never attached
# stdin: apply read an empty document, said "error: no objects passed
# to apply", and the generator died with "write /dev/stdout: broken
# pipe". The runner has failed on every tick since — silently, because
# a failed systemd oneshot notifies nobody — leaving the cluster on the
# placeholder tag committed in boss.yaml while forge main moved on.
#
# The lesson is the one from the zsh/bash mixup earlier the same day:
# a command validated in a different environment than the one that
# runs it has not been validated. I checked the kubectl invocation
# against my own kubectl and never against the docker wrapper it
# actually runs through.
# KAPPLY is defined above, before conditional deposit configuration.

# THE TENANT SOURCE PER INSTANCE (backlog f4f5c387, car 2 of fcc1d57b;
# David 2026-09-16 'Let's do it'). An instance's tenant is a directory
# the image ships (`tenant_dir`) or a repository on the forge
# (`tenant_repo` + `tenant_ref`) that this converge checks out under
# TENANTS_DIR and delivers as the `boss-tenant` ConfigMap in the
# instance's namespace, mounted at /opt/boss/tenant/seeds — the
# step-plugins mechanism, a generated object, never a committed
# manifest (§9a). The tenant is read the way root's tree is: from the
# forge's own repository on this host's disk, served by that
# repository's owner, with no credential at all (cluster-deploy-lib.sh
# tenant_repo_path; backlog 683d7fe9 — it was read with the checkout
# owner's forge token, which root does not hold).
#
# tenant_source_verdict NAME NS TDIR TREPO TREF — MEASURES the source
# and prints the packet's `tenant_source` entry for one instance:
#   <ns>: image (<dir>)                   an image-sourced instance
#   <ns>: readable (<repo>@<ref> <sha>)   ls-remote answered the ref
#   <ns>: unreadable (<repo>@<ref>)       it did not — rc 1
# so the first converge after a flip answers whether the repository and
# its ref are there instead of a person guessing. rc 2 when TREPO is not
# an owner/name the forge could hold.
tenant_source_verdict() {
    local iname="$1" ns="$2" tdir="$3" trepo="$4" tref="$5" sha rc=0
    if [ -z "$trepo" ]; then
        printf '%s: image (%s)' "$ns" "$tdir"
        return 0
    fi
    sha=$(tenant_source_check "$trepo" "$tref") || rc=$?
    case "$rc" in
        0) printf '%s: readable (%s@%s %s)' "$ns" "$trepo" "$tref" "${sha:0:8}" ;;
        1) printf '%s: unreadable (%s@%s)' "$ns" "$trepo" "$tref"; return 1 ;;
        *) echo "cluster-deploy-runner: cannot derive where $trepo lives on this host — it is not an owner/name" >&2; return 2 ;;
    esac
}
# converge_tenant NAME NS TREPO TREF SITE — a repo-sourced instance's
# tenant: a fresh shallow checkout under $TENANTS_DIR/<name>, staged
# flat (tenant_stage) and applied as ConfigMap boss-tenant in NS from a
# client dry run, exactly as converge_step_plugins below applies the
# bundles. Nothing to do for an image-sourced instance. A checkout or
# stage that fails FAILS this stage: readability was measured first,
# so what is left is a defect, not a missing scope.
#
# THE SITE rides the same checkout (design b64c4377; backlog c8f6b233).
# When the instance declares a SITE hostname, the checkout's site/ is
# staged flat (site_stage) and applied as ConfigMap boss-site in NS —
# the directory the gateway serves under that hostname, mounted at
# BOSS_SITE_DIR. A checkout with no site/ yet applies an EMPTY
# ConfigMap (the site answers 404, the instance converges) and the
# packet says so: `site_source: <ns>: <site> (<n> files from
# <repo>@<ref>)` or `… (no site/ in <repo>@<ref> — nothing staged,
# the site answers 404)`. An instance without a site stages nothing
# and records nothing.
#
# THE CHECK BEFORE THE APPLY (backlog 1af5119d, 2026-09-19; the lib's
# tenant_check says why). The checkout is judged with `boss tenant
# check` before anything is staged, and only a PASS reaches the
# ConfigMap. A refusal — or a check that could not run — keeps the
# previous ConfigMap, records `tenant unchanged: …` on this packet as
# `tenant_check`, files a packet naming the file and line, and RETURNS
# 0: the software converges and the train arrives; the tenant did not
# change, and the record says so. The site rides on regardless — it is
# a directory the gateway serves, not one it boots from.
SITE_SOURCE=""
TENANT_CHECK=""
converge_tenant() {
    local iname="$1" ns="$2" trepo="$3" tref="$4" site="${5:-}" dir stage kt sstage n from where check_rc=0 deliver=1
    [ -n "$trepo" ] || return 0
    dir="$TENANTS_DIR/$iname"
    stage="$TENANTS_DIR/$iname.configmap"
    mkdir -p "$TENANTS_DIR"
    tenant_checkout "$trepo" "$tref" "$dir"
    where=$(tenant_check "$dir" "$TENANTS_DIR/$iname.check") || check_rc=$?
    case "$check_rc" in
        0)
            TENANT_CHECK="${TENANT_CHECK:+$TENANT_CHECK; }$ns: checked ($trepo@$tref)" ;;
        1)
            deliver=0
            echo "cluster-deploy-runner: boss-tenant in $ns UNCHANGED — boss tenant check refused $trepo@$tref at $where; the previous ConfigMap stays and the pod never sees this manifest" >&2
            TENANT_CHECK="${TENANT_CHECK:+$TENANT_CHECK; }$ns: tenant unchanged: check refused $where ($trepo@$tref)"
            tenant_check_alert "tenant unchanged in $ns: boss tenant check refused $trepo@$tref at $where" \
                "The cluster converge checked out $trepo@$tref for instance $iname ($ns) and boss tenant check refused it at $where, so the boss-tenant ConfigMap was NOT applied — the previous one stays and the pod boots from it (a refused manifest would take the system of record down: Recreate, one container). Fix the file in the tenant repository; the next converge delivers it. The verb's report: $(cat "$TENANTS_DIR/$iname.check" 2>/dev/null || echo 'in the cluster-deploy-runner journal')" ;;
        *)
            deliver=0
            echo "cluster-deploy-runner: boss-tenant in $ns UNCHANGED — the tenant $trepo@$tref could NOT be checked (tenant_check rc $check_rc; boss is not on this host, or died without a verdict); an unchecked manifest is not applied" >&2
            TENANT_CHECK="${TENANT_CHECK:+$TENANT_CHECK; }$ns: tenant unchanged: could not check ($trepo@$tref) — boss is not on this host or gave no verdict (tenant_check rc $check_rc)"
            tenant_check_alert "tenant unchanged in $ns: could not check $trepo@$tref (boss tenant check did not run)" \
                "The cluster converge checked out $trepo@$tref for instance $iname ($ns) but could not run boss tenant check on it (rc $check_rc: ${BOSS_CLI:-boss} is not on this host, or died without a verdict), so the boss-tenant ConfigMap was NOT applied — an unchecked manifest never reaches the pod. install.sh installs the tree's CLI from the image on each forge converge; read the cluster-deploy-runner journal." ;;
    esac
    run_summary_field tenant_check "$TENANT_CHECK"
    if [ "$deliver" = 1 ]; then
        rm -rf "$stage"
        tenant_stage "$dir" "$stage"
        kt="docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro -v $stage:/tenant:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
        echo "cluster-deploy-runner: converging the boss-tenant ConfigMap in $ns from $trepo@$tref ($(ls "$stage" | wc -l) files)"
        $kt create configmap boss-tenant -n "$ns" --from-file=/tenant \
            --dry-run=client -o yaml | $KAPPLY apply -f -
    fi
    [ -n "$site" ] || return 0
    sstage="$TENANTS_DIR/$iname.site.configmap"
    n=$(site_stage "$dir" "$sstage")
    # `--from-file` only when there is a file to take: kubectl refuses
    # an empty directory, and an empty ConfigMap is the honest object
    # for a tenant with no site/ yet.
    from=""
    if [ "$n" -gt 0 ]; then from="--from-file=/site"; fi
    kt="docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro -v $sstage:/site:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
    echo "cluster-deploy-runner: converging the boss-site ConfigMap in $ns for $site from $trepo@$tref/site ($n files)"
    # shellcheck disable=SC2086
    $kt create configmap boss-site -n "$ns" $from \
        --dry-run=client -o yaml | $KAPPLY apply -f -
    if [ "$n" -gt 0 ]; then
        SITE_SOURCE="${SITE_SOURCE:+$SITE_SOURCE; }$ns: $site ($n files from $trepo@$tref)"
    else
        SITE_SOURCE="${SITE_SOURCE:+$SITE_SOURCE; }$ns: $site (no site/ in $trepo@$tref — nothing staged, the site answers 404)"
    fi
    run_summary_field site_source "$SITE_SOURCE"
}
# The source instance first: its verdict rides the packet as
# `tenant_source` before anything is applied, and an unreadable source
# FAILS the converge here — prod's manifests are not applied and
# nothing is rolled, the stage names itself (`failed_stage`), and the
# next tick retries once the scope exists. A skip is a state the other
# instances have; the source has none (render-tunnel-config.sh: a
# skipped source is a caller's error). Prod is image-sourced in this
# car, so this is the identity until David flips it.
STAGE="tenant $SOURCE_NS"
TENANT_SOURCE=""
verdict_rc=0
verdict=$(tenant_source_verdict "$SOURCE_NAME" "$SOURCE_NS" "$SOURCE_TENANT_DIR" "$SOURCE_TENANT_REPO" "$SOURCE_TENANT_REF") || verdict_rc=$?
TENANT_SOURCE="$verdict"
run_summary_field tenant_source "$TENANT_SOURCE"
if [ "$verdict_rc" -ne 0 ]; then
    echo "cluster-deploy-runner: the source instance's tenant is not readable ($verdict) — not applying $SOURCE_NS; nothing rolled" >&2
    exit 1
fi
echo "cluster-deploy-runner: tenant source: $verdict"
converge_tenant "$SOURCE_NAME" "$SOURCE_NS" "$SOURCE_TENANT_REPO" "$SOURCE_TENANT_REF" "$SOURCE_SITE"
STAGE="apply manifests"
echo "cluster-deploy-runner: applying infra/cluster/manifests for $SOURCE_NS (boss image pinned to the converged $LAST)"
# SAY WHAT THE APPLY DOES NOT DO. `kubectl apply` with no `--prune` is
# ADDITIVE: it creates and updates the objects the files name and has no
# opinion about anything else. So a manifest DELETED from the tree takes
# its declaration away and leaves the object running, and a reader who
# assumes prune semantics reads this line as a full reconciliation. It
# is not one. The orphan is named by the verify step below instead, and
# the `kubectl delete` is a human step on purpose — see the header of
# infra/lint/a-deleted-manifest-leaves-no-object.sh for why `--prune` is
# refused here (it takes a derived set, and that set contains the
# StatefulSets and PVCs holding the audit log).
echo "cluster-deploy-runner: apply is additive — NO --prune. An object whose manifest was deleted keeps running; the verify step below names it."
# THE TUNNEL INGRESS HAS ONE WRITER, and it is not this apply (backlog
# 40d46042). The committed cloudflared-config.yaml is the NO-SKIP render
# of infra/cluster/instances.toml; the object the cluster holds must be
# the render for what THIS converge skipped (the `tunnel ingress` stage
# below, after the instance secret gate). Applying the committed copy
# here and the re-render there would write the object twice per
# converge — and a connector pod that restarted in the minute between
# the two would load the wrong one and hold it, silently, until the
# next ingress change (the pods never re-read a ConfigMap; see that
# stage). So the staged copy is dropped from the apply set: the object
# is still converged on every run, from the same render, by the one
# stage that knows the skipped set.
TUNNEL_CONFIG_FILE=cloudflared-config.yaml
python3 "$REPO/infra/cluster/render-dev-build-policy.py" \
    "$REPO/infra/cluster/manifests/boss-dev-build-egress.yaml" \
    "$REPO/infra/estate/estate.toml" "$APPLY_DIR/$SOURCE_NS/boss-dev-build-egress.yaml" \
    || { echo 'cluster-deploy-runner: REFUSED — worker egress fence was not rendered; nothing applied' >&2; exit 1; }
rm -f "$APPLY_DIR/$SOURCE_NS/$TUNNEL_CONFIG_FILE"
apply_namespaces "$SOURCE_NS"
converge_dev_door "$SOURCE_NS"
run_summary_field dev_door_declaration_sha256 "$(sha256sum "$APPLY_DIR/dev-door-$SOURCE_NS/dev-door.json" | cut -d ' ' -f 1)"
apply_instance "$SOURCE_NS"

# StepPlugin bundles converge from the tree too (job d35aec77).
# Code converges in the image, config in the manifests above, schema
# in boss-init — but the `step-plugins` ConfigMap was built by a
# kubectl command someone ran by hand, so it converged with nothing.
# Adding a bundle to infra/step-plugins/ and landing a train delivered
# NOTHING: the row at /system/step-plugins pointed at a file that was
# never mounted, and the step rendered "No plugin registered" with no
# error anywhere. That is how seven seeded plugins came to be active
# with absent bundles, blocking eleven ready review-design steps.
#
# Regenerated from the directory every converge, so the mounted
# bundles are whatever the tree says. --dry-run=client | apply keeps
# it idempotent; README.md is excluded because it is documentation,
# not a bundle. Deliberately NOT committed as a manifest: it is a
# derived artifact whose sources are already in tree, and a committed
# copy would be the second definition that drifts (§9a).
KP="docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/step-plugins:/plugins:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
# The generated ConfigMap is piped into $KAPPLY — the `-i` kubectl
# defined above the prod apply, where its comment records the bug.
STAGE="configmaps"
PLUGIN_ARGS=""
for f in "$REPO"/infra/step-plugins/*.js; do
    [ -e "$f" ] || continue
    PLUGIN_ARGS="$PLUGIN_ARGS --from-file=$(basename "$f")=/plugins/$(basename "$f")"
done
# Per instance namespace: the boss pod mounts this ConfigMap without
# `optional`, so an instance whose namespace lacks it never starts.
converge_step_plugins() { # NAMESPACE
    local ns="$1"
    if [ -n "$PLUGIN_ARGS" ]; then
        echo "cluster-deploy-runner: converging the step-plugins ConfigMap in $ns"
        # shellcheck disable=SC2086
        $KP create configmap step-plugins -n "$ns" $PLUGIN_ARGS \
            --dry-run=client -o yaml | $KAPPLY apply -f -
    else
        echo "cluster-deploy-runner: no bundles in infra/step-plugins — leaving the ConfigMap in $ns alone"
    fi
}
converge_step_plugins "$SOURCE_NS"

# THE GATE RUNNER'S SCRIPT IS THE SECOND INSTANCE OF THE BUG ABOVE.
#
# infra/gate-runner/run.sh reaches the cluster only as the ConfigMap
# gate-runner-script, and that ConfigMap was built by a kubectl command
# written in a COMMENT in gate-runner.yaml and run by hand — so, exactly
# like step-plugins before it, it converged with nothing. Landing a
# train that changes run.sh delivered NOTHING, silently: the gate kept
# running the old script and no error appeared anywhere.
#
# Measured 2026-08-30: car e6f55a36 merged, deployed and arrived, and
# the string it adds appeared twice in the merged run.sh and zero times
# in the live ConfigMap. It sat at `Proven in prod` unprovable, because
# the behaviour it claims was not running (2b69220a).
#
# Same cure as step-plugins, and deliberately NOT a committed manifest:
# it is a derived artifact whose source is already in tree, and a
# committed copy would be the second definition that drifts (§9a).
#
# TWO KEYS SINCE e3100bc8 (design 1d7a1669). `bootstrap.sh` is the fixed
# part a gate pod starts: it fetches the run.sh of the sha being gated and
# execs it, so a run.sh car is gated by its OWN script; `run.sh` is main's
# copy, which the bootstrap falls back to (named on the receipt) and which
# a manifest still naming /repo-script/run.sh runs directly. The key set
# is pinned equal to apply-script-configmap.sh's and to the manifest's
# command by gate_runner_manifests.rs.
KG="docker run --rm --log-driver=none --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/gate-runner:/gate:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
if [ -f "$REPO/infra/gate-runner/run.sh" ] && [ -f "$REPO/infra/gate-runner/bootstrap.sh" ]; then
    echo "cluster-deploy-runner: converging the gate-runner-script ConfigMap"
    $KG create configmap gate-runner-script -n boss-dev \
        --from-file=run.sh=/gate/run.sh \
        --from-file=bootstrap.sh=/gate/bootstrap.sh \
        --dry-run=client -o yaml | $KAPPLY apply -f -
else
    echo "cluster-deploy-runner: infra/gate-runner/run.sh or bootstrap.sh missing — leaving the ConfigMap alone" >&2
fi

# ONE patch, ONE revision. This used to be `set image` (main container)
# followed by a separate init-container patch — kubectl's `set image`
# cannot reach initContainers — which minted TWO revisions per roll,
# the first carrying a MIXED template (new main, old init). A
# rollback that steps back one revision lands on exactly that
# intermediate; the 2026-09-02 RS table shows the same mixed-template
# class minted by hand during the firefight. Both images move in one
# json patch so every revision in history is a coherent template and
# every rollback target is real.
# Roll to $HEAD; on a boot failure roll back to the last CONVERGED build
# by image name (cluster-deploy-lib.sh roll_deployment) — never to
# "the previous revision", which on 2026-09-05 was the placeholder the
# apply had just created and could not boot.
STAGE="roll $HEAD"
if ! roll_deployment "$K" "$REGISTRY" "$HEAD" "$LAST" "$FAILED_FILE" "$SOURCE_NS"; then
    exit 1
fi
$K set image -n "$SOURCE_NS" cronjobs -l boss-chore=true "chore=$REGISTRY:$HEAD" || true

# THE CLUSTER-RESIDENT CONDUCTOR runs the same boss image and converges
# its tag here, exactly like the boss deployment above — the manifest
# (boss-conductor.yaml) commits a :latest placeholder and this is where
# the real :$HEAD replaces it. It is a Deployment in boss-dev, so the
# `boss -n boss` patch and the CronJob selector both miss it; it gets its
# own patch. Preparation shares its source image, so both advance in one
# revision. `|| true`: a cluster where this manifest has not applied yet
# must not fail the converge; a patch refusal remains visible in its log.
# The journal preparation and executor must run the same source revision.
$K set image -n boss-dev deploy/boss-conductor "conductor=$REGISTRY:$HEAD" "prepare-diagnostics=$REGISTRY:$HEAD" || true

echo "$HEAD" > "$STAMP_FILE"
rm -f "$FAILED_FILE"
echo "cluster-deploy-runner: cluster on $REGISTRY:$HEAD"
_stage_done roll_s
# The roll is real from here; a failed verification below is a finding
# about it, not a failed converge — the request reads converged either
# way, and the maintenance packet carries the verification verdict.
OUTCOME="converged=$HEAD"

# THE DECLARED BROKER SECRETS EXIST, EMPTY, before anything reads them
# (backlog 51c98681, David 2026-09-16). The credential broker fills a
# Secret by PATCH and is deliberately not granted `create`, so the
# empty object at the head of every machine rotation was a hand act —
# and for the tunnel credential nobody's, so the connector below read
# `skipped (secret absent)` on every converge with nothing that would
# change it. This runner holds the admin credential and the converged
# tree, so it creates each Secret the broker rules declare
# (infra/dispatcher/rules/*, the `secret_namespace` / `secret_name`
# args the broker itself writes to — cluster-deploy-lib.sh
# broker_secrets) when it is absent, EMPTY, and never touches one that
# exists. The line rides the packet as `secrets_declared` — the field
# the car's probe reads — and never fails the converge: a Secret the
# broker fills is not what a train delivers. Before the connector read
# on purpose: the field that read records is then about the connector.
STAGE="declared secrets"
SECRETS_DECLARED=$(ensure_declared_secrets "$K" "$REPO/infra/dispatcher/rules")
run_summary_field secrets_declared "$SECRETS_DECLARED"
echo "cluster-deploy-runner: secrets declared: $SECRETS_DECLARED"

# THE OTHER INSTANCES, after prod is stamped (backlog 07d7549c). Each
# is applied exactly as prod was — namespace first, then its rendered
# directory, then its step-plugins ConfigMap and its chores' image —
# and the packet records which namespaces this converge applied
# (`instances_applied`), the fact the proof-in-prod probe reads.
# Deliberately AFTER prod's stamp and BEFORE the verification below:
# a playground apply that fails is a finding about the playground and
# fails this unit, but prod is already converged and stamped, so the
# next converge does not re-roll it; and the verification then reads
# every rendered object, both namespaces, in one pass. The ROLL of
# these instances is the last stage of the run, for the same reason
# in reverse — an instance whose Secrets are not minted yet waits
# 420s+300s for a rollout that cannot complete, and that wait must
# not delay prod's verification or the orphan sweep.
#
# AN INSTANCE WITHOUT ITS SECRETS IS SKIPPED, not failed (car cb784b3a,
# held on the dock 2026-09-16). The Secrets are minted out of tree, once
# per namespace, by David; as first built this loop applied the
# playground regardless and the roll below then waited 420 s + 300 s
# on pods that could not start — "Maintenance failed" on EVERY train
# until the ceremony, which is a scheduled alarm, not reliability. So
# the gate (cluster-deploy-lib.sh instance_secret_gate) asks first,
# deriving the required Secrets from the RENDERED manifests; an
# instance missing any is skipped whole, named on the packet as
# `instances_skipped: <ns> (secrets absent: a, b)` beside
# `instances_applied`, the `kubectl create secret` shapes are printed
# (names and keys, never values), and the converge exits 0. Once the
# Secrets exist the instance applies with no other change and the
# field flips from skipped to applied. A read the credential cannot
# make is neither — it fails this stage, after prod's stamp.
#
# AND PROVISIONING MINTS WHAT THE INSTANCE CAN MINT ITSELF, before the
# gate decides (backlog dc1bc724; David 2026-09-16: no hand work unless
# absolutely required — the playground was skipped on every converge
# from 2026-09-15 for want of six hand-minted Secrets, and only one of
# the six is a person's). The loop reads the absent set QUIETLY first
# (instance_secrets_absent — no verdict, no shapes for a person), and
# when anything is absent it applies the instance's Namespace — the
# Secrets must live somewhere — and hands the set to
# provision_instance_secrets (cluster-deploy-lib.sh, where each of the
# six is classified with its reason): internal ones minted from random
# bytes, shared ones copied from the namespace the instance list's
# `shares_with` resolves to (the sixth --instances column), the OIDC
# root created empty so the gateway boots guest-only, and anything else
# left for the gate to name. Values ride the `-i` kubectl's stdin —
# KAPPLY, the one that reads it — never argv, never this journal: the
# line below is names only, and it rides the packet as
# `instance_secrets_minted` the moment it is known. THEN the gate asks
# the cluster again, loud, and an instance that now has everything
# applies on this same converge.
#
# AND THE TENANT SOURCE IS MEASURED FIRST (backlog f4f5c387). Before
# the quiet read, before anything is minted for it, a repo-sourced
# instance's tenant is asked for in the forge's own repositories
# (tenant_source_verdict above); one that cannot be read is
# SKIPPED whole — `<ns> (tenant source unreadable: <repo>@<ref>)` in
# `instances_skipped`, `<ns>: unreadable (…)` in `tenant_source` — so
# the first converge after a flip names what is missing. The tenant
# is DELIVERED after the loud gate and before the apply: the ConfigMap
# must exist when the pod first starts, or the launcher refuses the
# empty directory and the pod crash-loops until it arrives.
STAGE="apply instances"
INSTANCES_APPLIED="$SOURCE_NS"
INSTANCES_SKIPPED=""
INSTANCE_SECRETS_MINTED=""
while IFS="$IFS_ROW" read -r iname ins_ns tdir _s _h share_ns trepo tref site; do
    [ "$ins_ns" = "$SOURCE_NS" ] && continue
    STAGE="tenant $ins_ns"
    verdict_rc=0
    verdict=$(tenant_source_verdict "$iname" "$ins_ns" "$tdir" "$trepo" "$tref") || verdict_rc=$?
    TENANT_SOURCE="${TENANT_SOURCE:+$TENANT_SOURCE; }$verdict"
    run_summary_field tenant_source "$TENANT_SOURCE"
    echo "cluster-deploy-runner: tenant source: $verdict"
    if [ "$verdict_rc" -ne 0 ]; then
        INSTANCES_SKIPPED="${INSTANCES_SKIPPED:+$INSTANCES_SKIPPED; }$(skipped_tenant_entry "$ins_ns" unreadable "$trepo" "$tref")"
        echo "cluster-deploy-runner: instance $iname ($ins_ns) SKIPPED — its tenant $trepo@$tref cannot be read from the forge's repositories on this host (the reason is above: no such repository, or no such branch or tag)" >&2
        continue
    fi
    STAGE="apply $ins_ns"
    absent=""
    read_rc=0
    absent=$(instance_secrets_absent "$K" "$KM" "$ins_ns" "/manifests/$ins_ns") || read_rc=$?
    if [ "$read_rc" -eq 0 ] && [ -n "$absent" ]; then
        STAGE="provision $ins_ns"
        apply_namespaces "$ins_ns"
        minted=$(provision_instance_secrets "$K" "$KAPPLY" "$KM" "$ins_ns" "/manifests/$ins_ns" "$share_ns" "$absent")
        INSTANCE_SECRETS_MINTED="${INSTANCE_SECRETS_MINTED:+$INSTANCE_SECRETS_MINTED; }$ins_ns: $minted"
        run_summary_field instance_secrets_minted "$INSTANCE_SECRETS_MINTED"
        echo "cluster-deploy-runner: instance $ins_ns secrets: $minted"
        STAGE="apply $ins_ns"
    fi
    gate_rc=0
    absent=$(instance_secret_gate "$K" "$KM" "$ins_ns" "/manifests/$ins_ns") || gate_rc=$?
    case "$gate_rc" in
        0) ;;
        1)  INSTANCES_SKIPPED="${INSTANCES_SKIPPED:+$INSTANCES_SKIPPED; }$(skipped_entry "$ins_ns" "$absent")"
            continue ;;
        *)  echo "cluster-deploy-runner: cannot tell whether $ins_ns has its Secrets (rc=$gate_rc) — not applying it; prod is converged and stamped" >&2
            run_summary_field instances_applied "$INSTANCES_APPLIED"
            exit 1 ;;
    esac
    echo "cluster-deploy-runner: applying instance $iname ($ins_ns) — boss image pinned to the converged $LAST"
    # A repo-sourced tenant lands in the namespace before the manifests
    # do (the Namespace first, as provisioning does), so the pod's first
    # start finds it.
    if [ -n "$trepo" ]; then
        STAGE="tenant $ins_ns"
        apply_namespaces "$ins_ns"
        converge_tenant "$iname" "$ins_ns" "$trepo" "$tref" "$site"
        STAGE="apply $ins_ns"
    fi
    apply_namespaces "$ins_ns"
    converge_dev_door "$ins_ns"
    apply_instance "$ins_ns"
    converge_step_plugins "$ins_ns"
    $K set image -n "$ins_ns" cronjobs -l boss-chore=true "chore=$REGISTRY:$HEAD" || true
    INSTANCES_APPLIED="$INSTANCES_APPLIED,$ins_ns"
done <<< "$(instance_rows "$INSTANCES")"
run_summary_field instances_applied "$INSTANCES_APPLIED"
echo "cluster-deploy-runner: instances applied: $INSTANCES_APPLIED"
if [ -n "$INSTANCES_SKIPPED" ]; then
    run_summary_field instances_skipped "$INSTANCES_SKIPPED"
    echo "cluster-deploy-runner: instances skipped: $INSTANCES_SKIPPED"
fi
_stage_done instances_s

# THE TUNNEL INGRESS IS RE-RENDERED AFTER THE SECRET GATE, so a SKIPPED
# instance's hostname is served by the source instance until it is
# provisioned (backlog 40d46042, urgent). The committed
# cloudflared-config.yaml — dropped from prod's apply set above, so this
# stage is the object's only writer — routes every hostname to its own
# instance's gateway, and an instance the loop above skipped has
# NOTHING running in its namespace: measured 2026-09-16 07:4xZ, the
# rotation had moved playground.algedonic.dev to this tunnel as
# designed, and the hostname reached Cloudflare Access and then a dark
# namespace. The fix is
# sequencing, not a second file: the renderer is handed the SAME string
# the packet carries as `instances_skipped` — the idiom the manifests
# check below already reads, one definition — and routes a skipped
# instance's hostname to $SOURCE_NS's gateway under a comment naming
# why; with nothing skipped the render IS the committed file, byte for
# byte, so an instance that has just been applied flips back to its own
# gateway on this same converge. The map rides the packet as
# `tunnel_ingress` (`<hostname> → <namespace>[ (<ns> skipped: <reason>)]`),
# recorded by the `tunnel connector` stage below from the SAME skipped
# string — the one lib function both ticks record through (0b7804f3).
#
# THE CONNECTOR DOES NOT RE-READ A CHANGED CONFIGMAP. Read from
# cloudflared 2026.9.1's source, not assumed: `tunnel --config … run`
# parses its ingress once (cmd/cloudflared/tunnel/configuration.go
# prepareTunnelConfig → ParseIngressFromConfigAndCLI), and the only
# runtime update path is the orchestrator's UpdateConfig, fed by the
# edge for remotely-managed tunnels; the fsnotify config watcher
# (watcher/file.go) is wired only in cmd/cloudflared/main.go
# handleServiceMode — cloudflared started with no subcommand — and even
# there fires on fsnotify.Write alone, which a ConfigMap update (the
# kubelet's atomic symlink swap) never is. So the pods must roll to
# read a new render, and they roll EXACTLY when the render changed: the
# sha256 of the applied ConfigMap is patched into the Deployment's pod
# template as an annotation — the same value patches to "no change"
# and no pod moves, a new value is a rolling update (2 replicas, surge
# 1, unavailable 0, so the tunnel keeps a Ready connector throughout;
# the ConfigMap is applied BEFORE the patch, so the pods the roll
# creates mount this render). The annotation is a fact about which
# render the pods loaded, readable off the live object; `kubectl apply`
# of the committed cloudflared.yaml leaves it in place (three-way merge
# deletes only keys the last applied config carried). Recorded as
# `tunnel_ingress_render: <sha12> (connector
# rolled|unchanged)`. The connector is then READ, below, after this —
# so the `cloudflared` field is about the connector serving this
# render. A failed apply or patch fails this stage, after prod's stamp,
# exactly as a failed instance apply does: it is a write the credential
# could not make, not a Cloudflare-side fault.
STAGE="tunnel ingress"
TUNNEL_CONFIG=$(BOSS_INSTANCES_SKIPPED="$INSTANCES_SKIPPED" "$REPO/infra/cluster/render-tunnel-config.sh")
printf '%s\n' "$TUNNEL_CONFIG" | $KAPPLY apply -f -
TUNNEL_SHA=$(printf '%s\n' "$TUNNEL_CONFIG" | sha256sum | cut -d' ' -f1)
patch_out=$($K patch deploy cloudflared -n "$SOURCE_NS" --type merge \
    -p "{\"spec\":{\"template\":{\"metadata\":{\"annotations\":{\"boss.algedonic.dev/ingress-sha256\":\"$TUNNEL_SHA\"}}}}}")
echo "cluster-deploy-runner: $patch_out"
case "$patch_out" in
    *"no change"*) TUNNEL_ROLL="connector unchanged" ;;
    *)             TUNNEL_ROLL="connector rolled" ;;
esac
run_summary_field tunnel_ingress_render "${TUNNEL_SHA:0:12} ($TUNNEL_ROLL)"
echo "cluster-deploy-runner: tunnel ingress render ${TUNNEL_SHA:0:12}, $TUNNEL_ROLL"
_stage_done ingress_s

# THE TUNNEL CONNECTOR, read after the roll and after its ingress is
# applied (backlog 5a2bb0ce; design 4c565f8c, David 2026-09-16).
# cloudflared.yaml is a prod pipeline manifest, applied above with the
# rest; its one Secret is created empty by the declared-secrets stage
# and filled by the broker's rotation, and prod's apply has no secret
# gate, so the pods either connect or wait with nothing on the packet
# saying which. cluster-deploy-lib.sh connector_status derives the
# Secret from the RENDERED manifest (still mounted at /manifests here —
# the apply directory is discarded right after), and records
# `connected`, `not-ready`, or `skipped (secret absent: <name>)` — the
# field the car's probe reads; when the ingress stage above rolled the
# pods, `connected` means the NEW pods hold an edge connection. The
# read never fails the converge: the tunnel is not what a train
# delivers, and a Cloudflare-side fault must not hold every train's
# packet red. Recorded through observe_connector — the one function
# the unchanged tick records the same fields through (0b7804f3) —
# beside the ingress map for the skipped string the apply loop decided,
# so the map describes the render the pods above were rolled onto.
STAGE="tunnel connector"
observe_connector "$K" "$KM" "$SOURCE_NS" "/manifests/$SOURCE_NS/cloudflared.yaml" cloudflared "$INSTANCES_SKIPPED" deploy
rm -rf "$APPLY_DIR"

# THE SITE, read after the roll (backlog e114238a; design b64c4377):
# prod is rolled and its boss-site ConfigMap applied above, so what
# the gateway serves for `/` under the site hostname NOW is this
# converge's — `site.hash` on this packet is the sha256 of the body it
# got, the value the tenant's publish-the-landing-page rule matches
# against its own `site_hash` to mark the page live. Same lib function
# as the unchanged tick; same bound: never fatal, never faked. Needs
# no manifest mount, so it follows the apply directory's discard.
STAGE="observe site"
observe_sites "$K" "$INSTANCES" "$SOURCE_NS" "$INSTANCES_SKIPPED"
STAGE="verify manifests"

# THE CONVERGE VERIFIES WHAT IT APPLIED (60690755). Everything above
# is a write; nothing above reads back whether the cluster now holds
# what the tree says. infra/cluster/check-manifests-applied.sh is that
# read — every named object present, and for the kinds where
# present-but-wrong is the realistic failure, contents matching — and
# it was running nowhere with a credential that could see the 41
# objects (the dev-session credential reports 39 unreadable). This is
# the one place with both the tree and the admin kubeconfig, so it
# runs here, AFTER the stamp: the deploy above is real either way, and
# a drift is a finding about it, not a reason to roll it back.
#
# A failure — drift (exit 1) or cannot-verify (exit 2, which is
# 'unknown', not 'clean') — fails this unit, so ExecStopPost closes
# the converge packet "Maintenance failed" with the exit status
# (timers-leave-a-packet): the alarm channel that already exists,
# carrying the check's own report in the journal. It does not retry on
# its own; the next converge — the next main move — re-applies and
# re-checks, which is the honest cadence for a drift.
#
# THE CHECK IS TOLD WHAT THE APPLY SKIPPED, and its verdict rides the
# packet (backlog 07d7549c; forge journal 2026-09-16 06:06Z). The
# first converge with the secret gate above skipped the playground —
# correctly — and then failed here: the check walked the rendered set
# and counted the skipped instance's 21 objects MISSING. And the packet
# said `result: exit-code` and nothing else; the cause was readable
# only in this journal. So the check gets the SAME string the packet
# carries as `instances_skipped` (one definition; it counts those
# instances' absent objects `skipped`, apart from `missing`), and its
# one-line summary is recorded as `manifests_check` whatever its exit —
# the field a reader off the host needs, and the one the packet lacked.
echo "cluster-deploy-runner: verifying infra/cluster/manifests against the cluster"
check_rc=0
check_out=$(KUBECONFIG="$KUBECONFIG_PATH" BOSS_INSTANCES_SKIPPED="$INSTANCES_SKIPPED" \
    "$REPO/infra/cluster/check-manifests-applied.sh") || check_rc=$?
printf '%s\n' "$check_out"
check_summary=$(printf '%s\n' "$check_out" | grep '^check-manifests-applied: ' | tail -n 1) || true
run_summary_field manifests_check "${check_summary:-no summary printed (exit $check_rc)}"
if [ "$check_rc" -ne 0 ]; then
    rc=$check_rc
    echo "cluster-deploy-runner: MANIFESTS CHECK FAILED (rc=$rc) — the tree and the cluster disagree, or the check could not verify; the converge packet stays open until a converge passes it" >&2
    exit 1
fi
echo "cluster-deploy-runner: manifests verified — the cluster holds what the tree declares"
_stage_done verify_s

# AND THE OTHER DIRECTION. check-manifests-applied.sh asks "is every
# DECLARED object present?" — a question no deleted manifest is in, and
# one an orphan answers correctly by being absent from it. The apply
# above does not prune, so the converge's own writes cannot close that
# gap; naming it can. This is the same argument that put the check above
# here: this is the one place with both the tree and a credential that
# can read all 50 objects (the dev-session credential the lint usually
# runs under reports 9 of 16 pairs unreadable), so it is the only place
# the sweep means anything.
#
# It FAILS the unit like the check above, for the reason CLAUDE.md gives
# under "a check nobody reads is a check that is not running": a finding
# demoted to a printed warning in a passing converge is a finding nobody
# reads. An undeclared object in a managed namespace is a real finding
# and the message says the three things it can be.
#
# FIRST ADMIN-REACH RUN. Its exemption set was populated from the dev
# pod's partial view, so this may be the first time anything has
# enumerated live ConfigMaps, Roles, RoleBindings, ServiceAccounts and
# PVCs in `boss`. If it names something nobody has looked at, that is
# the check working: declare it, delete it, or add it to EXEMPT with the
# reason — one car either way. The deploy is already stamped above, so a
# finding here never rolls anything back.
#
# ITS VERDICT RIDES THE PACKET AS `orphans_check` (backlog cb0c9937),
# like `manifests_check` above — the count and the NAMED objects, bounded,
# recorded by the lint itself through run-summary.sh (it inherits this
# unit's BOSS_RUN_SUMMARY_FILE), on a pass, a finding and a refusal alike.
# Until 2026-10-06 the packet read `failed_stage: check orphans` and
# nothing else: two converges failed on ConfigMap/boss/boss-instance-config
# — a generated object the derivation had no entry for — and the name was
# in this journal alone. This stage is also the last one before the other
# instances are rolled, so a failure here that nobody can read from the
# packet is a playground left on an old build with no stated reason.
STAGE="check orphans"
echo "cluster-deploy-runner: checking for objects the tree no longer declares"
orphan_rc=0
KUBECONFIG="$KUBECONFIG_PATH" bash "$REPO/infra/lint/a-deleted-manifest-leaves-no-object.sh" || orphan_rc=$?
if [ "$orphan_rc" -ne 0 ]; then
    rc=$orphan_rc
    echo "cluster-deploy-runner: ORPHAN CHECK FAILED (rc=$rc) — something is running that no manifest declares; the apply cannot remove it (no --prune) so the delete is a named human step, printed above (and named on this run's packet as orphans_check); the other instances are NOT rolled to $HEAD by this run" >&2
    exit 1
fi
echo "cluster-deploy-runner: no orphans — nothing is running that the tree cannot account for"

# LAST: roll the other instances to $HEAD (backlog 07d7549c). Same
# function as prod's roll — patch, prove Ready, or roll back to the
# last converged build by name — in the instance's namespace, with the
# instance's OWN quarantine file: this head already booted on prod, so
# a failure here is the instance's environment, never a reason to hold
# prod's next converge. It fails the unit (the packet reads
# "Maintenance failed" at this stage, naming the instance) rather than
# printing a warning nobody reads — the playground converges on every
# train by decision, and a train that did not reach it must say so.
# An instance the apply loop SKIPPED (its Secrets absent) is not rolled:
# there is nothing of this head in it to prove Ready.
while IFS="$IFS_ROW" read -r iname ins_ns _t _s _h; do
    [ "$ins_ns" = "$SOURCE_NS" ] && continue
    case ",$INSTANCES_APPLIED," in *",$ins_ns,"*) ;; *) continue ;; esac
    STAGE="roll $ins_ns $HEAD"
    echo "cluster-deploy-runner: rolling instance $iname ($ins_ns) to $REGISTRY:$HEAD"
    if ! roll_deployment "$K" "$REGISTRY" "$HEAD" "$LAST" "$FAILED_FILE.$ins_ns" "$ins_ns"; then
        run_summary_field "roll_$ins_ns" failed
        echo "cluster-deploy-runner: INSTANCE $ins_ns DID NOT REACH $HEAD — prod is converged and stamped; its Secrets exist (the apply gate passed), so this is the instance's own environment: read its pods" >&2
        exit 1
    fi
    run_summary_field "roll_$ins_ns" "$HEAD"
done <<< "$(instance_rows "$INSTANCES")"

# LAST OF ALL: THE LIVE REGISTRIES AGAINST THE TREE THEY CONVERGED FROM
# (design d349e0ba car 2, backlog b79054b2). The image is rolled and its
# boot has run the seeds — the workflow bundle's insert-if-missing, the
# dispatcher's rules — so this is the one moment the tree, the image and
# the live registries are supposed to agree, and this host holds the
# converged checkout. Until car 1 the two lints that ask that question
# ran in every gate, where the live state decided a car's verdict (14 of
# 92 failed gates, 2026-09-21..28, none caused by the car); now they run
# here, and each disagreement becomes ONE backlog-item naming the side
# that is ahead and the command that resolves it (registry-drift.sh says
# how). The summary rides the packet as `registry_drift`.
#
# NEVER FATAL, by the rule every visibility write here follows: drift is
# work to queue, not a deploy failure, and the loop that deploys a fix
# must owe nothing to what it watches. The script exits 0 on every path
# but a usage error; its status is KEPT rather than swallowed, and a
# phase that died with no summary is recorded as that, with its exit,
# rather than as agreement.
STAGE="registry drift"
drift_rc=0
drift_out=$("$REPO/infra/forge/registry-drift.sh" --repo "$REPO" 2>&1) || drift_rc=$?
printf '%s\n' "$drift_out"
drift_line=$(printf '%s\n' "$drift_out" | awk '/^registry-drift: / { l = $0 } END { print l }')
if [ -n "$drift_line" ] && [ "$drift_rc" -eq 0 ]; then
    run_summary_field registry_drift "${drift_line#registry-drift: }"
else
    run_summary_field registry_drift "the phase exited $drift_rc; last said: ${drift_line:-nothing}"
fi

# Agent publication is a separate purpose after all core convergence checks.
# Failure refuses agent readiness, and never undoes or blocks BOSS delivery.
. "$REPO/infra/agent/publication-stage.sh"
STAGE="agent image publication"
agent_image_publication_stage "$REPO/infra/agent/publish-image.sh" "$HEAD_FULL" "$CONVERGE_STATE"
