#!/usr/bin/env bash
# migrate.sh — the only path schema takes into a database.
#
# The ordered migration list IS schema/*.sql, sorted by its numeric
# prefix. NEW migrations take a UTC `YYYYMMDDHHMMSS-` prefix, taken
# with `date -u +%Y%m%d%H%M%S` when you write the file; the legacy
# `NNN-` and minute-resolution files keep theirs forever (renaming an
# applied migration changes its checksum, which is the 2026-08-13
# outage). All of them sort correctly together because every timestamp
# exceeds every legacy number under `sort -t- -k1,1n`. A timestamp is
# not allocated from a shared counter, so two branches cannot collide
# on one — which the `NNN-` scheme could not promise and twice did
# not. SECONDS, not minutes: two builders took `202609082130` in the
# same minute on 2026-09-08 and a train refused the assembly
# (bc7cac00). The name was the last contended resource left.
# Files not yet recorded in schema_migrations are applied in order, each
# in one transaction WITH its bookkeeping row — so a re-run never
# re-applies, and a failed migration leaves nothing behind. The one
# exception is a file whose FIRST LINE is `-- migrate: no-transaction`:
# it runs alone, outside a transaction, and is recorded after it
# succeeds (a CONCURRENTLY index build cannot run inside one). A schema
# change is a NEW file, never an edit to an applied one (expand/contract:
# see docs/design/schema-migrations.md). Dropping a file in the directory
# is the whole act of adding a migration; there is no list to update.
#
#   ./migrate.sh                          # apply what's pending (psql from env)
#   ./migrate.sh -- psql -h db -U boss -d boss
#   ./migrate.sh --without ledger         # skip matching entries (not recorded)
#   ./migrate.sh --phase boot             # defer byte-pinned optional indexes
#   ./migrate.sh --phase post-start       # retry only declared optional indexes
#   ./migrate.sh --phase post-start --pending # 75 named debt, 0 when paid
#   ./migrate.sh --baseline               # record everything, run nothing
#   ./migrate.sh --baseline -- sudo -n -u postgres psql -d boss
#
# Everything after `--` is the psql command to run (default: `psql`,
# configured by the usual PG* env vars); migrate.sh appends its own
# flags, and streams SQL over stdin so the command never needs to read
# the repo's files itself (sudo -u postgres across a 0750 home dir).
#
# --baseline exists for databases that predate the runner: their tables
# already exist, so every current migration is recorded as applied
# without being run. Needed exactly once per pre-existing deployment.
# The default phase is all, preserving offline/restore behavior. BOOT
# validates every applied checksum but never records deferred SQL as done.
# A database that visibly predates the runner (core tables present,
# nothing recorded) is refused rather than re-applied from scratch.
#
# Contract pinned by crates/core/boss-testing/tests/migrate_sh.rs.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/schema" && pwd)"
# THE MIGRATION ORDER IS THE DIRECTORY, NUMERICALLY SORTED. There is no
# manifest file, deliberately.
#
# There used to be one, and adding a migration meant appending a line to
# its tail. That made the tail a contended line: any two cars carrying a
# migration at the same time conflicted there on merge. It cost four
# re-rails on 2026-08-13 and stranded two more cars on 2026-08-14 ("left
# for the next train"), which is more disruption than the file ever
# prevented — it held no information the directory listing did not
# already carry, in the same order.
#
# Numeric sort on the `NNN-` prefix, not lexical: `100-` must land after
# `20-`, and a plain `sort` puts it before.
migration_order() {
    find "$DIR" -maxdepth 1 -name '*.sql' -type f -exec basename {} \; \
        | sort -t- -k1,1n
}

BASELINE=false
PHASE=all
PENDING=false
WITHOUT=()
PSQL=(psql)

while [ $# -gt 0 ]; do
    case "$1" in
        --baseline) BASELINE=true; shift ;;
        --phase) shift; PHASE="${1:?--phase needs all, boot or post-start}"; shift ;;
        --pending) PENDING=true; shift ;;
        --without) shift; WITHOUT+=("${1:?--without needs a module name}"); shift ;;
        --) shift; [ $# -gt 0 ] && PSQL=("$@"); break ;;
        *) echo "migrate.sh: unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [ ! -d "$DIR" ]; then
    echo "migrate.sh: schema directory not found at $DIR" >&2
    exit 1
fi
if [ -z "$(migration_order)" ]; then
    echo "migrate.sh: no .sql migrations found in $DIR" >&2
    exit 1
fi

fail() { echo "migrate.sh: $*" >&2; exit 1; }

case "$PHASE" in all|boot|post-start) ;; *) fail "unknown migration phase: $PHASE" ;; esac
if $PENDING && { $BASELINE || [ "$PHASE" != post-start ]; }; then
    fail "--pending requires --phase post-start without --baseline"
fi
# Phase declarations bind immutable SQL bytes; they do not rewrite a
# recorded checksum or turn arbitrary ordinary schema into maintenance.
declare -A PHASES=()
PHASE_DIR="$DIR/../migration-phases"
if [ -d "$PHASE_DIR" ]; then
    shopt -s nullglob
    for declaration in "$PHASE_DIR"/*; do
        [ -f "$declaration" ] && [ ! -L "$declaration" ] || fail "not a regular phase declaration: $declaration"
        entry="${declaration##*/}"
        [[ "$entry" =~ ^[0-9]+-[a-z0-9-]+\.sql\.phase$ ]] || fail "unknown phase declaration: $entry"
        entry="${entry%.phase}"
        IFS=' ' read -r phase expected extra < "$declaration" || fail "unreadable phase declaration: $entry"
        [ -z "${extra:-}" ] && [ "$(wc -l < "$declaration")" -eq 1 ] || fail "malformed phase declaration: $entry"
        case "$phase" in boot-wait|post-start) ;; *) fail "unknown declared phase for $entry: $phase" ;; esac
        [[ "$expected" =~ ^[0-9a-f]{64}$ ]] || fail "invalid declared checksum: $entry"
        [ -f "$DIR/$entry" ] && [ ! -L "$DIR/$entry" ] || fail "phase names missing SQL: $entry"
        [ "$(head -n 1 "$DIR/$entry")" = '-- migrate: no-transaction' ] || fail "phase requires no-transaction SQL: $entry"
        [ "$(sha256sum < "$DIR/$entry" | cut -d' ' -f1)" = "$expected" ] || fail "phase checksum drift: $entry"
        PHASES[$entry]="$phase"
    done
    shopt -u nullglob
fi

# TWO RUNNERS, ONE LEDGER. Two pods booting together race this script
# — named as RollingUpdate blocker #2 in boss.yaml's strategy comment,
# and the expand half of getting off Recreate. The WHOLE RUN holds a
# session advisory lock on a dedicated connection, taken before the
# first read: the losing run blocks on the SELECT below, and when it
# proceeds it computes its pending set against the winner's COMMITTED
# bookkeeping and applies nothing. Serializing at any finer grain
# (per-migration) would let the loser re-apply files it listed as
# pending before the winner finished.
#
# The lock tag includes the database OID, so TestDb's per-test scratch
# databases never serialize on each other — only real contenders for
# one schema do. The lock dies with this process's connection; there
# is no unlock to forget. The key is an arbitrary fixed 64-bit id
# unique to this runner.
MIG_LOCK_KEY=477201126
# Optional indexes cannot own BOOT's migration lock while waiting for
# an old snapshot. They serialize with each other, on this database,
# and cannot execute ordinary schema or bypass its checksum checks.
[ "$PHASE" != post-start ] || MIG_LOCK_KEY=477201127
coproc MIGLOCK { "${PSQL[@]}" -X -q -A -t; }
# END THE LOCK SESSION BEFORE THIS SCRIPT EXITS, on every path (backlog
# f93bdee4). "The lock dies with this process's connection" was true
# and incomplete: nothing ended that connection, so the session outlived
# the script by however long psql takes to see EOF and hang up — and,
# orphaned, it outlived it as a process too. Under post-start-bound.py,
# which owns every descendant until reaped, that is a descendant left
# behind, and it refused every pass from 2026-11-01 to 2026-11-09
# (11,299 packets, one a minute, each one "no pending entries" and exit
# 125). Closing our end is the hang-up psql reads; waiting for it is
# what makes the session end inside this run rather than beside it.
# Pinned by migrate_sh.rs a_post_start_pass_ends_its_lock_session_before_it_exits.
# Copied now: bash unsets both when it reaps a coproc that already died.
MIGLOCK_SESSION=${MIGLOCK_PID:-}
MIGLOCK_IN=${MIGLOCK[1]:-}
end_lock_session() {
    local rc=$?
    [ -z "$MIGLOCK_IN" ] || { exec {MIGLOCK_IN}>&-; } 2>/dev/null || :
    [ -z "$MIGLOCK_SESSION" ] || wait "$MIGLOCK_SESSION" 2>/dev/null || :
    exit "$rc"
}
trap end_lock_session EXIT
printf 'SELECT pg_advisory_lock(%d);\n' "$MIG_LOCK_KEY" >&"${MIGLOCK[1]}" \
    || fail "could not reach the database to take the migration lock"
IFS= read -r _ <&"${MIGLOCK[0]}" \
    || fail "taking the migration advisory lock failed (is the database reachable?)"

# One shape for bookkeeping statements; migration files themselves go
# through apply() below so BEGIN/…/COMMIT arrives as a single stream.
q() { "${PSQL[@]}" -X -q -A -t -v ON_ERROR_STOP=1 -c "$1"; }

q "CREATE TABLE IF NOT EXISTS schema_migrations (
    id TEXT PRIMARY KEY,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
)" >/dev/null

declare -A RECORDED=()
if recorded_rows="$(q "SELECT id || '|' || checksum FROM schema_migrations")"; then
    :
else
    ledger_status=$?
    # Partial rows are evidence, never an applied-ledger answer.
    [ -z "$recorded_rows" ] || printf '%s\n' "$recorded_rows" >&2
    fail "schema_migrations ledger read failed (exit $ledger_status); partial rows cannot establish paid debt"
fi
while IFS='|' read -r id sum; do
    [ -n "$id" ] && RECORDED[$id]=$sum
done <<< "$recorded_rows"

# The entries this run covers, in migration order, minus --without skips.
ENTRIES=()
while IFS= read -r f; do
    [ -z "$f" ] && continue
    skip=
    for w in "${WITHOUT[@]}"; do
        case "$f" in *"$w"*) skip=1; break ;; esac
    done
    [ -n "$skip" ] && continue
    ENTRIES+=("$f")
done < <(migration_order)

# Pass 1 — validate before touching anything. An applied migration whose
# file has changed is history being rewritten: refuse, by name, before
# applying anything else. (Changes go in a new migration file.)
for f in "${ENTRIES[@]}"; do
    if [ -n "${RECORDED[$f]+x}" ]; then
        sum="$(sha256sum < "$DIR/$f" | cut -d' ' -f1)"
        [ "$sum" = "${RECORDED[$f]}" ] || fail \
            "$f changed after it was applied (recorded ${RECORDED[$f]:0:12}…, on disk ${sum:0:12}…) — applied migrations are history; put the change in a new migration file"
    fi
done

# Guard — a database with core tables but an empty ledger of migrations
# predates the runner. Re-applying every migration against it would
# duplicate seeds at best; the honest move is a one-time --baseline.
if ! $BASELINE && [ "${#RECORDED[@]}" -eq 0 ]; then
    present="$(q "SELECT to_regclass('audit_log') IS NOT NULL")"
    [ "$present" = "t" ] && fail \
        "this database has BOSS tables but no recorded migrations — if it predates the runner, adopt it once with: migrate.sh --baseline"
fi

applied=0
recorded_already=0
deferred=0
if [ "$PHASE" = post-start ]; then
    for f in "${ENTRIES[@]}"; do
        [ "${PHASES[$f]:-boot}" = post-start ] && continue
        [ -n "${RECORDED[$f]+x}" ] || fail "post-start requires recorded boot schema: $f"
    done
fi
if $PENDING; then
    pending=0
    for f in "${ENTRIES[@]}"; do
        [ "${PHASES[$f]:-boot}" = post-start ] || continue
        [ -z "${RECORDED[$f]+x}" ] || continue
        echo "pending $f (post-start; not recorded)"
        pending=$((pending + 1))
    done
    [ "$pending" -eq 0 ] || exit 75
    echo "migrate.sh: post-start debt paid — no pending entries"
    exit 0
fi
for f in "${ENTRIES[@]}"; do
    if [ -n "${RECORDED[$f]+x}" ]; then
        recorded_already=$((recorded_already + 1))
        continue
    fi
    entry_phase="${PHASES[$f]:-boot}"
    if [ "$PHASE" = boot ] && [ "$entry_phase" = post-start ]; then
        echo "deferred $f (post-start; not recorded — supervised maintenance still owed)"
        deferred=$((deferred + 1))
        continue
    fi
    if [ "$PHASE" = post-start ] && [ "$entry_phase" != post-start ]; then
        continue
    fi
    sum="$(sha256sum < "$DIR/$f" | cut -d' ' -f1)"
    if $BASELINE; then
        q "INSERT INTO schema_migrations (id, checksum) VALUES ('$f', '$sum')" >/dev/null
        echo "baselined $f"
    elif [ "$(head -n 1 "$DIR/$f")" = "-- migrate: no-transaction" ]; then
        # A file that cannot run in a transaction block (CREATE INDEX
        # CONCURRENTLY on a table that takes every write; design
        # c195e83e Q4) runs ALONE, each statement committing as psql
        # sends it, and is recorded only after the whole file succeeded.
        # A failure records nothing, so the next run tries it again —
        # which is why such a file must be safe to re-run from any point
        # it can stop at. TestDb reads the same first line.
        "${PSQL[@]}" -X -q -v ON_ERROR_STOP=1 < "$DIR/$f" \
            || fail "applying $f (no-transaction) failed — it is not recorded and runs again next time"
        q "INSERT INTO schema_migrations (id, checksum) VALUES ('$f', '$sum')" >/dev/null
        echo "applied $f (no-transaction)"
    else
        {
            echo "BEGIN;"
            # 033cb09a: even SET UNLOGGED on an already-unlogged table
            # waits for an exclusive lock. Bound EACH ordinary file's
            # wait, including a no-op ALTER in a later file or retry.
            # LOCAL ends at this transaction: no role/database/session policy,
            # and neither advisory-lock nor no-transaction sessions change.
            # This bounds lock waiting, not a statement's rewrite duration.
            echo "SET LOCAL lock_timeout = '5s';"
            cat "$DIR/$f"
            printf "\nINSERT INTO schema_migrations (id, checksum) VALUES ('%s', '%s');\nCOMMIT;\n" "$f" "$sum"
        } | "${PSQL[@]}" -X -q -v ON_ERROR_STOP=1 \
            || fail "applying $f failed — its transaction rolled back, nothing from it was kept"
        echo "applied $f"
    fi
    applied=$((applied + 1))
done

# A recorded id with no file on disk is drift worth hearing about, but
# not worth blocking on: it can only mean a migration was renamed or
# retired, and the database already holds its effect.
for id in "${!RECORDED[@]}"; do
    found=
    for f in "${ENTRIES[@]}"; do
        [ "$f" = "$id" ] && { found=1; break; }
    done
    # --without hides entries from this run on purpose; only warn when
    # the schema directory doesn't hold the file either.
    if [ -z "$found" ] && [ ! -f "$DIR/$id" ]; then
        echo "migrate.sh: warning: $id is recorded as applied but schema/$id no longer exists" >&2
    fi
done

verb=$($BASELINE && echo baselined || echo applied)
echo "migrate.sh: $verb $applied, already recorded $recorded_already, of ${#ENTRIES[@]} migrations; deferred $deferred"
