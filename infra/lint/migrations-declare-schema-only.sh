#!/usr/bin/env bash
# migrations-declare-schema-only.sh — a migration newer than the cutover
# declares SCHEMA; a registry row lives in its platform bundle.
#
# WHY THIS EXISTS (backlog 393d3234, design 42277636, consolidation H4).
# MEASURED 2026-09-18 on origin/main 1a09f660: registry rows whose ONLY
# home was a migration — stations (7 files), step_plugins (7),
# cadence_rules (9), delivery_policy (2). A migration is the wrong home
# for a registry row: it runs once, so a fresh instance cannot
# re-declare the row without replaying history (the playground's fresh
# database inherited 31 example-tenant rule names that way the same day);
# nothing drift-checks it against the live row (protocol-drift compares
# only workflows); and every edit is a contended timestamped file.
# Dispatcher rules left that shape on 2026-09-11 (one file per rule
# under infra/dispatcher/rules/) and the Workflow bundle before them
# (infra/platform/workflows/, seeded insert-if-missing every start).
#
# THE RULE. Each registry in REGISTRY_TABLES has a bundle directory
# under infra/platform/ that the platform seed publishes at every start,
# insert-if-missing by (name, version). From the CUTOVER stamp on, a
# migration may not `INSERT INTO <that table>`: the row goes in the
# bundle, and a change to a live row is a version bump there. The
# historical inserts stay exactly as they are — migrations are
# append-only history (migrations-append-only.sh) — which is why the
# rule is dated rather than absolute. A file whose leading numeric
# prefix is NEWER than the cutover is checked; one older is history.
#
# THE STAMP is written ONCE, here, and it is the car's own write time
# (`date -u +%Y%m%d%H%M%S`, the fourteen-digit prefix a migration
# carries — a-migration-prefix-is-fourteen-digits). It is not "today":
# a stamp re-derived at run time would move, and a rule that moves is
# not a ratchet. Cars 2–4 of the same packet append their table to
# REGISTRY_TABLES and leave the stamp alone; a table added later gets
# its own dated entry beside it if its history must be exempt.
#
# WHAT IS READ. Every tracked infra/postgres/schema/*.sql, its SQL with
# `--` comments removed (a comment may SAY "INSERT INTO stations" to
# tell the story; several do). `UPDATE stations` is not refused: an
# in-place column fill on a row the bundle declares is still a
# migration's business when the column is new (119 and 138 are the
# precedent), and the equality pin in boss-jobs holds the bundle equal
# to whatever the migrations produce.
#
# THE SECOND RULE — a migration does not rewrite an EVENTED registry row
# (backlog fa25700f, 2026-09-27). The registries in EVENTED_TABLES
# change through a door that publishes the fact of the change: a Class
# through PUT /api/classes/{subject_kind}/{code} (class.updated) and
# POST …/retire (class.retired); a Workflow through its bundle and
# /api/workflows/{kind}/publish|retire (jobs.kind.published|retired); a
# SubjectKind's metadata through PATCH /api/subject-kinds/{kind}/metadata
# (subject_kind.updated).
# An `UPDATE` or `DELETE FROM` on one of them inside a migration is a
# change the audit log never hears of — the refurb asset phases were
# retired that way on 2026-09-24, and no rebuilder can reproduce it.
# This is unrelated to the cutover: the rule is not "rows live in a
# bundle" but "the log hears every change", so it holds for every
# migration. The ones that already did it ran on every instance and
# cannot be rewritten (migrations-append-only.sh), so each is named in
# EVENTED_ALLOWLIST with its reason — a named set, not a stamp, so the
# ratchet admits exactly them and nothing that comes later.
# dispatcher_rules is deliberately NOT in the list: its rows are
# refused to a migration by no-migration-writes-a-dispatcher-rule.sh,
# which admits a DELETE for the reason it states (b5f21e82), and a
# second copy of that rule here would be a fact living twice (§9a).
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read, no post-cutover migration inserts a row, and
#      no migration outside the allowlist rewrites an evented row
#   1  the tree was read and a violation was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/migrations-declare-schema-only.sh
#   infra/lint/migrations-declare-schema-only.sh --self-test
set -uo pipefail

NAME="migrations-declare-schema-only"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
cd "$LINT_DIR/../.." || exit 1

# shellcheck source=/dev/null
. "$LINT_DIR/lib/git-answer.sh" || exit 3

SCHEMA_DIR="infra/postgres/schema"

# The cutover: written 2026-09-18 11:21:34 UTC by the car that made
# infra/platform/stations/ the home of every platform station (backlog
# 393d3234, car 1 of 4). A migration with a newer prefix declares
# schema only.
CUTOVER="20260918112134"

# The registries whose rows now live in a bundle, space-separated.
# stations: car 1 (infra/platform/stations/). step_plugins: car 2
# (infra/platform/step-plugins/ — the newest insert it replaces,
# 202609082130-sign-off-plugin-v3.sql, is older than the cutover, so
# the stamp did not move). cadence_rules: car 3 (infra/platform/cadence/
# — newest insert 202609042110-a-lone-car-still-ships.sql, older than
# the cutover; the stamp stays). delivery_policy: car 4, the last
# (infra/platform/delivery-policy/ — newest insert
# 202609050500-the-ci-host-floor-is-forty.sql, older than the cutover;
# 20260918102236 is newer but only DROPs a column, which is schema).
REGISTRY_TABLES="stations step_plugins cadence_rules delivery_policy"

# The registries whose rows change through an evented door, so a
# migration may not UPDATE or DELETE FROM them at all (backlog
# fa25700f; THE SECOND RULE in the header). classes: boss-classes'
# PUT and retire doors. workflows: the bundle and the publish/retire
# doors in boss-jobs. subject_kinds: boss-subject-kinds' metadata PATCH
# door (backlog abc2e9d5, subject_kind.updated); a kind's other columns
# have no door yet, which is the reason to build one, not to migrate.
EVENTED_TABLES="classes subject_kinds workflows"

# Migrations that rewrote an evented row before the rule existed,
# measured 2026-09-27 on origin/main 4c250f91. One entry per line,
# `file|reason`; the reason is required (the self-test refuses an entry
# without one, and a pin in boss-testing refuses an entry whose file is
# gone — a dead name would admit whatever next took it).
EVENTED_ALLOWLIST=(
    "01-registries.sql|the baseline that creates subject_kinds; its two UPDATEs fill calendar_reservable and birth on rows the same file inserts, before any door exists to call"
    "20260924172233-the-refurb-asset-phases-retire.sql|retired four refurb asset phases on 2026-09-24 as a bare UPDATE, the unevented retirement this rule was written after; applied on every instance and append-only"
    "20260926061106-an-archived-message-keeps-its-kind.sql|retired the message kind archived on 2026-09-26 after its backfill; applied on every instance and append-only, so it stays as history"
    "20260926223752-the-audit-account-is-not-headcount.sql|merged counts_in_headcount false into the audit-readonly role Class on 2026-09-26; applied on every instance and append-only, so it stays as history"
)

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<table>`. Empty output = clean. The
# SQL is read with `--` comments removed, so prose about an insert is
# not an insert; the match is `INSERT INTO [public.]<table>` at a word
# boundary, any case, any whitespace.
findings_in() { # file
    awk -v tables="$REGISTRY_TABLES" '
        BEGIN { n = split(tables, t, /[ \n]+/); for (k = 1; k <= n; k++) if (t[k] != "") want[t[k]] = 1 }
        {
            line = $0
            sub(/--.*$/, "", line)
            low = tolower(line)
            if (match(low, /insert[ \t]+into[ \t]+(public\.)?[a-z_]+/) > 0) {
                tbl = substr(low, RSTART, RLENGTH)
                sub(/^insert[ \t]+into[ \t]+/, "", tbl)
                sub(/^public\./, "", tbl)
                if (tbl in want) printf "%d\t%s\n", NR, tbl
            }
        }
    ' "$1"
}

# One file's evented-row rewrites, as `<line>\t<verb>\t<table>`. Empty
# output = clean. `UPDATE [ONLY] [public.]<table>` and `DELETE FROM
# [ONLY] [public.]<table>`, any case, any whitespace, every statement on
# a line (not only the first), and a keyword left dangling at the end of
# a line (`DELETE` / `FROM workflows`) is read with the next one and
# reported at the line it started on. A table named after the keyword
# is the statement's TARGET, so `UPDATE messages … FROM classes` (a
# read) and `AFTER UPDATE ON classes` (a trigger) are not writes.
rewrites_in() { # file
    awk -v tables="$EVENTED_TABLES" '
        BEGIN {
            n = split(tables, t, /[ \n]+/); for (k = 1; k <= n; k++) if (t[k] != "") want[t[k]] = 1
            kw = "(update([ \t]+only)?|delete[ \t]+from([ \t]+only)?)"
            carry = ""; carry_nr = 0
        }
        {
            line = $0
            sub(/--.*$/, "", line)
            low = tolower(line)
            clen = 0; from_nr = carry_nr
            if (carry != "") { low = carry " " low; clen = length(carry) + 1 }
            carry = ""
            rest = low; pos = 0
            while (match(rest, "(^|[^a-z0-9_])" kw "[ \t]+(public\\.)?[a-z_]+") > 0) {
                at = pos + RSTART
                stmt = substr(rest, RSTART, RLENGTH)
                pos += RSTART + RLENGTH - 1
                rest = substr(rest, RSTART + RLENGTH)
                sub(/^[^a-z]/, "", stmt)
                verb = (stmt ~ /^update/) ? "updates" : "deletes from"
                tbl = stmt
                sub("^" kw "[ \t]+", "", tbl)
                sub(/^public\./, "", tbl)
                if (tbl in want) printf "%d\t%s\t%s\n", (clen > 0 && at <= clen) ? from_nr : NR, verb, tbl
            }
            if (match(low, "(^|[^a-z0-9_])(update([ \t]+only)?|delete([ \t]+from)?([ \t]+only)?)[ \t]*$") > 0) {
                carry = substr(low, RSTART, RLENGTH)
                sub(/^[^a-z]/, "", carry)
                carry_nr = (clen > 0 && RSTART <= clen) ? from_nr : NR
            }
        }
    ' "$1"
}

# Whether a migration (by basename) is named in EVENTED_ALLOWLIST.
allowlisted() { # basename
    local e
    for e in "${EVENTED_ALLOWLIST[@]}"; do
        [ "${e%%|*}" = "$1" ] && return 0
    done
    return 1
}

# The leading numeric prefix of a migration file name, or `none`.
prefix_of() { # basename
    local p="${1%%-*}"
    case "${p:-empty}" in
        empty|*[!0-9]*) echo none ;;
        *) echo "$p" ;;
    esac
}

# Whether a migration is NEWER than the cutover. Numeric on the prefix
# (three-digit, twelve-digit and fourteen-digit prefixes all sort the
# way migrate.sh sorts them); a file with no numeric prefix sorts last
# in migrate.sh too, so it is newer.
is_after_cutover() { # prefix
    [ "$1" = none ] && return 0
    [ "$1" -gt "$CUTOVER" ]
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a scanner whose regex stopped matching
# passes every file, and only a fixture it must refuse tells that from
# a clean tree. Fixtures live in a mktemp dir this run owns.
self_test() {
    local t hits
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    # Accepted: schema statements, a comment that names the insert, an
    # UPDATE, an insert into a table that merely starts with the name.
    {
        printf -- '-- A comment may say INSERT INTO stations to tell the story.\n'
        printf 'ALTER TABLE stations ADD COLUMN IF NOT EXISTS lens JSONB;\n'
        printf 'UPDATE stations SET lens = NULL WHERE name = %s; -- INSERT INTO stations, in prose\n' "'x'"
        printf 'INSERT INTO stations_archive (name) VALUES (%s);\n' "'x'"
        printf 'INSERT INTO event_kinds (kind_pattern) VALUES (%s);\n' "'jobs.station.x'"
    } >"$t/good.sql"
    hits="$(findings_in "$t/good.sql")"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shape was flagged:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }

    # Refused: each spelling a migration in this tree has used.
    printf 'INSERT INTO stations (name, version) VALUES (%s, 1)\n' "'x'"   >"$t/bad1.sql"
    printf 'insert into\tpublic.stations (name)\nSELECT %s\n' "'x'"        >"$t/bad2.sql"
    printf 'INSERT   INTO   Stations (name) VALUES (%s);\n' "'x'"          >"$t/bad3.sql"
    printf 'INSERT INTO step_plugins (\n    kind, version\n) VALUES (%s, 1)\n' "'x'" >"$t/bad4.sql"
    # The retire-by-name supersede idiom (202609032030): the INSERT's
    # rows come from a SELECT, not a VALUES list.
    printf 'INSERT INTO cadence_rules\n    (name, version, status, verb, basis)\nSELECT %s, COALESCE(MAX(version), 0) + 1, %s, %s, %s\n  FROM cadence_rules WHERE name = %s;\n' \
        "'x'" "'active'" "'board'" "'queue-depth'" "'x'" >"$t/bad5.sql"
    # The copy-the-active-row supersede (202609050500): a retiring
    # UPDATE, then an INSERT whose column list opens on the next line.
    printf 'UPDATE delivery_policy SET status = %s WHERE name = %s AND status = %s;\nINSERT INTO delivery_policy (\n    name, version, status, max_red_trains\n)\nSELECT name, version + 1, %s, max_red_trains FROM delivery_policy WHERE name = %s;\n' \
        "'retired'" "'x'" "'active'" "'active'" "'x'" >"$t/bad6.sql"
    local f
    for f in bad1.sql bad2.sql bad3.sql bad4.sql bad5.sql bad6.sql; do
        [ -n "$(findings_in "$t/$f")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done

    # The second rule's scanner (backlog fa25700f). Accepted: a table
    # named only as a READ or a trigger's event, DDL, prose, a table
    # that merely starts with the name, a column called updated_at,
    # and a foreign key's ON DELETE / ON UPDATE — none rewrites a row.
    {
        printf -- '-- A comment may say UPDATE classes SET retired_at to tell the story.\n'
        printf 'UPDATE messages m SET kind = c.code FROM classes c WHERE m.kind = c.code;\n'
        printf 'CREATE TRIGGER t AFTER UPDATE ON classes FOR EACH ROW EXECUTE FUNCTION f();\n'
        printf 'ALTER TABLE workflows ADD COLUMN IF NOT EXISTS lens TEXT;\n'
        printf 'UPDATE classes_archive SET x = 1;\n'
        printf 'ALTER TABLE subject_kinds ADD CONSTRAINT p FOREIGN KEY (parent) REFERENCES subject_kinds (kind) ON DELETE CASCADE ON UPDATE CASCADE;\n'
        printf 'SELECT updated_at FROM classes;\n'
    } >"$t/good2.sql"
    hits="$(rewrites_in "$t/good.sql")$(rewrites_in "$t/good2.sql")"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shape was read as an evented-row rewrite:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }
    # Refused, each with the line its statement STARTS on.
    printf 'UPDATE classes SET retired_at = NOW() WHERE code = %s;\n' "'x'" >"$t/rw1.sql"
    printf 'update\tONLY public.subject_kinds set metadata = %s;\n' "'{}'" >"$t/rw2.sql"
    printf 'DELETE\n  FROM workflows WHERE kind = %s;\n' "'x'"              >"$t/rw3.sql"
    printf 'SELECT 1; UPDATE Workflows SET status = %s;\n' "'retired'"     >"$t/rw4.sql"
    printf -- '-- first\nDELETE FROM -- the row\n    classes WHERE code = %s;\n' "'x'" >"$t/rw5.sql"
    local want
    for f in "rw1.sql|1	updates	classes" "rw2.sql|1	updates	subject_kinds" \
             "rw3.sql|1	deletes from	workflows" "rw4.sql|1	updates	workflows" \
             "rw5.sql|2	deletes from	classes"; do
        want="${f#*|}"
        hits="$(rewrites_in "$t/${f%%|*}")"
        [ "$hits" = "$want" ] || {
            echo "$NAME: self-test FAILED — ${f%%|*} should read as [$want], read as [$hits]:" >&2
            sed 's/^/    /' "$t/${f%%|*}" >&2
            return 1
        }
    done
    # Every allowlist entry names a file and says why.
    local e
    for e in "${EVENTED_ALLOWLIST[@]}"; do
        case "$e" in
            ?*'|'?*) ;;
            *) echo "$NAME: self-test FAILED — allowlist entry is not file|reason: $e" >&2; return 1 ;;
        esac
    done

    # The cutover, both sides: a three-digit and a twelve-digit prefix
    # are history, a fourteen-digit one newer than the stamp is not, a
    # file with no numeric prefix sorts last and is not.
    ! is_after_cutover "$(prefix_of 116-stations.sql)" \
        && ! is_after_cutover "$(prefix_of 202609101900-the-corpus-index-is-deleted.sql)" \
        && ! is_after_cutover "$(prefix_of 20260915023614-a-machine-owned-proof.sql)" \
        && ! is_after_cutover "$(prefix_of "$CUTOVER-the-cutover-itself.sql")" \
        && is_after_cutover "$(prefix_of 20260918112135-one-second-later.sql)" \
        && is_after_cutover "$(prefix_of 20261001000000-next-month.sql)" \
        && is_after_cutover "$(prefix_of no-prefix.sql)" || {
        echo "$NAME: self-test FAILED — the cutover comparison answers wrongly" >&2
        return 1
    }
    echo "$NAME: self-test ok — schema statements, prose, an UPDATE and a prefixed table name pass; six spellings of INSERT INTO a registry table are refused; the cutover splits history from new; reads, triggers and foreign keys on an evented registry pass while five spellings of UPDATE or DELETE FROM one are refused at their line"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
# `git ls-files` answers 0 for a listing (empty included) and non-zero
# only when it could not look — git_answer keeps those apart.
files="$(git_answer "$NAME" 0 ls-files -- "$SCHEMA_DIR/*.sql")"
status=$?
[ "$status" -eq 0 ] || exit "$status"

scanned=0
findings=0
rewrites=0
admitted=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    scanned=$((scanned + 1))
    # The second rule reads EVERY migration; the allowlist, not the
    # cutover, is what admits history.
    rw="$(rewrites_in "$file")"
    if [ -n "$rw" ]; then
        if allowlisted "$(basename "$file")"; then
            admitted=$((admitted + 1))
        else
            while IFS=$'\t' read -r lineno verb tbl; do
                [ -n "${lineno:-}" ] || continue
                rewrites=$((rewrites + 1))
                echo "$NAME: $file:$lineno $verb $tbl" >&2
            done <<RW
$rw
RW
        fi
    fi
    is_after_cutover "$(prefix_of "$(basename "$file")")" || continue
    while IFS=$'\t' read -r lineno tbl; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno inserts into $tbl" >&2
    done < <(findings_in "$file")
done <<EOF
$files
EOF

if [ "$rewrites" -gt 0 ]; then
    cat >&2 <<MSG

FAIL — the migration(s) above rewrite an evented registry row in SQL
(an update or a delete). A migration's write publishes no event, so the
audit log — the system of record — never hears the registry changed, and no rebuilder
can reproduce it (backlog fa25700f: the refurb asset phases were
retired that way on 2026-09-24). Change the row through its door:

  * classes — PUT /api/classes/{subject_kind}/{code} edits a Class and
    publishes class.updated; POST /api/classes/{subject_kind}/{code}/retire
    withdraws one and publishes class.retired (boss-classes http.rs).
  * workflows — the protocol's file under infra/platform/workflows/,
    with a version bump, which the platform seed publishes; retire
    through POST /api/workflows/{kind}/retire (jobs.kind.published,
    jobs.kind.retired). A packet in flight moves only by boss job convert.
  * subject_kinds — PATCH /api/subject-kinds/{kind}/metadata merges keys
    into one kind's metadata (a null deletes) and publishes
    subject_kind.updated (boss-subject-kinds http.rs; platform-admin's
    Update on subject-kind). A kind's key, label, parent and ownership
    have no door yet; build that one first, never a migration.

A migration keeps its DDL. The migrations that did this before the rule
existed are named in EVENTED_ALLOWLIST with their reasons; that list is
history, not a queue — do not add a new migration to it.
MSG
fi

if [ "$findings" -gt 0 ]; then
    cat >&2 <<MSG

FAIL — the migration(s) above are newer than the cutover ($CUTOVER) and
insert a registry row. Since 2026-09-18 (backlog 393d3234) a migration
declares SCHEMA only; a registry row lives in its platform bundle, which
the seed publishes insert-if-missing at every start:

  * stations — infra/platform/stations/<name>.toml, one file per station,
    every column the row has. boss-platform-workflow-seed finds the
    directory beside its --seed-path and publishes it by (name, version);
    a row that differs from the live active row of the same (name,
    version) is refused, and a version bump is the edit path.
  * step_plugins — infra/platform/step-plugins/<kind>.toml, one file per
    plugin, every column of the row; the JS it names stays at
    infra/step-plugins/<frontend_url>. Same seed, same sibling lookup,
    same refusal, same edit path.
  * cadence_rules — infra/platform/cadence/<name>.toml, one file per
    rule, every column of the row. Same seed, same sibling lookup, same
    refusal, same edit path. The table stays live-editable: a rule
    re-versioned live is reported as ahead of its file, never rewritten,
    and a rule the operator retired stays retired.
  * delivery_policy — infra/platform/delivery-policy/<name>.toml, one
    file per policy (there is one, train-conductor), every column of
    the row. Same seed, same sibling lookup, same refusal, same edit
    path. One row is the whole policy: a train pins the version it
    departed under, so a bump changes the NEXT boarding's rules only.

Leave the migration to its ALTERs and put the row in the bundle. The
historical inserts before the cutover are history and stay as they are.
MSG
    exit 1
fi
[ "$rewrites" -eq 0 ] || exit 1

lint_scanned "$NAME" "$scanned" "migration(s) checked against the cutover"
echo "$NAME: ok — every migration newer than $CUTOVER declares schema only, and none rewrites an evented registry row ($admitted allowlisted)"
exit 0
