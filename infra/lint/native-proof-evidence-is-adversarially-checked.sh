#!/usr/bin/env bash
# 4196a7b1: native envelope/observer controls run before a first park,
# not only in the full gate after an author has already claimed proof.
# Pure local fixtures; this check opens no reader door or native event.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# A stored observer names its contract version. Add a new row rather
# than silently changing or deleting what an in-flight caller names.
changed="$(git -C "$repo" diff --name-only --diff-filter=MD origin/main...HEAD -- infra/platform/proof-observers/)"
if [[ -n "$changed" ]]; then
    printf 'native proof contracts are append-only; publish a new version:\n%s\n' "$changed" >&2
    exit 1
fi
python3 -I -B "$repo/infra/forge/native-evidence_test.py" "$repo"
python3 -I -B "$repo/infra/forge/proof-observation_test.py" "$repo"
