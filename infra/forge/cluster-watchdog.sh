#!/usr/bin/env bash
# cluster-watchdog — the loop that knows whether the cluster is working,
# from outside it, and rolls it to the last converged build when it is
# not. Every 5 minutes (cluster-watchdog.timer), no maintenance wrap on
# purpose: a watchdog that must open a packet through the API it
# watches is the 2026-09-05 deadlock again. Its record is its journal
# (readable over the journal gateway with the API dark) and, when the
# API answers, the estate observation series through the normal path.
#
# Env: JOBS_API (the system of record) and the registry host from
#      /etc/boss/sor.env through forge-defaults.sh (REGISTRY overrides
#      the image repo), BOSS_OPS_DIR (the admin kubeconfig's directory,
#      ops-credentials.sh), BOSS_FORGE_LAST_BUILT (the converge's stamp
#      file), WATCHDOG_STATE (dark-count file), WATCHDOG_BLIND_STATE
#      (set while it cannot read the cluster), WATCHDOG_DARK_LIMIT
#      (checks), BOSS_RUN_SUMMARY_FILE (what the run leaves its packet).
set -uo pipefail
. "$(dirname "$0")/cluster-watchdog-lib.sh"
. "$(dirname "$0")/forge-defaults.sh"
. "$(dirname "$0")/alert-lib.sh"
. "$(dirname "$0")/../estate/ops-credentials.sh"
. "$(dirname "$0")/../run-summary.sh"
sor_require JOBS_API
forge_need REGISTRY
run_summary_reset
STATE="${WATCHDOG_STATE:-$HOME/.boss-watchdog-dark}"
BLIND="${WATCHDOG_BLIND_STATE:-$HOME/.boss-watchdog-blind}"
LIMIT="${WATCHDOG_DARK_LIMIT:-3}"
# The admin kubeconfig David places and the converge checks — never a
# second copy (backlog fb444bbb). Read through sudo docker --mount only;
# see ops-credentials.sh for why there is no existence test here.
KC="$(ops_kubeconfig)"
K="$(ops_kubectl)"

live_commit=$(curl -s --max-time 8 "$JOBS_API/api/jobs/health" | sed -n 's/.*"commit" *: *"\([0-9a-f]\{7,\}\)".*/\1/p' | sed -n 1p | cut -c1-7)
if [ -n "$live_commit" ]; then live=up; else live=down; fi

# CAN IT ACT? The API answering says nothing about whether this loop
# could roll the cluster if it had to: that needs the credential, and
# until 2026-09-28 a failed read here was discarded (2>/dev/null) and the
# run closed "ok". A watchdog that cannot read the cluster has lost its
# lever, so it says so on its packet every tick (cluster_read, and a
# failed run), and files ONE urgent alert when it goes blind rather than
# one every five minutes.
read_err=$(mktemp) || exit 1
trap 'rm -f "$read_err"' EXIT
blind=""
# The kubectl image first: present (a bounded pull, so a stalled
# registry is a named blind reason rather than a systemd kill) and
# pinned against the disk sweep's prune (review of 8f50d314, M1/L3).
# `ops_image` rides on the packet every tick; a pin it could not make
# fails the run below, because the next prune would take the lever.
image_ready=""
unpinned=""
if image_ready=$(ops_image_ready 2>"$read_err") \
    && image=$(ops_boss_image 2>"$read_err") && [ -n "$image" ]; then
    image="${image##*:}"
    run_summary_field cluster_read "ok: deploy/boss read through $KC"
    if [ -e "$BLIND" ]; then
        echo "cluster readable again through $KC — the watchdog can act (blind since $(cat "$BLIND" 2>/dev/null))"
        rm -f "$BLIND"
    fi
else
    image=""
    # Never empty on a failed read: a read that failed without a word
    # (or answered no image) is blind all the same, and an empty reason
    # used to let the run close ok (backlog cf321ffd).
    blind="$(tr '\n' ' ' < "$read_err" | cut -c1-600)"
    blind="${blind:-no output from kubectl (exit status or an empty image)}"
    run_summary_field cluster_read "REFUSED: $KC — $blind"
    echo "cluster READ REFUSED through $KC (root material, placed by David; design 835c0c9c) — this watchdog cannot roll anything until it reads again: $blind" >&2
    if [ ! -e "$BLIND" ]; then
        alert "watchdog blind: it cannot read the cluster through $KC" "The forge watchdog could not read deploy/boss through $KC, so it cannot roll the cluster back if the API goes dark. Docker/kubectl said: $blind. Check that the admin kubeconfig David places is present and current there (the forge converge's ops_credentials field). If the kubectl image is what is missing and the forge registry cannot serve it: $(ops_image_fallback)"
        date -u +%Y-%m-%dT%H:%M:%SZ > "$BLIND"
    fi
fi
run_summary_field ops_image "${image_ready:-not ready — see cluster_read}"
case "$image_ready" in
    *'NOT pinned'*)
        unpinned="$image_ready"
        echo "watchdog: the kubectl image is not pinned — the disk sweep's next prune can take it, and a pull then needs the forge registry: $image_ready" >&2 ;;
esac
# THE STAMP IS ANOTHER ACCOUNT'S FILE, READ AS DATA (backlog a604a35b).
# This loop runs as root; the stamp is written by the cluster converge,
# which runs as the checkout's owner. So the path is one that account
# can repoint — at the machine token, at the admin kubeconfig — and
# whatever is read here goes into a kubectl patch, this journal and an
# alert's body. A build name is seven hex characters (the converge's
# ${HEAD_FULL:0:7}); anything else is `none`, which rolls nothing, and
# is said WITHOUT the bytes that were found. Bounded, so a name swapped
# for something that never ends cannot hold the tick.
# A read that FAILED (no file, a file this cannot open, the bound) has
# its own name and is never taken for a stamp: `none`, whatever came out.
#
# AND THE NAME IS NOT OPENED UNLESS IT IS A FILE OF ITS OWN (review
# 5f3736a2, F6). Checking what came out is not enough: a symlink made
# root OPEN whatever it pointed at — a device node of that account's
# choosing — and a symlink to a file holding a well-formed build name
# passed the shape check and was rolled to; a FIFO cost each tick the
# read bound. A regular file, not a symlink, or nothing is read — the
# check install.sh's one-time carry already made. Between this check and
# the read the name can still be swapped; what that wins is the shape
# check and the bound below, as before.
stamp_unread=""
stamp=""
if [ -L "$STAMP_FILE" ] || { [ -e "$STAMP_FILE" ] && [ ! -f "$STAMP_FILE" ]; }; then
    stamp_unread=1
    echo "watchdog: $STAMP_FILE is not a regular file of its own (a symlink, a pipe, a directory) — not opened, read as none, so there is nothing to roll to" >&2
    run_summary_field stamp "UNUSABLE: $STAMP_FILE is not a regular file of its own; not opened, nothing to roll to"
else
    stamp=$(timeout 5 head -c 64 -- "$STAMP_FILE" 2>/dev/null | awk 'NR == 1') || stamp_unread=1
fi
if [ -n "$stamp_unread" ] || ! [[ $stamp =~ ^[0-9a-f]{7}$ ]]; then
    if [ -f "$STAMP_FILE" ] && [ ! -L "$STAMP_FILE" ]; then
        echo "watchdog: $STAMP_FILE does not hold a build name (seven hex characters) — read as none, so there is nothing to roll to; what it holds is not printed" >&2
        run_summary_field stamp "UNUSABLE: $STAMP_FILE does not hold a build name; nothing to roll to"
    fi
    stamp=none
fi
# THE DARK COUNT: absent is zero — the first dark tick; unreadable or
# not a number is SAID, on the packet and in the journal, and fails the
# run (backlog f280dd01 item 5). It used to read as zero, which restarts
# the count and delays a rollback in silence. Counting on from this tick
# delays a rollback by up to $LIMIT checks — treating an unreadable
# count as past the threshold would roll on one tick of a deploy's
# ordinary dark — and holds ONLY if the write-back below works. When
# it does not (a full root volume, a STATE that is a directory), every
# tick reads the same unusable count and the watchdog never reaches the
# threshold, so the write is checked and that failure is named for
# what it is (review of 8f50d314, L4). The count stays on disk (the
# unit's StateDirectory) rather than tmpfs: checking the write is the
# smaller change and covers every way it can fail, not only a full disk.
dark=0
state_unreadable=""
if [ "$live" = "down" ]; then
    prev=0
    if [ -e "$STATE" ]; then
        if ! prev=$(cat "$STATE" 2>&1); then
            state_unreadable="$prev"
            prev=0
        elif ! [[ $prev =~ ^[0-9]+$ ]]; then
            state_unreadable="it holds '$(printf '%s' "$prev" | tr '\n' ' ' | cut -c1-80)', not a count"
            prev=0
        fi
    fi
    dark=$((prev + 1))
fi
if [ -n "$state_unreadable" ]; then
    run_summary_field dark_count "UNREADABLE: $STATE — $state_unreadable; counted from this tick"
    echo "watchdog: could not read the dark count in $STATE ($state_unreadable) — counting from this tick, so a rollback waits $LIMIT more checks" >&2
fi
state_unwritable=""
if ! write_err=$( { printf '%s\n' "$dark" > "$STATE"; } 2>&1 ); then
    state_unwritable="${write_err:-the write failed}"
    run_summary_field dark_count "UNWRITABLE: $STATE — $(printf '%s' "$state_unwritable" | tr '\n' ' '); this watchdog cannot count dark ticks and will NEVER roll"
    echo "watchdog: CANNOT COUNT, WILL NEVER ROLL — the dark count could not be written to $STATE ($state_unwritable), so every tick counts from zero and the threshold of $LIMIT is never reached" >&2
fi

decision=$(watchdog_decision "$live" "$image" "$stamp" "$dark" "$LIMIT" "$blind")
case "$decision" in
    ok)
        echo "cluster ok: api answers on $live_commit, deployment serves $image, last converged $stamp"
        alert_replay || true
        ;;
    wait)
        echo "cluster DARK ($dark of $LIMIT checks): api silent, deployment serves $image, last converged $stamp — waiting one more check before acting" >&2
        ;;
    roll-to-stamp)
        echo "cluster DARK past $LIMIT checks on $image — rolling deploy/boss to the last converged build $REGISTRY:$stamp by name" >&2
        # --request-timeout: a patch a half-dead API server never answers
        # ends here, as this branch's own "did not go Ready" alert, not
        # as a systemd kill that files nothing (backlog cf321ffd).
        if $K patch deploy boss -n boss --request-timeout=30s --type=json \
            -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/image\",\"value\":\"$REGISTRY:$stamp\"},{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$REGISTRY:$stamp\"}]" \
            && $K rollout status deploy/boss -n boss --timeout=420s; then
            echo "cluster RESTORED on the last converged build $stamp — the head that was serving ($image) needs a fix on main" >&2
            echo 0 > "$STATE"
            alert "cluster restored by the watchdog: rolled to the last converged build $stamp" "The API was dark for $dark checks while the deployment served $image; the watchdog rolled deploy/boss to $REGISTRY:$stamp by name and it went Ready. The head $image needs a fix on main before the converge rolls it again."
        else
            echo "cluster STILL DARK after rolling to $stamp — hands needed" >&2
            alert "cluster DARK: hands needed — the rollback to $stamp did not restore it" "The API was dark for $dark checks; the deployment served $image; rolling to $REGISTRY:$stamp did not go Ready. Read the forge journal for cluster-deploy-runner and cluster-watchdog."
            exit 1
        fi
        ;;
    hands)
        if [ -n "$blind" ]; then
            echo "cluster DARK past $LIMIT checks and this watchdog cannot read the cluster through $KC — it cannot tell what is served or roll anything; hands needed: $blind" >&2
            alert "cluster DARK: hands needed — the watchdog cannot read the cluster to roll it" "The API has been dark for $dark checks and the forge watchdog cannot read deploy/boss through $KC, so it cannot tell what the cluster serves or roll it to the last converged build ($stamp); no rollback was attempted. Docker/kubectl said: $blind. Read the forge journal for cluster-watchdog; rollback-to $stamp by hand needs the same credential to read. If the kubectl image is what is missing and the forge registry cannot serve it, on the forge: $(ops_image_fallback)"
            exit 1
        fi
        echo "cluster DARK past $LIMIT checks on the last converged build itself ($image, stamp $stamp) — nothing this loop can roll to; hands needed" >&2
        alert "cluster DARK: hands needed — the last converged build itself is dark" "The API has been dark for $dark checks while the deployment serves $image, which is the last converged build ($stamp); nothing safe to roll to. Read the forge journal for cluster-deploy-runner and cluster-watchdog."
        exit 1
        ;;
esac
# Blind is a failed run whatever the API said: "ok" would claim a
# safety net that is not there. So is a dark count it could not read
# or write, and a kubectl image the next prune can take.
[ -z "$blind" ] || exit 1
[ -z "$state_unreadable" ] || exit 1
[ -z "$state_unwritable" ] || exit 1
[ -z "$unpinned" ] || exit 1
