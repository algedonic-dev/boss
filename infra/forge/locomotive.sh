#!/usr/bin/env bash
# The locomotive check — a seconds-long environment gate that every
# heavy CI job waits behind.
#
# Evidence, not caution: on 2026-08-12 forge train #1 went red five
# rounds, and every cause was environment — a missing interpreter, a
# stale runner-cached image, container-uid semantics, and a
# load-induced timing flake twice. Each cost a 15–25 minute test run
# plus a manual log excavation to attribute. Everything this script
# checks is one of those five, moved to the front of the train and
# given its remediation in the failure text.
#
# What it cannot cover: actions/checkout itself runs node inside the
# job container, so an image broken enough to lack node dies before
# any step runs — that failure class stays pre-locomotive.
#
# Checks collect rather than short-circuit: one run names every
# problem, not the first.
set -uo pipefail

fail=0
say() { printf '%s\n' "$*"; }

# 1. Toolchain — every binary the gate invokes, from the one manifest.
while IFS= read -r tool; do
  case "$tool" in ''|\#*) continue ;; esac
  if ! command -v "$tool" >/dev/null 2>&1; then
    say "LOCOMOTIVE RED: '$tool' missing from the runner image (infra/forge/boss-ci/required-tools.txt)."
    say "  remediation: infra/forge/boss-ci/build.sh on the forge host, then re-signal."
    fail=1
  fi
done < infra/forge/boss-ci/required-tools.txt

# 2. Image freshness — the stamp baked at build time must match the
# hash of the image definition in this checkout. A mismatch means the
# image validating this tree is not the image this tree describes:
# either the runner cached a stale tag, or this very change edits the
# CI image and the rebuild must land first. Both were tonight's reds;
# both now fail here, named, in seconds.
want="$(cat infra/forge/boss-ci/Dockerfile infra/forge/boss-ci/required-tools.txt | sha256sum | cut -d' ' -f1)"
have="$(cat /etc/boss-ci-stamp 2>/dev/null || echo absent)"
if [ "$want" != "$have" ]; then
  say "LOCOMOTIVE RED: runner image stamp ($have) != this tree's image definition ($want)."
  say "  remediation: infra/forge/boss-ci/build.sh on the forge host, then re-signal."
  fail=1
fi

# 3. Ownership — the invariant is ownership, not uid zero (forge
# train #1 round 3): the workspace must belong to the uid the gate
# runs as, whatever that uid is.
owner="$(stat -c %u . 2>/dev/null || echo '?')"
if [ "$owner" != "$(id -u)" ]; then
  say "LOCOMOTIVE RED: workspace owned by uid $owner but the gate runs as uid $(id -u)."
  fail=1
fi

# 4. Headroom — the gate needs disk, and running out of it does not
# look like running out of it.
#
# 2026-08-16: train 48 carried twelve cars and died on four
# boss-ledger tests panicking with `could not extend file
# "base/888983/895760": No space left on device`. Nothing in that
# sentence says "the runner is full", and nothing in the four failing
# test names says it either — Postgres simply fails first, because it
# is the component extending files continuously, so the disk surfaces
# as whichever crate happened to be running. Train 47 had produced the
# subtler version of the same thing an hour earlier (`expected to read
# 5 bytes, got 0 bytes at EOF`) and cost an hour to attribute to
# anything at all.
#
# The cause was a workspace volume the runner reuses across jobs whose
# `target/` had grown to 63GB unpruned. Freeing it took the host from
# 74G to 141G free. Nothing bounds that growth, so it will come back —
# which is exactly the case for checking rather than fixing once.
#
# THE THRESHOLD IS THE WHOLE DESIGN, and it took three measurements
# to get right. Recorded because the wrong two are plausible.
#
# A job's workspace volume is PER-JOB: the runner creates it, the job
# fills it, and it goes away with the container. Watched live on train
# 49 — the host sat at 141G free, fell to 67G as `cargo test
# --all-features` built, and returned to 141G with no volumes left
# behind. So every CI job builds this workspace from cold, and a job
# needs about 74GB to finish.
#
# That kills the two natural checks. A CEILING on the cache is
# meaningless: there is no persistent cache to bound, and a 74GB
# `target/` is simply what a completed run looks like. And a floor set
# to "enough for a full build" (~80GB) would red honestly-fine hosts,
# because it is a hair under the 141G this host has free and one
# orphan away from failing every run.
#
# What actually happened on 2026-08-16 was neither. A job CRASHED on
# 2026-08-14 (exit 255) and its container was never reaped, so its
# 63GB volume stayed. That left 74G free — less than the ~74GB the
# next full job needed — and Postgres, extending files continuously,
# was the first thing to fail. The disk was not leaking; one dead
# job's corpse was sitting on it.
#
# So the check is a floor, set where it means something: below it a
# full run CANNOT finish, so failing here costs seconds instead of
# twenty minutes and names the reason. It will not catch every
# too-tight case — a host at 78GB free passes and might still run out
# — and that is stated rather than papered over. The durable fix is
# reaping orphaned job containers, which is a runner-host concern and
# is filed as bcaf4a54, not something a preflight can do.
min_free_gb="${BOSS_CI_MIN_FREE_GB:-70}"

# The remediation carries the trap that cost an extra step during the
# live recovery: an orphaned job's volume is NAMED, so
# `docker volume prune` skips it and reclaims only the few GB of
# anonymous ones.
reclaim_advice() {
  say "  remediation: on the forge host, reap dead jobs holding volumes —"
  say "    sudo docker ps -a --filter status=exited   # dead FORGEJO-ACTIONS-* jobs"
  say "    sudo docker rm <them> && sudo docker volume prune"
  say "    # an orphan's volume is NAMED, so plain prune skips it:"
  say "    sudo docker volume ls && sudo docker volume rm <workspace-volume>"
}
# `df -P` for POSIX columns, and the workspace's own filesystem rather
# than / — in a container job those are frequently not the same one.
avail_kb="$(df -Pk . 2>/dev/null | awk 'NR==2 {print $4}')"
if [ -z "$avail_kb" ]; then
  say "LOCOMOTIVE RED: could not read free space for the workspace filesystem."
  fail=1
else
  avail_gb=$((avail_kb / 1024 / 1024))
  if [ "$avail_gb" -lt "$min_free_gb" ]; then
    say "LOCOMOTIVE RED: ${avail_gb}GB free on the workspace filesystem, need ${min_free_gb}GB."
    say "  This is what a full disk looks like BEFORE it becomes a Postgres error"
    say "  in an unrelated crate twenty minutes from now (packet 1b63456b)."
    reclaim_advice
    fail=1
  fi
fi


# 5. Telemetry, not a gate — rounds 4 and 5 were load-induced timing
# flakes; load can't be pre-checked away, but it can be on the record
# next to whatever it breaks. Free space rides along even when green,
# so the growth that caused 1b63456b is visible in the log of every
# run before it is a failure in one of them.
say "locomotive: nproc=$(nproc) loadavg=$(cut -d' ' -f1-3 /proc/loadavg) stamp=$have free=${avail_gb:-?}GB"

exit "$fail"
