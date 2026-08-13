#!/usr/bin/env bash
#
# invariant-register — the shape check on docs/invariants.toml.
#
# THE PRINCIPLE
# -------------
# Every load-bearing invariant declares how it is held
# (docs/design/design-conformance.md). The failure this guards against
# is not a wrong claim — it is an UNDECLARED one: "a claim with no
# enforcement is indistinguishable, at a glance, from one with
# enforcement." A register that lets an entry omit its enforcement
# class, or name a lint that was deleted two cars ago, reintroduces
# exactly the ambiguity it was written to remove.
#
# THE CHECKED PROPERTY
# --------------------
# The SHAPE of the register, never the truth of a claim. Specifically:
#
#   * every entry carries all seven keys, and no others;
#   * `id` is a unique, citable slug (a conformance finding cites an
#     id, so a duplicate silently merges two histories);
#   * `claim`, `source` and `enforcement` are non-empty, and
#     `enforcement` is one of enforced / checked / unenforced;
#   * enforced   → `mechanism` names a path that EXISTS ON DISK. This
#                  is the one check with teeth against rot: a lint
#                  deleted or renamed without touching the register
#                  fails here, by id.
#   * checked    → `mechanism` says how it is verified, and
#                  `last_verified` is a real YYYY-MM-DD date.
#   * unenforced → `note` says why, and `mechanism` is EMPTY. An
#                  unenforced invariant that names a mechanism is
#                  claiming enforcement it does not have.
#
# What this deliberately does NOT do is grade the strength of the
# declaration. An author may write `unenforced`, and that honesty is
# the point — anything else turns the rule into pressure to fake
# enforcement (design-conformance Q2).
#
# Following the no-secrets.sh precedent of checks that prove
# themselves, every run starts with a self-test: one planted fixture
# per malformed shape, each asserted caught by id AND by field, plus a
# well-formed fixture asserted clean. `--self-test` runs just that and
# stops.
#
# bash 3.2 safe: no associative arrays, no mapfile, no ${x,,}. The
# TOML subset parsed here is the one the register is written in —
# `[[invariant]]` headers and single-line `key = "value"` pairs.
#
# Usage: infra/lint/invariant-register.sh [--self-test]
# Exit:  0 clean / 1 findings or self-test failure

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

REGISTER="docs/invariants.toml"
REQUIRED_KEYS="id claim source enforcement mechanism last_verified note"

ABSENT="<absent>"

# ---------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------
# Findings name the file, the line the entry starts on, the id, and the
# field — "fail with the offending id and field" is the whole contract
# of an error message here. An entry whose id is itself missing reports
# as <no id> so the line number is still actionable.
FINDINGS=0

finding() {
    # finding <file> <line> <id> <field> <message>
    local file="$1" line="$2" id="$3" field="$4" msg="$5"
    # An entry whose id is missing or empty still has to report
    # something citable, so the line number carries it.
    if [ -z "$id" ] || [ "$id" = "$ABSENT" ]; then
        id="<no id>"
    fi
    echo "  ${file}:${line}: ${id}: ${field}: ${msg}"
    FINDINGS=$((FINDINGS + 1))
}

# ---------------------------------------------------------------------
# Trim helpers (portable to bash 3.2)
# ---------------------------------------------------------------------
ltrim() { printf '%s' "${1#"${1%%[![:space:]]*}"}"; }
rtrim() { printf '%s' "${1%"${1##*[![:space:]]}"}"; }

# ---------------------------------------------------------------------
# Per-entry validation
# ---------------------------------------------------------------------
# Reads the seven field variables set by the parser. Kept separate from
# the parse loop so the rules read as a list rather than as control
# flow.
validate_entry() {
    local file="$1" line="$2"

    # --- presence + non-emptiness of the always-required three -------
    local key val
    for key in $REQUIRED_KEYS; do
        eval "val=\${f_$key}"
        if [ "$val" = "$ABSENT" ]; then
            finding "$file" "$line" "$f_id" "$key" "required key is missing"
        fi
    done

    # An entry with no id cannot be cited; everything below still runs
    # so one malformed entry reports all its problems at once.
    if [ "$f_id" != "$ABSENT" ]; then
        if [ -z "$f_id" ]; then
            finding "$file" "$line" "" "id" "must not be empty"
        elif ! printf '%s' "$f_id" | grep -qE '^[a-z0-9][a-z0-9-]*$'; then
            finding "$file" "$line" "$f_id" "id" \
                "must be a lowercase slug (a-z, 0-9, dashes) so findings can cite it"
        elif printf '%s\n' "$SEEN_IDS" | grep -qx -- "$f_id"; then
            finding "$file" "$line" "$f_id" "id" \
                "duplicate id — an id is cited by conformance findings and must be unique"
        fi
        SEEN_IDS="${SEEN_IDS}
${f_id}"
    fi

    if [ "$f_claim" != "$ABSENT" ] && [ -z "$f_claim" ]; then
        finding "$file" "$line" "$f_id" "claim" "must not be empty"
    fi
    if [ "$f_source" != "$ABSENT" ] && [ -z "$f_source" ]; then
        finding "$file" "$line" "$f_id" "source" \
            "must name the doc or file that states the claim"
    fi

    # --- the enforcement class dictates the rest ---------------------
    case "$f_enforcement" in
        "$ABSENT")
            : ;;  # already reported as a missing key
        enforced)
            if [ -z "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = enforced requires a lint/test path"
            elif [ ! -e "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = enforced names '${f_mechanism}', which does not exist on disk"
            fi
            ;;
        checked)
            if [ -z "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = checked requires the verification method"
            fi
            if [ -z "$f_last_verified" ]; then
                finding "$file" "$line" "$f_id" "last_verified" \
                    "enforcement = checked requires a date — an unverified 'checked' is an 'unenforced'"
            elif ! printf '%s' "$f_last_verified" | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
                finding "$file" "$line" "$f_id" "last_verified" \
                    "must be YYYY-MM-DD, got '${f_last_verified}'"
            fi
            ;;
        unenforced)
            if [ -z "$f_note" ]; then
                finding "$file" "$line" "$f_id" "note" \
                    "enforcement = unenforced requires a note saying why, or what would enforce it"
            fi
            if [ -n "$f_mechanism" ]; then
                finding "$file" "$line" "$f_id" "mechanism" \
                    "enforcement = unenforced must leave mechanism empty — naming one claims enforcement it does not have"
            fi
            ;;
        *)
            finding "$file" "$line" "$f_id" "enforcement" \
                "must be enforced, checked or unenforced, got '${f_enforcement}'"
            ;;
    esac
}

reset_entry() {
    f_id="$ABSENT";        f_claim="$ABSENT"
    f_source="$ABSENT";    f_enforcement="$ABSENT"
    f_mechanism="$ABSENT"; f_last_verified="$ABSENT"
    f_note="$ABSENT"
}

# ---------------------------------------------------------------------
# Parse + check one register file
# ---------------------------------------------------------------------
# Returns 0 clean, 1 if anything was reported. Findings land on stdout
# so both the gate and the self-test read them the same way.
check_file() {
    local file="$1"
    local lineno=0 entry_line=0 in_entry=0 entries=0
    local line key val

    FINDINGS=0
    SEEN_IDS=""
    reset_entry

    if [ ! -f "$file" ]; then
        echo "  ${file}: register file not found"
        return 1
    fi

    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno + 1))
        line=$(ltrim "$line")

        case "$line" in
            '')   continue ;;
            '#'*) continue ;;
            '[[invariant]]')
                if [ "$in_entry" -eq 1 ]; then
                    validate_entry "$file" "$entry_line"
                fi
                reset_entry
                in_entry=1
                entry_line="$lineno"
                entries=$((entries + 1))
                continue
                ;;
        esac

        case "$line" in
            *=*)
                key=$(rtrim "${line%%=*}")
                val=$(ltrim "${line#*=}")
                # Strip the surrounding double quotes of a TOML basic
                # string. Inner escaped quotes are irrelevant here —
                # this lint only ever asks whether a value is empty.
                case "$val" in
                    '"'*'"') val="${val#\"}"; val="${val%\"}" ;;
                esac

                if [ "$in_entry" -eq 0 ]; then
                    finding "$file" "$lineno" "" "$key" \
                        "key sits outside any [[invariant]] entry"
                    continue
                fi

                case "$key" in
                    id)            f_id="$val" ;;
                    claim)         f_claim="$val" ;;
                    source)        f_source="$val" ;;
                    enforcement)   f_enforcement="$val" ;;
                    mechanism)     f_mechanism="$val" ;;
                    last_verified) f_last_verified="$val" ;;
                    note)          f_note="$val" ;;
                    *)
                        finding "$file" "$lineno" "$f_id" "$key" \
                            "unknown key — the register's fields are: ${REQUIRED_KEYS}"
                        ;;
                esac
                ;;
            *)
                finding "$file" "$lineno" "$f_id" "<line>" \
                    "not a comment, an [[invariant]] header, or a key = \"value\" pair"
                ;;
        esac
    done < "$file"

    if [ "$in_entry" -eq 1 ]; then
        validate_entry "$file" "$entry_line"
    fi

    if [ "$entries" -eq 0 ]; then
        echo "  ${file}: register holds no [[invariant]] entries"
        return 1
    fi

    [ "$FINDINGS" -eq 0 ]
}

# ---------------------------------------------------------------------
# Self-test — plant each malformed shape, assert it is caught
# ---------------------------------------------------------------------
# Every rule above gets a fixture that violates it and nothing else, so
# a fixture that stops failing means that rule stopped working. The
# assertion is on the ID AND THE FIELD, not merely on the exit code: a
# check that fails without naming what to fix is a check nobody can
# act on.
ST_FAILS=0
ST_RUN=0

# A well-formed entry, used as the body every fixture mutates. The
# mechanism path is this script itself, so the enforced-path rule has
# something real to find.
valid_entry() {
    cat <<'ENTRY'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture, not a real invariant."
source = "infra/lint/invariant-register.sh"
enforcement = "enforced"
mechanism = "infra/lint/invariant-register.sh"
last_verified = ""
note = ""
ENTRY
}

# assert_caught <label> <expect-id> <expect-field> <<fixture
assert_caught() {
    local label="$1" want_id="$2" want_field="$3"
    local tmp out rc
    ST_RUN=$((ST_RUN + 1))
    tmp=$(mktemp)
    cat > "$tmp"
    out=$(check_file "$tmp"); rc=$?
    rm -f "$tmp"

    if [ "$rc" -eq 0 ]; then
        echo "invariant-register self-test FAIL: ${label} — malformed shape was accepted" >&2
        ST_FAILS=1
        return
    fi
    if ! printf '%s' "$out" | grep -q "${want_id}: ${want_field}:"; then
        echo "invariant-register self-test FAIL: ${label} — caught, but did not name '${want_id}: ${want_field}'; said:" >&2
        printf '%s\n' "$out" >&2
        ST_FAILS=1
    fi
}

assert_clean() {
    local label="$1"
    local tmp out rc
    ST_RUN=$((ST_RUN + 1))
    tmp=$(mktemp)
    cat > "$tmp"
    out=$(check_file "$tmp"); rc=$?
    rm -f "$tmp"
    if [ "$rc" -ne 0 ]; then
        echo "invariant-register self-test FAIL: ${label} — well-formed fixture was rejected:" >&2
        printf '%s\n' "$out" >&2
        ST_FAILS=1
    fi
}

self_test() {
    # Positive control first: if this fails, every negative below is
    # meaningless.
    assert_clean "a well-formed entry passes" < <(valid_entry)

    assert_caught "missing required key" fixture-one source <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "missing id" '<no id>' id <<'EOF'
[[invariant]]
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "empty claim" fixture-one claim <<'EOF'
[[invariant]]
id = "fixture-one"
claim = ""
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "duplicate id" fixture-one id < <(valid_entry; valid_entry)

    assert_caught "unknown enforcement class" fixture-one enforcement <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "mostly"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "enforced with no mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "enforced"
mechanism = ""
last_verified = ""
note = ""
EOF

    assert_caught "enforced naming a path that does not exist" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "enforced"
mechanism = "infra/lint/deleted-two-cars-ago.sh"
last_verified = ""
note = ""
EOF

    assert_caught "checked with no last_verified" fixture-one last_verified <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = "read it by hand"
last_verified = ""
note = ""
EOF

    assert_caught "checked with a non-date last_verified" fixture-one last_verified <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = "read it by hand"
last_verified = "last August"
note = ""
EOF

    assert_caught "checked with no mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "checked"
mechanism = ""
last_verified = "2026-08-13"
note = ""
EOF

    assert_caught "unenforced with no note" fixture-one note <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = ""
EOF

    assert_caught "unenforced still naming a mechanism" fixture-one mechanism <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = "infra/lint/invariant-register.sh"
last_verified = ""
note = "why not"
EOF

    assert_caught "an id that cannot be cited" 'Fixture One' id <<'EOF'
[[invariant]]
id = "Fixture One"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "a stray key outside any entry" '<no id>' orphan <<'EOF'
orphan = "adrift"
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
EOF

    assert_caught "an unknown key inside an entry" fixture-one owner <<'EOF'
[[invariant]]
id = "fixture-one"
claim = "A planted fixture."
source = "nowhere"
enforcement = "unenforced"
mechanism = ""
last_verified = ""
note = "why not"
owner = "nobody"
EOF

    # An empty register is a register that checks nothing — the failure
    # mode where the file survives but its content is gone.
    ST_RUN=$((ST_RUN + 1))
    local tmp rc
    tmp=$(mktemp)
    echo '# no entries here' > "$tmp"
    check_file "$tmp" >/dev/null; rc=$?
    rm -f "$tmp"
    if [ "$rc" -eq 0 ]; then
        echo "invariant-register self-test FAIL: an empty register was accepted" >&2
        ST_FAILS=1
    fi

    if [ "$ST_FAILS" -ne 0 ]; then
        echo "invariant-register: self-test FAILED — the shape checks cannot be trusted, fix them first" >&2
        exit 1
    fi
    echo "invariant-register: self-test ok — ${ST_RUN}/${ST_RUN} planted shapes caught by id and field, well-formed fixture clean"
}

# ---------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------
main_scan() {
    local out rc
    out=$(check_file "$REGISTER"); rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "FAIL — ${REGISTER} is malformed:"
        printf '%s\n' "$out"
        echo ""
        echo "Every invariant declares how it is held: enforced (name the lint/test"
        echo "path — it must exist), checked (name the method + the date it was last"
        echo "verified), or unenforced (say why, and leave mechanism empty). Writing"
        echo "'unenforced' is always allowed; leaving the question open is not."
        exit 1
    fi

    # check_file runs in a subshell, so the census is recomputed here
    # rather than carried out of it.
    local entries enforced checked unenforced
    entries=$(grep -c '^\[\[invariant\]\]' "$REGISTER")
    enforced=$(grep -c '^enforcement = "enforced"' "$REGISTER")
    checked=$(grep -c '^enforcement = "checked"' "$REGISTER")
    unenforced=$(grep -c '^enforcement = "unenforced"' "$REGISTER")
    echo "invariant-register: ok — ${entries} invariants declared (${enforced} enforced, ${checked} checked, ${unenforced} unenforced)"
}

case "${1:-}" in
    --self-test)
        self_test ;;
    '')
        self_test
        main_scan ;;
    *)
        echo "usage: infra/lint/invariant-register.sh [--self-test]" >&2
        exit 2 ;;
esac
