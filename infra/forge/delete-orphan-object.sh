#!/usr/bin/env bash
#
# delete-orphan-object — delete ONE cluster object the TREE ALREADY
# PROVES is undeclared, and nothing else, ever.
#
# WHY IT EXISTS (backlog a139d5bb, filed urgent 2026-09-10)
# --------------------------------------------------------
# The converge runs `kubectl apply` with no `--prune`, so deleting a
# manifest removes the DECLARATION and leaves the OBJECT running. Until
# this verb, clearing one was a sentence in a packet asking a person to
# type `kubectl -n boss delete svc <name>`, and an agent could not do it
# at all. That hand-off blocked real work: `Service/boss-docs-internal`
# outlived its manifest by nine days, and the orphan lint that finds it
# exits 1 inside the converge — so a gated-green car could not land,
# because landing it would fail every converge after it until a human
# intervened. The car was CORRECT to fail; the gap was that nothing but
# a human could clear what it detected.
#
# WHY NOT A `kubectl delete` VERB
# -------------------------------
# The allowlist's whole value is that every verb is a reviewed word with
# bounded params. "Delete any object by name" hands an agent
# namespace-wide destruction through an audited door, which is worse
# than no door: the audit would record exactly what was destroyed, with
# nothing having refused it. CLAUDE.md is explicit — reads and bounded
# reclaims are mechanical, destructive-by-policy actions are not, and
# keeping that line sharp is what makes handing over the first kind
# safe.
#
# SO THE AUTHORITY IS DERIVED, NOT GRANTED
# ----------------------------------------
# This script cannot delete anything the tree does not already prove is
# undeclared. It re-runs that computation itself, at call time, through
# the ONE definition of it — `infra/cluster/undeclared-objects.sh`, the
# same script the orphan lint reports from — and acts only if that
# computation names the object it was given. Five bounds, in the order
# they are applied:
#
#   1. THE ARGUMENT'S SHAPE. `<Kind>/<namespace>/<name>`, the same
#      pattern the allowlist validated, checked again here so the bound
#      does not depend on one layer (the run-car-probe convention).
#   2. A DECLARATION SET THAT IS A COMMIT. The tree read is a generation
#      of root's own tree (infra/forge/root-tree.sh), proved against the
#      store beside it to be its commit, byte for byte, and nothing else.
#      An edit nobody committed — a rename in progress, a local
#      experiment, a symlink — means the tree that proves the object
#      undeclared is not a tree anyone reviewed. This is the answer to
#      "what if a manifest is mid-rename": a rename that is not committed
#      is not a declaration set, and a rename that IS committed has
#      removed the old name on purpose.
#   3. THE DERIVATION. `undeclared-objects.sh --check` must say
#      undeclared. It refuses, by name, for a declared object, a
#      namespace the tree does not own, a kind the tree declares nothing
#      of, an excluded kind, an exempt object, a controller-owned
#      object, and an object that is not live; and it says CANNOT ANSWER
#      — never "undeclared" — when the credential cannot list the pair
#      or a manifest will not parse.
#   4. THE KIND FLOOR below, narrower than the derivation's scope: a
#      kind whose deletion destroys bytes or credentials stays a human
#      step even when the tree proves the object undeclared.
#   5. CAPTURE BEFORE DELETE. The object's own YAML is printed first, so
#      the packet that asked holds what it was. A deleted object cannot
#      be read back, and the ops-request is where the record lands.
#
# The delete is then issued with the kind, namespace and name from the
# DERIVATION's own output line — never from the argument string — so the
# packet's text is only ever compared, never executed against.
#
# `--dry-run` stops after bound 5: the way to ask the audited door
# "would this be allowed?" without acting, and the way the verb is
# exercised live without deleting anything.
#
# USAGE
#   delete-orphan-object.sh <Kind>/<namespace>/<name> [--dry-run]
#
# EXIT
#   0  deleted (or, with --dry-run, would be)
#   2  refused — the reason is named on stderr
#   1  could not answer, or the delete failed
#
# ENV
#   BOSS_CLUSTER_TREE  the tree whose manifests are the declaration set
#                      (default: the tree this script lives in — on the
#                      forge, the generation of root's tree the runner
#                      started from). Whatever it names must be a
#                      generation, <tree>/gen/<sha> beside <tree>/repo.git
#                      (bound 2). A test seam; a packet cannot set it,
#                      because the ops-runner passes no packet-supplied
#                      environment — only an argv built from the
#                      allowlist. BOSS_OPS_CHECKOUT, the checkout the
#                      forge's runner hands its git verbs, is deliberately
#                      NOT read: until backlog 1dde630c it was this
#                      default, and its owner could edit what root then
#                      derived its authority from.
#   BOSS_KUBECTL       see undeclared-objects.sh; resolved there, once.
#   KUBECONFIG         a credential that can read the managed namespaces
#                      and delete in them. Under the ops-runner there is
#                      no HOME, so the kubeconfig is named explicitly by
#                      the resolver rather than found at ~/.kube/config.

set -uo pipefail

ME="delete-orphan-object"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; exit 2; }

# Physical, once: started as /var/lib/boss/tree/current/..., this names
# the generation itself, which a move of `current` cannot change under
# the run (the ops runner resolves its own directory the same way).
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$SELF_DIR/../.." && pwd -P)"
DERIVE="$REPO/infra/cluster/undeclared-objects.sh"
TREE="${BOSS_CLUSTER_TREE:-$REPO}"
MANIFEST_REL="infra/cluster/manifests"

# --- bound 1: the argument's shape -----------------------------------------
TARGET="${1:-}"
MODE="${2:-}"
if [ -z "$TARGET" ] || [ "$#" -gt 2 ]; then
    say "usage: $ME <Kind>/<namespace>/<name> [--dry-run]"
    say "  One object, named in full. There is no form of this verb that takes a selector,"
    say "  a namespace on its own, or more than one object."
    exit 2
fi
DRY=0
case "$MODE" in
    "") ;;
    --dry-run) DRY=1 ;;
    *) refuse "the only second argument is --dry-run, not \`$MODE\`" ;;
esac
# The same pattern the allowlist applies, applied again here: a bound
# that exists in one layer only is a bound that disappears the day
# something else calls the script.
if ! [[ "$TARGET" =~ ^[A-Z][A-Za-z0-9]{0,62}/[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?/[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]]; then
    refuse "\`$TARGET\` is not <Kind>/<namespace>/<name> — Kind capitalised, namespace and name DNS labels"
fi

[ -x "$DERIVE" ] || refuse "the derivation $DERIVE is missing or not executable, so there is no authority to act on"

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- bound 2: the declaration set is a verified generation of root's tree --
#
# THE DEFECT (backlog 1dde630c, review a8b752d4 N6). This verb runs as
# root. Its declaration set used to be a CHECKOUT — on the forge the one
# the runner hands its git verbs, which its owner can edit — and the only
# proof that the set was reviewed content was `git status` over
# $MANIFEST_REL. The derivation reads more than the manifests:
# instances.toml and the renderer's RENDERS line (the per-instance
# exemption), infra/gate-runner/*.yaml, the consist worker's job.json, the
# dev and isolation declarations. Any of them could be edited uncommitted,
# or replaced by a symlink root then followed — and an uncommitted
# instances.toml that moved the instance out of `boss` made the ConfigMap
# the converge renders and mounts there read as undeclared, and deletable.
#
# Root's own tree is the answer that already exists (infra/forge/root-
# tree.sh, design c98c79aa): one root-owned export per commit main named,
# gen/<sha>, beside root's own bare store, repo.git, never written again.
# So the tree must BE such a generation — its name a commit, its
# `.boss-tree-sha` saying the same, the store beside it — every directory
# of the layout this uid's and writable by nobody else, and then it is
# PROVED to be that commit against the store: every tracked path matches
# its blob (content, mode and type, so a symlink or an exec bit is a
# difference) and nothing but `.boss-tree-sha` is present that the commit
# does not carry. Any difference is a refusal naming the path, before the
# derivation reads a byte.
#
# Root's git in root's store needs no privilege drop, which is why the
# as_owner drop this block replaced is gone: it existed because root
# cannot read a clone it does not own ("dubious ownership", ops-request
# c9877f75), and this verb no longer reads a clone. The store is read the
# way root-tree.sh reads it — no account's config, no system config, no
# hook — through an index of this run's own, so nothing in the store is
# written.
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so there is no declaration set to read"
TREE="$(cd "$TREE" && pwd -P)" || refuse "cannot resolve the tree $TREE"
SHA="${TREE##*/}"
GEN_DIR="${TREE%/*}"
ROOT_TREE="${GEN_DIR%/*}"
STORE="$ROOT_TREE/repo.git"
if ! [[ "$SHA" =~ ^[0-9a-f]{40,64}$ ]] || [ "${GEN_DIR##*/}" != gen ] || [ ! -d "$STORE" ]; then
    say "REFUSED — $TREE is not a generation of root's tree: <tree>/gen/<sha> with <tree>/repo.git"
    say "  beside it (infra/forge/root-tree.sh). A checkout, however clean, is a tree its owner can"
    say "  edit between this check and the derivation's read, so it is no declaration set for root."
    exit 2
fi
[ "$(cat -- "$TREE/.boss-tree-sha" 2>/dev/null)" = "$SHA" ] \
    || refuse "$TREE/.boss-tree-sha does not say it is ${SHA:0:12} — not a generation root-tree.sh made"
ME_UID="$(id -u)"
for d in "$ROOT_TREE" "$GEN_DIR" "$TREE" "$STORE"; do
    owner_uid="$(stat -c %u -- "$d" 2>/dev/null)"
    { [ "$owner_uid" = "$ME_UID" ] && [ ! -L "$d" ]; } \
        || refuse "$d is owned by uid ${owner_uid:-unknown}, not this uid ($ME_UID), or is a symlink — another account could rewrite the declaration set under this run"
    [ -z "$(find "$d" -maxdepth 0 -perm /0022 2>/dev/null)" ] \
        || refuse "$d is writable by its group or by anyone ($(stat -c %A -- "$d")) — another account could rewrite the declaration set under this run"
done
store_git() {
    GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_INDEX_FILE="$TMP/index" \
        git -c core.hooksPath=/dev/null --git-dir="$STORE" --work-tree="$TREE" "$@"
}
store_git cat-file -e "$SHA^{commit}" 2>"$TMP/git.err" \
    || refuse "the store $STORE does not hold the commit ${SHA:0:12} this generation names, so nothing proves what it is: $(tr '\n' ' ' <"$TMP/git.err")"
store_git read-tree "$SHA" 2>"$TMP/git.err" \
    || refuse "cannot read the commit ${SHA:0:12} out of $STORE: $(tr '\n' ' ' <"$TMP/git.err")"
# Hashes every tracked file against its blob; a path that differs stays
# unrefreshed and is listed below. Its own exit says only that some did.
store_git update-index -q --refresh >/dev/null 2>&1
store_git diff-files --name-only >"$TMP/differs" 2>"$TMP/git.err" \
    || refuse "cannot compare $TREE with its commit: $(tr '\n' ' ' <"$TMP/git.err")"
store_git ls-files --others >"$TMP/others" 2>"$TMP/git.err" \
    || refuse "cannot list what $TREE holds beyond its commit: $(tr '\n' ' ' <"$TMP/git.err")"
# grep's 1 is "nothing else is there"; only a 2 is a read that failed.
extra="$(grep -vxF .boss-tree-sha "$TMP/others")" || [ $? -eq 1 ] \
    || refuse "cannot read the list of what $TREE holds beyond its commit"
if [ -s "$TMP/differs" ] || [ -n "$extra" ]; then
    say "REFUSED — $TREE is not the commit ${SHA:0:12} it says it is, so the tree that would prove"
    say "  this object undeclared is not one anybody reviewed. Differs from the commit:"
    sed 's/^/    /' "$TMP/differs" >&2
    [ -z "$extra" ] || { say "  Not in the commit at all:"; printf '%s\n' "$extra" | sed 's/^/    /' >&2; }
    say "  A proved generation must remain unchanged after export; these paths differ from its record."
    exit 2
fi
# Read once, reported twice: the commit the declaration set came from.
HEAD_SHA="${SHA:0:12}"

# --- bound 3: the derivation -----------------------------------------------
say "deriving authority from $MANIFEST_REL at $HEAD_SHA (root's generation $TREE, proved against $STORE) via ${DERIVE#"$REPO"/}"
BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --check "$TARGET" >"$TMP/check.out" 2>"$TMP/check.err"
rc=$?
# The derivation's own words, always — on every path. A refusal whose
# reason somebody has to go re-derive is not a verdict.
cat "$TMP/check.err" >&2
case "$rc" in
    0) ;;
    3) say "REFUSED — the derivation does not name $TARGET as undeclared (see its reason above)."; exit 2 ;;
    *) say "CANNOT ANSWER — the derivation could not decide (exit $rc). Nothing deleted."; exit 1 ;;
esac

IFS=$'\t' read -r verdict KIND NS NAME < "$TMP/check.out"
if [ "${verdict:-}" != "undeclared" ] || [ -z "${KIND:-}" ] || [ -z "${NS:-}" ] || [ -z "${NAME:-}" ]; then
    say "CANNOT ANSWER — the derivation exited 0 but its answer did not parse: $(cat "$TMP/check.out")"
    exit 1
fi
# Belt and braces: the derivation answered about the object that was
# asked about. The delete below uses ITS fields, so this compares the two
# spellings rather than trusting either alone.
if [ "$KIND/$NS/$NAME" != "$TARGET" ]; then
    say "CANNOT ANSWER — the derivation answered about $KIND/$NS/$NAME, not $TARGET"
    exit 1
fi
printf '%s\n' "undeclared	$KIND	$NS	$NAME"

# --- bound 4: the kind floor ----------------------------------------------
# Narrower than the derivation's scope, and deliberately so. The
# derivation proves "the tree does not declare this"; it cannot prove
# "deleting this destroys nothing that cannot be rebuilt". These kinds
# are the ones where reapplying a manifest restores the object
# completely:
#
#   Service, ConfigMap, Deployment, CronJob, Job — configuration and
#   workloads. Deleting an undeclared one removes something the converge
#   does not manage and never did; the cluster's own controllers rebuild
#   nothing that was not declared.
#
# NOT on this list, each for its reason — these stay a named human step:
#   PersistentVolumeClaim, StatefulSet — bytes on Longhorn, including
#     the volumes holding the audit log and the system of record. A
#     deletion here is data loss, which no derivation makes reversible.
#   Secret — credential material, and already out of the derivation's
#     scope (the tree references secrets by name and creates them out of
#     band, so its declaration set for them is incomplete by design).
#   ServiceAccount — deleting one invalidates the tokens issued from it.
#   Role, RoleBinding — deleting a privilege can remove a live
#     capability nobody wrote down, which is exactly the shape of the
#     2026-08-28 dev-session grant (95f6aba5). Loud is better than
#     automatic here.
#   Everything cluster-scoped — out of the derivation's scope entirely.
#
# Widening this list is a PR against this file, reviewed like the
# allowlist entry itself. That is the point: the floor is a reviewed
# word, not a runtime decision.
DELETABLE_KINDS=(Service ConfigMap Deployment CronJob Job)
in_floor=0
for k in "${DELETABLE_KINDS[@]}"; do
    [ "$k" = "$KIND" ] && in_floor=1
done
if [ "$in_floor" -ne 1 ]; then
    say "REFUSED — $KIND is undeclared, and still not a kind this verb deletes."
    say "  This verb deletes: ${DELETABLE_KINDS[*]} — kinds a manifest reapply restores completely."
    say "  $KIND is withheld because its deletion destroys bytes or privileges that no derivation"
    say "  makes reversible (see DELETABLE_KINDS in $ME for the per-kind reason). That delete stays"
    say "  a named human step; widening the floor is a reviewed change to this file."
    exit 2
fi

# --- the kubectl, resolved once, by the derivation -------------------------
KUBECTL_LINE=$(BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --kubectl) || {
    say "CANNOT ANSWER — no kubectl to act with (see above). Nothing deleted."
    exit 1
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"

# --- bound 5: capture before delete ---------------------------------------
# A deleted object cannot be read back, and this packet is where the
# record lands. If the capture fails, the delete does not happen: a
# destructive step whose evidence failed first is one nobody can review
# afterwards.
if ! "${KUBECTL[@]}" get "$KIND" "$NAME" -n "$NS" -o yaml --request-timeout=30s > "$TMP/object.yaml" 2> "$TMP/get.err"; then
    say "CANNOT ANSWER — could not read $KIND/$NS/$NAME to record what it was:"
    sed 's/^/    /' "$TMP/get.err" >&2
    say "  Nothing deleted: the capture is the only copy there will be."
    exit 1
fi
echo "--- $KIND/$NS/$NAME as it was, before the delete ---"
cat "$TMP/object.yaml"
echo "--- end of $KIND/$NS/$NAME ---"

if [ "$DRY" -eq 1 ]; then
    say "DRY RUN — would delete $KIND \`$NAME\` in \`$NS\`. Every bound passed; nothing was deleted."
    exit 0
fi

# --- the delete -----------------------------------------------------------
say "deleting $KIND \`$NAME\` in \`$NS\` — undeclared by ${MANIFEST_REL} at ${HEAD_SHA:-?}"
if ! "${KUBECTL[@]}" delete "$KIND" "$NAME" -n "$NS" --request-timeout=60s > "$TMP/del.out" 2>&1; then
    cat "$TMP/del.out" >&2
    say "the delete FAILED. The object is still there as far as this run knows."
    exit 1
fi
cat "$TMP/del.out"

# Verified gone, because "the command exited 0" and "the object is gone"
# are different claims. And gone is ONE answer, NotFound — not "the get
# failed" (backlog 1058e686, car B): until this read-back took the
# server's words, a timeout, an auth error or an unreachable API server
# failed the get exactly as NotFound does and printed OK, a proof that
# failed open. Any other failure is said as what it is: the delete ran,
# and nothing has shown the object gone.
if "${KUBECTL[@]}" get "$KIND" "$NAME" -n "$NS" --request-timeout=10s >/dev/null 2> "$TMP/gone.err"; then
    say "the delete returned success and $KIND/$NS/$NAME is STILL THERE — something is recreating it."
    say "  Look for a controller or a hand-run apply; this verb will not try again."
    exit 1
fi
if ! grep -qF 'Error from server (NotFound)' "$TMP/gone.err"; then
    say "CANNOT ANSWER — deleted, not proven gone: the delete of $KIND/$NS/$NAME returned success, and the read-back answered something other than NotFound:"
    sed 's/^/    /' "$TMP/gone.err" >&2
    exit 1
fi
say "OK — $KIND \`$NAME\` is gone from \`$NS\` (read back: NotFound), and no manifest declared it."
exit 0
