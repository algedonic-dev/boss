# OSS quickstart — single image carrying every BOSS service binary
# + the built SPA. Ships from `docker compose up` in
# `infra/oss-quickstart/docker-compose.yml`.
#
# Multi-stage:
#   1. rust-build  — compile the workspace in release mode.
#   2. spa-build   — bun install + bun run build for apps/web.
#   3. runtime     — debian:slim with the binaries + SPA copied in.
#
# Image size: ~250 MB (binaries are stripped via `-C strip=symbols`
# in the workspace Cargo.toml; SPA dist is gzipped by Caddy).
#
# Build context: the repo root. Compose mounts this Dockerfile via
# `build.context: ../..` so the entire workspace is available.

# ---------------------------------------------------------------------------
# Stage 1 — Rust build.
# ---------------------------------------------------------------------------
FROM rust:1.95-slim-bookworm@sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082 AS rust-build

# sqlx + reqwest need pkg-config + libssl-dev for the rustls-native
# bridge; the rest of the workspace is pure Rust.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the workspace. .dockerignore excludes target/ and node_modules/
# so only source goes into the build context.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY examples ./examples
COPY infra/postgres ./infra/postgres

# Build every binary the runtime needs. --features postgres pulls in
# the sqlx-backed adapters; the workspace builds in one cargo
# invocation so dep compilation is shared across crates.
#
# Cache mounts (BuildKit) keep the cargo registry + target dir warm
# across rebuilds. Without BuildKit this still works, just slower.
RUN --mount=type=cache,target=/build/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    set -eu && \
    # Single cargo invocation — sim-daemon feature is enabled via
    # the cross-package syntax so boss-brewery-sim builds in the
    # same pass as the workspace, sharing dep compilation.
    # The prior two-cargo-build pattern invalidated cache between
    # passes (different feature set) and recompiled boss-brewery-
    # engine + dep tree twice; collapsing it halves the cold-build
    # wall-clock on cache-cold builds.
    cargo build --release --workspace \
        --features postgres \
        --features boss-brewery-engine/sim-daemon \
        --bins && \
    # boss-accounts-api + boss-events-api gate on umbrella features
    # (accounts-api / events-api), not `postgres`, so the workspace
    # build above silently skips them. Build explicitly — else the
    # gateway 502s on /api/accounts and the brewery data-seed can't
    # land accounts/vendors.
    cargo build --release -p boss-accounts --bin boss-accounts-api --features accounts-api && \
    cargo build --release -p boss-events --bin boss-events-api --features events-api && \
    mkdir -p /out && \
    # Copy every workspace binary out of the cache mount before it's
    # discarded. Strict mode (set -e) means the build fails if the
    # cargo step didn't produce binaries — silent failure used to
    # mask "image came up with no bins inside" bugs.
    find target/release -maxdepth 1 -type f \
        \( -name 'boss' -o -name 'boss-*' \) ! -name '*.d' \
        -exec cp {} /out/ \; && \
    test -x /out/boss-gateway || (echo "FATAL: boss-gateway missing in /out" >&2; ls /out >&2; exit 1) && \
    test -x /out/boss-brewery-sim || (echo "FATAL: boss-brewery-sim missing in /out" >&2; ls /out >&2; exit 1)

# ---------------------------------------------------------------------------
# Stage 2 — SPA build.
# ---------------------------------------------------------------------------
FROM oven/bun:1.3-debian@sha256:9dba1a1b43ce28c9d7931bfc4eb00feb63b0114720a0277a8f939ae4dfc9db6f AS spa-build

# Skip puppeteer's Chrome download in this build stage — puppeteer
# is a devDependency for browser-based smoke tests, not needed by
# the SPA build itself. Without this, `bun install` calls
# puppeteer's postinstall script which tries to extract Chrome via
# `unzip` and fails because the base image doesn't include it.
ENV PUPPETEER_SKIP_DOWNLOAD=true

WORKDIR /spa
COPY apps/web/package.json apps/web/bun.lock* ./
RUN bun install --frozen-lockfile

COPY apps/web/. ./

# `apps/web/src/build.ts` shells out to `boss-ports-list` for the
# canonical service-port table (single-source-of-truth shared with
# the Rust binaries). The binary comes from stage 1; copy it onto
# PATH so the SPA build resolves it. Bare-metal `quickstart.sh`
# gets this for free because the Rust workspace builds first into
# target/release/, which the SPA's build.ts probes via
# `find_boss_bin`; in docker the stages are isolated so we have to
# stage the binary explicitly.
COPY --from=rust-build /out/boss-ports-list /usr/local/bin/boss-ports-list
RUN bun run build

# ---------------------------------------------------------------------------
# Stage 3 — runtime.
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim@sha256:96e378d7e6531ac9a15ad505478fcc2e69f371b10f5cdf87857c4b8188404716 AS runtime

# Runtime deps: ca-certs for outbound HTTPS (the gateway), libssl
# for rustls's native bridge, postgresql-client for the init
# script's psql calls, tini as PID 1 so signal forwarding works.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    postgresql-client \
    tini \
    bash \
    wget \
    curl \
    && rm -rf /var/lib/apt/lists/*

# BOSS runs as a non-root user. UID 1500 matches the systemd-deploy
# convention so volume mounts behave the same in compose + bare-metal.
RUN groupadd --gid 1500 boss && \
    useradd --uid 1500 --gid boss --create-home --shell /bin/bash boss

# Pre-create the auth-volume mount point with boss ownership.
# Without this, docker volume init creates /var/lib/boss/auth as
# root:root and `boss-auth set` (run as the boss user from init.sh)
# silently fails to write credentials.toml. Same pattern for the
# sim-state dir (/var/lib/boss-sim) the brewery sim checkpoints its
# counterparty queue into — without it the sim (UID 1500) logs
# "could not create sim-state dir; Permission denied" and the
# collections queue can't survive a container restart.
RUN mkdir -p /var/lib/boss/auth /var/lib/boss-sim /etc/boss-gateway && \
    chown -R 1500:1500 /var/lib/boss /var/lib/boss-sim /etc/boss-gateway

# Binaries.
COPY --from=rust-build /out/ /usr/local/bin/

# SPA static files — Caddy serves these directly. A separate Caddy
# container in compose mounts /opt/boss/spa as a read-only volume.
COPY --from=spa-build /spa/dist /opt/boss/spa

# Brewery seeds + reference data — the `init.sh` first-run path
# applies these against the freshly-bootstrapped DB.
COPY infra/postgres /opt/boss/infra/postgres
# operator-baseline hires (emp-audit + the injected bootstrap-admin)
# MUST be available — seed-operator-baseline.sh reads this dir to POST
# them to /api/people once the stack is up.
COPY infra/operator-baseline /opt/boss/infra/operator-baseline
# Dispatcher rule registry — boss-dispatcher's side-effect runner reads
# BOSS_DISPATCHER_RULES (set in docker-compose.yml) which points at
# /opt/boss/infra/dispatcher/rules.toml. Without this dir the rules runner
# can't start and Jobs get assigned + worked but never close.
COPY infra/dispatcher /opt/boss/infra/dispatcher
COPY examples /opt/boss/examples

# Post-API seed scripts boss-launch (services-launcher) runs once the
# API stack is up: the platform operator-baseline, then the brewery
# tenant (classes/JobKinds/policy/data via `boss-brewery-sim prepare`).
# boss-init runs PRE-API and can't seed through the API, so these live
# here and run from boss-services. Both are invoked by absolute path
# (/opt/boss/infra/...), so they need the execute bit.
COPY infra/seed-operator-baseline.sh /opt/boss/infra/seed-operator-baseline.sh
COPY infra/seed-brewery-tenant.sh /opt/boss/infra/seed-brewery-tenant.sh

# Quickstart launcher + init scripts.
COPY infra/oss-quickstart/init.sh /usr/local/bin/boss-init
COPY infra/oss-quickstart/services-launcher.sh /usr/local/bin/boss-launch
COPY infra/oss-quickstart/generate-configs.sh /usr/local/bin/boss-generate-configs
RUN chmod +x /usr/local/bin/boss-init /usr/local/bin/boss-launch \
    /usr/local/bin/boss-generate-configs \
    /opt/boss/infra/seed-operator-baseline.sh \
    /opt/boss/infra/seed-brewery-tenant.sh

# /etc must be writable by the boss user so generate-configs can
# write the per-service TOML files at container start. Default
# debian permissions on /etc are root:root 0755, which the boss
# user can't write to. Open it up; the container is ephemeral.
RUN chmod 0777 /etc

USER boss
WORKDIR /home/boss

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/boss-launch"]
