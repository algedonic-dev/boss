#!/usr/bin/env bash
#
# feedback-queue — read the triage board from a terminal.
#
# The reader lives in `boss queue`
# (crates/orchestrators/boss-cli/src/queue.rs — its module doc carries
# this script's full history: why it is read-only, why columns are
# derived from the fork field and not a step kind, why it talks to
# jobs-api with an actor header instead of the gateway). Directive
# 26d61c97 retired the python heredoc that used to live here; the file
# stays for operator muscle memory.
#
#   ./infra/feedback-queue.sh            # all columns
#   ./infra/feedback-queue.sh with-agent # just the agent queue
set -euo pipefail
exec boss queue "$@"
