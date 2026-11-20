#!/usr/bin/env bash
# postgres-placement.sh — READ one namespace's postgres StatefulSet
# placement, live and as the converge renders it. The ops verb
# `read-postgres-placement`.
#
#   postgres-placement.sh <namespace>
#
# WHY IT EXISTS (backlog eb7c4e0b, 2026-11-16). The review of the
# postgres-rollout forward fix (car 973c1488, backlog 9123d811) had to
# know the first converge's effect on BOTH live StatefulSets: whether
# either carries an affinity, nodeSelector or tolerations key its
# rendering lacks, because that fix removes such a key and rolls the pod.
# dev-session is Forbidden a StatefulSet get in boss-playground (and the
# conductor's shell in boss too, measured at triage), so the playground
# stayed NOT OBSERVED and no actor below David could read it. No verb
# under infra/ops/verbs/ read a StatefulSet's placement.
#
# THE BOUND is this script's:
#   * ONE namespace per run, named from ALLOWED below and nothing else —
#     a word off the list is refused (exit 78) before any door opens. The
#     verb file's `one_of` offers exactly this list, and
#     crates/core/boss-testing/tests/read_postgres_placement_sh.rs holds
#     the two equal (CLAUDE.md §9a);
#   * through the forge's ops_kubectl door (the admin kubeconfig David
#     placed, infra/estate/ops-credentials.sh), no RBAC widened: `get
#     sts/postgres` for the live object, and — inside the converge's own
#     rendering — `create --dry-run=client`, which serializes the
#     rendered YAML on the client and sends no object to the server, the
#     form dev-build-fence-status.sh already reads with. There is no
#     write in either file, and the same test refuses a door call that is
#     neither.
#
# THE RENDERING is postgres-rollout.py's, imported, not copied
# (postgres-placement.py says how): the tree this run started from,
# rendered as rendered_postgres renders a named source once exported.
#
# THE ANSWER names the StatefulSet's uid, resourceVersion, generation,
# observedGeneration and current/update revision, the three placement
# fields of its pod template verbatim, the same three rendered, and each
# key that differs. Its last line is the verdict
# `READ — <ns> live=<keys> rendered=<keys> prune=<keys or none>`.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never an empty placement. Exit 78 is a refusal.
set -uo pipefail

ME=read-postgres-placement
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"

# The namespaces this verb may read, space-separated, one line.
ALLOWED="boss boss-playground"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

[ $# -eq 1 ] || refuse "usage: postgres-placement.sh <namespace> — one namespace, one of: $ALLOWED"
NS=""
for n in $ALLOWED; do
    [ "$1" = "$n" ] && NS="$n"
done
[ -n "$NS" ] || refuse "'$1' is not a namespace this verb reads — it reads only: $ALLOWED"
command -v python3 >/dev/null 2>&1 || fail "CANNOT ANSWER — python3 is not on PATH, and the converge's rendering is python"

# THE TREE THIS RUN STARTED FROM is what it renders, and the commit it
# names is that tree's (backlog 2cfea79b). On the forge the ops runner
# starts a verb from a generation of root's tree, gen/<sha>
# (infra/forge/root-tree.sh): the commit's content with NO .git, saying
# which commit it is in .boss-tree-sha. The first two live runs asked git
# for HEAD there and answered CANNOT ANSWER (ops-requests 4eff3ed4,
# 44e6bee1). A generation is named by its marker, held to its directory
# name as delete-orphan-object.sh holds it; a checkout (a hand run, the
# tests) by its HEAD, marked when the rendered paths differ from it.
REPO="$(cd "$HERE/../.." && pwd -P)"
if [ -e "$REPO/.boss-tree-sha" ]; then
    SOURCE="$(cat -- "$REPO/.boss-tree-sha" 2>/dev/null)"
    [[ $SOURCE =~ ^[0-9a-f]{40,64}$ ]] && [ "$SOURCE" = "${REPO##*/}" ] \
        || fail "CANNOT ANSWER — $REPO/.boss-tree-sha says '$SOURCE', not the commit the generation's directory names; what this tree renders has no name"
    FROM="the generation $REPO"
else
    SOURCE="$(git -C "$REPO" rev-parse --verify 'HEAD^{commit}' 2>&1)" \
        || fail "CANNOT ANSWER — $REPO is neither a generation of root's tree (no .boss-tree-sha) nor a checkout naming a HEAD: $SOURCE"
    FROM="the checkout $REPO"
    [ -z "$(git -C "$REPO" status --porcelain -- infra/cluster examples)" ] \
        || FROM="$FROM, WITH uncommitted changes under infra/cluster or examples"
fi

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "CANNOT ANSWER — the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2

python3 -B "$HERE/postgres-placement.py" --namespace "$NS" \
    --read "$(ops_kubectl)" --write "$(ops_kubectl --stdin)" \
    --repo "$REPO" --source "$SOURCE" --tree "$FROM" --work "$WORK"
