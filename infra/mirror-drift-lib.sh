#!/usr/bin/env bash
# mirror-drift-lib.sh — decide which commits a public mirror carries that
# the source ref does not are FOREIGN WORK, and which are the publish
# protocol's own bookkeeping.
#
# WHY COMMIT IDENTITY IS THE WRONG COMPARISON. The question worth
# blocking on is "would publishing a snapshot over the mirror lose file
# content that exists nowhere else?" Commit identity cannot answer it,
# because the publish protocol manufactures commits the source ref can
# never contain: the snapshot is `git commit-tree <source tree>` on top
# of the mirror's main, and merging the PR adds GitHub's merge commit
# above that. So `rev-list --count SOURCE..TARGET` is ≥ 1 from the first
# successful publish onward, forever — on 2026-09-10 it stood at 1 and
# refused a 238-commit / 763-file publish, and the one "absent" commit
# was b178a534 "Publish: forge main through 2026-08-27 (#129) (#236)"
# whose tree, 02a76d57, IS the tree of forge main's b78ae6d2. A check
# whose condition its own success makes permanently true is not a check.
#
# WHAT IS COMPARED INSTEAD. Trees. A mirror-only commit is bookkeeping
# when it introduced no content the source history does not already
# hold, which is true in exactly two shapes:
#
#   1. its tree is one the source history holds — the snapshot commit of
#      any past publish, and any commit that merely re-points to such a
#      tree — or (1b) the PUBLIC tree of a commit the source history
#      holds, which is what every snapshot carries once that commit's
#      root has an `internal` entry (public_tree, below); or
#   2. its tree equals a PARENT's tree — it changed no file at all: a
#      merge taking one side wholesale, or a re-commit of what we
#      published. (If that parent is itself foreign, the parent is in
#      this same set and gets named on its own line, so nothing hides.)
#
# Anything else changed a file relative to what was published, which is
# the work the original check was written to protect, and it blocks.
#
# THE BOUNDARY, NAMED. Shape 1 accepts a mirror commit that rolls the
# mirror BACK to an older source tree. That is deliberate: an old source
# tree holds no content the source history lacks, so a publish over it
# loses nothing unique — which is the question being asked. It is not a
# claim that the mirror is in a state anyone intended.
#
# A QUESTION THAT CANNOT BE ANSWERED IS A REFUSAL, NOT AN ANSWER. Every
# git invocation here is checked and a failure returns 2 with nothing on
# stdout. The script sourcing this file once reported `has_drift:false`
# — "the mirror is current" — from a `|| echo 0` while the mirror was
# 211 commits behind; a wrong target must error, not answer (CLAUDE.md
# §Doors).
#
# WHAT LEAVES IS THE PUBLIC TREE (design f7087b8c D1, backlog e7f062d3,
# David 2026-11-16: "split it"). How Algedonic runs its own estate lives
# in the forge under ONE top-level directory, `internal/`, and the
# snapshot carries `public_tree <source commit>` — that commit's tree
# with the root entry `internal` dropped. The chain proves itself by
# tree identity in three places (the snapshot, --merge's check, shape 1
# below), so the filter is this one function and every one of those
# readers calls it: a filter applied at `commit-tree` alone would leave
# the next measure reading the last publish as foreign work — a tree no
# source commit ever held, the 2026-09-10 permanent block again — and
# --merge refusing every PR. Shape 1 therefore also accepts the PUBLIC
# tree of a source commit (1b). The secrets scan is not a tree-identity
# reader: it stays over the FULL source tree, a superset of what leaves.
#
# Sourced by infra/prep-github-publish.sh and infra/forge/publish-github-pr.sh;
# self-tested against throwaway local repositories by
# infra/lint/the-mirrors-own-merge-is-not-foreign-work.sh.

# The root entry the publish snapshot omits — spelled ONCE. One
# top-level directory is what makes the filter a root-entry drop rather
# than a path walk (design f7087b8c D1).
PUBLIC_TREE_EXCLUDES=internal

# public_tree <commit>
#
# Prints the tree <commit> publishes: its root tree with the top-level
# entry $PUBLIC_TREE_EXCLUDES removed — `git ls-tree -z` of the root, the
# one entry dropped, `git mktree -z`. A pure function of the commit: the
# same commit gives the same tree on every host, and no index or
# worktree is touched. A commit whose root has no such entry publishes
# its own tree unchanged, and that is answered without writing anything.
# Exit 0 = printed; exit 2 = could not compute, nothing on stdout.
public_tree() {
    local commit="$1" tree present out
    tree=$(git rev-parse --verify --quiet "$commit^{tree}") || {
        echo "mirror-drift: cannot read the tree of $commit" >&2
        return 2
    }
    present=$(git ls-tree --full-tree --name-only "$tree" -- "$PUBLIC_TREE_EXCLUDES") || {
        echo "mirror-drift: cannot list the root of $commit (tree $tree)" >&2
        return 2
    }
    if [ -z "$present" ]; then
        printf '%s\n' "$tree"
        return 0
    fi
    # Both ends' exit statuses are read, whatever the caller's pipefail:
    # a listing that failed half way must not mktree into a smaller tree.
    out=$(
        git ls-tree --full-tree -z "$tree" | while IFS= read -r -d '' entry; do
            [ "${entry#*$'\t'}" = "$PUBLIC_TREE_EXCLUDES" ] || printf '%s\0' "$entry"
        done | git mktree -z
        st=("${PIPESTATUS[@]}")
        [ "${st[0]}" = 0 ] && [ "${st[2]}" = 0 ]
    ) && [ -n "$out" ] || {
        echo "mirror-drift: cannot build the public tree of $commit (tree $tree)" >&2
        return 2
    }
    printf '%s\n' "$out"
}

# public_tree_excluded <commit>
#
# Prints, one per line, every path under $PUBLIC_TREE_EXCLUDES in
# <commit>'s tree — what the snapshot omits. Both measures report the
# count as `internal_excluded`, so what stays behind is a number on the
# record rather than an assumption (design f7087b8c D1). Nothing printed
# means nothing is excluded. Exit 2 = could not list.
public_tree_excluded() {
    git ls-tree --full-tree -r --name-only "$1" -- "$PUBLIC_TREE_EXCLUDES" || {
        echo "mirror-drift: cannot list what $1 holds under $PUBLIC_TREE_EXCLUDES/" >&2
        return 2
    }
}

# foreign_mirror_commits <source_ref> <mirror_ref>
#
# Prints one TAB-separated line per foreign mirror commit:
#   <short sha><TAB><subject><TAB><space-separated files it changed>
# Exit 0 = classified; an empty stdout means every mirror-only commit is
#          bookkeeping.
# Exit 2 = could not classify. Nothing on stdout, the reason on stderr.
foreign_mirror_commits() {
    local source_ref="$1" mirror_ref="$2"
    local only source_trees commit tree parents parent parent_tree
    local subject files benign
    # Shape 1b's cache, read by _mirror_public_tree_of_source through
    # bash's dynamic scope: built at most once per call, and only when a
    # mirror commit fails every cheaper shape.
    local public_trees="" public_trees_read=0

    only=$(git rev-list "$source_ref".."$mirror_ref") || {
        echo "mirror-drift: git rev-list $source_ref..$mirror_ref failed" >&2
        return 2
    }
    [ -n "$only" ] || return 0

    # Every tree the source history has ever held. One walk, reused for
    # each mirror-only commit (there are normally one or two).
    source_trees=$(git log --format=%T "$source_ref") || {
        echo "mirror-drift: cannot list the trees of $source_ref" >&2
        return 2
    }
    [ -n "$source_trees" ] || {
        echo "mirror-drift: $source_ref yielded no trees; refusing to call the mirror clean on an empty history" >&2
        return 2
    }

    while read -r commit; do
        [ -n "$commit" ] || continue
        tree=$(git rev-parse --verify --quiet "$commit^{tree}") || {
            echo "mirror-drift: cannot read the tree of mirror commit $commit" >&2
            return 2
        }

        # Shape 1: content the source history already holds.
        if grep -qxF "$tree" <<<"$source_trees"; then
            continue
        fi

        # Shape 2: changed no file against any parent.
        parents=$(git rev-list --parents -n 1 "$commit") || {
            echo "mirror-drift: cannot read the parents of mirror commit $commit" >&2
            return 2
        }
        benign=0
        for parent in ${parents#* }; do
            [ "$parent" = "$parents" ] && break # a root commit: no parents
            parent_tree=$(git rev-parse --verify --quiet "$parent^{tree}") || {
                echo "mirror-drift: cannot read the tree of $parent, parent of $commit" >&2
                return 2
            }
            if [ "$parent_tree" = "$tree" ]; then
                benign=1
                break
            fi
        done
        [ "$benign" -eq 1 ] && continue

        # Shape 1b: the public tree of a source commit. Asked last, as
        # the one shape that costs a tree build per candidate.
        _mirror_public_tree_of_source "$source_ref" "$commit" "$tree"
        case $? in
            0) continue ;;
            1) ;;
            *) return 2 ;;
        esac

        # Foreign: name it and the files it changed. Against the first
        # parent for a child commit, against the empty tree for a root —
        # either way, the files this commit put on the mirror.
        subject=$(git log -1 --format=%s "$commit") || {
            echo "mirror-drift: cannot read the subject of $commit" >&2
            return 2
        }
        files=$(git diff-tree -r -m --root --no-commit-id --name-only "$commit") || {
            echo "mirror-drift: cannot diff mirror commit $commit" >&2
            return 2
        }
        files=$(printf '%s\n' "$files" | sort -u | grep . | tr '\n' ' ')
        printf '%s\t%s\t%s\n' \
            "$(git rev-parse --short "$commit")" "$subject" "${files% }"
    done <<<"$only"

    return 0
}

# _mirror_public_tree_of_source <source_ref> <mirror commit> <its tree>
#
# Shape 1b of foreign_mirror_commits: is <its tree> the public tree of a
# commit the source history holds? Exit 0 = yes, 1 = no, 2 = could not
# tell (the reason on stderr).
#
# Two ways to find the commit, cheapest first, and both end in the same
# tree comparison, so neither can turn foreign content into bookkeeping:
#
#   a. the commit the snapshot NAMES. publish-github-pr.sh writes
#      "Snapshot of forge main at <source sha>" into every snapshot body.
#      The name only picks the candidate: it must be a commit on the
#      source history, and its public tree must equal this tree. A
#      message naming anything else just fails over to (b).
#   b. every source commit whose root holds an `internal` entry, newest
#      first, its public tree computed once per call. Without (a) this
#      walk would run on every measure and grow with every train; with
#      it, only a mirror commit the verb did not make pays for it.
#
# A source commit with no such entry needs no 1b: its public tree IS its
# tree, and shape 1 has already compared that.
_mirror_public_tree_of_source() {
    local source_ref="$1" commit="$2" tree="$3" body named pt listing type c
    body=$(git log -1 --format=%B "$commit") || {
        echo "mirror-drift: cannot read the message of mirror commit $commit" >&2
        return 2
    }
    named=$(grep -m1 -oE 'forge main at [0-9a-f]{40}' <<<"$body") || named=""
    named="${named##* }"
    if [ -n "$named" ] && git cat-file -e "$named^{commit}" 2>/dev/null &&
        git merge-base --is-ancestor "$named" "$source_ref" 2>/dev/null; then
        pt=$(public_tree "$named") || return 2
        [ "$pt" = "$tree" ] && return 0
    fi

    if [ "$public_trees_read" = 0 ]; then
        # One cat-file for the whole history: `<tree>:internal <commit>`
        # answers `<type> <commit>` when the entry exists and
        # `<tree>:internal missing` when it does not.
        listing=$(
            git log --format="%T:$PUBLIC_TREE_EXCLUDES %H" "$source_ref" |
                git cat-file --batch-check='%(objecttype) %(rest)'
            st=("${PIPESTATUS[@]}")
            [ "${st[0]}" = 0 ] && [ "${st[1]}" = 0 ]
        ) || {
            echo "mirror-drift: cannot list which commits of $source_ref hold $PUBLIC_TREE_EXCLUDES/" >&2
            return 2
        }
        while read -r type c; do
            [ "$c" = missing ] && continue
            [ -n "$c" ] || continue
            pt=$(public_tree "$c") || return 2
            public_trees+="$pt"$'\n'
        done <<<"$listing"
        public_trees_read=1
    fi
    grep -qxF "$tree" <<<"$public_trees" && return 0
    return 1
}
