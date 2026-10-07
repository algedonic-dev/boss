# cluster-node-lib.sh — the ONE reading of which cluster node a node
# verb acts on, and whether it may (backlog f0aaa72f; design 8457c07b,
# the w-1 second-NVMe window). Sourced by cordon-node.sh, shutdown-node.sh,
# talos-get.sh, node-status.sh and node-converge.sh, so the verbs cannot disagree about
# what a worker is, which pods a drain evicts, or which budgets hold it
# (CLAUDE.md §9a).
#
# Source it after setting ME (the verb's voice) and WORK (a scratch
# directory the caller owns and removes). Bash, `set -uo pipefail`.
#
# THE BOUND IS TWO READINGS THAT MUST AGREE, never a parameter:
#
#   1. The estate registry (GET /api/estate/nodes on the system of
#      record — the live registry, not the tree's estate.toml, because
#      the registry is what the estate page, the compare and the alarms
#      read): the node must be one row, not retired, with an address
#      that is one IPv4 literal. node_require_worker then requires its
#      primary `role` to be exactly `talos-worker`.
#   2. The cluster's own Node object, read through ops_kubectl (the
#      admin kubeconfig's one door, infra/estate/ops-credentials.sh):
#      it must carry no control-plane label, and its InternalIP must be
#      the registry's address.
#
# WHY BOTH. A control plane carries etcd and the system of record's pod
# (boss.yaml pins it there); cordoning or shutting one down is a design
# decision, never a packet argument. The registry is data an operator
# edits, and the cluster is what actually runs — a registry row that
# drifted, or was edited, must not be able to aim a mutating verb at a
# node the cluster calls a control plane. Any disagreement refuses,
# naming both answers.
#
# FAILS CLOSED. A registry that does not answer, answers a status other
# than 2xx, or answers something that is not the node list is a FAILURE
# (exit 1, "cannot answer"), never an empty registry and never a cache:
# unlike node-roles.sh's converge, these verbs have no safe fallback.
# A node the registry does not hold, a retired one, or one whose role or
# labels say otherwise is a REFUSAL (exit 78): the request is wrong.
#
# ENV (test seams — the ops runner passes a verb no environment from the
# packet): BOSS_ESTATE_NODES_URL (default $BOSS_JOBS_URL/api/estate/nodes,
# the address from /etc/boss/sor.env through infra/lib/sor.sh), plus the
# ops-credentials.sh knobs (BOSS_OPS_DIR, BOSS_TALOSCTL,
# BOSS_FORGE_REGISTRY_HOST).

# shellcheck source=infra/lib/sor.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/estate/ops-credentials.sh"
# The machine token's one shell reader — sourced when it is there, and
# only defines functions; node_read_registry says what it is for.
# shellcheck source=infra/lib/secret-header.sh
if [ -r "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" ]; then . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh"; fi

# A Kubernetes node name, which is also its estate id here: a DNS label.
# No leading dash, no dot, no upper case, one word.
NODE_ID_RE='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'

node_refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
node_fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

# node_check_id <node> — refuse anything that is not one DNS label.
node_check_id() {
    [[ "${1:-}" =~ $NODE_ID_RE ]] || node_refuse "the node must be one lowercase DNS label (a Kubernetes node name, e.g. w-1), got '${1:-}'"
}

# node_read_registry <node> — sets NODE_ADDRESS and NODE_ROLE from the
# estate registry's one row for <node>.
node_read_registry() {
    local id="$1" url out code body rows n retired
    url="${BOSS_ESTATE_NODES_URL:-}"
    if [ -z "$url" ]; then
        sor_require BOSS_JOBS_URL
        url="$BOSS_JOBS_URL/api/estate/nodes"
    fi
    # The machine token is presented, never required (design 6805c764;
    # backlog 2710c8fc): made here, in the verb's own shell — every verb
    # sets its `trap … EXIT` before this call — and before the `$(…)`.
    # No lib, no slot, a refused slot, a host off the estate's list or a
    # header file that cannot be made all send the read as before.
    local NODE_MT_HDR=""
    if declare -F machine_token_header >/dev/null; then
        case "$url" in
            http://* | https://*) machine_token_header NODE_MT_HDR "$url" || NODE_MT_HDR="" ;;
        esac
    fi
    # The status is KEPT (node-roles.sh's lesson): a 403 read as "no such
    # node" would be a confident wrong answer. `000` is a file:// fixture.
    if ! out="$(curl -sS --max-time 15 \
            ${NODE_MT_HDR:+-H "$NODE_MT_HDR"} \
            -H "x-boss-user: $(sor_reader_header "automation:$ME")" \
            -w '\n%{http_code}' "$url" 2> "$WORK/registry.err")"; then
        node_fail "the estate registry at $url did not answer ($(tr '\n' ' ' < "$WORK/registry.err")) — a registry that cannot be read says nothing about which nodes are workers, so nothing was done"
    fi
    code="${out##*$'\n'}"
    body="${out%$'\n'*}"
    case "$code" in
        2?? | 000) ;;
        *) node_fail "the estate registry at $url answered HTTP $code (signed automation:$ME/audit-readonly), so nothing was done" ;;
    esac
    if ! rows="$(printf '%s' "$body" | jq -c --arg id "$id" '[.data[] | select(.id == $id)]' 2> "$WORK/registry.jq")"; then
        node_fail "the estate registry at $url did not answer a node list ($(tr '\n' ' ' < "$WORK/registry.jq")), so nothing was done"
    fi
    n="$(jq 'length' <<< "$rows")"
    [ "$n" = 1 ] || node_refuse "the estate registry holds $n node(s) with id '$id' — this verb acts only on a node the registry declares exactly once"
    retired="$(jq -r '.[0].retired' <<< "$rows")"
    [ "$retired" = false ] || node_refuse "the estate registry marks $id retired (retired=$retired) — a retired machine is history, not a target"
    NODE_ROLE="$(jq -r '.[0].role // ""' <<< "$rows")"
    NODE_ADDRESS="$(jq -r '.[0].address // ""' <<< "$rows")"
    # ONE address literal, or no door is opened (observe-nodefs.sh's rule,
    # review of 4327d1a3): `talosctl -n` takes a comma list and a hostname
    # would have apid proxy to whatever it resolves to.
    [[ "$NODE_ADDRESS" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] \
        || node_refuse "the estate registry gives $id the address '$NODE_ADDRESS', which is not one IPv4 literal"
}

# node_require_worker <node> — refuse unless the registry's primary role
# is exactly talos-worker (after node_read_registry).
node_require_worker() {
    [ "$NODE_ROLE" = talos-worker ] \
        || node_refuse "the estate registry says $1 is a ${NODE_ROLE:-node with no role}, not a talos-worker — this verb acts on worker nodes only; a control plane carries etcd and the system of record"
}

# node_require_talos <node> — refuse unless the registry's primary role
# is one of the two Talos roles (after node_read_registry). The one verb
# that may act on a control plane uses this in place of
# node_require_worker: node-converge (backlog 9d56c616), because design
# 1bc4b4ed decided the per-node declaration is converged on EVERY node
# that has one, and bounded the act instead — no reboot, ever, and a
# passkey on the rendered diff. A node with no Talos role has no Talos
# API and no declaration, so it is refused before any door opens.
node_require_talos() {
    case "$NODE_ROLE" in
        talos-worker | talos-control-plane) ;;
        *) node_refuse "the estate registry says $1 is a ${NODE_ROLE:-node with no role}, not a Talos node (talos-worker or talos-control-plane) — there is no machine config to converge" ;;
    esac
}

# node_agree_cluster <node> — read the Node through the door into
# $WORK/node.json and refuse unless the cluster agrees with the registry
# on BOTH the address and the kind of node (after node_read_registry,
# node_require_talos and node_door): the registry's address as the
# Node's InternalIP, and a control-plane label exactly when the registry
# says talos-control-plane. node_read_cluster's rule, for a verb that may
# act on either kind: a registry row edited to call a control plane a
# worker, or the reverse, acts on nothing.
node_agree_cluster() {
    local id="$1" ip cp
    node_get "$id"
    if ! cp="$(jq -r '(.metadata.labels // {}) | keys[]
            | select(. == "node-role.kubernetes.io/control-plane" or . == "node-role.kubernetes.io/master")' \
            "$WORK/node.json" 2> "$WORK/node.jq")"; then
        node_fail "node $id did not read back as a Node object: $(tr '\n' ' ' < "$WORK/node.jq")"
    fi
    case "$NODE_ROLE" in
        talos-control-plane)
            [ -n "$cp" ] || node_refuse "the estate registry calls $id a talos-control-plane and the cluster's Node carries no control-plane label — a disagreement acts on nothing" ;;
        *)
            [ -z "$cp" ] || node_refuse "the cluster labels $id $(printf '%s' "$cp" | tr '\n' ' ')— the registry calls it a $NODE_ROLE, the cluster calls it a control plane, and a disagreement acts on nothing" ;;
    esac
    ip="$(jq -r '[(.status.addresses // [])[] | select(.type == "InternalIP") | .address] | join(",")' "$WORK/node.json")"
    [ "$ip" = "$NODE_ADDRESS" ] \
        || node_refuse "the estate registry gives $id the address $NODE_ADDRESS and the cluster's Node reports InternalIP ${ip:-none} — a disagreement acts on nothing"
}

# node_door — make the kubectl image present (bounded, named when it
# fails) and set K to ops_kubectl's line.
node_door() {
    local ready
    if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
        node_fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
    fi
    echo "$ME: kubectl image $ready" >&2
    K="$(ops_kubectl)"
}

# node_read_cluster <node> — read the Node through the door into
# $WORK/node.json; refuse a control-plane label or an InternalIP other
# than the registry's (after node_read_registry and node_door).
node_read_cluster() {
    local id="$1" ip cp
    node_get "$id"
    if ! cp="$(jq -r '(.metadata.labels // {}) | keys[]
            | select(. == "node-role.kubernetes.io/control-plane" or . == "node-role.kubernetes.io/master")' \
            "$WORK/node.json" 2> "$WORK/node.jq")"; then
        node_fail "node $id did not read back as a Node object: $(tr '\n' ' ' < "$WORK/node.jq")"
    fi
    [ -z "$cp" ] || node_refuse "the cluster labels $id $(printf '%s' "$cp" | tr '\n' ' ')— the registry calls it a $NODE_ROLE, the cluster calls it a control plane, and a disagreement acts on nothing"
    ip="$(jq -r '[(.status.addresses // [])[] | select(.type == "InternalIP") | .address] | join(",")' "$WORK/node.json")"
    [ "$ip" = "$NODE_ADDRESS" ] \
        || node_refuse "the estate registry gives $id the address $NODE_ADDRESS and the cluster's Node reports InternalIP ${ip:-none} — a disagreement acts on nothing"
}

# node_get <node> — read the Node through the door into $WORK/node.json,
# judging nothing (after node_door). node_read_cluster judges it; the
# read-only node-status reads any node the registry declares.
node_get() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get node "$1" -o json --request-timeout=20s > "$WORK/node.json" 2> "$WORK/node.err"; then
        node_fail "could not read node $1 through $(ops_kubeconfig): $(tr '\n' ' ' < "$WORK/node.err")"
    fi
}

# node_unschedulable — the cordon as the Node read states it: true or false.
node_unschedulable() {
    jq -r 'if .spec.unschedulable == true then "true" else "false" end' "$WORK/node.json"
}

# node_pods <node> — the pods on <node> whose phase is not Succeeded or
# Failed, into $WORK/pods.sel as [{ref, ns, labels, owner, stays}] sorted
# by ref. `stays` is a pod a drain leaves until power off: a DaemonSet's,
# or a static mirror pod. Returns 1 with NODE_ERR set when it could not
# look — the caller decides what an unread node means for it.
node_pods() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get pods -A --field-selector "spec.nodeName=$1" -o json --request-timeout=30s \
            > "$WORK/pods.json" 2> "$WORK/pods.err"; then
        NODE_ERR="kubectl get pods on $1: $(tr '\n' ' ' < "$WORK/pods.err")"
        return 1
    fi
    if ! jq -c '[.items[]
            | select((.status.phase // "") != "Succeeded" and (.status.phase // "") != "Failed")
            | ([.metadata.ownerReferences[]? | select(.controller != false)] | .[0] // null) as $o
            | { ref: "\(.metadata.namespace)/\(.metadata.name)",
                ns: .metadata.namespace,
                labels: (.metadata.labels // {}),
                owner: (if (.metadata.annotations // {})["kubernetes.io/config.mirror"] != null then "static mirror pod"
                        elif $o == null then "no owner"
                        else "\($o.kind) \($o.name)" end),
                stays: ((.metadata.annotations // {})["kubernetes.io/config.mirror"] != null or ($o.kind // "") == "DaemonSet") }]
            | sort_by(.ref)' "$WORK/pods.json" > "$WORK/pods.sel" 2> "$WORK/pods.jq"; then
        NODE_ERR="the pod list for $1 would not parse: $(tr '\n' ' ' < "$WORK/pods.jq")"
        return 1
    fi
}

# node_held_by_budgets — every PodDisruptionBudget whose status allows 0
# disruptions and whose selector selects a pod the drain would evict (a
# pod of $WORK/pods.sel that does not stay), one sorted line each into
# $WORK/budgets.held (after node_pods). Returns 1 with NODE_ERR set when
# it could not look.
#
# WHY (backlog 9fa9f835; review bc33ff50 M1): an eviction a budget at
# zero refuses is retried, not failed, so Talos's drain WAITS on it, and
# shutdown-node then reads "NOT proven down" after 720 s with the node's
# shutdown sequence still in flight. Longhorn's instance-manager budget
# is the expected one: under node-drain-policy
# block-if-contains-last-replica it stays while the node holds a
# volume's last healthy replica. So the budgets are read before the act,
# never met during it.
#
# The selector is matched the way the API server matches it (policy/v1):
# same namespace; matchLabels all equal; matchExpressions In / NotIn /
# Exists / DoesNotExist; an empty selector selects every pod in the
# namespace and a null one selects none. A missing status counts as 0 —
# the eviction API refuses a budget its controller has not yet observed.
node_held_by_budgets() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get pdb -A -o json --request-timeout=30s > "$WORK/pdbs.json" 2> "$WORK/pdbs.err"; then
        NODE_ERR="kubectl get pdb (poddisruptionbudgets): $(tr '\n' ' ' < "$WORK/pdbs.err")"
        return 1
    fi
    if ! jq -r --slurpfile pods "$WORK/pods.sel" '
            def selects($l):
                if . == null then false
                else ((.matchLabels // {}) | to_entries | all(.key as $k | .value as $v | $l[$k] == $v))
                    and ((.matchExpressions // []) | all(
                        .key as $k | (.values // []) as $vs
                        | if .operator == "In" then ($l | has($k)) and any($vs[]; . == $l[$k])
                          elif .operator == "NotIn" then (($l | has($k)) | not) or all($vs[]; . != $l[$k])
                          elif .operator == "Exists" then $l | has($k)
                          elif .operator == "DoesNotExist" then ($l | has($k)) | not
                          else false end))
                end;
            [.items[]
             | select((.status.disruptionsAllowed // 0) == 0)
             | . as $b
             | [$pods[0][] | select((.stays | not) and .ns == $b.metadata.namespace)
                | select(.labels as $l | $b.spec.selector | selects($l)) | .ref] as $hit
             | select($hit | length > 0)
             | "pdb \($b.metadata.namespace)/\($b.metadata.name) (disruptionsAllowed \($b.status.disruptionsAllowed // 0)) selects \($hit | join(", "))"]
            | sort[]' "$WORK/pdbs.json" > "$WORK/budgets.held" 2> "$WORK/pdbs.jq"; then
        NODE_ERR="the disruption budgets would not parse: $(tr '\n' ' ' < "$WORK/pdbs.jq")"
        return 1
    fi
}
