#!/usr/bin/env bash
# run-while-held.sh — run a candidate command LOCALLY only while dev
# isolation is held, judged when it RUNS (backlog 9a37d9d8, 2026-10-18).
#
# Usage: /bin/bash <main checkout>/infra/dev/run-while-held.sh <script> [args…]
#        (from a script that has cd'd into the worktree; the command runs
#        as `/bin/bash <script> [args…]` there, and its exit status is this
#        door's). The door itself is named by absolute path into the main
#        checkout (backlog 6d2e2653): a worktree's own copy is a branch's
#        edit of the door, or — cut before the door landed — no door.
#
# WHY A DOOR AND NOT A LINE. `boss brief` reads the isolation selector
# ONCE, when it renders, and while the selector is absent prints the
# pre-flight as a local command (5008a6c9). The builder runs that line
# minutes to an hour later, and a plain `/bin/bash infra/gate.sh --lint`
# is no door: nothing re-reads the selector, so an activation in between
# ran candidate lints and clippy (build scripts, proc macros) beside this
# container's credentials — exactly what activation exists to forbid
# (design a68fc42a D2) — and the run came back green. Isolation flipped
# ON and OFF more than once on 2026-10-18, so the window is not
# hypothetical. wt-cargo and wt-web already judge the selector when they
# run; this is the same reading for every other local candidate command.
#
# THE RULE. Absent selector: the held state, so the command runs here.
# ANY selector — a generation, a refusal, a file, a foreign link — and
# this door REFUSES with 75 rather than forwarding: the brief that printed
# the local line was rendered for the held state, and the activated route
# (the published controller, its bindings and put doors) is the one a
# fresh `boss brief` names. A refusal is a stop; never retry locally.
#
# AN ABSENCE THIS CALLER CANNOT PROVE IS NOT THE HELD STATE (backlog
# 1fddbe13). activate-control.py keeps the store root-owned 0700, so as
# uid 65534 (infra/dev/as-gate-uid.sh) the stat of the selector fails with
# EACCES and `-e` answers false whether or not isolation is on. The
# selector is absent only when the nearest part of its path this caller
# can see is a directory it may search; anything else refuses, as
# door_env::dev_child_route refuses every error but NotFound. A store that
# does not exist at all (a runner with no /work) is still the held state.
#
# The store is spelled once here, and infra/dev/control-tools_test.py
# holds it equal to the other readers of the selector (CLAUDE.md §9a).
dev_control=/work/tools/dev-control
seen=$dev_control
until [[ -e $seen || -L $seen ]]; do seen=${seen%/*}; seen=${seen:-/}; done
if [[ -e $dev_control/enabled || -L $dev_control/enabled ]]; then
    echo "run-while-held: REFUSED — dev isolation is on here ($dev_control/enabled exists), so this candidate command does not run beside this container's credentials (design a68fc42a D2). The brief that printed this local line was rendered while isolation was held: run boss brief <packet> again and use the route it names now; never retry the command locally. Why: $dev_control/activation.log." >&2
    exit 75
elif [[ ! -d $seen || ! -x $seen ]]; then
    echo "run-while-held: REFUSED — this caller (uid $EUID) cannot read the dev isolation store $dev_control ($seen is closed to it), so it cannot tell whether isolation is on, and this candidate command does not run beside this container's credentials (design a68fc42a D2). Run it as the store's owner, or through the route a fresh boss brief <packet> names; never retry it locally." >&2
    exit 75
fi
if [[ $# -eq 0 ]]; then
    echo "usage: /bin/bash <main checkout>/infra/dev/run-while-held.sh <script> [args…]" >&2
    exit 2
fi
# AND THIS FILE'S OWN AGE (backlog 0b36dd65). Every executable here
# consults the one helper. A local pre-flight is a read, so a stale copy
# WARNS and runs, as wt-cargo does — refusing would strand a builder for
# a copy whose only job is the selector check above.
here=$(dirname "$(readlink -f "$0")")
if [ -r "$here/door-freshness.sh" ]; then
    # shellcheck source=door-freshness.sh
    . "$here/door-freshness.sh"
    if stale=$(door_is_stale "$0"); then
        echo "run-while-held: WARNING — $stale" >&2
    fi
fi
exec /bin/bash "$@"
