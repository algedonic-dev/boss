#!/usr/bin/env bash
# svelte-check over apps/web — the type gate for the frontend.
#
# WHY THIS EXISTS. Until 2026-08-13 infra/gate.sh ran fifteen Rust
# checks and zero frontend ones, so every .svelte and .ts file in
# apps/web reached main with its types unchecked by CI. The trigger was
# a web-only car (the KB page) whose only type verification was an
# svelte-check I happened to run by hand; nothing in the pipeline would
# have caught a mistake. A 53k-line frontend with no type gate is not a
# smaller risk than a workspace with fifteen.
#
# WHAT IT RUNS. `bun run typecheck` in apps/web, which is
# `svelte-check --tsconfig ./tsconfig.json`. Exits 0 on warnings and
# non-zero on errors — verified both directions in the CI image before
# this was wired in: the clean tree reports "0 errors and 63 warnings"
# and exits 0, and a planted `const n: number = "not a number"` reports
# "1 error" and exits 1. A check that cannot go red is decoration.
#
# The 63 warnings are deliberately not fatal. Failing on them today
# would mean fixing 63 things before the gate can be turned on at all,
# which is how a check ends up permanently disabled. Errors are the
# ratchet; the warning count is a number to bring down separately.
#
# PUPPETEER_SKIP_DOWNLOAD. boss-web depends on puppeteer, whose
# postinstall downloads a browser. In the CI container that download
# fails, and because bun aborts the whole install on a failed
# postinstall, NOTHING gets installed — svelte-check included, which
# then fails with "command not found" and looks like a missing tool
# rather than a failed install. Skipping the download is not a
# workaround for a broken dependency; a type check has no use for a
# browser binary.
#
# bun must exist. It is listed in infra/forge/boss-ci/required-tools.txt
# so the locomotive check names its absence in seconds. Locally, this
# refuses rather than skipping: a check that silently passes when its
# tool is missing reports success while verifying nothing, which is the
# defect class this repo has spent the day removing.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

if ! command -v bun >/dev/null 2>&1; then
    echo "svelte-check: bun not found — install it (https://bun.sh) or run the gate in the CI image." >&2
    echo "              Refusing to skip: a check that passes without running verifies nothing." >&2
    exit 1
fi

# --frozen-lockfile so CI cannot silently resolve a different tree than
# the lockfile records.
if ! PUPPETEER_SKIP_DOWNLOAD=1 bun install --frozen-lockfile >/tmp/boss-bun-install.log 2>&1; then
    echo "svelte-check: bun install failed" >&2
    tail -20 /tmp/boss-bun-install.log >&2
    exit 1
fi

cd apps/web || exit 1
bun run typecheck
