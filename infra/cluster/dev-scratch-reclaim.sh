#!/usr/bin/env bash
#
# dev-scratch-reclaim — keep the dev pod's two workspaces above a
# free-space floor by reclaiming REGENERABLE artifacts, and nothing
# else. The cluster-side sibling of infra/forge/disk-floor-sweep.sh.
#
# WHY THIS EXISTS. The dev pod's `scratch` is a node-local emptyDir
# with NO sizeLimit and NO reclaim (boss-dev.yaml). Measured
# 2026-09-06: /scratch/target/debug had grown past 136G and dozens of
# git worktrees had piled up under .claude/worktrees/, pushing the
# build node into repeated "system low on memory" pressure — "the
# filesystems bog down over time." Nothing ever reclaimed either:
#   - the gate runner's per-run sizeLimit backstop (gate-runner.yaml)
#     lives on a DIFFERENT, disposable pod; it does nothing for this
#     long-lived one.
#   - a scratch sizeLimit here would only take effect on a pod restart
#     (which kills the live session) and evicts rather than reclaims —
#     a separate operator decision, deliberately out of scope for this
#     script.
# This is the missing actuator: the SAFE half — measure, then reclaim
# bounded, regenerable space, stopping as soon as the floor is met.
#
# TWO WORKSPACES, TWO FLOORS, because the pod mounts two filesystems
# and each fills a different way:
#   * WORK  (/work, the ReadWriteOnce PVC): the git clone + its
#     worktrees. Reclaim = prune stale git worktrees.
#   * SCRATCH (/scratch, the node-local emptyDir): CARGO_TARGET_DIR and
#     every builder's sibling target. Reclaim = the idle siblings, least
#     recently used first, then the primary's incremental cache.
# Each is checked against its own floor and reclaimed independently.
#
# AND SINCE 2026-09-18, ONE PASS THAT SPANS BOTH AND IS NOT
# FLOOR-TRIGGERED (backlog 1933db9e, audit H11): a worktree whose
# branch is GONE — no origin/ ref left in the checkout, and either
# landed on origin/main or abandoned unpushed — clean and idle, is
# removed together with the per-worktree cargo target wt-cargo made
# for it. The judgement reads the checkout's OWN refs and never the
# forge (backlog b50a65ef, below). Measured
# that day: 203 worktrees under /work/boss, 175 of their 182 branches
# no longer on the forge, 36 target-* dirs — 364 GB on a 929 GB disk at
# 77% — and nothing here could see any of it: the floor passes wait for
# a floor that 221 GB of headroom never reaches, and the age pass
# below retires a target but never the checkout that keeps minting it.
# The disk floor is what stops a host, so the bound is kept BEFORE the
# floor, the way the stale-target pass already does.
#
# WHAT IT NEVER DOES: it never discards work. `git worktree remove`
# runs WITHOUT --force, so a worktree with uncommitted changes is
# refused and left standing; and committed work lives in the repo's
# object store regardless of the worktree, so removing a checkout only
# ever costs a `git worktree add` to recover. It never removes a
# worktree under a LIVE lock — one whose owner is alive, or whose text
# names a person or purpose — the main worktree, or the worktree this
# run is executing from. A lock whose owner is gone, or that names no
# owner at all, is judged like no lock (`lock_stale_why`,
# OWNERLESS_LOCK), and so is a harness lock whose agent's every gated run
# is closed on the record (`harness_lock_released`). It never touches the build cache while a build is
# running (checked through the pod's shared PID namespace), and even
# then it removes only `incremental/` dirs, which cargo regenerates.
# It touches no path outside the two workspaces. If a floor stays
# unmet after every bounded remediation it exits non-zero LOUDLY and
# stops: escalating to `cargo clean` or deleting linked artifacts is a
# human's call, never this script's.
#
# WHAT IT RECORDS. A pass that reclaimed something, or could not meet a
# floor, files a `maintenance-dev-scratch-reclaim` packet on the system
# of record and completes its `run` step with the totals — worktrees
# removed and their size, the dirty trees it kept BY NAME, targets and
# caches dropped — through the same boss-maintenance-wrap.sh +
# boss-step.sh pair every timer chore uses. A pass that found nothing
# to do files nothing: that is a reading, not work (David, 2026-09-16),
# and twenty-four identical packets a day would bury the one that
# matters. The record never gates the work — an unreachable system of
# record costs this pass its visibility, not its reclaim.
#
# AND SINCE 2026-09-18 THE ONE PASS THAT INSTALLS RATHER THAN RECLAIMS
# (backlog c35eda6c, retro 27fad542): the pod's `boss` CLI, taken out of
# the cluster image at origin/main with infra/estate/install-cli-from-
# image.sh — the installer boss-gcp and the forge already run — into
# /work/tools/image-cli, where the shim (infra/dev/boss) prefers it
# over the hand-built /scratch/target binary. The retro counted that
# binary rebuilt by hand three times in one window because a landed
# car had changed what the CLI validates locally, and a stale one
# refused a valid verb. This pass runs here, and not in the manifest,
# because a manifest change rolls the pod and ends the operator's live
# session (memory: boss-dev-manifest-cars-restart-the-session); the
# sidecar already runs THIS script from the checkout every hour with
# git, curl, jq and tar in its image and /work writable. `--cli` runs
# that leg alone (the ten-second fix the shim's refusal names).
#
# AND SINCE 2026-09-20, ONE PASS THAT MOVES THE CHECKOUT ITSELF
# (backlog 033d1fd3): /work/boss is fast-forwarded to the origin/main it
# has already fetched, so the pod doors symlinked into its infra/dev
# stop answering from a copy the tree has moved past. No gate holds it
# (backlog af27db95: none reads /work/boss once its packet exists), and
# git's own refusal is a problem on the packet, never a silent stall.
# The long form is at the pass itself.
#
# AND SINCE 2026-10-08, ONE THING IT KEEPS RUNNING (backlog 9ecd12ea):
# the memory watch, infra/cluster/dev-memory-watch.sh — a ring of
# per-process samples on the PVC and an alert on a container restart,
# after the dev container was OOMKilled with no record of either. Each
# hourly pass starts its loop when none is alive. The long form is in
# that script.
#
# WHAT RUNS IT. The `reclaim` sidecar in boss-dev.yaml fires it hourly
# (the disk-floor-sweep.timer cadence: above the floor a pass is one
# log line; below it a pass frees GBs, well ahead of the fill rate).
# "Well ahead" was false on 2026-09-24 (backlog 3f2a08ab: the floor
# crossed to the eviction line in under 27 minutes), so since then
# infra/dev/wt-cargo also runs `--scratch-floor` — the floor's sibling
# pass alone — before every build; the long form is at that mode.
# It is also safe to run by hand:
#   kubectl exec -n boss-dev deploy/boss-dev -c reclaim -- \
#     bash /work/boss/infra/cluster/dev-scratch-reclaim.sh
#
# Tunables (env, with in-sidecar defaults):
#   BOSS_SCRATCH_FLOOR_PCT   a flat share of /scratch's filesystem to
#                            keep free, as a percentage (default: the
#                            SCRATCH_RECLAIM_* rule in
#                            infra/build-floor.env)
#   BOSS_LIVE_TARGET_MIN     minutes since a sibling target was touched
#                            before the floor pass may take it (default 30)
#   BOSS_STALE_TARGET_H      hours before a sibling target dir is dead (default 3)
#   (the /work floor is not a tunable: it is the gate's floor plus a
#    margin, from infra/build-floor.env — BOSS_WORK_FLOOR_GB is retired
#    and ignored; see WORK_FLOOR_GB below)
#   BOSS_WORKTREE_MAX_AGE_H  only prune worktrees older than; also the
#                            git quiet before an UNPUSHED worktree (no
#                            origin/ ref, not on origin/main) counts as
#                            abandoned                      (default 48)
#   BOSS_WORKTREE_GRACE_H    hours of git quiet before a LANDED
#                            worktree (by sha or by content) is
#                            removable; under the /work
#                            floor it yields to BOSS_LIVE_TARGET_MIN
#                                                           (default 3)
#   BOSS_WORKTREE_IDLE_H     hours of git quiet before a main/detached
#                            worktree is removable          (default 168)
#   BOSS_JOBS_URL            the system of record the pass records on;
#                            else the line in REPO_DIR/infra/dev/sor-url
# Paths (env, defaulted to the boss-dev layout):
#   REPO_DIR (/work/boss) WORKTREES_DIR (REPO_DIR/.claude/worktrees)
#   CARGO_TARGET_DIR (/scratch/target)
#   SCRATCH_MOUNT (/scratch) WORK_MOUNT (/work)
#   PROC_ROOT (/proc) — the process table a worktree lock is judged
#     against (a test fakes it)
#   BOSS_CLI_STORE (WORK_MOUNT/tools/image-cli) — the image CLI's
#     generations, what the shim reads; BOSS_CLI_LINK
#     (WORK_MOUNT/tools/bin/boss-image) — the image CLI on PATH by its
#     own name, whatever the shim decides; BOSS_CLI_INSTALLER — the
#     installer to run (the estate's, beside this checkout; a test
#     stubs it)
set -euo pipefail

# The scratch floor is a SHARE of the filesystem, because the line it
# must stay ahead of is one: the kubelet evicts the pod when the node
# fs falls below 15% free (measured on w-1 2026-09-23: 139 GiB of 929).
# A floor in GB sat at 50 — ninety GB BELOW that line — so the kubelet
# evicted the whole pod six times in eight days while this pass never
# fired once. 25% kept ~90 GiB of margin on w-1 over that line — and
# sat exactly on Longhorn's refusal line, under the estate's disk_tight
# at 27.5%, so the alarm fired while this pass was designed to do
# nothing (backlog 4cc39798, 2026-10-20). The floor is now read from
# infra/build-floor.env (below, where that file is located) and has the
# estate's own shape — a share, or a capped headroom share where that
# is higher (scratch_floor_gb) — pinned above the estate's floor at
# every disk size. The knob stays for a fixture or a hand pass: set, it
# is a flat share that replaces the whole rule; it spells no default.
# A sibling target touched this recently belongs to a build in flight;
# the floor pass never takes it (the mtime is the per-dir liveness
# check, as in the stale-target pass).
LIVE_TARGET_MIN="${BOSS_LIVE_TARGET_MIN:-30}"
# Hours a SIBLING target dir may go untouched before it is a dead cache.
# Every builder gets its own CARGO_TARGET_DIR under the scratch mount
# (boss brief says so), and a landed branch's target outlives it by
# days. 3h is longer than any build, and wt-cargo touches its target
# continuously while one runs. It was 12h until backlog b4112350
# (2026-10-22 00:05Z, measured on the pod): /scratch, w-1's system NVMe
# and Longhorn's only w-1 disk, held 369 GiB in 35 live targets of
# 10-16 GB, leaving 88 GiB placeable above Longhorn's 25% floor — under
# one 100 GiB worker volume, so dev isolation admitted one builder.
STALE_TARGET_H="${BOSS_STALE_TARGET_H:-3}"
# THE /work FLOOR IS DERIVED, NOT CONFIGURED (backlog 99ce8744): the
# gate's own floor plus a margin, both read from infra/build-floor.env,
# the one definition infra/gate.sh reads too. Measured 2026-09-24
# ~23:00Z: /work at 12 GB free of 40, the gate refusing below 12 and
# this pass acting only below 6 (`BOSS_WORK_FLOOR_GB`, default 6 and 6
# again in the sidecar's env) — so the automatic pass never fired before
# a builder's pre-flight was refused, and two builders lowered the
# gate's floor by hand in one hour. Adding the margin to the gate's
# number is what makes "the reclaim acts first" true by construction.
#
# BOSS_WORK_FLOOR_GB IS RETIRED AND IGNORED. The live sidecar still sets
# it to 6 (boss-dev.yaml), and a manifest edit rolls the pod and ends
# the operator's session (memory: boss-dev-manifest-cars-restart-the-
# session), so it is this script, which the sidecar runs from the
# checkout every hour, that stops reading it. The file is found beside
# this script's own checkout, so the sidecar reads the floor of the tree
# it is running.
BUILD_FLOOR_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/build-floor.env"
. "${BUILD_FLOOR_FILE%/*}/lib/jq.sh"
build_floor() {
    awk -F= -v k="$1" '$1 == k {v = $2} END {print v}' "$BUILD_FLOOR_FILE" 2>/dev/null
}
GATE_MIN_FREE_GB="$(build_floor GATE_MIN_FREE_GB)"
WORK_RECLAIM_MARGIN_GB="$(build_floor WORK_RECLAIM_MARGIN_GB)"
SCRATCH_RECLAIM_FLOOR_PCT="$(build_floor SCRATCH_RECLAIM_FLOOR_PCT)"
SCRATCH_RECLAIM_HEADROOM_PCT="$(build_floor SCRATCH_RECLAIM_HEADROOM_PCT)"
SCRATCH_RECLAIM_HEADROOM_CEILING_GB="$(build_floor SCRATCH_RECLAIM_HEADROOM_CEILING_GB)"
for name in GATE_MIN_FREE_GB WORK_RECLAIM_MARGIN_GB SCRATCH_RECLAIM_FLOOR_PCT \
    SCRATCH_RECLAIM_HEADROOM_PCT SCRATCH_RECLAIM_HEADROOM_CEILING_GB; do
    case "${!name:-empty}" in
        empty|*[!0-9]*)
            echo "dev-scratch-reclaim: $BUILD_FLOOR_FILE must define $name as a whole number, got '${!name}'" >&2
            exit 64
            ;;
    esac
done
WORK_FLOOR_GB=$((GATE_MIN_FREE_GB + WORK_RECLAIM_MARGIN_GB))
SCRATCH_FLOOR_PCT="${BOSS_SCRATCH_FLOOR_PCT:-$SCRATCH_RECLAIM_FLOOR_PCT}"
WORKTREE_MAX_AGE_H="${BOSS_WORKTREE_MAX_AGE_H:-48}"
# Hours of git QUIET (no commit, HEAD write or reflog entry) before a
# worktree whose branch has LANDED — head on origin/main, no origin/
# ref left — may go. Landed is necessary, not sufficient: a builder's
# tree is CLEAN in the minutes between its commit and its push, and an
# unpushed branch has no origin/ ref either, which is why an unpushed
# tree waits the longer WORKTREE_MAX_AGE_H instead. The same 3h as the
# target pass — longer than any session's silence between two git
# commands. The grace exists so a run re-dispatched into the same tree
# finds it, and 3h covers that: a repair dispatch follows its review
# within the hour. It was 12h ("shorter than the next morning") until
# backlog b4112350 (2026-10-22): a closed run's tree and target held
# w-1's scratch for half a day, and with it every worker slot dev
# isolation needs (see STALE_TARGET_H above). Every other guard stands —
# a dirty tree, unpushed commits, a detached head no ref holds, an open
# run, or a record that cannot be read keeps the tree at any age.
WORKTREE_GRACE_H="${BOSS_WORKTREE_GRACE_H:-3}"
# A worktree on `main` or a detached HEAD has no branch for the forge
# to have forgotten, so idleness is the whole judgement: 7 days.
WORKTREE_IDLE_H="${BOSS_WORKTREE_IDLE_H:-168}"

REPO_DIR="${REPO_DIR:-/work/boss}"
RECLAIM_GIT_OBSERVER="$(dirname "$(readlink -f "$0")")/dev-reclaim-git.sh"
# Independent, bounded per-pass failure signal: work/PVC storage can refuse a
# phase write while the existing Git operation must retain its original policy.
# Its native pass metadata records UNKNOWN, never validity of older rows as PASS.
GIT_PHASE_ATTEMPT_FILE=$(mktemp "${TMPDIR:-/tmp}/dev-reclaim-git-attempt.XXXXXX") || GIT_PHASE_ATTEMPT_FILE=""
if [ -n "$GIT_PHASE_ATTEMPT_FILE" ] && exec 9<> "$GIT_PHASE_ATTEMPT_FILE"; then
    trap 'rm -f -- "$GIT_PHASE_ATTEMPT_FILE"' EXIT
else
    GIT_PHASE_ATTEMPT_FILE=""
    exec 9>&2
fi
# Value-free child evidence precedes Git execution; the observer forwards the
# original arguments, streams and exit status, never broadening cleanup policy.
git() {
    local diagnostic_fd=2
    [ ! -e /proc/self/fd/9 ] || diagnostic_fd=9
    if [ ! -r "$RECLAIM_GIT_OBSERVER" ] || [ ! -r "${RECLAIM_GIT_OBSERVER%.sh}.py" ] || ! command -v python3 >/dev/null 2>&1; then
        if [ -n "${GIT_PHASE_ATTEMPT_FILE:-}" ]; then
            printf '%s\n' unknown > "$GIT_PHASE_ATTEMPT_FILE"
        else
            printf '%s\n' 'reclaim-git-observer: unknown observer unavailable' >&"$diagnostic_fd"
        fi
        command git "$@"
        return $?
    fi
    BOSS_RECLAIM_GIT_DIR="${BOSS_RECLAIM_GIT_DIR:-${WORK_MOUNT:-/work}/telemetry/dev-reclaim-git}" \
        bash "$RECLAIM_GIT_OBSERVER" --run "${FUNCNAME[1]:-reclaim}" -- "$@" 3>&"$diagnostic_fd"
}
WORKTREES_DIR="${WORKTREES_DIR:-$REPO_DIR/.claude/worktrees}"
TARGET_DIR="${CARGO_TARGET_DIR:-/scratch/target}"
SCRATCH_MOUNT="${SCRATCH_MOUNT:-/scratch}"
WORK_MOUNT="${WORK_MOUNT:-/work}"
PROC_ROOT="${PROC_ROOT:-/proc}"

for name in SCRATCH_FLOOR_PCT LIVE_TARGET_MIN WORK_FLOOR_GB WORKTREE_MAX_AGE_H STALE_TARGET_H WORKTREE_GRACE_H WORKTREE_IDLE_H; do
    case "${!name}" in
        ''|*[!0-9]*)
            echo "dev-scratch-reclaim: $name must be a whole number, got '${!name}'" >&2
            exit 64
            ;;
    esac
done
for name in SCRATCH_FLOOR_PCT SCRATCH_RECLAIM_HEADROOM_PCT; do
    if [ "${!name}" -gt 100 ]; then
        echo "dev-scratch-reclaim: $name is a percentage, got '${!name}'" >&2
        exit 64
    fi
done

# The /scratch floor in GB for a filesystem of $1 GB. With the knob
# unset it is the file's rule, which has the shape of the estate's own
# disk_tight floor for a cluster node (backlog 4cc39798): the higher of
# a share and a capped headroom share, so on a small disk, where the
# estate's 35% rules, the reclaim still acts first, and on w-1, where
# Longhorn's 27.5% rules, the 30% share does. Set, the knob is a flat
# share. Both sides of that order are pinned at every disk size by
# the_build_floor_lives_once.rs, which computes this rule the same way.
SCRATCH_FLOOR_RULE="${SCRATCH_FLOOR_PCT}%"
[ -n "${BOSS_SCRATCH_FLOOR_PCT:-}" ] ||
    SCRATCH_FLOOR_RULE="${SCRATCH_FLOOR_PCT}%, or ${SCRATCH_RECLAIM_HEADROOM_PCT}% up to ${SCRATCH_RECLAIM_HEADROOM_CEILING_GB}GB"
scratch_floor_gb() {
    local share=$(($1 * SCRATCH_FLOOR_PCT / 100)) headroom
    if [ -n "${BOSS_SCRATCH_FLOOR_PCT:-}" ]; then
        echo "$share"
        return 0
    fi
    headroom=$(($1 * SCRATCH_RECLAIM_HEADROOM_PCT / 100))
    [ "$headroom" -le "$SCRATCH_RECLAIM_HEADROOM_CEILING_GB" ] || headroom=$SCRATCH_RECLAIM_HEADROOM_CEILING_GB
    if [ "$headroom" -gt "$share" ]; then echo "$headroom"; else echo "$share"; fi
}

log() { echo "dev-scratch-reclaim: $*"; }

# Is a build live? Scans /proc for cargo/rustc — no pgrep dependency
# (the CI image ships neither procps nor psmisc), and the pod's
# shareProcessNamespace means this sidecar's /proc lists the dev
# container's processes too. /proc/<pid>/comm is the executable's
# name (truncated to 15 chars, ample for these). A read that races a
# vanishing pid just `continue`s.
build_running() {
    local c n
    for c in /proc/[0-9]*/comm; do
        read -r n < "$c" 2>/dev/null || continue
        case "$n" in cargo|rustc) return 0 ;; esac
    done
    return 1
}

# Free space on a mount, in KB — `df -Pk` for POSIX columns, same as
# the forge sweep. A missing mount is reported and treated as "no
# pressure" so a partial pod layout never turns into a hard failure.
free_kb() {
    local m="$1"
    if [ ! -d "$m" ]; then
        echo ""
        return
    fi
    df -Pk "$m" | awk 'NR==2 {print $4}'
}

# The filesystem's size, in KB — the base the scratch floor's share is
# taken of.
size_kb() {
    df -Pk "$1" | awk 'NR==2 {print $2}'
}

problems=0

# A PROBLEM IS COUNTED AND NAMED IN ONE ACT (backlog 5e5d4be8). Until
# 2026-10-09 every site below bumped the count itself, beside a log
# line, so the packet carried a count and the sidecar's stdout —
# which nobody reads — carried what it counted. Read 2026-10-08 23:58Z:
# packet 0777f3f6 closed `failed` with `problems=2` and no key among its
# 52 saying which two, and every hourly pass that day closed the same
# way (CLAUDE.md §Diagnosis: a verdict must name what failed).
#
# `problem <slug> <sentence>` is the only place the count moves (pinned:
# every_problem_the_pass_counts_is_named_on_the_packet), and it writes
# the line the packet carries as `output`, one per problem, in the shape
# the chore judge already reads: `RED <route> <kind>: <error>`
# (maintenance.chore.file_reds). So the rule
# file-backlog-items-on-dev-scratch-reclaim-red files ONE backlog-item
# per slug while one is open — a floor unmet every hour is one item, not
# twenty-four `failed` packets that reach nobody. The slug is the dedup
# key, so it names the KIND of problem and never the tree or the number.
#
# Flattened and cut to 400 characters at this last step before storage,
# as FF_DETAIL is: the caller has already logged the whole sentence.
PROBLEM_LINES=""
problem() {
    local why
    problems=$((problems + 1))
    why=$(printf '%s' "$2" | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-400)
    PROBLEM_LINES="${PROBLEM_LINES:+$PROBLEM_LINES
}RED dev-scratch-reclaim/$1 problem: $why"
}

# REMOVE A TREE, AS ITS OWNER WHEN THIS PROCESS IS REFUSED (backlog
# cb1cdc61, 5e5d4be8). Measured 2026-10-07 and again 2026-10-08:
# /scratch/target-repo, uid 65534 gid 1500 mode 2755 — the target
# wt-cargo names for a checkout called `repo`, which is what
# infra/dev/as-gate-uid.sh calls the workspace the gate's uid creates.
# The dev container's root holds CAP_SETUID and CAP_SETGID and no
# CAP_DAC_OVERRIDE (as-gate-uid.sh has the measurement), so `rm -rf` was
# refused on every file: about 11,025 stderr lines per build from
# wt-cargo's floor pass, nothing reclaimed, the floor never restored, and
# so the same pass again on the next build.
#
# The capability this process does have is the one that removes it: rm
# runs again as the tree's own uid and gid — the door as-gate-uid.sh
# uses on its own workspace — and then once more as this process, for a
# top directory whose PARENT only this process can write. A container
# with no CAP_SETUID, or no setpriv, fails that retry quietly and the
# refusal stands. It widens nothing: the tree is one a pass had already
# selected and already tried to remove.
#
# SAID ONCE. rm's stderr is captured, never streamed: a refusal the
# retry repaired prints nothing, and one that stands leaves REMOVE_WHY —
# the owner, how many paths refused and the FIRST of them — for the
# caller's one log line and its `problem`. The other lines are that
# line with another filename, and printing them is what buried a
# builder's real output.
REMOVE_WHY=""
remove_tree() {
    local t="$1" out uid="" gid="" mode="" n
    REMOVE_WHY=""
    out=$(rm -rf -- "$t" 2>&1) && return 0
    read -r uid gid mode < <(stat -c '%u %g %a' "$t" 2>/dev/null) || true
    if [ -n "$uid" ] && command -v setpriv >/dev/null 2>&1; then
        setpriv --reuid="$uid" --regid="$gid" --clear-groups rm -rf -- "$t" >/dev/null 2>&1 || true
        rm -rf -- "$t" >/dev/null 2>&1 || true
        if [ ! -e "$t" ]; then
            log "  removed $t as its owner, uid $uid (this process was refused)"
            return 0
        fi
    fi
    n=$(printf '%s\n' "$out" | grep -c . || true)
    REMOVE_WHY="owned by uid ${uid:-unread} gid ${gid:-unread} mode ${mode:-unread}; rm refused ${n:-0} path(s), the first: $(printf '%s\n' "$out" | sed -n '1p')"
    return 1
}

# Totals every pass leaves for the record at the end.
WT_PASS=skipped; WT_PASS_REASON=""
WT_MAIN_SHA=""; WT_MAIN_TS=""
WT_REMOVED=0; WT_REMOVED_MIB=0; WT_REMOVED_BY_CONTENT=0; WT_REMOVED_BY_RECORD=0
WT_KEPT_NO_LANDING=0; WT_KEPT_SOR_UNREAD=0; WT_SOR_UNREAD_REASON=""
WT_KEPT_DIRTY=0; WT_KEPT_DIRTY_NAMES=""
WT_KEPT_UNREFERENCED=0; WT_KEPT_UNREFERENCED_NAMES=""
WT_KEPT_LIVE=0; WT_KEPT_RECENT=0; WT_KEPT_LOCKED=0; WT_KEPT_REFUSED=0
WT_STALE_LOCKS=0; WT_STALE_LOCK_NAMES=""
WT_OWNERLESS_LOCKS=0; WT_OWNERLESS_LOCK_NAMES=""
WT_RELEASED_LOCKS=0; WT_RELEASED_LOCK_NAMES=""
WT_PRUNED=0
WT_KEPT_UNSEEN=0; WT_KEPT_UNSEEN_NAMES=""
# Worktree path -> its admin entry, read from the checkout's held .git;
# and the entries positively held and paired by this pass. No pass runs
# git in any other tree (hold_worktrees, backlogs 0cc54611, b11c5e91).
declare -A WT_ADMIN=() WT_HELD_PAIRED=() WT_REGISTRY=() WT_DUPLICATE=()
WT_ADMIN_READ=0; WT_COMMON=""
WT_ADMIN_WHY=""; WT_HOLD_ROWS=""; WT_HOLD_INVENTORY='[]'; WT_HOLD_TOTAL=0
WT_HOLD_HELD=0; WT_HOLD_SHA256=""; WT_ANCESTORS_HELD=0
WT_HOLD_PENDING_FILE=""
WT_REGISTRY_STATE=not-read; WT_HOLD_STATE=not-read
WT_HOLD_COMPLETE=false
WT_TARGETS_REMOVED=0; WT_TARGETS_MIB=0
FLOOR_WORKTREES_REMOVED=0; FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES=""
STALE_TARGETS_RECLAIMED=0
FLOOR_TARGETS_RECLAIMED=0; FLOOR_TARGETS_MIB=0
INCREMENTAL_DROPPED=0

# ---------------------------------------------------------------------
# THE CHECKOUT ITSELF: /work/boss catches up to origin/main.
# ---------------------------------------------------------------------
# WHY (backlog 033d1fd3, 2026-09-19). Every pod door — boss-api, the
# boss shim, wt-cargo, wt-web — is a symlink into THIS checkout's
# infra/dev, so each runs whatever the checkout last held. Car
# fix/a-door-refuses-to-answer-from-a-stale-checkout gave each door
# door-freshness.sh, which turns a stale copy's silently wrong answer
# into a loud one: a read WARNS and a boss-api write is REFUSED. That
# is the correctness half. The repair was still a human act — `git -C
# /work/boss merge --ff-only origin/main` — and the operator session
# typed it FOUR TIMES on 2026-09-19 alone (session start, 17:05 after
# the 404, 17:40, 18:00) while trains land roughly every 50 minutes. A
# warning everyone expects is a warning nobody reads, which is how the
# class comes back. Mechanical operations belong to the machine
# (CLAUDE.md §Diagnosis), and this sidecar is the machine already
# standing in front of this checkout every hour.
#
# NO NETWORK, for the same reason the passes below take none: the
# sidecar mounts only /work and /scratch, so it has no forge credential
# and an anonymous fetch is `could not read Username` (b50a65ef). It
# fast-forwards to the refs/remotes/origin/main the checkout ALREADY
# has — the very ref door-freshness.sh compares against — so the door
# can call a copy stale only in the window where this pass can repair
# it, and a checkout nobody has fetched for is one neither of them can
# judge. The dev container's own sessions keep the ref fresh (worktrees
# share the object store).
#
# NO GATE HOLDS IT (backlog af27db95, 2026-10-01). Until then the pass
# deferred while a `gate-run` packet was open — narrowed by backlog
# 475fbd10 to one opened in the last 120 s, because "any open gate-run"
# was true 84% of the day and the checkout sat five commits behind — on
# the theory that `boss gate` renders its runner from the tree as it
# launches. The guard locked nothing. `boss gate` takes everything it
# will ever take from a tree in ONE `read_to_string` of the runner
# manifest, the first statement of `gate::run` in
# crates/orchestrators/boss-cli/src/gate.rs, BEFORE its gate-run packet
# is filed, so the window the deferral watched opened only after the
# read it named; and the runner Job clones from the forge and mounts no
# /work (infra/gate-runner/gate-runner.yaml). Measured on the live
# record from 2026-09-23: 16 deferrals (a floor — a pass that only
# deferred filed no packet), each for a gate launched from a builder's
# own worktree, by the in-cluster conductor, or one that had already
# read its manifest — none for a reader still to read this tree. The
# two-hour deadline went with it: it existed only to make a stuck
# deferral loud, and red three passes running on 2026-09-25/26 for a
# stall the deferral itself made. The skips below never had one.
#
# WHAT A FAST-FORWARD CAN TEAR, written here so nobody asks a third
# time: a reader that takes SEVERAL files from this tree in sequence
# can see two versions in one call. Three are known, and none is tied
# to a gate, so no gate-run reading could ever have guarded them:
#   1. a door's multi-file read — boss-api sources door-freshness.sh,
#      its routes, its secret and roll libs one after another, and the
#      boss shim, wt-cargo and wt-web source door-freshness.sh;
#   2. a cargo build run inside /work/boss itself, which compiles each
#      file as it stands when rustc reaches it;
#   3. `boss orient` listing infra/step-plugins under the checkout.
# 1 and 3 are millisecond reads the next call corrects; 2 is a build in
# the checkout the machine owns, and builders build in their own
# worktrees through wt-cargo. A single-file read is never torn: git
# replaces a file with a new inode, so an fd already open keeps the old.
#
# THREE MORE THINGS IT REFUSES, each the same line door-freshness.sh
# draws: a DIRTY checkout not on `main` (a clean one goes back to main
# first — return_checkout_to_main, backlog 88553fea), one that has
# DIVERGED (a commit of its own is a developer's, never this pass's to
# rewind), and one with no origin/main at all. And the last
# lock is git's: `merge --ff-only` refuses rather than overwrite a
# local edit, and that refusal is a PROBLEM — loud, on stderr and on
# the packet — because the checkout then stays behind until someone
# looks. Measured 2026-09-20: /work/boss carried a modified, TRACKED
# .claude/settings.json, so a "clean tree" precondition would have made
# this pass a permanent no-op; git's own narrower refusal (only files
# the merge would overwrite) is the one that keeps it working.
FF_RESULT=skipped
# Git's own complaint when a fast-forward is refused, for the packet.
FF_DETAIL=
FF_TO=""

# Who this pass reads the system of record as (the worktree pass's
# record reads, `sor_read` below). The same platform-admin shape the
# maintenance helpers use, under this sidecar's own id: a scope that
# cannot see the gate-runs and cars it asks about answers a smaller
# world, not the record.
# The machine token's header file: empty until the hourly pass makes it
# (at the bottom, before its first leg), and empty in every other mode.
MT_HDR=""
SOR_USER='{"id":"automation:dev-scratch-reclaim","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# A CHECKOUT SOMEBODY MOVED GOES BACK (backlog 88553fea, 2026-10-20).
# This pass used to skip a checkout off main as "theirs to move back".
# Measured from the /work/boss reflog that day, nobody whose it was knew
# they had moved it: reviewer runs 2233667d and c1a5a342 detached it
# (17:40Z, 19:03Z), and run 2452d6ff switched it to its own branch and
# left it there from 19:04 to 19:17Z — each a bare git whose `cd
# <worktree> &&` the harness dropped — while every door here ran that
# tree, until the operator put it back by hand. The hook
# (infra/dev/hooks/shared-checkout-guard.py) now refuses the git, and
# door-freshness.sh warns while it stands; this is the repair, the same
# arm the fast-forward already is.
#
# CLEAN ONLY: a tracked change in the checkout is somebody's — or a
# stray edit nobody owns, which infra/dev/restore-shared puts back — and
# a checkout carrying one is left exactly as it is and NAMED, a problem
# on the packet with the branch and the files, because every door still
# runs that tree until someone looks. Untracked files are git's to judge:
# `checkout main` refuses rather than overwrite one, and that refusal is
# a problem too. The reflog entry that moved the checkout rides the
# packet (`ff_returned_from`), so the reader learns which run did it
# without reading the reflog by hand.
CHECKOUT_RETURNED=0
FF_RETURNED_FROM=""
return_checkout_to_main() {
    local where="$1" dirty moved out rc
    dirty=$(git -C "$REPO_DIR" status --porcelain --untracked-files=no 2>/dev/null) || dirty="(git status failed)"
    if [ -n "$dirty" ]; then
        dirty=$(printf '%s' "$dirty" | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-300)
        log "PROBLEM: $REPO_DIR is on $where, not main, and carries tracked changes ($dirty) — left as it is; every door symlinked into its infra/dev runs that tree until someone looks" >&2
        FF_RESULT="skipped: not on main, dirty"
        problem checkout-off-main "$REPO_DIR is on $where, not main, with tracked changes left as they are: $dirty"
        return 1
    fi
    # A detached commit no branch holds is a commit of its own — the
    # diverged rule below — and switching away would leave it only in
    # the reflog. Left as it is and named, like a dirty tree.
    if [ "$where" = "a detached HEAD" ] && [ -z "$(git -C "$REPO_DIR" for-each-ref --count=1 --contains HEAD --format=held refs/heads refs/remotes 2>/dev/null)" ]; then
        log "PROBLEM: $REPO_DIR is on a detached HEAD that no branch holds — left as it is; every door symlinked into its infra/dev runs that tree until someone looks" >&2
        FF_RESULT="skipped: not on main, detached commit held by no ref"
        problem checkout-off-main "$REPO_DIR is on a detached HEAD that no branch holds, left as it is"
        return 1
    fi
    moved=$(git -C "$REPO_DIR" log -g -1 --date=iso-strict --format='%h %gd %gs' HEAD 2>/dev/null) || moved=""
    moved=$(printf '%s' "$moved" | tr -d '"\\' | tr '[:cntrl:]' ' ' | cut -c1-400)
    rc=0
    out=$(git -C "$REPO_DIR" checkout -q main 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        out=$(printf '%s' "$out" | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-400)
        log "PROBLEM: returning $REPO_DIR from $where to main FAILED (git exit $rc): $out" >&2
        FF_RESULT="skipped: not on main, return refused"
        problem checkout-off-main "git exit $rc returning $REPO_DIR from $where to main: $out"
        return 1
    fi
    CHECKOUT_RETURNED=1
    FF_RETURNED_FROM="$where; ${moved:-reflog unread}"
    log "returned $REPO_DIR to main from $where — the reflog entry that moved it: ${moved:-unread}"
    return 0
}

# THE TENANT CHECKOUTS CATCH UP TOO (backlog 6b274be1, 2026-10-19). The
# pod keeps a checkout of each tenant repo instances.toml names at
# /work/<the repo's last segment> — the path boss brief hands a tenant
# builder (brief.rs `tenant_checkout`) — and nothing kept it current.
# Measured that day: /work/algedonic-llc at 082d920 (2026-10-07) while
# the forge's main was 3306bbe (2026-10-12), and a triage measured that
# copy and filed a claim the forge had already fixed (b5fb985), costing
# a nomination and a builder run. Its refs/remotes/origin/main WAS
# current — a session's fetch had moved it — so the repair is this
# pass's own: a fast-forward to the ref the checkout already holds, NO
# NETWORK (the sidecar has no forge credential, b50a65ef).
#
# ONE DIFFERENCE FROM THE PASS BELOW, ON PURPOSE: no door judges a
# tenant copy the way door-freshness.sh judges /work/boss, so a tenant
# checkout this pass cannot bring current — on another branch, diverged,
# or refused by git — would go stale in silence again. Each is a PROBLEM
# named on the packet. A checkout AHEAD of its ref is somebody's next
# push, never stale, and one this pod does not hold is not a problem:
# not every pod clones every tenant. Nothing here switches a branch.
#
# The instances file is the one beside this script, so the sidecar reads
# the declaration of the tree it runs; BOSS_INSTANCES (the converge's own
# spelling, infra/forge/cluster-deploy-lib.sh) names another for a test.
# A routine fast-forward files no packet, like the checkout's own; its
# verdict rides any packet the pass files, as `tenant_ff`.
TENANT_INSTANCES="${BOSS_INSTANCES:-$(dirname "$(readlink -f "$0")")/instances.toml}"
TENANT_FF=""
fast_forward_tenant_checkouts() {
    local repo ref dir head target behind out rc branch verdict
    [ -r "$TENANT_INSTANCES" ] || {
        log "tenant checkouts: no instances file at $TENANT_INSTANCES — none judged"
        return 0
    }
    while read -r repo ref; do
        dir="$WORK_MOUNT/${repo##*/}"
        verdict=""
        if [ ! -e "$dir/.git" ]; then
            verdict="absent"
        elif ! target=$(git -C "$dir" rev-parse --verify -q "refs/remotes/origin/$ref" 2>/dev/null); then
            verdict="skipped: no refs/remotes/origin/$ref (git fetch origin in the dev container)"
        else
            branch=$(git -C "$dir" symbolic-ref --quiet --short HEAD 2>/dev/null) || branch="a detached HEAD"
            head=$(git -C "$dir" rev-parse HEAD 2>/dev/null) || head=""
            if [ "$branch" != "$ref" ]; then
                verdict="stuck: on $branch, not $ref"
            elif [ "$head" = "$target" ]; then
                verdict="current at ${target:0:8}"
            elif git -C "$dir" merge-base --is-ancestor "$target" "$head" >/dev/null 2>&1; then
                verdict="ahead of origin/$ref at ${head:0:8}"
            elif ! git -C "$dir" merge-base --is-ancestor "$head" "$target" >/dev/null 2>&1; then
                verdict="stuck: ${head:0:8} has diverged from origin/$ref ${target:0:8}"
            else
                behind=$(git -C "$dir" rev-list --count "$head..$target" 2>/dev/null) || behind="?"
                rc=0
                out=$(git -C "$dir" merge --ff-only "$target" 2>&1) || rc=$?
                if [ "$rc" -eq 0 ]; then
                    verdict="fast-forwarded ${head:0:8} -> ${target:0:8} ($behind behind)"
                else
                    log "tenant checkout: fast-forward FAILED (git exit $rc) taking $dir from ${head:0:8} to ${target:0:8}. git said: $out" >&2
                    out=$(printf '%s' "$out" | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-300)
                    verdict="stuck: git exit $rc taking ${head:0:8} to ${target:0:8}: $out"
                fi
            fi
        fi
        log "tenant checkout $dir ($repo@$ref): $verdict"
        TENANT_FF="${TENANT_FF:+$TENANT_FF; }${repo##*/}: $verdict"
        case "$verdict" in
            stuck:*) problem tenant-checkout-stuck "the tenant checkout $dir ($repo@$ref) cannot be brought current, and no door warns on it: ${verdict#stuck: }" ;;
        esac
    done < <(awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*\[/ { if (repo != "" && ref != "") print repo, ref; repo = ""; ref = ""; next }
        /^[[:space:]]*tenant_repo[[:space:]]*=/ { split($0, q, "\""); repo = q[2] }
        /^[[:space:]]*tenant_ref[[:space:]]*=/ { split($0, q, "\""); ref = q[2] }
        END { if (repo != "" && ref != "") print repo, ref }
    ' "$TENANT_INSTANCES" | sort -u)
}

fast_forward_checkout() {
    local branch head main behind out rc behind_word
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "fast-forward skipped: $REPO_DIR is not a git checkout"
        FF_RESULT="skipped: not a checkout"
        return 0
    fi
    branch=$(git -C "$REPO_DIR" symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=""
    if [ "$branch" != main ]; then
        return_checkout_to_main "${branch:-a detached HEAD}" || return 0
    fi
    head=$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null) || head=""
    main=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>/dev/null) || main=""
    if [ -z "$head" ] || [ -z "$main" ]; then
        log "fast-forward skipped: $REPO_DIR has no HEAD or no refs/remotes/origin/main (git fetch origin in the dev container)" >&2
        FF_RESULT="skipped: no origin/main"
        return 0
    fi
    if [ "$head" = "$main" ]; then
        FF_RESULT=current
        return 0
    fi
    if ! git -C "$REPO_DIR" merge-base --is-ancestor "$head" "$main" >/dev/null 2>&1; then
        log "fast-forward skipped: $REPO_DIR at ${head:0:8} has diverged from origin/main ${main:0:8} — a commit of its own is a developer's, never this pass's to rewind"
        FF_RESULT=diverged
        # ...and a PROBLEM, not only a log line (review e5607585, finding
        # 2b). door-freshness.sh declines to judge a diverged checkout, so
        # this line was the only word that every door ran a tree
        # origin/main does not hold — and a builder call whose cd the
        # harness dropped leaves exactly this with one `git commit -am`
        # in a script. Never rewound from here; named, with what it
        # carries, for whoever reads the packet.
        local carries
        carries=$(git -C "$REPO_DIR" log --format='%h %s' -3 "$main..$head" 2>/dev/null | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-300)
        problem checkout-diverged "$REPO_DIR at ${head:0:8} has diverged from origin/main ${main:0:8}, left as it is; it carries: ${carries:-unread}"
        return 0
    fi
    FF_TO="$main"
    behind=$(git -C "$REPO_DIR" rev-list --count "$head..$main" 2>/dev/null) || behind=""
    case ${behind:-empty} in
        empty | *[!0-9]*) behind=1 ;;
    esac
    if [ "$behind" = 1 ]; then behind_word=commit; else behind_word=commits; fi

    rc=0
    out=$(git -C "$REPO_DIR" merge --ff-only "$main" 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        log "fast-forward FAILED (git exit $rc) taking $REPO_DIR from ${head:0:8} to ${main:0:8}; the doors keep warning until someone looks. git said: $out" >&2
        # GIT'S OWN WORDS RIDE THE PACKET, not only the journal
        # (backlog 6db0b658; David, 2026-09-22 — accept the jam and make
        # the refusal loud). `ff_result` carried `failed: exit 1`, which
        # is a verdict a reader must go and re-derive: CLAUDE.md
        # §Diagnosis, "a verdict must name what failed". The journal had
        # the answer all along and the packet is what anyone reads.
        #
        # BOUNDED AND FLATTENED, because this is interpolated into JSON
        # by the step writer: newlines and tabs to spaces, quotes and
        # backslashes dropped, 400 characters. The reduction is at the
        # LAST step before storage and the full text is already in the
        # log above — never the only copy.
        FF_DETAIL=$(printf '%s' "$out" | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-400)
        FF_RESULT="failed: exit $rc"
        problem fast-forward-refused "git exit $rc taking $REPO_DIR from ${head:0:8} to ${main:0:8}: $FF_DETAIL"
        return 0
    fi
    FF_RESULT=ok
    log "fast-forward: $REPO_DIR moved ${head:0:8} -> ${main:0:8} ($behind $behind_word), so every door symlinked into its infra/dev runs the tree's own copy"
}

# ---------------------------------------------------------------------
# THE SHARED CHECKOUT IS HELD BY ITS OWNER (backlog c19ee02b; measured
# on the pod 2026-10-21 05:00Z by run bf97aae8).
# ---------------------------------------------------------------------
# /work/boss is what root runs: this script (hourly, holding the machine
# token), every door /work/tools/bin links into its infra/dev, the login
# menu infra/dev/dev-session.sh, the hooks .claude/ hands every session,
# and git's own config and hooks under .git, which every git command a
# root session or this pass runs reads. That day, outside the worktrees'
# contents, 7,972 of its 56,785 entries were group- or other-writable:
# 1,209 directories drwxrwsr-x root:1500 with no sticky bit, and files
# among them infra/dev/sor-url (where the doors and this pass send the
# machine token) and .git/config at 0664, and 5,280 node_modules files
# at 0666. Every builder under as-gate-uid is group 1500, so it could
# rename a door aside and plant its own, point the token at another
# host, set core.fsmonitor, or drop a hook into .git/hooks, and root
# would run what it left. Not one of those entries was owned by any uid
# but root, and root is the only uid that writes there: git refuses a
# root-owned repository to any other uid as dubious ownership, which is
# why as-gate-uid.sh bundles instead.
#
# Nothing wrote those modes on purpose (the pod's fsGroup and an older
# umask did), so nothing kept them. This pass runs as their owner (root
# with every capability dropped and the pod's group 1500 beside its own,
# so an owner's chmod needs nothing and keeps setgid) and holds them
# every hour:
#   * group and other write come off the checkout and everything in it
#     except the worktrees' own contents. Setgid, the owner's bits and
#     everyone's read stay, so root's git, the harness's `git worktree
#     add` and the fast-forward above write exactly as before.
#   * each worktree under WORKTREES_DIR, and everything in it, is left
#     as it is: a builder's tree, which group members build in (uid
#     65534 had written apps/web/dist in two that day). The directory
#     holding them is held, so no one but root adds a tree beside them.
#   * an entry this uid does not own cannot be held without CAP_CHOWN:
#     a problem, by path and owner, every pass. A planted hook or door
#     is the finding itself, not noise. So is a checkout that is a link
#     or not a directory: what a group member leaves after moving the
#     real one aside.
#   * the directory above the checkout is NOT this pass's: making the
#     work mount sticky, so the checkout cannot be moved aside whole, is
#     the doors' tree's hold (backlog 3db83d0d), one owner for one mode.
# Holding modes is not reclaiming: like the fast-forward it files no
# packet of its own, and `checkout` rides every packet a pass does file.
# No checkout at all (a fresh pod) is not a problem.
CHECKOUT_STATE=""

hold_checkout() {
    local me entry mode held=0 open=""
    if [ ! -e "$REPO_DIR" ] && [ ! -L "$REPO_DIR" ]; then
        CHECKOUT_STATE="absent"
        log "shared checkout: absent, no $REPO_DIR"
        return 0
    fi
    me=$(id -u)
    if [ -L "$REPO_DIR" ] || [ ! -d "$REPO_DIR" ]; then
        open="$REPO_DIR is not a directory"
    else
        # Every entry another uid owns, a link included: none can be held.
        while IFS= read -r -d '' entry; do
            open="${open:+$open; }${entry#* } is owned by uid ${entry%% *}"
        done < <(find "$REPO_DIR" -xdev -path "$WORKTREES_DIR/*" -prune \
            -o ! -uid "$me" -printf '%U %p\0' 2>/dev/null)
        # A link has no mode of its own; the directory it sits in guards it.
        while IFS= read -r -d '' entry; do
            mode=${entry%% *}
            entry=${entry#* }
            if chmod go-w -- "$entry" 2>/dev/null; then
                held=$((held + 1))
                log "shared checkout: took group and other write off $entry (was $mode)"
            else
                open="${open:+$open; }chmod go-w $entry failed (mode $mode)"
            fi
        done < <(find "$REPO_DIR" -xdev -path "$WORKTREES_DIR/*" -prune \
            -o ! -type l -uid "$me" -perm /022 -printf '%m %p\0' 2>/dev/null)
    fi
    if [ -n "$open" ]; then
        CHECKOUT_STATE="OPEN: $(printf '%s' "$open" | cut -c1-400)"
        log "PROBLEM: the shared checkout is NOT held: $open; a group member may still replace what root runs from it" >&2
        problem checkout-open "the shared checkout is not held: $open; a group member may still replace what root runs from it"
        return 0
    fi
    CHECKOUT_STATE="held"
    [ "$held" -eq 0 ] || CHECKOUT_STATE="held, re-held $held this pass"
    log "shared checkout: $CHECKOUT_STATE ($REPO_DIR and everything in it but the worktrees' contents not group- or other-writable)"
}

# ---------------------------------------------------------------------
# NO CLIENT CONFIG IN THE DEV HOME (backlog 2b1bf9b4; review 4ceac0c8 of
# car 4c4e23bf, 2026-10-21).
# ---------------------------------------------------------------------
# /work/home is HOME for root here — the doors, this pass, every
# interactive shell — and group 1500, every builder's group, may create
# files in it: a sticky bit stops a rename, not a new file. curl reads
# $HOME/.curlrc, wget $HOME/.wgetrc, and curl, wget and git (through
# libcurl) $HOME/.netrc, none with an ownership check. A `proxy =` line
# planted in a fixture home's .curlrc sent a request for 127.0.0.1:9 to
# 127.0.0.1:1, and infra/dev/boss-api's curl carries the machine token.
# The doors now pass curl -q first (infra/lib/curl-through-a-roll.sh,
# infra/dev/dev-session.sh), so they read no curlrc; this is the alarm
# for every other reader and for the attempt itself. Nothing in the
# estate writes any of the three here, so ONE existing is the finding,
# named by path, owner and type every pass. It removes nothing: the file
# is the evidence, and who left it is a person's question. No home (a
# fixture, a fresh pod) names nothing.
#
# ROOT'S GIT CONFIG IS DIFFERENT: one is meant to exist (backlog
# aa1f7d7c). A group member who can write it can set a credential
# helper, core.fsmonitor, core.sshCommand or an include, and root's git
# runs what it names. Root's git on this pod reads one of TWO places:
#
#   - $WORK_MOUNT/.gitconfig: the dev container sets
#     GIT_CONFIG_GLOBAL=/work/.gitconfig (infra/cluster/manifests/
#     boss-dev.yaml), and with that variable set git reads the named
#     file and ignores $HOME's entirely. Every process that inherits the
#     container env reads it: a kubectl exec, the container's own tree,
#     the agent daemons (4 live root processes carried it, review
#     ef57ab2e of car 335ace12, 2026-10-22, stat only: root:1500 664).
#     The pass derives it from WORK_MOUNT, because the sidecar mounts
#     the same /work; the test
#     the_container_git_config_global_is_held_by_its_owner reads the
#     manifest's value and plants its fixture THERE, so a manifest that
#     moves GIT_CONFIG_GLOBAL fails that test rather than leaving this
#     pass holding the old file (CLAUDE.md 9a).
#   - $DEV_HOME/.gitconfig and $DEV_HOME/.config/git/config: what root's
#     git reads where that variable is NOT set — a login or tmux shell
#     with HOME=/work/home (measured 2026-10-21: root:1500 664). This
#     pass is not among those readers: the reclaim sidecar sets neither
#     HOME nor GIT_CONFIG_GLOBAL, so its own git reads the image's.
#
# So hold_root_gitconfigs, run first, takes group and other WRITE off
# each as its owner every pass, the way the shared checkout is held
# above; read stays, since the file holds no secret and only a write can
# plant a command. What it cannot hold — a link, not a regular file,
# another uid's (what a member leaves after moving the real one aside
# while its directory is not sticky, or creates where none was), a chmod
# that failed — the audit names on the same RED line, by full path.
# Neither reads one.
DEV_HOME="${BOSS_DEV_HOME:-$WORK_MOUNT/home}"
ROOT_GIT_CONFIGS=(
    "$WORK_MOUNT/.gitconfig"
    "$DEV_HOME/.gitconfig"
    "$DEV_HOME/.config/git/config"
)

hold_root_gitconfigs() {
    local p mode
    for p in "${ROOT_GIT_CONFIGS[@]}"; do
        if [ ! -e "$p" ] && [ ! -L "$p" ]; then
            log "root's git config: absent, no $p"
            continue
        fi
        # Not ours to hold: the audit below names each of these.
        [ -f "$p" ] && [ ! -L "$p" ] && [ "$(stat -c %u "$p")" = "$(id -u)" ] || continue
        mode=$(stat -c %a "$p")
        if [ $((8#$mode & 8#022)) -eq 0 ]; then
            log "root's git config: held ($p not group- or other-writable)"
        elif chmod go-w -- "$p"; then
            log "root's git config: took group and other write off $p (was $mode)"
        else
            log "root's git config: chmod go-w $p failed (mode $mode)" >&2
        fi
    done
}

audit_home_client_config() {
    local name p mode named="" me
    me=$(id -u)
    for name in .curlrc .netrc .wgetrc; do
        p="$DEV_HOME/$name"
        [ -e "$p" ] || [ -L "$p" ] || continue
        named="${named:+$named; }$p ($(stat -c 'uid %u, mode %a, %F' "$p" 2>/dev/null || echo 'unreadable'))"
    done
    for p in "${ROOT_GIT_CONFIGS[@]}"; do
        if [ -L "$p" ] || { [ -e "$p" ] && [ ! -f "$p" ]; }; then
            named="${named:+$named; }$p ($(stat -c 'uid %u, %F' "$p" 2>/dev/null || echo 'unreadable'))"
        elif [ -f "$p" ]; then
            mode=$(stat -c %a "$p" 2>/dev/null || echo 7777)
            if [ "$(stat -c %u "$p" 2>/dev/null)" != "$me" ] || [ $((8#$mode & 8#022)) -ne 0 ]; then
                named="${named:+$named; }$p ($(stat -c 'uid %u, mode %a, %F' "$p" 2>/dev/null || echo 'unreadable'))"
            fi
        fi
    done
    if [ -z "$named" ]; then
        log "dev home's client config: none ($DEV_HOME holds no .curlrc, .netrc or .wgetrc, and root's git configs, where present, are held by uid $me: ${ROOT_GIT_CONFIGS[*]})"
        return 0
    fi
    log "PROBLEM: client config that root's curl, wget or git reads and group 1500 may have written: $named" >&2
    problem home-client-config "client config that root's curl, wget or git reads and group 1500 may have written: $named"
}

# ---------------------------------------------------------------------
# WORK + SCRATCH, the 364 GB: worktrees whose branch the forge no
# longer has, and the target each one kept alive.
# ---------------------------------------------------------------------
# Measured 2026-09-18 (backlog 1933db9e): 203 worktrees under /work/boss
# — 83 of them coding agents' under .claude/worktrees/, 20 on a detached
# HEAD — carrying 182 distinct branches of which 175 no longer existed
# on the forge; 36 target-* dirs; 364 GB of a 929 GB disk at 77%. The
# forge deletes a car's branch when it lands, so "the forge has no such
# branch" is the fact that says the checkout's work is over — and the
# floor passes could not act on it: /scratch stood 221 GB above its
# floor with a third of the disk holding caches for branches that had
# landed days before. This pass runs on that fact regardless of free
# space, the way the stale-target pass runs on age.
#
# THREE THINGS EVERY REMOVAL REQUIRES, and any one missing keeps it:
#   1. the branch is GONE, read from the checkout's OWN refs and never
#      from the forge: a worktree's branch is gone when the checkout
#      holds no refs/remotes/origin/<branch> AND EITHER its head is an
#      ancestor of refs/remotes/origin/main (LANDED — the forge sweeps
#      a car's branch when it merges) OR git has been quiet in it for
#      WORKTREE_MAX_AGE_H (ABANDONED unpushed — the harness's own
#      `worktree-agent-*` branches never reach the forge at all). A
#      worktree on `main` or a detached HEAD has no branch to be gone,
#      so it is judged by (2) alone with the longer WORKTREE_IDLE_H
#      window. The pass first shipped (H11) took this from ONE `git
#      ls-remote --heads origin`, and on the pod that read failed
#      every hour: the reclaim sidecar mounts only /work and /scratch
#      (boss-dev.yaml) — no forge token, no HOME carrying the
#      credential helper — and the forge answers an anonymous
#      info/refs with 401, so git asked for a username it had no
#      terminal to read (`fatal: could not read Username for
#      'http://10.20.0.15:3000'`, exit 128; measured 2026-09-18,
#      backlog b50a65ef). The pass skipped in silence and the three
#      newest packets read `worktree_pass=skipped` with no reason.
#      The refs are what the operator's session and every builder
#      worktree fetch (they share one object store), so the pass is
#      ONLY AS FRESH AS THE DEV CONTAINER'S LAST FETCH — it records
#      the origin/main sha and its commit time on the packet so a
#      stale read is visible — and a landed branch's origin/ ref
#      outlives the forge's copy until a fetch prunes it (fetch.prune
#      is not set on the pod), which keeps a worktree, never removes
#      one: every error here is on the side of keeping. SINCE 2026-09-25
#      (backlog 4a738ca5) a head NOT on origin/main is still LANDED when
#      its work is — every file it changed byte-identical on main, or
#      every commit of its own there by patch-id (`landed_by_content`)
#      — because a rebase and a train's squash leave the checkout's own
#      sha on no origin ref at all, and 56 of 58 measured heads were;
#      and where git cannot see a landing at all, the SYSTEM OF RECORD
#      may (backlog 9a044141, `landed_on_record`): the tree's car merged,
#      or the builder run that gated from it finished. A record that
#      cannot be read is no landing, and keeps;
#   2. git has been QUIET in it for the window (1) chose — no commit,
#      no HEAD or index write, no new reflog ENTRY (an entry's own
#      time, never the file's mtime, which a gc rewrites in every
#      worktree at once — backlog adce5171) — because a builder's tree
#      is clean for the minutes between its commit and its push;
#   3. the tree is CLEAN: `git status --porcelain` empty, untracked
#      files included. A dirty tree is kept and NAMED with its count,
#      in the log and on the packet, so an operator can decide. A
#      DETACHED tree must also have its head held by some ref, or it
#      is kept and named the same way: the checkout is the only thing
#      naming those commits (adce5171).
# Live-locked worktrees, the main checkout and the one this run stands
# in are never candidates — and the lock is what keeps a RUNNING
# agent's tree: the Claude harness locks each agent worktree with its
# pid (`claude agent agent-<id> (pid N start T)`) for the session's
# life. A lock is a claim by a PROCESS, so it is judged against the
# process table (`lock_stale_why`): a harness lock whose pid is gone,
# or started at another time than it recorded, is STALE, and the tree
# is judged like any other — (1) to (3) all still apply — and named.
# A lock reading only git's `added with --lock` names no owner at all
# and is judged the same way, behind at least the unpushed window
# (OWNERLESS_LOCK, backlog f66227aa). A harness lock whose pid lives is
# still released when the record says the agent is done — every run that
# gated from the tree closed, a grace ago (`harness_lock_released`,
# backlog 3a84cbf5), because the pid it names is the operator's session.
# Only a tree about to be removed is unlocked, and it is re-locked if
# git then refuses, so a stale lock the pass keeps stays as it was.
# `git worktree remove` still runs without --force, a second lock on
# (3).
#
# A PASS THAT CANNOT ANSWER — no refs/remotes/origin/main, git refusing
# — removes nothing AND RECORDS IT: `worktree_pass=skipped` with the
# reason beside it, counted as a problem so the packet is filed
# (result=incomplete). A skipped pass is a finding, not silence.
#
# WHAT GOES WITH IT: the per-worktree cargo target. wt-cargo names it
# `$WT_TARGET_ROOT/target-<basename of the worktree>` (infra/dev/
# wt-cargo), so `worktree_target` derives the same name here — one
# shape in two files, pinned by the test at each end; the primary
# $TARGET_DIR is never a candidate. A `.seeding` sibling wt-cargo left
# from a killed copy goes too. The local BRANCH is left alone: it
# costs nothing, and deleting refs is a different decision.

# Newest git activity in a worktree, as epoch seconds: its HEAD
# commit's time, the mtimes of the worktree's own HEAD and of its
# directory, and the time of the NEWEST ENTRY in its reflog.
#
# The INDEX is not read (backlog e14a741c). A `git status` rewrites a
# CLEAN tree's index to refresh its stat cache — the harness snapshots
# every session's status as it starts, and this pass's own dirty check
# is a status — so its mtime says someone LOOKED: measured 2026-09-23,
# five worktrees' index files written 2026-09-21 17:11, days after
# their last commit. What the index can hold that HEAD does not is a
# staged change, and the dirty guard keeps that at any age; a commit,
# checkout or reset that writes it also writes a reflog entry, read
# below for its own time.
#
# The reflog is read for what it SAYS, never for its mtime (backlog
# adce5171). Until 2026-09-23 this took the mtime of $gitdir/logs/HEAD,
# and a repo-wide `git gc` rewrites that file in EVERY worktree at once
# — its `reflog expire --all` copies each worktree's entries to a new
# file whether or not one expires. Measured 2026-09-21: the pass kept
# 373 of 397 worktrees "with git activity inside the window", and three
# unrelated ones whose HEAD, index and directory were 73h, 80h and 101h
# quiet all read logs/HEAD EXACTLY 32h old. Re-measured 2026-09-23: 154
# reflogs rewritten inside four seconds at 2026-09-22 05:43:30Z, beside
# gc's writes of info/refs and objects/info. Because the measure is a
# MAX, one such touch reset the idle clock of the whole population, so
# a pass could only ever remove what a window shorter than the gc
# interval let through. Each reflog line carries the time git wrote it
# (`<old> <new> <ident> <epoch> <tz><TAB><message>`), and an expire
# copies lines without redating them, so the last line's epoch is the
# last act in THIS worktree and nothing another command did to the file.
#
# Git reads the tree through its ADMIN ENTRY, never through the tree's
# .git file (backlog 0cc54611, `worktree_admin`): a tree no entry names
# reads as active now, so it is kept.
worktree_last_activity() {
    local path="$1" gitdir f t newest
    if ! gitdir=$(worktree_admin "$path"); then
        date +%s
        return 0
    fi
    newest=$(git --git-dir="$gitdir" --work-tree="$path" log -1 --format=%ct 2>/dev/null || echo 0)
    for f in "$gitdir/HEAD" "$path"; do
        [ -e "$f" ] || continue
        t=$(stat -c %Y "$f" 2>/dev/null || echo 0)
        [ "$t" -gt "$newest" ] && newest=$t
    done
    t=0
    if [ -f "$gitdir/logs/HEAD" ]; then
        t=$(tail -n 1 "$gitdir/logs/HEAD" 2>/dev/null | cut -f1 | awk '{print $(NF-1)}')
    fi
    case ${t:-empty} in empty|*[!0-9]*) t=0 ;; esac
    [ "$t" -gt "$newest" ] && newest=$t
    echo "${newest:-0}"
}

# Is the process table this pass reads the POD's? The sidecar sees the
# dev container's processes only because the pod sets
# shareProcessNamespace (infra/cluster/manifests/boss-dev.yaml), and
# then pid 1 is the pod's `pause`. In a namespace of its own pid 1 is
# the sidecar's own entrypoint, every harness pid would read as gone,
# and every LIVE agent's lock as stale — so no lock is judged there.
proc_view_is_pods() {
    local c=""
    read -r c < "$PROC_ROOT/1/comm" 2>/dev/null || return 1
    [ "$c" = pause ]
}

# Is a worktree lock STALE? Prints why and succeeds when it is; fails —
# keep the lock — for a live one AND for any lock it cannot judge.
# Only the harness's shape is judged, `... (pid N start T)` with T the
# process's start time in clock ticks since boot (field 22 of
# /proc/<pid>/stat, world-readable where /proc/<pid>/cwd is not): pid
# gone, or pid alive with another start, means the locker is gone —
# measured 2026-09-23 (backlog e14a741c), two locks named pid 355 start
# 94472290 while pid 355 had started at 95092646, and their trees were
# kept forever. Any other reason is a human's lock and is never judged.
# The comm field is split at its LAST `) `, since a comm may hold one.
lock_stale_why() {
    local reason="$1" pid start line rest now_start
    [[ "$reason" =~ \(pid\ ([0-9]+)\ start\ ([0-9]+)\) ]] || return 1
    pid="${BASH_REMATCH[1]}"; start="${BASH_REMATCH[2]}"
    if [ ! -e "$PROC_ROOT/$pid" ]; then
        echo "pid $pid is gone"
        return 0
    fi
    read -r line < "$PROC_ROOT/$pid/stat" 2>/dev/null || return 1
    rest="${line##*) }"
    now_start=$(awk '{print $20}' <<<"$rest")
    case ${now_start:-empty} in empty|*[!0-9]*) return 1 ;; esac
    [ "$now_start" = "$start" ] && return 1
    echo "pid $pid started at $now_start, not the $start the lock recorded"
}

# A LOCK THAT NAMES NO OWNER (backlog f66227aa). `git worktree add --lock`
# with no `--reason` writes git's own sentence and nothing else — no pid,
# no person, no run (builtin/worktree.c). Measured 2026-10-09 20:14Z: 139
# of 315 worktrees carried exactly this text, 75 of them under /work/wt,
# every one idle 98h to 185h and 73 clean, while /work stood at 11 GB
# free against its 18 GB floor; the pass kept them all as a human's lock,
# removed nothing, and gate.sh's 12 GB floor refused every builder's
# pre-flight. (The Claude harness's own lock is the pid shape above; all
# 47 of those were live that day.) There is no owner to ask, so the tree
# answers instead: it is judged like an unlocked one — every guard still
# runs — but never sooner than WORKTREE_MAX_AGE_H, whatever the branch or
# the floor says, because the floor's live window is minutes and an owner
# the lock does not name may sit that long in a clean tree. The process
# table is not read. Any OTHER text, an empty one included (`git worktree
# lock` with no reason is an act someone took after the add), is still
# somebody's lock and is never judged.
OWNERLESS_LOCK="added with --lock"

# The first ref that holds a commit — the one question the detached-HEAD
# guard asks, in whichever pass is about to remove a checkout (backlog
# adce5171 for the gone-worktree pass, 5da0428a for the floor pass).
# Empty for an empty sha, for a commit no ref holds, and for a git that
# cannot answer: every one of them reads as "no ref", and keeps.
# `--count=1` stops at the first ref that holds it.
ref_holding() {
    [ -n "$1" ] || return 0
    git -C "$REPO_DIR" for-each-ref --count=1 --format='%(refname)' --contains "$1" 2>/dev/null || true
}

# Is a head's WORK on origin/main although the head itself is not?
# Prints how — `content` or `patch-id` — and succeeds; fails for
# anything else, including a git that cannot answer, so every error
# here reads as "not landed" and keeps.
#
# WHY (backlog 4a738ca5). Measured 2026-09-25 04:10Z: under the /work
# floor a pass removed 11 worktrees and kept 286 "recent" while /work
# sat at 12 GB free of 40, and of 58 sampled worktree heads only 2 were
# on any origin ref. Builders commit on local `worktree-agent-*`
# branches, `boss gate --rebase` replays the commit onto a new sha, and
# a train squashes its cars into one commit — so no sha a worktree holds
# ever reaches origin/main, the ancestry test above called every landed
# tree UNPUSHED, and each waited the 48h abandoned window, not the
# landed one.
#
# The two rules are `boss merged`'s (crates/orchestrators/boss-cli/src/
# merged.rs), which settled this question for cars, in the same terms:
#   CONTENT — every file the head changed since its merge-base with main
#     is byte-identical on main. The only signal that survives a train:
#     a squash of N cars has a patch-id equal to no single car's.
#   PATCH-ID — `git cherry` finds every commit of the head's own on main
#     by patch-id. The one that survives a LATER car editing the file.
# A head with no commits of its own is the ancestry case, not this one;
# a head whose commits change no file answers nothing and is kept. A
# SOME-files match is not a landing either (a sibling car editing one
# of the files looks the same), so it keeps, as `boss merged` reports
# Unknown for it. Content runs first because it is two diffs; the
# cherry computes a patch-id for every main commit since the base.
landed_by_content() {
    local head="$1" main="$2" base cherry
    local -a files=()
    base=$(git -C "$REPO_DIR" merge-base "$main" "$head" 2>/dev/null) || return 1
    [ -n "$base" ] || return 1
    mapfile -d '' files < <(git -C "$REPO_DIR" diff --name-only -z "$base" "$head" 2>/dev/null)
    if [ "${#files[@]}" -gt 0 ] &&
        git --literal-pathspecs -C "$REPO_DIR" diff --quiet "$main" "$head" -- "${files[@]}" 2>/dev/null; then
        echo content
        return 0
    fi
    cherry=$(git -C "$REPO_DIR" cherry "$main" "$head" 2>/dev/null) || return 1
    [ -n "$cherry" ] || return 1
    case $'\n'"$cherry" in *$'\n+'*) return 1 ;; esac
    echo patch-id
}

# LANDED ON RECORD (backlog 9a044141). A head whose work neither git
# test above can see on origin/main may still have landed — or its
# builder may be done with it — and the system of record says which.
# Measured 2026-09-25 16:15-16:40Z: /work at 14 GB free against its
# 18 GB floor, 297 worktrees under .claude/worktrees, 253 a named branch
# with no origin/ ref and a head on no origin ref, and
# `landed_by_content` called 0 of the 218 idle ones landed: a train
# squashes N cars into one commit, `boss gate --rebase` replays each,
# and a later car edits the same files, so neither the content rule nor
# the patch-id rule survives. Every finished builder's tree waited the
# 48h unpushed window, even under the floor, where a landed one waits
# 30 minutes — ~6 builder trees an hour times 48h is the ~290 measured.
#
# THREE READS, each a fact the record already holds, cheapest first:
#   1. the CAR on the tree's own branch (`kind=ship-a-change`,
#      `metadata={"branch":...}`) — landed when closed with
#      outcome=merged or stamped `merged` by the conductor as its train
#      merges; the one definition is `boss_jobs::car::is_landed`
#      (crates/core/boss-jobs/src/car.rs), read here the same way. The
#      stamp comes with the train's id, so the car IS the train's word.
#   2. the GATE-RUNS launched from this worktree (`kind=gate-run`,
#      `metadata={"worktree":...}` — `boss gate` stamps the path it
#      launched from, flat, beside `branch` and `agent_run`; the run's
#      `building` step copies them as `gate_run.worktree`), and for each
#      the AGENT-RUN it names: a CLOSED run is a finished builder —
#      landed on its green, reported, refused or died — whose checkout
#      holds nothing its branch ref does not (the dirty guard below
#      still keeps an uncommitted edit).
#   3. the CAR on each branch those gate-runs carried, for a builder
#      that committed on the harness's `worktree-agent-*` branch and
#      pushed under the car's name.
# The answer only moves the WAIT from the unpushed window to the landed
# one. Every guard after it still runs — quiet, clean, a detached head
# some ref holds, a stale lock judged, `git worktree remove` without
# --force — and the local branch outlives its checkout, so removing a
# clean tree the record calls done costs at most a `git worktree add`.
#
# FAILS CLOSED. A read that errors, answers a shape with no `.data`, or
# returns fewer rows than its `.total` (a limit is not a filter) is NO
# landing, and the tree keeps its unpushed window, as it did before the
# record was asked. The first such read ends the asking for the pass
# (WT_SOR_UNREAD_REASON latches): a dark API asked once per tree would
# spend a retry window on each of hundreds. The reader is the one the
# pass files its packet through — BOSS_JOBS_URL, else
# infra/dev/sor-url beside this checkout, through boss-api-curl.sh as
# the sidecar's platform-admin identity — never an address of its own.

# GET one jobs-API path; prints the body. Fails with curl's exit, or 3
# when no system of record is named.
sor_read() {
    local here url api
    local -a timeout_args=()
    [ "$#" -lt 2 ] || timeout_args=(--max-time "$2" --connect-timeout 5)
    here="$(dirname "$(readlink -f "$0")")"
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    [ -n "$url" ] || return 3
    api="$here/../boss-api-curl.sh"
    [ -x "$api" ] || api=boss-api-curl.sh
    # MT_HDR: the machine token's header file, made once at the top
    # level of the hourly pass (below, before its first leg) — this
    # function runs inside `$( )`, where the reader refuses a first call.
    # Empty wherever no token is mounted, and then this is the read it
    # always was.
    "$api" -fsS "${timeout_args[@]}" -H "x-boss-user: $SOR_USER" ${MT_HDR:+-H "$MT_HDR"} "$url$1"
}

# `metadata=` for a list read: a one-key containment document, url-encoded.
meta_query() {
    jq -rn --arg k "$1" --arg v "$2" '{($k): $v} | tojson | @uri'
}

# Does a list reply hold its whole answer? A `.data` array, and every row
# its `.total` counts.
page_is_whole() {
    local count total
    count=$(printf '%s' "$1" | jq '.data | if type == "array" then length else error("no .data") end' 2>/dev/null) || return 1
    case ${count:-empty} in empty | *[!0-9]*) return 1 ;; esac
    total=$(printf '%s' "$1" | jq '.total // empty' 2>/dev/null) || total=
    case ${total:-empty} in empty | *[!0-9]*) total=$count ;; esac
    [ "$total" -le "$count" ]
}

# Latch the record as unreadable for the rest of the pass, and say so.
sor_unread() {
    case "$2" in
        0) WT_SOR_UNREAD_REASON="$1 answered a shape this pass cannot read (no .data array, fewer rows than its .total, or no status)" ;;
        3) WT_SOR_UNREAD_REASON="no system of record named (BOSS_JOBS_URL unset, infra/dev/sor-url absent)" ;;
        *) WT_SOR_UNREAD_REASON="$1 failed (curl exit $2)" ;;
    esac
    log "worktree pass: the system of record could not be read — $WT_SOR_UNREAD_REASON; every unpushed worktree keeps its ${WORKTREE_MAX_AGE_H}h window this pass" >&2
}

# Read 1 and 3: has the car on branch $1 landed? 0 yes (REC_HOW says
# which car), 1 no, 2 unread.
record_car_landed() {
    local b="$1" reply rc=0 car
    reply=$(sor_read "/api/jobs?kind=ship-a-change&limit=50&metadata=$(meta_query branch "$b")") || rc=$?
    if [ "$rc" -ne 0 ] || ! page_is_whole "$reply"; then
        sor_unread "the car read for branch $b" "$rc"
        return 2
    fi
    car=$(printf '%s' "$reply" | jq -r '[.data[]
        | select((.status == "closed" and .metadata.outcome == "merged")
                 or .metadata.merged == "true" or .metadata.merged == true)][0].id // empty' 2>/dev/null) || car=
    [ -n "$car" ] || return 1
    REC_HOW="car ${car:0:8} for branch $b is merged"
}

# What a failed read said, on one line: the first non-empty of the files
# named (a page's curl stderr, a jq error), cut to 300 characters, or
# nothing. A latch that names only a shape hid a 400 for six days
# (backlog 7c9715b6), so the reason carries the answer itself.
read_said() {
    local f
    for f in "$@"; do
        [ -s "$f" ] || continue
        tr '\n' ' ' <"$f" | cut -c1-300
        return 0
    done
}

# EVERY gate-run carrying an immutable submission, read WHOLE once a
# pass (backlog 7c9715b6). The jobs API's `metadata=` takes a flat
# object of string values and refuses anything else 400
# (boss_jobs::metadata_containment), so the nested
# `{gate_submission:{source:{worktree:...}}}` this pass asked from
# 2026-10-16 (#1206) was refused on every pass, and every pass latched
# the record unreadable: no landed tree released on record, no harness
# lock released, /work held under its floor. The source cannot be asked
# for by value, so the submitted gate-runs are asked for by KEY
# (`metadata_has=gate_submission`, measured 2026-10-22: 117 of 4344
# gate-runs, three pages) and each tree's are picked out here. Once a
# pass, not once a tree: a pass judges hundreds. 0 with SUBMITTED_GATES
# naming the rows; 1 with SUBMITTED_DIR holding the failed read's pages.
SUBMITTED_GATES=""; SUBMITTED_DIR=""
read_submitted_gates() {
    [ -z "$SUBMITTED_GATES" ] || return 0
    SUBMITTED_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dev-reclaim-submitted.XXXXXX") || return 1
    read_job_pages "kind=gate-run&metadata_has=gate_submission" "$SUBMITTED_DIR/submitted" \
        >"$SUBMITTED_DIR/submitted.json" || return 1
    # Read whole: the pages are the rows again. Only the rows are kept.
    rm -f -- "$SUBMITTED_DIR"/submitted.page-* "$SUBMITTED_DIR/submitted.rows"
    SUBMITTED_GATES="$SUBMITTED_DIR/submitted.json"
}

# The gate-runs launched from the worktree at $1, read WHOLE from both
# indexes (the flat `worktree` stamp, and the immutable submission's
# source out of `read_submitted_gates`) and each checked against the
# source its submission binds — the one reading `landed_on_record` and
# `harness_lock_released` share. A submission naming this tree that the
# flat read did not return has had its stamp moved, and fails closed: it
# may be the open run that keeps the tree. The flat read's rows are the
# ones judged, so a gate that moved since the pass-wide read is judged
# as it is now. 0 with GATE_EVIDENCE naming the directory whose
# sources.json holds one source per gate-run; 2 when the record could
# not be read (latched, naming what the read said). Called directly,
# never in $(...), so the latch and GATE_EVIDENCE outlive it.
GATE_EVIDENCE=""
read_gate_sources() {
    local path="$1" readers raw_query said
    GATE_EVIDENCE=""
    readers="$(dirname "$(readlink -f "$0")")/../lib"
    if [ ! -r "$readers/jobs-pages.sh" ] || [ ! -r "$readers/immutable-metadata.jq" ]; then
        sor_unread "the authoritative gate readers" 0
        return 2
    fi
    GATE_EVIDENCE=$(mktemp -d "${TMPDIR:-/tmp}/dev-reclaim-gates.XXXXXX") || { sor_unread "gate evidence storage" 0; return 2; }
    . "$readers/jobs-pages.sh"
    job_pages_get() { sor_read "/api/jobs?$1"; }
    if ! read_submitted_gates; then
        said=$(read_said "$SUBMITTED_DIR"/*.stderr)
        sor_unread "the pass's read of every submitted gate-run (evidence $SUBMITTED_DIR${said:+; it said: $said})" 0
        return 2
    fi
    raw_query=$(meta_query worktree "$path")
    if ! read_job_pages "kind=gate-run&metadata=$raw_query" "$GATE_EVIDENCE/legacy" >"$GATE_EVIDENCE/legacy.json" \
        || ! jq --arg path "$path" '[.[] | select(
                (try getpath(["metadata", "gate_submission", "source", "worktree"]) catch null) == $path
                or (try getpath(["metadata", "_immutable_metadata", "gate_submission", "source", "worktree"]) catch null) == $path)]
            ' "$SUBMITTED_GATES" >"$GATE_EVIDENCE/canonical.json" 2>"$GATE_EVIDENCE/canonical.stderr" \
        || ! jq -s -L "$readers" --arg path "$path" '
            include "immutable-metadata";
            .[0] as $flat
            | ([.[1][].id] - [$flat[].id]) as $moved
            | if $moved != [] then error("gate index disagrees with checkout: \($moved | join(" ")) name it only in their submission") else $flat end
            | map((.metadata | gate_source) as $source
                | if $source.worktree != $path then error("gate index disagrees with checkout") else $source end)
            ' "$GATE_EVIDENCE/legacy.json" "$GATE_EVIDENCE/canonical.json" >"$GATE_EVIDENCE/sources.json" 2>"$GATE_EVIDENCE/authority.stderr"; then
        said=$(read_said "$GATE_EVIDENCE"/*.stderr)
        sor_unread "the complete authoritative gate-run read for $path (evidence $GATE_EVIDENCE${said:+; it said: $said})" 0
        return 2
    fi
}

# Is the worktree at $1, on branch $2, landed on record? 0 yes (REC_HOW
# says how), 1 the record holds no landing, 2 the record could not be
# read. Called directly, never in $(...), so the latch outlives it.
REC_HOW=""
landed_on_record() {
    local path="$1" branch="$2" reply rc run status outcome b
    local -a runs=() branches=()
    REC_HOW=""
    [ -z "$WT_SOR_UNREAD_REASON" ] || return 2
    rc=0
    record_car_landed "$branch" || rc=$?
    [ "$rc" = 1 ] || return "$rc"

    local evidence
    read_gate_sources "$path" || return 2
    evidence=$GATE_EVIDENCE
    log "worktree gate authority evidence: $evidence" >&2
    mapfile -t runs < <(jq -r '[.[].agent_run // empty] | unique[]' "$evidence/sources.json")
    mapfile -t branches < <(jq -r --arg b "$branch" '[.[].branch // empty | select(. != $b)] | unique[]' "$evidence/sources.json")
    for run in "${runs[@]}"; do
        # An id goes into a path: anything but a uuid's alphabet is not one.
        case ${run:-empty} in empty | *[!0-9A-Za-z-]*) continue ;; esac
        rc=0
        reply=$(sor_read "/api/jobs/$run") || rc=$?
        status=$(printf '%s' "$reply" | jq -r '.status // empty' 2>/dev/null) || status=
        if [ "$rc" -ne 0 ] || [ -z "$status" ]; then
            sor_unread "the read of agent-run $run" "$rc"
            return 2
        fi
        if [ "$status" = closed ]; then
            outcome=$(printf '%s' "$reply" | jq -r '.metadata.outcome // "no outcome"' 2>/dev/null) || outcome="no outcome"
            REC_HOW="agent-run ${run:0:8}, which gated from this worktree, is finished ($outcome)"
            return 0
        fi
    done
    for b in "${branches[@]}"; do
        [ -n "$b" ] || continue
        rc=0
        record_car_landed "$b" || rc=$?
        [ "$rc" = 1 ] || return "$rc"
    done
    return 1
}

# A HARNESS LOCK OUTLIVES ITS AGENT (backlog 3a84cbf5). The Claude
# harness writes `claude agent agent-<id> (pid N start T)` with the pid
# of the OPERATOR'S session, not the agent's: measured 2026-09-28, all
# 26 locks named one pid, and 2026-10-21 16:15Z, all 116 named one of
# two live sessions. So `lock_stale_why` reads every finished agent's
# lock as live for the session's whole life, and the tree is never a
# candidate — that day /work sat at its 18 GB floor, the pass reported
# `removed 0, kept 536`, and 97 of the 116 harness-locked trees (9.1 GB)
# had gated from runs that were every one closed. The record is the
# owner the lock cannot name: the agent-runs whose gate-runs launched
# from the tree (`read_gate_sources`, the reading `landed_on_record`
# makes). The lock is RELEASED — judged like no lock, every guard after
# it still running — only when ALL of:
#   1. the reason is exactly the harness's shape (any other text is a
#      person's or another tool's, and is never judged here);
#   2. git has been quiet in the tree for WORKTREE_GRACE_H — read first,
#      so a tree in use costs the record nothing;
#   3. at least one gate-run launched from the tree names an agent-run,
#      and EVERY such run is closed, each with a `closed_at` at least
#      WORKTREE_GRACE_H old (a run re-dispatched into the same tree is
#      an open one, and keeps it).
# The grace is NOT shortened under the /work floor, as a landed tree's
# is: the session the lock names is alive and could resume the agent. A
# record that cannot be read, a run with no close time, or an id that is
# not a uuid's alphabet releases nothing. 0 released (LOCK_RELEASED_WHY
# says why), 1 kept, 2 the record could not be read (latched). Called
# directly, never in $(...), so the latch outlives it. The evidence of a
# read that answered is removed — this asks of every quiet harness-
# locked tree every hour — and that of a read that failed is kept.
HARNESS_LOCK_SHAPE='^claude agent agent-[0-9A-Za-z]+ \(pid [0-9]+ start [0-9]+\)$'
LOCK_RELEASED_WHY=""
harness_lock_released() {
    local path="$1" reason="$2" now grace_s last run reply rc status closed_at closed_s n=0
    local -a runs=()
    LOCK_RELEASED_WHY=""
    [[ "$reason" =~ $HARNESS_LOCK_SHAPE ]] || return 1
    now=$(date +%s); grace_s=$((WORKTREE_GRACE_H * 3600))
    last=$(worktree_last_activity "$path")
    [ $((now - last)) -ge "$grace_s" ] || return 1
    [ -z "$WT_SOR_UNREAD_REASON" ] || return 2
    read_gate_sources "$path" || return 2
    mapfile -t runs < <(jq -r '[.[].agent_run // empty] | unique[]' "$GATE_EVIDENCE/sources.json")
    rm -rf -- "$GATE_EVIDENCE"
    [ "${#runs[@]}" -gt 0 ] || return 1
    for run in "${runs[@]}"; do
        case ${run:-empty} in empty | *[!0-9A-Za-z-]*) return 1 ;; esac
        rc=0
        reply=$(sor_read "/api/jobs/$run") || rc=$?
        status=$(printf '%s' "$reply" | jq -r '.status // empty' 2>/dev/null) || status=
        if [ "$rc" -ne 0 ] || [ -z "$status" ]; then
            sor_unread "the read of agent-run $run" "$rc"
            return 2
        fi
        [ "$status" = closed ] || return 1
        closed_at=$(printf '%s' "$reply" | jq -r '.metadata.closed_at // empty' 2>/dev/null) || closed_at=
        # `date -d ''` answers midnight, not an error: the empty case first.
        [ -n "$closed_at" ] || return 1
        closed_s=$(date -u -d "$closed_at" +%s 2>/dev/null) || return 1
        case ${closed_s:-empty} in empty | *[!0-9]*) return 1 ;; esac
        [ $((now - closed_s)) -ge "$grace_s" ] || return 1
        n=$((n + 1))
    done
    LOCK_RELEASED_WHY="its harness lock outlives its agent: every agent-run that gated from this worktree ($n) closed at least ${WORKTREE_GRACE_H}h ago"
}

worktree_target() { echo "$SCRATCH_MOUNT/target-$(basename "$1")"; }

remove_worktree_target() {
    local target t kb
    target=$(worktree_target "$1")
    for t in "$target" "$target.seeding"; do
        [ -d "$t" ] || continue
        [ "$t" = "$TARGET_DIR" ] && continue
        kb=$(du -sk "$t" 2>/dev/null | awk '{print $1}')
        if remove_tree "$t"; then
            log "  removed target $t ($((${kb:-0} / 1024))MiB, its worktree is gone)"
            WT_TARGETS_REMOVED=$((WT_TARGETS_REMOVED + 1))
            WT_TARGETS_MIB=$((WT_TARGETS_MIB + ${kb:-0} / 1024))
        else
            log "could not remove target $t — $REMOVE_WHY" >&2
            problem target-not-removed "$t (its worktree is gone): $REMOVE_WHY"
        fi
    done
}

# THE PRUNE IS JUDGED HERE, NOT HANDED TO GIT (backlog 52fefc45,
# 2026-09-28). A bare `git worktree prune` drops every admin entry
# whose worktree it cannot find — and this sidecar mounts only /work
# and /scratch (boss-dev.yaml), so a worktree under the dev container's
# /tmp, where every session scratchpad and so every reviewer's detached
# tree lives, is one it can NEVER find. Measured by run ff5ff31c: the
# sidecar logged "pruned: Removing worktrees/rv", "wt", "rr", "wt" at
# 16:58, 17:59 and 18:59Z, inside the window in which three live
# reviewers each lost their tree's metadata mid-run and recreated it.
# Absent from here is not gone. So an entry is dropped only when its
# worktree lies under a mount this process can see — WORKTREES_DIR,
# WORK_MOUNT, SCRATCH_MOUNT, each only when it is there to look in —
# AND that directory is absent. An entry pointing anywhere else is
# kept, logged and counted (worktrees_kept_unseen on the packet); so is
# one whose path cannot be judged (no gitdir, or a path with . or ..
# segments, which a prefix cannot place). A locked entry is untouched,
# as git's own prune leaves it. Removing the admin directory is what
# git's prune itself does (prune_worktree in builtin/worktree.c).
# Widening the sidecar's mounts instead would roll the dev pod and end
# the operator's session, and would still leave the next unseen path
# exposed.
prune_gone_worktrees() {
    local common entry name gitdir wt m visible pruned=0 unseen=0 unseen_names=""
    local -a roots=()
    if ! common=$(git -C "$REPO_DIR" rev-parse --git-common-dir 2>&1) || [ -z "$common" ]; then
        log "could not read $REPO_DIR's git directory (${common:-empty answer}) — no worktree entry pruned" >&2
        problem git-dir-unread "$REPO_DIR's git directory could not be read (${common:-empty answer}); no worktree entry pruned"
        return 0
    fi
    case "$common" in /*) ;; *) common="$REPO_DIR/$common" ;; esac
    [ -d "$common/worktrees" ] || return 0
    # A mount that is not there is not a place to look: were /work
    # unmounted, every entry under it would read as gone.
    for m in "$WORKTREES_DIR" "$WORK_MOUNT" "$SCRATCH_MOUNT"; do
        [ -n "$m" ] && [ -d "$m" ] || continue
        [ -n "${m%/}" ] && roots+=("${m%/}")
        if m=$(readlink -f "$m") && [ -n "${m%/}" ]; then
            roots+=("${m%/}")
        fi
    done
    for entry in "$common"/worktrees/*; do
        [ -d "$entry" ] || continue
        name=$(basename "$entry")
        [ -e "$entry/locked" ] && continue
        wt=""
        if gitdir=$(head -n1 "$entry/gitdir" 2>/dev/null) && [ -n "$gitdir" ]; then
            case "$gitdir" in /*) ;; *) gitdir="$entry/$gitdir" ;; esac
            wt=$(dirname "$gitdir")
        fi
        visible=0
        for m in ${roots[@]+"${roots[@]}"}; do
            case "$wt" in "$m"/*) visible=1; break ;; esac
        done
        case "$wt" in */./*|*/../*|*/.|*/..) visible=0 ;; esac
        if [ "$visible" = 0 ]; then
            log "  kept worktrees/$name: its worktree ${wt:-(no gitdir recorded)} lies outside every mount this pass can see (${roots[*]:-none}) — absent from here is not gone"
            unseen=$((unseen + 1))
            unseen_names="${unseen_names:+$unseen_names, }$name"
            continue
        fi
        [ -e "$wt" ] && continue
        if rm -rf "$entry"; then
            log "  pruned: Removing worktrees/$name: its worktree $wt is gone"
            pruned=$((pruned + 1))
        else
            log "could not remove the admin entry worktrees/$name" >&2
            problem worktree-entry-not-pruned "the admin entry worktrees/$name, whose worktree $wt is gone, could not be removed"
        fi
    done
    WT_PRUNED=$((WT_PRUNED + pruned))
    # A reading of what is there now, not a tally: one pass may prune
    # three times (the floor pass twice), and an entry kept three times
    # is still one entry.
    WT_KEPT_UNSEEN=$unseen; WT_KEPT_UNSEEN_NAMES=$unseen_names
}

# ---------------------------------------------------------------------
# EACH WORKTREE'S TOP AND ITS .git FILE ARE HELD BY THEIR OWNER, AND THIS
# PASS ADMITS ONLY HELD AND PAIRED TREES (backlog 0cc54611, review finding F1
# of run d6747820 on car 335ace12, 2026-10-22).
# ---------------------------------------------------------------------
# A linked worktree's `.git` is a FILE naming the admin entry git reads
# for that tree (HEAD, index, config.worktree), and `git -C <tree>`
# follows it wherever it points. hold_checkout leaves every worktree's
# contents as they are, because builders write there, and that left the
# tree's top and its .git file with them: measured 2026-10-22 ~15:57Z by
# run 73c1e68a (stat only), 18 of 221 .git files under .claude/worktrees
# root:1500 at 0664, in the same 18 tree directories at 2775. A group
# member could rewrite such a file, or rename one of its own over it, to
# name a gitdir it had built, and this pass's `git status` on an idle
# tree, and any root session's git in that tree, would then read that
# gitdir's config as uid 0. So every hour, just before the worktree pass:
#   * each registered directory named by the held common admin entries,
#     including trees outside WORKTREES_DIR, and its .git file,
#     lose group and other write when this uid owns them (2775 -> 2755,
#     664 -> 644). Nothing inside a tree is touched. One this uid does
#     not own cannot be held without CAP_CHOWN: a problem by path and
#     owner, and the tree is skipped.
#   * the declared work or scratch mount is verified as a mount, then
#     every ancestor below it is checked before the next entry is read.
#     Owner-only sticky bits protect shared writable parents without
#     removing creation rights or setgid. Linked/unknown mount boundaries
#     and foreign owners admit nothing. The whole classified registry,
#     including absent/invalid entries, rides the pass's file-backed report.
#   * the .git file must name the admin entry under the checkout's
#     worktrees/ whose own gitdir names this tree back, the pair `git
#     worktree add` writes. One that does not (a path that is not there,
#     another tree's entry, anything but a regular file) is a problem
#     naming the PATH, never what the file says, and the tree is skipped.
#   * this pass's own judgement git runs as `--git-dir=<admin entry>
#     --work-tree=<tree>`, the entry found from the admin side, which
#     hold_checkout holds. Status ignores submodules: otherwise a staged
#     gitlink follows its nested .git file and can run its fsmonitor
#     (review 479a2589). Removal rereads the tree's .git file, so both
#     passes require positive admission by the hold before any per-tree
#     git, including removal (b11c5e91). Every other tree is kept.
# The 18 open trees were legacy entries; later trees were measured born
# at 2755/644 (run 1fbfa159), not open until the next pass.
# This holds registered entries against other-uid writers. It neither
# confines this same uid/root nor authorizes privileged Git in an
# unregistered, foreign-owned or otherwise unheld tree.

# Every admin entry, keyed by the tree its gitdir names, read once from
# the checkout's held .git (a bounded line per entry, nothing run).
read_worktree_admin() {
    local common entry gitdir
    WT_ADMIN_READ=1
    WT_ADMIN=(); WT_REGISTRY=(); WT_DUPLICATE=(); WT_ADMIN_WHY=""; WT_COMMON=""; WT_REGISTRY_STATE=unreadable
    # A fresh/non-Git checkout has no registry, rather than an unreadable one.
    if [ ! -e "$REPO_DIR/.git" ] && [ ! -L "$REPO_DIR/.git" ]; then WT_REGISTRY_STATE=absent; return 0; fi
    case "$CHECKOUT_STATE" in held*) ;; *) WT_ADMIN_WHY="the common checkout is not held"; return 0 ;; esac
    if ! hold_worktree_ancestors "$REPO_DIR"; then
        WT_ADMIN_WHY="the common checkout's ancestor boundary is not held: $HOLD_WHY"
        return 0
    fi
    common="$REPO_DIR/.git"
    if [ -L "$common" ] || [ ! -d "$common" ]; then
        WT_ADMIN_WHY="the held common checkout has no regular Git directory"
        return 0
    fi
    if [ -L "$common/worktrees" ] || { [ -e "$common/worktrees" ] && [ ! -d "$common/worktrees" ]; }; then
        WT_ADMIN_WHY="the common worktree registry is linked or not a directory"
        return 0
    fi
    WT_COMMON=$common
    WT_REGISTRY_STATE=observed
    for entry in "$common"/worktrees/*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        WT_REGISTRY["$entry"]=""
        [ -d "$entry" ] && [ ! -L "$entry" ] && [ -f "$entry/gitdir" ] && [ ! -L "$entry/gitdir" ] || continue
        gitdir=""
        IFS= read -r -n 4097 gitdir 2>/dev/null < "$entry/gitdir" || true
        [ -n "$gitdir" ] && [ "${#gitdir}" -le 4096 ] || continue
        case "$gitdir" in /*/.git) ;; *) continue ;; esac
        gitdir=${gitdir%/.git}
        WT_REGISTRY["$entry"]=$gitdir
        [ -z "${WT_ADMIN[$gitdir]-}" ] || WT_DUPLICATE["$gitdir"]=1
        WT_ADMIN["$gitdir"]=$entry
    done
}

# Protect each directory entry before inspecting the next component. An
# owner-held top alone can still be renamed through a writable parent.
# Sticky preserves shared creation/setgid; it protects owner-held children
# against other uids, not against root or a writer with this same uid.
hold_worktree_ancestors() {
    local path="$1" parent owner mode after me current="" component mount="" candidate
    HOLD_WHY=""
    case "$path" in /*) ;; *) HOLD_WHY="the registered path is not absolute"; return 1 ;; esac
    case "$path" in *'//'*|*/./*|*/../*|*/.|*/..) HOLD_WHY="the registered path is not canonical"; return 1 ;; esac
    for candidate in "$WORK_MOUNT" "$SCRATCH_MOUNT"; do
        case "$path/" in "$candidate/"*) [ "${#candidate}" -le "${#mount}" ] || mount=$candidate ;; esac
    done
    if [ -z "$mount" ] || [ -L "$mount" ] || ! mountpoint -q -- "$mount"; then
        HOLD_WHY="outside a verified declared work or scratch mount"; return 1
    fi
    # A mounted root cannot be renamed through its parent (EBUSY).
    # Everything below it still needs an owner-held rename boundary.
    parent=${path%/*}; me=$(id -u)
    local -a components=("$mount") relative=()
    if [ "$parent" != "$mount" ]; then
        IFS=/ read -r -a relative <<< "${parent#"$mount"/}"
        components+=("${relative[@]}")
    fi
    for component in "${components[@]}"; do
        [ -n "$component" ] || continue
        case "$component" in .|..) HOLD_WHY="the registered path contains a noncanonical component"; return 1 ;; esac
        if [ -z "$current" ]; then current=$component; else current="$current/$component"; fi
        if [ -L "$current" ] || [ ! -d "$current" ]; then
            HOLD_WHY="$current is a linked, missing or unreadable ancestor"; return 1
        fi
        if [ "$current" != "$mount" ] && mountpoint -q -- "$current"; then
            HOLD_WHY="$current is an undeclared nested mount"; return 1
        fi
        if ! read -r owner mode < <(stat -c '%u %a' -- "$current" 2>/dev/null); then
            HOLD_WHY="$current ownership and mode could not be read"; return 1
        fi
        if [ "$owner" != "$me" ] && [ "$owner" != 0 ]; then
            HOLD_WHY="$current is owned by foreign uid $owner"; return 1
        fi
        if [ $((8#$mode & 8#022)) -ne 0 ] && [ $((8#$mode & 8#1000)) -eq 0 ]; then
            if [ "$owner" != "$me" ] || ! chmod +t -- "$current" 2>/dev/null; then
                HOLD_WHY="$current has a writable rename boundary this uid cannot hold (mode $mode)"; return 1
            fi
            after=$(stat -c %a -- "$current" 2>/dev/null) || after=0
            if [ $((8#$after & 8#1000)) -eq 0 ] || [ $((8#$after & 8#7777)) -ne $(((8#$mode | 8#1000) & 8#7777)) ]; then
                HOLD_WHY="$current did not retain its permissions and acquire sticky after chmod"; return 1
            fi
            WT_ANCESTORS_HELD=$((WT_ANCESTORS_HELD + 1))
            log "worktrees: set the owner-only sticky rename boundary on $current (was $mode)"
        fi
    done
}

record_worktree_hold() {
    local row
    row=$(jq -nc --arg path "$1" --arg admin "$2" --arg state "$3" --arg why "$4" \
        '{path:$path,admin:$admin,state:$state,why:$why,git_admitted:($state == "held")}') || return 1
    WT_HOLD_ROWS+="$row"$'\n'
    WT_HOLD_TOTAL=$((WT_HOLD_TOTAL + 1))
    [ "$3" != held ] || WT_HOLD_HELD=$((WT_HOLD_HELD + 1))
}

# The admin entry for the tree at $1, or fail: by the path as listed,
# then by its real path. Never read from the tree's own .git file.
worktree_admin() {
    local path="$1" real
    [ "$WT_ADMIN_READ" = 1 ] || read_worktree_admin
    if [ -n "${WT_ADMIN[$path]-}" ]; then
        printf '%s\n' "${WT_ADMIN[$path]}"
        return 0
    fi
    real=$(readlink -f -- "$path" 2>/dev/null) || return 1
    [ -n "${WT_ADMIN[$real]-}" ] || return 1
    printf '%s\n' "${WT_ADMIN[$real]}"
}

# Take group and other write off $1 when this uid owns it. Fails, with
# the reason in HOLD_WHY, when it cannot be held.
hold_worktree_entry() {
    local f="$1" owner mode after
    HOLD_WHY=""
    if ! read -r owner mode < <(stat -c '%u %a' -- "$f" 2>/dev/null) || [ -z "$mode" ]; then
        HOLD_WHY="$f could not be read"
        return 1
    fi
    if [ "$owner" != "$(id -u)" ]; then
        HOLD_WHY="$f is owned by uid $owner"
        return 1
    fi
    [ $((8#$mode & 8#022)) -ne 0 ] || return 0
    if ! chmod go-w -- "$f" 2>/dev/null; then
        HOLD_WHY="chmod go-w $f failed (mode $mode)"
        return 1
    fi
    after=$(stat -c %a -- "$f" 2>/dev/null) || after=7777
    if [ $((8#$after & 8#022)) -ne 0 ]; then
        HOLD_WHY="$f remains group- or other-writable after chmod"
        return 1
    fi
    WT_HELD_NOW=$((WT_HELD_NOW + 1))
    log "worktrees: took group and other write off $f (was $mode)"
}

WT_HELD_NOW=0
hold_worktrees() {
    local dir real gitfile line target entry trees=0 skipped=0
    local enumerated=0 registry_count=0
    # Admission is positive and belongs to this pass. Registered trees
    # outside this hold were never in the negative list, yet git worktree remove
    # follows their writable .git file (b11c5e91).
    WT_HELD_PAIRED=()
    WT_HOLD_COMPLETE=false
    WT_HOLD_ROWS=""; WT_HOLD_TOTAL=0; WT_HOLD_HELD=0; WT_HOLD_INVENTORY='[]'
    read_worktree_admin
    registry_count=${#WT_REGISTRY[@]}
    WT_HOLD_STATE=$WT_REGISTRY_STATE
    if [ -n "$WT_ADMIN_WHY" ]; then
        problem worktree-registry-unreadable "$WT_ADMIN_WHY; no worktree is admitted"
        record_worktree_hold "$REPO_DIR" "" registry-unreadable "$WT_ADMIN_WHY"
    fi
    while IFS= read -r -d '' entry; do
        # Admin identities are absolute paths; this non-path sentinel
        # can only acknowledge the successful end of the NUL stream.
        if [ "$entry" = worktree-registry-enumeration-completed ]; then
            enumerated=1
            continue
        fi
        dir=${WT_REGISTRY[$entry]}
        trees=$((trees + 1))
        if [ -z "$dir" ]; then
            record_worktree_hold "" "$entry" invalid-registration 'the admin gitdir is not a regular bounded absolute worktree path'
            problem worktree-registry-unreadable "$entry has no readable regular bounded gitdir; no worktree is admitted from it"
            continue
        fi
        # A registration describes another namespace as well as this one.
        # Absence is not proof of deletion, and a path outside our mounts
        # cannot be held here. Both remain counted, never admitted to Git.
        if [ ! -e "$dir" ] && [ ! -L "$dir" ]; then
            record_worktree_hold "$dir" "$entry" absent 'the registered directory is absent'
            continue
        fi
        case "$dir/" in
            "$WORK_MOUNT/"*|"$SCRATCH_MOUNT/"*) ;;
            *) record_worktree_hold "$dir" "$entry" unseen 'outside the declared work and scratch mounts; no git is admitted'; continue ;;
        esac
        if ! hold_worktree_ancestors "$dir"; then
            record_worktree_hold "$dir" "$entry" unsafe-ancestor "$HOLD_WHY"
            problem worktree-open "$dir is not admitted: $HOLD_WHY; this pass runs no git in it"
            continue
        fi
        if [ -L "$dir" ] || [ ! -d "$dir" ] || [ -n "${WT_DUPLICATE[$dir]-}" ]; then
            record_worktree_hold "$dir" "$entry" unpaired 'the root is linked, not a directory or has duplicate registrations'
            problem worktree-git-unpaired "$dir is linked, not a directory or has duplicate registrations; no git is run in it"
            continue
        fi
        real=$(readlink -f -- "$dir" 2>/dev/null) || real=$dir
        gitfile="$dir/.git"
        if ! hold_worktree_entry "$dir"; then
            record_worktree_hold "$dir" "$entry" unheld "$HOLD_WHY"
            problem worktree-open "the worktree's top is not held: $HOLD_WHY; a group member may replace its .git file, so this pass runs no git in it"
            skipped=$((skipped + 1))
            continue
        fi
        # No .git at all is no worktree: nothing for git to follow.
        if [ ! -e "$gitfile" ] && [ ! -L "$gitfile" ]; then
            record_worktree_hold "$dir" "$entry" unpaired 'the registered root has no .git file'
            problem worktree-git-unpaired "$gitfile is absent; no git is run in $dir"
            continue
        fi
        if [ -L "$gitfile" ] || [ ! -f "$gitfile" ]; then
            record_worktree_hold "$dir" "$entry" unpaired 'the .git entry is not a regular file'
            problem worktree-git-unpaired "$gitfile is not a regular file; this pass runs no git in $dir"
            skipped=$((skipped + 1))
            continue
        fi
        if ! hold_worktree_entry "$gitfile"; then
            record_worktree_hold "$dir" "$entry" unheld "$HOLD_WHY"
            problem worktree-open "the worktree's .git file is not held: $HOLD_WHY; this pass runs no git in $dir"
            skipped=$((skipped + 1))
            continue
        fi
        line=""
        IFS= read -r -n 4096 line 2>/dev/null < "$gitfile" || true
        target=${line#gitdir: }
        case "$target" in /*) ;; *) target="$dir/$target" ;; esac
        entry=${WT_ADMIN[$real]-}
        [ -n "$entry" ] || entry=${WT_ADMIN[$dir]-}
        if [ "${line:0:8}" != "gitdir: " ] || [ -z "$entry" ] || [ "$target" != "$entry" ]; then
            record_worktree_hold "$dir" "$entry" unpaired 'the regular .git file does not name its own trusted admin entry'
            problem worktree-git-unpaired "$gitfile does not name an admin entry under ${WT_COMMON:-$REPO_DIR/.git}/worktrees that names $dir back; this pass runs no git in it"
            skipped=$((skipped + 1))
            continue
        fi
        WT_HELD_PAIRED["$dir"]=$entry
        WT_HELD_PAIRED["$real"]=$entry
        record_worktree_hold "$dir" "$entry" held 'owner-held top and .git, protected ancestor entries and exact trusted admin pair'
    done < <(
        set -o pipefail
        if [ "$registry_count" -eq 0 ] || printf '%s\0' "${!WT_REGISTRY[@]}" | LC_ALL=C sort -z; then
            printf '%s\0' worktree-registry-enumeration-completed
        fi
    )
    # Object order may change through the API's JSON storage. Hash
    # compact sorted keys, without a trailing newline, so readback is
    # verifiable after that round trip.
    WT_HOLD_INVENTORY=$(printf '%s' "$WT_HOLD_ROWS" | jq -scS .)
    WT_HOLD_SHA256=$(printf '%s' "$WT_HOLD_INVENTORY" | sha256sum); WT_HOLD_SHA256=${WT_HOLD_SHA256%% *}
    # This component completed its registry traversal even if another
    # subsystem reports trouble, or named trees were safely refused.
    if [ "$WT_HOLD_STATE" = observed ]; then
        if [ "$enumerated" = 1 ] && [ "$WT_HOLD_TOTAL" -eq "$registry_count" ]; then
            WT_HOLD_COMPLETE=true
        else
            problem worktree-inventory-incomplete "registry enumeration did not complete successfully: $WT_HOLD_TOTAL rows for $registry_count admin entries; no full coverage recorded"
        fi
    fi
    if [ -n "$WT_COMMON" ]; then WT_HOLD_PENDING_FILE="$WT_COMMON/boss-worktree-hold-pending.json"; fi
    log "worktrees: $WT_HOLD_TOTAL registrations inventoried, $WT_HOLD_HELD held and paired; inventory sha256 $WT_HOLD_SHA256"
}

# Consult the hold BEFORE even looking up the current admin entry. A
# missing/failed hold admits nothing; an entry changed since it ran is
# not the pair it held. No git in an unadmitted tree, in either pass.
held_worktree_admin() {
    local path="$1" held admin
    held=${WT_HELD_PAIRED[$path]-}
    [ -n "$held" ] || return 1
    admin=$(worktree_admin "$path") || return 1
    [ "$admin" = "$held" ] || return 1
    printf '%s\n' "$admin"
}

reclaim_gone_worktrees() {
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "$REPO_DIR is not a git checkout — worktree pass skipped"
        return 0
    fi
    # ONE read of origin/main, up front, from the checkout's own refs.
    # Without it "landed" has no meaning — under a missing ref every
    # branch would read as abandoned-or-not by age alone — so the pass
    # skips, says so, and RECORDS the skip (below) rather than return
    # in silence.
    local main_sha main_ts
    if ! main_sha=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>&1) || [ -z "$main_sha" ]; then
        WT_PASS_REASON="$REPO_DIR has no refs/remotes/origin/main (git fetch origin in the dev container; ${main_sha:-empty answer})"
        log "worktree pass skipped: $WT_PASS_REASON" >&2
        problem worktree-pass-skipped "$WT_PASS_REASON"
        return 0
    fi
    if ! main_ts=$(git -C "$REPO_DIR" log -1 --format=%ct "$main_sha" 2>&1) || [ -z "$main_ts" ]; then
        WT_PASS_REASON="git could not read refs/remotes/origin/main at $main_sha (${main_ts:-empty answer})"
        log "worktree pass skipped: $WT_PASS_REASON" >&2
        problem worktree-pass-skipped "$WT_PASS_REASON"
        return 0
    fi
    WT_MAIN_SHA="$main_sha"; WT_MAIN_TS="$main_ts"
    WT_PASS=ran
    log "worktree pass: judging against origin/main ${main_sha:0:8} (committed $(date -u -d "@$main_ts" +%FT%TZ 2>/dev/null || echo "@$main_ts"), read from $REPO_DIR's refs — only as fresh as the dev container's last fetch)"

    # Admin entries whose directory is already gone — a worktree an
    # operator rm -rf'd — hold the branch checked out and nothing else.
    prune_gone_worktrees
    read_worktree_admin

    local self now judge_locks=1
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"
    now=$(date +%s)

    # THE GRACE YIELDS TO THE FLOOR (backlog 99ce8744), as it already
    # does for targets: above the floor a stale target waits
    # STALE_TARGET_H, below it only LIVE_TARGET_MIN. A landed worktree's
    # WORKTREE_GRACE_H exists for a tree someone may still be standing in, and
    # the same live window answers that for a tree as for a target —
    # while /work under its floor is what refuses the next builder's
    # pre-flight. One reading, up front: the pass removes every landed
    # tree past the live window rather than stopping at the floor,
    # because each would go at the grace's mark anyway and holds no work
    # (clean, on origin/main).
    local under_floor=0 wkb
    wkb=$(free_kb "$WORK_MOUNT")
    if [ -n "$wkb" ] && [ $((wkb / 1024 / 1024)) -lt "$WORK_FLOOR_GB" ]; then
        under_floor=1
        log "worktree pass: $WORK_MOUNT $((wkb / 1024 / 1024))GB free < ${WORK_FLOOR_GB}GB floor — a landed worktree waits only the ${LIVE_TARGET_MIN}-minute live window, not the ${WORKTREE_GRACE_H}h grace"
    fi
    if ! proc_view_is_pods; then
        judge_locks=0
        log "worktree pass: pid locks not judged — $PROC_ROOT/1 is not the pod's pause, so this process table cannot say a harness pid is gone; every tree under one is kept"
    fi

    # `path<TAB>branch<TAB>locked<TAB>reason` per worktree, `detached`
    # standing in for a HEAD with no branch; the first block is the main
    # worktree.
    local first=1 path branch locked reason stale window_s why last idle_s idle_h dirty kb head held
    local landed_window_s floor_note unpushed how by_content by_record rc
    local tree_landed_s tree_note ownerless admin
    landed_window_s=$((WORKTREE_GRACE_H * 3600)); floor_note=""
    if [ "$under_floor" = 1 ]; then
        landed_window_s=$((LIVE_TARGET_MIN * 60)); floor_note=", under the /work floor"
    fi
    while IFS=$'\t' read -r path branch locked reason; do
        [ -z "$path" ] && continue
        if [ "$first" = 1 ]; then first=0; continue; fi
        [ "$path" = "$REPO_DIR" ] && continue
        case "$self" in "$path"|"$path"/*) continue ;; esac
        [ -d "$path" ] || continue
        # Git here reads the tree through its admin entry, and only a
        # held, paired tree is judged at all (backlog 0cc54611).
        if ! admin=$(held_worktree_admin "$path"); then
            log "  kept $path (not held and paired by this pass: no git run in it)"
            continue
        fi
        stale=""; ownerless=0
        tree_landed_s=$landed_window_s; tree_note=$floor_note
        if [ "$locked" = 1 ] && [ "$reason" = "$OWNERLESS_LOCK" ]; then
            # `stale` set is what unlocks it at the removal and locks it
            # back, with the same text, if git then refuses.
            stale="its lock names no owner"; ownerless=1
            if [ "$tree_landed_s" -lt $((WORKTREE_MAX_AGE_H * 3600)) ]; then
                tree_landed_s=$((WORKTREE_MAX_AGE_H * 3600)); tree_note=""
            fi
            WT_OWNERLESS_LOCKS=$((WT_OWNERLESS_LOCKS + 1))
            WT_OWNERLESS_LOCK_NAMES="${WT_OWNERLESS_LOCK_NAMES:+$WT_OWNERLESS_LOCK_NAMES, }$(basename "$path")"
        elif [ "$locked" = 1 ]; then
            if [ "$judge_locks" = 1 ] && stale=$(lock_stale_why "$reason"); then
                log "  $(basename "$path"): stale lock ($stale) — judged as an ordinary candidate"
                WT_STALE_LOCKS=$((WT_STALE_LOCKS + 1))
                WT_STALE_LOCK_NAMES="${WT_STALE_LOCK_NAMES:+$WT_STALE_LOCK_NAMES, }$(basename "$path")"
            elif harness_lock_released "$path" "$reason"; then
                # Backlog 3a84cbf5: the record, not the pid, says the
                # agent is done. It waits the full grace, floor or not.
                stale=$LOCK_RELEASED_WHY
                if [ "$tree_landed_s" -lt $((WORKTREE_GRACE_H * 3600)) ]; then
                    tree_landed_s=$((WORKTREE_GRACE_H * 3600)); tree_note=""
                fi
                log "  $(basename "$path"): $stale — judged as an ordinary candidate"
                WT_RELEASED_LOCKS=$((WT_RELEASED_LOCKS + 1))
                WT_RELEASED_LOCK_NAMES="${WT_RELEASED_LOCK_NAMES:+$WT_RELEASED_LOCK_NAMES, }$(basename "$path")"
            else
                WT_KEPT_LOCKED=$((WT_KEPT_LOCKED + 1))
                continue
            fi
        fi

        unpushed=0; by_content=0
        case "$branch" in
            detached|main)
                window_s=$((WORKTREE_IDLE_H * 3600)); why="on $branch"
                ;;
            *)
                if git -C "$REPO_DIR" rev-parse --verify -q "refs/remotes/origin/$branch" >/dev/null 2>&1; then
                    WT_KEPT_LIVE=$((WT_KEPT_LIVE + 1))
                    continue
                fi
                if git -C "$REPO_DIR" merge-base --is-ancestor "refs/heads/$branch" "$main_sha" 2>/dev/null; then
                    window_s=$tree_landed_s; why="branch $branch has no origin/ ref and its head is on origin/main (landed)$tree_note"
                else
                    unpushed=1
                    window_s=$((WORKTREE_MAX_AGE_H * 3600)); why="branch $branch has no origin/ ref and is not on origin/main (unpushed)"
                fi
                ;;
        esac

        last=$(worktree_last_activity "$path")
        idle_s=$((now - last))
        idle_h=$((idle_s / 3600))
        # A head that is not on origin/main may still have LANDED — its
        # work replayed and squashed onto main under other shas (backlog
        # 4a738ca5, `landed_by_content`). Asked only where the answer
        # can change the outcome: idle past the landed window but inside
        # the unpushed one, which is where the 286 kept trees stood.
        if [ "$unpushed" = 1 ] && [ "$idle_s" -ge "$tree_landed_s" ] && [ "$idle_s" -lt "$window_s" ] &&
            how=$(landed_by_content "refs/heads/$branch" "$main_sha"); then
            by_content=1
            window_s=$tree_landed_s
            why="branch $branch has no origin/ ref and its work is on origin/main by $how (landed — rebased or squashed under other shas)$tree_note"
        fi
        # ...and where git cannot see it at all, the system of record
        # may (backlog 9a044141, `landed_on_record`): asked in the same
        # window, only once git has had its say. No landing on record,
        # or no reading of it, keeps the tree — counted by which, so
        # the packet says why each unpushed tree stays.
        by_record=0
        if [ "$unpushed" = 1 ] && [ "$by_content" = 0 ] &&
            [ "$idle_s" -ge "$tree_landed_s" ] && [ "$idle_s" -lt "$window_s" ]; then
            rc=0
            landed_on_record "$path" "$branch" || rc=$?
            case "$rc" in
                0)
                    by_record=1
                    window_s=$tree_landed_s
                    why="branch $branch has no origin/ ref and is not on origin/main, and the system of record says $REC_HOW (landed on record)$tree_note"
                    ;;
                1)
                    log "  kept $path (unpushed, no landing on record: no merged car for $branch and no finished run that gated from it; idle ${idle_h}h of ${WORKTREE_MAX_AGE_H}h)"
                    WT_KEPT_NO_LANDING=$((WT_KEPT_NO_LANDING + 1))
                    continue
                    ;;
                *)
                    WT_KEPT_SOR_UNREAD=$((WT_KEPT_SOR_UNREAD + 1))
                    continue
                    ;;
            esac
        fi
        if [ "$ownerless" = 1 ]; then
            why="$why; its lock names no owner, so it waits at least ${WORKTREE_MAX_AGE_H}h"
        fi
        if [ "$idle_s" -lt "$window_s" ]; then
            WT_KEPT_RECENT=$((WT_KEPT_RECENT + 1))
            continue
        fi

        dirty=$(git --git-dir="$admin" --work-tree="$path" status --porcelain --ignore-submodules=all 2>/dev/null | grep -c . || true)
        if [ "${dirty:-0}" -gt 0 ]; then
            log "  kept $path ($dirty dirty: uncommitted or untracked files; $why, idle ${idle_h}h)"
            WT_KEPT_DIRTY=$((WT_KEPT_DIRTY + 1))
            WT_KEPT_DIRTY_NAMES="${WT_KEPT_DIRTY_NAMES:+$WT_KEPT_DIRTY_NAMES, }$(basename "$path"):$dirty"
            continue
        fi

        # A DETACHED head names its commit in two places only — the
        # worktree's HEAD and its reflog — and `git worktree remove`
        # deletes both, handing any commit no ref holds to the next gc.
        # That is unpushed work, as surely as a dirty tree is uncommitted
        # work, so it is kept and NAMED the same way (backlog adce5171).
        # A branch needs no such check: its ref outlives the checkout.
        # Measured 2026-09-23: of the 16 detached trees the corrected
        # idle clock makes due, 13 sit on a ref (a forge PR ref, a
        # branch) and 3 on none. A git that cannot answer reads as no
        # ref, and keeps (`ref_holding`).
        if [ "$branch" = detached ]; then
            head=$(git --git-dir="$admin" --work-tree="$path" rev-parse -q --verify HEAD 2>/dev/null || true)
            held=$(ref_holding "$head")
            if [ -z "$held" ]; then
                log "  kept $path (detached at ${head:0:8}, a head no ref holds: removing the checkout would leave its commits to gc; idle ${idle_h}h)"
                WT_KEPT_UNREFERENCED=$((WT_KEPT_UNREFERENCED + 1))
                WT_KEPT_UNREFERENCED_NAMES="${WT_KEPT_UNREFERENCED_NAMES:+$WT_KEPT_UNREFERENCED_NAMES, }$(basename "$path")"
                continue
            fi
        fi

        kb=$(du -sk "$path" 2>/dev/null | awk '{print $1}')
        # A stale lock comes off only here, at the removal it would
        # refuse — and goes back on, with its own reason, if git refuses
        # anyway, so a tree the pass keeps keeps its lock (e14a741c).
        if [ -n "$stale" ] && ! git -C "$REPO_DIR" worktree unlock "$path" 2>/dev/null; then
            log "  kept $path (git would not unlock its stale lock; $why, idle ${idle_h}h)"
            WT_KEPT_REFUSED=$((WT_KEPT_REFUSED + 1))
            continue
        fi
        if ! git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            if [ -n "$stale" ] && ! git -C "$REPO_DIR" worktree lock --reason "$reason" "$path" 2>/dev/null; then
                log "  could not put $path's lock back ($reason)" >&2
                problem worktree-lock-not-restored "$path was unlocked for a removal git then refused, and its lock ($reason) could not be put back"
            fi
            log "  kept $path (git refused to remove it without --force; $why, idle ${idle_h}h)"
            WT_KEPT_REFUSED=$((WT_KEPT_REFUSED + 1))
            continue
        fi
        WT_REMOVED=$((WT_REMOVED + 1))
        WT_REMOVED_MIB=$((WT_REMOVED_MIB + ${kb:-0} / 1024))
        if [ "$by_content" = 1 ]; then WT_REMOVED_BY_CONTENT=$((WT_REMOVED_BY_CONTENT + 1)); fi
        if [ "$by_record" = 1 ]; then WT_REMOVED_BY_RECORD=$((WT_REMOVED_BY_RECORD + 1)); fi
        log "  removed worktree $path ($((${kb:-0} / 1024))MiB; $why, idle ${idle_h}h, clean)"
        remove_worktree_target "$path"
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" b "\t" l "\t" r; p=substr($0, 10); b="detached"; l=0; r="" }
            /^branch /   { b=substr($0, 8); sub("^refs/heads/", "", b) }
            /^locked/    { l=1; r=substr($0, 8) }
            END { if (p != "") print p "\t" b "\t" l "\t" r }
        '
    )
    # The pass-wide submitted read answered: its rows go with the pass. A
    # read that failed keeps its pages, named in the latch (7c9715b6).
    if [ -n "$SUBMITTED_GATES" ]; then rm -rf -- "$SUBMITTED_DIR"; SUBMITTED_GATES=""; fi

    log "worktree pass: removed $WT_REMOVED worktree(s) (${WT_REMOVED_MIB}MiB; $WT_REMOVED_BY_CONTENT of them landed by content, not by sha, and $WT_REMOVED_BY_RECORD landed on record) and $WT_TARGETS_REMOVED target(s) (${WT_TARGETS_MIB}MiB); kept $WT_KEPT_LIVE with an origin/ ref still in the checkout, $WT_KEPT_RECENT with git activity inside the window, $WT_KEPT_NO_LANDING unpushed with no landing on record, $WT_KEPT_SOR_UNREAD unpushed because the record could not be read, $WT_KEPT_DIRTY dirty, $WT_KEPT_UNREFERENCED with a head no ref holds, $WT_KEPT_LOCKED locked, $WT_KEPT_REFUSED refused by git; pruned $WT_PRUNED entries whose directory was gone; kept $WT_KEPT_UNSEEN entries outside the mounts this pass can see${WT_KEPT_UNSEEN_NAMES:+ ($WT_KEPT_UNSEEN_NAMES)}; judged $WT_STALE_LOCKS with a stale lock, $WT_OWNERLESS_LOCKS with a lock naming no owner and $WT_RELEASED_LOCKS with a harness lock whose runs are closed"
}

# ---------------------------------------------------------------------
# WORK: stale git worktrees on the ReadWriteOnce PVC.
# ---------------------------------------------------------------------
reclaim_work() {
    local kb gb
    kb=$(free_kb "$WORK_MOUNT")
    if [ -z "$kb" ]; then
        log "$WORK_MOUNT not mounted — skipping worktree reclaim"
        return 0
    fi
    gb=$((kb / 1024 / 1024))
    if [ "$gb" -ge "$WORK_FLOOR_GB" ]; then
        log "$WORK_MOUNT ${gb}GB free >= ${WORK_FLOOR_GB}GB floor (the gate's ${GATE_MIN_FREE_GB} + ${WORK_RECLAIM_MARGIN_GB} margin, infra/build-floor.env) — no worktree reclaim"
        return 0
    fi
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "$REPO_DIR is not a git checkout — cannot prune worktrees"
        return 0
    fi
    log "$WORK_MOUNT ${gb}GB free < ${WORK_FLOOR_GB}GB floor (the gate's ${GATE_MIN_FREE_GB} + ${WORK_RECLAIM_MARGIN_GB} margin, infra/build-floor.env) — pruning stale git worktrees"

    # Metadata first: drop admin entries for worktree dirs that are
    # already gone — gone where this process can SEE, which a bare
    # `git worktree prune` never asked (backlog 52fefc45).
    prune_gone_worktrees
    read_worktree_admin

    # This run's own worktree is off limits — never saw the axe fall on
    # the branch it is standing on.
    local self
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"

    # `path<TAB>locked<TAB>detached` per worktree; the first block is the
    # main worktree. Locked worktrees carry a `locked` line — skip those.
    local removed=0 skipped=0 first=1 path locked detached head admin
    while IFS=$'\t' read -r path locked detached; do
        [ -z "$path" ] && continue
        if [ "$first" = 1 ]; then first=0; continue; fi   # main worktree
        [ "$locked" = 1 ] && { skipped=$((skipped + 1)); continue; }
        [ "$path" = "$REPO_DIR" ] && continue
        case "$self" in "$path"|"$path"/*) continue ;; esac  # our own
        [ -d "$path" ] || continue
        # As in the hourly pass: only a held, paired tree, and its git
        # through the admin entry (backlog 0cc54611).
        if ! admin=$(held_worktree_admin "$path"); then
            log "  kept $path (not held and paired by this pass: no git run in it)"
            skipped=$((skipped + 1))
            continue
        fi

        # Age gate: skip anything touched within the cutoff — an active
        # session writes into its worktree, so a recent mtime means
        # "in use". `-mmin +N` = the dir's own mtime is older than N
        # minutes.
        if [ -z "$(find "$path" -maxdepth 0 -mmin +$((WORKTREE_MAX_AGE_H * 60)) 2>/dev/null)" ]; then
            skipped=$((skipped + 1))
            continue
        fi

        # Committed work outlives the checkout only if a REF names it. A
        # branch's ref survives `git worktree remove`; a detached HEAD's
        # only names are the worktree's HEAD and reflog, which the remove
        # deletes, handing the commits to the next gc. So a detached tree
        # whose head no ref holds is kept and named, as the gone-worktree
        # pass keeps it (adce5171). This pass took such trees at 48h with
        # no reading at all until backlog 5da0428a (2026-09-24), found by
        # the builder of 99ce8744 — the car that raises this floor from 6
        # to 18 GB, so this pass fires far more often.
        if [ "$detached" = 1 ]; then
            head=$(git --git-dir="$admin" --work-tree="$path" rev-parse -q --verify HEAD 2>/dev/null || true)
            if [ -z "$(ref_holding "$head")" ]; then
                log "  kept $path (detached at ${head:0:8}, a head no ref holds: removing the checkout would leave its commits to gc)"
                skipped=$((skipped + 1))
                FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES="${FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES:+$FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES, }$(basename "$path")"
                continue
            fi
        fi

        # No --force: a dirty worktree is refused and left standing, so
        # uncommitted work is never discarded.
        if git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            log "  removed stale worktree $path"
            removed=$((removed + 1))
        else
            log "  kept $path (uncommitted changes or in use — not forced)"
            skipped=$((skipped + 1))
        fi
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" l "\t" d; p=substr($0, 10); l=0; d=0 }
            /^locked/    { l=1 }
            /^detached/  { d=1 }
            END { if (p != "") print p "\t" l "\t" d }
        '
    )

    prune_gone_worktrees
    FLOOR_WORKTREES_REMOVED=$removed

    kb=$(free_kb "$WORK_MOUNT")
    gb=$((kb / 1024 / 1024))
    log "worktree reclaim: removed $removed, kept $skipped — $WORK_MOUNT now ${gb}GB free"
    if [ "$gb" -lt "$WORK_FLOOR_GB" ]; then
        log "WORK FLOOR UNMET — ${gb}GB free < ${WORK_FLOOR_GB}GB after pruning every eligible worktree." >&2
        log "  Remaining use is the clone, locked worktrees, worktrees with uncommitted work, or detached heads no ref holds${FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES:+ ($FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES)}. A human decides next." >&2
        problem work-floor-unmet "$WORK_MOUNT ${gb}GB free < ${WORK_FLOOR_GB}GB after pruning every eligible worktree (removed $removed, kept $skipped); what remains is the clone, locked worktrees, worktrees with uncommitted work, or detached heads no ref holds${FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES:+ ($FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES)}"
    fi
}

# ---------------------------------------------------------------------
# SCRATCH: the regenerable incremental cache under CARGO_TARGET_DIR.
# ---------------------------------------------------------------------
reclaim_scratch() {
    local kb gb SCRATCH_FLOOR_GB
    kb=$(free_kb "$SCRATCH_MOUNT")
    if [ -z "$kb" ]; then
        log "$SCRATCH_MOUNT not mounted — skipping build-cache reclaim"
        return 0
    fi
    gb=$((kb / 1024 / 1024))
    SCRATCH_FLOOR_GB=$(scratch_floor_gb "$(( $(size_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))")
    if [ "$gb" -ge "$SCRATCH_FLOOR_GB" ]; then
        log "$SCRATCH_MOUNT ${gb}GB free >= ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_RULE}) — no build-cache reclaim"
        return 0
    fi
    log "$SCRATCH_MOUNT ${gb}GB free < ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_RULE}) — reclaiming regenerable build cache"

    # The siblings first: every builder's own target, least recently
    # touched first, until the floor is met. They hold the bulk (the
    # 2026-09-23 sawtooth was ~450 GB of them), each is regenerable
    # (wt-cargo reseeds one from the primary), and each carries its own
    # liveness in its mtime — so no build_running() guard, which would
    # see some builder's cargo on a busy pod and defer forever.
    reclaim_targets_to_floor "$SCRATCH_FLOOR_GB"
    gb=$(( $(free_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))
    if [ "$gb" -ge "$SCRATCH_FLOOR_GB" ]; then
        log "$SCRATCH_MOUNT ${gb}GB free — floor met by the sibling targets"
        return 0
    fi

    if [ ! -d "$TARGET_DIR" ]; then
        log "$TARGET_DIR does not exist — nothing to reclaim"
        return 0
    fi

    # NEVER trim the cache out from under a running build. The pod sets
    # shareProcessNamespace, so this sidecar sees the dev container's
    # processes; a live cargo/rustc means "leave the target alone".
    if build_running; then
        log "a build is running (cargo/rustc live) — skipping build-cache trim this pass" >&2
        log "build-cache reclaim deferred: ${gb}GB < ${SCRATCH_FLOOR_GB}GB floor, retried next pass." >&2
        problem build-cache-trim-deferred "$SCRATCH_MOUNT ${gb}GB free < ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_RULE}) after the sibling targets, and the incremental trim was deferred because a build is running (cargo/rustc live); retried next pass"
        return 0
    fi

    # Incremental-compilation caches only: regenerable by definition,
    # and separate from the linked artifacts under deps/. cargo simply
    # rebuilds a missing incremental dir on the next compile.
    local n=0 d
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        rm -rf "$d" && n=$((n + 1))
    done < <(find "$TARGET_DIR" -type d -name incremental -prune 2>/dev/null)
    INCREMENTAL_DROPPED=$n

    kb=$(free_kb "$SCRATCH_MOUNT")
    gb=$((kb / 1024 / 1024))
    log "build-cache reclaim: dropped $n incremental cache dir(s) — $SCRATCH_MOUNT now ${gb}GB free"
    if [ "$gb" -lt "$SCRATCH_FLOOR_GB" ]; then
        log "SCRATCH FLOOR UNMET — ${gb}GB free < ${SCRATCH_FLOOR_GB}GB after dropping every incremental cache." >&2
        log "  The remainder is linked artifacts under $TARGET_DIR/*/deps. Reclaiming those is a" >&2
        log "  full rebuild's worth of cost — run \`cargo clean\` deliberately, or an operator decides." >&2
        problem scratch-floor-unmet "$SCRATCH_MOUNT ${gb}GB free < ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_RULE}) after every idle sibling target and $n incremental cache dir(s); the remainder is linked artifacts under $TARGET_DIR or space this pod does not hold"
    fi
}

# Under the floor: sibling target dirs, least recently touched first,
# until free space reaches $1 GB. A dir touched inside LIVE_TARGET_MIN
# is a build in flight and is never taken; the primary TARGET_DIR is
# the incremental trim's and never removed whole.
reclaim_targets_to_floor() {
    local floor="$1" d kb gb n=0 mib=0 cutoff
    cutoff="@$(( $(date +%s) - LIVE_TARGET_MIN * 60 ))"
    while IFS=$'\t' read -r _ d; do
        [ -n "$d" ] || continue
        gb=$(( $(free_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))
        [ "$gb" -lt "$floor" ] || break
        if [ -n "$(find "$d" -maxdepth 3 -newermt "$cutoff" -print -quit 2>/dev/null)" ]; then
            log "  kept $d — touched within ${LIVE_TARGET_MIN}m, a build in flight"
            continue
        fi
        kb=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
        if remove_tree "$d"; then
            n=$((n + 1))
            mib=$((mib + ${kb:-0} / 1024))
            log "floor target reclaimed: $d ($((${kb:-0} / 1024))MiB, least recently used; ${gb}GB free < ${floor}GB floor)"
        else
            log "could not remove target $d — $REMOVE_WHY" >&2
            problem target-not-removed "$d (idle, under the scratch floor): $REMOVE_WHY"
        fi
    done < <(
        for d in "$SCRATCH_MOUNT"/*/; do
            d="${d%/}"
            [ -d "$d" ] || continue
            [ "$d" = "$TARGET_DIR" ] && continue
            [ -f "$d/CACHEDIR.TAG" ] || [ -d "$d/debug" ] || [ -d "$d/release" ] || continue
            # The newest mtime at depth <= 3 is when a build last wrote.
            printf '%s\t%s\n' "$(find "$d" -maxdepth 3 -printf '%T@\n' 2>/dev/null | sort -n | tail -n1)" "$d"
        done | sort -n
    )
    FLOOR_TARGETS_RECLAIMED=$n
    FLOOR_TARGETS_MIB=$mib
    log "floor-target pass: $n sibling target(s) reclaimed (${mib}MiB), least recently used first"
}

# ---------------------------------------------------------------------
# SCRATCH, the other 62 GB: every OTHER builder's target dir.
# ---------------------------------------------------------------------
# Measured 2026-09-11 01:40Z (backlog 0efbd69e): /scratch held 90 GB,
# $TARGET_DIR was 28 GB of it, and twenty sibling dirs — one per coding
# agent, as their briefs instruct — held 62 GB for branches that had
# landed the day before. The floor pass above scans one path and would
# not have fired anyway at 304 GB free: a dead cache is not a headroom
# problem, it is an AGE problem, so this pass runs on age regardless of
# free space. A target dir is one that carries cargo's CACHEDIR.TAG or
# a debug/ or release/ profile; the primary $TARGET_DIR is never a
# candidate (the floor pass owns it, and only ever trims incremental/).
#
# NO build_running() GUARD HERE, on purpose: a build in progress writes
# to its target continuously, so a dir untouched for STALE_TARGET_H
# hours has no live build in it — the mtime IS the liveness check, per
# dir, which the process scan cannot be (it sees every builder's cargo
# at once and would defer this pass forever on a busy pod).
reclaim_stale_targets() {
    local d n=0 kb since
    [ -d "$SCRATCH_MOUNT" ] || return 0
    since="@$(( $(date +%s) - STALE_TARGET_H * 3600 ))"
    for d in "$SCRATCH_MOUNT"/*/; do
        d="${d%/}"
        [ -d "$d" ] || continue
        [ "$d" = "$TARGET_DIR" ] && continue
        if [ ! -f "$d/CACHEDIR.TAG" ] && [ ! -d "$d/debug" ] && [ ! -d "$d/release" ]; then
            continue
        fi
        # Anything inside touched within the window means a builder is
        # (or was just) using it. Depth-bounded: a target dir has
        # hundreds of thousands of files and the fingerprints at depth 3
        # move on every compile.
        if [ -n "$(find "$d" -maxdepth 3 -newermt "$since" -print -quit 2>/dev/null)" ]; then
            continue
        fi
        kb=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
        if remove_tree "$d"; then
            n=$((n + 1))
            log "stale target reclaimed: $d ($((${kb:-0} / 1024))MiB, untouched for more than ${STALE_TARGET_H}h)"
        else
            log "could not remove stale target $d — $REMOVE_WHY" >&2
            problem target-not-removed "$d (untouched for more than ${STALE_TARGET_H}h): $REMOVE_WHY"
        fi
    done
    log "stale-target pass: $n stale target dir(s) reclaimed under $SCRATCH_MOUNT (age > ${STALE_TARGET_H}h, $TARGET_DIR exempt)"
    STALE_TARGETS_RECLAIMED=$n
}

# ---------------------------------------------------------------------
# THE CLI: the tree's `boss`, out of the cluster image, into /work.
# ---------------------------------------------------------------------
# Measured 2026-09-18 14:4xZ (backlog c35eda6c): /scratch/target/debug/
# boss said `built from 6286857f` (#448) while origin/main stood at
# cb053ed6 (#450), and the same installer this pass runs pulled the CLI
# out of the registry's david/boss:cb053ed into a scratch store in 10
# seconds from this pod — no docker, no root, no manifest change. So:
#
#   * origin/main's sha is the checkout's OWN remote-tracking ref
#     (refs/remotes/origin/main), not a `git ls-remote` of the forge:
#     the sidecar mounts only /work and /scratch (boss-dev.yaml) — no
#     forge token, no HOME carrying the credential helper — and the
#     forge answers an anonymous read of david/boss with 401, so a
#     network read here would skip every hour. The ref is what the
#     operator's session and every builder worktree fetch (they share
#     it), and it is THE SAME ref the shim compares a candidate against
#     — one definition of "main" on the pod, so the sidecar installs
#     exactly the sha the shim will call current (CLAUDE.md §9a). A
#     checkout without the ref installs nothing and says so.
#   * the registry host reaches the installer the way it reaches every
#     managed host: a sor.env rendered from infra/estate/estate.toml
#     (render-sor-env.sh --to), handed as BOSS_SOR_ENV, so
#     forge-defaults.sh builds the image repo from it. Never a literal
#     (the lint the-estate-address-lives-once refuses one).
#   * the store and the link are under WORK_MOUNT: the sidecar runs
#     without root and /usr/local/bin is its own container overlay,
#     invisible to the dev container; /work is the shared mount.
#   * the installer's verdicts are the forge's install.sh's: 0 is the
#     tree's CLI confirmed through the link; 75 is "not yet" — the
#     deploy runner builds the image a few minutes after each train,
#     and this pass runs hourly, so the first pass after a train often
#     lands here; a wait, logged, never a problem; anything else is a
#     refusal (registry dark, digest mismatch, a binary naming another
#     commit) and a problem, loud and on the packet with the exit named.
#     The installer leaves `current` at the previous confirmed
#     generation in every non-zero case.
#
# The installer's output is captured whole and printed under a `cli:`
# prefix — a tail or a digest would throw away the only copy (CLAUDE.md
# §Diagnosis).
CLI_STORE="${BOSS_CLI_STORE:-$WORK_MOUNT/tools/image-cli}"
CLI_LINK="${BOSS_CLI_LINK:-$WORK_MOUNT/tools/bin/boss-image}"
# Dev isolation's store and selector (b3071bf4; design a68fc42a D6): on
# the work volume, where the installer this pass runs keeps the selector
# following main. The doors and the CLI spell /work/tools/dev-control.
DEV_CONTROL_STORE="${BOSS_DEV_CONTROL_STORE:-$WORK_MOUNT/tools/dev-control}"
DEV_CONTROL_STATE=""
CLI_SHA=""
CLI_RESULT=skipped
install_tree_cli() {
    local here installer render sha log rc
    here="$(dirname "$(readlink -f "$0")")"
    installer="${BOSS_CLI_INSTALLER:-$here/../estate/install-cli-from-image.sh}"
    render="$here/../estate/render-sor-env.sh"
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "CLI install skipped: $REPO_DIR is not a git checkout" >&2
        return 0
    fi
    if ! sha=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>&1) || [ -z "$sha" ]; then
        log "CLI install skipped: $REPO_DIR has no refs/remotes/origin/main (git fetch origin in the checkout; ${sha:-empty answer})" >&2
        return 0
    fi
    case "$sha" in
        *[!0-9a-f]*|"") log "CLI install skipped: origin/main resolved to '$sha', not a sha" >&2; return 0 ;;
    esac
    if [ "${#sha}" -ne 40 ]; then
        log "CLI install skipped: origin/main resolved to '$sha' (${#sha} chars), not a full sha" >&2
        return 0
    fi
    CLI_SHA="$sha"
    if [ ! -f "$installer" ] || [ ! -f "$render" ]; then
        log "CLI install skipped: $installer or $render is missing beside this script" >&2
        return 0
    fi
    if ! mkdir -p "$CLI_STORE" || ! bash "$render" --to "$CLI_STORE/sor.env" >/dev/null; then
        log "CLI install FAILED: could not render $CLI_STORE/sor.env from infra/estate/estate.toml" >&2
        CLI_RESULT="failed: no sor.env"
        problem cli-install-failed "could not render $CLI_STORE/sor.env from infra/estate/estate.toml"
        return 0
    fi
    log "CLI: installing the tree's boss at ${sha:0:8} (origin/main) from the cluster image into $CLI_STORE"
    log=$(mktemp "${TMPDIR:-/tmp}/dev-scratch-reclaim-cli.XXXXXX")
    rc=0
    BOSS_SOR_ENV="$CLI_STORE/sor.env" BOSS_CLI_STORE="$CLI_STORE" BOSS_CLI_LINK="$CLI_LINK" \
        BOSS_DEV_CONTROL_STORE="$DEV_CONTROL_STORE" \
        bash "$installer" "$sha" >"$log" 2>&1 || rc=$?
    sed 's/^/dev-scratch-reclaim:   cli: /' "$log"
    rm -f "$log"
    case "$rc" in
        0)  CLI_RESULT=ok
            log "CLI: the tree's boss at ${sha:0:8} is confirmed at $CLI_LINK (store $CLI_STORE); the shim runs it" ;;
        75) CLI_RESULT="not yet"
            log "CLI: not yet — the image for ${sha:0:8} is not in the registry (the deploy runner builds it a few minutes after each train); the next pass retries, and the store stays at $(readlink "$CLI_STORE/current" 2>/dev/null || echo none)" ;;
        *)  CLI_RESULT="failed: exit $rc"
            log "CLI install FAILED (exit $rc) at ${sha:0:8} — the installer's complete output is above; the store stays at $(readlink "$CLI_STORE/current" 2>/dev/null || echo none)" >&2
            problem cli-install-failed "the installer exited $rc at ${sha:0:8}; the store stays at $(readlink "$CLI_STORE/current" 2>/dev/null || echo none), and the sidecar's log holds the installer's whole output" ;;
    esac
}

# ---------------------------------------------------------------------
# DEV ISOLATION IS NEVER SILENT (design a68fc42a D1, David 2026-10-10).
# ---------------------------------------------------------------------
# Read AFTER the installer, which re-activates (or refuses) the selector.
# No store means isolation was never activated on this pod — the held
# state (b3071bf4 held_by_david_20261009), inert and not a problem. Once
# the store exists, every pass that finds the selector refused, missing
# (the doors are running candidate code beside the token again), or
# naming a generation the installed CLI has moved past (the doors froze)
# files a problem on this pass's packet, every hour until it is fixed.
check_dev_control() {
    local selector target current gen why off_mode
    selector="$DEV_CONTROL_STORE/enabled"
    if [ ! -d "$DEV_CONTROL_STORE" ] && [ ! -L "$DEV_CONTROL_STORE" ]; then
        DEV_CONTROL_STATE="inert: never activated"
        log "dev isolation: inert — no store at $DEV_CONTROL_STORE (never activated on this pod)"
        return 0
    fi
    why="read $DEV_CONTROL_STORE/activation.log"
    if [ ! -L "$selector" ] && [ ! -e "$selector" ]; then
        # Judge the same off record as activate-control's load_record
        # and the installer: a public file or a stream ending in off is
        # not the supported rollback (review cd351532, car 3f64de32).
        off_mode=$(stat -c %a "$DEV_CONTROL_STORE/off.json" 2>/dev/null || true)
        if [ -f "$DEV_CONTROL_STORE/off.json" ] && [ ! -L "$DEV_CONTROL_STORE/off.json" ] &&
           [ "$(stat -c %u "$DEV_CONTROL_STORE/off.json" 2>/dev/null)" = "$(id -u)" ] &&
           [ "${off_mode: -2}" = 00 ] &&
           jq_doc_file "$DEV_CONTROL_STORE/off.json" &&
           jq -e -s 'length == 1 and .[0] == {state:"off"}' "$DEV_CONTROL_STORE/off.json" >/dev/null 2>&1; then
            DEV_CONTROL_STATE="off: supported rollback"
            log "dev isolation: off — supported rollback recorded at $DEV_CONTROL_STORE/off.json"
            return 0
        fi
        DEV_CONTROL_STATE="selector missing"
        log "PROBLEM: dev isolation's selector $selector is MISSING while its store exists — the doors run candidate code beside this pod's credentials; $why" >&2
        problem dev-control-selector-missing "the selector $selector is missing while the store exists, so the doors run candidate code beside the token; re-activate, or use the published dev-control deactivate door to record isolation off; $why"
        return 0
    fi
    target="$(readlink "$selector" 2>/dev/null || true)"
    gen="${target#"$DEV_CONTROL_STORE"/generations/}"
    case "$gen" in *[!0-9a-f]*) gen="" ;; esac
    if [ "$gen" = "$target" ] || [ "${#gen}" -ne 40 ]; then
        DEV_CONTROL_STATE="refused: ${target:-not a link}"
        log "PROBLEM: dev isolation is REFUSED — $selector names '${target:-not a link}', so wt-cargo, wt-web, boss --built and the CLI's children refuse with 75; $why" >&2
        problem dev-control-refused "the selector $selector names '${target:-not a link}', not an activated generation, so candidate execution is refused; $why"
        return 0
    fi
    current="$(readlink "$CLI_STORE/current" 2>/dev/null || true)"
    if [ -n "$current" ] && [ "$current" != "$gen" ]; then
        DEV_CONTROL_STATE="behind: on ${gen:0:8}, CLI at ${current:0:8}"
        log "PROBLEM: dev isolation runs generation ${gen:0:8} while the installed CLI is ${current:0:8} — the selector did not follow main; $why" >&2
        problem dev-control-behind "the selector runs generation ${gen:0:8} while the installed CLI is ${current:0:8}, so the doors did not follow main; $why"
        return 0
    fi
    DEV_CONTROL_STATE="on at $gen"
    log "dev isolation: on at ${gen:0:8} ($selector)"
}

# ---------------------------------------------------------------------
# THE DOORS' TREE IS HELD BY ITS OWNER (backlog 3db83d0d; reviewer run
# 6d7b418b of car c93c6dfa, measured on the pod 2026-10-19 08:11Z).
# ---------------------------------------------------------------------
# /work/tools holds what every door runs: bin/ (boss, boss-api,
# wt-cargo, wt-web, kubectl), image-cli/ (the tree's CLI the shim
# prefers) and dev-control/ (the isolation store the doors route
# candidate execution through). That day /work/tools, bin and image-cli
# were drwxrwsr-x root:1500 with no sticky bit, kubectl and image-cli's
# wrapper were 0775, and /work itself was 2775. Every builder under
# as-gate-uid is group 1500, and a group member may rename any entry of
# a group-writable directory that is not sticky: it could move
# dev-control aside and plant a store pointing the doors at an older
# generation or at a launcher of its own, overwrite kubectl, or move
# /work/tools aside whole.
#
# Nothing wrote those modes on purpose (an fsGroup walk and an umask
# did), so nothing kept them. This pass runs as the uid that owns the
# tree (root with every capability dropped; an owner's chmod needs
# none) and holds it every hour:
#   * group and other write come off /work/tools, bin and image-cli and
#     off each entry directly inside them. A link has no mode of its
#     own; the directory it sits in is what guards it. Not recursive: a
#     generation's files are written 0755/0644 by the installer, and a
#     private store (dev-control, 2700) has nothing to take off.
#   * /work gets the sticky bit and keeps group rwx and setgid. The
#     pod's fsGroupChangePolicy is OnRootMismatch, which re-walks the
#     whole volume, adding group write back everywhere, when the root
#     loses either; it ignores the sticky bit.
#   * an entry this uid does not own cannot be held without CAP_CHOWN,
#     which the sidecar does not have. It is a problem, by path and
#     owner, every pass until it is removed: a builder's file in the
#     doors' tree is the finding itself, not noise.
# Holding modes is not reclaiming: like the fast-forward it files no
# packet of its own, and `tools` rides every packet a pass does file.
# No /work/tools at all (a fixture, a fresh pod) is not a problem.
TOOLS_DIR="${BOSS_TOOLS_DIR:-$WORK_MOUNT/tools}"
TOOLS_STATE=""

# One path: owned by this uid, and not group- or other-writable unless
# it is a link. Reads and writes the caller's `me`, `held` and `open`.
hold_tools_entry() {
    local p="$1" owner mode
    owner=$(stat -c %u "$p" 2>/dev/null) || return 0
    if [ "$owner" != "$me" ]; then
        open="${open:+$open; }$p is owned by uid $owner"
        return 0
    fi
    [ -L "$p" ] && return 0
    mode=$(stat -c %a "$p")
    [ $((8#$mode & 8#022)) -eq 0 ] && return 0
    if chmod go-w "$p"; then
        held=$((held + 1))
        log "doors' tree: took group and other write off $p (was $mode)"
    else
        open="${open:+$open; }chmod go-w $p failed (mode $mode)"
    fi
}

hold_tools_tree() {
    local me mount owner mode after dir entry held=0 open=""
    if [ ! -e "$TOOLS_DIR" ] && [ ! -L "$TOOLS_DIR" ]; then
        TOOLS_STATE="absent"
        log "doors' tree: absent, no $TOOLS_DIR"
        return 0
    fi
    me=$(id -u)
    mount="$(dirname "$TOOLS_DIR")"
    if [ -L "$TOOLS_DIR" ] || [ ! -d "$TOOLS_DIR" ]; then
        open="$TOOLS_DIR is not a directory"
    else
        owner=$(stat -c %u "$mount")
        mode=$(stat -c %a "$mount")
        if [ "$owner" != "$me" ]; then
            open="$mount is owned by uid $owner, so its sticky bit is not this pass's to set"
        elif [ $((8#$mode & 8#1000)) -eq 0 ]; then
            if chmod +t "$mount"; then
                held=$((held + 1))
                log "doors' tree: set the sticky bit on $mount (was $mode)"
            else
                open="chmod +t $mount failed (mode $mode)"
            fi
            after=$(stat -c %a "$mount")
            if [ $((8#$mode & 8#2070)) -ne $((8#$after & 8#2070)) ]; then
                open="${open:+$open; }$mount went from $mode to $after, and the kubelet re-walks the volume when its root loses group rwx or setgid"
            fi
        fi
        for dir in "$TOOLS_DIR" "$TOOLS_DIR/bin" "$TOOLS_DIR/image-cli"; do
            { [ -d "$dir" ] && [ ! -L "$dir" ]; } || continue
            hold_tools_entry "$dir"
            while IFS= read -r -d '' entry; do
                hold_tools_entry "$entry"
            done < <(find "$dir" -mindepth 1 -maxdepth 1 -print0 2>/dev/null)
        done
    fi
    if [ -n "$open" ]; then
        TOOLS_STATE="OPEN: $open"
        log "PROBLEM: the doors' tree is NOT held: $open; a group member may still replace what the doors run" >&2
        problem tools-open "the doors' tree is not held: $open; a group member may still replace what the doors run"
        return 0
    fi
    TOOLS_STATE="held"
    [ "$held" -eq 0 ] || TOOLS_STATE="held, re-held $held this pass"
    log "doors' tree: $TOOLS_STATE ($TOOLS_DIR, bin and image-cli not group- or other-writable, $mount sticky)"
}

# THE AGENT-ARTIFACT INVENTORY — A DRY RUN, EVERY HOUR (backlog
# 25022367, design 195276f0, approved by David 2026-10-17). /work
# reached 96% on 2026-10-16 with ~22 GiB of agent scratch, session and
# transcript files on it and no record of which bytes were active work,
# evidence still needed, or transcripts a harness reads back; this pass
# had a lifecycle for worktrees and targets and none for those.
# infra/cluster/dev-artifact-inventory.py accounts every file under the
# families artifact-lifecycle.toml names (the policy, versioned beside
# it) by the reason it is retained, and MUTATES NOTHING it reads: policy
# version 1 grants no action, and its loader refuses any key it does not
# know. The only files it writes are the report, replaced atomically,
# and a growth ring of one totals line per pass, 720 kept (30 days of
# hourly passes: the measured window the item asks for). Measured on the
# live pod 2026-10-18: 167,306 files, 16.4 GiB, a 13 s walk.
#
# AFTER the reclaims, so the reading is of what they left, and BEFORE
# the record, so a failed inventory is a problem the packet names: a
# dry run that did not happen must not read as one that found nothing.
# BOSS_ARTIFACT_INVENTORY=off is for a harness that drives this script
# against a fixture, as BOSS_DEV_MEMORY_WATCH=off is.
#
# SINCE ITS SECOND CAR, THE JOIN AND THE ARCHIVE (design 195276f0): the
# policy's version 2 joins a family's units to the agent runs that own
# them, read from the system of record through THIS pass's reader — the
# same curl helper, the same identity, the same machine-token header
# file sor_read hands curl — so no second way to reach the record
# exists. Where the policy grants `action = "archive"`, the lifecycle
# archives complete evidence losslessly behind the manifest beside the
# report, and `--restore PATH` gives a reader its literal path back:
#   python3 -I infra/cluster/dev-artifact-inventory.py --work-mount /work \
#     --manifest /work/telemetry/artifact-inventory/archive-manifest.jsonl \
#     --restore <path>
# The tree's policy grants no action (artifact-lifecycle.toml says why).
# Exit 65 — a refused action or a conflict a person must read — is a
# problem here like any other. BOSS_ARTIFACT_INVENTORY_NOW (an epoch) is
# the harness's clock: a fixture cannot age a file's ctime, which the
# settle window reads, so a test moves the pass's "now" instead.
ARTIFACT_INVENTORY="${BOSS_ARTIFACT_INVENTORY:-on}"
ARTIFACT_INVENTORY_DIR="${BOSS_ARTIFACT_INVENTORY_DIR:-$WORK_MOUNT/telemetry/artifact-inventory}"
inventory_artifacts() {
    local here said status url api
    local -a record=()
    [ "$ARTIFACT_INVENTORY" != off ] || return 0
    here="$(dirname "$(readlink -f "$0")")"
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    api="$here/../boss-api-curl.sh"
    [ -x "$api" ] || api=boss-api-curl.sh
    if [ -n "$url" ]; then
        record=(--sor-url "$url" --sor-user "$SOR_USER" ${MT_HDR:+-H "$MT_HDR"} --via "$api")
    fi
    said=$(python3 -I "$here/dev-artifact-inventory.py" \
        --policy "$here/artifact-lifecycle.toml" \
        --work-mount "$WORK_MOUNT" \
        --proc-root "$PROC_ROOT" \
        --reserve-gb "$WORK_FLOOR_GB" \
        --report "$ARTIFACT_INVENTORY_DIR/latest.json" \
        --ring "$ARTIFACT_INVENTORY_DIR/growth.jsonl" \
        --manifest "$ARTIFACT_INVENTORY_DIR/archive-manifest.jsonl" \
        ${BOSS_ARTIFACT_INVENTORY_NOW:+--now "$BOSS_ARTIFACT_INVENTORY_NOW"} \
        "${record[@]}" 2>&1) && status=0 || status=$?
    if [ "$status" = 0 ]; then
        log "$said"
        return 0
    fi
    log "PROBLEM: the artifact lifecycle exited $status — $said" >&2
    problem artifact-inventory-failed "the artifact lifecycle exited $status: $said"
}

# ---------------------------------------------------------------------
# THE DEV HOME'S KUBE CONFIG IS HELD BY ITS OWNER (backlog 5ebc7a6d;
# measured by run c38afb80 on the pod 2026-10-19, read again 2026-10-21).
# ---------------------------------------------------------------------
# /work/home/.kube/config was -rw-rw-r-- root:1500, inside /work/home/.kube
# and /work/home, both drwxrwsr-x root:1500 with no sticky bit. Every
# builder under as-gate-uid is group 1500, so it could rewrite the
# config, or rename .kube aside and plant one of its own (review
# 6d7b418b), and kubectl — run as root with HOME=/work/home by the shim
# with isolation off, and by any interactive shell — would send the dev
# session's ServiceAccount token to whatever server that file names.
# Every live reader of it runs as root, the uid that owns it.
#
# Nothing wrote those modes on purpose (the pod's fsGroup and an umask
# did), so nothing kept them. This pass runs as that owner (root with
# every capability dropped; an owner's chmod needs none) and holds them
# every hour:
#   * group and other lose every bit on /work/home/.kube and its config.
#     Below a 0700 .kube nothing else needs holding: kubectl's discovery
#     and HTTP cache (.kube/cache, 2770 that day) and a kuberc (read by
#     default from kubectl 1.34) are out of every other uid's reach.
#   * /work/home gets the sticky bit and keeps group rwx and setgid, so
#     .kube cannot be renamed aside; the home's own group-writable mode
#     is a separate decision (the item says so). The work mount above it
#     is the doors' tree's to make sticky (backlog 3db83d0d); until it
#     is, the audit below names it, because the home can be moved aside
#     whole.
#   * a link, a wrong type, or an entry this uid does not own cannot be
#     held without CAP_CHOWN: a problem, by path, every pass.
# Holding modes is not reclaiming: it files no packet of its own, and
# `home_kube` rides every packet a pass does file. No /work/home (a
# fixture, a fresh pod) is not a problem.
HOME_KUBE_STATE=""
HOME_CREDENTIALS_STATE=""

# One path private to this uid: `want` is d or f. Reads and writes the
# caller's `me`, `held` and `open`.
hold_home_private() {
    local p="$1" want="$2" owner mode
    if [ -L "$p" ]; then
        open="${open:+$open; }$p is a link"
        return 0
    fi
    if { [ "$want" = d ] && [ ! -d "$p" ]; } || { [ "$want" = f ] && [ ! -f "$p" ]; }; then
        open="${open:+$open; }$p is not a $([ "$want" = d ] && echo directory || echo regular file)"
        return 0
    fi
    owner=$(stat -c %u "$p")
    if [ "$owner" != "$me" ]; then
        open="${open:+$open; }$p is owned by uid $owner"
        return 0
    fi
    mode=$(stat -c %a "$p")
    [ $((8#$mode & 8#077)) -eq 0 ] && return 0
    if chmod go-rwx "$p"; then
        held=$((held + 1))
        log "dev home's kube config: took group and other off $p (was $mode)"
    else
        open="${open:+$open; }chmod go-rwx $p failed (mode $mode)"
    fi
}

hold_home_kube() {
    local me owner mode held=0 open=""
    if [ ! -e "$DEV_HOME" ] && [ ! -L "$DEV_HOME" ]; then
        HOME_KUBE_STATE="absent"
        log "dev home's kube config: absent, no $DEV_HOME"
        return 0
    fi
    me=$(id -u)
    if [ -L "$DEV_HOME" ] || [ ! -d "$DEV_HOME" ]; then
        open="$DEV_HOME is not a directory"
    else
        owner=$(stat -c %u "$DEV_HOME")
        mode=$(stat -c %a "$DEV_HOME")
        if [ "$owner" != "$me" ]; then
            open="$DEV_HOME is owned by uid $owner, so its sticky bit is not this pass's to set"
        elif [ $((8#$mode & 8#1000)) -eq 0 ]; then
            if chmod +t "$DEV_HOME"; then
                held=$((held + 1))
                log "dev home's kube config: set the sticky bit on $DEV_HOME (was $mode)"
            else
                open="chmod +t $DEV_HOME failed (mode $mode)"
            fi
        fi
        if [ -e "$DEV_HOME/.kube" ] || [ -L "$DEV_HOME/.kube" ]; then
            hold_home_private "$DEV_HOME/.kube" d
            if [ -e "$DEV_HOME/.kube/config" ] || [ -L "$DEV_HOME/.kube/config" ]; then
                hold_home_private "$DEV_HOME/.kube/config" f
            fi
        fi
    fi
    if [ -n "$open" ]; then
        HOME_KUBE_STATE="OPEN: $open"
        log "PROBLEM: the dev home's kube config is NOT held: $open; a group member may still point kubectl, and the session's token, at another server" >&2
        problem home-kube-open "the dev home's kube config is not held: $open; a group member may still point kubectl, and the session's token, at another server"
        return 0
    fi
    HOME_KUBE_STATE="held"
    [ "$held" -eq 0 ] || HOME_KUBE_STATE="held, re-held $held this pass"
    log "dev home's kube config: $HOME_KUBE_STATE ($DEV_HOME sticky, .kube and its config private to uid $me)"
}

# THE AUDIT (the same item): every other credential-adjacent path under
# the home a group or other may write, by name, every pass. It NAMES and
# holds nothing: who reads each of these, and whether a group member
# must write any, is each one's own decision, and a mode changed here
# would make it silently. Adjacent means the credential itself, every
# directory between it and the home (a writable one lets a member
# rename the credential aside and plant its own), and a listed
# directory's direct entries. The list: SSH keys; the boss config (the
# actor every verb signs as, and the dev workers' bindings); the agent
# CLIs' session tokens; git's, curl's and docker's stored credentials.
# A link is guarded by the directory it sits in, which is checked.
audit_home_credentials() {
    local path p mode rel named="" count=0 candidates
    [ -d "$DEV_HOME" ] && [ ! -L "$DEV_HOME" ] || return 0
    mode=$(stat -c %a "$WORK_MOUNT" 2>/dev/null || echo 0)
    if [ $((8#$mode & 8#022)) -ne 0 ] && [ $((8#$mode & 8#1000)) -eq 0 ]; then
        named="$WORK_MOUNT ($mode, not sticky: the home can be moved aside whole)"
        count=1
    fi
    for path in "$DEV_HOME"/.ssh "$DEV_HOME"/.config/boss "$DEV_HOME"/.claude*/.credentials.json \
        "$DEV_HOME"/.codex/auth.json "$DEV_HOME"/.gemini/oauth_creds.json \
        "$DEV_HOME"/.git-credentials "$DEV_HOME"/.netrc "$DEV_HOME"/.docker/config.json; do
        [ -e "$path" ] || [ -L "$path" ] || continue
        candidates=("$path")
        p="$(dirname "$path")"
        while [ "$p" != "$DEV_HOME" ] && [ "$p" != / ]; do
            candidates+=("$p")
            p="$(dirname "$p")"
        done
        if [ -d "$path" ] && [ ! -L "$path" ]; then
            while IFS= read -r -d '' p; do
                candidates+=("$p")
            done < <(find "$path" -mindepth 1 -maxdepth 1 -print0 2>/dev/null)
        fi
        for p in "${candidates[@]}"; do
            [ -L "$p" ] && continue
            mode=$(stat -c %a "$p" 2>/dev/null) || continue
            [ $((8#$mode & 8#022)) -ne 0 ] || continue
            rel="${p#"$DEV_HOME"/}"
            case "; $named; " in *"; $rel ("*) continue ;; esac
            named="${named:+$named; }$rel ($mode)"
            count=$((count + 1))
        done
    done
    if [ "$count" -eq 0 ]; then
        HOME_CREDENTIALS_STATE="private"
        log "dev home's credentials: private (no credential-adjacent path under $DEV_HOME is group- or other-writable)"
        return 0
    fi
    HOME_CREDENTIALS_STATE="OPEN: $count path(s)"
    log "PROBLEM: credential-adjacent paths under $DEV_HOME a group or other may write: $named" >&2
    problem home-credentials-open "credential-adjacent paths under $DEV_HOME a group or other may write, so a group member may replace or rename aside what they hold: $named"
}

# ---------------------------------------------------------------------
# THE LAUNCH REFS: what a killed `boss dispatch --launch` leaves.
# ---------------------------------------------------------------------
# WHY (backlog af54aae4, from review 09e63624 of car ff1b7a35). A launch
# fetches main into refs/boss-launch/<run>, a ref of its own run's, and
# deletes it once the worktree stands on it (dispatch_launch.rs,
# launch_ref and fetched_main). A dispatch killed outright mid-fetch —
# SIGKILL, an OOM, a pod roll — runs no deletion, so the ref, and a
# `.lock` beside it when git died mid-update, stays in this checkout and
# nothing removed it: a ref per killed launch, for ever.
#
# JUDGED BY THE RECORD, NOT BY AGE. The ref is needed only while its
# launch fetches, and the launch is over once its RUN is closed: a
# launch that ended wrote its run's ending, and one killed outright left
# its run open and silent, which the silence clock
# (agent-run-dies-when-building-is-silent) ends `died` within hours. So
# this arm only reads what that clock already decided; it keeps no clock
# of its own. A ref's age cannot be read anyway for the refs this exists
# for: `git gc --auto` packs every loose ref, and a packed ref has no
# mtime, so an age rule would keep exactly the old ones. A run still open
# keeps its ref; a record that cannot be read keeps every ref and is
# asked once a pass, as the worktree pass's is (a dark record asked per
# ref would spend a retry window on each).
LAUNCH_REFS_REMOVED=0; LAUNCH_REFS_REMOVED_NAMES=""
LAUNCH_REFS_KEPT_OPEN=0; LAUNCH_REFS_KEPT_UNREAD=0; LAUNCH_REFS_UNREAD_REASON=""
reclaim_launch_refs() {
    local common dir run reply rc status said
    local -a runs=()
    common=$(git -C "$REPO_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 0
    dir="$common/refs/boss-launch"
    # The refs, loose or packed, and every lock a killed update left —
    # with or without the ref it was taking.
    mapfile -t runs < <(
        {
            git -C "$REPO_DIR" for-each-ref --format='%(refname:lstrip=2)' refs/boss-launch/ 2>/dev/null || true
            if [ -d "$dir" ]; then
                find "$dir" -maxdepth 1 -type f -name '*.lock' -printf '%f\n' 2>/dev/null | sed 's/\.lock$//' || true
            fi
        } | sort -u
    )
    for run in "${runs[@]}"; do
        # An id goes into a path: anything but a uuid's alphabet is not one.
        case ${run:-empty} in empty | *[!0-9A-Za-z-]*) continue ;; esac
        [ -n "$LAUNCH_REFS_UNREAD_REASON" ] || [ -z "$WT_SOR_UNREAD_REASON" ] \
            || LAUNCH_REFS_UNREAD_REASON="$WT_SOR_UNREAD_REASON"
        if [ -n "$LAUNCH_REFS_UNREAD_REASON" ]; then
            LAUNCH_REFS_KEPT_UNREAD=$((LAUNCH_REFS_KEPT_UNREAD + 1))
            continue
        fi
        rc=0
        reply=$(sor_read "/api/jobs/$run") || rc=$?
        status=$(printf '%s' "$reply" | jq -r '.status // empty' 2>/dev/null) || status=
        if [ "$rc" -ne 0 ] || [ -z "$status" ]; then
            LAUNCH_REFS_UNREAD_REASON="the read of agent-run $run failed (curl exit $rc, status '${status}')"
            log "launch refs: the system of record could not be read — $LAUNCH_REFS_UNREAD_REASON; every launch ref is kept this pass" >&2
            LAUNCH_REFS_KEPT_UNREAD=$((LAUNCH_REFS_KEPT_UNREAD + 1))
            continue
        fi
        if [ "$status" != closed ]; then
            LAUNCH_REFS_KEPT_OPEN=$((LAUNCH_REFS_KEPT_OPEN + 1))
            continue
        fi
        # The lock first: while it stands, git refuses to delete the ref.
        # Nothing holds it — the run that took it is closed.
        said=""
        rm -f -- "$dir/$run.lock" 2>/dev/null || said="its lock could not be removed"
        if [ -z "$said" ] && git -C "$REPO_DIR" rev-parse --verify -q "refs/boss-launch/$run" >/dev/null 2>&1; then
            said=$(git -C "$REPO_DIR" update-ref -d "refs/boss-launch/$run" 2>&1) && said=""
        fi
        if [ -n "$said" ]; then
            log "PROBLEM: refs/boss-launch/$run, of a closed run, could not be removed — $said" >&2
            problem launch-ref-unremovable "refs/boss-launch/$run, of a closed run, could not be removed: $said"
            continue
        fi
        log "  removed refs/boss-launch/$run: agent-run ${run:0:8} is closed, so its launch is over"
        LAUNCH_REFS_REMOVED=$((LAUNCH_REFS_REMOVED + 1))
        LAUNCH_REFS_REMOVED_NAMES="${LAUNCH_REFS_REMOVED_NAMES:+$LAUNCH_REFS_REMOVED_NAMES }${run:0:8}"
    done
}

# A run's terminal protocol fact outlives its crashed launcher (f0de9c6f,
# B3). Observe it here, in the pod which already owns the controller.
# This does not judge death: unread, open or malformed evidence keeps the
# worker. Only stop-run selects its name/UID, under the controller lock.
WORKERS_RETIRED=0; WORKERS_KEPT_OPEN=0; WORKERS_KEPT_UNREAD=0; WORKER_PUBLICATIONS=0
WORKERS_OBSERVED=0; WORKER_RETIREMENT_FAILED=0; WORKER_PUBLICATION_PENDING=0
worker_problem() {
    # problem() stores a bounded line; retain the whole diagnosis first.
    log "PROBLEM: terminal workers: $2" >&2
    problem "$1" "$2"
}
reclaim_terminal_workers() {
    local controller cursor_dir cursor retired_cursor inventory run reply receipt patch readback next said rc url api here temporary
    case "$DEV_CONTROL_STATE" in 'on at '*) ;; *) return 0 ;; esac
    controller="$DEV_CONTROL_STORE/dev-build"
    cursor_dir="$WORK_MOUNT/telemetry/terminal-workers"
    mkdir -p -m 700 "$cursor_dir" || { worker_problem worker-cursor 'cannot retain terminal-worker inventory cursor'; return; }
    if [ -L "$cursor_dir" ] || [ "$(stat -c %u "$cursor_dir")" != "$(id -u)" ]; then
        worker_problem worker-cursor 'terminal-worker cursor directory is not owned regular storage'; return
    fi
    cursor=$(jq -r '.active // ""' "$cursor_dir/cursor" 2>/dev/null || true)
    retired_cursor=$(jq -r '.retired // ""' "$cursor_dir/cursor" 2>/dev/null || true)
    inventory=$(timeout 30 "$controller" owned-runs --after "$cursor" --retired-after "$retired_cursor" --limit 4 2>&1) && rc=0 || rc=$?
    if [ "$rc" != 0 ] || ! jq_doc_text "$inventory" || ! jq -e 'type == "object" and (.runs | type == "array") and (.runs | length <= 4) and (.next | type == "string")' <<<"$inventory" >/dev/null 2>&1; then
        worker_problem worker-inventory-unread "terminal-worker inventory refused (exit $rc): $inventory"
        return
    fi
    here="$(dirname "$(readlink -f "$0")")"
    api="$here/../boss-api-curl.sh"; [ -x "$api" ] || api=boss-api-curl.sh
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    while IFS= read -r run; do
        WORKERS_OBSERVED=$((WORKERS_OBSERVED + 1))
        if ! [[ "$run" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
            worker_problem worker-invalid-handle 'controller returned a noncanonical run handle'; continue
        fi
        said=$(jq -r --arg run "$run" '.runs[] | select(.run == $run) | .error // empty' <<<"$inventory")
        if [ -n "$said" ]; then
            WORKERS_KEPT_UNREAD=$((WORKERS_KEPT_UNREAD + 1))
            worker_problem worker-owner-unread "run $run kept: controller ownership unread: $said"; continue
        fi
        reply=$(BOSS_API_RETRY_DEADLINE=15 sor_read "/api/jobs/$run" 15) && rc=0 || rc=$?
        if [ "$rc" != 0 ] || ! jq_doc_text "$reply" || ! jq -e --arg run "$run" '.id == $run and .kind == "agent-run" and (.status == "open" or .status == "closed")' <<<"$reply" >/dev/null 2>&1; then
            WORKERS_KEPT_UNREAD=$((WORKERS_KEPT_UNREAD + 1))
            worker_problem worker-terminal-unread "run $run kept: terminal record unread (exit $rc): $reply"; continue
        fi
        if [ "$(jq -r .status <<<"$reply")" != closed ]; then
            WORKERS_KEPT_OPEN=$((WORKERS_KEPT_OPEN + 1)); continue
        fi
        if ! jq_doc_text "$reply" || ! jq -e '.metadata | (.outcome as $o | ["landed","unreported","died","refused"] | index($o) != null) and (.closed_at | type == "string" and length > 0)' <<<"$reply" >/dev/null 2>&1; then
            WORKERS_KEPT_UNREAD=$((WORKERS_KEPT_UNREAD + 1))
            worker_problem worker-terminal-invalid "run $run kept: closure lacks a known terminal outcome and stamp"; continue
        fi
        receipt=$(timeout 180 "$controller" stop-run "$run" 2>&1) && rc=0 || rc=$?
        if [ "$rc" != 0 ] || ! jq_doc_text "$receipt" || ! jq -e --arg run "$run" '.run == $run and ((.retirement | type == "object") or .state == "not-admitted")' <<<"$receipt" >/dev/null 2>&1; then
            WORKER_RETIREMENT_FAILED=$((WORKER_RETIREMENT_FAILED + 1))
            worker_problem worker-retirement-failed "run $run retirement refused (exit $rc): $receipt"; continue
        fi
        # Retried stop-run returns its durable exact receipt without a
        # second destructive call. A failed PATCH/readback remains unpaid.
        if jq_doc_text "$reply" && jq -e --argjson receipt "$receipt" '.metadata.worker_retirement == $receipt' <<<"$reply" >/dev/null 2>&1; then
            continue
        fi
        if jq_doc_text "$reply" && jq -e '.metadata.worker_retirement != null' <<<"$reply" >/dev/null 2>&1; then
            WORKER_PUBLICATION_PENDING=$((WORKER_PUBLICATION_PENDING + 1))
            worker_problem worker-publication-conflict "run $run retains a different retirement receipt; original and controller evidence kept, nothing overwritten"; continue
        fi
        WORKERS_RETIRED=$((WORKERS_RETIRED + 1))
        patch=$(jq -c --argjson receipt "$receipt" --arg generation "${DEV_CONTROL_STATE#on at }" --arg observed "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '. as $run | {worker_retirement:$receipt,worker_terminal_cleanup:{actor:"automation:dev-scratch-reclaim",run:$run.id,outcome:$run.metadata.outcome,closed_at:$run.metadata.closed_at,observed_at:$observed,controller_generation:$generation,physical_gc:"not observed"}}' <<<"$reply")
        # The header stays a file, as in sor_read; neither credential nor
        # caller-provided API destination enters a native controller argv.
        said=$(BOSS_API_RETRY_DEADLINE=15 "$api" -fsS --max-time 15 --connect-timeout 5 -X PATCH -H "x-boss-user: $SOR_USER" ${MT_HDR:+-H "$MT_HDR"} \
            -H 'Content-Type: application/json' --data-binary "$patch" "$url/api/jobs/$run/metadata" 2>&1) && rc=0 || rc=$?
        readback=$(BOSS_API_RETRY_DEADLINE=15 sor_read "/api/jobs/$run" 15) || rc=$?
        if [ "$rc" != 0 ] || ! jq_doc_text "$readback" || ! jq -e --argjson patch "$patch" '.metadata.worker_retirement == $patch.worker_retirement and .metadata.worker_terminal_cleanup == $patch.worker_terminal_cleanup' <<<"$readback" >/dev/null 2>&1; then
            WORKER_PUBLICATION_PENDING=$((WORKER_PUBLICATION_PENDING + 1))
            worker_problem worker-publication-pending "run $run retirement receipt publication/readback unpaid (exit $rc): $said"; continue
        fi
        WORKER_PUBLICATIONS=$((WORKER_PUBLICATIONS + 1))
        log "worker $run: terminal retirement copied and read back; physical Job/PVC GC remains unobserved"
    done < <(jq -r '.runs[].run' <<<"$inventory")
    next=$(jq -c '{active:.next,retired:(.retired_next // "")}' <<<"$inventory")
    temporary=$(mktemp "$cursor_dir/cursor.XXXXXX") || { worker_problem worker-cursor 'cannot retain next worker inventory cursor'; return; }
    printf '%s' "$next" > "$temporary" && mv "$temporary" "$cursor_dir/cursor"
}

# ---------------------------------------------------------------------
# THE RECORD: a packet for a pass that did something.
# ---------------------------------------------------------------------
# The sidecar's log is read by nobody (`kubectl logs -c reclaim`, by
# hand, on a pod whose restarts lose it) — CLAUDE.md §Diagnosis, a check
# nobody reads is a check that is not running. So a pass that removed
# anything, ended with a floor unmet, or could not answer (the worktree
# pass skipped, counted as a problem), opens a maintenance packet and
# completes its `run` step with the totals, through the SAME two
# helpers every timer chore uses (the timer is the executor, the Job is
# the visibility — boss-maintenance-wrap.sh) rather than a third
# packet-filing idiom. Where it files is not a default: BOSS_JOBS_URL,
# else the one spelling of the system of record every pod door reads,
# infra/dev/sor-url beside this checkout. Neither named, the pass runs
# unrecorded and says so; an unreachable API costs the same — the
# executor never waits on its visibility.
RECLAIM_KIND=maintenance-dev-scratch-reclaim

# THE MEMORY WATCH'S STATE, for the packet (backlog 9ecd12ea; the long
# form is at the hourly pass's `--ensure` below). `--state` prints
# key=value lines — the loop's pid and generation, the newest sample's
# age, alerts kept / refused / dropped, what --ensure last decided — and
# each rides the run step as its own key. BOSS_DEV_MEMORY_WATCH=off is
# for a harness that drives this script against a fixture pod: there the
# watch has nothing to stand outside of, and its refusal would be every
# test's problem.
MEMORY_WATCH="${BOSS_DEV_MEMORY_WATCH:-on}"
MEMORY_WATCH_SCRIPT="$(dirname "$(readlink -f "$0")")/dev-memory-watch.sh"
WATCH_STARTED=0
WATCH_ACKED=0
WATCH_ACKS=""
PASS_RECORDED=0
WATCH_STATE=()
read_watch_state() {
    WATCH_STATE=()
    if [ "$MEMORY_WATCH" = off ]; then
        WATCH_STATE=("memory_watch=off")
        return 0
    fi
    mapfile -t WATCH_STATE < <(bash "$MEMORY_WATCH_SCRIPT" --state 2> /dev/null)
    [ "${#WATCH_STATE[@]}" -gt 0 ] || WATCH_STATE=("memory_watch=not sampling: its state could not be read")
}
watch_field() {
    local kv
    for kv in "${WATCH_STATE[@]}"; do
        case "$kv" in "$1="*) printf '%s' "${kv#*=}"; return 0 ;; esac
    done
}

git_phase_state() {
    local result coverage=unknown
    result=$(BOSS_RECLAIM_GIT_DIR="${BOSS_RECLAIM_GIT_DIR:-${WORK_MOUNT:-/work}/telemetry/dev-reclaim-git}" \
        bash "$RECLAIM_GIT_OBSERVER" --state 2>/dev/null) || result=""
    [ -n "$result" ] || result='{"state":"unknown","reason":"phase reader unavailable"}'
    if [ -n "${GIT_PHASE_ATTEMPT_FILE:-}" ] && [ -r "$GIT_PHASE_ATTEMPT_FILE" ] && [ ! -s "$GIT_PHASE_ATTEMPT_FILE" ]; then
        coverage=no_observed_recording_failure
    else
        printf '%s\n' 'reclaim-git-observer: unknown attempt coverage; a phase refusal or unavailable independent signal remains' >&2
    fi
    printf '%s' "$result" | jq -c --arg coverage "$coverage" \
        '. + {attempt_coverage: $coverage} | if $coverage == "unknown" then .state = "unknown" else . end' \
        || printf '%s\n' '{"state":"unknown","attempt_coverage":"unknown","reason":"phase state unavailable"}'
}

record_pass() {
    local acted git_phase_state hold_pending=0 hold_summary="" hold_send="" hold_output=""
    git_phase_state=$(git_phase_state)
    # The floor mode reaches here without the hourly pass's read.
    [ "${#WATCH_STATE[@]}" -gt 0 ] || read_watch_state
    acted=$((CHECKOUT_RETURNED + WT_REMOVED + WT_TARGETS_REMOVED + WT_PRUNED + FLOOR_WORKTREES_REMOVED + STALE_TARGETS_RECLAIMED + FLOOR_TARGETS_RECLAIMED + INCREMENTAL_DROPPED + LAUNCH_REFS_REMOVED + WATCH_STARTED + WATCH_ACKED + WT_HELD_NOW + WT_ANCESTORS_HELD + WORKERS_RETIRED + WORKER_PUBLICATIONS))
    [ -z "$WT_HOLD_PENDING_FILE" ] || [ ! -f "$WT_HOLD_PENDING_FILE" ] || hold_pending=1
    if [ "$acted" -eq 0 ] && [ "$problems" -eq 0 ] && [ "$hold_pending" -eq 0 ]; then
        log "nothing reclaimed and no floor unmet — a pass that only looked files no packet"
        return 0
    fi
    local here wrap step url result
    here="$(dirname "$(readlink -f "$0")")"
    wrap="$here/../boss-maintenance-wrap.sh"
    step="$here/../boss-step.sh"
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    if [ -z "$url" ]; then
        log "no system of record named (BOSS_JOBS_URL unset, $here/../dev/sor-url absent) — this pass is unrecorded" >&2
        return 0
    fi
    if [ ! -f "$wrap" ] || [ ! -f "$step" ]; then
        log "$wrap or $step is missing beside this script — this pass is unrecorded" >&2
        return 0
    fi
    result=ok
    [ "$problems" -gt 0 ] && result=incomplete
    # Keep the whole latest observation on the file door, not in one
    # argv value (Linux caps each at 128 KiB). The held common Git
    # directory retains an unpublished snapshot for the next pass;
    # this is a latest-state reading, not a history of every chmod.
    if [ -n "$WT_HOLD_PENDING_FILE" ]; then
        hold_summary=$(mktemp "${WT_HOLD_PENDING_FILE%/*}/.worktree-hold-summary.XXXXXX") || return 1
    else
        hold_summary=$(mktemp) || return 1
    fi
    if ! printf '%s' "$WT_HOLD_INVENTORY" | jq -c --arg sha "$WT_HOLD_SHA256" \
        --arg at "$(date -u +%FT%TZ)" --argjson total "$WT_HOLD_TOTAL" --argjson held "$WT_HOLD_HELD" \
        --arg state "$WT_HOLD_STATE" --argjson complete "$WT_HOLD_COMPLETE" \
        '{worktree_hold_state:$state,worktree_hold_complete:$complete,worktree_hold_inventory:.,worktree_hold_sha256:$sha,worktree_hold_hash_format:"compact-sorted-keys-no-newline",worktree_hold_total:$total,worktree_hold_held:$held,worktree_hold_at:$at}' > "$hold_summary"; then
        log "the complete worktree inventory could not be prepared; no coverage recorded" >&2
        rm -f -- "$hold_summary"
        return 1
    fi
    if [ -n "$WT_HOLD_PENDING_FILE" ]; then
        mv -f -- "$hold_summary" "$WT_HOLD_PENDING_FILE" || { rm -f -- "$hold_summary"; return 1; }
        hold_summary=$WT_HOLD_PENDING_FILE
    fi
    if ! BOSS_JOBS_URL="$url" bash "$wrap" "$RECLAIM_KIND" "Dev pod scratch reclaim"; then
        log "could not open the $RECLAIM_KIND packet — full latest inventory retained at $hold_summary" >&2
        [ -n "$WT_HOLD_PENDING_FILE" ] || rm -f -- "$hold_summary"
        return 0
    fi
    hold_send=$(mktemp) || return 1
    cp -- "$hold_summary" "$hold_send" || { rm -f -- "$hold_send"; return 1; }
    hold_output=$(mktemp) || { rm -f -- "$hold_send"; return 1; }
    printf '%s' "$PROBLEM_LINES" > "$hold_output" || { rm -f -- "$hold_send" "$hold_output"; return 1; }
    BOSS_JOBS_URL="$url" BOSS_STEP_ACTOR=automation:dev-scratch-reclaim BOSS_RUN_SUMMARY_FILE="$hold_send" BOSS_STEP_REQUIRE_RECORD=1 BOSS_STEP_OUTPUT_FILE="$hold_output" \
        bash "$step" "$RECLAIM_KIND" run \
            "result=$result" "problems=$problems" \
            "home_kube=$HOME_KUBE_STATE" "home_credentials=$HOME_CREDENTIALS_STATE" \
            "worktree_pass=$WT_PASS" "worktree_pass_reason=$WT_PASS_REASON" \
            "ff_result=$FF_RESULT" "ff_to=$FF_TO" "ff_detail=$FF_DETAIL" \
            "ff_returned_from=$FF_RETURNED_FROM" "checkout=$CHECKOUT_STATE" \
            "origin_main_sha=$WT_MAIN_SHA" "origin_main_ref_ts=$WT_MAIN_TS" \
            "worktrees_removed=$WT_REMOVED" "worktrees_removed_mib=$WT_REMOVED_MIB" \
            "worktrees_removed_by_content=$WT_REMOVED_BY_CONTENT" \
            "worktrees_removed_by_record=$WT_REMOVED_BY_RECORD" \
            "worktrees_kept_unpushed_no_landing=$WT_KEPT_NO_LANDING" \
            "worktrees_kept_unpushed_sor_unread=$WT_KEPT_SOR_UNREAD" "worktrees_sor_unread_reason=$WT_SOR_UNREAD_REASON" \
            "worktrees_kept_dirty=$WT_KEPT_DIRTY" "worktrees_kept_dirty_names=$WT_KEPT_DIRTY_NAMES" \
            "worktrees_kept_unreferenced=$WT_KEPT_UNREFERENCED" "worktrees_kept_unreferenced_names=$WT_KEPT_UNREFERENCED_NAMES" \
            "worktrees_kept_live=$WT_KEPT_LIVE" "worktrees_kept_recent=$WT_KEPT_RECENT" \
            "worktrees_kept_locked=$WT_KEPT_LOCKED" "worktrees_kept_refused=$WT_KEPT_REFUSED" \
            "worktrees_stale_locks=$WT_STALE_LOCKS" "worktrees_stale_lock_names=$WT_STALE_LOCK_NAMES" \
            "worktrees_ownerless_locks=$WT_OWNERLESS_LOCKS" "worktrees_ownerless_lock_names=$WT_OWNERLESS_LOCK_NAMES" \
            "worktrees_released_locks=$WT_RELEASED_LOCKS" "worktrees_released_lock_names=$WT_RELEASED_LOCK_NAMES" \
            "worktrees_pruned=$WT_PRUNED" \
            "worktrees_kept_unseen=$WT_KEPT_UNSEEN" "worktrees_kept_unseen_names=$WT_KEPT_UNSEEN_NAMES" \
            "targets_removed=$WT_TARGETS_REMOVED" "targets_removed_mib=$WT_TARGETS_MIB" \
            "floor_worktrees_removed=$FLOOR_WORKTREES_REMOVED" "floor_worktrees_kept_unreferenced_names=$FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES" \
            "stale_targets_reclaimed=$STALE_TARGETS_RECLAIMED" \
            "floor_targets_reclaimed=$FLOOR_TARGETS_RECLAIMED" "floor_targets_mib=$FLOOR_TARGETS_MIB" \
            "incremental_dirs_dropped=$INCREMENTAL_DROPPED" \
            "launch_refs_removed=$LAUNCH_REFS_REMOVED" "launch_refs_removed_names=$LAUNCH_REFS_REMOVED_NAMES" \
            "launch_refs_kept_open=$LAUNCH_REFS_KEPT_OPEN" "launch_refs_kept_unread=$LAUNCH_REFS_KEPT_UNREAD" \
            "launch_refs_unread_reason=$LAUNCH_REFS_UNREAD_REASON" \
            "workers_retired=$WORKERS_RETIRED" "worker_publications=$WORKER_PUBLICATIONS" \
            "workers_observed=$WORKERS_OBSERVED" "worker_retirement_failed=$WORKER_RETIREMENT_FAILED" \
            "worker_publication_pending=$WORKER_PUBLICATION_PENDING" \
            "workers_kept_open=$WORKERS_KEPT_OPEN" "workers_kept_unread=$WORKERS_KEPT_UNREAD" \
            "cli_sha=$CLI_SHA" "cli_result=$CLI_RESULT" "dev_control=$DEV_CONTROL_STATE" \
            "tenant_ff=$TENANT_FF" \
            "tools=$TOOLS_STATE" \
            "git_phase_ledger=$git_phase_state" \
            "${WATCH_STATE[@]}" \
        && { PASS_RECORDED=1; rm -f -- "$hold_summary"; } \
        || { log "could not complete the $RECLAIM_KIND run step — its packet stays open; full latest worktree inventory retained at $hold_summary for retry; full problems follow in this pass's journal" >&2
             printf '%s\n' "$PROBLEM_LINES" >&2; }
    # Only the held durable snapshot survives a failed send. The journal
    # already contains the problems; per-attempt transport files do not.
    rm -f -- "$hold_send" "$hold_output"
    [ -n "$WT_HOLD_PENDING_FILE" ] || rm -f -- "$hold_summary"
}

# `--cli`: the CLI leg alone, its exit the verdict — 0 confirmed, 75
# not yet (the installer's own code), 1 failed. The hourly pass above
# counts not yet as a wait, exit 0; here it is a STATUS, because since
# 2026-09-18 the shim (infra/dev/boss) runs this leg itself before a
# write verb and must tell "installed, proceed" from "wait for the
# deploy runner" without parsing the log (backlog 49d9e99d). Runnable
# from the dev container by hand as well.
if [ "${1:-}" = "--cli" ]; then
    install_tree_cli
    [ "$problems" -eq 0 ] || exit 1
    [ "$CLI_RESULT" != "not yet" ] || exit 75
    exit 0
fi

# `--scratch-floor`: the scratch floor's sibling pass ALONE, run by
# infra/dev/wt-cargo before every build (backlog 3f2a08ab). WHY: the
# dev pod was evicted at 2026-09-24 01:27Z with the 25% floor in force.
# The gate receipts' free_gb (GiB free on w-1's ephemeral xfs, one
# reading per verdict) date the drain: 487 at 21:47Z, 378 at 00:01,
# 284 at 00:53, 218 at 01:14, 159 at 01:20, 136 at the eviction, 567
# at 01:31 — ~430 GiB of it this pod's own /scratch. The hourly passes
# at 21:58, 22:59 and 23:59 took no floor target, and the ~01:00 pass
# filed nothing, so it found the floor met; from there w-1 went from
# above 232 GiB free to the kubelet's 139 in under 27 minutes. A 93 GiB
# margin read once an hour cannot hold a drain that crosses it in 27
# minutes, so the reclaim follows the EVENT that fills the disk, as the
# forge's did (the_reclaim_follows_the_build.rs): each build is where
# /scratch grows, so each build checks first.
#
# WHY THE FLOOR, AND NOT THE PRIORITY, IS THE PROTECTION. The kubelet
# ranks eviction candidates by "usage exceeds request" FIRST and by
# priority only within that class. This pod requests 100Gi and its
# /scratch held ~430 GiB of real bytes — more by the kubelet's measure,
# which walks st_blocks and so counts every reflink-seeded sibling at
# full size — while each gate stayed inside its 90Gi. So the pod was
# the only low-priority pod over its request, and the boss-dev-session
# class never came into play: once w-1 reaches the 15% line, this pod
# is first whatever its priority. Staying off that line is the only
# lever, and it is this one.
#
# What it does NOT do, on purpose: the worktree, age, checkout and CLI
# legs (the hourly pass's; a build must not wait on git or a registry),
# and the incremental trim (it defers whenever any cargo runs, which on
# a busy pod is always — that deferral is the hourly pass's to report).
# Above the floor it is one df and says nothing. Concurrent builds race
# to it, so one takes the lock and the rest build on.
if [ "${1:-}" = "--scratch-floor" ]; then
    kb=$(free_kb "$SCRATCH_MOUNT")
    [ -n "$kb" ] || exit 0
    floor_gb=$(scratch_floor_gb "$(( $(size_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))")
    [ $((kb / 1024 / 1024)) -lt "$floor_gb" ] || exit 0
    if command -v flock >/dev/null 2>&1 && { exec 9>>"$SCRATCH_MOUNT/.dev-scratch-reclaim.lock"; } 2>/dev/null; then
        flock -n 9 || exit 0
    fi
    log "$SCRATCH_MOUNT $((kb / 1024 / 1024))GB free < ${floor_gb}GB floor (${SCRATCH_FLOOR_RULE}) — the build-triggered floor pass"
    WT_PASS_REASON="scratch-floor: the build-triggered pass runs only the floor's sibling reclaim"
    reclaim_targets_to_floor "$floor_gb"
    record_pass
    exit 0
fi

# FIRST, before the hourly pass runs or reads anything out of the
# checkout as root: the memory watch below, the token's address in
# infra/dev/sor-url, git (backlog c19ee02b).
hold_checkout
# Every pass, first (backlog 2b1bf9b4, aa1f7d7c): hold the git configs
# root's git reads in the dev container and its shells, then name what
# could not be held and any curl, wget or netrc file in the dev home.
hold_root_gitconfigs
audit_home_client_config

# THE MEMORY WATCH (backlog 9ecd12ea, 2026-10-08): the dev container was
# OOMKilled at its 32Gi limit and nothing had recorded what filled it or
# that it happened. infra/cluster/dev-memory-watch.sh keeps a ring of
# per-process samples on the PVC and raises one alert per container
# restart; this pass only makes sure its loop is alive, because this
# pass is what the sidecar runs from the checkout and the manifest is
# not the place (an edit there rolls the pod and ends every session).
# Hourly passes only — the two modes above have exited by here, and both
# run inside the dev container, where a sampler dies with what it
# watches. Never fatal: the reclaim does not wait on its neighbour.
#
# AND IT SAYS SO ON THE PACKET (review of car 7849553a, finding 2).
# Every way `--ensure` declines returns 0, so a watch that never started
# left one line on the sidecar's stdout, which nobody reads. The watch's
# own state now rides this pass's packet (record_pass), a pass that
# STARTED the loop counts as having acted, and a watch that is not
# sampling after `--ensure` is a PROBLEM — both of which file the
# packet, so the refusal and its reason reach a reader.
if [ "$MEMORY_WATCH" != off ]; then
    bash "$MEMORY_WATCH_SCRIPT" --ensure \
        || log "the memory watch could not be ensured — the dev container is unsampled until a pass manages it" >&2
    read_watch_state
    case "$(watch_field memory_watch_ensure)" in started*) WATCH_STARTED=1 ;; esac
    if [ "$(watch_field memory_watch)" != sampling ]; then
        log "PROBLEM: the dev memory watch is $(watch_field memory_watch) — ensure said: $(watch_field memory_watch_ensure)" >&2
        problem memory-watch-not-sampling "the dev memory watch is $(watch_field memory_watch); ensure said: $(watch_field memory_watch_ensure)"
    fi
    # AN ALERT THAT DID NOT LEAVE IS A PROBLEM TOO (delta review, N2).
    # The counts ride the packet, but a packet is filed only for a pass
    # that acted or had a problem — so on a healthy pod a kill alert the
    # API refused five times, or one the spool dropped, would be counted
    # on a packet nobody files. It stays a problem every hour until a
    # person has read it (each refused body has its refusal beside it as
    # .why, in alert-refused/ under the watch's directory on the work
    # volume) and acknowledged it with the watch's --acknowledge, which
    # keeps the bodies and the dropped count in acknowledged/ and puts
    # the act on the next packet. Emptying the directory by hand is the
    # unrecorded act that verb replaced (backlog 9ecd12ea; this comment
    # still prescribed it until review 1cd6b7a1, backlog 8aff9d6b).
    watch_refused="$(watch_field memory_watch_alerts_refused)"
    watch_dropped="$(watch_field memory_watch_alerts_dropped)"
    case "${watch_refused:-0}${watch_dropped:-0}" in
        *[!0]*)
            log "PROBLEM: the dev memory watch holds alerts that never reached the system of record — ${watch_refused:-0} refused and moved aside, ${watch_dropped:-0} dropped from a full spool; once read, acknowledge them with $MEMORY_WATCH_SCRIPT --acknowledge" >&2
            problem memory-watch-alerts-unsent "the dev memory watch holds alerts that never reached the system of record: ${watch_refused:-0} refused and moved aside, ${watch_dropped:-0} dropped from a full spool; once read, acknowledge them with $MEMORY_WATCH_SCRIPT --acknowledge"
            ;;
    esac
    # AN ALERT HELD WITHOUT BEING COUNTED IS A PROBLEM TOO (review
    # fb97dd5d of car 1fe37401, finding F1). An answer that judges the
    # caller or the API — a 403 to the watch's signer while this pass's
    # own kind still files, a 5xx one body provokes — counts nothing
    # against the alert, so the counts above stay 0 and the alert waits
    # in the spool with no end but its bound of 48. The watch names how
    # long its oldest kept alert has waited and the bound past which no
    # refusal of the body explains it; this only compares the two.
    watch_held_age="$(watch_field memory_watch_alerts_oldest_kept_age_s)"
    watch_held_after="$(watch_field memory_watch_alerts_held_after_s)"
    case "${watch_held_age:-empty}${watch_held_after:-empty}" in
        *[!0-9]*) ;;
        *)
            if [ "$watch_held_age" -gt "$watch_held_after" ]; then
                log "PROBLEM: the dev memory watch has held a kill alert unsent for ${watch_held_age} s, past its ${watch_held_after} s bound, with nothing counted against it — the jobs API keeps answering something that is not a refusal of the body (no answer, a 5xx, or a refusal of automation:dev-scratch-reclaim); its answers are in $(watch_field memory_watch_dir)/watch.log" >&2
                problem memory-watch-alerts-held "the dev memory watch has held a kill alert unsent for ${watch_held_age} s, past its ${watch_held_after} s bound, with nothing counted against it: the jobs API keeps answering something that is not a refusal of the body (no answer, a 5xx, or a refusal of automation:dev-scratch-reclaim); its answers are in $(watch_field memory_watch_dir)/watch.log"
            fi
            ;;
    esac
    # A FROZEN RING RETIRED WHILE AN ITEM HELD IT IS A PROBLEM TOO
    # (backlog 0143612a). The watch keeps every ring an open item names
    # out of its rotation, up to a bound; past it the oldest is removed
    # and counted, and a count riding only a packet nobody files is the
    # silent loss that item measured. It stays a problem until a person
    # acknowledges it, the same verb as above.
    watch_retired="$(watch_field memory_watch_rings_retired)"
    case "${watch_retired:-0}" in
        0 | *[!0-9]*) ;;
        *)
            log "PROBLEM: the dev memory watch retired ${watch_retired} frozen ring(s) an item not yet read closed still named, past its bound — each is named in $(watch_field memory_watch_dir)/kills-retired.log; once read, acknowledge them with $MEMORY_WATCH_SCRIPT --acknowledge" >&2
            problem memory-watch-rings-retired "the dev memory watch retired ${watch_retired} frozen ring(s) an item not yet read closed still named, past its bound: each is named in $(watch_field memory_watch_dir)/kills-retired.log; once read, acknowledge them with $MEMORY_WATCH_SCRIPT --acknowledge"
            ;;
    esac
    # AN ACKNOWLEDGEMENT IS ON THE RECORD (the same call). Clearing the
    # problem above is a person's act, so the pass after it files a
    # packet even when nothing else happened, the acknowledgement rides
    # it (memory_watch_acknowledged), and only once that packet's run
    # step is written does the watch hear it was carried — a dark system
    # of record leaves it to ride the next one.
    WATCH_ACKS="$(watch_field memory_watch_acknowledgements)"
    [ -z "$WATCH_ACKS" ] || WATCH_ACKED=1
fi

# THE MACHINE TOKEN, ONCE, HERE — PRESENTED AND NEVER REQUIRED (backlog
# 37742794; David's answer to design-doc c8502e17, `reclaim-sidecar`,
# 2026-10-07). The worktree pass asks the record which cars landed
# (sor_read), and until this line every one of those reads went out
# with x-boss-user alone: 66 would-refuse facts in the 26 h to
# 2026-10-07T18:00Z, and on the day the machine door enforces, a record
# that cannot be read — which fails closed, so no landed worktree is
# ever reclaimed early again. The sidecar mounts Secret
# boss-machine-token at the reader's default directory
# (infra/cluster/manifests/boss-dev.yaml); the reader leaves MT_HDR
# naming a 0600 file, and sor_read hands curl that FILE.
#
# AFTER the two modes above, on purpose: `--cli` and `--scratch-floor`
# run in the dev container, before a write verb and before every build,
# where no token is mounted at that directory and none is wanted; they
# source nothing. (The floor mode's packet is still stamped: its wrap
# and step read the doors' directory, which infra/dev/wt-cargo names to
# this one child — backlog ffd309c1.) HERE and not inside sor_read,
# because that runs in `$( )` and the reader chains its cleanup onto
# THIS shell's EXIT.
#
# Every way this can go wrong leaves MT_HDR empty and the pass exactly
# as it was: no system of record named, no reader beside this checkout,
# one that does not parse (a syntax error in a sourced file ends the
# shell, `set -e` or not, so it is asked first), no mount, a blank or
# malformed slot, a host that is not the estate's, a header file that
# could not be written. The wrap and the step below make their own.
#
# AFTER THE MEMORY WATCH'S `--ensure` ABOVE, AND NOTHING IS HANDED TO IT
# (the re-rail over car 7849553a, 2026-10-08). The watch's loop is this
# container's too, so the mount is its mount: each tick is its own bash,
# and its alert POST and owner read take the token from the same reader
# in that process (dev-memory-watch.sh file_kept_alerts, alert-lib.sh
# alert_machine_headers). MT_HDR is this shell's variable, never
# exported, and it is made only after the loop was started, so no child
# of this pass inherits a header path. The watch's read of the cluster
# API carries the pod's service-account token and never this one
# (dev_memory_watch_sh.rs holds both).
MT_HDR=""
_reclaim_here="$(dirname "$(readlink -f "$0")")"
_reclaim_sor="${BOSS_JOBS_URL:-$(head -n1 "$_reclaim_here/../dev/sor-url" 2>/dev/null || true)}"
if [ -n "$_reclaim_sor" ] && [ -r "$_reclaim_here/../lib/secret-header.sh" ] \
    && bash -n "$_reclaim_here/../lib/secret-header.sh" 2>/dev/null; then
    # shellcheck source=infra/lib/secret-header.sh
    . "$_reclaim_here/../lib/secret-header.sh"
    machine_token_header MT_HDR "$_reclaim_sor" || MT_HDR=""
fi

fast_forward_checkout
fast_forward_tenant_checkouts
# Before the installer, so the CLI it lands goes into a held tree.
hold_tools_tree
install_tree_cli
check_dev_control
reclaim_terminal_workers
# Before any git runs in a worktree: hold each tree's top and .git file,
# and pair the file with its admin entry (backlog 0cc54611).
hold_worktrees
# Before the audit, so it reads the home this pass has held.
hold_home_kube
audit_home_credentials
reclaim_gone_worktrees
reclaim_launch_refs
reclaim_work
reclaim_stale_targets
reclaim_scratch
inventory_artifacts
record_pass
# The acknowledgements this pass's packet carried (read above, BEFORE the
# packet, so one made since is not marked by a packet that never saw it).
# shellcheck disable=SC2086 # names, word-split on purpose; --recorded skips any that is not one
if [ "$PASS_RECORDED" = 1 ] && [ -n "$WATCH_ACKS" ]; then
    bash "$MEMORY_WATCH_SCRIPT" --recorded $WATCH_ACKS \
        || log "could not mark the memory watch's acknowledgements recorded — the next packet carries them again" >&2
fi

if [ "$problems" -gt 0 ]; then
    exit 1
fi
log "both workspaces above their floors — nothing more to do"
exit 0
