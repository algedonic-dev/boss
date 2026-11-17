#!/usr/bin/env bash
# A signed, reversible change of ONE boolean setting (backlog c84ab39b).
# The disk spec inventory is signed: changing which disks this closes
# invalidates the plan. Status and free space are not signed; they move
# while a human reads. No replica is moved, detached or deleted here.
# Cross-object reads are not a transaction: the patch atomically tests
# the setting's value; concurrent disk edits still require coordination.
set -uo pipefail
ME=set-longhorn-selector-policy
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
LH=longhorn-system
SETTING=allow-empty-disk-selector-volume
DEFINITION="/api/v1/namespaces/$LH/services/longhorn-backend:9500/proxy/v1/settings/$SETTING"
refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $*; no evidence is not a pass"; }
MODE=write
if [ "${1-}" = --plan ]; then
    MODE=plan
    shift
    [ $# -eq 1 ] || refuse 'usage: longhorn-selector-policy.sh --plan <true|false>'
else
    [ $# -eq 2 ] || refuse 'usage: longhorn-selector-policy.sh <true|false> <plan-sha256>'
    [[ "$2" =~ ^[0-9a-f]{64}$ ]] || refuse 'the approved plan hash must be 64 lowercase hex characters'
    APPROVED="$2"
fi
WANT="$1"
case "$WANT" in true|false) ;; *) refuse 'the value must be true or false; no parameter names a setting';; esac
command -v jq >/dev/null 2>&1 || fail 'jq is not on PATH'
WAIT_S="${BOSS_SELECTOR_POLICY_WAIT_S:-90}"
case "${WAIT_S:-empty}" in *[!0-9]*) fail 'BOSS_SELECTOR_POLICY_WAIT_S must be whole seconds';; esac
[ "$WAIT_S" -le 90 ] || fail 'the read-back wait is bounded to 90 seconds'
WORK="$(mktemp -d)" || fail 'cannot make a scratch directory'
trap 'rm -rf "$WORK"' EXIT
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    unread "the kubectl image door: $(cat "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"
get() {
    local file="$1"
    shift
    # shellcheck disable=SC2086 # The published door prints argv for word splitting.
    if ! $K get "$@" --request-timeout=20s > "$WORK/$file" 2> "$WORK/$file.err"; then
        unread "$*: $(cat "$WORK/$file.err")"
    fi
    jq_doc_file "$WORK/$file" || unread "$* returned no single JSON document"
}
read_pair() {
    get setting.json settings.longhorn.io "$SETTING" -n "$LH" -o json
    get def.json --raw "$DEFINITION"
    jq_doc_file "$WORK/setting.json" && jq -e --arg s "$SETTING" '.metadata.name == $s and (.value == "true" or .value == "false")' "$WORK/setting.json" >/dev/null \
        || unread 'the stored setting did not identify the fixed boolean setting'
    jq_doc_file "$WORK/def.json" && jq -e --arg s "$SETTING" '.id == $s and .definition.type == "bool"
        and (.definition.default == "true" or .definition.default == "false")
        and (.definition.readOnly | type == "boolean")
        and (.value == "true" or .value == "false")
        and (.applied | type == "boolean")' "$WORK/def.json" >/dev/null \
        || unread 'Longhorn did not return a readable boolean definition and applied state'
    CUR="$(jq -r .value "$WORK/setting.json")"
    API_VALUE="$(jq -r .value "$WORK/def.json")"
    # Before signing, disagreement is unknown state. After the patch,
    # Longhorn's asynchronous manager can lag the CR; poll both until
    # they converge, rather than rejecting a successfully accepted write.
    if [ "${1:-plan}" = plan ]; then
        [ "$CUR" = "$API_VALUE" ] || unread "stored value $CUR disagrees with Longhorn API value $API_VALUE"
    fi
}
read_pair
[ "$(jq -r .definition.readOnly "$WORK/def.json")" = false ] || refuse 'Longhorn marks the setting read-only'
[ "$CUR" != "$WANT" ] || refuse "$SETTING already reads $WANT; an applied plan cannot run twice"
BEFORE="$CUR"
get nodes.json nodes.longhorn.io -n "$LH" -o json
jq_doc_file "$WORK/nodes.json" && jq -e '.items | type == "array" and length > 0 and all(
    (.metadata.name | type == "string" and length > 0)
    and (.metadata.uid | type == "string" and length > 0)
    and (.spec.disks | type == "object")
    and (.spec.disks | to_entries | all(
        (.value.path | type == "string" and length > 0)
        and ((.value.tags // []) | type == "array" and all(type == "string")))))' "$WORK/nodes.json" >/dev/null \
    || unread 'Longhorn nodes did not return a nonempty disk inventory with identities, paths and tags'
# Sign complete disk specs, including scheduling flags and limits, but
# exclude changing node/disk status. Canonical order makes list order irrelevant.
jq -S '[.items[] | {name:.metadata.name,uid:.metadata.uid,disks:.spec.disks}] | sort_by(.name)' "$WORK/nodes.json" > "$WORK/disks.json" \
    || unread 'the disk inventory could not be canonicalized'
if [ "$WANT" = false ]; then
    jq_doc_file "$WORK/nodes.json" && jq -e '[.items[].spec.disks | to_entries[]
        | select(.key | startswith("default-disk-"))
        | select((.value.tags // []) | length > 0)] | length == 0' "$WORK/nodes.json" >/dev/null \
        || refuse 'a default disk is already tagged; restore true before the durable-disk sequence, or keep default disks untagged'
    jq_doc_file "$WORK/nodes.json" && jq -e '[.items[].spec.disks[] | select((.tags // []) | length == 0)] | length > 0' "$WORK/nodes.json" >/dev/null \
        || refuse 'no untagged disk remains for volumes without a disk selector'
fi
{
    echo 'plan: set-longhorn-selector-policy'
    echo "setting: settings.longhorn.io/$SETTING in $LH"
    echo "value: $BEFORE -> $WANT"
    echo "definition: $(jq -Sc .definition "$WORK/def.json")"
    echo '== signed Longhorn disk specs (all nodes) =='
    cat "$WORK/disks.json"
    echo '== tagged disks affected by this setting =='
    jq -r '.items | sort_by(.metadata.name)[] | .metadata.name as $node | .spec.disks | to_entries | sort_by(.key)[]
        | select((.value.tags // []) | length > 0)
        | "\($node)/\(.key) path \(.value.path) tags \(.value.tags | join(","))"' "$WORK/nodes.json"
    echo "effect: requested $WANT. True permits unselected volumes on tagged disks; false excludes them from every tagged disk, including scratch and postgres. Existing replicas are not moved."
    echo "command: test /value is $BEFORE and replace /value with $WANT on the ONE setting."
    echo "read-back: stored value and Longhorn API both $WANT, applied true; also read-longhorn-setting $SETTING."
    echo "inverse: render and approve a fresh plan for $BEFORE; before tagging default disks durable, restore true or select every durable volume."
} > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
if [ "$MODE" = plan ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi
[ "$HASH" = "$APPROVED" ] || refuse "today's plan hashes to $HASH, not signed plan $APPROVED; re-render and approve it again; nothing was patched"
cat "$WORK/plan"
PATCH="$(jq -nc --arg before "$BEFORE" --arg want "$WANT" '[{op:"test",path:"/value",value:$before},{op:"replace",path:"/value",value:$want}]')"
# shellcheck disable=SC2086
if ! $K patch settings.longhorn.io "$SETTING" -n "$LH" --type=json -p "$PATCH" --request-timeout=20s; then
    fail 'the compare-and-set patch failed; read the setting again before any further action'
fi
start="$(date -u +%s)"
while :; do
    read_pair back
    if [ "$CUR" = "$WANT" ] && [ "$API_VALUE" = "$WANT" ] && [ "$(jq -r .applied "$WORK/def.json")" = true ]; then
        echo "$ME: allow-empty-disk-selector-volume reads back $WANT — settings.longhorn.io and Longhorn API agree, applied true (was $BEFORE)"
        exit 0
    fi
    elapsed=$(($(date -u +%s) - start))
    [ "$elapsed" -lt "$WAIT_S" ] || fail "the patch stands but is NOT proven: stored $CUR, API $API_VALUE, applied $(jq -r .applied "$WORK/def.json"); read-longhorn-setting $SETTING"
    sleep 5
done
