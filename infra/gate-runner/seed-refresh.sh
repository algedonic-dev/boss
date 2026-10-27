#!/usr/bin/env bash
# seed-refresh.sh — the gate's seed housekeeping, run AFTER its verdict.
#
# WHY THIS IS ITS OWN SCRIPT (backlog 3e2c8ddc; design b4bf8bba, both
# questions approved 2026-10-16). The gate runner wrote its verdict,
# then refreshed the warm seed and emptied its workspace, then ended -
# and the conductor reads a verdict only from a container that has ENDED
# (boss-cli train/carried_verdict.rs). So a green waited out its own
# housekeeping before its packet said so: 638 s of reflink copy on
# gate-run 86dd2d48 (2026-10-16), and on 4aad01a9 (2026-10-17) about
# eight minutes of `find /gate-target -delete` in uninterruptible I/O
# (folded in from 82ffaff3). The approved layout runs the judging `gate`
# as an init container and this script as the regular container after
# it: the gate ends on its verdict and the kubelet publishes that end at
# once, and the housekeeping runs here, bounded and recorded, unable to
# delay or change what was judged.
#
# ONE DEFINITION OF THE REFRESH. run.sh's old layout (gate as the only
# regular container, still rendered by every branch cut before the
# layout lands) calls `refresh` here too, inside its own container, so
# the staged copy, the prune and the lock discipline live once (§9a).
#
# WHAT RUNS HERE IS MAIN'S. This file reaches a pod only as a key of
# ConfigMap gate-runner-script (cluster-deploy-runner.sh writes it from
# main after each train), never from the branch being gated. It runs no
# git, reads no branch file as code, makes no request, holds no
# credential, and takes nothing from the gate but one line it validates
# against a fixed shape: `refresh <40 hex>` or `skip <word>`.
#
# Subcommands:
#   refresh <head>  the staged refresh under the exclusive, non-blocking
#                   seed lock. Exit 0 refreshed, 2 skipped because
#                   another writer holds the lock, 3 skipped because the
#                   seed is already at <head>, anything else failed - the
#                   previous seed stands, or none does; never a torn one.
#   maintain        the helper container: refresh as the gate's handoff
#                   says, bounded; then empty the workspace, bounded; then
#                   leave ONE record line, on stdout and as this
#                   container's termination message, for the conductor.
#                   Always exits 0: housekeeping that went wrong is
#                   stated in the record, and is never a second verdict.
set -uo pipefail

SEED="${GATE_SEED_DIR:-/gate-seed}"
SEED_LOCK="$SEED/.seed.lock"
WS="${GATE_WORKSPACE_DIR:-/gate-target}"
HANDOFF="$WS/.seed-handoff"

# PRUNE THE STALE BINARIES BEFORE THE SEED IS RENAMED INTO PLACE.
# Measured 2026-09-17 17:58Z, the day the seed grew past the workspace:
# /gate-seed/target was 154G, 153G of it debug/deps, and 3,226 of those
# files were TEST AND BIN EXECUTABLES — 138 builds of `boss`, 195 of
# `boss_dispatcher`, 126 of `rebuild_e2e`, each 50–110 MB, one per
# gate that ever relinked them. Cargo names an executable
# <stem>-<16 hex> and never deletes a superseded one, so the seed
# grows by every relink and a reflink copy of it is measured by the
# kubelet at full size: the train gate for #424 was evicted two minutes
# in ("Usage of EmptyDir volume gate-workspace exceeds the limit
# 160Gi") before a single check ran, and settled LOST. An executable is
# never reused across source changes — the next gate relinks it in
# seconds — so the seed keeps ONE per stem (the newest) and drops the
# rest. Libraries (.rlib/.rmeta/.so/.d) are untouched: those ARE the
# warmth. Pure over its argument; the refresh calls it on the staged
# copy, so a torn prune can only ever touch target.partial.
prune_seed_binaries() { # <deps dir> — prints "pruned N binaries, M MiB"
    local deps="$1" n=0 bytes=0 f stem
    [ -d "$deps" ] || { echo "pruned 0 binaries, 0 MiB"; return 0; }
    # newest first, so the first of each stem is the keeper
    while IFS= read -r f; do
        case "$f" in *.*) continue ;; esac
        [ -f "$deps/$f" ] && [ -x "$deps/$f" ] || continue
        stem=$(printf '%s' "$f" | sed -E 's/-[0-9a-f]{16}$//')
        [ "$stem" = "$f" ] && continue
        case " $KEPT " in *" $stem "*)
            bytes=$((bytes + $(stat -c %s "$deps/$f")))
            rm -f -- "$deps/$f" "$deps/$f.d" && n=$((n + 1)) ;;
        *) KEPT="$KEPT $stem" ;;
        esac
    done < <(ls -t "$deps" 2>/dev/null)
    echo "pruned $n binaries, $((bytes / 1048576)) MiB"
}

# THE REFRESH: exclusive and non-blocking, staged, then renamed.
#   - EXCLUSIVE, NON-BLOCKING lock. Readers hold the lock shared while
#     copying; a second refresher just skips (-n) — best-effort
#     housekeeping does not queue.
#   - ALREADY CURRENT is asked again INSIDE the lock: the gate's own
#     check ran before its verdict, and another refresher may have
#     installed this very head since.
#   - STAGE THEN RENAME. The copy lands in target.partial and is
#     mv-ed into place; a pod that dies mid-refresh (w-1 has reset
#     mid-gate before) leaves a MISSING seed — next gate cold, slow,
#     correct — never a torn one under a fresh-looking marker. The
#     old seed is removed first because two targets (~74G each) do
#     not fit the 120Gi volume; the cold window is the price of
#     fitting, and it opens only on a death or a timeout mid-refresh.
refresh_seed() { # <head> — exit 0 refreshed, 2 lock busy, 3 already current, else failed
    local head="$1"
    (
        flock -x -n 9 || { echo "seed-refresh: another writer holds the seed lock - skipped"; exit 2; }
        if [ "$(cat "$SEED/.seed-head" 2>/dev/null || true)" = "$head" ]; then
            echo "seed-refresh: seed already at $head - not refreshed"
            exit 3
        fi
        rm -f "$SEED/.seed-head" &&
        rm -rf "$SEED/target" "$SEED/target.partial" &&
        cp -a --reflink=auto "$WS/target" "$SEED/target.partial" &&
        { KEPT=""; prune_seed_binaries "$SEED/target.partial/debug/deps"; } &&
        mv "$SEED/target.partial" "$SEED/target" &&
        echo "$head" > "$SEED/.seed-head"
    ) 9>>"$SEED_LOCK"
}

# The bounds, from the manifest beside this container (design b4bf8bba,
# Q2: an initial 15-minute deadline and a 30-second grace, above the
# 638 s measured). Each of the two phases gets its own.
bound() { # <value> <default> — a positive integer, or the default
    case "${1:-}" in ''|*[!0-9]*|0) echo "$2" ;; *) echo "$1" ;; esac
}
DEADLINE=$(bound "${GATE_SEED_MAINTENANCE_DEADLINE_SECS:-}" 900)
GRACE=$(bound "${GATE_SEED_MAINTENANCE_GRACE_SECS:-}" 30)

# What `timeout` answered, as the record's word. 124 is the deadline's
# TERM, 137 the grace's KILL — both "timed-out": nothing was finished.
# A KILL cannot reach a process in uninterruptible I/O (the 82ffaff3
# shape), so `timeout` itself then waits on it: the bound is real for a
# slow copy or delete and is not for a stuck disk, which this record can
# only date afterwards. The verdict no longer waits on either.
outcome_of() { # <rc> <word for 0>
    case "$1" in
        0) echo "$2" ;;
        2) echo skipped:lock-busy ;;
        3) echo skipped:already-current ;;
        124|137) echo timed-out ;;
        *) echo failed ;;
    esac
}

maintain() {
    local job="${NATIVE_JOB_NAME:-}" term="${GATE_TERMINATION_LOG:-/dev/termination-log}"
    local line head=- refresh cleanup rc t0 refresh_s=0 cleanup_s=0
    mkdir -p "$WS"
    # THE WORKSPACE IS LIVE WHILE THIS RUNS: the same shared lock run.sh
    # held, so no gate's sweep (run.sh, sweep_dead_workspaces) can take
    # the exclusive one and delete what is being copied. Between the gate
    # ending and this taking it, the sweep's one-hour idle floor covers
    # the seconds of the gap.
    exec 8>>"$WS/.alive"
    flock -s 8
    # The handoff is ONE line of a fixed shape, never a path or a command,
    # and at most 128 bytes of it are read.
    line=""
    IFS= read -r -n 128 line < "$HANDOFF" 2>/dev/null || true
    if [[ "$line" =~ ^refresh\ ([0-9a-f]{40})$ ]]; then
        head="${BASH_REMATCH[1]}"
        t0=$SECONDS
        timeout -k "$GRACE" "$DEADLINE" bash "${BASH_SOURCE[0]}" refresh "$head"
        rc=$?
        refresh_s=$((SECONDS - t0))
        refresh=$(outcome_of "$rc" refreshed)
        echo "seed-refresh: refresh of $head ended $refresh after ${refresh_s}s"
    elif [[ "$line" =~ ^skip\ ([a-z-]{1,40})$ ]]; then
        refresh="skipped:${BASH_REMATCH[1]}"
    elif [ -z "$line" ]; then
        refresh="skipped:no-handoff"
    else
        refresh="refused:handoff-shape"
    fi
    # EMPTY THE WORKSPACE, bounded, after the refresh has read it. What a
    # timeout leaves is a dead run's directory, which the next gate's
    # sweep removes once it has been idle for an hour.
    t0=$SECONDS
    timeout -k "$GRACE" "$DEADLINE" find "$WS" -mindepth 1 -delete 2>/dev/null
    rc=$?
    cleanup_s=$((SECONDS - t0))
    cleanup=$(outcome_of "$rc" done)
    line=$(printf 'gate-runner: seed-maintenance v1 job=%s head=%s refresh=%s refresh_s=%s cleanup=%s cleanup_s=%s deadline_s=%s' \
        "${job:--}" "$head" "$refresh" "$refresh_s" "$cleanup" "$cleanup_s" "$DEADLINE")
    echo "$line"
    { printf '%s\n' "$line" > "$term"; } 2>/dev/null \
        || echo "seed-refresh: WARN: the record could not be written to $term - the conductor will record this housekeeping as unknown"
    return 0
}

case "${1:-}" in
    refresh)
        [[ "${2:-}" =~ ^[0-9a-f]{40}$ ]] || { echo "seed-refresh: refresh needs a 40-hex head" >&2; exit 64; }
        refresh_seed "$2"
        ;;
    maintain)
        maintain
        ;;
    *)
        echo "usage: seed-refresh.sh refresh <head> | maintain" >&2
        exit 64
        ;;
esac
