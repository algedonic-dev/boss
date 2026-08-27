#!/usr/bin/env bash
#
# reap-dead-ci-jobs — remove the corpses of crashed CI jobs, and the
# disk they are sitting on.
#
# WHAT THIS IS FOR, measured. A Forgejo Actions job gets a container
# and a workspace volume; on a normal finish the runner reclaims both,
# and the host returns to where it started (watched live on train 49:
# 141G free, 67G during the build, 141G after, no volumes remaining).
# A job that CRASHES does not get that treatment. On 2026-08-14 one
# exited 255 and its container was never reaped, so its 63GB volume
# stayed — permanently.
#
# Two days later that orphan was still there, the host had 74G free
# instead of 141G, and the next full job needed about 74GB. It ran
# out. Postgres failed first, because it is the component extending
# files continuously, so the visible symptom was four boss-ledger
# tests dying on `could not extend file ...: No space left on device`
# and a twelve-car train going red. The train before it had died the
# same way with a subtler message. Neither said anything about disk
# (packet 1b63456b).
#
# So this is not a cache pruner and must not become one. There is no
# persistent cache to bound — a 74GB `target/` is what a healthy
# completed run looks like, and a scheduled prune of live volumes
# would just make every job slower. The only thing being reclaimed
# here is state belonging to jobs that are already dead.
#
# THE TRAP that cost an extra step during the live recovery: a job's
# workspace volume is NAMED, so `docker volume prune` skips it and
# reclaims only the few GB of anonymous ones. Removing it takes an
# explicit `docker volume rm` after its container is gone.
#
# SAFETY. Three properties, in order of how much they would cost to
# get wrong:
#   1. Only containers whose name matches the runner's own
#      FORGEJO-ACTIONS-* pattern. The forgejo server itself runs in a
#      container on this host and must never be a candidate.
#   2. Only containers that have EXITED. A running job is untouched
#      whatever its age.
#   3. Only after a grace period (default 6h). A job that finished
#      ten minutes ago may still be having its artifacts read, and the
#      failure this guards against took two days to bite — there is no
#      reason to race.
# Volumes are removed only after their container is gone AND only when
# docker reports no container referencing them.
#
# Usage: reap-dead-ci-jobs.sh [--dry-run] [--grace-hours N]
# Exit:  0 always on a clean run (nothing to reap is the healthy case)

set -uo pipefail

DRY=0
GRACE_HOURS="${BOSS_REAP_GRACE_HOURS:-6}"
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        --grace-hours) shift; GRACE_HOURS="${1:?--grace-hours needs a number}"; shift ;;
        *) echo "reap-dead-ci-jobs: unknown arg: $1" >&2; exit 2 ;;
    esac
done

say() { printf '%s\n' "$*"; }
run() { if [ "$DRY" -eq 1 ]; then say "  DRY: would $*"; else "$@" >/dev/null 2>&1; fi; }

# WHICH DAEMON. This host runs BOTH a system docker and a rootless one
# for the invoking user, and the runner's containers live in the
# system daemon. The first draft of this script tried to be clever —
# use plain `docker`, fall back to `sudo docker` if `docker ps` fails
# — and that is silently wrong: `docker ps` SUCCEEDS against the
# rootless daemon, which has none of the containers. It reported "0
# dead jobs" against a host with a planted one, cheerfully, and would
# have run daily for months doing nothing.
#
# So there is no detection. The unit runs as root, where plain
# `docker` is the system daemon; anything else must say so explicitly.
DOCKER="${BOSS_REAP_DOCKER:-docker}"

free_before="$(df -Pk / | awk 'NR==2 {print $4}')"

# ---------------------------------------------------------------------
# Candidates
# ---------------------------------------------------------------------
# `docker ps` filters do status and name; AGE is not among them, so the
# grace period is applied by reading each candidate's FinishedAt. Doing
# it per-container rather than parsing the human "Exited (255) 2 days
# ago" string keeps this correct in a locale that renders it otherwise.
cutoff_epoch=$(( $(date -u +%s) - GRACE_HOURS * 3600 ))
reaped=0
skipped_young=0

while IFS= read -r cid; do
    [ -n "$cid" ] || continue
    name="$($DOCKER inspect -f '{{.Name}}' "$cid" 2>/dev/null | sed 's|^/||')"
    finished="$($DOCKER inspect -f '{{.State.FinishedAt}}' "$cid" 2>/dev/null)"
    # An unparseable timestamp means "do not touch it" — the whole
    # point of the grace period is that we are never in a hurry.
    fin_epoch="$(date -u -d "$finished" +%s 2>/dev/null || echo '')"
    if [ -z "$fin_epoch" ]; then
        say "  skipping ${name} — cannot read FinishedAt (${finished})"
        continue
    fi
    if [ "$fin_epoch" -gt "$cutoff_epoch" ]; then
        skipped_young=$((skipped_young + 1))
        continue
    fi

    # The volumes this container holds, captured BEFORE it is removed —
    # afterwards there is nothing left to ask.
    vols="$($DOCKER inspect -f '{{range .Mounts}}{{if eq .Type "volume"}}{{.Name}} {{end}}{{end}}' "$cid" 2>/dev/null)"
    say "  reaping ${name} (exited ${finished})"
    run $DOCKER rm "$cid"
    reaped=$((reaped + 1))

    for v in $vols; do
        [ -n "$v" ] || continue
        # Belt and braces: never remove a volume something still holds.
        # In a dry run the container we just "removed" is of course
        # still there, so it is discounted explicitly — otherwise the
        # dry run reports "keeping" for every volume it would actually
        # remove, which is the opposite of what a dry run is for.
        refs="$($DOCKER ps -a --filter volume="$v" --format '{{.ID}}' 2>/dev/null \
                | grep -v "^${cid}" | grep -c . | tr -d ' ')"
        if [ "$refs" != "0" ]; then
            say "    keeping volume ${v} — still referenced by ${refs} other container(s)"
            continue
        fi
        say "    removing volume ${v}"
        run $DOCKER volume rm "$v"
    done
done < <($DOCKER ps -a --filter status=exited --filter name=FORGEJO-ACTIONS --format '{{.ID}}' 2>/dev/null)

free_after="$(df -Pk / | awk 'NR==2 {print $4}')"
reclaimed_gb=$(( (free_after - free_before) / 1024 / 1024 ))

# Say something on every run, including the quiet one. A reaper that
# only speaks when it acts is one nobody can tell is still installed —
# and the failure it prevents took two days to appear.
say "reap-dead-ci-jobs: ${reaped} dead job(s) reaped, ${skipped_young} inside the ${GRACE_HOURS}h grace, ${reclaimed_gb}GB reclaimed, $(( free_after / 1024 / 1024 ))GB free"
exit 0
