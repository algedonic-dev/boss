#!/usr/bin/env bash
# retire-access-object — retire ONE ServiceAccount, Role or RoleBinding the
# tree no longer declares, through a plan David signs with his passkey.
#
#   retire-access-object.sh --plan <Kind> <namespace> <name>
#   retire-access-object.sh <Kind> <namespace> <name> <plan-sha256>
#
# WHY IT EXISTS (backlog 526bedf2). delete-orphan-object withholds these
# three kinds from its kind floor on purpose: "ServiceAccount — deleting
# one invalidates the tokens issued from it. Role, RoleBinding — deleting
# a privilege can remove a live capability nobody wrote down." So an
# undeclared one stayed a named human step, typed with the admin
# kubeconfig — and David, 2026-10-18: manually running any ops command is
# a gap in BOSS's coverage. retire-volume-claim (backlog f36eb7ce) closed
# the same gap for claims; this is its sibling for privileges, built the
# same way. The human act is the approval, not the command: the read-only
# `plan-an-access-object-retirement` renders this script's --plan onto the
# request's approve step, David's passkey signs those bytes, and the runner
# hands this script sha256 of the SIGNED plan (design 17835005).
#
# NOTHING NEW IS DELETABLE WITHOUT A PASSKEY. delete-orphan-object's floor
# is unchanged — this verb does not widen it, it is a second door with a
# passkey on it — and boss-testing's retire_access_object_sh.rs holds both
# floors, this one to the allowlist's `one_of`, and the two disjoint.
#
# THE AUTHORITY IS DERIVED, NOT GRANTED. An object is retirable only when
# every one of these holds, each refusing by name (exit 78) and checked
# again at the write:
#   * the kind is one of RETIRABLE_KINDS, and the namespace and name are
#     DNS names (re-checked here, not only in the allowlist);
#   * the name is not PROTECTED — the namespace's `default` ServiceAccount,
#     which the control plane recreates and every Pod naming no account
#     runs as, and anything named break-glass*, the recovery road
#     (infra/recovery/re-entry.toml) that must survive this tree being
#     wrong — before anything is read;
#   * the declaration set is COMMITTED, read as the checkout's owner;
#   * infra/cluster/undeclared-objects.sh --check names it undeclared —
#     the one definition, which refuses a declared object, a namespace the
#     tree does not own, an exempt object, a controller-owned one and one
#     that is not live;
#   * it is not already being deleted and carries no owner;
#   * NOTHING CONSUMES IT, per kind, every read failing closed (a list
#     that could not be read is exit 1, never an empty list):
#       ServiceAccount — no Pod in its namespace, in ANY phase, runs as it,
#         and no Job, CronJob, Deployment, StatefulSet, DaemonSet or
#         ReplicaSet would run one as it: those are the holders of the
#         tokens issued from it, now and next. No Secret in its namespace
#         is a long-lived token issued from it (the token controller would
#         delete that credential with it). No RoleBinding in ANY namespace
#         and no ClusterRoleBinding grants it anything: a grant left naming
#         a deleted account re-arms for whatever next takes the name.
#       Role — no RoleBinding in its namespace grants it, for the same
#         reason.
#       RoleBinding — every subject is a ServiceAccount, no Pod (any phase)
#         runs as one, no workload would, and no Secret in the subject's
#         namespace is a long-lived token issued from one: those are who
#         lose the grant, the last of them outside any Pod (review
#         64b37b0c B1). A User or Group subject is refused, because no read
#         of the cluster names who holds one, so no plan could name every
#         consumer.
#
# THE PLAN IS THE SIGNED BYTES: the object (uid, labels, annotation keys,
# finalizers, creation), what it carries (a Role's rules, a binding's
# roleRef and subjects, an account's secrets), the consumer verdict, the
# holder no read can see (a TokenRequest token carried off the cluster,
# named on its own `cannot read:` line) and the one DELETE it authorises. No clock, resourceVersion or tree sha: two
# renders of one state are byte-identical, and a fact that moves voids the
# approval (exit 78, both hashes named). The kind is in the plan, so a
# plan signed for one kind never hashes for another; a second run of an
# applied plan finds the object gone and refuses, so one approval deletes
# at most once.
#
# CAPTURE BEFORE DELETE, ON THE RECORD: the object's YAML is attached to
# this ops-request through `boss attach`, which reads the bytes back; only
# a verified attachment lets the delete run. Then ONE DELETE carrying
# DeleteOptions.preconditions.uid, so a same-named object recreated in the
# race is refused by the API server. THE READ-BACK FAILS CLOSED: gone is
# only the server's NotFound or a document naming a DIFFERENT uid; a get
# that answers anything else — Forbidden, a timeout, no document, a
# document with no uid — is "deleted, not proven gone", exit 1. Only the
# complete read-back prints the effect line
# (infra/ops/verbs/retire-access-object.json `effect`).
#
# ENV (test seams; the ops runner passes no environment from the packet):
# BOSS_CLUSTER_TREE and BOSS_KUBECTL, read by undeclared-objects.sh;
# BOSS_BIN, the boss CLI (default `boss` on PATH, else
# /usr/local/bin/boss); BOSS_RETIRE_ACCESS_WAIT_S, how long the read-back
# waits (default 60). OPS_REQUEST_ID is set by the runner for the execute
# step; the write refuses without it, because the capture has nowhere to
# land.
#
# Exit 78 is a refusal (the request was wrong or no longer true); exit 1
# is a step that genuinely failed, including a read that could not look.
set -uo pipefail

ME=retire-access-object
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
DERIVE="$REPO/infra/cluster/undeclared-objects.sh"
TREE="${BOSS_CLUSTER_TREE:-${BOSS_OPS_CHECKOUT:-$REPO}}"
DECLARATION_DIRS=(infra/cluster/manifests infra/gate-runner)
# The ONE floor of this verb. Widening it is a reviewed change to this
# line, and retire_access_object_sh.rs names the change.
RETIRABLE_KINDS=(ServiceAccount Role RoleBinding)
WORKLOAD_KINDS=pods,jobs,cronjobs,deployments,statefulsets,daemonsets,replicasets
CAPTURE_MAX=1048576

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
say() { echo "$ME: $*" >&2; }

# --- the arguments ----------------------------------------------------------
PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 3 ] || refuse "usage: $ME --plan <Kind> <namespace> <name> — one object, named in full"
else
    [ $# -eq 4 ] || refuse "usage: $ME <Kind> <namespace> <name> <plan-sha256> — this deletes only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-an-access-object-retirement verb)"
    [[ "$4" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$4'"
    APPROVED="$4"
fi
KIND="$1"
NS="$2"
NAME="$3"
in_floor=0
for k in "${RETIRABLE_KINDS[@]}"; do [ "$k" = "$KIND" ] && in_floor=1; done
[ "$in_floor" -eq 1 ] \
    || refuse "'$KIND' is not a kind this verb retires — it retires ServiceAccount, Role, RoleBinding and nothing else (a claim is retire-volume-claim's, and Service, ConfigMap, Deployment, CronJob, Job are delete-orphan-object's)"
[[ "$NS" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
    || refuse "the namespace '$NS' is not a DNS label"
[[ "$NAME" =~ ^[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?$ ]] \
    || refuse "the name '$NAME' is not a DNS subdomain name"
if [ "$KIND" = ServiceAccount ] && [ "$NAME" = default ]; then
    refuse "ServiceAccount '$NAME' is protected in every namespace — the control plane recreates it and every Pod that names no account runs as it"
fi
case "$NAME" in
    break-glass*) refuse "'$NAME' is protected in every namespace — break-glass* is the recovery road (infra/recovery/re-entry.toml), which must outlive this tree being wrong" ;;
esac
if [ "$PLAN" -eq 0 ]; then
    [[ "${OPS_REQUEST_ID:-}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "OPS_REQUEST_ID is '${OPS_REQUEST_ID:-}', not a request id — the capture of what is deleted has nowhere to land, so nothing is deleted (the ops runner sets it for the execute step)"
fi
case "$KIND" in
    ServiceAccount) API_PATH="/api/v1/namespaces/$NS/serviceaccounts/$NAME" ;;
    Role) API_PATH="/apis/rbac.authorization.k8s.io/v1/namespaces/$NS/roles/$NAME" ;;
    RoleBinding) API_PATH="/apis/rbac.authorization.k8s.io/v1/namespaces/$NS/rolebindings/$NAME" ;;
esac

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the object cannot be read, and no evidence is not a plan"
# jq-1.6 passes `jq -e` over an input holding no document (backlog
# d96e38ab), so every identity check below asks jq_doc_file first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
[ -x "$DERIVE" ] || refuse "the derivation $DERIVE is missing or not executable, so there is no authority to act on"
WAIT_S="${BOSS_RETIRE_ACCESS_WAIT_S:-60}"
case "${WAIT_S:-empty}" in
    empty | *[!0-9]*) fail "BOSS_RETIRE_ACCESS_WAIT_S '$WAIT_S' is not whole seconds" ;;
esac

WORK=$(mktemp -d) || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- a committed declaration set, read as the checkout's owner --------------
# retire-volume-claim.sh's bound, for its reason: the runner executes verbs
# as root, git refuses a clone root does not own, and silencing that with
# safe.directory would make root-owned objects in the owner's clone one
# edit away.
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so there is no declaration set to read"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}') to an account on this host, so there is no account to read its git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
if ! as_owner "git -C '$TREE' rev-parse --git-dir" >/dev/null 2> "$WORK/git.err"; then
    sed 's/^/    /' "$WORK/git.err" >&2
    refuse "cannot read $TREE as a git checkout (read as '$OWNER'), so the declaration set is not reviewable content (git's words above)"
fi
dirty=$(as_owner "git -C '$TREE' status --porcelain -- ${DECLARATION_DIRS[*]}" 2>&1)
if [ -n "$dirty" ]; then
    printf '    %s\n' "$dirty" >&2
    refuse "${DECLARATION_DIRS[*]} carry uncommitted changes (above), so the tree that would prove this object undeclared is not one anybody reviewed"
fi
say "declaration set: ${DECLARATION_DIRS[*]} at $(as_owner "git -C '$TREE' rev-parse --short HEAD" 2>/dev/null || echo '?') (owner $OWNER)"

# --- the derivation: a manifest declares it, or not --------------------------
BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --check "$KIND/$NS/$NAME" > "$WORK/check.out" 2> "$WORK/check.err"
rc=$?
cat "$WORK/check.err" >&2
case "$rc" in
    0) ;;
    3) refuse "undeclared-objects.sh does not name $KIND/$NS/$NAME undeclared (its reason above)" ;;
    *) fail "undeclared-objects.sh could not decide (exit $rc, above) — 'could not look' is not 'undeclared'" ;;
esac
IFS=$'\t' read -r verdict ckind cns cname < "$WORK/check.out"
[ "${verdict:-}/${ckind:-}/${cns:-}/${cname:-}" = "undeclared/$KIND/$NS/$NAME" ] \
    || fail "the derivation answered '$(cat "$WORK/check.out")', not about $KIND/$NS/$NAME"

KUBECTL_LINE=$(BOSS_CLUSTER_TREE="$TREE" "$DERIVE" --kubectl) || fail "no kubectl to read the object with (above)"
read -r -a KUBECTL <<< "$KUBECTL_LINE"

# read_json <out> <resource> <name> [-n ns] — one object as JSON. 0 read,
# 2 NotFound (the server said so), 1 any other failure, words in <out>.err.
read_json() {
    local out="$1"; shift
    if "${KUBECTL[@]}" get "$@" -o json --request-timeout=30s > "$out" 2> "$out.err"; then return 0; fi
    grep -q 'NotFound' "$out.err" && return 2
    return 1
}
# list_json <out> <what> <kubectl get args...> — a list, or exit 1 naming
# what could not be read: a list nobody read is not an empty one.
list_json() {
    local out="$1" what="$2"; shift 2
    if ! "${KUBECTL[@]}" get "$@" -o json --request-timeout=30s > "$out" 2> "$out.err"; then
        sed 's/^/    /' "$out.err" >&2
        fail "could not read $what (above) — a list nobody read is not one with no consumer, so there is no plan"
    fi
    jq_doc_file "$out" && jq -e '(.items | type) == "array"' "$out" > /dev/null \
        || fail "the answer for $what is not a list — no answer is not an empty one"
}

# --- the object --------------------------------------------------------------
read_json "$WORK/obj.json" "$KIND" "$NAME" -n "$NS"
case $? in
    0) ;;
    2) refuse "there is no $KIND $NS/$NAME (NotFound) — a delete of it would be a no-op reported as work" ;;
    *) sed 's/^/    /' "$WORK/obj.json.err" >&2; fail "could not read $KIND $NS/$NAME (above)" ;;
esac
jq_doc_file "$WORK/obj.json" || fail "the API server's answer for $KIND $NS/$NAME holds no JSON document — no answer is not an object"
jq -e --arg k "$KIND" --arg n "$NAME" --arg ns "$NS" '.kind == $k and .metadata.name == $n
    and ((.metadata.namespace // $ns) == $ns) and ((.metadata.uid // "") != "")' "$WORK/obj.json" > /dev/null \
    || fail "the API server's answer for $KIND $NS/$NAME is not that object with a uid"
UID_=$(jq -r '.metadata.uid' "$WORK/obj.json")
[ "$(jq -r '.metadata.deletionTimestamp // ""' "$WORK/obj.json")" = "" ] \
    || refuse "$KIND $NS/$NAME is already being deleted (deletionTimestamp $(jq -r '.metadata.deletionTimestamp' "$WORK/obj.json"), finalizers $(jq -r '(.metadata.finalizers // []) | join(",")' "$WORK/obj.json")) — that is not this verb's to force"
[ "$(jq '(.metadata.ownerReferences // []) | length' "$WORK/obj.json")" = 0 ] \
    || refuse "$KIND $NS/$NAME carries an ownerReference ($(jq -c '.metadata.ownerReferences' "$WORK/obj.json")); deleting its owner releases it"

# --- consumers -----------------------------------------------------------------
: > "$WORK/users"
# runs_as <ns> <account> — every Pod (any phase) in <ns> running as the
# account, and every workload whose template would run one as it, appended
# to $WORK/users. A Pod that names no account runs as `default`.
runs_as() {
    list_json "$WORK/workloads-$1.json" "$WORKLOAD_KINDS in $1" "$WORKLOAD_KINDS" -n "$1"
    jq -r --arg sa "$2" --arg ns "$1" '
        .items[] as $o | ($o.kind // "?") as $k
        | (if $k == "Pod" then $o.spec
           elif $k == "CronJob" then $o.spec.jobTemplate.spec.template.spec
           else $o.spec.template.spec end) as $ps
        | select(($ps.serviceAccountName // $ps.serviceAccount // "default") == $sa)
        | "\($k) \($ns)/\($o.metadata.name)"
          + (if $k == "Pod" then " (phase \($o.status.phase // "unknown")) runs as ServiceAccount \($ns)/\($sa)"
             else " would run a Pod as ServiceAccount \($ns)/\($sa)" end)
        ' "$WORK/workloads-$1.json" >> "$WORK/users" 2> "$WORK/jq.err" \
        || { sed 's/^/    /' "$WORK/jq.err" >&2; fail "the workload list for $1 would not parse (above)"; }
}
# holds_token <ns> <account> — every long-lived token Secret in <ns>
# issued from the account, appended to $WORK/users. Read without their
# bytes: name, type and the account annotation only. A token Secret is a
# holder OUTSIDE any Pod — whoever has its bytes (the boss-gcp kubeconfig's
# dev-session-token, the break-glass operator's token) — so a binding's
# subjects are scanned for one exactly as an account is: review 64b37b0c
# B1 found the RoleBinding branch planning "consumers: none" over one.
holds_token() {
    if ! "${KUBECTL[@]}" get Secret -n "$1" --request-timeout=30s \
            -o 'jsonpath={range .items[*]}{.metadata.name}{"\t"}{.type}{"\t"}{.metadata.annotations.kubernetes\.io/service-account\.name}{"\n"}{end}' \
            > "$WORK/secrets-$1.tsv" 2> "$WORK/secrets-$1.err"; then
        sed 's/^/    /' "$WORK/secrets-$1.err" >&2
        fail "could not read the Secrets in $1 (above) — a token nobody looked for is not a token that does not exist, so there is no plan"
    fi
    awk -F'\t' -v sa="$2" -v ns="$1" \
        '$2 == "kubernetes.io/service-account-token" && $3 == sa { print "Secret " ns "/" $1 " is a long-lived token issued from ServiceAccount " ns "/" sa }' \
        "$WORK/secrets-$1.tsv" >> "$WORK/users"
}

case "$KIND" in
    ServiceAccount)
        runs_as "$NS" "$NAME"
        holds_token "$NS" "$NAME"
        list_json "$WORK/rb.json" "the RoleBindings in every namespace" RoleBinding -A
        list_json "$WORK/crb.json" "the ClusterRoleBindings" ClusterRoleBinding
        jq -r --arg sa "$NAME" --arg ns "$NS" '
            .items[] as $b
            | select(any(($b.subjects // [])[]; .kind == "ServiceAccount" and .name == $sa
                         and ((.namespace // $b.metadata.namespace // "") == $ns)))
            | "\($b.kind // "RoleBinding") \(if $b.metadata.namespace then "\($b.metadata.namespace)/" else "" end)\($b.metadata.name) grants it \($b.roleRef.kind)/\($b.roleRef.name)"
            ' "$WORK/rb.json" "$WORK/crb.json" >> "$WORK/users" 2> "$WORK/jq.err" \
            || { sed 's/^/    /' "$WORK/jq.err" >&2; fail "the binding lists would not parse (above)"; }
        CONSUMERS="no Pod (any phase) in $NS runs as it and no Job, CronJob, Deployment, StatefulSet, DaemonSet or ReplicaSet there would run one as it, so no Pod holds a token issued from it or will be issued one; no Secret in $NS is a long-lived token issued from it; and no RoleBinding in any namespace and no ClusterRoleBinding grants it anything"
        UNSEEN="a token minted from it through the TokenRequest API (kubectl create token) and carried outside the cluster leaves no object any read can find, so this plan cannot name its holder; the delete invalidates such a token"
        ;;
    Role)
        list_json "$WORK/rb.json" "the RoleBindings in $NS" RoleBinding -n "$NS"
        jq -r --arg r "$NAME" '
            .items[] | select(.roleRef.kind == "Role" and .roleRef.name == $r)
            | "RoleBinding \(.metadata.namespace)/\(.metadata.name) grants it to \([(.subjects // [])[] | "\(.kind) \(.name)"] | join(", "))"
            ' "$WORK/rb.json" >> "$WORK/users" 2> "$WORK/jq.err" \
            || { sed 's/^/    /' "$WORK/jq.err" >&2; fail "the RoleBinding list for $NS would not parse (above)"; }
        CONSUMERS="no RoleBinding in $NS grants this Role"
        UNSEEN=""
        ;;
    RoleBinding)
        jq -r '(.subjects // [])[] | select(.kind != "ServiceAccount") | "\(.kind) \(.name)"' "$WORK/obj.json" > "$WORK/opaque"
        if [ -s "$WORK/opaque" ]; then
            sed 's/^/    /' "$WORK/opaque" >&2
            refuse "$NS/$NAME grants to $(paste -sd, "$WORK/opaque" | sed 's/,/, /g') — no read of the cluster names who holds a User or Group, so no plan can name every consumer of this grant"
        fi
        jq -r --arg ns "$NS" '(.subjects // [])[] | "\(.namespace // $ns)\t\(.name)"' "$WORK/obj.json" | LC_ALL=C sort -u > "$WORK/subjects"
        while IFS=$'\t' read -r sns sname; do
            [ -n "$sname" ] || continue
            [[ "$sns" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] \
                || refuse "a subject of $NS/$NAME names namespace '$sns', which is not a DNS label"
            runs_as "$sns" "$sname"
            holds_token "$sns" "$sname"
        done < "$WORK/subjects"
        CONSUMERS="every subject is a ServiceAccount, and no Pod (any phase) runs as one of them, no Job, CronJob, Deployment, StatefulSet, DaemonSet or ReplicaSet would run one as one of them, and no Secret in a subject's namespace is a long-lived token issued from one of them"
        UNSEEN="a token minted from a subject through the TokenRequest API (kubectl create token) and carried outside the cluster leaves no object any read can find, so this plan cannot name its holder; that holder loses this grant too"
        ;;
esac
if [ -s "$WORK/users" ]; then
    sed 's/^/    /' "$WORK/users" >&2
    refuse "$KIND $NS/$NAME is in use (above) — an account something runs as or something grants, a role something binds, or a grant something holds is not retired"
fi

# --- the plan document ----------------------------------------------------------
render() {
    echo "plan: retire-access-object"
    jq -r --arg ns "$NS" '
        "object: \(.kind) \($ns)/\(.metadata.name)",
        "  uid: \(.metadata.uid)",
        "  created: \(.metadata.creationTimestamp // "unknown")",
        "  labels: \((.metadata.labels // {}) | if length == 0 then "none" else (to_entries | sort_by(.key) | from_entries | tojson) end)",
        "  annotation keys: \((.metadata.annotations // {}) | keys | if length == 0 then "none" else join(", ") end)",
        "  owner references: none",
        "  finalizers: \((.metadata.finalizers // []) | if length == 0 then "none" else join(", ") end)"
        ' "$WORK/obj.json"
    case "$KIND" in
        ServiceAccount)
            jq -r '
                "  automount token: \(if has("automountServiceAccountToken") then (.automountServiceAccountToken | tostring) else "unset" end)",
                "  secrets it names (kept): \([(.secrets // [])[].name] | if length == 0 then "none" else join(", ") end)",
                "  image pull secrets it names (kept): \([(.imagePullSecrets // [])[].name] | if length == 0 then "none" else join(", ") end)"
                ' "$WORK/obj.json"
            ;;
        Role)
            echo "  rules (the privilege this delete removes):"
            jq -r '(.rules // []) | if length == 0 then "    none" else .[] | "    - \(tojson)" end' "$WORK/obj.json"
            ;;
        RoleBinding)
            jq -r --arg ns "$NS" '
                "  grants: \(.roleRef.kind) \(if .roleRef.kind == "Role" then "\($ns)/" else "" end)\(.roleRef.name)",
                "  subjects (each loses this grant):",
                ((.subjects // []) | if length == 0 then "    none" else .[] | "    - \(.kind) \(.namespace // $ns)/\(.name)" end)
                ' "$WORK/obj.json"
            ;;
    esac
    echo "consumers: none — $CONSUMERS"
    # What no read can see is said, not implied away (review 64b37b0c N1).
    [ -z "$UNSEEN" ] || echo "cannot read: $UNSEEN"
    echo "authority: infra/cluster/undeclared-objects.sh --check names $KIND/$NS/$NAME undeclared (a namespace the tree owns, no manifest declares it, no controller owns it), and it is not protected (ServiceAccount default, break-glass*)"
    echo
    echo "== the delete this plan authorises =="
    echo "DELETE $API_PATH with preconditions.uid=$UID_"
    echo "then: nothing else — this verb deletes one object and reads it back gone."
}

render > "$WORK/plan"
HASH=$(sha256sum "$WORK/plan" | cut -d' ' -f1)

if [ "$PLAN" -eq 1 ]; then cat "$WORK/plan"; echo "plan-sha256: $HASH" >&2; exit 0; fi

# --- the write ------------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "the plan as it stands (above) hashes to $HASH, not the approved $APPROVED: a fact the signed plan recorded has moved, the plan was signed for another object, or this one is already gone. Nothing was deleted; render and approve it again"
fi

BOSS_BIN="${BOSS_BIN:-}"
if [ -z "$BOSS_BIN" ]; then
    if command -v boss >/dev/null 2>&1; then BOSS_BIN="$(command -v boss)"; else BOSS_BIN=/usr/local/bin/boss; fi
fi
[ -x "$BOSS_BIN" ] || fail "no boss CLI at $BOSS_BIN to attach the capture with — nothing was deleted"

cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — capturing $KIND $NS/$NAME onto request $OPS_REQUEST_ID before the delete"

CAP="$WORK/retire-access-object-$KIND-$NS-$NAME-${UID_:0:8}.yaml"
{
    echo "# retire-access-object capture for request $OPS_REQUEST_ID, plan $APPROVED"
    echo "# the object as the API server served it immediately before the delete"
    echo "--- $KIND $NS/$NAME ---"
} > "$CAP"
"${KUBECTL[@]}" get "$KIND" "$NAME" -n "$NS" -o yaml --request-timeout=30s >> "$CAP" 2> "$WORK/cap.err" \
    || { sed 's/^/    /' "$WORK/cap.err" >&2; fail "could not capture $KIND $NS/$NAME (above) — nothing was deleted: the capture is the only copy there will be"; }
CAP_N=$(wc -c < "$CAP" | tr -d ' ')
[ "$CAP_N" -le "$CAPTURE_MAX" ] || fail "the capture is $CAP_N bytes, over this verb's $CAPTURE_MAX — not the one object it expects; nothing was deleted"
CAP_SHA=$(sha256sum "$CAP" | cut -d' ' -f1)
if ! "$BOSS_BIN" attach "$OPS_REQUEST_ID" "$CAP" > "$WORK/attach.out" 2> "$WORK/attach.err"; then
    sed 's/^/    /' "$WORK/attach.out" "$WORK/attach.err" >&2
    fail "the capture did not land on request $OPS_REQUEST_ID (boss attach's words above) — nothing was deleted"
fi
FILE_ID=$(sed -n 's/.* as file \([0-9a-f-]\{36\}\),.*/\1/p' "$WORK/attach.out")
if [ "$(printf '%s\n' "$FILE_ID" | grep -c .)" -ne 1 ] || ! grep -qF "$CAP_SHA" "$WORK/attach.out" \
        || ! grep -qF 'byte-identical' "$WORK/attach.out"; then
    sed 's/^/    /' "$WORK/attach.out" >&2
    fail "boss attach exited 0 but its receipt (above) does not name one file id, the capture's sha256 $CAP_SHA and a byte-identical read-back — nothing was deleted"
fi
echo "--- the capture, as attached ---"
cat "$CAP"
echo "--- end of the capture ---"
echo "$ME: captured $KIND $NS/$NAME ($CAP_N bytes, sha256 $CAP_SHA) onto request $OPS_REQUEST_ID as file $FILE_ID, read back byte-identical"

# The delete reads its DeleteOptions body from stdin, and the docker form
# of the resolved kubectl runs without -i, which would drop the
# precondition SILENTLY (reap-terminated-pods.sh says the same).
if [ "${KUBECTL[0]}" = docker ] && [ "${KUBECTL[1]:-}" = run ]; then
    KDEL=(docker run -i "${KUBECTL[@]:2}")
else
    KDEL=("${KUBECTL[@]}")
fi
jq -n --arg uid "$UID_" '{kind: "DeleteOptions", apiVersion: "v1", preconditions: {uid: $uid}}' > "$WORK/body.json"
echo "$ME: deleting $KIND $NS/$NAME with preconditions.uid=$UID_"
if ! "${KDEL[@]}" delete --raw "$API_PATH" -f - --request-timeout=60s \
        < "$WORK/body.json" > "$WORK/del.out" 2> "$WORK/del.err"; then
    sed 's/^/    /' "$WORK/del.err" >&2
    fail "the delete of $KIND $NS/$NAME (uid $UID_) was refused or failed (above). A precondition refusal means a different object now holds the name and it was left alone; any other failure leaves the outcome unread — read it with plan-an-access-object-retirement"
fi

# --- the read-back: fails closed --------------------------------------------------
# Gone is only ever one of two READS: the server's NotFound, or a document
# naming a DIFFERENT non-empty uid. A get that exits 0 with no document,
# or with one carrying no uid, read nothing; a get that fails any other way
# (Forbidden, a timeout, an unreachable server) read nothing. Neither is
# gone (backlog 37338a58, 1058e686 car B: both shapes once printed OK).
deadline=$((SECONDS + WAIT_S))
while :; do
    read_json "$WORK/back.json" "$KIND" "$NAME" -n "$NS"; rc=$?
    if [ "$rc" -eq 2 ]; then BACK="NotFound"; break; fi
    if [ "$rc" -eq 0 ]; then
        if ! jq_doc_file "$WORK/back.json"; then
            rc=1; echo "the get exited 0 with no JSON document" > "$WORK/back.json.err"
        else
            now=$(jq -r '.metadata.uid // ""' "$WORK/back.json")
            if [ -z "$now" ]; then
                rc=1; echo "the get answered a document with no uid: $(head -c 200 "$WORK/back.json" | tr '\n' ' ')" > "$WORK/back.json.err"
            elif [ "$now" != "$UID_" ]; then
                BACK="its name is now held by uid $now, a different object"; break
            else
                STILL=$(jq -r 'if .metadata.deletionTimestamp then "being deleted since \(.metadata.deletionTimestamp), held by finalizers \((.metadata.finalizers // []) | join(","))" else "present" end' "$WORK/back.json")
            fi
        fi
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
        if [ "$rc" -ne 0 ]; then
            fail "deleted, not proven gone: the delete of $KIND $NS/$NAME was accepted and the read-back is unreadable: $(tr '\n' ' ' < "$WORK/back.json.err")"
        fi
        fail "the delete of $KIND $NS/$NAME was accepted and after ${WAIT_S}s uid $UID_ is still there, $STILL. Nothing more was done; read it with plan-an-access-object-retirement"
    fi
    sleep 2
done
echo "$ME: $KIND $NS/$NAME uid $UID_ is gone (read back: $BACK)"
