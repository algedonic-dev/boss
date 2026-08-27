#!/usr/bin/env bash
# No two migrations may share a prefix.
#
# THE CONVENTION THIS BACKS. A NEW migration takes a UTC
# `YYYYMMDDHHMM-` prefix — `infra/postgres/migrate.sh`'s header states
# it, and `202608241600-migration-prefixes-are-timestamps.sql` is the
# first one and explains itself. Legacy `NNN-` files keep their numbers
# forever; migrate.sh checksums every applied file, and renaming one is
# the 2026-08-13 outage. The two schemes coexist because the apply
# order is a NUMERIC sort on the prefix and every timestamp exceeds
# every legacy number.
#
# WHY IT CHANGED, AND WHAT IT LEFT FOR THIS LINT. `NNN-` was a shared
# counter. The schema directory has no manifest (CLAUDE.md 9a records
# why it was deleted), so adding a migration touched no shared line —
# which removed the contended LINE and left a contended NUMBER. Two
# branches each picked "the next free number" against their own tree,
# both were right, and the collision existed only once a train
# assembled them. It happened on 2026-08-17, and again on 2026-08-23
# at a cost of a red CI run, a cancelled train and nine held cars.
#
# A timestamp is allocated from nothing, so that whole class is gone.
# THE PREFIX IS THE DEFENCE; THIS LINT IS THE BACKSTOP. Exactly one
# case survives it — two authors who write a migration in the same UTC
# minute — plus any duplicate already sitting in the frozen `NNN-`
# range. Rare, cheap to catch, expensive to miss, so the check stays.
#
# WHY IT MATTERS MORE THAN IT LOOKS. With duplicate prefixes the apply
# order stops being the prefix and becomes the rest of the filename —
# so which of two colliding migrations runs first depends on their
# titles. And once applied, a migration is history: it is
# checksum-guarded on every live database (docs/design/schema-
# migrations.md), so it cannot be renamed afterwards. The window to fix
# a collision closes when it first applies, which is why this must fail
# before a train, not after.
#
# WHAT IT DOES NOT DO. It reads one tree, so it cannot see a
# same-minute twin sitting on another branch. It catches the collision
# at the point the second one meets main — on that car's own gate run
# and again on the train — which is early enough to rename, and is the
# whole ask.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
DIR="infra/postgres/schema"
[ -d "$DIR" ] || { echo "migration-numbers-unique: $DIR not found" >&2; exit 1; }

prefixed=$(find "$DIR" -maxdepth 1 -name '[0-9]*-*.sql' -printf '%f\n' 2>/dev/null \
    || ls -1 "$DIR" | grep -E '^[0-9]+-.*\.sql$')

count=$(printf '%s\n' "$prefixed" | grep -c . || true)
if [ "$count" -lt 10 ]; then
    echo "migration-numbers-unique: only found $count prefixed migrations in $DIR —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 1
fi

# One rule for both schemes: the prefix is the leading run of digits,
# whether that is `142` or `202608241600`.
dupes=$(printf '%s\n' "$prefixed" | sed -E 's/^([0-9]+)-.*/\1/' | sort | uniq -d)

if [ -n "$dupes" ]; then
    echo "migration-numbers-unique: two or more migrations share a prefix." >&2
    for n in $dupes; do
        echo "  ${n}:" >&2
        printf '%s\n' "$prefixed" | grep -E "^${n}-" | sed 's/^/    /' >&2
    done
    echo "" >&2
    echo "  Rename the one that has NOT been applied yet — and do it now." >&2
    echo "" >&2
    echo "  New migrations take a UTC YYYYMMDDHHMM- prefix, so if you collided" >&2
    echo "  it is because someone else wrote one in the same minute: take a" >&2
    echo "  fresh stamp with \`date -u +%Y%m%d%H%M\` and rename your file." >&2
    echo "  Legacy NNN- files are frozen at the number they were applied with;" >&2
    echo "  a collision between two of those means renumbering the unapplied" >&2
    echo "  one, not picking a new counter value for both." >&2
    echo "" >&2
    echo "  Applied migrations are checksum-guarded history and cannot be" >&2
    echo "  renamed, so the window closes the first time this reaches a live" >&2
    echo "  database. With duplicate prefixes the apply order also stops being" >&2
    echo "  the prefix and becomes the rest of the filename." >&2
    exit 1
fi

echo "migration-numbers-unique: $count migrations, no shared prefixes"
