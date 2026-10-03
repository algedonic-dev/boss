#!/usr/bin/env bash
# longhorn-drain-policy.sh — set Longhorn's node-drain-policy, through a
# rendered plan a passkey signs, and read how long it has stood
# non-default. The ops verbs `plan-a-longhorn-drain-policy` (--plan),
# `set-longhorn-drain-policy` and `check-longhorn-drain-policy` (--check).
#
#   longhorn-drain-policy.sh --plan <value>
#   longhorn-drain-policy.sh <value> <plan-sha256>
#   longhorn-drain-policy.sh --check
#
# WHY IT EXISTS (backlog 46584350; backlog 52ea56ac, review 8599a3a8
# M1/M2). w-1 drains on the morning of 2026-09-30 for its second NVMe,
# and under Longhorn's default node-drain-policy
# (block-if-contains-last-replica) the dev /work volume and
# gate-runner-disk — one replica each, on w-1 — hold that drain. w-1 is
# the only talos-worker, so a second replica could only land on a
# control plane beside etcd; and the NVMe is ADDED, w-1's own disk
# stays, so a one-replica volume survives the power-off. David chose
# 2026-09-30 ~05:05Z: relax the policy for the window
# (allow-if-replica-is-stopped) and put it back after w-1 boots. Both
# directions are this one verb. The dev session cannot read or write
# longhorn.io, so it is a forge verb over the ops_kubectl door (the admin
# kubeconfig David placed, infra/estate/ops-credentials.sh).
#
# THE SHAPE is design 17835005's, as set-volume-replicas' and
# shutdown-node's: the write's verb file declares requires_approval and
# names plan-a-longhorn-drain-policy as its plan_verb, so the runner
# renders --plan onto the request's approve step and hands the write
# sha256 of the SIGNED plan. The write re-renders and patches nothing
# unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter:
#   * ONE setting, settings.longhorn.io/node-drain-policy in
#     longhorn-system, fixed here — no argument names a setting;
#   * the value is one of the options LONGHORN's live definition
#     enumerates (its manager API, GET /v1/settings/node-drain-policy,
#     read through the API server's service proxy), never a list typed
#     here: a Longhorn upgrade that adds or drops an option moves the
#     bound with it. A definition that is read-only, or that enumerates
#     nothing, is a refusal;
#   * the value differs from the one that stands — a plan that changes
#     nothing is refused, so an applied plan re-renders to a refusal: at
#     most once;
#   * the stored setting and Longhorn's API agree on the value that
#     stands — a disagreement acts on nothing.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the
# setting, the value before and after, Longhorn's default, the options
# its definition allows, Longhorn's own words for the requested option,
# the one patch, and every Longhorn volume whose only healthy replica is
# on a worker node — the volumes the policy decides for. A non-default
# value carries a note naming the daily check that alarms on it. No clock
# and no volume state in the bytes (a gate's disk attaches and detaches
# many times an hour, and a plan that signed it could never be run); the
# states ride stderr.
#
# THE WRITE is one `kubectl patch settings.longhorn.io --type=json` that
# TESTS `value` is still the planned one and replaces it — a
# compare-and-set on the one field. Longhorn's admission webhook judges
# the value again.
#
# THE EFFECT IS READ BACK: every BOSS_DRAIN_POLICY_POLL_S (default 5) up
# to BOSS_DRAIN_POLICY_WAIT_S (default 90, inside the verb's 180 s
# timeout), the stored setting AND Longhorn's API. Only when both read
# the new value and the API says `applied` is the line the verb file
# declares as `effect` printed; otherwise exit 1 naming what each read.
#
# THE CHECK (--check, backlog 46584350: "an alarm that fires if
# node-drain-policy stays non-default") reads the value, Longhorn's
# default, and — when they differ — WHEN the value was last written: the
# API server's own managedFields time for the entry that owns `value`
# (never a clock this script kept). Its last line is the verdict
# `READ — node-drain-policy <v>, default <d>, non_default <0|1>, hours
# <h>`; the threshold is not here but in the watch rule's `when`
# (infra/dispatcher/rules/watch-check-longhorn-drain-policy-daily.toml),
# which files an urgent alarm past it. A non-default value whose age
# cannot be read is CANNOT ANSWER, exit 1 — which the watch also alarms.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan and never a default. Exit 78 is a refusal.
set -uo pipefail

ME=set-longhorn-drain-policy
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
SETTING=node-drain-policy
# Longhorn's manager API, through the API server's service proxy: the
# forge is outside the pod network, and this is where the setting's
# DEFINITION (its options, its default) lives — the CR carries only the
# value. Measured 2026-09-30 against Longhorn v1.11.3: the definition
# answers `options` and `default` under `.definition`.
DEFINITION="/api/v1/namespaces/$LH/services/longhorn-backend:9500/proxy/v1/settings/$SETTING"
VALUE_RE='^[a-z]([a-z-]{0,61}[a-z])?$'

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread setting has no answer, and nothing was written"; }

MODE=write
case "${1-}" in
    --plan)
        MODE=plan
        shift
        [ $# -eq 1 ] || refuse "usage: longhorn-drain-policy.sh --plan <value> — the plan takes the value and nothing else"
        ;;
    --check)
        MODE=check
        ME=check-longhorn-drain-policy
        shift
        [ $# -eq 0 ] || refuse "usage: longhorn-drain-policy.sh --check — the check takes nothing"
        ;;
    *)
        [ $# -eq 2 ] || refuse "usage: longhorn-drain-policy.sh <value> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-longhorn-drain-policy verb)"
        [[ "$2" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$2'"
        APPROVED="$2"
        ;;
esac
WANT=""
if [ "$MODE" != check ]; then
    WANT="$1"
    [[ "$WANT" =~ $VALUE_RE ]] || refuse "the value must be one lowercase hyphenated word (one of Longhorn's node-drain-policy options), got '$WANT'"
fi
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not an answer"

WAIT_S="${BOSS_DRAIN_POLICY_WAIT_S:-90}"
POLL_S="${BOSS_DRAIN_POLICY_POLL_S:-5}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_DRAIN_POLICY_WAIT_S and BOSS_DRAIN_POLICY_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# read_setting — the stored setting into $WORK/setting.json, with the
# managedFields kubectl hides by default (the check dates the value by
# them). Sets CUR.
read_setting() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get settings.longhorn.io "$SETTING" -n "$LH" -o json --show-managed-fields --request-timeout=20s \
            > "$WORK/setting.json" 2> "$WORK/setting.err"; then
        unread "settings.longhorn.io $SETTING: $(tr '\n' ' ' < "$WORK/setting.err")"
    fi
    jq_doc_file "$WORK/setting.json" \
        && jq -e --arg s "$SETTING" '.metadata.name == $s and (.value | type == "string") and .value != ""' \
            "$WORK/setting.json" > /dev/null 2>&1 \
        || unread "settings.longhorn.io $SETTING did not read back as that setting with a value (its first 200 bytes: '$(head -c 200 "$WORK/setting.json" | tr '\n' ' ')')"
    CUR="$(jq -r '.value' "$WORK/setting.json")"
}

# read_definition — Longhorn's API answer for the setting into
# $WORK/def.json. Sets DEF (the default), API_VALUE, APPLIED, OPTIONS
# (comma-joined) and READONLY.
read_definition() {
    # shellcheck disable=SC2086
    if ! $K get --raw "$DEFINITION" --request-timeout=20s > "$WORK/def.json" 2> "$WORK/def.err"; then
        unread "Longhorn's definition of $SETTING ($DEFINITION): $(tr '\n' ' ' < "$WORK/def.err")"
    fi
    jq_doc_file "$WORK/def.json" \
        && jq -e --arg s "$SETTING" '.id == $s
            and (.definition.options | type == "array" and length > 0 and all(type == "string"))
            and (.definition.default | type == "string" and . != "")
            and (.value | type == "string")' "$WORK/def.json" > /dev/null 2>&1 \
        || unread "Longhorn's API did not answer a definition of $SETTING with its options and default (its first 200 bytes: '$(head -c 200 "$WORK/def.json" | tr '\n' ' ')')"
    DEF="$(jq -r '.definition.default' "$WORK/def.json")"
    API_VALUE="$(jq -r '.value' "$WORK/def.json")"
    APPLIED="$(jq -r 'if .applied == true then "true" elif .applied == false then "false" else "unreported" end' "$WORK/def.json")"
    OPTIONS="$(jq -r '.definition.options | join(", ")' "$WORK/def.json")"
    READONLY="$(jq -r 'if .definition.readOnly == true then "true" else "false" end' "$WORK/def.json")"
}

read_setting
read_definition
[ "$CUR" = "$API_VALUE" ] \
    || unread "the stored setting says $SETTING is '$CUR' and Longhorn's API says '$API_VALUE' — they disagree, so which one stands is not known"

# --- the check -----------------------------------------------------------------
if [ "$MODE" = check ]; then
    echo "$ME: $SETTING is $CUR (settings.longhorn.io, and Longhorn's API agrees; applied $APPLIED); Longhorn's default is $DEF; options: $OPTIONS"
    if [ "$CUR" = "$DEF" ]; then
        echo "$ME: READ — node-drain-policy $CUR, default $DEF, non_default 0, hours 0"
        exit 0
    fi
    # WHEN the value was last written: the API server's managedFields
    # entry that owns `value` (a status write owns no `f:value`). The
    # latest such time, in UTC, compared as epochs.
    since="$(jq -r '[(.metadata.managedFields // [])[]
            | select((.subresource // "") == "" and ((.fieldsV1 // {}) | has("f:value")))
            | .time // empty] | sort | last // ""' "$WORK/setting.json" 2>/dev/null)"
    who="$(jq -r --arg t "$since" '[(.metadata.managedFields // [])[]
            | select((.subresource // "") == "" and ((.fieldsV1 // {}) | has("f:value")) and .time == $t)
            | .manager // "an unnamed manager"] | unique | join(", ")' "$WORK/setting.json" 2>/dev/null)"
    case "${since:-empty}" in
        empty) fail "CANNOT ANSWER — $SETTING is $CUR, not Longhorn's default $DEF, and no managedFields entry says when its value was written (read with --show-managed-fields), so how long it has stood is unknown. The way back is plan-a-longhorn-drain-policy $DEF, then set-longhorn-drain-policy with its passkey" ;;
    esac
    since_s="$(date -u -d "$since" +%s 2>/dev/null)"
    now_s="${BOSS_DRAIN_POLICY_NOW:-$(date -u +%s)}"
    case "${since_s:-empty}${now_s:-empty}" in
        *[!0-9]*) fail "CANNOT ANSWER — $SETTING is $CUR, not Longhorn's default $DEF, and its write time '$since' (or the clock '$now_s') is not a time, so how long it has stood is unknown" ;;
    esac
    hours=$(((now_s - since_s) / 3600))
    [ "$hours" -ge 0 ] || hours=0
    echo "$ME: NON-DEFAULT — $SETTING has been $CUR since $since (${hours} hour(s)), written by ${who:-an unnamed manager}. The way back is plan-a-longhorn-drain-policy $DEF, then set-longhorn-drain-policy with its passkey"
    echo "$ME: READ — node-drain-policy $CUR, default $DEF, non_default 1, hours $hours"
    exit 0
fi

# --- the plan ------------------------------------------------------------------
# read_list <resource> <file> — a list into $WORK/<file>.
read_list() {
    local ns=()
    [ "$1" = nodes ] || ns=(-n "$LH")
    # shellcheck disable=SC2086
    if ! $K get "$1" "${ns[@]}" -o json --request-timeout=30s > "$WORK/$2" 2> "$WORK/$2.err"; then
        unread "$1: $(tr '\n' ' ' < "$WORK/$2.err")"
    fi
    jq_doc_file "$WORK/$2" && jq -e '.items | type == "array"' "$WORK/$2" >/dev/null 2>&1 \
        || unread "$1 did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/$2" | tr '\n' ' ')')"
}

# render — the bounds, then the plan into $WORK/plan and the volumes'
# live states into $WORK/states.
render() {
    local opt
    opt="$(jq -r --arg w "$WANT" '.definition.options | index($w) // "none"' "$WORK/def.json")"
    [ "$opt" != none ] \
        || refuse "'$WANT' is not a $SETTING option — Longhorn's live definition allows: $OPTIONS"
    [ "$READONLY" = false ] \
        || refuse "Longhorn's definition marks $SETTING read-only, so it is not a setting to write"
    [ "$WANT" != "$CUR" ] \
        || refuse "$SETTING is already $WANT (Longhorn's default is $DEF) — a plan that changes nothing is not a plan, and a second run of an applied one is refused here"
    read_list nodes k8snodes.json
    read_list replicas.longhorn.io replicas.json
    read_list volumes.longhorn.io volumes.json
    # The volumes the policy decides for: every healthy replica
    # (spec.healthyAt set, spec.failedAt empty — Longhorn's own test, the
    # one node-status reads) on ONE node, and that node a worker (no
    # control-plane label on its Node).
    if ! jq -r --slurpfile nodes "$WORK/k8snodes.json" --slurpfile vols "$WORK/volumes.json" '
            [$nodes[0].items[]
             | select(((.metadata.labels // {}) | has("node-role.kubernetes.io/control-plane") or has("node-role.kubernetes.io/master")) | not)
             | .metadata.name] as $workers
            | [.items[] | select((.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "")] as $h
            | [$h[] | .spec.volumeName] | unique[] as $v
            | ([$h[] | select(.spec.volumeName == $v) | .spec.nodeID] | unique) as $on
            | select(($on | length) == 1 and ($workers | index($on[0])) != null)
            | ([$vols[0].items[] | select(.metadata.name == $v)] | .[0] // {}) as $vo
            | ($vo.status.kubernetesStatus // {}) as $ks
            | (if ($ks.pvcName // "") != "" then "pvc \($ks.namespace)/\($ks.pvcName)" else "no claim" end) as $pvc
            | "\($v)\t\($pvc)\t\($on[0])\t\($vo.status.state // "state unread")"' \
            "$WORK/replicas.json" > "$WORK/last" 2> "$WORK/last.err"; then
        unread "the replicas, volumes and nodes would not parse together: $(tr '\n' ' ' < "$WORK/last.err")"
    fi
    sort -o "$WORK/last" "$WORK/last"
    awk -F'\t' '{ printf "volume %s (%s, live state %s): only healthy replica on worker %s\n", $1, $2, $4, $3 }' \
        "$WORK/last" > "$WORK/states"
    {
        echo "plan: set-longhorn-drain-policy"
        echo "setting: settings.longhorn.io/$SETTING in $LH"
        echo "value: $CUR -> $WANT"
        echo "Longhorn's default: $DEF"
        echo "allowed (Longhorn's live definition): $OPTIONS"
        echo "Longhorn's words for $WANT: $(jq -r --arg w "$WANT" '[(.definition.description // "") | split("\n")[]
                | select(startswith("- **\($w)**"))
                | sub("^- \\*\\*[a-z-]+\\*\\* "; "")] | .[0] // "its definition describes no such option"' "$WORK/def.json")"
        echo "command: kubectl patch settings.longhorn.io $SETTING -n $LH --type=json — test that value is still $CUR, replace it with $WANT; the one field this writes."
        echo "read-back: until settings.longhorn.io and Longhorn's API both read $WANT and the API reports it applied."
        if [ "$WANT" = "$DEF" ]; then
            echo "note: $WANT is Longhorn's default — this puts the setting back."
        else
            echo "note: $WANT is NOT Longhorn's default ($DEF). check-longhorn-drain-policy reads it every day and files an urgent alarm once it has stood non-default for 24 hours; the way back is this verb with $CUR."
        fi
        echo
        echo "== longhorn volumes whose only healthy replica is on a worker ($(wc -l < "$WORK/last" | tr -d ' ')) =="
        if [ -s "$WORK/last" ]; then
            awk -F'\t' '{ printf "volume %s (%s): only healthy replica on worker %s\n", $1, $2, $3 }' "$WORK/last"
        else
            echo "none"
        fi
    } > "$WORK/plan"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
sed "s/^/$ME: not signed (a volume's state moves with every attach): /" "$WORK/states" >&2

if [ "$MODE" = plan ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write -----------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $SETTING (above) hashes to $HASH, not the approved plan $APPROVED — its value, Longhorn's definition or the volumes it decides for moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — setting $SETTING from $CUR to $WANT"

PATCH="[{\"op\":\"test\",\"path\":\"/value\",\"value\":\"$CUR\"},{\"op\":\"replace\",\"path\":\"/value\",\"value\":\"$WANT\"}]"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch settings.longhorn.io "$SETTING" -n "$LH" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch settings.longhorn.io $SETTING did not succeed (its words above) — this run cannot say the value changed; read it again with plan-a-longhorn-drain-policy before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted $SETTING=$WANT — reading it back (up to ${WAIT_S}s: the stored setting, and Longhorn's API applying it)"

# --- the read-back --------------------------------------------------------------
start="$(date -u +%s)"
stored="not read yet"
api="not read yet"
applied="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $K get settings.longhorn.io "$SETTING" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        stored="$(jq -r '.value // "unreported"' "$WORK/back.json")"
    else
        stored="unread: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    # shellcheck disable=SC2086
    if $K get --raw "$DEFINITION" --request-timeout=20s > "$WORK/backdef.json" 2> "$WORK/backdef.err" \
        && jq_doc_file "$WORK/backdef.json"; then
        api="$(jq -r '.value // "unreported"' "$WORK/backdef.json")"
        applied="$(jq -r 'if .applied == true then "true" elif .applied == false then "false" else "unreported" end' "$WORK/backdef.json")"
    else
        api="unread: $(tr '\n' ' ' < "$WORK/backdef.err" | cut -c1-200)"
        applied="unread"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$stored" = "$WANT" ] && [ "$api" = "$WANT" ] && [ "$applied" = true ]; then
        # The literal words are the verb file's `effect`, spelled here as
        # it reads them (a_mutating_verb_declares_its_effect.rs).
        echo "$ME: node-drain-policy reads back $WANT — settings.longhorn.io value $stored, Longhorn's API value $api, applied true (was $CUR; Longhorn's default $DEF; after ${elapsed}s)"
        exit 0
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        fail "Longhorn accepted $SETTING=$WANT, and after ${elapsed}s it is NOT proven: settings.longhorn.io value $stored, Longhorn's API value $api, applied $applied. The patch stands — read it again with check-longhorn-drain-policy"
    fi
    sleep "$POLL_S"
done
