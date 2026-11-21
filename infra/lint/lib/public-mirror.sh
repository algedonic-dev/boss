# public-mirror.sh — the one place that decides whether a run IS the
# public mirror, for the declarations only the mirror may make.
#
# WHY ONE FILE (backlog 2d28df78, design e2710fbc; CLAUDE.md §9a). Two
# declarations name a fact about the run environment that its checks
# cannot supply for themselves: `BOSS_ESTATE=none` (lib/no-estate.sh,
# backlog 3e63662c — there is no estate to read) and `BOSS_TRUNK=none`
# (lib/trunk-ref.sh — there is no per-car trunk to judge against). Both
# are true on the public mirror's GitHub-hosted runner and nowhere else,
# and both would pass every car without its judgements if they reached a
# cluster or forge gate. The guard below was written for the first one;
# the second calls the same function rather than carrying a copy that
# could be tightened in one place and not the other.
#
# THE RUNTIME GUARD (the adversarial review of c9439bd1). A test holds
# the tree to one declarer per variable, but a text scan cannot see a
# variable set outside the tree — a manifest applied by hand, an image
# built elsewhere, an inline prefix typed at a shell. So a declaration is
# honoured only where it is true — a GitHub-hosted runner on github.com —
# and refused, in this order, everywhere else:
#
#   KUBERNETES_SERVICE_HOST set   REFUSED. The gate-runner Job's
#                                 container carries it (measured by the
#                                 review of cc7273c0), and that Job is
#                                 exactly where every car is judged.
#                                 NOT every shell in a pod carries it:
#                                 the dev pod's sessions start through
#                                 sshd and tmux without it (measured, the
#                                 same review), so a builder's shell is
#                                 caught by the checks below instead.
#   FORGEJO_ACTIONS or            REFUSED. The re-review of cc7273c0
#   GITEA_ACTIONS set             reproduced the estate lints honouring
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
# Every refusal is exit 3 (LINT_CANNOT_ANSWER), naming the declaration
# and the variable that decided it.

# shellcheck source=infra/lint/lib/git-answer.sh
. "$(dirname "${BASH_SOURCE[0]}")/git-answer.sh" || exit 3

_public_mirror_refuse() { # <lint> <declaration> <why>
    {
        printf '%s: %s — %s is refused here: %s\n' \
            "$1" "$LINT_CANNOT_ANSWER_MARKER" "$2" "$3"
        printf '  The declaration belongs to the public mirror, a GitHub-hosted runner, and\n'
        printf '  nowhere else. Honoured here it would pass a check that was never made\n'
        printf '  (infra/lint/lib/public-mirror.sh). Unset it, or run where it is true.\n'
    } >&2
    exit "$LINT_CANNOT_ANSWER"
}

# refuse_off_the_public_mirror <lint> <declaration, e.g. BOSS_TRUNK=none>
#
#   return 0  this is a GitHub-hosted runner on github.com
#   exits     $LINT_CANNOT_ANSWER anywhere else, naming the variable
refuse_off_the_public_mirror() {
    local lint="$1" decl="$2"
    if [ -n "${KUBERNETES_SERVICE_HOST:-}" ]; then
        _public_mirror_refuse "$lint" "$decl" "KUBERNETES_SERVICE_HOST is set, so this is a Kubernetes pod such as the gate-runner Job, where every car is judged"
    fi
    if [ -n "${FORGEJO_ACTIONS:-}" ]; then
        _public_mirror_refuse "$lint" "$decl" "FORGEJO_ACTIONS is set, so this is a forge Actions job, not the public mirror"
    fi
    if [ -n "${GITEA_ACTIONS:-}" ]; then
        _public_mirror_refuse "$lint" "$decl" "GITEA_ACTIONS is set, so this is a Gitea Actions job, not the public mirror"
    fi
    if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
        _public_mirror_refuse "$lint" "$decl" "GITHUB_ACTIONS is not true, so this is no Actions runner at all"
    fi
    if [ "${GITHUB_SERVER_URL:-}" != "https://github.com" ]; then
        _public_mirror_refuse "$lint" "$decl" "GITHUB_SERVER_URL is '${GITHUB_SERVER_URL:-unset}', not https://github.com, so this is not the public mirror"
    fi
    return 0
}
