#!/usr/bin/env bash
# emitted-kinds-are-declared — every domain-event kind emitted in
# crates/ must be declared in the event_kinds registry
# (infra/postgres/schema/*.sql). WHY this is a GATE lint and not a
# runtime warning:
#
# The audit-integrity checker (boss-audit-integrity-check, schema 108)
# already guards drift — but from the OTHER side, and too late. It
# scans the live `audit_log` and WARNS when it finds a kind no
# event_kinds row covers. Two failures ride on that being a runtime
# warning over live rows:
#   1. It only fires once a row of that kind has actually landed in
#      the production log. An emitted-but-never-yet-exercised kind is
#      invisible until the day someone triggers it in prod.
#   2. It is a WARNING inside an exit-0 run. `ledger.excise_rate_
#      schedule.upserted` rode inside passing nightly runs, unread,
#      for 8 days (fixed reactively, migration 202609022300); the
#      `credential.*` kinds needed a same-car declaration to avoid the
#      same fate (202609031830). CLAUDE.md §Diagnosis names this class:
#      "a real finding — an emitted-but-undeclared event kind — rode
#      inside a passing run for days, unread."
#
# This lint catches the same hole AT AUTHORSHIP, statically, before
# the code merges: it diffs the kinds emitted in source against the
# kinds declared in the schema and FAILS the gate naming any emitted
# kind no row covers. The fix a failure asks for is exactly the one
# the runtime checker asks for — add a row (or a family pattern) in a
# new migration.
#
# HONEST LIMIT — literal + const kinds only. A kind assembled at
# runtime (`format!("ledger.{}.upserted", x)`) cannot be read from the
# source text, and this lint does not try; it covers kinds that appear
# as a string literal at an emit call, or as a `pub const … = "…"`
# whose name is passed to one. This is the same limit the manual diff
# that seeded migration 202609022300 had. Every kind that has bitten
# so far was a literal or a const, which is why the literal cover is
# the one worth gating.
#
# KIND-FORWARDING HELPERS ARE DERIVED, NOT LISTED (backlog 7d05ded2,
# 2026-10-19). Most emits go through a helper that takes the kind as
# its first argument and hands it to `Event::new` —
# boss-jobs' `events::workflow_registry_event(kind, actor, spec)` and
# its station / step-plugin / cadence twins. The lint used to know only
# three call NAMES, so the 14 registry.rs calls of that helper were
# invisible and `jobs.kind.draft_discarded` was emitted undeclared from
# ebd7bb70 on while this lint printed `ok`. A fourth name would be
# walked around by the fifth helper, so the helpers are read off the
# source instead: a function whose FIRST parameter is `kind: &str` and
# whose body passes that `kind` as `Event::new`'s kind argument — or on
# to another such function, to a fixed point — is a forwarder, and a
# call to one emits its first argument. Pinned on fixture trees by
# crates/core/boss-testing/tests/a_kind_sent_through_a_helper_is_still_emitted.rs.
# Two limits of the derivation, each of which MISSES a helper (never a
# false red), named here rather than discovered (review 738b2811): the
# parameter must be spelled `kind` (an `event_kind: &str` is not read),
# and the body is brace-matched without reading literals, so a `'}'` or
# `"}"` before the Event::new call ends the body early.
#
# Enforcement shape mirrors the other infra/lint/*.sh checks: derive
# both sets from their source of truth, exit 1 naming every offender.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

SCHEMA_DIR="infra/postgres/schema"

# ---------------------------------------------------------------------
# DECLARED: the first single-quoted column of every VALUES tuple inside
# an `INSERT INTO event_kinds` statement — the kind_pattern. Scoped to
# that statement so a hyphenated dispatcher-rule name in a neighbouring
# INSERT (same file, different table) is not mistaken for a kind.
# Family patterns like `step.done.*` are kept verbatim; the matcher
# below expands them.
# ---------------------------------------------------------------------
declared() {
    awk '
        /INSERT INTO event_kinds/ { inblk = 1 }
        inblk && /^[[:space:]]*\(/ {
            line = $0
            if (match(line, /\x27[^\x27]+\x27/)) {
                print substr(line, RSTART + 1, RLENGTH - 2)
            }
        }
        inblk && /;[[:space:]]*$/ { inblk = 0 }
    ' "$SCHEMA_DIR"/*.sql | sort -u
}

# ---------------------------------------------------------------------
# EMITTED: every domain-event kind passed to an audit-log emit
# primitive in a non-test crate path. The audit-log write path is the
# transactional outbox (`record_event_in_tx` / `record_ledger_event_
# in_tx`), fed by `EventStamp::event(kind, …)` and, in a few sites, a
# direct `Event::new("source", "kind", …)` assignment or a crate-local
# `record(&mut tx, stamp, kind, …)` wrapper. A kind argument is either
# a string literal or the NAME of a `pub const … : &str = "kind"` —
# both are resolved here.
#
# A kind constant is referenced by path (`crate::events::ACCOUNT_
# UPDATED`) from a DIFFERENT file than the one that defines it, so the
# const map is built ONCE across every crate file (phase 1) and the
# emit scan (phase 2) resolves against that global map.
#
# Deliberately EXCLUDED, because they are not audit-log writes:
#   - `#[cfg(test)] mod { … }` bodies (fixtures like "order.created")
#     and everything under a tests/ directory.
#   - `hub.publish(Event::new(…))` — SSE/telemetry broadcasts that
#     never reach the outbox (e.g. `cybernetics.cost.recorded`; the
#     free `event()` telemetry fn that also did this left with
#     boss-cybernetics, 467175e7). Matching only the
#     `let … = Event::new(` assignment form, never `publish(Event::new(`,
#     is what draws that line.
#   - fact kinds (`finance.*`) and dispatcher rule topics (`step.done`,
#     `steps.*.done`): these are not passed to the emit primitives.
#
# The whole file is slurped and inter-token whitespace flattened so a
# call split across lines (the common formatting) still matches.
# ---------------------------------------------------------------------

# Phase 1: the global const map, `NAME<TAB>value`, kind-shaped values
# only. Written once and fed to the scan as awk's first input file.
kind_const_map() {
    grep -rhnE 'const [A-Z][A-Z0-9_]*[[:space:]]*:[[:space:]]*&[[:space:]]*str[[:space:]]*=[[:space:]]*"[a-z][a-z0-9_.-]*"' \
        crates --include='*.rs' 2>/dev/null \
        | sed -E 's/.*const ([A-Z][A-Z0-9_]*)[[:space:]]*:[[:space:]]*&[[:space:]]*str[[:space:]]*=[[:space:]]*"([^"]+)".*/\1\t\2/' \
        | awk -F'\t' '$2 ~ /^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)+$/' \
        | sort -u
}

emitted() {
    local mapfile fwdfile
    mapfile="$(mktemp)"
    fwdfile="$(mktemp)"
    # Cleaned up at the end of this function, NOT by a RETURN trap: a
    # RETURN trap set here fires again when a later `.`-sourced file
    # finishes, with $mapfile out of scope and `set -u` aborting the
    # lint (measured in a-boot-check, 2026-09-18).
    kind_const_map > "$mapfile"
    # Phase 1b: the kind-forwarding helpers, one name per line. The
    # scan prints per-file facts (`D <fn>` passes its `kind` to
    # Event::new, `C <fn> <callee>` passes it to <callee>); the fixed
    # point is taken here, over EVERY file, because xargs may split the
    # file list across several awk processes and a helper is usually
    # called from a file other than the one defining it.
    scan_crates forwarders /dev/null /dev/null \
        | awk '
            $1 == "D" { F[$2] = 1 }
            $1 == "C" { E[++n] = $2 " " $3 }
            END {
                do {
                    changed = 0
                    for (i = 1; i <= n; i++) {
                        split(E[i], p, " ")
                        if (!(p[1] in F) && (p[2] in F)) { F[p[1]] = 1; changed = 1 }
                    }
                } while (changed)
                for (k in F) print k
            }' > "$fwdfile"
    # Phase 2: scan every non-test crate file, resolving const names
    # (`NAME<TAB>value`, read from mapfile) against the global map.
    scan_crates emits "$mapfile" "$fwdfile" | sort -u
    rm -f "$mapfile" "$fwdfile"
}

# scan_crates <mode> <mapfile> <fwdfile> — one awk program over every
# non-test crate file, in one of two modes: `forwarders` prints the
# per-file helper facts above, `emits` prints every kind emitted.
scan_crates() {
    # shellcheck disable=SC2016
    find crates -name '*.rs' -not -path '*/tests/*' -print0 \
        | xargs -0 awk -v mode="$1" -v mapfile="$2" -v fwdfile="$3" '
        BEGIN {
            while ((getline line < mapfile) > 0) {
                split(line, a, "\t")
                if (a[1] != "") CONST[a[1]] = a[2]
            }
            while ((getline line < fwdfile) > 0) {
                if (line != "") FWD[line] = 1
            }
        }
        # --- accumulate one file at a time ---
        FNR == 1 && NR > 1 { process(); buf = "" }
        { buf = buf $0 "\n" }
        END { process() }

        function process(   src, n) {
            src = strip_cfg_test(buf)
            # Flatten whitespace so multi-line calls match as one span.
            gsub(/[[:space:]]+/, " ", src)
            if (mode == "forwarders") { derive(src); buf = ""; return }
            # A call to a derived kind-forwarding helper emits its first
            # argument. The leading class keeps `x_registry_event(` from
            # matching a helper named `registry_event`; a path prefix
            # (`crate::events::`) ends in `:`, which it accepts. The kind
            # may be the ONLY argument — `audit(KIND)`, which rustfmt keeps
            # on one line with no trailing comma — so the span ends at a
            # comma OR the closing paren: derive() accepts a `kind: &str`
            # that is the sole parameter, and a forwarder derived but never
            # read here is the invisible helper this pass exists to close
            # (review 738b2811, 2026-10-19).
            for (n in FWD) {
                scan(src, "(^|[^A-Za-z0-9_])" n " *\\( *(\"[^\"]+\"|[A-Za-z0-9_:]+) *[,)]")
            }
            # NOTE: the patterns are passed as STRINGS, not /regex/
            # literals — a regex literal handed to a function is
            # evaluated as `($0 ~ /re/)` (a number), which silently
            # matched nothing here until it was caught.
            scan(src, "\\.event\\( *(\"[^\"]+\"|[A-Za-z0-9_:]+) *,")
            scan(src, "record_ledger_event_in_tx\\( *[^,]+, *[^,]+, *(\"[^\"]+\"|[A-Za-z0-9_:]+) *,")
            scan(src, "record\\( *&?mut tx *, *[^,]+, *(\"[^\"]+\"|[A-Za-z0-9_:]+) *,")
            # Direct Event::new assignment (NOT publish(Event::new(…)):
            # a literal source and a literal kind.
            scan_newlit(src)
            buf = ""
        }

        # Remove every `#[cfg(test)] mod … { … }` block by brace match,
        # so test-only kind fixtures never count as emitted.
        function strip_cfg_test(s,   out, idx, mi, tail, bstart, depth, j, ch) {
            out = ""
            while (1) {
                idx = index(s, "#[cfg(test)]")
                if (idx == 0) { out = out s; break }
                tail = substr(s, idx + 12)   # 12 = length("#[cfg(test)]")
                mi = match(tail, /mod[^{;]*\{/)
                if (mi == 0) {
                    # not a module cfg(test); keep the marker, move past it
                    out = out substr(s, 1, idx + 11)
                    s = tail
                    continue
                }
                out = out substr(s, 1, idx - 1)
                # opening brace position within s
                bstart = idx + 12 + mi + RLENGTH - 2
                depth = 0
                for (j = bstart; j <= length(s); j++) {
                    ch = substr(s, j, 1)
                    if (ch == "{") depth++
                    else if (ch == "}") { depth--; if (depth == 0) break }
                }
                s = substr(s, j + 1)
            }
            return out
        }

        function is_kind(v) {
            return (v ~ /^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)+$/)
        }

        function resolve(arg,   p) {
            if (arg ~ /^".*"$/) return substr(arg, 2, length(arg) - 2)
            p = arg
            while (match(p, /::/)) { p = substr(p, RSTART + 2) }
            if (p in CONST) return CONST[p]
            return ""
        }

        # The captured span always ends `… <kind-arg> ,` (or `… <kind-arg> )`
        # for a one-argument forwarder) — the kind is the LAST argument
        # before that delimiter, whether the call is `.event(KIND,` (kind
        # first) or `record(&mut tx, stamp, KIND,` (kind third). So drop
        # the delimiter and read the final token, not the first.
        function scan(s, re,   rest, a, kind) {
            rest = s
            while (match(rest, re)) {
                a = substr(rest, RSTART, RLENGTH)
                rest = substr(rest, RSTART + RLENGTH)
                sub(/ *[,)][[:space:]]*$/, "", a)       # drop trailing , or )
                if (match(a, /"[^"]+"$/)) {              # last arg is a literal
                    kind = resolve(substr(a, RSTART, RLENGTH))
                } else if (match(a, /[A-Za-z0-9_:]+$/)) { # last arg is an ident/path
                    kind = resolve(substr(a, RSTART, RLENGTH))
                } else {
                    kind = ""
                }
                if (kind != "" && is_kind(kind)) print kind
            }
        }

        # let … = Event::new("source", "kind", …)  — assignment form only.
        function scan_newlit(s,   rest, a, k2) {
            rest = s
            while (match(rest, /= Event::new\( *"[^"]+" *, *"[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)+"/)) {
                a = substr(rest, RSTART, RLENGTH)
                rest = substr(rest, RSTART + RLENGTH)
                if (match(a, /, *"[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)+" *$/)) {
                    k2 = substr(a, RSTART, RLENGTH)
                    match(k2, /"[^"]+"/)
                    print substr(k2, RSTART + 1, RLENGTH - 2)
                }
            }
        }

        # A function whose FIRST parameter is `kind: &str` (any
        # lifetime), with a body. Prints `D <name>` when the body hands
        # `kind` to Event::new as its kind, and `C <name> <callee>` for
        # every call in the body whose first argument is `kind`. A
        # `self`-first method is not one: `.event(KIND,` is read above.
        function derive(s,   rest, name, after, semi, open, depth, j, ch, body, b, c) {
            rest = s
            while (match(rest, "fn [a-z_][a-z0-9_]* *(<[^>(]*>)? *\\( *kind *: *& *([^ ,)]+ )?str *[,)]")) {
                name = substr(rest, RSTART + 3, RLENGTH - 3)
                sub(/[^a-z0-9_].*$/, "", name)
                after = substr(rest, RSTART + RLENGTH)
                rest = after
                open = index(after, "{")
                semi = index(after, ";")
                if (open == 0 || (semi > 0 && semi < open)) continue   # no body
                depth = 0
                for (j = open; j <= length(after); j++) {
                    ch = substr(after, j, 1)
                    if (ch == "{") depth++
                    else if (ch == "}") { depth--; if (depth == 0) break }
                }
                body = substr(after, open, j - open + 1)
                # The source argument may itself be one flat call
                # (`SOURCE.to_string()`, `self.source()`) and the kind may
                # be converted on the way in (`kind.to_string()`): both
                # were helpers this missed (review 738b2811, finding 2).
                if (match(body, "Event::new\\( *[^,()]+(\\([^(),]*\\))? *, *kind(\\.(to_string|to_owned|into)\\(\\))? *[,)]")) print "D " name
                b = body
                while (match(b, "[A-Za-z_][A-Za-z0-9_]* *\\( *kind *[,)]")) {
                    c = substr(b, RSTART, RLENGTH)
                    b = substr(b, RSTART + RLENGTH)
                    sub(/[^A-Za-z0-9_].*$/, "", c)
                    print "C " name " " c
                }
            }
        }
    '
}

DECLARED="$(declared)"
EMITTED="$(emitted)"

# An emitted kind is covered if it equals a declared pattern exactly,
# or matches a declared `<prefix>.*` family pattern.
undeclared=()
while IFS= read -r kind; do
    [ -n "$kind" ] || continue
    covered=0
    while IFS= read -r pat; do
        [ -n "$pat" ] || continue
        if [ "$kind" = "$pat" ]; then covered=1; break; fi
        case "$pat" in
            *.\*)
                prefix="${pat%.\*}."
                case "$kind" in
                    "$prefix"*) covered=1; break ;;
                esac
                ;;
        esac
    done <<< "$DECLARED"
    if [ "$covered" -eq 0 ]; then
        undeclared+=("$kind")
    fi
done <<< "$EMITTED"

emitted_count=$(printf '%s\n' "$EMITTED" | grep -c . || true)
declared_count=$(printf '%s\n' "$DECLARED" | grep -c . || true)
printf '[emitted-kinds-are-declared] emitted (literal/const): %s | declared: %s\n' \
    "$emitted_count" "$declared_count"

if [ "${#undeclared[@]}" -gt 0 ]; then
    echo
    echo "FAIL: event kinds EMITTED in crates/ but NOT declared in the"
    echo "event_kinds registry ($SCHEMA_DIR/*.sql):"
    for k in "${undeclared[@]}"; do
        echo "  - $k"
    done
    echo
    echo "Each of these is written to the audit_log with no registry row"
    echo "to name it, so the nightly audit-integrity checker will WARN on"
    echo "it (invisibly, inside a passing run) the first time one lands in"
    echo "prod. Declare them at authorship instead: add a row in a NEW"
    echo "migration under $SCHEMA_DIR/ — an event_kinds insert of"
    echo "(kind_pattern, source, description, suffix_domain), one tuple"
    echo "per kind, guarded by ON CONFLICT (kind_pattern) DO NOTHING."
    echo "See 202609022300-event-kinds-ledger-control-plane.sql for the"
    echo "worked shape. When the suffix is bounded by another registry,"
    echo "declare a family pattern ('<prefix>.*', …, suffix_domain =>"
    echo "that registry) instead of one row per suffix."
    exit 1
fi

lint_scanned emitted-kinds-are-declared "$emitted_count" "emitted kind(s) resolved across crates/, against $declared_count declared"
echo "ok: every literal/const emitted kind is declared in event_kinds"
