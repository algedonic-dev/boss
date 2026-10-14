#!/usr/bin/env bash
# gate-runner/bootstrap.sh — run the run.sh of the sha being gated.
#
# WHY THIS EXISTS (backlog e3100bc8; design 1d7a1669, shape C). The gate
# pod's script reaches it as ConfigMap gate-runner-script, which the forge
# converge (infra/forge/cluster-deploy-runner.sh) rewrites from MAIN after
# every train. So a car that changed run.sh was gated by the OLD script:
# car 646c0ade's own gate 19b7253f, and every gate between its merge and
# the next converge, carried no `seed_order` — the key its run.sh adds. A
# run.sh change was proven only by pins that lift pieces of the script,
# never by a gate that ran it.
#
# This file is the fixed part. It lives in the ConfigMap beside main's
# run.sh, resolves the branch ONCE, fetches that sha's
# infra/gate-runner/run.sh, and execs it with GATE_SHA, so the script and
# the tree it gates are one sha (run.sh checks out GATE_SHA, not the head
# it finds). When it cannot — the branch is gone, the forge does not
# answer, the sha carries no run.sh — it execs main's copy beside it,
# NAMED as the fallback with its reason, and run.sh writes both onto the
# receipt. A silent fallback would be this defect again.
#
# WHAT IT DOES NOT CHANGE (the trust argument is the design's). The pod
# spec — image, mounts, securityContext, no service-account token — stays
# on main, and the branch's gate.sh and tests already run in this same
# container with the same token readable. It writes NOTHING under
# /gate-target, because run.sh's disk guard refuses before anything is
# written there: the fetch lands in the container's own writable layer and
# the credential rides `git -c`, not a ~/.gitconfig.
#
# Required env: GATE_BRANCH. Optional: FORGE_URL, GATE_BOOT_DIR.
# Hands run.sh: GATE_SHA, GATE_RUNNER_SOURCE (branch | main-fallback),
# GATE_RUNNER_SHA256, GATE_RUNNER_FALLBACK_REASON (fallback only).
set -uo pipefail

FORGE_URL="${FORGE_URL:-http://10.20.0.15:3000/david/boss.git}"
BOOT_DIR="${GATE_BOOT_DIR:-${TMPDIR:-/tmp}/gate-boot}"
# main's copy: the ConfigMap's run.sh key, mounted beside this file.
FALLBACK="$(dirname "${BASH_SOURCE[0]}")/run.sh"
RUNNER=infra/gate-runner/run.sh

# Same helper run.sh installs, read from the mounted FILE so the token is
# never in argv or a URL; passed per command, inherited by the lazy blob
# fetch through git's own config environment.
GIT=(git)
if [ -r /etc/forge/token ]; then
    GIT=(git -c 'credential.helper=!f() { echo username=x-access-token; echo "password=$(cat /etc/forge/token)"; }; f')
fi

fallback() { # <reason>
    echo "gate-bootstrap: FALLBACK to main's run.sh from the ConfigMap: $1" >&2
    unset GATE_SHA GATE_RUNNER_SHA256
    export GATE_RUNNER_SOURCE=main-fallback GATE_RUNNER_FALLBACK_REASON="$1"
    exec bash "$FALLBACK"
}

[ -n "${GATE_BRANCH:-}" ] || fallback "GATE_BRANCH is not set"
rm -rf "$BOOT_DIR" && mkdir -p "$BOOT_DIR" \
    || fallback "could not make $BOOT_DIR"
"${GIT[@]}" -C "$BOOT_DIR" init -q \
    && "${GIT[@]}" -C "$BOOT_DIR" remote add origin "$FORGE_URL" \
    || fallback "could not initialise $BOOT_DIR"
# ONE resolution: the sha is whatever this fetch brought, and nothing
# after it asks the forge where the branch is. Tree and commits only; the
# one blob is fetched lazily by the show below.
"${GIT[@]}" -C "$BOOT_DIR" fetch -q --depth 1 --filter=blob:none origin "refs/heads/$GATE_BRANCH" \
    || fallback "could not fetch refs/heads/$GATE_BRANCH from the forge"
GATE_SHA=$("${GIT[@]}" -C "$BOOT_DIR" rev-parse --verify 'FETCH_HEAD^{commit}') \
    || fallback "fetched refs/heads/$GATE_BRANCH but FETCH_HEAD is not a commit"
"${GIT[@]}" -C "$BOOT_DIR" show "$GATE_SHA:$RUNNER" > "$BOOT_DIR/run.sh" \
    || fallback "$GATE_BRANCH@$GATE_SHA carries no readable $RUNNER"
[ -s "$BOOT_DIR/run.sh" ] || fallback "$GATE_BRANCH@$GATE_SHA has an empty $RUNNER"
GATE_RUNNER_SHA256=$(sha256sum "$BOOT_DIR/run.sh" | cut -d' ' -f1)

echo "gate-bootstrap: running $RUNNER of $GATE_BRANCH@$GATE_SHA (sha256 $GATE_RUNNER_SHA256)"
export GATE_SHA GATE_RUNNER_SHA256 GATE_RUNNER_SOURCE=branch
unset GATE_RUNNER_FALLBACK_REASON
exec bash "$BOOT_DIR/run.sh"
