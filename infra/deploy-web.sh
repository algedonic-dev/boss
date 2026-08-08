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
# Find bun. `$HOME/.bun/bin` for the invoking user is the common case
# and was the only case this handled — which fails on any box where the
# toolchain was installed under a different account than the one running
# the deploy. That is this playground: bun lives in dauld's home, the
# deploy runs as david, and the build died with "bun: command not found"
# while every other part of the script worked. The web bundle then
# silently stayed at whatever version was last built by hand, which is
# how a CSS fix committed at 23:42 was still not in the browser at 04:05
# and got reported as a bug twice.
#
# So: look in the obvious places and SAY which ones were tried.
find_bun() {
    local candidates=()
    command -v bun >/dev/null 2>&1 && { command -v bun; return 0; }
    [[ -n "${BOSS_BUN:-}" ]] && candidates+=("${BOSS_BUN}")
    [[ -n "${SUDO_USER:-}" ]] && candidates+=("$(getent passwd "${SUDO_USER}" | cut -d: -f6)/.bun/bin/bun")
    candidates+=("${HOME}/.bun/bin/bun" "/usr/local/bin/bun")
    # Any user's bun install. Last resort, but a deploy that cannot find
    # the toolchain is worse than one that looks a little harder.
    local h
    while IFS=: read -r _ _ _ _ _ h _; do
        [[ -x "$h/.bun/bin/bun" ]] && candidates+=("$h/.bun/bin/bun")
    done < <(getent passwd)
    local c
    for c in "${candidates[@]}"; do
        [[ -x "$c" ]] && { echo "$c"; return 0; }
    done
    {
        echo "bun not found. Tried:"
        printf '  %s\n' "${candidates[@]}"
        echo "Set BOSS_BUN=/path/to/bun, or install bun for the user running this."
    } >&2
    return 1
}

BUN="$(find_bun)" || exit 1
echo "==> building web bundle (bun: ${BUN})"

# Build as a user who can write node_modules + dist, not as root.
BUILD_AS="${SUDO_USER:-$(id -un)}"
if [[ "${BUILD_AS}" != "$(id -un)" ]]; then
    sudo -u "${BUILD_AS}" env "PATH=$(dirname "${BUN}"):${PATH}" \
        bash -c "cd '${WEB_DIR}' && bun run build"
else
    (cd "${WEB_DIR}" && PATH="$(dirname "${BUN}"):${PATH}" bun run build)
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
