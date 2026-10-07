#!/usr/bin/env bash
# Approved c6f08b60 D4: stage an explicit deployment declaration for
# the generic static reader. Validate a bounded snapshot before apply;
# a missing/failed input never means that this install declares none.
set -euo pipefail
# Keep the shared document guard even though the slurp cardinality also refuses silence.
. "$(dirname "$0")/../lib/jq.sh"
if [ "$#" -ne 2 ]; then echo 'stage-dev-door: expected source and staging directory' >&2; exit 2; fi
source_file="$1"
stage_dir="$2"
mkdir -p "$stage_dir"
snapshot=$(mktemp "$stage_dir/.dev-door.XXXXXX")
trap 'rm -f "$snapshot"' EXIT
if ! cat "$source_file" > "$snapshot"; then echo 'stage-dev-door: declaration unreadable; not staged' >&2; exit 2; fi
size=$(wc -c < "$snapshot")
if [ "$size" -gt 65536 ]; then echo 'stage-dev-door: declaration exceeds 64 KiB; not staged' >&2; exit 2; fi
if ! jq_doc_file "$snapshot"; then echo 'stage-dev-door: declaration unreadable or absent; not staged' >&2; exit 2; fi
if ! jq -es '
  def text: type == "string" and length <= 8192 and test("\\S");
  def steps: type == "array" and length <= 32 and all(.[];
    type == "object" and (.what | text) and (.command | text) and (.why | text));
  # A label end anchor accepts a final LF; refuse unsupported host bytes
  # before rendering an unreadable replacement (review 75129737).
  length == 1 and (.[0] |
    type == "object" and
    ((has("host") | not) or .host == null or
      (.host | type == "string" and length <= 253 and
        (test("[^a-zA-Z0-9.-]") | not) and
        (split(".") | all(.[]; test("^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$"))))) and
    ((has("steps") | not) or (.steps | steps)) and
    (.host == null or (.steps | type == "array" and length > 0)))
' "$snapshot" >/dev/null; then echo 'stage-dev-door: declaration incomplete or malformed; not staged' >&2; exit 2; fi
mv "$snapshot" "$stage_dir/dev-door.json"
echo 'stage-dev-door: exact validated declaration staged'
