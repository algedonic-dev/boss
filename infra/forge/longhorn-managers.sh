#!/usr/bin/env bash
# Fixed read-only observation for1462e441. Reasons are evidence, not a
# diagnosis inferred from simultaneous readiness transitions.
set -uo pipefail
ME=read-longhorn-managers
refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
unread() { echo "$ME: CANNOT ANSWER — $*" >&2; exit 1; }
[ "$#" -eq 0 ] || refuse 'no namespace, selector or command arguments are accepted'
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../estate/ops-credentials.sh"
. "$HERE/../lib/jq.sh"
command -v jq >/dev/null 2>&1 || unread 'jq is unavailable'
WORK="$(mktemp -d)" || unread 'cannot retain the observation'
trap 'rm -rf "$WORK"' EXIT
if ! ops_image_ready > "$WORK/image.out" 2> "$WORK/image.err"; then
    cat "$WORK/image.out" "$WORK/image.err" >&2
    unread 'the existing kubectl image door is unavailable'
fi
cat "$WORK/image.out" >&2
K="$(ops_kubectl)" || unread 'the existing kubectl door is unavailable'
# A known namespace on this same connection precedes absence conclusions.
# Each argv is fixed; no logs, exec, Secrets or mutation authority.
# shellcheck disable=SC2086
$K get namespace longhorn-system -o json --request-timeout=20s > "$WORK/namespace.json" 2> "$WORK/namespace.err"
code=$?
if [ "$code" -ne 0 ]; then cat "$WORK/namespace.err" >&2; unread 'namespace read failed'; fi
# shellcheck disable=SC2086
$K get daemonset longhorn-manager -n longhorn-system -o json --request-timeout=20s > "$WORK/daemonset.json" 2> "$WORK/daemonset.err"
code=$?
if [ "$code" -ne 0 ]; then cat "$WORK/daemonset.err" >&2; unread 'manager DaemonSet read failed'; fi
# shellcheck disable=SC2086
$K get pods -n longhorn-system -l app=longhorn-manager -o json --request-timeout=20s --chunk-size=64 > "$WORK/pods.json" 2> "$WORK/pods.err"
code=$?
if [ "$code" -ne 0 ]; then cat "$WORK/pods.err" >&2; unread 'manager pods read failed'; fi
# Events are retained history, not a promised time window. Their actual
# object UID/name/time/count stay in the output, including deleted pods.
# shellcheck disable=SC2086
$K get events -n longhorn-system -o json --request-timeout=20s --chunk-size=64 > "$WORK/events.json" 2> "$WORK/events.err"
code=$?
if [ "$code" -ne 0 ]; then cat "$WORK/events.err" >&2; unread 'namespace events read failed'; fi
for name in namespace daemonset pods events; do
    bytes="$(wc -c < "$WORK/$name.json")"
    [ "$bytes" -le 1048576 ] || unread "$name exceeds the1MiB input bound; observation incomplete"
    jq_doc_file "$WORK/$name.json" || unread "$name is not one complete JSON document"
done
if ! jq -en --slurpfile ns "$WORK/namespace.json" --slurpfile ds "$WORK/daemonset.json" \
    --slurpfile pods "$WORK/pods.json" --slurpfile events "$WORK/events.json" \
    -f "$HERE/longhorn-managers.jq" > "$WORK/result.json" 2> "$WORK/result.err"; then
    cat "$WORK/result.err" >&2
    unread 'incomplete, malformed or mismatched native identities; values UNKNOWN'
fi
# The ops runner has its own output ceiling. Refuse instead of emitting a
# valid-looking prefix that could lose the last manager or event.
[ "$(wc -c < "$WORK/result.json")" -le 98304 ] || unread 'observation exceeds96KiB output bound; no complete observation available'
cat "$WORK/result.json"
