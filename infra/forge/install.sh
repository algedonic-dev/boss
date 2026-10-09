#!/usr/bin/env bash
# Install the forge host's systemd units from this checkout.
#
# WHY THIS EXISTS. boss-gcp has had an installer with a timer roster
# (today infra/gcp/install-units.sh reading infra/estate/roles.toml;
# until 2026-09-18 the TIMERS array of the deleted bare-metal deploy
# script) since the day its own comment was written: "Adding a new
# timer = author the .service + .timer in the right place under infra/,
# then add a row here" — instead of a `sudo install` treadmill that had
# been the source of every 'this timer was authored but never installed'
# gap so far (audit-integrity, ml-inference-batch, ledger-recognize,
# conservation-invariants — all caught by hand).
#
# The forge host had no equivalent, so its units went on by hand, and
# one of the two was forgotten. On 2026-08-17 the CI runner's disk
# defect (feedback 1b63456b) was still open because
# `reap-dead-ci-jobs` — script, service and timer, all committed —
# had never been installed. `cluster-deploy-runner` had been. Nothing
# in the tree could tell the difference, and nothing was going to.
#
# WHERE IT RUNS. On the forge host (infra/estate/estate.toml
# `forge_host`), unattended, from root's own tree at
# /var/lib/boss/tree/current — which is where the installed units point
# since backlog a604a35b (see ROOT RUNS THIS FROM ITS OWN TREE below).
# By hand it is still started from the checkout, and hands itself over
# to the tree's copy:
#
#   ssh <forge> 'cd /home/david/boss && git fetch forgejo main \
#     && git checkout -qf FETCH_HEAD && sudo infra/forge/install.sh'
#
# (NOT `git pull` — the checkout tracks no upstream branch; it is driven
# by cluster-deploy-runner's detached checkouts of forgejo/main, so a
# bare pull has nothing to merge and stops to ask. This is the same
# fetch+checkout forge-converge.sh runs unattended.)
#
# It is idempotent — re-running installs the same files and restarts
# nothing that has not changed.
set -euo pipefail

cd "$(dirname "$0")" || exit 1
HERE="$(pwd)"

# Every unit this host runs. A unit absent from this list is a unit
# nobody installs, which is the entire defect above.
#
# reap-dead-ci-jobs: removes the corpses of crashed CI jobs and the
#   named volumes they hold. A crashed job's volume is NAMED, so
#   `docker volume prune` skips it — on 2026-08-14 one held 63GB and
#   left the next run 74GB, less than a cold `cargo test` needs, and
#   the symptom was four unrelated boss-ledger tests failing on
#   "could not extend file".
# cluster-deploy-runner: builds forge main and rolls the cluster onto
#   it every ten minutes. This is the SECOND deploy path — the
#   conductor deploys boss-gcp — and the reason "the train deployed"
#   and "the cluster is current" can differ by ten minutes.
# disk-floor-sweep: below BOSS_DISK_FLOOR_GB free on the root volume,
#   reclaims regenerable docker caches in a fixed order and stops at
#   the floor; an unmet floor is a failed unit, which is the alarm.
#   Exists because cluster-deploy-runner's cleanup only runs when main
#   moves — which needs CI — which needs disk. Circular exactly when
#   the disk fills, which it did on 2026-09-02, blocking every train.
# forge-converge: runs THIS script from forge main on a timer, so a
#   unit that lands on main installs itself on the next tick instead of
#   waiting for someone to remember to ssh in. It is the fix for the
#   whole class this file's header describes; disk-floor-sweep sitting
#   uninstalled through the 2026-09-03 fill is the most recent instance.
#   The bootstrap that installs forge-converge is the one surviving hand
#   action — after it, the host converges like the cluster.
# estate-observe-host: the forge observes itself every 15 minutes —
#   the estate loop's tightest disk was the one box with no observer
#   (49a8d842), and the boarding host check (BOSS_TRAIN_CI_HOST) can
#   only read a host that reports. Same script as boss-gcp's observer,
#   HOST_ID=forge.
# cluster-watchdog: the loop that knows the cluster is working from
#   OUTSIDE it — reads the API, compares what serves with what the
#   converge last stamped, rolls to that build by name when the API
#   has been dark longer than a deploy, and says so every 5 minutes.
#   No maintenance wrap, by design: the 2026-09-05 outage lasted four
#   hours because every loop that could act needed the API it watched.
# forge-backup: a nightly `forgejo dump` of the repositories and
#   Forgejo's database, verified and kept here in a bounded count
#   (backlog 121831e6). Until it, the forge held the only copy of the
#   repository, the runner registration and the signing keys, and
#   nothing copied them. Local only — the offsite legs need a
#   credential this host does not hold, and every run says so.
# estate-observe-units: the forge watches its OWN units every five
#   minutes (backlog c98dcf38) — the same observer boss-gcp runs,
#   HOST_ID=forge, its roster read off THIS list through the `rows` /
#   `roster` modes below. Until it, the unit observer ran on boss-gcp
#   alone, and forge-converge.service closed failed 72 times in twelve
#   hours on 2026-09-26 while no ESTATE ALARM fired.
UNITS=(
    reap-dead-ci-jobs
    cluster-deploy-runner
    disk-floor-sweep
    forge-converge
    estate-observe-host
    cluster-watchdog
    forge-backup
    estate-observe-units
)

# THE READ-ONLY MODES, before anything that writes or needs root. The
# unit observer (infra/estate/observe-units.sh, OBSERVE_UNITS_INSTALLER
# pointed here by infra/forge/estate-observe-units.service) derives the
# forge's watch roster from these, in the shape boss-gcp's installer
# prints (infra/gcp/install-units.sh `rows` / `roster`), so what the
# forge installs and what it watches are ONE list (CLAUDE.md §9a).
# Every row is in role: this host installs its whole list, whatever its
# roles; the ops runner, which IS role-gated, the observer asks
# install-ops-runner.sh --in-role about itself.
case "${1:-}" in
    rows)
        for u in "${UNITS[@]}"; do echo "$u:forge"; done
        exit 0
        ;;
    roster)
        for u in "${UNITS[@]}"; do echo "in-role $u"; done
        exit 0
        ;;
    "") ;;
    *)
        echo "usage: $0 [rows|roster]" >&2
        exit 2
        ;;
esac

# What this run leaves for forge-converge's own packet — counts, each
# sub-installer's verdict, every anomaly verbatim. A no-op unless the
# caller set BOSS_RUN_SUMMARY_FILE; forge-converge.service does. The forge
# has a readable journal door, unlike boss-gcp, but a door that answered
# 200 with a seven-hour-stale journal is already on the record
# (2026-09-10), and two converges reporting differently about what they
# installed is the §9a shape. One definition: infra/run-summary.sh.
# shellcheck source=infra/run-summary.sh
. "${HERE}/../run-summary.sh"

# Where units land and who reloads them. Overridable so the installer
# can be exercised into a scratch directory with a stub systemctl —
# infra/lint/forge-install-covers-the-ops-runner.sh runs it on every
# gate and asserts what it would install. On the host both are the
# defaults, and root is required as before.
ETC="${INSTALL_ETC:-/etc/systemd/system}"
SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"

# WHICH MACHINE THIS IS, BEFORE ANYTHING IS WRITTEN (backlog 62b09c57,
# N7). "Overridable so the installer can be exercised into a scratch
# directory" was true of two knobs and assumed of the rest: a run that
# named INSTALL_ETC and forgot another still rendered /etc/boss/sor.env,
# asked the package manager for the journal door and downloaded kubectl,
# on whatever machine it was started on — the lint that drives this file
# on every gate among them, and on 2026-10-07 the dev pod, which a
# fixture's converge installed the forge onto for 1.5 s (review aa901496;
# infra/lib/host-check.sh carries the account and the rule).
#
# So EVERY seam is named here, once, ahead of the first write. With each
# one redirected this is a scratch run and is asked nothing. With any one
# left at the host's own default, this machine must hold the address
# infra/estate/estate.toml declares for the forge — or the run exits 78,
# naming the seams, having written nothing. A seam that only matters when
# a step is switched on is named under that switch; the probe account
# (probe-account.sh), the tree (root-tree.sh) and the ops runner's
# installer ask for themselves as well, because each is also started by
# hand. Adding a write to this file means adding its line here, and
# host_check_sh.rs refuses an INSTALL_* name this block does not judge.
#
# A FIRST INSTALL on a forge the estate does not declare yet:
#   sudo BOSS_FIRST_INSTALL_AS=forge infra/forge/install.sh
# said out loud on the run's own lines and its packet, never assumed.
# shellcheck source=infra/lib/host-check.sh
. "${HERE}/../lib/host-check.sh"
host_seam INSTALL_ETC /etc/systemd/system
host_seam INSTALL_SYSTEMCTL systemctl
host_seam INSTALL_SOR_ENV /etc/boss/sor.env
host_seam INSTALL_KUBECTL 1
host_seam INSTALL_APT_GET
host_seam BOSS_OPS_RUNNER_RETIRED
if [ -n "${INSTALL_ROOT_TREE:-}" ]; then
    host_seam BOSS_ROOT_TREE /var/lib/boss/tree
    host_seam INSTALL_TREE_LIBEXEC /usr/local/libexec/boss
    host_seam INSTALL_WATCHDOG_STATE /var/lib/boss/watchdog
fi
if [ -n "${BOSS_CONVERGE_HOLD:-}" ]; then
    host_seam BOSS_CONVERGE_HOLD_LEGACY
fi
if [ -n "${INSTALL_KIT_LIBEXEC:-}" ]; then
    host_seam INSTALL_KIT_SUDOERS_DIR
fi
case ",${BOSS_NODE_ROLES:-}," in
    *,cluster-operator,*)
        host_seam INSTALL_TALOSCTL 1
        if [ "${INSTALL_CLI:-1}" = "1" ] && [ -n "${BOSS_CONVERGE_SHA:-}" ]; then
            host_seam BOSS_CLI_STORE
            host_seam BOSS_CLI_LINK
        fi
        ;;
esac
host_check forge install.sh
[ -z "$HOST_CHECK_VERDICT" ] || echo "install.sh: host check — $HOST_CHECK_VERDICT"

if [ "$ETC" = "/etc/systemd/system" ] && [ "$(id -u)" -ne 0 ]; then
    echo "install.sh: needs root to write /etc/systemd/system — re-run with sudo." >&2
    exit 1
fi

# ROOT RUNS THIS FROM ITS OWN TREE, OR BRINGS THE TREE AND STARTS AGAIN
# (backlog a604a35b; decided by David on design-doc c98c79aa, question
# root-tree, and as position B, 2026-10-07). The units this file installs
# execute from /var/lib/boss/tree/current — a tree root fetched from the
# forge's own repository (infra/forge/root-tree.sh carries the reasoning)
# — and no longer from the checkout this host's owner can write. So does
# this file: started from anywhere else it refreshes the tree and execs
# the copy there, once.
#
# THAT IS THE BOOTSTRAP, AND IT IS THE OLD UNIT THAT RUNS IT. On the tick
# this lands, the installed forge-converge.service still starts the
# checkout's forge-converge.sh — a snapshot taken before its own checkout,
# so the script of the commit BEFORE this one — and that calls this file
# from the checkout. Here it makes the tree and hands over; the units the
# tree's copy installs point at the tree, and the next tick starts there.
# A hand `sudo infra/forge/install.sh` from the checkout takes the same
# road.
#
# WITH NO TREE, THE UNITS ARE HELD AND EVERYTHING ELSE CONVERGES. A
# refresh that failed leaves nothing to exec, and this copy carries on
# from where it stands; the gate ahead of the unit loop then finds the
# units' commands absent and installs NO unit file, so every unit —
# the watchdog first among them — keeps running as it was installed.
# A scratch run (the lints, INSTALL_ETC elsewhere) manages no tree
# unless it names seams of its own.
TREE_ROOT="${BOSS_ROOT_TREE:-/var/lib/boss/tree}"
TREE_LIBEXEC="${INSTALL_TREE_LIBEXEC:-/usr/local/libexec/boss}"
tree_managed=0
if [ "$ETC" = "/etc/systemd/system" ] || [ -n "${INSTALL_ROOT_TREE:-}" ]; then
    tree_managed=1
fi
in_tree=0
if [ "$tree_managed" -eq 1 ]; then
    # Each read that can fail has its own name: no generations directory
    # is "not in the tree", a refresh that failed is said on its line and
    # judged by what `path` then answers, and `path` exiting 1 is "no
    # tree" — none is an empty answer taken for a pass.
    no_gen="" refresh_rc=0 no_tree=""
    tree_gen="$(cd "$TREE_ROOT/gen" 2>/dev/null && pwd -P)" || no_gen=1
    if [ -z "$no_gen" ]; then
        case "$(pwd -P)/" in
            "$tree_gen"/*) in_tree=1 ;;
        esac
    fi
    if [ "$in_tree" -eq 0 ] && [ -z "${BOSS_INSTALL_FROM_TREE:-}" ]; then
        tree_line="$(BOSS_ROOT_TREE="$TREE_ROOT" bash "${HERE}/root-tree.sh" refresh 2>&1)" || refresh_rc=$?
        printf '%s\n' "$tree_line"
        [ "$refresh_rc" -eq 0 ] || echo "install.sh: the tree's refresh exited $refresh_rc — the tree as it stands is what runs, if one stands" >&2
        tree_now="$(BOSS_ROOT_TREE="$TREE_ROOT" bash "${HERE}/root-tree.sh" path 2>/dev/null)" || no_tree=1
        if [ -z "$no_tree" ] && [ -n "$tree_now" ] && [ -x "$tree_now/infra/forge/install.sh" ]; then
            echo "install.sh: started from ${HERE}, which is not root's tree — running $tree_now/infra/forge/install.sh instead"
            BOSS_INSTALL_FROM_TREE=1 exec "$tree_now/infra/forge/install.sh" "$@"
        fi
        echo "install.sh: root's tree at $TREE_ROOT is not there to run from (the line above says why) — carrying on from ${HERE}; the units are HELD as installed" >&2
    fi
fi

# THE CONVERGE HOLD'S DIRECTORY, BEFORE ANYTHING ELSE (backlog d94d287e).
# The hold moved out of world-writable /var/tmp to /var/lib/boss
# (forge-defaults.sh says why); this root step makes that directory
# root:root 0755 on every tick and, once, carries a hold standing at the
# old path across. FIRST, because this tick's checkout is the one that
# moved the path: the deploy runner reading the new path while a hold
# still stood at the old one is the window this closes, and every line
# below it is time the window would stay open. Its failure is carried,
# not fatal, like the ops runner's: a refused prepare reds the run and
# names itself, but never leaves a unit uninstalled. A scratch run (the
# lints, INSTALL_ETC elsewhere) leaves the host's directory alone unless
# it names a hold of its own.
hold_rc=0
if [ "$ETC" = "/etc/systemd/system" ] || [ -n "${BOSS_CONVERGE_HOLD:-}" ]; then
    bash "${HERE}/converge-hold.sh" prepare || hold_rc=$?
    [ "$hold_rc" -eq 0 ] || run_summary_field converge_hold "prepare failed (exit $hold_rc) — see the journal"
else
    echo "install.sh: scratch run (INSTALL_ETC=$ETC) — the converge hold's directory is the host's, left alone"
fi

# THE ADDRESS FILE, FIRST. /etc/boss/sor.env is the one place on this
# host that spells the system of record and the forge's own addresses;
# every unit installed below reads it with EnvironmentFile= (no `-`: a
# unit that started without its address would answer a wrong target)
# and every script sources infra/lib/sor.sh. Rendered from the tree's
# ONE source, infra/estate/estate.toml, on every converge — so the
# boss.algedonic.dev cutover is an edit to that file and a tick of this
# timer (backlog 5222163e, audit H10). Before the units, deliberately:
# a daemon-reload that finds the file absent would leave every unit
# refusing to start until the next tick. Overridable for the scratch
# run the lints drive (a test never writes /etc).
SOR_ENV="${INSTALL_SOR_ENV:-/etc/boss/sor.env}"
bash "${HERE}/../estate/render-sor-env.sh" --to "$SOR_ENV"
run_summary_field sor_env "$SOR_ENV"
# Now this run itself has the addresses the rest of the install reads
# (the journal door below, the roles read, the ops-runner installer).
export BOSS_SOR_ENV="$SOR_ENV"
# shellcheck source=infra/lib/sor.sh
. "${HERE}/../lib/sor.sh"
sor_require BOSS_JOBS_URL BOSS_FORGE_JOURNAL_URL

# THE CONVERGE'S LAUNCHER, a root-owned copy outside every tree
# (backlog a604a35b): what forge-converge.service starts, and the thing
# that keeps the converge able to bring in its own fix when a generation
# cannot (forge-converge-launch.sh says how). Renamed into place, so the
# unit never starts half a file. BEFORE the gate below, which looks for
# it; a copy that could not be made holds the units.
if [ "$tree_managed" -eq 1 ]; then
    install -d -m 0755 -- "$TREE_LIBEXEC" \
        && install -m 0755 -- "${HERE}/forge-converge-launch.sh" "$TREE_LIBEXEC/.forge-converge-launch.new" \
        && mv -f -- "$TREE_LIBEXEC/.forge-converge-launch.new" "$TREE_LIBEXEC/forge-converge-launch" \
        || echo "install.sh: could not install $TREE_LIBEXEC/forge-converge-launch" >&2
fi

# NO UNIT IS INSTALLED WHOSE COMMAND IS NOT THERE (backlog a604a35b).
# Every absolute path a unit's Exec line names must be an executable
# file NOW, read through the names the unit itself uses — the tree's
# `current`, the launcher — or NO unit file is replaced on this run. One
# rule covers a tree that could not be fetched, a generation that lacks a
# script, and a launcher that did not install; and it is all-or-nothing
# on purpose: the installed set is one commit's or another's, never half
# of each. The units already installed keep running from wherever they
# point; this run goes red at its end and says which command was missing,
# and the next tick asks again. The two roots are read through their
# seams, so a test never looks in /var/lib or /usr/local.
units_held=""
if [ "$tree_managed" -eq 1 ]; then
    for u in "${UNITS[@]}"; do
        [ -f "${HERE}/${u}.service" ] || continue
        while IFS= read -r cmd; do
            case "$cmd" in
                /var/lib/boss/tree/*) at="$TREE_ROOT/${cmd#/var/lib/boss/tree/}" ;;
                /usr/local/libexec/boss/*) at="$TREE_LIBEXEC/${cmd#/usr/local/libexec/boss/}" ;;
                # The two units still on the checkout (the cluster
                # converge and the disk sweep; no_unattended_unit_
                # runs_from_a_home.rs carries the roster and why).
                /home/david/boss/*) at="${BOSS_FORGE_REPO_DIR:-/home/david/boss}/${cmd#/home/david/boss/}" ;;
                /*) at="$cmd" ;;
                *) continue ;;
            esac
            [ -f "$at" ] && [ -x "$at" ] || units_held="$units_held ${u}.service:$cmd"
        done < <(sed -n -E 's/^Exec(StartPre|Start|StartPost|Stop|StopPost)=[-@:+!]*([^[:space:]]+).*/\2/p' "${HERE}/${u}.service")
    done
    if [ -n "$units_held" ]; then
        echo "install.sh: UNITS HELD — no unit file is replaced on this run, because a command a unit names is not an executable file:$units_held. Every unit keeps running as it is installed." >&2
        run_summary_field units_held "$(printf '%s' "$units_held" | cut -c1-900)"
    fi
fi

# THE WATCHDOG'S STATE, CARRIED ONCE TO ROOT'S DIRECTORY (backlog
# a604a35b). cluster-watchdog.service runs as root from this car and
# keeps its dark count and its blind marker under /var/lib/boss/watchdog
# (its StateDirectory=) instead of its old user's home. Two values are
# worth carrying, because losing either has a cost in the middle of an
# outage: the DARK COUNT (lost, a rollback waits up to three more
# checks) and the BLIND MARKER (lost, a second urgent "watchdog blind"
# alert is filed). Each is read from the old home as DATA from another
# account — bounded, one line, and carried only when it is exactly a
# count or exactly a timestamp; anything else is dropped and never
# printed, so a name planted there to be read through cannot put a
# secret in this journal. ONCE: the marker records that the carry ran,
# and nothing reads the old home again. NOT CARRIED: kept alerts and the
# door observer's unposted readings and dark-since (the unit says why).
# Before the unit files, so the first root tick finds its count.
if [ "$tree_managed" -eq 1 ] && [ -z "$units_held" ]; then
    wd_new="${INSTALL_WATCHDOG_STATE:-/var/lib/boss/watchdog}"
    wd_old="${INSTALL_WATCHDOG_OLD_HOME:-/home/david}"
    if install -d -m 0700 -- "$wd_new" && [ ! -e "$wd_new/.carried" ]; then
        carried=""
        while IFS=: read -r name file; do
            [ -e "$wd_new/$name" ] && continue
            [ -f "$wd_old/$file" ] && [ ! -L "$wd_old/$file" ] || continue
            unread=""
            val="$(timeout 5 head -c 64 -- "$wd_old/$file" 2>/dev/null | awk 'NR == 1')" || unread=1
            case "$name" in
                dark) shape='^[0-9]{1,6}$' ;;
                *) shape='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' ;;
            esac
            if [ -z "$unread" ] && [[ "$val" =~ $shape ]]; then
                printf '%s\n' "$val" >"$wd_new/$name" && carried="$carried $name"
            else
                echo "install.sh: the watchdog's old $file does not hold what it should — not carried (and not printed)" >&2
            fi
        done <<'CARRY'
dark:.boss-watchdog-dark
blind:.boss-watchdog-blind
CARRY
        date -u +%Y-%m-%dT%H:%M:%SZ >"$wd_new/.carried"
        echo "install.sh: the watchdog's state moved to $wd_new; carried from $wd_old:${carried:- nothing (no count and no blind marker stood there)}"
        run_summary_field watchdog_state "moved to $wd_new; carried:${carried:- nothing}"
    fi
fi

installed=0
for u in "${UNITS[@]}"; do
    for ext in service timer; do
        src="${HERE}/${u}.${ext}"
        if [ ! -f "$src" ]; then
            echo "install.sh: ${u}.${ext} is listed here but missing from ${HERE}" >&2
            exit 1
        fi
        [ -n "$units_held" ] || install -m 0644 "$src" "${ETC}/${u}.${ext}"
    done
    installed=$((installed + 1))
done

# kubectl ON THE HOST, for the observer that closes the converge loop.
# cluster-deploy-runner drives kubectl through the alpine/k8s image
# (no host binary needed to APPLY), but check-manifests-applied.sh —
# the "is what's in the tree what's running?" read that 60690755 found
# running nowhere with a real credential — calls plain `kubectl` over
# every manifest and cannot be fed through a container mount cleanly.
# One binary, pinned by sha so CDN weather cannot ship a different one
# (the a700d3a4 lesson), the same version as the image and the cluster
# line (1.33). Downloads once; a later run finds it and moves on.
KUBECTL_VERSION="v1.33.3"
KUBECTL_SHA256="2fcf65c64f352742dc253a25a7c95617c2aba79843d1b74e585c69fe4884afb0"
if [ "${INSTALL_KUBECTL:-1}" = "1" ] && [ ! -x /usr/local/bin/kubectl ]; then
    tmp="$(mktemp)"
    if curl -sfL -o "$tmp" "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" \
        && echo "${KUBECTL_SHA256}  ${tmp}" | sha256sum -c - >/dev/null; then
        install -m 0755 "$tmp" /usr/local/bin/kubectl
        echo "install.sh: kubectl ${KUBECTL_VERSION} installed"
    else
        echo "install.sh: kubectl download or checksum failed — the manifests check will report 'cannot verify' until it is present" >&2
    fi
    rm -f "$tmp"
fi

# WHAT THE cluster-operator ROLE BRINGS (design 1bc4b4ed: cluster
# management runs on this host; the workstation is a terminal). Read
# off BOSS_NODE_ROLES, which forge-converge exports from the estate
# registry; a hand run with no roles set installs nothing here and says
# so. Two things, and a check:
#
#   * talosctl, pinned by sha like kubectl above — the Talos client is
#     the ONLY interface to the nodes (no ssh), and it must stay within
#     one minor of the cluster. v1.13.8 is the version David's own client
#     runs, so the forge answers exactly as the workstation did.
#   * The credentials the role needs — /etc/boss-ops/talosconfig and
#     /etc/boss-ops/kubeconfig — are CHECKED, never written. They are
#     placed once by David (token admin is his) and must be root:root
#     mode 0600; anything else is reported on the converge packet as
#     absent-or-wrong until fixed. The estate's own converge never
#     writes a credential.
#   * The tree's `boss` CLI, since 2026-09-18 (backlog 9f00a805,
#     consolidation H8, car 1) — taken out of the cluster image built
#     for the commit this converge checked out, by the same installer
#     boss-gcp has run since 2026-09-15 (infra/estate/
#     install-cli-from-image.sh; the store is /opt/boss-cli, the link
#     /usr/local/bin/boss, `cli_sha` beside `converge_sha` on the
#     packet). Measured on #448: infra/forge/*.sh was 32 scripts and
#     9,790 lines, the largest of them shell twins of CLI verbs
#     (run-car-probe.sh for `boss prove --from-car`, tenant-census.sh
#     for `boss tenant`, …) each with its own pin, because this host
#     had no binary to shell to. The role that brings it is the one
#     the verbs serve: cluster management runs here. The CLI step runs
#     below, after the credential check.
. "${HERE}/../estate/node-roles.sh"
cli_rc=0
if has_role cluster-operator; then
    # talosctl and the /etc/boss-ops credential check are the ROLE's,
    # not this host's: since 2026-09-20 boss-gcp holds the role too, and
    # a second copy of the version pin is the drift CLAUDE.md §9a names.
    # One definition, sourced so it can report through run_summary_field.
    . "${HERE}/../estate/install-cluster-operator.sh"
    # The id forge-converge.sh reads the roles with, which picks the
    # credentials checked here (backlog f371c749).
    install_cluster_operator "${BOSS_NODE_ID:-forge}"

    # THE CLI, FROM THE IMAGE AT THE SHA THIS CONVERGE CHECKED OUT.
    # forge-converge.sh hands the sha over as BOSS_CONVERGE_SHA (root
    # cannot read the owner's clone); a hand run has none and installs
    # no CLI rather than guessing one. The installer records its own
    # facts (cli_sha, cli_result, cli_action, cli_image) through the
    # run summary; its output is captured and printed whole under its
    # own prefix, like boss-gcp's converge prints it.
    #
    # THREE VERDICTS, NOT TWO. Exit 0 is the CLI confirmed at the sha.
    # Exit 75 is `not yet`: the registry answered and has no image for
    # this commit's tag — the deploy runner on THIS host builds it a
    # few minutes after each train, and this converge fetched main ten
    # minutes after the last one, so the first tick after every train
    # lands here. That is a wait, recorded on the packet, retried next
    # tick, and NOT a red: a converge that failed on every train would
    # be an alarm nobody could read (CLAUDE.md §Diagnosis). Anything
    # else is a real refusal — the registry dark, a digest mismatch, a
    # binary that names another commit — and reds the run the way it
    # reds boss-gcp's: after the units below are installed, enabled and
    # reported, with the exit on the packet. The installer leaves
    # /usr/local/bin/boss at whatever the previous confirmed generation
    # was in every non-zero case.
    if [ "${INSTALL_CLI:-1}" = "1" ]; then
        if [ -z "${BOSS_CONVERGE_SHA:-}" ]; then
            echo "install.sh: no converged sha in the environment (BOSS_CONVERGE_SHA, set by forge-converge.sh) — a hand run installs no CLI; the next converge tick does"
            run_summary_field cli_result "skipped: no BOSS_CONVERGE_SHA (hand run)"
        else
            cli_log="$(mktemp -t forge-install-cli.XXXXXX)"
            bash "${HERE}/../estate/install-cli-from-image.sh" "$BOSS_CONVERGE_SHA" >"$cli_log" 2>&1 || cli_rc=$?
            sed 's/^/  cli: /' "$cli_log"
            rm -f "$cli_log"
            case "$cli_rc" in
                0) echo "install.sh: the CLI is the tree's at ${BOSS_CONVERGE_SHA:0:8} (cluster-operator)" ;;
                75)
                    echo "install.sh: the image for ${BOSS_CONVERGE_SHA:0:8} is not in the registry yet — the deploy runner builds it after each train; the next tick retries, and /usr/local/bin/boss stays whatever the previous converge confirmed (cli_result on the packet)"
                    cli_rc=0 ;;
                *)
                    echo "install.sh: the CLI step FAILED (exit $cli_rc) at ${BOSS_CONVERGE_SHA:0:8} — its complete" >&2
                    echo "    output is above. Every unit still converges below; /usr/local/bin/boss is" >&2
                    echo "    whatever the previous converge confirmed (cli_result on the packet says why)." >&2
                    run_summary_field cli_exit "$cli_rc" ;;
            esac
        fi
    fi
else
    echo "install.sh: cluster-operator not among this host's roles (${BOSS_NODE_ROLES:-none}) — no Talos client installed, no CLI"
fi

# The per-unit `jobs-url.conf` drop-in that used to carry the system of
# record (from 2026-09-03, when reap-dead-ci-jobs failed every run for
# want of it, to 2026-09-18) is RETIRED: every unit reads
# /etc/boss/sor.env itself. A drop-in left behind would carry a second
# copy of the address that nothing re-renders, so it is removed — the
# converge that stops writing a file must also stop the file standing.
for u in "${UNITS[@]}"; do
    rm -f "${ETC}/${u}.service.d/jobs-url.conf"
    rmdir "${ETC}/${u}.service.d" 2>/dev/null || true
done

# The ops-request runner is the same unit boss-gcp runs, installed the
# same way: infra/ops/install-ops-runner.sh is the ONE definition of how
# a host gets one — the unit pair byte-identical from infra/ops, plus a
# drop-in carrying THIS host's identity and checkout and the system of
# record pinned inline. Until 2026-09-05 this host answered packets only
# because someone had installed it by hand; a rebuild would have lost
# the read door (packet 4d5f158a, infra/forge/OPERATIONS.md §Residue).
# The block that used to sit here was copied for boss-gcp on 2026-09-11
# and collapsed into that script the same day rather than living twice
# (CLAUDE.md §9a). It enables the timer itself, which is why the loop
# below no longer appends it.
#
# CALLED UNCONDITIONALLY, AND IT DECIDES. Since 2026-09-22 (backlog
# cb9eb0f2) the script reads BOSS_NODE_ROLES — which forge-converge
# exported above from the estate registry — and installs a runner only
# where the `ops-runner` role is declared. The predicate lives there,
# once, so this caller and boss-gcp's cannot hold two answers to "which
# hosts run a runner"; a host outside the role gets a skip line and
# exit 0, not a refusal.
#
# ITS FAILURE IS LOUD BUT LATE, and that is why the rc is carried instead
# of letting `set -e` act here: a runner that did not install deserves a
# red unit and a packet on the `failed` terminal, but not at the price of
# leaving every timer below installed-and-not-enabled.
#
# THE RUNNER STILL EXECUTES THE CHECKOUT, AND THAT IS SAID HERE BECAUSE IT
# IS THE ROAD THIS CAR LEAVES OPEN (backlog a604a35b, stage 2 of design
# c98c79aa). The installer points the runner's ExecStart at the tree it
# is itself run from, and run from root's tree that would move every verb
# script there at once — while the publish, merge and tag verbs still
# take "the repository I am in" to be a git checkout with a forge remote.
# Separating where a verb's code runs from the checkout it works on is
# that stage's car. Until it lands the runner is installed from the tree
# (its unit pair) and pointed at the checkout (its ExecStart), by name.
ops_runner_rc=0
ops_runner_repo=""
[ "$in_tree" -eq 0 ] || ops_runner_repo="${BOSS_FORGE_REPO_DIR:-/home/david/boss}"
INSTALL_ETC="$ETC" INSTALL_SYSTEMCTL="$SYSTEMCTL" INSTALL_OPS_RUNNER_REPO="$ops_runner_repo" \
    bash "${HERE}/../ops/install-ops-runner.sh" forge || ops_runner_rc=$?
installed=$((installed + 1))

# THE RECOVERY KIT'S READER (backlog c1bb822e): a root-owned copy of
# recovery-kit-read.sh outside this checkout, and the one sudoers rule
# that grants it — the review of car 3d12b774 refused a rule pointing
# into a tree its grantee can edit. Carried, not fatal, like the ops
# runner's. A scratch run (the lints) leaves /usr/local and /etc/sudoers.d
# alone unless it names a directory of its own.
#
# IN ROOT'S TREE THE GRANTEE IS NAMED HERE (backlog ebfd2f46). The kit
# installer read the rule's user off the owner of the tree it runs from,
# which was the forge user's checkout until root's tree (a604a35b) and
# is root's generation since: from 2026-10-08 00:32Z it refused "the
# checkout is root's" on every tick and this converge exited 1 for that
# alone. The name is the one forge-converge.sh runs the checkout's git
# as — BOSS_FORGE_REPO_OWNER, the same default, held equal by
# root_tree_sh.rs — because the tree carries it and no account on this
# host can write the tree: it moves by a merge to the forge's main.
# NOT `stat` of the checkout: what that answers is decided by whoever
# can replace a name in the checkout's parent directory, and it would
# make the grantee of a sudo rule a fact read out of a home.
kit_reader_rc=0
kit_user="${INSTALL_KIT_USER:-}"
[ "$in_tree" -eq 0 ] || [ -n "$kit_user" ] || kit_user="${BOSS_FORGE_REPO_OWNER:-david}"
if [ "$ETC" = "/etc/systemd/system" ] || [ -n "${INSTALL_KIT_LIBEXEC:-}" ]; then
    INSTALL_KIT_USER="$kit_user" bash "${HERE}/install-recovery-kit-reader.sh" || kit_reader_rc=$?
    if [ "$kit_reader_rc" -ne 0 ]; then
        run_summary_field recovery_kit_reader "install failed (exit $kit_reader_rc) — see the journal"
    else
        # WHO HOLDS THE RULE, READ BACK OFF THE RULE. The packet that
        # reported this step failing could not say whether an earlier
        # rule still stood ("presumably", ebfd2f46): the file is root's
        # 0440 and the only account of it was a journal line. So the
        # grantee goes on the packet, read from what was placed rather
        # than from what this run meant to place.
        kit_rule="${INSTALL_KIT_SUDOERS_DIR:-/etc/sudoers.d}/boss-recovery-kit"
        kit_unread=""
        kit_grantee="$(sed -n 's/^\([^ #]*\) ALL=(root) NOPASSWD: .*$/\1/p' "$kit_rule" 2>/dev/null)" || kit_unread=1
        if [ -n "$kit_unread" ] || [ -z "$kit_grantee" ]; then
            run_summary_field recovery_kit_reader "installed, but no grantee could be read back from $kit_rule"
        else
            run_summary_field recovery_kit_reader "installed; the rule grants the reader to: $kit_grantee"
        fi
    fi
fi

# THE ACCOUNT A CAR'S RECORDED PROBE RUNS AS (backlog 703358ce; decided
# by David on design-doc bdc60b65, question probe-user). Until this step
# probes ran as the checkout's owner, who reaches root on this host by
# two roads (review 9a1e289b). infra/forge/probe-account.sh `ensure` is
# the one definition: it makes `boss-probe` and a root-owned read-only
# view of this checkout for it to read, verifies both by effect, and
# writes the drop-in that points the ops runner's probes at them and at
# the marker a tick writes only when the account verifies then.
# AFTER the ops runner's installer, whose drop-in directory it writes
# beside, and BEFORE the daemon-reload that makes the drop-in live.
# Adding an account is a root change to the host, so it goes through the
# door root changes already use — this converge — and like the steps
# above its failure is carried: a host where the account cannot be made
# keeps every unit, and NO PROBE RUNS THERE until it verifies — never a
# fallback to the checkout's owner (decided by David on design-doc
# c98c79aa, question `fallback`) — with a red converge that says so and
# repairs it on a later tick. A scratch run (the lints) makes no account
# unless it names seams of its own.
probe_account_rc=0
if [ "$ETC" = "/etc/systemd/system" ] || [ -n "${INSTALL_PROBE_LIBEXEC:-}" ]; then
    INSTALL_ETC="$ETC" bash "${HERE}/probe-account.sh" ensure || probe_account_rc=$?
fi

"$SYSTEMCTL" daemon-reload
for u in "${UNITS[@]}"; do
    "$SYSTEMCTL" enable --now "${u}.timer"
    printf '  %-24s %s\n' "$u" "$("$SYSTEMCTL" is-active "${u}.timer")"
done

# The journal-over-HTTP read door (:19531). Hand-enabled once on
# 2026-09-03 and in the tree nowhere, which is the Residue item this
# closed: a host rebuild silently loses the one door that still works
# when the API is dark.
#
# ONE DEFINITION, SHARED WITH boss-gcp's CONVERGE. The six lines that
# did this used to live here, and then backlog 68757702 found boss-gcp
# with no read path at all and needing exactly the same six. A copy is
# what drifts (CLAUDE.md §9a), so the how moved to
# infra/journal-door-ensure.sh and both converges call it: that file
# carries why the door exists, why nothing of ours is shipped for it, and
# why every failure in it is non-fatal.
JOURNAL_DOOR_URL="$BOSS_FORGE_JOURNAL_URL" bash "${HERE}/../journal-door-ensure.sh"

# A HELD RUN SAYS HELD, HERE TOO (review 5f3736a2, F7). This line used to
# read "9 unit pair(s) installed and enabled" on a run that had replaced
# no unit file — the success line, and the `summary` a reader of the
# packet sees first, for work that was not done.
if [ -n "$units_held" ]; then
    echo "install.sh: HELD — 0 unit files replaced (the $((installed - 1)) forge unit pairs are as they were installed); their timers were enabled as they stand" >&2
    run_summary_field units_installed 0
    run_summary_field units_skipped "$((installed - 1))"
    run_summary_field summary "HELD: no unit file replaced — a command a unit names is not there (units_held); everything else converged"
else
    echo "install.sh: ${installed} unit pair(s) installed and enabled"
    run_summary_field units_installed "$installed"
    run_summary_field units_skipped 0
    run_summary_field summary "installed $installed unit pair(s) and enabled their timers"
fi
if [ "$ops_runner_rc" -ne 0 ]; then
    echo "install.sh: the ops-request runner did NOT install (exit $ops_runner_rc) — it named" >&2
    echo "    what failed above, and the run summary carries it. Every other unit converged;" >&2
    echo "    this host cannot answer an ops-request until that is fixed." >&2
    exit "$ops_runner_rc"
fi
# The CLI verdict last, for the same reason the ops runner's is: a
# refused pull deserves a red unit and a packet on `failed` — the host
# has not converged on the tree until its CLI is the tree's — but never
# at the price of a unit left uninstalled or a timer left disabled.
if [ "$cli_rc" -ne 0 ]; then
    echo "install.sh: the CLI did NOT install (exit $cli_rc) — cli_result on the packet says why." >&2
    echo "    Every unit converged; /usr/local/bin/boss is whatever the previous converge confirmed." >&2
    exit "$cli_rc"
fi
# Held units: named above and on the packet (`units_held`).
if [ -n "$units_held" ]; then
    echo "install.sh: the units were HELD (a command one names is not there:$units_held) — every unit runs as it was" >&2
    echo "    installed; everything else converged. The next tick asks again." >&2
    exit 1
fi
# The hold's prepare, the same way: its refusal is above, in its own words.
if [ "$kit_reader_rc" -ne 0 ]; then
    echo "install.sh: the recovery kit's reader did NOT install (exit $kit_reader_rc) — it named what failed" >&2
    echo "    above. Every unit converged; no workstation can write a kit until it installs." >&2
    exit "$kit_reader_rc"
fi
if [ "$hold_rc" -ne 0 ]; then
    echo "install.sh: the converge hold's directory was NOT prepared (exit $hold_rc) — converge-hold.sh said why above." >&2
    echo "    Every unit converged; a hold-converge refuses loudly until the directory is root's." >&2
    exit "$hold_rc"
fi
# The probe account's, last: probe-account.sh named what failed above and
# on the packet (`probe_account`).
if [ "$probe_account_rc" -ne 0 ]; then
    echo "install.sh: the probe account did NOT verify (exit $probe_account_rc) — probe-account.sh said why above." >&2
    echo "    Every unit converged; no probe runs on this host until it does (\`probe_account\` on the packet)." >&2
    exit "$probe_account_rc"
fi
