#!/usr/bin/env bash
# read-service-address.sh — READ-ONLY: the address the cluster assigned
# one LoadBalancer Service, with its ports, the pins its manifest may
# carry and the allocator the object itself names. The ops verb
# `read-service-address`.
#
#   read-service-address.sh <namespace> <service>
#
# WHY IT EXISTS (backlog 7f05ef2f, 2026-11-21). Item 47601829 — the
# standby dev pod's ssh door — must pin the address the pool gave
# Service boss-dev/boss-dev-standby-ssh, and no door could read it: the
# dev session's credential cannot `get services` in boss-dev, no verb
# read a Service (cluster-deploy-lib.sh's site_reach reads only
# boss-gateway, inside the converge), and the estate API holds no
# load-balancer address. The manifest told its reader to run a kubectl
# no session holding that manifest could run.
#
# WHICH ALLOCATOR SERVES IS A LIVE FACT. boss.yaml pins with Cilium
# LB-IPAM's annotation; boss-dev.yaml and boss-jobs-internal.yaml with
# MetalLB's plus spec.loadBalancerIP, which both honour; the tree
# declares no pool. So the verb prints every pin spelling beside the
# allocator the OBJECT names — MetalLB's ip-allocated-from-pool
# annotation, a cilium.io/ status condition — and its first run answers
# the question with an artifact rather than a comment.
#
# WHAT IT PRINTS (stdout, no clock), after a line naming the manifest
# that declares the Service:
#
#   service <ns>/<name> type <type> class <loadBalancerClass|none>
#   ingress <ip|hostname>                    one per assigned address
#   port <name|-> <port>/<protocol>          one per port
#   pin io.cilium/lb-ipam-ips=<value|none>
#   pin metallb.universe.tf/loadBalancerIPs=<value|none>
#   pin spec.loadBalancerIP=<value|none>
#   allocator metallb pool <pool> (<annotation>)          and/or
#   allocator cilium-lb-ipam <condition>=<status> [<reason>]
#                                            or `allocator none named — …`
#
# and a closing line naming the address. Only those named fields are
# read off the object, never its annotations wholesale, so nothing else
# a Service carries reaches the packet.
#
# THE BOUND IS THE TREE: a Service some manifest under
# infra/cluster/manifests declares, in that namespace, read off the ONE
# derivation of what the tree declares (infra/cluster/undeclared-objects.sh
# --declared) — never a typed list and never a second YAML reader.
# Anything else is a REFUSAL (exit 78) before any cluster read. A declared
# set that could not be derived is CANNOT ANSWER (exit 1), not a refusal:
# it says nothing about the Service.
#
# A WRONG TARGET ANSWERS INSTEAD OF ERRORING (CLAUDE.md §Doors). A Service
# the cluster does not hold, a read that failed or ran past its bound, an
# answer about another object, or a Service with no assigned address
# prints CANNOT ANSWER and exits 1 — never an empty success.
#
# The one cluster read is `kubectl get svc` through the forge's
# ops_kubectl door (infra/estate/ops-credentials.sh), bounded twice:
# kubectl's own --request-timeout and `timeout` around the door, its
# answer redirected into files and never read through a `$(…)` pipe
# (backlog c44ba179). Nothing is written.
#
# ENV (test seams — the ops runner passes a verb no environment from the
# packet): BOSS_CLUSTER_TREE and BOSS_KUBECTL, read by
# undeclared-objects.sh; BOSS_READ_SERVICE_TIMEOUT_S, the door's bound in
# seconds (default 60); and the ops-credentials.sh knobs (BOSS_OPS_DIR,
# BOSS_FORGE_REGISTRY_HOST).
set -uo pipefail

ME=read-service-address
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DERIVE="$HERE/../cluster/undeclared-objects.sh"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so the identity guard below asks that first — an
# empty answer from a door that exited 0 is no Service at all.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
cannot() { echo "CANNOT ANSWER — $*"; exit 1; }

# A namespace and a Service name: each one DNS label, no leading dash.
DNS_LABEL='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
[ $# -eq 2 ] || refuse "usage: read-service-address.sh <namespace> <service> — one Service, named in full"
NS="$1"
SVC="$2"
for word in "$NS" "$SVC"; do
    [[ "$word" =~ $DNS_LABEL ]] || refuse "a namespace and a Service name are each one lowercase DNS label, got '$word'"
done
command -v jq >/dev/null 2>&1 || cannot "jq is not on PATH — the Service cannot be read"
T="${BOSS_READ_SERVICE_TIMEOUT_S:-60}"
case "${T:-empty}" in
    empty | *[!0-9]*) cannot "the read's bound '$T' is not whole seconds" ;;
esac

WORK="$(mktemp -d)" || cannot "no scratch directory to read into"
trap 'rm -rf "$WORK"' EXIT

# --- the bound: a Service the tree declares under infra/cluster/manifests
if ! "$DERIVE" --declared > "$WORK/declared" 2> "$WORK/declared.err"; then
    sed 's/^/    /' "$WORK/declared.err" >&2
    cannot "undeclared-objects.sh could not derive what the tree declares (above) — an underived set bounds nothing, so nothing was read"
fi
FILE="$(LC_ALL=C awk -F'\t' -v ns="$NS" -v n="$SVC" '
    $1 == "Service" && $2 == ns && $3 == n && index($4, "infra/cluster/manifests/") == 1 { print $4; exit }
' "$WORK/declared")"
[ -n "$FILE" ] || refuse "no manifest under infra/cluster/manifests declares Service $NS/$SVC — this verb reads only a Service the tree declares"

# --- the door ---------------------------------------------------------------
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    cannot "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

echo "$ME: $NS/$SVC — READ-ONLY; nothing is written; declared in $FILE"
rc=0
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
timeout -k 5 "$T" $K get svc "$SVC" -n "$NS" -o json --request-timeout=20s \
    > "$WORK/svc.json" 2> "$WORK/svc.err" || rc=$?
case "$rc" in
    0) ;;
    124 | 137) cannot "kubectl get svc $SVC -n $NS timed out after ${T}s, so nothing about its address was read" ;;
    *)
        if grep -q 'NotFound' "$WORK/svc.err"; then
            cannot "the cluster holds no Service $NS/$SVC, though $FILE declares it: $(tr '\n' ' ' < "$WORK/svc.err")"
        fi
        cannot "kubectl get svc $SVC -n $NS failed (exit $rc): $(tr '\n' ' ' < "$WORK/svc.err")" ;;
esac

# An answer about another object is not an answer about this one.
if ! { jq_doc_file "$WORK/svc.json" \
        && jq -e -s --arg ns "$NS" --arg n "$SVC" \
            'length == 1 and (.[0] | .kind == "Service" and .metadata.namespace == $ns and .metadata.name == $n)' \
            "$WORK/svc.json" > /dev/null 2> "$WORK/svc.jq"; }; then
    cannot "the read did not answer Service $NS/$SVC: $(jq -r '"\(.kind // "no kind") \(.metadata.namespace // "?")/\(.metadata.name // "?")"' "$WORK/svc.json" 2>/dev/null || echo 'not a JSON object')"
fi

if ! jq -r '
        (.metadata.annotations // {}) as $a
        | "service \(.metadata.namespace)/\(.metadata.name) type \(.spec.type // "none") class \(.spec.loadBalancerClass // "none")",
          ((.status.loadBalancer.ingress // [])[] | (.ip // .hostname // empty) | "ingress \(.)"),
          ((.spec.ports // [])[] | "port \(.name // "-") \(.port)/\(.protocol // "TCP")"),
          "pin io.cilium/lb-ipam-ips=\($a["io.cilium/lb-ipam-ips"] // "none")",
          "pin metallb.universe.tf/loadBalancerIPs=\($a["metallb.universe.tf/loadBalancerIPs"] // "none")",
          "pin spec.loadBalancerIP=\(.spec.loadBalancerIP // "none")",
          ([ ($a | to_entries[]
                | select(.key | test("^metallb\\.(io|universe\\.tf)/ip-allocated-from-pool$"))
                | "allocator metallb pool \(.value) (\(.key))"),
             ((.status.conditions // [])[]
                | select((.type // "") | startswith("cilium.io/"))
                | "allocator cilium-lb-ipam " + ([.type + "=" + (.status // "?"), .reason]
                    | map(select(. != null and . != "")) | join(" "))) ]
           | if length == 0
             then "allocator none named — the Service carries neither the MetalLB ip-allocated-from-pool annotation nor a cilium.io/ condition"
             else .[] end)
        ' "$WORK/svc.json" > "$WORK/lines" 2> "$WORK/lines.err"; then
    cannot "Service $NS/$SVC would not read: $(tr '\n' ' ' < "$WORK/lines.err")"
fi
cat "$WORK/lines"

addresses="$(jq -r '[(.status.loadBalancer.ingress // [])[] | (.ip // .hostname // empty)] | join(",")' "$WORK/svc.json")"
[ -n "$addresses" ] \
    || cannot "Service $NS/$SVC has no assigned address — its type is $(jq -r '.spec.type // "none"' "$WORK/svc.json"), and no allocator has given it one (the allocator line above says which one, if any, has claimed it)"
echo "$ME: $NS/$SVC — address $addresses"
