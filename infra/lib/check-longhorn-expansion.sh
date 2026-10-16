#!/usr/bin/env bash
# One JSON input, the existing checked Longhorn arithmetic. No cluster access.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/longhorn-ledger.sh"
. "$HERE/jq.sh"
input=$(cat)
jq_doc_text "$input" || { echo 'Longhorn admission requires one JSON document' >&2; exit 78; }
printf '%s' "$input" | jq -e -c -s "$LONGHORN_LEDGER_JQ"'
    if length != 1 then error("Longhorn admission requires exactly one document") else .[0] end |
    . as $i
    # Materialize the predicate before a later admission error: jq try
    # inside the numeric reader must not relabel a real physical refusal.
    | [lh_exact_inputs($i.volume.metadata.name; $i.volume; $i.replicas; $i.nodes; $i.opct; $i.mpct)] as $valid
    | if $valid != [true]
      then error("unreported or inexact Longhorn inputs") else . end
    | ($i.opct | tonumber) as $opct | ($i.mpct | tonumber) as $mpct
    | ($i.replicas | group_by([.spec.nodeID,.spec.diskID])) as $groups
    | [ $groups[] | . as $g | $g[0].spec.nodeID as $node | $g[0].spec.diskID as $uuid
        | (lh_disks($i.nodes) | map(select(.node == $node and .uuid == $uuid))) as $found
        | if ($found | length) != 1 then error("replica disk identity is ambiguous") else $found[0] end
        | . as $d | ($i.growth * ($g | length)) as $required
        | if ($required | lh_safe | not) then error("growth arithmetic is inexact") else . end
        | (lh_expansion($required; $opct; $mpct)) as $why
        | if $why != [] then error($why | join("; ")) else . end
        | {node:$node, uuid:$uuid, name:.name, max:.max, reserved:.reserved,
           scheduled:.scheduled, count:($g | length), required_bytes:$required,
           provisioned_limit:(lh_limit($opct)), scheduled_after:(.scheduled+$required),
           minimal_available:(lh_floor($mpct))} ] | sort_by([.node,.uuid])'
