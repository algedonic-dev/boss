#!/usr/bin/env bash
# 4196a7b1: native envelope/observer controls run before a first park,
# not only in the full gate after an author has already claimed proof.
# Pure local fixtures; this check opens no reader door or native event.
#
# THE TRUNK HALF (backlog 6665aa97; review 30397e74 of car 4f1a96ab).
# The append-only check below judges what THIS CHANGE altered, so it is
# trunk-relative, and it ran its own `git diff origin/main...HEAD`: the
# one such lint that never sourced lib/trunk-ref.sh, so it ignored
# BOSS_TRUNK_REF, compared against the GitHub mirror's stale origin on a
# dev box, and on the public mirror judged every forge car since the
# last publish as one change. It now resolves the trunk the way the
# other seven do and honours BOSS_TRUNK=none; the self-tests read the
# tree alone, so they run first and the declaration excuses only the
# comparison. Held to the tree by
# a_declared_no_trunk_skips_every_trunk_relative_lint.rs, which derives
# the trunk-relative set from merge-base use.
#
# Exit: 0 clean / 1 a contract modified or deleted, or no trunk ref /
#       3 git could not answer (lib/git-answer.sh) / a self-test's own.
set -uo pipefail
LINT=native-proof-evidence-is-adversarially-checked
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$LINT_DIR/../.." && pwd)"
cd "$repo" || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3

python3 -I -B "$repo/infra/forge/native-evidence_test.py" "$repo" || exit $?
python3 -I -B "$repo/infra/forge/proof-observation_test.py" "$repo" || exit $?

# A stored observer names its contract version. Add a new row rather
# than silently changing or deleting what an in-flight caller names.
# A ratchet that cannot see the trunk certifies nothing, so an ABSENT
# trunk refuses (1), as it did when the diff itself failed.
lint_no_trunk "$LINT" "which stored proof-observer contracts this change modified or deleted" && exit 0
BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates))" >&2
    echo "  Cannot tell which proof-observer contracts this change altered without one. Fetch the trunk." >&2
    exit 1
fi
mb=$(resolve_merge_base "$LINT" "$BASE" HEAD) || exit $?
changed=$(git_answer "$LINT" 0 diff --no-renames --name-only --diff-filter=MD "$mb" HEAD -- infra/platform/proof-observers/) || exit $?
if [[ -n "$changed" ]]; then
    printf 'native proof contracts are append-only; publish a new version:\n%s\n' "$changed" >&2
    exit 1
fi
