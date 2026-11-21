#!/usr/bin/env bash
# A fixed Longhorn trimFilesystem action on one instance claim, never an
# operator-supplied URL or command (backlog b1520f80 part b; the logic and
# its refusals are instance-volume-trim.py). The effect line is printed
# HERE, and only from the record the Python's read-back wrote — the one
# place its text lives, which trim-instance-volume.json's `effect` matches.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
if ! instances="$("$REPO/infra/cluster/render-instance.sh" --instances)"; then
    echo 'instance-volume-trim: CANNOT ANSWER — instance registry unread' >&2
    exit 1
fi
INSTANCE_TRIM_NAMESPACES="$(printf '%s\n' "$instances" | cut -f2)"
export INSTANCE_TRIM_NAMESPACES
ops_image_ready >&2 || exit 1
INSTANCE_TRIM_KUBECTL="$(ops_kubectl)" || exit 1
export INSTANCE_TRIM_KUBECTL
# The plan's stdout is the signed bytes: handed through untouched.
if [ "${1-}" = --plan ]; then
    exec python3 -B "$HERE/instance-volume-trim.py" "$@"
fi
outcome="$(python3 -B "$HERE/instance-volume-trim.py" "$@")"
rc=$?
printf '%s\n' "$outcome"
[ "$rc" -eq 0 ] || exit "$rc"
# jq-1.6's -e passes an input with no document (backlog d96e38ab): an
# adapter that exited 0 and printed nothing is no read-back.
if ! jq_doc_text "$outcome" || ! line="$(printf '%s\n' "$outcome" | jq -er 'select(.state == "trim-acknowledged"
        and (.volume.name | type) == "string" and (.settle_seconds | type) == "number"
        and (.actual_size_before | type) == "number" and (.actual_size_after | type) == "number")
    | "instance-volume-trim: Longhorn acknowledged trimFilesystem on \(.volume.name); status.actualSize read back \(.actual_size_before) -> \(.actual_size_after) bytes over \(.settle_seconds) s; per-disk free space is not read here"')"; then
    echo 'instance-volume-trim: FAILED — trim requested; its read-back record is unread, effect NOT proven' >&2
    exit 1
fi
printf '%s\n' "$line"
