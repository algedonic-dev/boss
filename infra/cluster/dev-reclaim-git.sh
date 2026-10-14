#!/usr/bin/env bash
# Foreground observer only: the original Git arguments and streams stay intact.
set -uo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/dev-reclaim-git.py" "$@"
