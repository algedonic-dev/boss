#!/usr/bin/env bash
#
# a-rule-edit-bumps-its-version — a dispatcher rule file whose CONTENT
# changes must raise its `version`, or the change never goes live.
#
# THE INCIDENT
# ------------
# Measured 2026-09-29 (backlog 732c3cf9). The dispatcher's boot seed
# (crates/core/boss-dispatcher/src/rules/seed.rs) publishes each file
# under infra/dispatcher/rules/ at the version the file declares, and
# touches NOTHING that already exists at that version — the append-only
# contract every versioned registry here keeps. So a file edited at the
# version its row already holds is kept in the tree and ignored by the
# system. Four were: car e84793768 (train #819) added `request_id` to
# the two rules that mint the org-admin GitHub App token, and #808 their
# org filter, all at v1 — the live rows stayed the v1 of 00:14:57Z, the
# 15:40:12Z firing minted into the shared `token` key, and the plan verb
# found its per-request key empty. #753 and ae1e618af did the same to
# department-retros-weekly and open-a-packet-when-an-event-is-dead-
# lettered. Every gate was green, and the boot line said "already
# matches the authored directory".
#
# The seed now names such a rule at every boot and files an alarm. That
# is detection AFTER the merge; this is the same finding at gate time,
# before a car can carry it — the migrations-append-only pattern.
#
# THE CHECKED PROPERTY
# --------------------
# Against the merge-base with the trunk, for every rule (by NAME, so a
# rule moved to another file is still the same rule) present on both
# sides of a changed file under infra/dispatcher/rules/:
#   - if anything but `why` and `version` differs — the trigger, the
#     schedule, `when`, `do`, `delay` — the version must be HIGHER;
#   - the version may never go DOWN (the seed never walks one back, so
#     a lowered version is dead text too).
# Comments and `why` are free: the row stores neither, so an edit to
# them changes nothing the seed compares and needs no bump. A new rule
# and a deleted one are free: the seed inserts and retires those.
#
# Usage: infra/lint/a-rule-edit-bumps-its-version.sh
# Exit:  0 clean / 1 a violation, or no trunk ref in this checkout /
#        3 git or python3 could not answer, so NOTHING was judged — an
#        infrastructure refusal, see lib/git-answer.sh

set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3

LINT=a-rule-edit-bumps-its-version
RULES_DIR="infra/dispatcher/rules"

command -v python3 >/dev/null 2>&1 || {
    echo "$LINT: CANNOT ANSWER — python3 (3.11+, for tomllib) is not on this box," >&2
    echo "  so no rule file was compared. An infrastructure refusal, not a verdict." >&2
    exit "$LINT_CANNOT_ANSWER"
}

# A ratchet that cannot see the trunk certifies nothing, so an ABSENT
# trunk refuses (1); a git that could not answer is the machine's (3).
BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates))" >&2
    echo "  Cannot tell which rule edits are new without one. Fetch the trunk." >&2
    exit 1
fi
mb=$(resolve_merge_base "$LINT" "$BASE" HEAD) || exit $?

# --no-renames: a rename is a deletion plus an addition, and the rules
# are paired by NAME below, so a rule moved between files is compared.
diff_out=$(git_answer "$LINT" 0 diff --no-renames --name-status "$mb" HEAD -- "$RULES_DIR") || exit $?

work=$(mktemp -d "${TMPDIR:-/tmp}/$LINT.XXXXXX") || {
    echo "$LINT: CANNOT ANSWER — no writable temp dir for the two sides." >&2
    exit "$LINT_CANNOT_ANSWER"
}
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/base" "$work/head"

changed=0
while read -r status file; do
    [ -n "${file:-}" ] || continue
    case "$file" in *.toml) ;; *) continue ;; esac
    changed=$((changed + 1))
    name=$(basename "$file")
    case "$status" in
        M*|D*)
            git_answer "$LINT" 0 show "$mb:$file" > "$work/base/$name" || exit $?
            ;;
    esac
    case "$status" in
        M*|A*)
            git_answer "$LINT" 0 show "HEAD:$file" > "$work/head/$name" || exit $?
            ;;
    esac
done <<EOF
$diff_out
EOF

if [ "$changed" -eq 0 ]; then
    echo "$LINT: clean — no rule file changed against $BASE"
    exit 0
fi

python3 - "$work/base" "$work/head" <<'PY'
import pathlib, sys, tomllib

def load(d, errors):
    rules = {}
    for p in sorted(pathlib.Path(d).glob("*.toml")):
        try:
            doc = tomllib.loads(p.read_text())
        except Exception as e:  # noqa: BLE001 - reported, never swallowed
            errors.append(f"VIOLATION: {p.name} does not parse as TOML: {e}")
            continue
        for rule in doc.get("rule", []):
            rules[rule.get("name")] = (p.name, rule)
    return rules

def content(rule):
    # What the seed publishes into the row, and so what it compares:
    # everything but the reviewed prose and the version itself.
    return {k: v for k, v in rule.items() if k not in ("why", "version")}

def judge():
    # A base file that will not parse is the trunk's problem, not this
    # branch's: its rules are simply not compared.
    base = load(sys.argv[1], [])
    found = []
    head = load(sys.argv[2], found)
    compared = 0
    for name, (fname, h) in sorted(head.items()):
        if name not in base:
            continue
        compared += 1
        b = base[name][1]
        bv, hv = b.get("version", 1), h.get("version", 1)
        if hv < bv:
            found.append(
                f"VIOLATION: {fname}: rule `{name}` goes from v{bv} to v{hv}. The seed never "
                f"walks a version back, so this file would be dead text; raise it instead."
            )
            continue
        bc, hc = content(b), content(h)
        if bc != hc and hv == bv:
            fields = ", ".join(sorted(k for k in set(bc) | set(hc) if bc.get(k) != hc.get(k)))
            found.append(
                f"VIOLATION: {fname}: rule `{name}` changes {fields} at v{bv} without a "
                f"version bump. The seed keeps the row it already holds at v{bv}, so this "
                f"edit would NEVER go live (backlog 732c3cf9). Set `version = {bv + 1}`."
            )
    for line in found:
        print(line)
    print(f"a-rule-edit-bumps-its-version: compared {compared} rule(s) present on both sides")
    return 1 if found else 0

try:
    status = judge()
except Exception as e:  # noqa: BLE001 - a crash is "could not judge", never a verdict
    print(f"a-rule-edit-bumps-its-version: the comparison crashed: {e!r}", file=sys.stderr)
    status = 3
sys.exit(status)
PY
py=$?
case "$py" in
    0)
        echo "$LINT: clean — $changed changed rule file(s) against $BASE, every content edit raised its version"
        exit 0
        ;;
    1)
        echo "$LINT: a rule's content changed without a version bump; see above" >&2
        exit 1
        ;;
    *)
        echo "$LINT: CANNOT ANSWER — the comparison itself exited $py, so nothing was judged." >&2
        exit "$LINT_CANNOT_ANSWER"
        ;;
esac
