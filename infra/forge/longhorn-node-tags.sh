#!/usr/bin/env bash
# Read the Longhorn node's tags, not its disks' tags or the Kubernetes
# default-node-tags annotation (90b6c292; e14a472d Option C).
# A complete node list establishes absence; an error mentioning
# NotFound does not. Only two fixed GETs use the existing forge door.
set -uo pipefail

ME=read-longhorn-node-tags
refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
unread() { echo "$ME: CANNOT ANSWER — $*" >&2; exit 1; }
NODE_ID_RE='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
[ "$#" -eq 1 ] || refuse 'exactly one node DNS label is required'
[[ "$1" =~ $NODE_ID_RE ]] || refuse 'the node must be one lowercase DNS label'
NODE="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
. "$HERE/../lib/jq.sh"
command -v jq >/dev/null 2>&1 || unread 'jq is unavailable'
WORK="$(mktemp -d)" || unread 'cannot retain the native reads'
trap 'rm -rf "$WORK"' EXIT
if ! ops_image_ready > "$WORK/image.out" 2> "$WORK/image.err"; then
    cat "$WORK/image.out" "$WORK/image.err" >&2
    unread 'the existing kubectl image door is unavailable'
fi
cat "$WORK/image.out" >&2
K="$(ops_kubectl)" || unread 'the existing kubectl door is unavailable'

# A known-positive namespace on the SAME connection precedes any absence
# conclusion. Empty, malformed or multiple documents cannot be evidence.
# shellcheck disable=SC2086 # the trusted door prints one fixed argv
if ! $K get namespace longhorn-system -o json --request-timeout=20s \
    > "$WORK/namespace.json" 2> "$WORK/namespace.err"; then
    cat "$WORK/namespace.err" >&2
    unread 'the Longhorn namespace could not be read'
fi
if ! jq_doc_file "$WORK/namespace.json" || ! jq -es 'length == 1 and (.[0] | .apiVersion == "v1" and .kind == "Namespace"
    and .metadata.name == "longhorn-system"
    and (.metadata.uid | type == "string" and length > 0))' \
    "$WORK/namespace.json" > /dev/null 2> "$WORK/namespace.jq"; then
    cat "$WORK/namespace.jq" >&2
    unread 'the same-connection namespace control is not a complete native Namespace'
fi

# No packet-selected namespace, resource, JSONPath or proxy endpoint.
# kubectl wraps collection output as v1/List; typed list items also
# need no per-item TypeMeta. A continuation or invalid
# item would make selecting zero nodes ambiguous and must refuse instead.
# shellcheck disable=SC2086
if ! $K get nodes.longhorn.io -n longhorn-system -o json --request-timeout=20s --chunk-size=64 \
    > "$WORK/nodes.json" 2> "$WORK/nodes.err"; then
    cat "$WORK/nodes.err" >&2
    unread 'the Longhorn node list could not be read'
fi
if ! jq_doc_file "$WORK/nodes.json" || ! jq -es 'length == 1 and (.[0] |
    ((.apiVersion == "longhorn.io/v1beta2" and .kind == "NodeList")
     or (.apiVersion == "v1" and .kind == "List"))
    and (.metadata.continue == null or .metadata.continue == "")
    and (.items | type == "array")
    and all(.items[]; type == "object" and .metadata.namespace == "longhorn-system"
        and (.metadata.name | type == "string" and length > 0)
        and (.metadata.uid | type == "string" and length > 0)))' \
    "$WORK/nodes.json" > /dev/null 2> "$WORK/nodes.jq"; then
    cat "$WORK/nodes.jq" >&2
    unread 'the Longhorn response is not one complete node list'
fi
COUNT="$(jq --arg node "$NODE" '[.items[] | select(.metadata.name == $node)] | length' "$WORK/nodes.json")"
if [ "$COUNT" = 0 ]; then
    jq -n --arg node "$NODE" --slurpfile ns "$WORK/namespace.json" \
        '{state:"NOT FOUND", node:$node, namespace:"longhorn-system", namespace_uid:$ns[0].metadata.uid}'
    exit 0
fi
[ "$COUNT" = 1 ] || unread 'the named Longhorn node is ambiguous'
jq --arg node "$NODE" '.items[] | select(.metadata.name == $node)' "$WORK/nodes.json" > "$WORK/node.json"
if ! jq_doc_file "$WORK/node.json" || ! jq -e 'def tags: type == "array" and all(.[]; type == "string");
    (.spec.tags | tags) and (.spec.allowScheduling | type == "boolean")
    and (.spec.disks | type == "object")
    and all(.spec.disks[]; type == "object" and (.tags | tags)
        and (.allowScheduling | type == "boolean"))' \
    "$WORK/node.json" > /dev/null 2> "$WORK/fields.jq"; then
    cat "$WORK/fields.jq" >&2
    unread 'node tags, scheduling or disk tags are missing or malformed; their values are UNKNOWN'
fi
jq --slurpfile ns "$WORK/namespace.json" '{state:"observed", node:.metadata.name,
    node_uid:.metadata.uid, namespace:.metadata.namespace, namespace_uid:$ns[0].metadata.uid,
    tags:.spec.tags, allowScheduling:.spec.allowScheduling,
    disks:(.spec.disks | to_entries | sort_by(.key) | map({name:.key,
        tags:.value.tags, allowScheduling:.value.allowScheduling}))}' "$WORK/node.json"
