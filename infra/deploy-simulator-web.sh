#!/usr/bin/env bash
# Build + install the Simulator UX bundle to its live static dir.
#
# The boss-simulator service serves the SPA from
# `/var/lib/boss-simulator/dist/` (BOSS_SIM_STATIC_DIR) under /simulator;
# the gateway reverse-proxies /simulator/* there. Frontend changes don't
# land until we `bun run build` apps/simulator and rsync the output into
# that dir — this script wraps both. Mirrors infra/deploy-web.sh (minus
# the step-plugin copy, which the cockpit doesn't use).
#
#   sudo ./infra/deploy-simulator-web.sh
#
# Idempotent. No service restart required — boss-simulator serves the
# dist from disk per request (a hard refresh may be needed if the
# browser cached index.html).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WEB_DIR="${REPO_ROOT}/apps/simulator"
DIST_SRC="${WEB_DIR}/dist"
DIST_DST="${BOSS_SIM_STATIC_DIR:-/var/lib/boss-simulator/dist}"

if [[ ! -d "${WEB_DIR}" ]]; then
    echo "simulator web source not found: ${WEB_DIR}" >&2
    exit 1
fi

# Build as the invoking user (bun needs write to node_modules + dist,
# owned by the dev user). De-escalate when run under sudo; add bun's
# default install path since `sudo -u` doesn't source the user's rc.
if [[ -n "${SUDO_USER:-}" ]]; then
    echo "==> building simulator bundle (as ${SUDO_USER})"
    sudo -u "${SUDO_USER}" bash -lc \
        "export PATH=\"\$HOME/.bun/bin:\$PATH\"; cd '${WEB_DIR}' && bun run build"
else
    if ! command -v bun >/dev/null 2>&1; then
        echo "bun not on PATH; install it or run from a shell where it is" >&2
        exit 1
    fi
    echo "==> building simulator bundle"
    (cd "${WEB_DIR}" && bun run build)
fi

if [[ ! -f "${DIST_SRC}/index.html" ]]; then
    echo "build output missing: ${DIST_SRC}/index.html" >&2
    exit 1
fi

echo "==> installing to ${DIST_DST}"
mkdir -p "${DIST_DST}"
rsync -a --delete "${DIST_SRC}/" "${DIST_DST}/"

echo
echo "==> deploy complete"
ls -la "${DIST_DST}/" | head -10
