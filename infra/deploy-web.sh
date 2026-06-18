#!/usr/bin/env bash
# Build + install the Boss web dashboard to the live static dir.
#
# The gateway (`boss-gateway`) serves the SPA from
# `/var/lib/boss-web/dist/` (see `BOSS_STATIC_DIR` in
# `crates/core/boss-gateway/src/static_files.rs`). Frontend changes don't
# land in the browser until we run `bun run build` and rsync the
# output into that directory — this script wraps both steps.
#
#   sudo ./infra/deploy-web.sh
#
# Idempotent — safe to re-run. No service restart required: the
# browser picks up the new chunk filenames (from index.html) on next
# load. A hard-refresh may still be needed if the index.html itself
# is in the browser cache.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WEB_DIR="${REPO_ROOT}/apps/web"
DIST_SRC="${WEB_DIR}/dist"
DIST_DST="${BOSS_STATIC_DIR:-/var/lib/boss-web/dist}"

if [[ ! -d "${WEB_DIR}" ]]; then
    echo "web source not found: ${WEB_DIR}" >&2
    exit 1
fi

# Build as the invoking user — bun needs write to node_modules + the
# source dist dir, and those are owned by the dev user, not root. If
# the script is run with sudo, de-escalate for the build. `sudo -u`
# doesn't source `.bashrc` (only login shells do, and bash -lc
# sources `.bash_profile`/`.profile`), so we add bun's default
# install path manually — covers the common case where bun ships to
# ~/.bun/bin and is put on PATH by the user's `.bashrc`.
if [[ -n "${SUDO_USER:-}" ]]; then
    echo "==> building web bundle (as ${SUDO_USER})"
    sudo -u "${SUDO_USER}" bash -lc \
        "export PATH=\"\$HOME/.bun/bin:\$PATH\"; cd '${WEB_DIR}' && bun run build"
else
    if ! command -v bun >/dev/null 2>&1; then
        echo "bun not on PATH; install it or run from a shell where it is" >&2
        exit 1
    fi
    echo "==> building web bundle"
    (cd "${WEB_DIR}" && bun run build)
fi

if [[ ! -f "${DIST_SRC}/index.html" ]]; then
    echo "build output missing: ${DIST_SRC}/index.html" >&2
    exit 1
fi

# rsync --delete so chunks renamed by the bundler don't pile up in
# the static dir. Keeps the dir size-stable across deploys.
echo "==> installing to ${DIST_DST}"
mkdir -p "${DIST_DST}"
rsync -a --delete "${DIST_SRC}/" "${DIST_DST}/"

# Step-plugin bundles. Authored as static JS in `infra/step-plugins/`
# (no build step — each plugin is hand-authored vanilla DOM). Gateway
# serves them at `/plugins/*` from `/var/lib/boss/step-plugins` (see
# `crates/core/boss-gateway/src/plugin_files.rs`). Without this copy
# the gateway returns 404 for every plugin URL the step_plugins
# registry references — which renders broken step surfaces in the
# SPA's Job Detail view.
PLUGIN_SRC="${REPO_ROOT}/infra/step-plugins"
PLUGIN_DST="${BOSS_STEP_PLUGIN_DIR:-/var/lib/boss/step-plugins}"
if [[ -d "${PLUGIN_SRC}" ]]; then
    echo "==> installing step-plugin bundles to ${PLUGIN_DST}"
    mkdir -p "${PLUGIN_DST}"
    # Only copy the JS bundles — the README in that dir is author
    # docs, not a runtime asset.
    rsync -a --delete --include='*.js' --exclude='*' \
        "${PLUGIN_SRC}/" "${PLUGIN_DST}/"
fi

echo
echo "==> deploy complete"
ls -la "${DIST_DST}/" | head -10
