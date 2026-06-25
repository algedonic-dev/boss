# BOSS dev toolchain. Base image ships Rust stable (rustup + rustfmt +
# clippy), git, curl, and the non-root `vscode` user the container runs
# as. We add the workspace's system build deps + the postgres client +
# Bun (the web toolchain).
# Pinned by manifest-list digest (multi-arch) for supply-chain integrity;
# tag kept in the comment so Dependabot/humans can see what it resolves to.
# Refresh: docker buildx imagetools inspect mcr.microsoft.com/devcontainers/rust:1-bookworm
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm@sha256:7d2b4e7912ea2913ce82f29299b0934b716234d0fe49e5edf5f05f7d36ef2d80

# - cmake + clang: ring / aws-lc-sys (rustls) build their C/asm bits
# - pkg-config + libssl-dev: native crates that probe for OpenSSL
# - postgresql-client: psql, used by apply-schema.sh + the CLIs
# - jq: the infra bring-up scripts parse JSON with it
# (build-essential / gcc are already in the base image.)
RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        cmake clang pkg-config libssl-dev postgresql-client jq \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# Bun, installed for the vscode user the dev container runs as, and put
# on PATH for both interactive and non-interactive (postCreate) shells.
USER vscode
RUN curl -fsSL https://bun.sh/install | bash
ENV BUN_INSTALL=/home/vscode/.bun
ENV PATH=/home/vscode/.bun/bin:${PATH}
USER root
