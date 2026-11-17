# trunk-ref.sh — which ref a baseline-comparing lint compares against,
# resolved once, in one place.
#
# WHY THIS EXISTS (§9a: a fact that lives four times). The same
# `resolve_base` walk was copy-pasted into FOUR lints —
# migrations-append-only, steptype-bundle-ratchet,
# a-new-style-has-a-caller, a-kind-bundle-does-not-tighten — each
# carrying the same `2>&1`-suppressed `git rev-parse`, and so each
# carrying the same defect: a git that could not answer read as "the ref
# is not here". Two of the four then told the operator to fetch a trunk
# that was already present; the other two skipped and exited 0, which a
# gate records as a pass. One walk, one fix.
#
# ORDER MATTERS, and it is the comment migrations-append-only carried:
# the forge is the trunk every car actually merges into; `origin` on a
# dev box is the GitHub mirror, a periodic backup that was 24 commits
# stale the day this was written, and comparing against it reports every
# already-merged change as a fresh modification. In CI the checkout's
# `origin` IS the forge, so the first hit there is correct.
# BOSS_TRUNK_REF overrides for the case where neither name applies.
#
# WHAT IT DOES NOT DECIDE. Whether a genuinely ABSENT trunk is a refusal
# or a skip stays with each lint, because the lints disagree on purpose:
# a ratchet that cannot see the trunk certifies nothing and refuses
# (exit 1), while a "what did this branch add" check with no baseline
# would flag every pre-existing line as this branch's fault and so skips
# (exit 0). Both are defensible. Neither is a thing to say when git did
# not answer at all.

#
# NO PER-CAR TRUNK (backlog 2d28df78, design e2710fbc). Every lint that
# sources this file judges "what did THIS CHANGE add or alter, against
# the merge-base with the trunk". On the forge that is one car against
# the main it merges into. On the public mirror (.github/workflows/
# ci.yml, infra/gate.sh full) the only trunk is the PREVIOUS PUBLISH, so
# "this change" is every forge car since then, judged as one: publish
# d2e189f5 (PR 258, 2026-11-16) had factory-vocabulary class 711 files
# as added against the 2026-10-22 publish and red on 364 lines in 27
# files that every forge car and train gate had passed — a rule that
# landed partway through a window reds every publish spanning it. The
# mirror has no car, so it DECLARES so, `BOSS_TRUNK=none`, the way it
# declares `BOSS_ESTATE=none` (lib/no-estate.sh): a run-environment
# fact is declared, never inferred from a ref name or a window size.
#
#   unset or empty  nothing changes: the lint resolves the trunk below.
#   none            on a GitHub-hosted runner on github.com and nowhere
#                   else (lib/public-mirror.sh, the guard shared with
#                   BOSS_ESTATE): `lint_no_trunk` prints the marker below
#                   naming the comparison not made, and the lint passes
#                   that judgement without making it. The gate reads the
#                   marker off the lint's stderr and names the lint under
#                   `trunk` on the receipt. The forge's per-car receipts
#                   are where each car WAS judged.
#   anything else   REFUSED, exit 3: a misspelt declaration must read as
#                   neither answer.
#   none + BOSS_TRUNK_REF   REFUSED, exit 3: a run cannot both name a
#                   trunk and declare it has none.
#
# Not a replay of each forge train and not a grandfathering rule: both
# rebuild or add a judgement the forge already made or never made. Pinned
# by a_declared_no_trunk_skips_every_trunk_relative_lint.rs, which also
# holds the lints that source this file equal to those that call
# `lint_no_trunk`.

# shellcheck source=infra/lint/lib/git-answer.sh
. "$(dirname "${BASH_SOURCE[0]}")/git-answer.sh" || exit 3
# shellcheck source=infra/lint/lib/public-mirror.sh
. "$(dirname "${BASH_SOURCE[0]}")/public-mirror.sh" || exit 3

# The words a lint prints when it honoured BOSS_TRUNK=none. A constant
# because two things read it: an operator, and gate.sh, which names every
# lint that printed it on the receipt (CLAUDE.md §9a).
LINT_NO_TRUNK_MARKER="TRUNK NOT JUDGED"
export LINT_NO_TRUNK_MARKER

# lint_no_trunk <lint> <what the trunk comparison would have judged>
#
#   return 1  no declaration: resolve the trunk, as always
#   return 0  BOSS_TRUNK=none on the public mirror: the marker is on
#             stderr and a ::warning annotation on stdout; the caller
#             makes no trunk comparison and passes it
#   exits     $LINT_CANNOT_ANSWER for any other value, for `none` beside
#             BOSS_TRUNK_REF, and for `none` off the public mirror
#
# Call it immediately before `resolve_trunk_ref`, after whatever the lint
# reads of the tree alone, so the declaration excuses only the judgement
# that needs a trunk.
lint_no_trunk() {
    local lint="$1" what="$2"
    case "${BOSS_TRUNK:-}" in
        "") return 1 ;;
        none)
            if [ -n "${BOSS_TRUNK_REF:-}" ]; then
                {
                    printf '%s: %s — BOSS_TRUNK=none declares there is no per-car trunk, and BOSS_TRUNK_REF names one (%s).\n' \
                        "$lint" "$LINT_CANNOT_ANSWER_MARKER" "$BOSS_TRUNK_REF"
                    printf '  A contradiction is refused rather than resolved either way: unset one of them\n'
                    printf '  (infra/lint/lib/trunk-ref.sh).\n'
                } >&2
                exit "$LINT_CANNOT_ANSWER"
            fi
            refuse_off_the_public_mirror "$lint" "BOSS_TRUNK=none"
            {
                printf '%s: %s — BOSS_TRUNK=none declares this run has no per-car trunk, so %s was not judged.\n' \
                    "$lint" "$LINT_NO_TRUNK_MARKER" "$what"
                printf '  NOTHING IS CLAIMED about what any one car added: each was judged against its own\n'
                printf '  base on the forge, and the gate receipt names this lint under `trunk`.\n'
            } >&2
            # The same fact where a reader of the GitHub checks page sees
            # it: a workflow-command annotation, read off stdout.
            printf '::warning title=%s::%s did not judge against a trunk — BOSS_TRUNK=none, so its per-car comparison was not made\n' \
                "$LINT_NO_TRUNK_MARKER" "$lint"
            return 0
            ;;
        *)
            {
                printf '%s: %s — BOSS_TRUNK=%s is not a declaration this tree knows — the one word is none.\n' \
                    "$lint" "$LINT_CANNOT_ANSWER_MARKER" "$BOSS_TRUNK"
                printf '  Refusing rather than reading it as "no trunk" or as unset: a misspelt\n'
                printf '  declaration must read as neither answer (infra/lint/lib/trunk-ref.sh).\n'
            } >&2
            exit "$LINT_CANNOT_ANSWER"
            ;;
    esac
}

# The candidates, and the ONE list the message is also built from, so the
# sentence a lint prints cannot drift from the walk it describes (the
# three lints' messages each said "(tried origin/main, forge/main, main)"
# while the code tried forge/main first).
TRUNK_CANDIDATES=("forge/main" "origin/main" "main")

# trunk_candidates — the refs that were tried, in order, for a message.
trunk_candidates() {
    printf '%s' "${BOSS_TRUNK_REF:+$BOSS_TRUNK_REF }${TRUNK_CANDIDATES[*]}"
}

# resolve_trunk_ref <lint>
#
#   stdout  the ref name that resolved
#   return  0 resolved
#           1 every candidate is genuinely ABSENT — git looked and said
#             so; the caller's own judgement applies
#           3 ($LINT_CANNOT_ANSWER) git could not answer; the refusal is
#             already on stderr, naming the command and git's words
resolve_trunk_ref() {
    local lint="$1" ref status
    for ref in ${BOSS_TRUNK_REF:-} "${TRUNK_CANDIDATES[@]}"; do
        git_answer "$lint" 0,1 rev-parse --verify --quiet "$ref" >/dev/null
        status=$?
        case "$status" in
            0) printf '%s\n' "$ref"; return 0 ;;
            1) continue ;;
            *) return "$LINT_CANNOT_ANSWER" ;;
        esac
    done
    return 1
}

# resolve_merge_base <lint> <base> [head]
#
# The point to compare against. No common ancestor (status 1) is an
# ANSWER — fall back to the base itself, which is what all four callers
# did — and only a git failure refuses.
#
#   stdout  the merge-base, or <base> when there is no common ancestor
#   return  0 / 3
resolve_merge_base() {
    local lint="$1" base="$2" head="${3:-HEAD}" mb status
    mb=$(git_answer "$lint" 0,1 merge-base "$base" "$head")
    status=$?
    [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && return "$LINT_CANNOT_ANSWER"
    [ -n "$mb" ] || mb="$base"
    printf '%s\n' "$mb"
    return 0
}
