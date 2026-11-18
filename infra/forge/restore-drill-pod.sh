#!/bin/sh
# restore-drill-pod.sh — the `restore` container of the drill Job
# (infra/forge/restore-drill-job.yaml; the ops verb drill-gcs-restore,
# backlog 156eb6e5, design 3c475a19 car 4's risk section: "A backup
# nobody has restored is a belief").
#
# It runs AFTER the `fetch` initContainer, which is infra/forge/
# gcs-readback.py writing the newest boss-<stamp>.sql.gz it verified
# (pinned generation, sha256, gzip -t, pg_dump's completion trailer) to
# $DRILL_SCRATCH/dump.sql.gz on the scratch claim. Kubernetes starts
# this container only when that one exited 0.
#
# WHAT IT DOES, in order:
#   1. initdb a fresh cluster on the scratch claim and start it on a
#      unix socket only (listen_addresses empty: nothing can reach it);
#   2. reads the live system of record's role names, READ-ONLY, and
#      creates each as a NOLOGIN role here, so the dump's GRANT and
#      OWNER TO lines restore under ON_ERROR_STOP instead of erroring;
#   3. restores the dump into database `drill` with ON_ERROR_STOP=1,
#      printing a progress line every DRILL_PROGRESS_S (60 s) and ending
#      ITSELF at DRILL_RESTORE_DEADLINE_S (below);
#   4. counts audit_log, jobs and steps and reads max(audit_log.id) in
#      the scratch database, then the same four in the LIVE database,
#      and whether the live log still holds the dump's newest audit id;
#   5. prints ONE line, `restore-drill: counts {json}`, for the verb to
#      judge, carrying the restore's seconds and the bytes it restored.
#      It judges nothing itself: the verdict is the forge's
#      (drill-gcs-restore.sh --read), where a test can hold it.
#
# THE INNER DEADLINE AND THE PROGRESS (backlog a1cbd16c). The first real
# run, ops-request 24fd49e7, ran into the Job's activeDeadlineSeconds
# (2700 s) mid-restore. On DeadlineExceeded the Job controller DELETES
# the pod it was running, so the log went with it and the record cannot
# say how far the 7.5 GB dump got — at 18 minutes the log showed only
# step 1's and step 2's lines. So this container ends itself first: past
# DRILL_RESTORE_DEADLINE_S it stops the scratch server under psql and
# exits 1 with a line naming the deadline, and the pod ends Failed —
# which the controller KEEPS, log and all (backoffLimit 0, one attempt).
# The deadline is the manifest's, one definition the verb reads too.
# Every DRILL_PROGRESS_S it prints the elapsed seconds, the scratch
# database's pg_database_size and the statement psql is running (COPY
# <table>, CREATE INDEX …), so a read mid-run or after a failure says how
# far it got. The dump is plain SQL fed to psql, not a custom archive, so
# there is no pg_restore --verbose table list; the active statement is
# its cheap equivalent.
#
# THE LIVE SIDE IS READ-ONLY TWICE: every live session opens with
# PGOPTIONS default_transaction_read_only=on (set by the server at
# connection start, before any statement) AND runs `SET
# default_transaction_read_only = on` in its own -c first — the
# switch-instance-database / tenant-census shape. Its URL arrives from
# Secret boss-secrets key database-url as an environment variable and
# is never printed: a failed live read prints psql's stderr with any
# user:password@ in a URL masked.
#
# POSIX sh: the postgres:16-alpine image has no bash.
set -eu

SCRATCH="${DRILL_SCRATCH:-/scratch}"
DUMP="$SCRATCH/dump.sql.gz"
PGDATA_DIR="$SCRATCH/pgdata"
# The socket and the server log, in a directory the postgres user owns
# (the claim's root is root's, 0755).
SOCK="$SCRATCH/run"
# Who the scratch server runs as. initdb and pg_ctl refuse root, so the
# container starts as root (to own its directories) and drops to the
# image's postgres user for every server command. A test sets it empty.
AS="${DRILL_AS-su-exec postgres}"

fail() { echo "restore-drill: FAILED — $*" >&2; exit 1; }
say() { echo "restore-drill: $*"; }
masked() { sed -E 's#://[^@/ ]*@#://<masked>@#g' "$1" | tail -n 20 | cut -c1-400 >&2; }
num() { # <value> <what> — the value, or a failure naming what was not a number
    case "${1:-empty}" in
        empty | *[!0-9]*) fail "$2 read '${1:-}', not a whole number" ;;
    esac
    printf '%s' "$1"
}

INNER="${DRILL_RESTORE_DEADLINE_S:-}"
case "${INNER:-empty}" in
    empty | *[!0-9]*) fail "DRILL_RESTORE_DEADLINE_S is '${INNER}', not whole seconds — infra/forge/restore-drill-job.yaml declares it, and an unbounded restore is the one 24fd49e7 lost" ;;
esac
EVERY="${DRILL_PROGRESS_S:-60}"
case "${EVERY:-empty}" in
    empty | *[!0-9]* | 0) EVERY=60 ;;
esac

[ -s "$DUMP" ] || fail "the fetch leg left no dump at $DUMP"
[ -n "${LIVE_DATABASE_URL:-}" ] || fail 'LIVE_DATABASE_URL is not set — Secret boss-secrets key database-url did not reach this container'

# live <sql> <what> — one read-only session against the live system of
# record; the value bare (-At).
live() {
    PGOPTIONS='-c default_transaction_read_only=on' \
        psql "$LIVE_DATABASE_URL" -X -q -At -v ON_ERROR_STOP=1 \
        -c 'SET default_transaction_read_only = on' \
        -c 'SET statement_timeout = 600000' \
        -c "$1" 2> "$SCRATCH/live.err" \
        || { masked "$SCRATCH/live.err"; fail "the live read-only query for $2 failed"; }
}

# scratch <db> <sql> — one statement against the scratch server.
scratch() {
    $AS psql -h "$SOCK" -U boss -d "$1" -X -q -At -v ON_ERROR_STOP=1 -c "$2"
}

# --- 1. a fresh cluster on the scratch claim ---------------------------
mkdir -p "$PGDATA_DIR" "$SOCK"
[ -z "$AS" ] || chown postgres:postgres "$PGDATA_DIR" "$SOCK"
$AS initdb -D "$PGDATA_DIR" -U boss --auth=trust --encoding=UTF8 > "$SCRATCH/initdb.log" 2>&1 \
    || { tail -n 20 "$SCRATCH/initdb.log" >&2; fail 'initdb could not make a scratch cluster on the claim'; }
# fsync and full_page_writes off: this database is deleted with its
# claim, so a crash loses nothing the drill needs, and the restore runs
# at the disk's speed rather than its flush rate.
$AS pg_ctl -D "$PGDATA_DIR" -l "$SOCK/postgres.log" -w -t 300 \
    -o "-c listen_addresses= -c unix_socket_directories=$SOCK -c fsync=off -c full_page_writes=off -c synchronous_commit=off -c max_wal_size=8GB -c maintenance_work_mem=1GB" \
    start > "$SCRATCH/pg_ctl.log" 2>&1 \
    || { tail -n 20 "$SOCK/postgres.log" >&2; fail 'the scratch server did not start'; }
SCRATCH_VERSION="$(scratch postgres 'SHOW server_version')" || fail 'the scratch server did not answer'
scratch postgres 'CREATE DATABASE drill' > /dev/null || fail 'the scratch database could not be created'
say "scratch server $SCRATCH_VERSION started on a unix socket only, database drill created"

# --- 2. the live roles, so the dump's grants restore strictly -----------
LIVE_VERSION="$(live 'SHOW server_version' 'server_version')"
live "SELECT rolname FROM pg_roles WHERE rolname !~ '^pg_' ORDER BY 1" 'role names' > "$SCRATCH/roles"
roles=0
while IFS= read -r role; do
    [ -n "$role" ] || continue
    # :'r' is psql's own literal quoting, and %I the server's identifier
    # quoting, so no role name is ever spliced into SQL text here.
    printf '%s\n' "SELECT format('CREATE ROLE %I NOLOGIN', :'r') WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'r') \\gexec" \
        | $AS psql -h "$SOCK" -U boss -d postgres -X -q -v ON_ERROR_STOP=1 -v r="$role" > /dev/null \
        || fail "role $role could not be created in the scratch cluster"
    roles=$((roles + 1))
done < "$SCRATCH/roles"
say "live server $LIVE_VERSION; $roles live role name(s) created here NOLOGIN"

# --- 3. the restore, bounded and reporting ---------------------------------
# progress <elapsed> — one line: how long, how big, what psql is running.
# Each read is best-effort, and a failed one is SAID on the line ("an
# unread number of", "the statement could not be read") rather than
# stopping the restore it reports on — a report, never a guard.
progress() {
    if bytes="$(scratch drill 'SELECT pg_database_size(current_database())' 2> /dev/null)"; then
        case "${bytes:-empty}" in empty | *[!0-9]*) bytes='an unread number of' ;; esac
    else
        bytes='an unread number of'
    fi
    if stmt="$(scratch drill "SELECT left(regexp_replace(query, '[[:space:]]+', ' ', 'g'), 80) FROM pg_stat_activity WHERE datname = 'drill' AND state = 'active' AND pid <> pg_backend_pid() LIMIT 1" 2> /dev/null)"; then
        stmt="$(printf '%s' "$stmt" | tr -cd 'A-Za-z0-9_.,()* =-')"
    else
        stmt='the statement could not be read'
    fi
    say "progress: ${1}s of ${INNER}s, database drill holds $bytes bytes; running: ${stmt:-no statement reported}"
}

# No pipefail in POSIX sh, so gzip's own status is written beside the
# stream and read after it: a decompressor that died mid-stream would
# otherwise hand psql a clean-looking prefix. The pipeline runs in the
# background so this shell can report on it and bound it; psql's status
# lands in psql.rc, renamed into place, which is how the loop below
# knows it ended (a zombie still answers kill -0).
rm -f "$SCRATCH/psql.rc" "$SCRATCH/gzip.rc"
t0=$(date +%s)
(
    set +e
    { gzip -dc "$DUMP"; echo $? > "$SCRATCH/gzip.rc"; } \
        | $AS psql -h "$SOCK" -U boss -d drill -X -q -v ON_ERROR_STOP=1 > "$SCRATCH/restore.out" 2> "$SCRATCH/restore.err"
    echo $? > "$SCRATCH/psql.rc.tmp" && mv "$SCRATCH/psql.rc.tmp" "$SCRATCH/psql.rc"
) < /dev/null > /dev/null 2>&1 &
RPID=$!
say "restoring $(basename "$DUMP") into database drill: a progress line every ${EVERY}s; this container ends itself after ${INNER}s"
next=$EVERY
while [ ! -f "$SCRATCH/psql.rc" ]; do
    el=$(($(date +%s) - t0))
    if [ "$el" -ge "$INNER" ]; then
        progress "$el"
        # The server stopped under it, psql loses its connection and
        # gzip its reader; the container's exit ends whatever is left.
        $AS pg_ctl -D "$PGDATA_DIR" -m immediate stop > /dev/null 2>&1 || true
        kill "$RPID" 2> /dev/null || true
        fail "the restore exceeded its inner deadline after ${el}s (DRILL_RESTORE_DEADLINE_S=${INNER}); the last progress line above says how far it got"
    fi
    if [ "$el" -ge "$next" ]; then
        progress "$el"
        next=$((next + EVERY))
    fi
    sleep 1
done
wait "$RPID" 2> /dev/null || true
[ "$(cat "$SCRATCH/psql.rc")" = 0 ] \
    || { tail -n 20 "$SCRATCH/restore.err" | cut -c1-400 >&2; fail 'psql stopped on the first error restoring the dump (ON_ERROR_STOP)'; }
[ "$(cat "$SCRATCH/gzip.rc" 2>/dev/null)" = 0 ] || fail "gzip -dc exited '$(cat "$SCRATCH/gzip.rc" 2>/dev/null)' while streaming the dump"
RESTORE_S=$(($(date +%s) - t0))
BYTES="$(num "$(scratch drill 'SELECT pg_database_size(current_database())')" 'the restored database size')"
say "restored $(basename "$DUMP") into database drill in ${RESTORE_S}s, ${BYTES} bytes"

# --- 4. the four numbers, both sides -------------------------------------
D_AUDIT="$(num "$(scratch drill 'SELECT count(*) FROM audit_log')" 'the dump audit_log count')"
D_JOBS="$(num "$(scratch drill 'SELECT count(*) FROM jobs')" 'the dump jobs count')"
D_STEPS="$(num "$(scratch drill 'SELECT count(*) FROM steps')" 'the dump steps count')"
D_MAX="$(num "$(scratch drill 'SELECT coalesce(max(id), 0) FROM audit_log')" 'the dump newest audit id')"
L_AUDIT="$(num "$(live 'SELECT count(*) FROM audit_log' 'audit_log count')" 'the live audit_log count')"
L_JOBS="$(num "$(live 'SELECT count(*) FROM jobs' 'jobs count')" 'the live jobs count')"
L_STEPS="$(num "$(live 'SELECT count(*) FROM steps' 'steps count')" 'the live steps count')"
L_MAX="$(num "$(live 'SELECT coalesce(max(id), 0) FROM audit_log' 'newest audit id')" 'the live newest audit id')"
# D_MAX is a whole number (num above), so it rides in the SQL bare.
L_HAS="$(num "$(live "SELECT count(*) FROM audit_log WHERE id = $D_MAX" 'the dump newest audit id')" 'the live row count at the dump newest audit id')"
$AS pg_ctl -D "$PGDATA_DIR" -m fast stop > /dev/null 2>&1 || true

printf 'restore-drill: counts {"scratch_server_version":"%s","live_server_version":"%s","restore_seconds":%s,"restored_bytes":%s,"live_roles_created":%s,"dump":{"audit_log":%s,"jobs":%s,"steps":%s,"max_audit_id":%s},"live":{"audit_log":%s,"jobs":%s,"steps":%s,"max_audit_id":%s},"live_rows_at_dump_max_audit_id":%s}\n' \
    "$(printf '%s' "$SCRATCH_VERSION" | tr -cd 'A-Za-z0-9. ()-')" \
    "$(printf '%s' "$LIVE_VERSION" | tr -cd 'A-Za-z0-9. ()-')" \
    "$RESTORE_S" "$BYTES" "$roles" "$D_AUDIT" "$D_JOBS" "$D_STEPS" "$D_MAX" "$L_AUDIT" "$L_JOBS" "$L_STEPS" "$L_MAX" "$L_HAS"
