# Cluster builder image — the ONE image build-1 needs
# (docs/design/dev-cluster.md §Bring-up mechanics, item 2).
#
# Runs as the workload inside actions-runner-controller pods on the
# Talos cluster: Rust toolchain + sccache client, wired to the
# cluster's sccache Deployment via SCCACHE_ENDPOINT at deploy time.
# CI's rust job (clippy / test / fmt) runs here; the web job stays on
# hosted runners until a bun variant earns its keep (README).
#
# Base is the SAME digest-pinned toolchain image the OSS-quickstart
# build uses (infra/oss-quickstart/Dockerfile) — one toolchain truth.
# The two images are allowed to diverge deliberately (this one adds
# sccache and no cargo-chef; the quickstart bakes dependency layers
# instead), but a rustc version bump should land in both in one
# change.
#
# FIRST-CONTACT HONESTY: untested until the cluster exists — built
# and validated on build-1, not here. Budget the fix pass
# (dev-cluster.md says the same about every bring-up artifact).
FROM rust:1.96-slim-bookworm@sha256:c8a94a78f67ec8c4d474ec7f71e0720f21eb7e584e158daec0874cafa7c30e4d

# Same native deps as the quickstart's rust-base stage (sqlx +
# reqwest want pkg-config + libssl-dev), plus git for checkout and
# postgresql-client for the pg-backed test suites' psql probes.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    git \
    postgresql-client \
    && rm -rf /var/lib/apt/lists/*

# Built from source with --locked rather than a fetched release
# binary: slower once, but no hand-copied artifact hash to trust.
# The layer keys on the base + apt layers and persists until the
# toolchain bumps.
RUN cargo install sccache --version 0.8.2 --locked

# The wrapper is on for every cargo invocation in this image; the
# cache server address arrives from the pod spec (SCCACHE_ENDPOINT),
# and sccache degrades to local-disk cache when it is unset — a
# runner without the cache Deployment still builds, just cold.
ENV RUSTC_WRAPPER=/usr/local/cargo/bin/sccache
