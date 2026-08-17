#!/usr/bin/env bash
# Every installed timer must leave a Job behind.
#
# WHY. boss-maintenance-wrap.sh states the contract: "the timer is the
# EXECUTOR, the Job is the VISIBILITY." ExecStartPre opens or reuses a
# Job, ExecStartPost completes it, and a FAILED run completes nothing —
# so the Job stays open on the fleet view until a later run succeeds or
# a human closes it.
#
# Three of eleven timers were wired that way. Eight ran nightly with no
# packet, no findings, no event-log trace and nobody's queue, which
# means a silent failure in any of them was indistinguishable from a
# success. deploy-services.sh's own comment records that four of these
# units were "authored but never installed" and each was caught by
# hand; this is the same class one step later — installed, running, and
# unobservable.
#
# David, 2026-08-16: "Let's make sure we have a job to handle each" and
# "get as much maintenance and management into job protocols rather
# than floating around scripts or system timers elsewhere."
#
# WHAT IT CHECKS, for every row of deploy-services.sh's TIMERS array:
#   1. the .service unit exists where the array says it does
#   2. it calls boss-maintenance-wrap.sh with a kind (opens the Job)
#   3. it calls boss-step.sh with the SAME kind (completes it)
#   4. that kind is a real Workflow in the platform bundle
#
# (3) and (4) are the ones worth having. A unit that opens a Job and
# never completes it leaves an open packet every run — worse than no
# packet, because the fleet view fills with false failures. And a kind
# that no Workflow defines makes the wrapper's spawn fail at 03:00,
# where nobody is reading.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

DEPLOY="infra/deploy-services.sh"
BUNDLE="infra/platform/workflows.toml"
for f in "$DEPLOY" "$BUNDLE"; do
    [ -f "$f" ] || { echo "timers-leave-a-packet: $f not found" >&2; exit 1; }
done

# The kinds a Workflow actually defines: the bundle, plus the three
# still baked into platform_workflows() in registry.rs. Both are read,
# because a kind in either place is a real protocol — and the tree is
# mid-migration from the second to the first.
kinds=$(
    { grep -oE '^kind = "maintenance-[a-z-]+"' "$BUNDLE" | sed -E 's/kind = "(.*)"/\1/'
      grep -oE '"maintenance-[a-z-]+"' crates/core/boss-jobs/src/registry.rs | tr -d '"'
    } | sort -u
)

rows=$(sed -n '/^TIMERS=(/,/^)/p' "$DEPLOY" | grep -oE '"[a-z0-9-]+:[^"]+"' | tr -d '"')
count=$(printf '%s\n' "$rows" | grep -c . || true)
if [ "$count" -lt 5 ]; then
    echo "timers-leave-a-packet: only parsed $count timer rows from $DEPLOY —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 1
fi

problems=0
for row in $rows; do
    name="${row%%:*}"; sub="${row##*:}"
    [ "$sub" = "." ] && unit="infra/$name.service" || unit="infra/$sub/$name.service"

    if [ ! -f "$unit" ]; then
        echo "timers-leave-a-packet: $name is installed by $DEPLOY but $unit does not exist" >&2
        problems=$((problems + 1)); continue
    fi

    open_kind=$(grep -oE 'boss-maintenance-wrap\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)
    done_kind=$(grep -oE 'boss-step\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)

    if [ -z "$open_kind" ]; then
        echo "timers-leave-a-packet: $name runs with no Job — add an ExecStartPre calling" >&2
        echo "    boss-maintenance-wrap.sh <kind> \"<label>\", and an ExecStartPost calling" >&2
        echo "    boss-step.sh <kind> run result=ok. A timer with no packet fails silently." >&2
        problems=$((problems + 1)); continue
    fi
    if [ -z "$done_kind" ]; then
        echo "timers-leave-a-packet: $name OPENS a Job ($open_kind) and never completes it." >&2
        echo "    Missing the ExecStartPost boss-step.sh call: every run would leave an open" >&2
        echo "    packet, so the fleet view fills with failures that did not happen." >&2
        problems=$((problems + 1)); continue
    fi
    if [ "$open_kind" != "$done_kind" ]; then
        echo "timers-leave-a-packet: $name opens '$open_kind' but completes '$done_kind'." >&2
        problems=$((problems + 1)); continue
    fi
    if ! printf '%s\n' "$kinds" | grep -qxF -- "$open_kind"; then
        echo "timers-leave-a-packet: $name uses kind '$open_kind', which no Workflow defines." >&2
        echo "    The wrapper's spawn would fail at run time, in the middle of the night." >&2
        problems=$((problems + 1))
    fi
done

if [ "$problems" -gt 0 ]; then
    echo "" >&2
    echo "  $problems timer(s) without working Job visibility." >&2
    exit 1
fi
echo "timers-leave-a-packet: $count timers, each opens and completes a defined Job"
