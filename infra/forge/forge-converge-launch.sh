#!/usr/bin/env bash
# forge-converge-launch — WHICH forge-converge.sh this tick runs. Installed
# by infra/forge/install.sh as a root-owned copy outside every tree
# (/usr/local/libexec/boss/forge-converge-launch), and it is what
# forge-converge.service starts. It sources nothing.
#
# WHY A LAUNCHER (backlog a604a35b). The forge converge is the loop that
# installs this host's repairs, and since this car it runs from the tree
# root fetched (infra/forge/root-tree.sh) — which the converge itself
# moves. A generation whose converge script cannot run, or cannot bring
# in the next generation, would hold the host on itself for good: the fix
# is on main and nothing left can fetch it. A loop that can act must owe
# nothing to what it repairs (CLAUDE.md §Diagnosis), so this file picks,
# and it is deliberately small enough to read whole:
#
#   current   $TREE/current — the newest generation root accepted.
#   good      $TREE/good — the newest generation whose OWN converge
#             script completed a tree refresh (it marks itself; see
#             forge-converge.sh). Proven able to bring in the next fix.
#   legacy    the checkout's copy, /home/david/boss — what this host ran
#             until this car. Offered ONLY while no generation has EVER
#             been marked good: the old way keeps working until the new
#             one has run once, and never after. `good` is a root-owned
#             name nothing removes, so once it exists this arm is dead:
#             THE CONVERGE never runs the checkout's copy again. That is
#             all it closes. Root's ops runner still executes every verb
#             from the checkout every minute, and the cluster converge
#             and the disk sweep still run there as its owner (review
#             5f3736a2, F3; no_unattended_unit_runs_from_a_home.rs is
#             the roster) — and a merged REVERT of the car puts the old
#             units, the checkout's converge among them, back on purpose.
#
# THE RULE. current when it is the good one. Otherwise current gets two
# tries; after two that did not mark it good, this tick runs the
# fallback (good, or legacy inside the bootstrap window) and current
# gets its next try on the tick after — so a current that failed for a
# passing reason heals by itself, and one that is truly broken costs
# every other tick until main carries its fix, which the fallback's own
# refresh then fetches.
#
# It says which it chose and why on one line, and execs it: the unit's
# exit is the converge's own.
#
# IT DOES NOT ASK WHICH MACHINE IT IS ON, AND WHAT IT STARTS DOES
# (backlog 62b09c57, N7). On 2026-10-07 this file, started in a fixture
# tree on the dev pod, chose the fixture generation's real converge and
# that converge installed the forge onto the pod. The check that refuses
# that is infra/lib/host-check.sh, and it is NOT sourced here on purpose:
# this is a copy outside every tree, so the library would have to be a
# second copy beside it, in the one script whose failure nothing can
# route around. Instead forge-converge.sh, from the commit that added
# this paragraph on, asks before its first write, as does everything it
# runs (a generation older than that does not, and is not something a
# fixture is built from); all this file writes is the tries count inside
# the tree it was pointed at.
# host_check_sh.rs starts it in the reviewer's shape and holds the exit
# at 78 with nothing run.
#
# ENV (seams; the host sets none): BOSS_ROOT_TREE,
# BOSS_FORGE_LEGACY_CONVERGE.
# Pinned by crates/core/boss-testing/tests/root_tree_sh.rs.
set -uo pipefail
export LC_ALL=C

ME="forge-converge-launch"
TREE="${BOSS_ROOT_TREE:-/var/lib/boss/tree}"
LEGACY="${BOSS_FORGE_LEGACY_CONVERGE:-/home/david/boss/infra/forge/forge-converge.sh}"
REL="infra/forge/forge-converge.sh"
TRIES="$TREE/.launch-tries"

# The generation directory a link names, resolved once: the script this
# tick runs must not change under it when `current` moves mid-run.
gen_of() { # <link name>
    local d
    [ -L "$TREE/$1" ] || return 1
    d="$(cd -- "$TREE/$1" 2>/dev/null && pwd -P)" || return 1
    [ -x "$d/$REL" ] || return 1
    printf '%s' "$d"
}

# A name that resolves to no runnable generation is "none", by name: the
# rule below reads the two flags, never an empty string that might be a
# read that failed.
no_cur="" no_good=""
cur="$(gen_of current)" || no_cur=1
good="$(gen_of good)" || no_good=1
[ -z "$no_cur" ] || cur=""
[ -z "$no_good" ] || good=""

run() { # <why> <script>
    echo "$ME: $1"
    exec "$2"
}

if [ -n "$cur" ] && [ "$cur" = "$good" ]; then
    run "running current (${cur##*/}) — it is the good generation" "$cur/$REL"
fi

fallback=""
fallback_why=""
if [ -n "$good" ]; then
    fallback="$good/$REL"
    fallback_why="the last good generation (${good##*/})"
elif [ ! -L "$TREE/good" ] && [ -x "$LEGACY" ]; then
    fallback="$LEGACY"
    fallback_why="the checkout's copy at $LEGACY — BOOTSTRAP: no generation of $TREE has ever been marked good"
fi

if [ -z "$cur" ]; then
    if [ -n "$fallback" ]; then
        run "$TREE/current names no runnable generation — running $fallback_why" "$fallback"
    fi
    echo "$ME: NOTHING TO RUN — $TREE/current names no runnable generation, there is no good one, and the bootstrap arm is closed or absent. By hand, as root: /home/david/boss/infra/forge/root-tree.sh refresh" >&2
    exit 1
fi

# How many tries current has had without becoming good. One line,
# "<generation> <n>"; anything else reads as none.
tried=0
if read -r t_gen t_n 2>/dev/null <"$TRIES"; then
    if [ "$t_gen" = "${cur##*/}" ] && [[ "$t_n" =~ ^[0-9]+$ ]]; then tried="$t_n"; fi
fi
if [ "$tried" -ge 2 ] && [ -n "$fallback" ]; then
    printf '%s 1\n' "${cur##*/}" >"$TRIES" 2>/dev/null || true
    run "current (${cur##*/}) has run $tried time(s) without marking itself good — this tick runs $fallback_why; current gets its next try on the tick after" "$fallback"
fi
printf '%s %s\n' "${cur##*/}" "$((tried + 1))" >"$TRIES" 2>/dev/null || true
run "running current (${cur##*/}), try $((tried + 1)) — not yet marked good" "$cur/$REL"
