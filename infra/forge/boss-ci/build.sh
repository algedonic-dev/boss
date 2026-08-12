#!/usr/bin/env bash
# Build + push the boss-ci runner image, stamped with its own
# provenance: the sha256 of (Dockerfile + required-tools.txt) at build
# time. The locomotive CI job recomputes that hash from its checkout
# and refuses to run when they disagree — so "the runner cached an old
# image" (forge train #1 round 2: a registry retag does not refresh a
# runner's local tag) stops being a 25-minute mystery and becomes a
# named red with this script as the remediation.
#
# Run on the forge host (rootful docker, which the runner shares):
#   infra/forge/boss-ci/build.sh
set -euo pipefail
cd "$(dirname "$0")"

REGISTRY="${BOSS_CI_REGISTRY:-10.20.0.15:3000/david}"
TAG="${BOSS_CI_TAG:-rust1.96}"

stamp="$(cat Dockerfile required-tools.txt | sha256sum | cut -d' ' -f1)"

docker build --build-arg BOSS_CI_STAMP="$stamp" \
  -t "$REGISTRY/boss-ci:$TAG" .
docker push "$REGISTRY/boss-ci:$TAG"

echo "boss-ci:$TAG pushed, stamp $stamp"
