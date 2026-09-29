#!/usr/bin/env bash
#
# no-tenant-vocabulary-above-its-tier — the sensor, with a ratchet, on
# tenant words leaking into the tiers above the tenant (backlog
# be39298f).
#
# THE PRINCIPLE
# -------------
# BOSS core is a generic state-machine toolkit; the brewery and the
# used-device shop are tenants instantiated on it, and CLAUDE.md §10
# says no tenant-specific assumption lives in core. Until 2026-09-16
# that rule was enforced only as a Cargo dependency audit
# (tier-import-audit.sh), which sees crate edges and nothing else. A
# grep for the brewery's own words outside the brewery found them in
# every tier above it — 362 files: kegs in core registries, excise in
# module ledgers, the brewery simulator in an orchestrator crate.
# David, the same day: development moves on without the brewery
# tenant, "because I am guessing we will need to clear out a lot of
# hardcoded brewery code still."
#
# Clearing it is one car per concentration. This is the instrument
# those cars are measured with, and the ratchet that stops the number
# rising between them.
#
# THE CHECKED PROPERTY
# --------------------
# For every tenant that declares `examples/<tenant>/VOCABULARY` — one
# term per line, the tenant's own words, `*` for a word-start prefix —
# count the case-insensitive occurrences of every term in every FILE
# under each TIER (the roots below), over .rs .ts .svelte .toml .sql .sh
# .yaml, and compare each file's count to its line in
# infra/lint/tenant-vocabulary.baseline (one `<count>\t<path>` line per
# file that still carries a word; a file with no line has a baseline of
# 0). Then, per file:
#
#   count > baseline  -> REFUSED, naming the file and both numbers:
#                        the leak grew (or reached a file it was not in).
#   count = baseline  -> clean.
#   count < baseline  -> REFUSED, telling the author to set that file's
#                        line to the count — or delete it at 0 — in the
#                        same car. The ratchet only goes down, and
#                        refusing a baseline ABOVE the count is what
#                        stops anyone raising it.
#
# The count is derived from the tree on every run; the baseline is the
# one fact that lives twice, and the equality above is its test
# (CLAUDE.md §9a). It is written from this lint's own measurement
# (`--measure`), never typed — this file is the definition.
#
# WHY PER FILE, NOT PER TIER (backlog e889cfa4, 2026-09-27). Until then
# the baseline was one `<tier> <count>` line per tier, and every car
# that removed a demo word from apps/web/src rewrote the one
# `apps/web/src` integer. Three cars did in one afternoon (254 to 249,
# 251 and 253); the first to land left the others CONFLICTING WITH MAIN
# on that line, a rerail made one boardable for sixteen minutes before
# the next train moved the number again, and at the worst point six
# dock cars could not board beside each other. That is the contended
# tail line of CLAUDE.md §9a — manifest.txt, PREFLIGHT_LINTS, the
# rules.toml BASELINE — in a new file: an authoritative copy everyone
# has to EDIT, holding one number that is a sum of numbers each car
# already owns. Per file, a car edits the lines of the files it touched
# and no other, the shape infra/lint/svelte-check.sh keeps its warning
# ratchet in. The tier table the lint prints is still per tier; the
# tiers scanned did not change.
#
# The entries are kept one BLANK LINE apart, and that is load-bearing.
# git's three-way merge refuses two edits to ADJACENT lines, so with
# the entries packed, two cars lowering two neighbouring files — two
# pages in one directory, the usual shape of a burn-down — still
# conflicted. Measured with `git merge-file` the day this changed: two
# neighbouring edits conflict packed and merge clean blank-separated
# (the test two_files_lowering_their_counts_merge_without_a_conflict
# pins that). What still meets is one car DELETING a line beside a line
# another car changes; that needs the two cars to empty and touch
# alphabetically neighbouring files at once.
#
# WHAT IS NOT COUNTED, and why
#   - the tenant's own homes: examples/<tenant>/ and
#     crates/tenants/<its engine>/ are not tiers above the tenant, and
#     neither is a root below;
#   - test FILES: any path under a /tests/ directory, *_test.rs,
#     *.test.*, *.spec.*. A `#[cfg(test)]` block inside a source file
#     IS counted — telling one apart needs a parser, and a lint that
#     guessed would count differently from the number an author sees.
#     Move a tenant-flavoured test into tests/ or into the tenant;
#   - docs, worktrees, node_modules, target/: none is under a root;
#   - a tenant's NAME used as a PATH or as an ID: `examples/<tenant>`
#     (so `examples/brewery/seeds/tenant.toml`, `/opt/boss/examples/
#     brewery/data`) and `tenant_id = "<tenant>"`. The product must be
#     able to say which tenant an instance runs, and that is not the
#     tenant's vocabulary leaking. Measured the day this lint was
#     written: the car rendering every instance from one manifest
#     (infra/cluster/instances.toml, `tenant = "examples/brewery/seeds/
#     tenant.toml"`) was clean on its own gate and red beside this one
#     on the assembled tree, for naming the playground's tenant path.
#     ONLY those two forms, for the names of tenants that declare a
#     VOCABULARY; the same name as a bare word, or as any other key's
#     value, still counts;
#   - a word inside a migration's DELETE statement (backlog b5f21e82,
#     2026-09-18): `DELETE FROM … ;` in a `.sql` file, from the line
#     the statement starts on to the line its `;` ends. The rows the
#     historical migrations inserted under the demo tenant's rule names
#     are the leak in every running database, and the one migration
#     that deletes them has to spell those names. Inside a DELETE a
#     word can only leave the product, never arrive — and every file
#     in infra/postgres/schema is applied history that cannot be
#     edited, so without this form that tier could never fall to zero
#     once the deleting migration landed. The same word in an INSERT
#     beside it, or in the comment above it, still counts.
#   - a DISCLAIMED PHRASE: a `!<phrase>` line in a VOCABULARY, matched
#     as a whole phrase, case-insensitively. A term can be right in
#     general and wrong for one common literal — `brew*` deliberately
#     reaches brewery, brewhouse and brewing, and also reaches
#     `brew install`, which is Homebrew. The tenant already made this
#     class of judgement in prose (its "deliberately NOT listed" block
#     names tap, batch, hop, grain, barrel, ale); a comment asking the
#     next author to make it again is not a mechanism (CLAUDE.md §9a),
#     so the phrase is declared beside the words it qualifies and is
#     subtracted with the same exact weighting as the forms above. A
#     phrase carrying no term of that VOCABULARY is refused: it would
#     exempt nothing while reading as though it covered something
#     (backlog e9423392).
#
# A wrong path answers 0 instead of erroring (CLAUDE.md §Doors), so a
# missing tier root, a missing VOCABULARY, a malformed term, and a
# baseline line naming a path under no tier this lint scans (or a path
# twice, or a count of 0) are all refusals, never a smaller count.
#
# Usage:  infra/lint/no-tenant-vocabulary-above-its-tier.sh
#         infra/lint/no-tenant-vocabulary-above-its-tier.sh --measure
#             print the per-file table as baseline entries and judge
#             nothing — what the baseline was written from.
set -uo pipefail

LINT=no-tenant-vocabulary-above-its-tier
MEASURE=0
case "${1:-}" in
    '') ;;
    --measure) MEASURE=1 ;;
    *) printf 'usage: %s [--measure]\n' "$0" >&2; exit 2 ;;
esac
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

BASELINE="infra/lint/tenant-vocabulary.baseline"

# The tiers above a tenant. Order is the report's order; every baseline
# line names a file under one of them.
TIERS=(
    crates/core
    crates/modules
    crates/orchestrators
    apps/web/src
    apps/simulator
    infra/postgres/schema
    infra/platform
    infra/cluster
)

refuse() { printf '%s: %s\n' "$LINT" "$*" >&2; exit 1; }

for tier in "${TIERS[@]}"; do
    [ -d "$tier" ] || refuse "tier root $tier is not a directory here — a missing root would count 0, which is a wrong path, not a clean tier"
done

# ---- the word list: every tenant's VOCABULARY, merged --------------
shopt -s nullglob
vocab_files=(examples/*/VOCABULARY)
shopt -u nullglob
[ "${#vocab_files[@]}" -gt 0 ] \
    || refuse "no examples/*/VOCABULARY found — zero words would count zero hits everywhere and certify nothing"

# One ERE alternation. A bare term is a whole word (`wort`, not
# `worth`); a trailing `*` is a word-start prefix (`brew*` reaches
# brewery, brewhouse, brewing). The term alphabet is restricted so no
# escaping is ever needed and a stray metacharacter cannot widen the
# match silently.
#
# A line beginning with `!` is a DISCLAIMED PHRASE, not a term: a
# literal that carries a term but names something outside this tenant
# (`brew install` is Homebrew, though `brew*` is right in general —
# backlog e9423392). It is collected here and subtracted below with the
# exempt forms, by the same exact weighting.
pattern=""
terms=0
tenants=""
phrases=""
phrase_list=""
for vf in "${vocab_files[@]}"; do
    tenant=${vf#examples/}; tenant=${tenant%/VOCABULARY}
    tenants="${tenants:+$tenants, }$tenant"
    while IFS= read -r line || [ -n "$line" ]; do
        line=${line%%#*}
        line=${line#"${line%%[![:space:]]*}"}
        line=${line%"${line##*[![:space:]]}"}
        [ -n "$line" ] || continue
        case "$line" in
            '!'*)
                phrase=${line#!}
                phrase=${phrase#"${phrase%%[![:space:]]*}"}
                case "$phrase" in
                    ''|*[!a-z0-9\ -]*)
                        refuse "$vf: disclaimed phrase '$line' — a phrase is lowercase letters, digits, spaces and hyphens, with no trailing *" ;;
                esac
                phrases="${phrases:+$phrases|}\\b${phrase}\\b"
                phrase_list="${phrase_list:+$phrase_list
}$vf $phrase"
                continue ;;
        esac
        case "$line" in
            *'*') term=${line%\*}; tail='' ;;
            *)    term=$line;      tail='\b' ;;
        esac
        case "$term" in
            ''|*[!a-z0-9\ -]*)
                refuse "$vf: term '$line' — terms are lowercase letters, digits, spaces and hyphens, with an optional trailing *" ;;
        esac
        pattern="${pattern:+$pattern|}\\b${term}${tail}"
        terms=$((terms + 1))
    done < "$vf"
done
[ "$terms" -gt 0 ] || refuse "every VOCABULARY is empty — nothing to count"

# A disclaimed phrase that carries no term exempts nothing: a
# declaration that reads as covering something and covers nothing, which
# is the wrong-path-answers-0 shape (CLAUDE.md §Doors). Refused here,
# where the term pattern is finally complete.
while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    vf=${entry%% *}; phrase=${entry#* }
    n=$(printf '%s' "$phrase" | grep -oiE -e "$pattern" | wc -l | tr -d ' ')
    [ "$n" -gt 0 ] \
        || refuse "$vf: disclaimed phrase '$phrase' carries no term of this VOCABULARY, so it would exempt nothing — remove it, or spell the phrase the way the term matches"
done <<PHRASES
$phrase_list
PHRASES

# ---- the count: one grep per tier, occurrences per file ------------
# `-o` so a line carrying two terms counts two; `-H` so every hit
# carries its file. Test files are dropped by path AFTER the grep so
# the same file set is scanned whatever grep's --exclude semantics are.
# Paths here never contain ':' (the separator); a tree that adds one
# would miscount and this is where to fix it.
hits=""
for tier in "${TIERS[@]}"; do
    tier_hits=$(grep -rIioHE \
        --include='*.rs' --include='*.ts' --include='*.svelte' \
        --include='*.toml' --include='*.sql' --include='*.sh' \
        --include='*.yaml' \
        -e "$pattern" -- "$tier" 2>/dev/null \
        | grep -vE '^[^:]*(/tests/|_test\.rs:|\.test\.[^/:]*:|\.spec\.[^/:]*:)' \
        || true)
    [ -n "$tier_hits" ] && hits="${hits:+$hits
}$tier_hits"
done

# ---- the exemption: a tenant's name as a path or as an id ----------
# The same grep, for the two exempt forms, over the names of the
# tenants that declared a VOCABULARY. Each exempt occurrence is
# SUBTRACTED from its file's count, weighted by how many vocabulary
# hits that exact literal carries (`examples/brewery` is one `brew*`
# hit; a tenant whose name were two terms would be two), so the
# subtraction is exact and never reaches a word outside the form.
names=""
for vf in "${vocab_files[@]}"; do
    n=${vf#examples/}; n=${n%/VOCABULARY}
    names="${names:+$names|}$n"
done
exempt_pattern="examples/($names)\\b|\\btenant_id *= *\"($names)\"${phrases:+|$phrases}"
exempt=""
for tier in "${TIERS[@]}"; do
    tier_exempt=$(grep -rIioHE \
        --include='*.rs' --include='*.ts' --include='*.svelte' \
        --include='*.toml' --include='*.sql' --include='*.sh' \
        --include='*.yaml' \
        -e "$exempt_pattern" -- "$tier" 2>/dev/null \
        | grep -vE '^[^:]*(/tests/|_test\.rs:|\.test\.[^/:]*:|\.spec\.[^/:]*:)' \
        || true)
    [ -n "$tier_exempt" ] && exempt="${exempt:+$exempt
}$tier_exempt"
done
# `<file> <weight>` per exempt occurrence: the literal's own hit count,
# looked up once per distinct literal (there are a handful).
exempt_weighted=$(
    declare -A weight
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        file=${line%%:*}; lit=${line#*:}
        if [ -z "${weight[$lit]+x}" ]; then
            weight[$lit]=$(printf '%s' "$lit" | grep -oiE -e "$pattern" | wc -l)
        fi
        printf '%s %s\n' "$file" "${weight[$lit]}"
    done <<< "$exempt"
)

# ---- the exemption: what a migration's DELETE names, it removes ----
# The text of every `DELETE FROM … ;` statement in a `.sql` file under
# a tier, counted with the same pattern and SUBTRACTED from that file:
# a word there is the leak leaving. Whole-line comments inside the
# statement are dropped the way the header above the statement is,
# so prose cannot ride the exemption; a statement is the lines from
# its `DELETE FROM` to the first line ending in `;`.
delete_exempt=$(
    for tier in "${TIERS[@]}"; do
        while IFS= read -r f; do
            [ -n "$f" ] || continue
            n=$(LC_ALL=C awk '
                tolower($0) ~ /^[ \t]*delete[ \t]+from[ \t]/ { inside = 1 }
                inside && $0 !~ /^[ \t]*--/ { print }
                inside && /;[ \t]*$/ { inside = 0 }
            ' "$f" | grep -oiE -e "$pattern" | wc -l | tr -d ' ')
            [ "$n" -gt 0 ] && printf '%s %s\n' "$f" "$n"
        done <<EOF
$(find "$tier" -name '*.sql' -type f 2>/dev/null | grep -vE '/tests/' | LC_ALL=C sort)
EOF
    done
)
exempt_weighted="${exempt_weighted}
${delete_exempt}"

# `<count>\t<file>` per file, descending — raw hits minus the exempt
# weight, files at zero dropped — and `<count> <tier>` per tier.
per_file=$(
    {
        printf '%s\n' "$hits" | sed '/^$/d' | cut -d: -f1 | awk '{ print $1, 1 }'
        printf '%s\n' "$exempt_weighted" | sed '/^$/d' | awk '{ print $1, -$2 }'
    } | awk '{ n[$1] += $2 } END { for (f in n) if (n[f] > 0) print n[f] "\t" f }' \
      | LC_ALL=C sort -t "$(printf '\t')" -k1,1nr -k2,2
)

# --measure: the same table as baseline entries, in PATH order and one
# blank line apart — the shape the baseline is kept in, for the reason
# its header gives (two cars lowering neighbouring files must not edit
# adjacent lines). A reading, not a verdict: it judges nothing.
if [ "$MEASURE" -eq 1 ]; then
    [ -n "$per_file" ] || exit 0
    LC_ALL=C sort -t "$(printf '\t')" -k2,2 <<< "$per_file" \
        | awk 'NR > 1 { print "" } { print }'
    exit 0
fi

# ---- the baseline ---------------------------------------------------
# One `<count>\t<path>` line per FILE (backlog e889cfa4) — see the
# header for why it is not one line per tier any longer. `#` lines and
# blank lines are skipped; the blank line between entries is deliberate.
[ -f "$BASELINE" ] || refuse "$BASELINE is missing — write one '<count><TAB><path>' line per file, from $0 --measure"

# A line that cannot be ratcheted is refused before any count is
# judged, because each of these would read as covering something while
# covering nothing: a malformed line (the old per-tier shape among
# them), a count of 0 (a clean file has no line), a path the lint does
# not scan, and a path named twice.
bad_lines=$(awk -F'\t' -v tiers="${TIERS[*]}" '
    BEGIN { nt = split(tiers, T, " ") }
    /^[ \t]*(#|$)/ { next }
    NF != 2 || $1 !~ /^[0-9]+$/ || $2 == "" || $2 ~ /[ \t]/ {
        printf "  line %d: %s — not <count><TAB><path>\n", FNR, $0; next
    }
    $1 + 0 == 0 {
        printf "  line %d: %s — a count of 0; a file with no words has no line\n", FNR, $2; next
    }
    {
        under = 0
        for (i = 1; i <= nt; i++) if (index($2, T[i] "/") == 1) under = 1
        if (!under) { printf "  line %d: %s — under no tier this lint scans\n", FNR, $2; next }
        if ($2 in seen) { printf "  line %d: %s — named more than once (first on line %d)\n", FNR, $2, seen[$2]; next }
        seen[$2] = FNR
    }
' "$BASELINE")
if [ -n "$bad_lines" ]; then
    printf '%s: REFUSED — %s has lines nobody can ratchet:\n%s\n' "$LINT" "$BASELINE" "$bad_lines" >&2
    printf '  Each line is <count><TAB><path> for one file under a tier, count above 0;\n' >&2
    printf '  %s --measure prints the table they are written from.\n' "$0" >&2
    exit 1
fi

# `<count>\t<path>` per baseline entry — validated above.
base_file=$(awk -F'\t' '!/^[ \t]*(#|$)/ { print $1 "\t" $2 }' "$BASELINE")

# The per-file verdicts: `GREW <count> <baseline> <path>` for a file
# above its line (an unlisted file's line is 0), `FELL …` for one below
# it (a listed file with no words is 0). Everything else is equal.
verdicts=$(awk -F'\t' '
    FNR == NR { if (NF == 2) base[$2] = $1 + 0; next }
    NF == 2 { cur[$2] = $1 + 0 }
    END {
        for (f in cur) { b = (f in base) ? base[f] : 0; if (cur[f] > b) print "GREW", cur[f], b, f }
        for (f in base) { c = (f in cur) ? cur[f] : 0; if (c < base[f]) print "FELL", c, base[f], f }
    }
' <(printf '%s\n' "$base_file") <(printf '%s\n' "$per_file") | LC_ALL=C sort -k4,4)

sum_under() { # <root> <table>: the counts of the table's paths under one tier
    awk -F'\t' -v r="$1/" 'index($2, r) == 1 { n += $1 } END { print n + 0 }' <<< "$2"
}
off_under() { # <root> <verdict>: how many files under one tier carry it
    awk -v r="$1/" -v v="$2" '$1 == v && index($4, r) == 1 { n++ } END { print n + 0 }' <<< "$verdicts"
}

# ---- the verdict: a table per tier, judged per file -----------------
echo "$LINT: tenant words above their tier ($tenants; $terms terms)"
echo
printf '  %-24s %7s %9s\n' tier count baseline
for tier in "${TIERS[@]}"; do
    count=$(sum_under "$tier" "$per_file")
    base=$(sum_under "$tier" "$base_file")
    grew=$(off_under "$tier" GREW); fell=$(off_under "$tier" FELL)
    note="at baseline"
    if [ "$grew" -gt 0 ] || [ "$fell" -gt 0 ]; then
        note="$grew file(s) above their line, $fell below"
    fi
    printf '  %-24s %7s %9s  %s\n' "$tier" "$count" "$base" "$note"
done
echo
echo "  top 10 files:"
# The limit lives in awk, never in a `| head` after a multi-line
# writer: under pipefail a reader that exits early SIGPIPEs the writer
# and the script reports 141 for a list that IS there (backlog 28af807c).
awk -F'\t' 'NR <= 10 { printf "  %6s  %s\n", $1, $2 }' <<< "$per_file"

grew=$(awk '$1 == "GREW" { n++ } END { print n + 0 }' <<< "$verdicts")
fell=$(awk '$1 == "FELL" { n++ } END { print n + 0 }' <<< "$verdicts")

if [ "$grew" -gt 0 ]; then
    echo >&2
    echo "$LINT: REFUSED — $grew file(s) carry more tenant vocabulary than their baseline line." >&2
    echo "  A tenant's words in a tier above it are a tenant assumption in core" >&2
    echo "  (CLAUDE.md §10). Move the code to crates/tenants/<engine> or to" >&2
    echo "  tenant data under examples/<tenant>/, or express it as a registry" >&2
    echo "  row the tenant seeds. The files that grew:" >&2
    awk '$1 == "GREW" {
        printf "    %s: %d word(s), baseline %d%s\n", $4, $2, $3, ($3 == 0 ? " — not in the baseline" : "")
    }' <<< "$verdicts" >&2
    echo "  The baseline in $BASELINE is never raised." >&2
fi

if [ "$fell" -gt 0 ]; then
    echo >&2
    echo "$LINT: REFUSED — $fell file(s) sit BELOW their baseline line. Good: the" >&2
    echo "  leak shrank. Now lower each file's line in $BASELINE, in this same" >&2
    echo "  car, so the ratchet holds the new number:" >&2
    awk '$1 == "FELL" {
        printf "    %s: %d word(s), baseline %d — %s\n", $4, $2, $3, ($2 == 0 ? "delete its line" : "set its line to " $2)
    }' <<< "$verdicts" >&2
    echo "  (A baseline above the count is refused so that nobody can raise one.)" >&2
fi

[ "$grew" -eq 0 ] && [ "$fell" -eq 0 ] || exit 1

echo
# The files the greps above read — the same seven extensions under the
# same tier roots, test files included (they are read and dropped after,
# as the count comment says). Files LOOKED AT, not files carrying a hit:
# a clean tree has none of the second and that is not a zero scan.
lint_scanned "$LINT" "$(find "${TIERS[@]}" -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.svelte' -o -name '*.toml' -o -name '*.sql' -o -name '*.sh' -o -name '*.yaml' \) 2>/dev/null | wc -l | tr -d ' ')" "file(s) read across ${#TIERS[@]} tier(s)"
echo "$LINT: clean (every file at its baseline line)"
