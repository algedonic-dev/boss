# no-estate.sh — the one way a run declares there is no estate where it
# runs, and what a lint that reads the estate does with the declaration.
#
# WHY (backlog 3e63662c, measured 2026-09-27 by run 37e76794). The
# public mirror's Gate (.github/workflows/ci.yml, infra/gate.sh full)
# ended REFUSED, exit 2, on every publish PR since ci.yml returned on
# 2026-09-22 (#243, #244, #245). Three roster lints read the estate —
# a-car-stays-under-the-edit-level (BOSS_JOBS_URL), the-live-rules-are-
# the-authored-rules (BOSS_DISPATCHER_URL), the-live-protocols-are-the-
# authored-protocols (BOSS_JOBS_URL) — their defaults are
# *.svc.cluster.local, and a GitHub runner has no route to any of them.
# Each exited 3 (CANNOT ANSWER), check_lint wrote a refusal receipt, and
# the refusal outranked even a real red: a publish PR's Gate said
# nothing about the tree, every time, for a reason that was true of the
# runner and never of the branch.
#
# THE DECLARATION. `BOSS_ESTATE=none`, set in the environment of a run
# that has no estate to read. Today exactly one caller sets it, the
# public mirror's workflow, and a test holds it to that one
# (a_declared_no_estate_skips_only_the_live_half.rs,
# `only_the_public_mirror_declares_no_estate`).
#
#   unset or empty  nothing changes: the lint reads the estate, and an
#                   unreachable one is still CANNOT ANSWER — a refusal.
#                   Everywhere but the mirror this is the only answer.
#   none            on a GitHub-hosted runner on github.com and nowhere
#                   else (the runtime guard below refuses it in the
#                   gate-runner Job, in a forge or Gitea Actions job,
#                   off Actions and on any other server): the lint
#                   runs its TREE half as always, does not read
#                   the estate, prints the marker below naming what it
#                   did not compare, and passes that half. The gate
#                   reads the marker off the lint's stderr and names the
#                   lint under `estate` on the receipt, so a reader sees
#                   the live checks did not run.
#   anything else   REFUSED, exit 3, naming the variable and its value.
#                   A misspelt declaration (`None`, `off`) must read as
#                   neither answer: not as "no estate", which would
#                   excuse a live check on a machine that has one, and
#                   not as unset, which would hide the misspelling
#                   behind a refusal about something else.
#
# WHY NOT A SUBSET MODE. the_mirror_runs_the_one_gate.rs refuses one:
# a mode that drops checks by name is the second definition of the gate
# that file exists to prevent. The declaration drops no check — every
# lint still runs, and each says, in its own output, the one comparison
# it did not make and why.
#
# WHO HONOURS IT. The lints that read the estate — the ones that source
# lib/sor-read.sh — and no others; a test holds the two sets equal. A
# caller that REQUIRES the live comparison (the protocols lint's
# `--require-live`, which infra/protocol-drift.sh runs) does not consult
# it at all: for that caller "could not compare" stays exit 75.

# shellcheck source=infra/lint/lib/git-answer.sh
. "$(dirname "${BASH_SOURCE[0]}")/git-answer.sh" || exit 3

# The words a lint prints when it honoured the declaration. A constant
# because two things read it: an operator, and gate.sh, which names
# every lint that printed it on the receipt (CLAUDE.md §9a).
LINT_NO_ESTATE_MARKER="LIVE HALF NOT RUN"
export LINT_NO_ESTATE_MARKER

# lint_no_estate <lint> <what the live half would have read>
#
#   return 1  no declaration: read the estate, as always
#   return 0  BOSS_ESTATE=none on a GitHub-hosted runner: the marker is printed
#             on stderr, and the caller skips its live half and passes it
#   exits     $LINT_CANNOT_ANSWER for any other value, and for `none`
#             anywhere but a GitHub-hosted runner (the guard below)
#
# Call it where the live half begins, AFTER the tree half has run and
# recorded its findings, so a declaration can never excuse the tree.
#
# THE RUNTIME GUARD (the adversarial review of c9439bd1). A test holds
# the tree to one declarer, but a text scan cannot see a variable set
# outside the tree — a manifest applied by hand, an image built
# elsewhere, an inline prefix typed at a shell — and a declaration that
# reached a cluster gate would pass every car without its live
# comparisons, while every reader of a gate judges the verdict alone.
# So the declaration is honoured only where it is true — a GitHub-hosted
# runner on github.com — and refused, in this order, everywhere else:
#
#   KUBERNETES_SERVICE_HOST set   REFUSED. The gate-runner Job's
#                                 container carries it (measured by the
#                                 review of cc7273c0), and that Job is
#                                 exactly where the estate must be read.
#                                 NOT every shell in a pod carries it:
#                                 the dev pod's sessions start through
#                                 sshd and tmux without it (measured, the
#                                 same review), so a builder's shell is
#                                 caught by the checks below instead.
#   FORGEJO_ACTIONS or            REFUSED. The re-review of cc7273c0
#   GITEA_ACTIONS set             reproduced all three lints honouring
#                                 the declaration in a forge Actions
#                                 job: Forgejo sets GITHUB_ACTIONS=true
#                                 too, and .forgejo/workflows/ci.yml runs
#                                 on the forge host, off the cluster.
#   GITHUB_ACTIONS not `true`     REFUSED — no Actions runner at all.
#   GITHUB_SERVER_URL not         REFUSED — Actions on another server
#   https://github.com            (a forge that clears its own variable,
#                                 an Enterprise instance) is not the
#                                 public mirror.
#
# Every refusal is exit 3, naming the variable that decided it.
_no_estate_refuse() { # <lint> <why>
    {
        printf '%s: %s — BOSS_ESTATE=none is refused here: %s\n' \
            "$1" "$LINT_CANNOT_ANSWER_MARKER" "$2"
        printf '  The declaration belongs to the public mirror, a GitHub-hosted runner, and\n'
        printf '  nowhere else. Honoured here it would pass a check that was never made\n'
        printf '  (infra/lint/lib/no-estate.sh). Unset it, or run where it is true.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
}

lint_no_estate() {
    local lint="$1" what="$2"
    case "${BOSS_ESTATE:-}" in
        "") return 1 ;;
        none)
            if [ -n "${KUBERNETES_SERVICE_HOST:-}" ]; then
                _no_estate_refuse "$lint" "KUBERNETES_SERVICE_HOST is set, so this is a Kubernetes pod such as the gate-runner Job, where the estate is"
            fi
            if [ -n "${FORGEJO_ACTIONS:-}" ]; then
                _no_estate_refuse "$lint" "FORGEJO_ACTIONS is set, so this is a forge Actions job, not the public mirror"
            fi
            if [ -n "${GITEA_ACTIONS:-}" ]; then
                _no_estate_refuse "$lint" "GITEA_ACTIONS is set, so this is a Gitea Actions job, not the public mirror"
            fi
            if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
                _no_estate_refuse "$lint" "GITHUB_ACTIONS is not true, so this is no Actions runner at all"
            fi
            if [ "${GITHUB_SERVER_URL:-}" != "https://github.com" ]; then
                _no_estate_refuse "$lint" "GITHUB_SERVER_URL is '${GITHUB_SERVER_URL:-unset}', not https://github.com, so this is not the public mirror"
            fi
            {
                printf '%s: %s — BOSS_ESTATE=none declares there is no estate here, so %s was not read.\n' \
                    "$lint" "$LINT_NO_ESTATE_MARKER" "$what"
                printf '  The tree half runs as always. NOTHING IS CLAIMED about the live system: this\n'
                printf '  comparison was not made, and the gate receipt names this lint under `estate`.\n'
            } >&2
            # The same fact where a reader of the GitHub checks page sees
            # it: a workflow-command annotation, read off stdout.
            printf '::warning title=%s::%s did not read the estate — BOSS_ESTATE=none, so its live comparison was not made\n' \
                "$LINT_NO_ESTATE_MARKER" "$lint"
            return 0
            ;;
        *)
            {
                printf '%s: %s — BOSS_ESTATE=%s is not a declaration this tree knows — the one word is none.\n' \
                    "$lint" "$LINT_CANNOT_ANSWER_MARKER" "$BOSS_ESTATE"
                printf '  Refusing rather than reading it as "no estate" or as unset: a misspelt\n'
                printf '  declaration must read as neither answer (infra/lint/lib/no-estate.sh).\n'
            } >&2
            exit "$LINT_CANNOT_ANSWER"
            ;;
    esac
}
