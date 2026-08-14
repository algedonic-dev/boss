# CI runner image for the BOSS gate (internal-forge Q2's recorded
# debt: this Dockerfile lived off-repo on the forge host; now the
# image and the workflow that runs in it share one tree).
#
# Forgejo Actions runs JavaScript actions (actions/checkout) inside
# the job container, so node must be present or every job dies at
# checkout with exitcode 127. python3 used to be here for one reason
# only: boss-testing's conductor_preflight tests drove
# infra/train/conductor.py directly, and their absence-of-python
# failure mode was a panic that read as "3 tests failed" while the
# real story was "the container can't run what the suite runs"
# (forge train #1's red signal, 2026-08-12). Directive 26d61c97 ported
# the conductor to `boss train` and the tests now drive the compiled
# boss binary, so the interpreter — only ever there for the conductor
# tests — is out. Carrying rustfmt/clippy and the postgres client
# means a CI run does no toolchain installation at all.
#
# Build + push (forge host):
#   docker build -t 10.20.0.15:3000/david/boss-ci:rust1.96 infra/forge/boss-ci
#   docker push 10.20.0.15:3000/david/boss-ci:rust1.96
# bun is the workspace's package manager and script runner (root
# bun.lock, `bun run typecheck` in apps/web). The gate runs
# svelte-check, and without bun in the image there is no way to install
# the workspace's node_modules or invoke the script that drives it — so
# the web app's types went unchecked by CI entirely while every Rust
# lint was enforced. Copied from the official image rather than curl'd
# into a shell: a pinned tag is reproducible and reviewable, and the
# installer script wants unzip and writes to $HOME, neither of which
# this image should acquire for one binary.
FROM oven/bun:1.3-slim AS bun

FROM rust:1.96-slim-bookworm

RUN apt-get update -qq \
 && apt-get install -y --no-install-recommends \
      nodejs git ca-certificates postgresql-client pkg-config libssl-dev curl \
 && rm -rf /var/lib/apt/lists/*

COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun

RUN rustup component add rustfmt clippy

# Provenance stamp: build.sh injects the sha256 of (Dockerfile +
# required-tools.txt); the locomotive CI job recomputes the same hash
# from its checkout and goes red on a mismatch. That is the equality
# test (CLAUDE.md 9a) between the image a runner cached and the image
# this tree describes — build with build.sh, not bare docker build.
ARG BOSS_CI_STAMP=unstamped
RUN echo "$BOSS_CI_STAMP" > /etc/boss-ci-stamp
