#!/usr/bin/env bash
# docker-driver-report.sh — which storage driver, and so which image
# store, each of this host's two docker daemons runs. READ-ONLY: one
# `docker info -f` per daemon, no arguments, nothing mutated. It serves
# the forge (infra/ops/verbs/docker-driver-report.json).
#
# WHY (backlog 3f28860d, from the delta review of car cf59b3bf). The
# mirror read-back's correctness depends on whether the forge's
# ROOTLESS dockerd — david's, the one the converge builds in — uses the
# containerd image store, and no door could read it: disk-report prints
# `docker system df` and a du, ci-image-report reads only the system
# daemon, and journal-tail cannot reach a user unit. So the question was
# answered from belief. This prints `Driver` and `DriverStatus` for both
# daemons verbatim; a containerd image store reads as a snapshotter
# driver-type (`io.containerd.snapshotter.v1`) in DriverStatus, the
# classic store as overlay2 and its backing filesystem.
#
# THE TWO DAEMONS (disk-report.sh header): the system daemon the
# Forgejo Actions jobs run in, read as `sudo -n docker` with no
# DOCKER_HOST (an inherited one would aim it at the rootless socket);
# and the rootless daemon at unix:///run/user/1000/docker.sock.
#
# AN UNREACHED DAEMON IS NAMED, NEVER SKIPPED. A daemon that refuses,
# is down, or answers nothing gets a `NOT REACHED` line carrying
# docker's own first line of complaint, the closing `unreached:` line
# lists it, and the exit is 1 — no evidence is not a pass. Both daemons
# are always asked, so one dark daemon never hides the other's answer.
#
# The two env knobs name stand-in daemons for the pin
# (crates/core/boss-testing/tests/docker_driver_report_sh.rs); unset on
# the host.
set -uo pipefail

SYSTEM_DOCKER="${BOSS_DOCKER_REPORT_SYSTEM:-sudo -n docker}"
ROOTLESS_DOCKER="${BOSS_DOCKER_REPORT_ROOTLESS:-docker}"
ROOTLESS_SOCK="unix:///run/user/1000/docker.sock"
FORMAT='{{.Driver}} {{json .DriverStatus}}'

unreached=""

# read_driver <name> <where> <command words…>: one line per daemon.
read_driver() {
    local name="$1" where="$2" out rc
    shift 2
    out=$("$@" info -f "$FORMAT" 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ] && [ -n "$out" ]; then
        printf '%s: %s\n' "$name" "$out"
        return
    fi
    unreached="$unreached $name"
    [ -n "$out" ] || out="exit $rc, no output"
    printf '%s: NOT REACHED%s (%s)\n' "$name" "$where" "$(printf '%s\n' "$out" | sed -n '1p')"
}

# shellcheck disable=SC2086 # SYSTEM_DOCKER is two or three words by design
read_driver system "" env -u DOCKER_HOST $SYSTEM_DOCKER
read_driver rootless " at $ROOTLESS_SOCK" env DOCKER_HOST="$ROOTLESS_SOCK" "$ROOTLESS_DOCKER"

if [ -z "$unreached" ]; then
    echo "unreached: none"
    exit 0
fi
echo "unreached:$unreached"
exit 1
