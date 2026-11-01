# wt-target-dir.sh — the ONE name for a worktree's own cargo target
# dir, the ONE decision about when a build must leave a shared one, and
# (since backlog 9ecd12ea) the ONE width a worktree's cargo runs at.
# Sourced by infra/dev/wt-cargo (which seeds the dir) and by
# infra/gate.sh (which must not check into a dir another checkout
# owns). Not executable; it runs nothing by itself.
#
# WHY (backlog 955c99b6, 2026-09-22). The pod's manifest exports
# CARGO_TARGET_DIR=/scratch/target for the whole container
# (infra/cluster/manifests/boss-dev.yaml). wt-cargo overrides it per
# worktree, so every builder's real build is isolated — but gate.sh
# set only CARGO_INCREMENTAL, so its cargo inherited the SHARED dir.
# Six builders ran concurrently that day: six source trees, one target
# directory, and `infra/gate.sh --lint` — the door CLAUDE.md names
# before pushing — false-redded a clean tree three times for one
# builder, with `no variant named List found for enum boss_expr::Expr`
# against source that compiles cleanly under wt-cargo.
#
# THE MECHANISM, reproduced in a two-package fixture rather than
# inferred: cargo's artifact identity does not include the workspace
# PATH — two packages of the same name and version at different paths
# were given the same `-C metadata` hash and shared one rmeta — and
# freshness is judged by resolving the dep-info's cwd-RELATIVE source
# paths against the current directory. A neighbour's rlib written
# minutes ago is therefore newer than this tree's checked-out sources,
# reads as fresh, and this tree compiles against the neighbour's API.
# The failure is indistinguishable from a real compile error and names
# a real-looking symbol, which is what makes it expensive: a door that
# reds a clean tree teaches builders to distrust it or to re-run until
# green, and either habit destroys the door (CLAUDE.md §Diagnosis, "a
# check nobody reads is a check that is not running").
#
# WHY A LINKED WORKTREE IS THE WHOLE TEST. It is the only checkout
# that shares a target dir with a checkout it does not own. The
# cluster gate runner `git clone`s into a per-run emptyDir and exports
# its own /gate-target/target (infra/gate-runner/run.sh), forge CI
# clones too, and the operator's /work/boss owns /scratch/target — all
# three are MAIN worktrees, all three are left exactly as they are.
# Redirecting a gate would trade a false red for a cold ~74 GB build.

# wt_target_dir <target-root> <repo-root>
#
# The per-worktree dir's name, spelled once. Keyed on the worktree's
# basename, which is what wt-cargo has seeded since 2026-09-12 — so
# gate.sh lands in a dir the builder's own build already warmed rather
# than a third one.
wt_target_dir() {
    printf '%s/target-%s\n' "$1" "$(basename "$2")"
}

# wt_target_lock <target-root> <repo-root>
#
# The lock a build holds on that dir (backlog 94cd0c23, 2026-09-27):
# wt-cargo takes it SHARED for cargo's whole run, and a green's free
# (boss-cli scratch_target.rs, `lock_path`, pinned equal) takes it
# EXCLUSIVE without waiting and keeps the target when it will not come.
# BESIDE the dir, not inside it, because the free deletes the dir while
# it holds the lock; a plain file, so the reclaim's `*/` walk never
# counts it as a target.
wt_target_lock() {
    printf '%s.lock\n' "$(wt_target_dir "$1" "$2")"
}

# wt_isolate_target_dir [repo-root]
#
# Points CARGO_TARGET_DIR at this worktree's own dir when the ambient
# one belongs to another checkout, and says so on stderr. Silent and
# inert in every other case — an unset variable, a main checkout, a
# dir already inside this tree, a dir that is already the answer.
wt_isolate_target_dir() {
    # Nothing set means cargo uses <workspace>/target, which is
    # per-worktree by construction. Do NOT invent a /scratch here: a
    # machine without one would get a dir nothing seeds.
    [ -n "${CARGO_TARGET_DIR:-}" ] || return 0

    local root gitdir common mine
    root=${1:-}
    if [ -z "$root" ]; then
        root=$(git rev-parse --show-toplevel 2>/dev/null) || return 0
    fi
    [ -n "$root" ] || return 0

    # A LINKED worktree has its own git dir under the common one; a
    # main checkout has them equal. This is the question "does another
    # checkout own the dir I was handed", asked of git rather than of
    # a path pattern that a rename would falsify.
    gitdir=$(git -C "$root" rev-parse --git-dir 2>/dev/null) || return 0
    common=$(git -C "$root" rev-parse --git-common-dir 2>/dev/null) || return 0
    [ "$gitdir" != "$common" ] || return 0

    # Already this tree's own.
    case "$CARGO_TARGET_DIR" in
        "$root" | "$root"/*) return 0 ;;
    esac

    # Beside the shared one, so the isolated dir lands on the same
    # filesystem — /scratch is the XFS with reflink that makes
    # wt-cargo's seed free.
    mine=$(wt_target_dir "$(dirname "$CARGO_TARGET_DIR")" "$root")
    [ "$mine" != "$CARGO_TARGET_DIR" ] || return 0

    # One line, naming both dirs and the packet, because a builder who
    # sees this run use a different target dir than the run before
    # must be able to read why without opening a script.
    printf '%s\n' "gate: this is a linked worktree — building into $mine, \
not the shared $CARGO_TARGET_DIR that other checkouts write (backlog 955c99b6). \
Cargo keys artifacts by package name and version, not by workspace path, and \
judges freshness against cwd-relative sources, so a neighbour's stale rlib \
reads as fresh here and reds a clean tree." >&2
    if [ ! -d "$mine" ]; then
        printf '%s\n' "gate: $mine does not exist yet, so this build starts cold; \
wt-cargo reflink-seeds it from the shared dir in seconds." >&2
    fi
    export CARGO_TARGET_DIR="$mine"
}

# wt_build_jobs <repo-root> <pod-bound>
#
# The cargo width for a build in <repo-root>, decided once. A BUILDER
# worktree (basename agent-*) builds 4 wide: the pod is one cgroup
# shared by every builder and the operator's interactive session
# (measured 2026-09-14: two builders and the operator throttled it ~10
# min). Every other tree takes the pod's bound, which the caller reads
# from pod-build.env and hands in. WT_JOBS wins over either. wt-cargo
# builds at this width and gate.sh's pre-flight checks at it, so the
# number has one home (backlog 9ecd12ea).
wt_build_jobs() {
    case "$(basename "$1")" in
        agent-*) printf '%s\n' "${WT_JOBS:-4}" ;;
        *) printf '%s\n' "${WT_JOBS:-$2}" ;;
    esac
}

# wt_bound_build_jobs [repo-root]
#
# Bounds an UNBOUNDED cargo in a linked worktree to wt_build_jobs'
# width, and says so on stderr. Inert when the caller already chose a
# width (wt-cargo exports its own; a human may) and in a main checkout.
#
# WHY (backlog 9ecd12ea, ask 3). The dev container was OOM-killed at its
# 32Gi limit on 2026-10-08, and the memory watch that car 7849553a
# installed has since shown what the unbounded consumer is: `gate.sh
# --lint`, the door every builder runs before gating, ran `cargo clippy
# --workspace` at cargo's default width, one job per CPU — and on this
# pod `nproc` reads the NODE (32), not the container's cgroup (16 CPU /
# 32Gi). Measured 2026-10-10 in the ring at 07:48:04Z: one such check
# (`cargo check --workspace --all-features --all-targets`, the cargo a
# clippy run drives) held ten clippy-drivers at once, 3.6 GB summed and
# the two largest 1.10 GB (boss_dispatcher_handlers) and 1.05 GB
# (boss_jobs) — wider than the four wt-cargo ever lets a builder build;
# 32 drivers at the larger figure is more than the container holds.
# Every wt-cargo build was already bounded; this was the cargo that was
# not. It is the widest consumer the watch has seen, not a proven cause
# of the 10-08 kill, which predates the watch and left no samples.
#
# A MAIN checkout is left alone for the reason wt_isolate_target_dir
# leaves it: it is the gate runner's per-run clone or forge CI, each on
# a machine sized for one build, and slowing the judge to suit the dev
# pod is the trade pod-build.env refuses for `.cargo/config.toml`. A
# missing bound is refused rather than defaulted, as wt-cargo refuses.
wt_bound_build_jobs() {
    [ -z "${CARGO_BUILD_JOBS:-}" ] || return 0

    local root gitdir common env_file bound
    root=${1:-}
    if [ -z "$root" ]; then
        root=$(git rev-parse --show-toplevel 2>/dev/null) || return 0
    fi
    [ -n "$root" ] || return 0
    gitdir=$(git -C "$root" rev-parse --git-dir 2>/dev/null) || return 0
    common=$(git -C "$root" rev-parse --git-common-dir 2>/dev/null) || return 0
    [ "$gitdir" != "$common" ] || return 0

    # BASH ONLY, and said so (backlog 9f7ee428, ask 4). The file is found
    # beside this helper through BASH_SOURCE, which dash does not have:
    # there it printed `Bad substitution`, read the file as
    # `/pod-build.env`, and refused in the missing-bound words below —
    # naming a file that was never in question. Both callers, wt-cargo
    # and gate.sh, are bash; a caller that is not gets the reason.
    if [ -z "${BASH_VERSION:-}" ]; then
        printf '%s\n' "gate: wt_bound_build_jobs needs bash — it finds infra/dev/pod-build.env beside itself through BASH_SOURCE, which this shell does not set, so it cannot read this linked worktree's cargo bound. Refusing." >&2
        return 2
    fi

    env_file=$(dirname "${BASH_SOURCE[0]}")/pod-build.env
    # MISSING and DECLARING NOTHING are two refusals, as they are in
    # wt-cargo (ask 1): a missing file is a checkout or fixture that did
    # not carry it, an empty one is an edit to the file itself, and one
    # sentence for both sent the reader to the wrong repair.
    if [ ! -f "$env_file" ]; then
        printf '%s\n' "gate: $env_file is missing — this linked worktree's cargo bound has no home to read, and an unbounded cargo here is one job per NODE CPU against the dev container's memory (backlog 9ecd12ea). Refusing." >&2
        return 2
    fi
    bound=$(
        CARGO_BUILD_JOBS=
        # shellcheck source=pod-build.env
        . "$env_file" 2>/dev/null && printf '%s' "${CARGO_BUILD_JOBS:-}"
    ) || bound=
    if [ -z "$bound" ]; then
        printf '%s\n' "gate: $env_file sets no CARGO_BUILD_JOBS — this linked worktree's cargo has no bound to read, and an unbounded one here is one job per NODE CPU against the dev container's memory (backlog 9ecd12ea). Refusing." >&2
        return 2
    fi

    CARGO_BUILD_JOBS=$(wt_build_jobs "$root" "$bound")
    export CARGO_BUILD_JOBS
    printf '%s\n' "gate: this is a linked worktree — cargo runs $CARGO_BUILD_JOBS jobs wide, the width wt-cargo builds it at, not one per node CPU (backlog 9ecd12ea: an unbounded workspace clippy was the widest cargo the dev container's memory watch saw)." >&2
}
