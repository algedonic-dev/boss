#!/usr/bin/env bash
# No two migrations may share a number.
#
# WHY. The schema directory has no manifest — CLAUDE.md 9a records why
# it was deleted: "the ordered list is `schema/*.sql` sorted by the
# `NNN-` prefix, which every reader derives independently. Adding a
# migration is now dropping a file in a directory, touching no shared
# line at all."
#
# That removed the contended LINE and left a contended NUMBER. Two cars
# developed in parallel both take "the next one", neither touches a
# shared file, so nothing conflicts at merge and nothing complains. On
# 2026-08-17 exactly that happened: 142-dispatcher-rule-cluster-
# conformance landed on train 57 while 142-estate-subjects sat on
# another branch, and the collision was found by eye with the second
# already boarded.
#
# WHY IT MATTERS MORE THAN IT LOOKS. With duplicate prefixes the apply
# order stops being the number and becomes the rest of the filename —
# so which of two same-numbered migrations runs first depends on their
# titles. And once applied, a migration is history: it is
# checksum-guarded on every live database (docs/design/schema-
# migrations.md), so it cannot be renamed afterwards. The window to fix
# a collision closes when it first applies, which is why this must fail
# before a train, not after.
#
# WHAT IT DOES NOT DO. It cannot see other branches, so it will not
# stop two cars choosing the same number in parallel. It catches the
# collision at the point the second one meets main — on the car's own
# gate run and again on the train — which is early enough to renumber,
# and is the whole ask.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
DIR="infra/postgres/schema"
[ -d "$DIR" ] || { echo "migration-numbers-unique: $DIR not found" >&2; exit 1; }

numbered=$(find "$DIR" -maxdepth 1 -name '[0-9]*-*.sql' -printf '%f\n' 2>/dev/null \
    || ls -1 "$DIR" | grep -E '^[0-9]+-.*\.sql$')

count=$(printf '%s\n' "$numbered" | grep -c . || true)
if [ "$count" -lt 10 ]; then
    echo "migration-numbers-unique: only found $count numbered migrations in $DIR —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 1
fi

dupes=$(printf '%s\n' "$numbered" | sed -E 's/^([0-9]+)-.*/\1/' | sort | uniq -d)

if [ -n "$dupes" ]; then
    echo "migration-numbers-unique: two or more migrations share a number." >&2
    for n in $dupes; do
        echo "  ${n}:" >&2
        printf '%s\n' "$numbered" | grep -E "^${n}-" | sed 's/^/    /' >&2
    done
    echo "" >&2
    echo "  Renumber the one that has NOT been applied yet — and do it now." >&2
    echo "  Applied migrations are checksum-guarded history and cannot be" >&2
    echo "  renamed, so the window closes the first time this reaches a live" >&2
    echo "  database. With duplicate prefixes the apply order also stops being" >&2
    echo "  the number and becomes the rest of the filename." >&2
    exit 1
fi

echo "migration-numbers-unique: $count migrations, no shared numbers"
