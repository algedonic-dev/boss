#!/usr/bin/env bash
# Fixed bootstrap only. No branch code runs while forge-read is mounted.
set -euo pipefail
umask 077
export GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
export HOME=/tmp/clone-home
mkdir -p "$HOME" /work/repo
git init -q /work/repo
cd /work/repo
helper='!f() { echo username=x-access-token; echo "password=$(cat /etc/forge/token)"; }; f'
# Process-local helper, never written to HOME or the shared .git/config.
git -c credential.helper= -c "credential.helper=$helper" -c core.hooksPath=/dev/null \
    -c protocol.file.allow=never -c http.followRedirects=false \
    fetch --no-tags "$1" "$2:refs/consist/assembled" "$4:refs/consist/main"
# FETCH_HEAD names the first fetched ref, not the baseline (35119b27).
# Bind both observations before a checkout can expose candidate files.
test "$(git rev-parse 'refs/consist/assembled^{commit}')" = "$3"
test "$(git rev-parse 'refs/consist/main^{commit}')" = "$5"
git update-ref refs/remotes/origin/main refs/consist/main
git -c core.hooksPath=/dev/null checkout -q --detach "$3"
git config remote.origin.url "$1"
