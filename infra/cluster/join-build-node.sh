#!/usr/bin/env bash
# join-build-node.sh — turn a LAN Linux box into a BOSS build node.
#
#   BOSS_RUNNER_TOKEN=<gh registration token> ./join-build-node.sh
#
# Idempotent: each phase checks before it installs, and re-running is
# safe. Where a HUMAN credential is required (Tailscale login, the
# runner registration token from GitHub → Settings → Actions →
# Runners), the script stops loudly with the exact next command
# instead of half-completing — a node that silently joined without a
# runner would look done and be useless.
#
# FIRST-CONTACT HONESTY: this script has not touched real hardware
# yet. Every VM-validated install script in this repo needed a
# first-contact fix pass (the OSS quickstart took eight); expect this
# one to need its own. See docs/design/dev-cluster.md.
#
# Phases:
#   1. mesh     — verify Tailscale is up (instructs, never auto-auths)
#   2. rust     — rustup + stable toolchain for the runner user
#   3. sccache  — shared compile cache (server on THIS node if first)
#   4. runner   — GitHub Actions self-hosted runner, repo-scoped
set -euo pipefail

REPO_SLUG="${BOSS_GH_REPO:-algedonic-dev/boss}"
RUNNER_DIR="${BOSS_RUNNER_DIR:-$HOME/actions-runner}"
RUNNER_VERSION="${BOSS_RUNNER_VERSION:-2.319.1}"
SCCACHE_VERSION="${BOSS_SCCACHE_VERSION:-0.8.2}"
SCCACHE_PORT="${BOSS_SCCACHE_PORT:-10600}"

say()  { printf '==> %s\n' "$*"; }
die()  { printf 'join-build-node: %s\n' "$*" >&2; exit 1; }

# --- 1. mesh ---------------------------------------------------------------
if command -v tailscale >/dev/null 2>&1; then
    if tailscale status >/dev/null 2>&1; then
        say "mesh: tailscale up ($(tailscale ip -4 2>/dev/null | head -1))"
    else
        die "tailscale installed but not authenticated — run: sudo tailscale up   (then re-run this script)"
    fi
else
    die "tailscale not installed — install per https://tailscale.com/download, \
'sudo tailscale up', then re-run. (Q2 in docs/design/dev-cluster.md may swap \
this for bare WireGuard; the check changes, the shape doesn't.)"
fi

# --- 2. rust ---------------------------------------------------------------
if command -v cargo >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo" ]; then
    say "rust: toolchain present"
else
    say "rust: installing stable via rustup (non-interactive)"
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
export PATH="$HOME/.cargo/bin:$PATH"

# --- 3. sccache ------------------------------------------------------------
if ! command -v sccache >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/sccache" ]; then
    say "sccache: installing v$SCCACHE_VERSION"
    arch="$(uname -m)"
    url="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${arch}-unknown-linux-musl.tar.gz"
    tmp="$(mktemp -d)"
    curl -fsSL "$url" | tar -xz -C "$tmp" --strip-components=1
    install -m 0755 "$tmp/sccache" "$HOME/.cargo/bin/sccache"
    rm -rf "$tmp"
else
    say "sccache: present"
fi
# The first node hosts the cache; later nodes point at it via
# SCCACHE_ENDPOINT in the runner env. Local disk cache either way —
# a dedicated cache server is a later, measured decision.
say "sccache: build jobs should run with RUSTC_WRAPPER=sccache (runner env below)"

# --- 4. runner -------------------------------------------------------------
if [ -f "$RUNNER_DIR/.runner" ]; then
    say "runner: already configured in $RUNNER_DIR"
else
    [ -n "${BOSS_RUNNER_TOKEN:-}" ] || die "runner: BOSS_RUNNER_TOKEN not set — mint one at \
https://github.com/${REPO_SLUG}/settings/actions/runners/new and re-run with it in the env"
    say "runner: installing v$RUNNER_VERSION into $RUNNER_DIR"
    mkdir -p "$RUNNER_DIR"
    arch="$(uname -m)"; case "$arch" in x86_64) rarch=x64 ;; aarch64) rarch=arm64 ;; *) die "unsupported arch $arch" ;; esac
    curl -fsSL "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-${rarch}-${RUNNER_VERSION}.tar.gz" \
        | tar -xz -C "$RUNNER_DIR"
    (cd "$RUNNER_DIR" && ./config.sh --unattended \
        --url "https://github.com/${REPO_SLUG}" \
        --token "$BOSS_RUNNER_TOKEN" \
        --name "boss-build-$(hostname -s)" \
        --labels "boss-cluster,linux,$(uname -m)" \
        --replace)
    # Env the runner's jobs inherit: the shared compile cache.
    {
        echo "RUSTC_WRAPPER=sccache"
        echo "SCCACHE_DIR=$HOME/.cache/sccache"
    } >> "$RUNNER_DIR/.env"
    say "runner: installing as a service (needs sudo once)"
    (cd "$RUNNER_DIR" && sudo ./svc.sh install "$(whoami)" && sudo ./svc.sh start)
fi

say "node joined. Verify: the runner shows Idle at https://github.com/${REPO_SLUG}/settings/actions/runners"
say "then route a workflow at it with:  runs-on: [self-hosted, boss-cluster]"
