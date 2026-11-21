#!/usr/bin/env bash
# Reject citations of the roadmap file that lead nowhere.
#
# Two shapes, one reason.
#
# 1. `TODO #N` / `TODO.md #N` numeric citations in code comments.
#    These pointed at a numbered scheme the file dropped long ago, so
#    the numbers did not resolve to anything stable. A comment that
#    says "see TODO #46" is no more useful than one that names the
#    actual concern in plain English — the latter survives refactors,
#    the former rots.
#
# 2. ANY mention of TODO.md in a tracked file that is not Markdown
#    (backlog fce9a81e, 2026-11-17). TODO.md was cut to a pointer on
#    2026-11-16 (backlog d83cbd3f) and holds no sections at all, and
#    the repair run that cut it measured, with `git log --all -S`, that
#    some sections code cited — "Strip BatchEngine", "Hourly tick
#    granularity for the live sim", the audit_log "id-ordering race" —
#    were in no version of the file, while the login page told every
#    evaluator to "see TODO.md". Open work is a backlog-item packet in
#    BOSS, or a GitHub issue from outside; a limitation with neither is
#    stated where it lives.
#
#    Two forms pass. Markdown: README, CHANGELOG and SECURITY link the
#    pointer, which is what it is for. And a mention pinned to a commit,
#    `git show <sha>:TODO.md`, because that resolves to the same text
#    whatever the file says today.
#
# Allowed: GitHub issue refs in code comments (we explicitly don't
# block `#NNN` on its own, only the `TODO #NNN` form).
#
# Usage: ./infra/lint/no-todo-citation.sh
# Exit: 0 clean, 1 a citation, 3 the machine could not answer
# (lib/git-answer.sh).

set -euo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. infra/lint/lib/git-answer.sh || exit 3

NAME=no-todo-citation
pattern='(TODO|TODO\.md) #[0-9]+'

# Restrict to the source trees we actually maintain. Excludes target/
# build dirs, generated/, node_modules, and Markdown.
numbered=$(grep -rEon "$pattern" crates/ apps/ \
  --include="*.rs" \
  --include="*.ts" \
  --include="*.svelte" \
  2>/dev/null || true)

# Tracked files only, so node_modules and build output never answer.
# This file names the shape it refuses, so it is the one exclusion.
not_md=(-- . ':(exclude)*.md' ":(exclude)infra/lint/$NAME.sh")
status=0
tracked=$(git_answer "$NAME" 0 ls-files "${not_md[@]}") || status=$?
[ "$status" -eq 0 ] || exit "$status"
status=0
mentions=$(git_answer "$NAME" 0,1 grep -n -I -F 'TODO.md' "${not_md[@]}") || status=$?
case "$status" in 0|1) ;; *) exit "$status" ;; esac

# A line passes only when every mention on it is pinned to a commit.
pinned='[0-9a-f]{7,40}:TODO\.md'
pointer=""
if [ -n "$mentions" ]; then
  while IFS= read -r line; do
    rest=$line
    while [[ $rest =~ $pinned ]]; do
      rest=${rest/"${BASH_REMATCH[0]}"/}
    done
    if [[ $rest == *TODO.md* ]]; then
      pointer+="$line"$'\n'
    fi
  done <<< "$mentions"
fi
pointer=${pointer%$'\n'}

if [ -z "$numbered" ] && [ -z "$pointer" ]; then
  lint_scanned "$NAME" "$(grep -c . <<< "$tracked")" "tracked non-Markdown file(s)"
  echo "$NAME: clean"
  exit 0
fi

if [ -n "$numbered" ]; then
  echo "$NAME: $(wc -l <<< "$numbered" | tr -d ' ') comment(s) cite a numbered TODO that doesn't resolve:"
  echo ""
  echo "$numbered"
  echo ""
  echo "Replace each citation with a description of the actual concern."
fi
if [ -n "$pointer" ]; then
  echo "$NAME: $(wc -l <<< "$pointer" | tr -d ' ') line(s) outside Markdown cite TODO.md, which holds no sections since 2026-11-16 (backlog d83cbd3f):"
  echo ""
  echo "$pointer"
  echo ""
  echo "Name the BOSS packet that tracks the work, or state the limitation in place;"
  echo "a reader outside the instance is pointed at a GitHub issue (CONTRIBUTING.md)."
  echo "A section of the old roadmap is cited pinned: git show dab00c7bb6f3:TODO.md."
fi
exit 1
