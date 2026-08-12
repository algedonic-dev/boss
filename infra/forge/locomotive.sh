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

# 4. Telemetry, not a gate — rounds 4 and 5 were load-induced timing
# flakes; load can't be pre-checked away, but it can be on the record
# next to whatever it breaks.
say "locomotive: nproc=$(nproc) loadavg=$(cut -d' ' -f1-3 /proc/loadavg) stamp=$have"

exit "$fail"
