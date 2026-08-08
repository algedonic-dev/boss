#!/usr/bin/env bash
# migrate.sh — the only path schema takes into a database.
#
# schema/manifest.txt is the ordered migration list. Entries not yet
# recorded in schema_migrations are applied in order, each in one
# transaction WITH its bookkeeping row — so a re-run never re-applies,
# and a failed migration leaves nothing behind. A schema change is a
# NEW file appended to the manifest, never an edit to an applied one
# (expand/contract: see docs/design/schema-migrations.md).
#
#   ./migrate.sh                          # apply what's pending (psql from env)
#   ./migrate.sh -- psql -h db -U boss -d boss
#   ./migrate.sh --without ledger         # skip matching entries (not recorded)
#   ./migrate.sh --baseline               # record everything, run nothing
#   ./migrate.sh --baseline -- sudo -n -u postgres psql -d boss
#
# Everything after `--` is the psql command to run (default: `psql`,
# configured by the usual PG* env vars); migrate.sh appends its own
# flags, and streams SQL over stdin so the command never needs to read
# the repo's files itself (sudo -u postgres across a 0750 home dir).
#
# --baseline exists for databases that predate the runner: their tables
# already exist, so every current manifest entry is recorded as applied
# without being run. Needed exactly once per pre-existing deployment.
# A database that visibly predates the runner (core tables present,
# nothing recorded) is refused rather than re-applied from scratch.
#
# Contract pinned by crates/core/boss-testing/tests/migrate_sh.rs.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/schema" && pwd)"
MANIFEST="$DIR/manifest.txt"

BASELINE=false
WITHOUT=()
PSQL=(psql)

while [ $# -gt 0 ]; do
    case "$1" in
        --baseline) BASELINE=true; shift ;;
        --without) shift; WITHOUT+=("${1:?--without needs a module name}"); shift ;;
        --) shift; [ $# -gt 0 ] && PSQL=("$@"); break ;;
        *) echo "migrate.sh: unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [ ! -f "$MANIFEST" ]; then
    echo "migrate.sh: manifest not found at $MANIFEST" >&2
    exit 1
fi

fail() { echo "migrate.sh: $*" >&2; exit 1; }

# One shape for bookkeeping statements; migration files themselves go
# through apply() below so BEGIN/…/COMMIT arrives as a single stream.
q() { "${PSQL[@]}" -X -q -A -t -v ON_ERROR_STOP=1 -c "$1"; }

q "CREATE TABLE IF NOT EXISTS schema_migrations (
    id TEXT PRIMARY KEY,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
)" >/dev/null

declare -A RECORDED=()
while IFS='|' read -r id sum; do
    [ -n "$id" ] && RECORDED[$id]=$sum
done < <(q "SELECT id || '|' || checksum FROM schema_migrations")

# The entries this run covers, in manifest order, minus --without skips.
ENTRIES=()
while IFS= read -r f; do
    f="${f%%#*}"; f="$(echo "$f" | xargs)"    # strip comments + whitespace
    [ -z "$f" ] && continue
    skip=
    for w in "${WITHOUT[@]}"; do
        case "$f" in *"$w"*) skip=1; break ;; esac
    done
    [ -n "$skip" ] && continue
    [ -f "$DIR/$f" ] || fail "manifest names $f but schema/$f does not exist"
    ENTRIES+=("$f")
done < "$MANIFEST"

# Pass 1 — validate before touching anything. An applied migration whose
# file has changed is history being rewritten: refuse, by name, before
# applying anything else. (Changes go in a new manifest entry.)
for f in "${ENTRIES[@]}"; do
    if [ -n "${RECORDED[$f]+x}" ]; then
        sum="$(sha256sum < "$DIR/$f" | cut -d' ' -f1)"
        [ "$sum" = "${RECORDED[$f]}" ] || fail \
            "$f changed after it was applied (recorded ${RECORDED[$f]:0:12}…, on disk ${sum:0:12}…) — applied migrations are history; put the change in a new migration file"
    fi
done

# Guard — a database with core tables but an empty ledger of migrations
# predates the runner. Re-applying the full manifest against it would
# duplicate seeds at best; the honest move is a one-time --baseline.
if ! $BASELINE && [ "${#RECORDED[@]}" -eq 0 ]; then
    present="$(q "SELECT to_regclass('audit_log') IS NOT NULL")"
    [ "$present" = "t" ] && fail \
        "this database has BOSS tables but no recorded migrations — if it predates the runner, adopt it once with: migrate.sh --baseline"
fi

applied=0
recorded_already=0
for f in "${ENTRIES[@]}"; do
    if [ -n "${RECORDED[$f]+x}" ]; then
        recorded_already=$((recorded_already + 1))
        continue
    fi
    sum="$(sha256sum < "$DIR/$f" | cut -d' ' -f1)"
    if $BASELINE; then
        q "INSERT INTO schema_migrations (id, checksum) VALUES ('$f', '$sum')" >/dev/null
        echo "baselined $f"
    else
        {
            echo "BEGIN;"
            cat "$DIR/$f"
            printf "\nINSERT INTO schema_migrations (id, checksum) VALUES ('%s', '%s');\nCOMMIT;\n" "$f" "$sum"
        } | "${PSQL[@]}" -X -q -v ON_ERROR_STOP=1 \
            || fail "applying $f failed — its transaction rolled back, nothing from it was kept"
        echo "applied $f"
    fi
    applied=$((applied + 1))
done

# A recorded id the manifest no longer names is drift worth hearing
# about, but not worth blocking on: it can only mean an entry was
# renamed or retired, and the database already holds its effect.
for id in "${!RECORDED[@]}"; do
    found=
    for f in "${ENTRIES[@]}"; do
        [ "$f" = "$id" ] && { found=1; break; }
    done
    # --without hides entries from this run on purpose; only warn when
    # the full manifest doesn't know the id either.
    if [ -z "$found" ] && ! grep -qx "$id" "$MANIFEST"; then
        echo "migrate.sh: warning: $id is recorded as applied but the manifest no longer names it" >&2
    fi
done

verb=$($BASELINE && echo baselined || echo applied)
echo "migrate.sh: $verb $applied, already recorded $recorded_already, of ${#ENTRIES[@]} manifest entries"
