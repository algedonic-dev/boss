#!/usr/bin/env bash
# observe-volumes — every PersistentVolumeClaim in every BOSS instance
# namespace, capacity and free, read from OUTSIDE the cluster through the
# forge's admin kubeconfig (backlog 21ee3b4e, incident d3c0a67c).
#
# WHY IT EXISTS. On 2026-10-01 the system of record's Postgres volume
# (boss/pgdata-postgres-0, Longhorn volume
# pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56, 20 GiB) filled and every
# write failed with No space left on device. Nothing observed it: the
# estate's disk floor judged hosts (df /) and cluster nodes (the
# kubelet's /var), and a claim is a filesystem of its own that neither
# reading sees. A builder found it through a 500, and the remedy was an
# admin kubectl patch by hand to 30 GiB. David: "I want the system to be
# able to do this." This is the reading the alarm needs.
#
# WHICH NUMBER, AND WHY: THE KUBELET'S VOLUME STATS, not Longhorn's
# actualSize. Both are readable through the door this host already holds
# (the admin kubeconfig, root material, design 835c0c9c), so neither
# needs a new privilege; they differ in what they measure.
#   - The kubelet's /stats/summary reports, per pod volume with a
#     pvcRef, a statfs of the MOUNTED FILESYSTEM: capacityBytes,
#     usedBytes, availableBytes. availableBytes is what a non-root writer
#     (postgres runs as uid 999) can still use — the exact quantity that
#     reached zero in the incident. It works for every storage class,
#     not only Longhorn.
#   - Longhorn's volumes.longhorn.io status.actualSize is BLOCKS
#     ALLOCATED on the replica, snapshots included. A filesystem does not
#     hand freed blocks back without a trim, so it only ever grows toward
#     the size (and past it with snapshots): a database that vacuums
#     reads as full for ever. It would be a permanently-true alarm, or
#     with snapshots a free figure below zero.
# The read is `kubectl get --raw /api/v1/nodes/<node>/proxy/stats/summary`
# — a GET through the API server's node proxy, which the admin
# kubeconfig already holds. That grant was taken AWAY from the
# in-cluster observer (eeac3d56: nodes/proxy is exec into any pod, and
# no_service_account_reaches_the_kubelet_proxy.rs keeps it away); this
# reads it where it already lives, from outside, and grants nothing.
#
# READ-ONLY. Three verbs, all GETs: `get pvc`, `get nodes`, `get --raw`
# of the summary. It patches, refuses and changes nothing.
#
# WHICH CLAIMS. Every claim in every namespace infra/cluster/instances.toml
# declares (prod's `boss` and each other instance's) — the converge's own
# list of instances, read the way render-instance.sh reads it (one
# `namespace = "…"` per instance, no TOML parser). A claim is named
# `<namespace>/<claim>`, because every instance has a pgdata-postgres-0.
#
# A FAILED READ SAYS UNREAD, NEVER FINE. A claim whose filesystem no
# kubelet reported — not Bound, not mounted by a running pod, or mounted
# on a node whose summary could not be read — is RECORDED with
# `unread` naming why, its figures null. A namespace whose claims could
# not be listed is one row, `<namespace>/*`, saying so. estate.compare
# lists every such row as `disk_unmeasured` and never as evaluated, and
# estate.alarm raises `blind:disk_tight/<id>` once it persists — so a
# volume nobody can see is an alarm, not a quiet one.
#
# WHAT IT POSTS. One `instance-volumes` observation per run; the floor
# (free below 20% or 4 GiB) is applied by estate.compare, not here, so
# the judgement has one definition (compare_volumes). Bytes are carried
# as bytes: a 1 GiB claim cannot be judged in whole GiB.
#
# NOT RETAINED, as observe-nodefs.sh beside it is not: the alarm judges
# the newest readings, the series' silence is its own alarm
# (`unobserved:instance-volumes`), and a spool would put old rows at the
# head of the series.
#
# Env: JOBS_API (required to post; from /etc/boss/sor.env),
#      BOSS_OPS_DIR / BOSS_FORGE_REGISTRY_HOST (ops-credentials.sh),
#      BOSS_INSTANCES (default: infra/cluster/instances.toml beside this
#      tree), BOSS_OBSERVE_WORK (scratch; default a mktemp -d of its own).
#
# `--print` reads and prints the observation instead of posting it.
# An intermittently mounted claim may name a producer in
# mounted-volume-sources.json. Its latest final native statfs can be
# reused within that workflow's declared cadence, retaining the sample
# time, backing-volume identity and server step provenance. A newer
# failed/no-sample attempt invalidates it, and nothing judges its floor
# here. The input's source is carried on that row, not relabelled kubelet.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
. "$here/observe-lib.sh"
# shellcheck source=/dev/null
. "$here/ops-credentials.sh"

mode=post
case "${1:-}" in
    --print) mode=print ;;
    '') ;;
    *) echo "usage: observe-volumes.sh [--print]" >&2; exit 2 ;;
esac
[[ "$mode" == print ]] || : "${JOBS_API:?JOBS_API is required to post (it comes from /etc/boss/sor.env)}"

INSTANCES="${BOSS_INSTANCES:-$here/../cluster/instances.toml}"
if [[ -n "${BOSS_OBSERVE_WORK:-}" ]]; then
    WORK="$BOSS_OBSERVE_WORK"; mkdir -p "$WORK"
else
    WORK="$(mktemp -d)" || exit 1
    trap 'rm -rf "$WORK"' EXIT
fi

# The namespaces. No declaration is no observation: there would be
# nothing to name, and the series' silence raises unobserved.
namespaces=$(sed -n 's/^namespace *= *"\([a-z0-9-]*\)" *$/\1/p' "$INSTANCES" 2>/dev/null | sort -u)
if [[ -z "$namespaces" ]]; then
    echo "observe-volumes: REFUSED — $INSTANCES declares no instance namespace (or cannot be read), so there is no claim to name" >&2
    exit 1
fi

# Every read is bounded: this runs inside the forge's host observer
# unit, whose TimeoutStartSec is the outer bound, and one stalled call
# must not spend it (review of 4327d1a3, finding 3).
kc() { $(ops_kubectl) "$@" --request-timeout=20s; }
oneline() { tr '\n' ' ' <"$1" | cut -c1-300; }

# The claims, per namespace. A namespace that cannot be listed is a row
# of its own, so its absence reads as unread rather than as no claims.
: >"$WORK/claims.jsonl"
for ns in $namespaces; do
    if kc get pvc -n "$ns" -o json >"$WORK/pvc.json" 2>"$WORK/pvc.err" \
        && [[ "$(jq -r '.items | type' "$WORK/pvc.json" 2>/dev/null)" == array ]]; then
        jq -c --arg ns "$ns" '.items[] | {
            ns: $ns,
            claim: .metadata.name,
            volume: (.spec.volumeName // null),
            storage_class: (.spec.storageClassName // null),
            requested: (.spec.resources.requests.storage // null),
            phase: (.status.phase // null) }' "$WORK/pvc.json" >>"$WORK/claims.jsonl"
    else
        why="the claims of $ns could not be listed through $(ops_kubeconfig): $(oneline "$WORK/pvc.err")"
        echo "no volume reading for $ns/*: $why"
        jq -cn --arg ns "$ns" --arg why "$why" '{ns: $ns, claim: "*", unlisted: $why}' >>"$WORK/claims.jsonl"
    fi
done

# The kubelet volume stats, node by node. One node's failed read costs
# the claims mounted there and nothing else, and is named on each.
: >"$WORK/stats.jsonl"
: >"$WORK/node-errors.txt"
if kc get nodes -o json >"$WORK/nodes.json" 2>"$WORK/nodes.err" \
    && [[ "$(jq -r '.items | type' "$WORK/nodes.json" 2>/dev/null)" == array ]]; then
    for node in $(jq -r '.items[].metadata.name' "$WORK/nodes.json"); do
        # A node name rides a URL path: one DNS-1123 name, or it is not asked.
        if ! [[ "$node" =~ ^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$ ]]; then
            echo "$(printf '%s' "$node" | cut -c1-80): not a node name the summary path can carry" >>"$WORK/node-errors.txt"
            continue
        fi
        if kc get --raw "/api/v1/nodes/$node/proxy/stats/summary" >"$WORK/summary.json" 2>"$WORK/summary.err" \
            && [[ "$(jq -r '.pods | type' "$WORK/summary.json" 2>/dev/null)" == array ]]; then
            jq -c --arg node "$node" '.pods[] | .podRef as $p | (.volume // [])[]
                | select(.pvcRef != null)
                | { ns: .pvcRef.namespace, claim: .pvcRef.name, node: $node, pod: $p.name,
                    capacity: .capacityBytes, used: .usedBytes, available: .availableBytes }' \
                "$WORK/summary.json" >>"$WORK/stats.jsonl"
        else
            echo "$node: $(oneline "$WORK/summary.err")" >>"$WORK/node-errors.txt"
        fi
    done
else
    echo "the node list: $(oneline "$WORK/nodes.err")" >>"$WORK/node-errors.txt"
fi

# The join, and every claim's verdict on whether it was read.
jq -n -c --slurpfile claims "$WORK/claims.jsonl" --slurpfile stats "$WORK/stats.jsonl" \
    --rawfile errs "$WORK/node-errors.txt" '
    ($errs | split("\n") | map(select(length > 0))) as $errs
    | (if ($errs | length) > 0
       then "; the kubelet stats of " + ($errs | join("; ")) + " could not be read"
       else "" end) as $blind
    | $claims[] as $c
    | ($c.ns + "/" + $c.claim) as $id
    | if $c.unlisted then
        { id: $id, namespace: $c.ns, claim: null, volume: null,
          capacity_bytes: null, used_bytes: null, free_bytes: null, unread: $c.unlisted }
      else
        ([ $stats[] | select(.ns == $c.ns and .claim == $c.claim
                             and (.capacity | type) == "number" and (.available | type) == "number") ]
         | first) as $s
        | { id: $id, namespace: $c.ns, claim: $c.claim, volume: $c.volume,
            storage_class: $c.storage_class, requested: $c.requested, phase: $c.phase }
          + if $c.phase != "Bound" then
              { capacity_bytes: null, used_bytes: null, free_bytes: null,
                unread: ("the claim is " + ($c.phase // "of no phase") + ", not Bound, so no filesystem stands behind it") }
            elif $s == null then
              { capacity_bytes: null, used_bytes: null, free_bytes: null,
                unread: ("no kubelet reports this claim'"'"'s filesystem — no running pod mounts it, or it is mounted on a node whose stats were not read" + $blind) }
            else
              { node: $s.node, pod: $s.pod, capacity_bytes: $s.capacity,
                used_bytes: $s.used, free_bytes: $s.available }
            end
      end' >"$WORK/rows.jsonl" || { echo "observe-volumes: REFUSED — the reading did not join (jq failed)" >&2; exit 1; }

# Only missing live figures consult durable evidence. Every currently
# measured kubelet control stays intact, and non-Bound claims stay blind.
sources=${BOSS_VOLUME_SAMPLE_SOURCES:-$here/mounted-volume-sources.json}
: > "$WORK/with-samples.jsonl"
while IFS= read -r row; do
    ns=$(printf '%s' "$row" | jq -r '.namespace')
    claim=$(printf '%s' "$row" | jq -r '.claim // empty')
    needs=$(printf '%s' "$row" | jq -r '.phase == "Bound" and .free_bytes == null')
    if [[ "$needs" == true ]]; then
        if ! configured=$(jq -ser --arg ns "$ns" --arg claim "$claim" '
            if length == 1 then .[0] else error("not one entire source registry") end
            | if type != "array" then error("not a source registry") else . end
            | any(.[]; .namespace == $ns and .claim == $claim)' "$sources" 2>/dev/null); then
            # jq -e also says false with exit 1: that is an ordinary
            # claim with no declared intermittent producer, not an error.
            [[ "$configured" == false ]] || echo "observe-volumes: mounted source registry $sources could not be read" >&2
        fi
        if [[ "$configured" == true ]]; then
            volume=$(printf '%s' "$row" | jq -r '.volume // empty')
            if ! sample=$(bash "$here/read-volume-sample.sh" "$ns" "$claim" "$volume"); then
                sample='{"capacity_bytes":null,"used_bytes":null,"free_bytes":null,"unread":"mounted sample reader failed"}'
            fi
            row=$(printf '%s' "$row" | jq -c --argjson sample "$sample" '
                if $sample.free_bytes == null then .unread += ("; " + $sample.unread)
                else del(.unread) + $sample end') || { echo 'observe-volumes: REFUSED — mounted evidence did not join' >&2; exit 1; }
        fi
    fi
    printf '%s\n' "$row" >> "$WORK/with-samples.jsonl"
done < "$WORK/rows.jsonl"
mv "$WORK/with-samples.jsonl" "$WORK/rows.jsonl"

jq -r 'if .free_bytes == null then "no volume reading for \(.id): \(.unread)"
       else "  \(.id) (\(.volume // "no volume")): \((.capacity_bytes / 1073741824 * 10 | round) / 10)G, free \((.free_bytes / 1073741824 * 10 | round) / 10)G"
       end' "$WORK/rows.jsonl"

observation=$(jq -cs --arg ns "$namespaces" '{
    observed_at: (now | todate),
    observer: "boss-estate-observe-volumes",
    scope: "instance-volumes",
    source: "kubelet /stats/summary through the admin kubeconfig",
    namespaces: ($ns | split("\n") | map(select(length > 0))),
    nodes: .
}' "$WORK/rows.jsonl")

if [[ "$mode" == print ]]; then
    printf '%s\n' "$observation"
    exit 0
fi
post_observation "$observation"
