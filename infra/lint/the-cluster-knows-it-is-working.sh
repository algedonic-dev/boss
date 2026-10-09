#!/usr/bin/env bash
# the-cluster-knows-it-is-working.sh — the forge watchdog's decision
# table, exercised as a pure function, plus the unit's one rule: it
# carries no maintenance wrap.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/cluster-watchdog-lib.sh"; unit="$here/../forge/cluster-watchdog.service"
[[ -f "$lib" && -f "$unit" ]] || { echo "the-cluster-knows-it-is-working: missing $lib or $unit" >&2; exit 1; }
# shellcheck source=/dev/null
. "$lib"
fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { local got; got=$(watchdog_decision "$1" "$2" "$3" "$4" 3 "${6:-}"); [[ "$got" == "$5" ]] || fail "live=$1 image=$2 stamp=$3 dark=$4 blind='${6:-}' -> $got, expected $5"; }
expect up    2de17dd 2683908 0 ok              # the API answers: nothing to do
expect down  2de17dd 2683908 1 wait            # one dark check is a deploy roll, not an outage
expect down  2de17dd 2683908 2 wait
expect down  2de17dd 2683908 3 roll-to-stamp   # past the threshold, serving a non-converged image: the named lever
expect down  b2814ef 2683908 5 roll-to-stamp   # the placeholder image too
expect down  2683908 2683908 3 hands           # dark on the last converged build itself: nothing safe to roll to
expect down  2de17dd none    3 hands           # no stamp yet: nothing to roll to
# Blind (the read of deploy/boss failed; the 6th fact is why). Dark past
# the threshold it cannot tell what is served, and a patch through the
# credential that could not read cannot land: hands, never a rollback it
# would report as "did not go Ready" every tick (backlog cf321ffd).
expect down  ""      2683908 3 hands  "bind source path does not exist"
expect down  2de17dd 2683908 3 hands  "context deadline exceeded"
expect down  ""      2683908 2 wait   "bind source path does not exist"   # blind, not yet past the threshold
expect up    ""      2683908 5 ok     "bind source path does not exist"   # the API answers: blind is the run's failure, not an outage
# Its packet is visibility, never a precondition: both hooks must be
# prefixed `-` so systemd runs the watchdog whatever the API says.
grep -qE '^ExecStartPre=-/var/lib/boss/tree/current/infra/boss-maintenance-wrap.sh maintenance-cluster-watchdog' "$unit" || fail "the watchdog's packet hook is missing or is not best-effort (needs the - prefix)"
grep -qE '^ExecStopPost=-/var/lib/boss/tree/current/infra/boss-step.sh maintenance-cluster-watchdog' "$unit" || fail "the watchdog's completion hook is missing or is not best-effort (needs the - prefix)"
grep -qE '^ExecStart(Pre|Post)?=/var/lib/boss/tree/current/infra/(boss-maintenance-wrap|boss-step)' "$unit" && fail "a packet hook on the watchdog is a hard precondition — it would deadlock on the API it watches"
grep -q '^ExecStart=/var/lib/boss/tree/current/infra/forge/cluster-watchdog.sh' "$unit" || fail "the unit does not run the watchdog"
echo "the-cluster-knows-it-is-working: self-test ok — ok/wait/roll-to-stamp/hands decided as designed (11 cases, 4 of them blind); its packet hooks are best-effort, never a precondition"
exit 0
