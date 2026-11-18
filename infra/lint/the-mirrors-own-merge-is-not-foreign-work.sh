#!/usr/bin/env bash
# the-mirrors-own-merge-is-not-foreign-work.sh — the publish measurement
# blocks on CONTENT the public mirror holds that the source ref does
# not, and never on the mirror's own publish bookkeeping.
#
# WHY THIS LINT EXISTS. Until 2026-09-10 infra/prep-github-publish.sh
# compared COMMIT IDENTITY: any commit in `SOURCE..TARGET` blocked the
# publish. But every successful publish PLANTS such a commit — the
# snapshot is a `commit-tree` of the source ref's tree onto the mirror's
# main, and merging the PR adds GitHub's merge commit on top, neither of
# which the source ref can ever contain. The condition therefore became
# permanently true the first time the protocol was used, and the
# protocol could never run again. Measured that night: the one "absent"
# commit was b178a534 "Publish: forge main through 2026-08-27 (#129)
# (#236)", tree 02a76d57 — which is exactly the tree of forge main's
# b78ae6d2. Identical trees, nothing to lose, and 238 commits / 763
# files of drift declared unpublishable.
#
# The protection the check was written for is real and stays: a commit
# somebody made ON the mirror that changed files is foreign work, and
# publishing a snapshot over it destroys it. So the comparison is now
# content, and this self-test drives the REAL script over throwaway
# local repositories in a temp dir — no remote, no credential, no
# network — in four directions:
#
#   A. bookkeeping   a mirror commit whose tree is already in the source
#                    history, plus a merge on top that changed nothing:
#                    does NOT block.
#   B. foreign work  a mirror commit that changed a file: DOES block,
#                    and names the commit and the file.
#   C. cannot tell   the classifier handed an unresolvable ref REFUSES
#                    (exit 2) rather than reporting "no foreign work" —
#                    the `|| echo 0` lesson in the script's own header,
#                    which once read "the mirror is current" while the
#                    mirror was 211 commits behind.
#   D. lib absent    the script run from a tree with no classifier
#                    refuses (exit 2) rather than measuring without it.
#   E. filtered      once the source holds the forge-only `internal/`, a
#                    snapshot carries its PUBLIC tree, which no source
#                    commit ever held (design f7087b8c D1). That snapshot
#                    is bookkeeping — found both by the source it names
#                    and, naming none, by the walk — and a train that
#                    touched only internal/ is no drift.
#   F. the filter    public_tree drops the ROOT entry `internal` and
#                    nothing else (not docs/internal, not internalx), and
#                    is the commit's own tree when there is none.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
infra="$here/.."
script="$infra/prep-github-publish.sh"
lib="$infra/mirror-drift-lib.sh"
fails=0
fail() {
    echo "FAIL: $*" >&2
    fails=$((fails + 1))
}

[ -f "$script" ] || {
    echo "the-mirrors-own-merge-is-not-foreign-work: missing $script" >&2
    exit 1
}
[ -f "$lib" ] || {
    echo "the-mirrors-own-merge-is-not-foreign-work: missing $lib" >&2
    exit 1
}

# The fixture runs with inherited git context DROPPED. With GIT_DIR set —
# a pre-push hook exports it — a `git init` in a temp dir re-targets the
# INVOKING repo and the fixture's commits land on the real one (the
# incident behind migrations-append-only.sh's identical guard). A
# self-test that can write to the repo under test is worse than none.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY
export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@invalid
export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

tmp="$(mktemp -d)" || exit 1
trap 'rm -rf "$tmp"' EXIT
cd "$tmp" || exit 1

# Tripwire BEFORE the first mutating command: with no repo here yet, git
# must see no repository at all. If it answers with one, inherited
# context is aiming the fixture at somebody else's repo.
if leaked=$(git rev-parse --absolute-git-dir 2>/dev/null); then
    echo "the-mirrors-own-merge-is-not-foreign-work: fixture would operate on $leaked — inherited git context" >&2
    exit 1
fi

# --- the source repo: the tree a publish would push -------------------
git init -q -b main src || exit 1
mkdir -p src/infra/lint
cp "$script" src/infra/prep-github-publish.sh || exit 1
cp "$lib" src/infra/mirror-drift-lib.sh || exit 1
# A stub for the tree-wide secrets gate: this lint is about the mirror
# comparison, and the real gate is its own lint.
printf '#!/usr/bin/env bash\nexit 0\n' >src/infra/lint/no-secrets.sh
(cd src && git add -A && git commit -qm "publish one") || exit 1
PUBLISHED_TREE=$(cd src && git rev-parse 'HEAD^{tree}')
echo "later work" >src/NEWER.md
(cd src && git add -A && git commit -qm "publish two") || exit 1

# --- the mirror: what a publish left behind, then a merge on top ------
# The snapshot commit carries the published tree with no parent in the
# source history, exactly as `commit-tree <source tree>` does; the merge
# commit on top re-points history and changes nothing.
SNAP=$(cd src && git commit-tree "$PUBLISHED_TREE" -m "Publish: forge main through 2026-08-27")
MERGE=$(cd src && git commit-tree "$PUBLISHED_TREE" -p "$SNAP" -m "Publish: forge main through 2026-08-27 (#129) (#236)")
git init -q --bare mirror || exit 1
(cd src && git push -q "$tmp/mirror" "$MERGE:refs/heads/main") || exit 1
(cd src && git remote add github "$tmp/mirror") || exit 1

measure() {
    (cd "$tmp/src" && SOURCE_REF=main GITHUB_REMOTE=github GITHUB_BRANCH=main \
        bash infra/prep-github-publish.sh "$@" 2>&1)
}

# --- A. the mirror's own bookkeeping does not block -------------------
out=$(measure)
rc=$?
[ "$rc" -eq 0 ] || fail "A: the mirror's own snapshot+merge blocked the publish (exit $rc)
$out"
case "$out" in *"DO NOT PUSH"*) fail "A: the mirror's own bookkeeping printed DO NOT PUSH
$out" ;; esac
json=$(measure --json)
rc=$?
[ "$rc" -eq 0 ] || fail "A: --json blocked on bookkeeping (exit $rc): $json"
case "$json" in
    *'"blocking":""'*) ;;
    *) fail "A: --json reported blocking on bookkeeping: $json" ;;
esac
case "$json" in
    *'"commits_behind":2'*) ;;
    *) fail "A: commits_behind must still COUNT the mirror-only commits (expected 2): $json" ;;
esac
case "$json" in
    *'"commits_behind_foreign":0'*) ;;
    *) fail "A: --json must report commits_behind_foreign:0 for bookkeeping: $json" ;;
esac

# --- B. a file changed on the mirror blocks, by name ------------------
FOREIGN_BLOB=$(cd src && printf 'someone edited the mirror\n' | git hash-object -w --stdin)
(cd src && git read-tree "$PUBLISHED_TREE" &&
    git update-index --add --cacheinfo "100644,$FOREIGN_BLOB,FOREIGN.md") || exit 1
FOREIGN_TREE=$(cd src && git write-tree)
FOREIGN=$(cd src && git commit-tree "$FOREIGN_TREE" -p "$MERGE" -m "fix a typo on the mirror")
(cd src && git push -q -f "$tmp/mirror" "$FOREIGN:refs/heads/main") || exit 1
(cd src && git read-tree --empty) || exit 1

out=$(measure)
rc=$?
[ "$rc" -eq 1 ] || fail "B: a file changed on the mirror did not block (exit $rc, expected 1)
$out"
case "$out" in *"DO NOT PUSH"*) ;; *) fail "B: a foreign commit printed no refusal
$out" ;; esac
short=$(cd src && git rev-parse --short "$FOREIGN")
case "$out" in *"$short"*) ;; *) fail "B: the refusal did not name the foreign commit $short
$out" ;; esac
case "$out" in *FOREIGN.md*) ;; *) fail "B: the refusal did not name the changed file FOREIGN.md
$out" ;; esac
json=$(measure --json)
rc=$?
[ "$rc" -eq 1 ] || fail "B: --json did not block on foreign work (exit $rc): $json"
case "$json" in
    *'"blocking":"mirror-has-unmerged-commits"'*) ;;
    *) fail "B: --json lost the blocking token: $json" ;;
esac
case "$json" in
    *'"commits_behind_foreign":1'*) ;;
    *) fail "B: --json must count exactly the foreign commit: $json" ;;
esac

# --- C. a classifier that cannot tell refuses ------------------------
# Not "no foreign work found": an unanswerable question is a refusal.
out=$(cd "$tmp/src" && bash -c '. infra/mirror-drift-lib.sh
foreign_mirror_commits no-such-ref github/main' 2>&1)
rc=$?
[ "$rc" -eq 2 ] || fail "C: the classifier answered on an unresolvable ref instead of refusing (exit $rc): $out"
[ -n "$out" ] || fail "C: the refusal said nothing about why"

# --- E. a filtered snapshot is bookkeeping; internal-only is no drift --
(cd src && git read-tree HEAD) || exit 1
mkdir -p src/internal/lint
echo "how Algedonic runs it" >src/internal/operating.md
echo "exit 0" >src/internal/lint/a.sh
echo "a public change" >src/PUBLIC.md
(cd src && git add -A && git commit -qm "publish three, with internal") || exit 1
WITH_INTERNAL=$(cd src && git rev-parse HEAD)
# The public tree built BY HAND — the index without internal/ — never by
# the function under test, so the two can disagree.
PUBLIC=$(cd src && git rm -r -q --cached internal && git write-tree) || exit 1
(cd src && git read-tree HEAD) || exit 1
echo "a later internal-only train" >>src/internal/operating.md
(cd src && git add -A && git commit -qm "internal only") || exit 1

for named in yes no; do
    if [ "$named" = yes ]; then
        body="Snapshot of forge main at $WITH_INTERNAL onto the public mirror."
    else
        body="A commit carrying the same tree that names no source."
    fi
    SNAP2=$(cd src && git commit-tree "$PUBLIC" -p "$MERGE" -m "publish: three" -m "$body")
    (cd src && git push -q -f "$tmp/mirror" "$SNAP2:refs/heads/main") || exit 1
    json=$(measure --json)
    rc=$?
    [ "$rc" -eq 0 ] || fail "E($named): our own filtered snapshot blocked the publish (exit $rc): $json"
    case "$json" in
        *'"commits_behind_foreign":0'*) ;;
        *) fail "E($named): the filtered snapshot was read as foreign work: $json" ;;
    esac
    case "$json" in
        *'"has_drift":false'*) ;;
        *) fail "E($named): an internal-only train must read as no drift: $json" ;;
    esac
    case "$json" in
        *'"internal_excluded":2'*) ;;
        *) fail "E($named): internal_excluded must count the two internal/ files: $json" ;;
    esac
done

# --- F. the filter drops the root entry and nothing else --------------
mkdir -p src/docs/internal src/internalx
echo "public" >src/docs/internal/kept.md
echo "public" >src/internalx/kept.md
(cd src && git add -A && git commit -qm "lookalikes") || exit 1
names=$(cd "$tmp/src" && bash -c '. infra/mirror-drift-lib.sh
t=$(public_tree HEAD) || exit 2
git ls-tree -r --name-only "$t"' 2>&1)
rc=$?
[ "$rc" -eq 0 ] || fail "F: public_tree failed (exit $rc): $names"
case "$names" in *internal/operating.md*) fail "F: the public tree still holds internal/: $names" ;; esac
case "$names" in *docs/internal/kept.md*) ;; *) fail "F: docs/internal/ is public and was dropped: $names" ;; esac
case "$names" in *internalx/kept.md*) ;; *) fail "F: internalx/ is public and was dropped: $names" ;; esac
same=$(cd "$tmp/src" && bash -c '. infra/mirror-drift-lib.sh; public_tree "$1"' _ "$MERGE" 2>&1)
[ "$same" = "$PUBLISHED_TREE" ] || fail "F: a commit with no internal/ must publish its own tree ($PUBLISHED_TREE), got: $same"

# --- D. the script refuses when the classifier is absent -------------
(cd src && rm infra/mirror-drift-lib.sh) || exit 1
out=$(measure)
rc=$?
[ "$rc" -eq 2 ] || fail "D: a tree with no classifier measured anyway (exit $rc, expected 2)
$out"
case "$out" in *REFUSED*) ;; *) fail "D: the missing classifier was not a loud refusal
$out" ;; esac

if [ "$fails" -gt 0 ]; then
    echo "the-mirrors-own-merge-is-not-foreign-work: $fails check(s) failed" >&2
    exit 1
fi
echo "the-mirrors-own-merge-is-not-foreign-work: self-test ok — the mirror's own snapshot+merge does not block (2 behind, 0 foreign), a commit that changed FOREIGN.md blocks and names it, an unresolvable ref refuses instead of answering, a filtered snapshot of a source holding internal/ is bookkeeping by name and by walk with an internal-only train reading no drift, public_tree drops only the root internal entry, and a tree with no classifier refuses instead of measuring"
exit 0
