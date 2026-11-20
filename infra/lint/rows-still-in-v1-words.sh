#!/usr/bin/env bash
# rows-still-in-v1-words — how many station and workflow rows still
# carry the v1 words, and which files they are. It REPORTS; it does not
# fail on a v1 row yet.
#
# WHY (backlog 53abeb8d, David 2026-11-08 ~05:17Z): "systematically
# update protocols as we touch them to reflect the new design paradigm;
# departments are just the name now for the tier of stations under the
# Company station." The station tree bundle (design 4b27a65a,
# docs/design/factory-taxonomy-v2.md) is additive: the old keys stay
# while a live reader still reads them, and a row moves to the v2 words
# when a car touches it. Retirement that happens "as we touch them" is
# invisible unless something counts it, so this prints the count as
# `v1-rows:<n>` — a number that falls — and lists every file still
# carrying one. When it reaches zero, the car that retires the old
# readers flips this to a refusal.
#
# THE TWO VOCABULARIES, held here once. `boss brief` reads these four
# assignments out of THIS file into its `paradigm` invariant (so a
# builder is told the words this lint counts, never a retyped copy),
# and this lint refuses a V2_STATION_KEYS word the station row type
# (StationSpec in crates/core/boss-jobs/src/stations.rs) does not carry,
# so the words cannot drift from the code that reads the rows (CLAUDE.md
# §9a). Pinned by crates/core/boss-testing/tests/rows_still_in_v1_words.rs.
#
#   a station row in v1 words declares `department`, `floor` or `region`:
#     a department is the tier of stations directly under the root, read
#     off `parent`; `floor` and `region` are a drawing of the tree.
#   a workflow row in v1 words names no `metadata.owner` (its station),
#     or still carries `metadata.department` beside it
#     (infra/platform/workflows/README.md: it "stays beside it until the
#     department join reads the owner").
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the rows were read and counted (whatever the count)
#   1  a row file does not parse, the vocabulary names a v2 key the row
#      type lacks, or nothing was scanned (lib/scanned.sh)
#   3  python3 (3.11+, for tomllib) is not here: nothing was read
#
# USAGE
#   infra/lint/rows-still-in-v1-words.sh
set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3

LINT=rows-still-in-v1-words
V2_STATION_KEYS="id parent mode capacity allocation"
V1_STATION_KEYS="department floor region"
V2_WORKFLOW_KEY="owner"
V1_WORKFLOW_KEY="department"

command -v python3 >/dev/null 2>&1 || {
    echo "$LINT: CANNOT ANSWER — python3 (3.11+, for tomllib) is not on this box," >&2
    echo "  so no row was read. An infrastructure refusal, not a verdict." >&2
    exit 3
}

out=$(python3 - "$V2_STATION_KEYS" "$V1_STATION_KEYS" "$V2_WORKFLOW_KEY" "$V1_WORKFLOW_KEY" <<'PY'
import pathlib, re, sys, tomllib

v2_station, v1_station = sys.argv[1].split(), sys.argv[2].split()
v2_workflow, v1_workflow = sys.argv[3], sys.argv[4]

# The v2 words must be fields the station row type carries.
spec_rs = pathlib.Path("crates/core/boss-jobs/src/stations.rs")
src = spec_rs.read_text()
body = src.split("pub struct StationSpec", 1)
if len(body) != 2:
    print(f"REFUSE {spec_rs} declares no StationSpec; the v2 words cannot be checked")
    sys.exit(0)
body = body[1].split("\n}", 1)[0]
fields = set(re.findall(r"^\s*pub (\w+):", body, re.M))
for key in v2_station:
    if key not in fields:
        print(f"REFUSE V2_STATION_KEYS names `{key}`, which StationSpec ({spec_rs}) does not carry")

scanned = 0
for path in sorted(pathlib.Path("infra/platform/stations").glob("*.toml")):
    try:
        doc = tomllib.loads(path.read_text())
    except Exception as e:  # noqa: BLE001 - reported, never swallowed
        print(f"REFUSE {path} does not parse as TOML: {e}")
        continue
    for row in doc.get("station", []):
        scanned += 1
        old = [k for k in v1_station if k in row]
        if old:
            print(f"ROW station {path}: {' '.join(old)}")
for path in sorted(pathlib.Path("infra/platform/workflows").glob("*.toml")):
    try:
        doc = tomllib.loads(path.read_text())
    except Exception as e:  # noqa: BLE001
        print(f"REFUSE {path} does not parse as TOML: {e}")
        continue
    for row in doc.get("workflow", []):
        scanned += 1
        md = row.get("metadata", {})
        old = []
        if v2_workflow not in md:
            old.append(f"no metadata.{v2_workflow}")
        if v1_workflow in md:
            old.append(f"metadata.{v1_workflow}")
        if old:
            print(f"ROW workflow {path}: {', '.join(old)}")
print(f"SCANNED {scanned}")
PY
)
rc=$?
if [ "$rc" -ne 0 ]; then
    echo "$LINT: the row reader failed (exit $rc) — nothing was counted" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi

refusals=$(printf '%s\n' "$out" | grep '^REFUSE ' || true)
if [ -n "$refusals" ]; then
    printf '%s\n' "$refusals" | sed "s/^REFUSE /$LINT: /" >&2
    exit 1
fi

scanned=$(printf '%s\n' "$out" | sed -n 's/^SCANNED //p')
lint_scanned "$LINT" "$scanned" "station and workflow row(s)"

rows=$(printf '%s\n' "$out" | grep '^ROW ' || true)
n=0
[ -n "$rows" ] && n=$(printf '%s\n' "$rows" | wc -l | tr -d ' ')
stations=$(printf '%s\n' "$rows" | grep -c '^ROW station ' || true)
workflows=$(printf '%s\n' "$rows" | grep -c '^ROW workflow ' || true)
echo "$LINT: v1-rows:$n ($stations station, $workflows workflow) — a number that falls; reported, not refused, until the retirement car"
echo "  a touched row moves: station -> $V2_STATION_KEYS (not $V1_STATION_KEYS); workflow -> metadata.$V2_WORKFLOW_KEY (not metadata.$V1_WORKFLOW_KEY)"
[ -n "$rows" ] && printf '%s\n' "$rows" | sed 's/^ROW \(station\|workflow\) /  /'
exit 0
