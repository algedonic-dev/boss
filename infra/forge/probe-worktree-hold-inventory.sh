#!/bin/bash
# Only native post-convergence acknowledged complete inventory can prove this car.
# This proves held-or-named-refused coverage, not a globally clean reclaim.
# The full native packet below retains unrelated problems and every refusal.
set -eu
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
wait_for() { printf 'not yet: %s\n' "$*"; exit 75; }
for tool in boss-sor-read jq date timeout sha256sum mktemp; do
    command -v "$tool" >/dev/null || wait_for "published tool absent: $tool"
done
case ${BOSS_CAR_CONVERGED_AT:-} in ''|*[!0-9]*) wait_for 'convergence epoch unavailable';; esac
frontier=$(date -u -d "@$BOSS_CAR_CONVERGED_AT" +%FT%TZ) || wait_for 'invalid convergence epoch'
evidence=$(mktemp -d) || wait_for 'cannot retain native evidence'
printf 'complete native inventory proof files=%s\n' "$evidence"
read_page() {
    timeout -k 1 30 boss-sor-read "$1" > "$2" 2> "$2.stderr" || wait_for "native read refused: $1; evidence=$2"
    jq_doc_file "$2" && jq -e -s 'length == 1 and (.[0] | type == "object")' "$2" >/dev/null || wait_for "not one native object: $2"
}
# Confirm the completion frontier actually filters, rather than silently falling back.
read_page '/api/jobs?kind=maintenance-dev-scratch-reclaim&step_completed_since=2999-01-01T00:00:00Z&limit=1' "$evidence/control.json"
jq_doc_file "$evidence/control.json" && jq -e '.total == 0 and .data == []' "$evidence/control.json" >/dev/null || wait_for 'completion-filter negative control failed'
offset=0; total=''; : > "$evidence/rows.jsonl"
while :; do
    [ "$offset" -lt 10000 ] || wait_for 'bounded native page budget exhausted'
    page="$evidence/page-$offset.json"
    read_page "/api/jobs?kind=maintenance-dev-scratch-reclaim&step_completed_since=$frontier&full=true&limit=100&offset=$offset" "$page"
    jq_doc_file "$page" && jq -e --argjson offset "$offset" '.full == true and .offset == $offset and .limit == 100 and (.total | type == "number" and floor == . and . >= 0) and (.data | type == "array")' "$page" >/dev/null || wait_for "invalid native page envelope: $page"
    count=$(jq '.data | length' "$page"); seen_total=$(jq '.total' "$page")
    [ -n "$total" ] || total=$seen_total
    [ "$total" = "$seen_total" ] || wait_for 'native total changed while reading; retry consistent snapshot'
    jq -c '.data[]' "$page" >> "$evidence/rows.jsonl"
    offset=$((offset+count))
    [ "$offset" -le "$total" ] || wait_for 'more native rows than declared'
    [ "$offset" -ne "$total" ] || break
    [ "$count" -gt 0 ] || wait_for 'native pagination truncated'
done
[ "$total" -gt 0 ] || wait_for 'no acknowledged maintenance after convergence'
jq_doc_file "$evidence/rows.jsonl" && jq -es --argjson total "$total" 'length == $total and ([.[].id] | unique | length) == $total' "$evidence/rows.jsonl" >/dev/null || wait_for 'duplicate or missing native rows'
# Floor passes never read this registry. Select the newest hourly attempt,
# including one still pending; never fall back to an older clean inventory.
if ! jq -s '
  def instant:
    if test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,9})?Z$")
    then capture("^(?<whole>[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(?:\\.(?<fraction>[0-9]{1,9}))?Z$")
    else error("malformed native admission instant") end
    | [(.whole + "Z" | fromdateiso8601), ((.fraction // "") + "000000000" | .[:9])];
  map(select(all(.steps[] | select(.spec_slug == "run");
    (.metadata.worktree_pass_reason // "" | startswith("scratch-floor:")) | not)))
  | if length == 0 then error("no hourly attempt") else . end
  | map(. + {_probe_order:(.opened_at | instant)}) | sort_by(._probe_order)
  | if length > 1 and .[-1]._probe_order == .[-2]._probe_order
    then error("latest attempt admission instant is ambiguous")
    else last | del(._probe_order) end
' "$evidence/rows.jsonl" > "$evidence/latest.json"; then
    wait_for 'native attempt timestamps malformed or latest admission ambiguous'
fi
jq_doc_file "$evidence/latest.json" && jq -e --argjson cut "$BOSS_CAR_CONVERGED_AT" '
  def epoch: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
  .partition == "real" and .simulated == false
  and (.opened_at | epoch) >= $cut
  and ([.steps[] | select(.spec_slug == "run")] | length) == 1
  and ([.steps[] | select(.spec_slug == "run")][0] |
    .status == "completed" and .completed_by == "automation:dev-scratch-reclaim"
    and (.completed_at | epoch) >= $cut
    and (.metadata.result == "ok" or .metadata.result == "incomplete")
    and (.metadata.problems | type == "string" and test("^[0-9]+$")))
' "$evidence/latest.json" >/dev/null || wait_for 'newest maintenance is pending, refused, failed, simulated or lacks native completion ACK'
jq '[.steps[] | select(.spec_slug == "run")][0].metadata' "$evidence/latest.json" > "$evidence/metadata.json"
jq_doc_file "$evidence/metadata.json" && jq -e --argjson cut "$BOSS_CAR_CONVERGED_AT" --argjson now "$(date +%s)" '
  def epoch: sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
  .worktree_hold_state == "observed"
  and .worktree_hold_complete == true
  and .worktree_hold_hash_format == "compact-sorted-keys-no-newline"
  and (.worktree_hold_at | type == "string")
  and (.worktree_hold_at | epoch) >= $cut and (.worktree_hold_at | epoch) <= $now
  and (.worktree_hold_inventory | type == "array" and length > 0)
  and (.worktree_hold_total | type == "number" and floor == .)
  and .worktree_hold_total == (.worktree_hold_inventory | length)
  and (.worktree_hold_held | type == "number" and floor == . and . > 0)
  and .worktree_hold_held == ([.worktree_hold_inventory[] | select(.state == "held")] | length)
  and (.worktree_hold_sha256 | type == "string" and test("^[0-9a-f]{64}$"))
  and all(.worktree_hold_inventory[];
    type == "object" and (keys | sort) == ["admin","git_admitted","path","state","why"]
    and (.state == "held" or .state == "absent" or .state == "unseen" or .state == "unpaired" or .state == "unheld" or .state == "unsafe-ancestor")
    and .git_admitted == (.state == "held")
    and (.path | type == "string" and startswith("/"))
    and (.admin | type == "string" and startswith("/"))
    and (.why | type == "string" and length > 0))
  and ([.worktree_hold_inventory[].admin] | unique | length) == .worktree_hold_total
  # Multiple admin entries may safely refuse one path. A path admitted
  # to Git must still identify exactly one registration.
  and (.worktree_hold_inventory | group_by(.path) |
    all(.[]; length == 1 or all(.[]; .git_admitted == false)))
' "$evidence/metadata.json" >/dev/null || wait_for 'missing, malformed, future, empty or incomplete full typed inventory'
jq -cS '.worktree_hold_inventory' "$evidence/metadata.json" | tr -d '\n' > "$evidence/canonical-inventory.json"
actual=$(sha256sum "$evidence/canonical-inventory.json"); actual=${actual%% *}
expected=$(jq -r '.worktree_hold_sha256' "$evidence/metadata.json")
[ "$actual" = "$expected" ] || wait_for 'whole native inventory hash mismatch'
cat "$evidence/latest.json"
printf '\nb11-f5-complete-acknowledged-post-convergence-inventory-v2 sha256=%s\n' "$actual"
