# shellcheck shell=bash
# longhorn-ledger.sh — Longhorn's own disk arithmetic, as jq definitions
# a forge verb that places or grows a replica reads, so it is written once.
#
#   . "$HERE/../lib/longhorn-ledger.sh"
#   jq -n --slurpfile nodes lhnodes.json --argjson opct 100 --argjson mpct 25 \
#       "$LONGHORN_LEDGER_JQ"' lh_disks($nodes[0].items) | map(lh_expansion(32212254720; $opct; $mpct))'
#
# WHY (backlog ab39a34e, incident d3c0a67c, 2026-10-01). The system of
# record's database volume could not grow past 30Gi because one replica
# sat on a ~110 GiB disk whose scheduling ledger had ~755 MB left, and
# Longhorn's admission webhook refused the next size. Three verbs need that
# arithmetic: expand-instance-volume (can every replica's disk take the
# growth?) and move-volume-replica (can the disk a replica moves to take
# it, and the growth after?), and retire-volume-replica (the physical
# growth ceiling through lh_physical and lh_physical_room). These latter
# interfaces stay as main shipped them during this merge. A second copy
# of a webhook's sum drifts from
# the first, and the first copy here DID: it judged growth with a
# required-storage term Longhorn passes as 0, and judged placement at
# twice the size where Longhorn uses the volume's actualSize (adversarial
# review 091904d3). So every definition below is one function of
# longhorn-manager v1.11.3 — the version the tree records
# (infra/forge/longhorn-drain-policy.sh, measured 2026-09-30) — named for
# it, read from scheduler/replica_scheduler.go, and pinned against
# Longhorn's answers by crates/core/boss-testing/tests/longhorn_ledger_sh.rs
# (which carries the expand verb's own fixtures too; backlog 64e00b78 puts
# that verb on this file).
#
# A disk is { max, reserved, scheduled, available } in bytes:
# storageMaximum, storageScheduled and storageAvailable from the node's
# status.diskStatus[<disk>], storageReserved from its spec.disks[<disk>]
# (GetDiskSchedulingInfo). A replica names its disk by UUID, spec.diskID =
# status.diskStatus[<disk>].diskUUID — NOT by the disk's name. Go computes
# int64(float64(x) * float64(pct) / 100); for the non-negative values here
# that is jq's floor.
#
#   ProvisionedLimit = (storageMaximum - storageReserved) x over-provisioning% / 100
#   MinimalAvailable = storageMaximum x minimal-available% / 100
#
#   lh_schedulable($size; $required) — IsSchedulableToDisk(size,
#     requiredStorage): refuses storageMaximum <= 0, storageAvailable <= 0,
#     storageAvailable - requiredStorage <= MinimalAvailable, and
#     size + storageScheduled > ProvisionedLimit.
#   lh_validate_expansion($required) — ValidateDiskAvailableForExpansion:
#     nothing to judge for required <= 0; else refuses storageMaximum <= 0,
#     storageAvailable <= 0, and with physicalUsed = storageMaximum -
#     storageAvailable - storageReserved, physicalAfter = physicalUsed +
#     required: storageMaximum - physicalAfter < MinimalAvailable, and
#     physicalAfter > ProvisionedLimit. storageReserved counts as free here.
#   lh_expansion($required) — what CheckReplicasSizeExpansion asks of ONE
#     replica disk, required = growth x the volume's replicas on that disk:
#     lh_validate_expansion(required), then lh_schedulable(required; 0)
#     ("requiredStorage = 0 is intentional", replica_scheduler.go:1460).
#   lh_placement($size; $actual) — the scheduler's disk filter for a NEW
#     replica (replica_scheduler.go:578): lh_schedulable(spec.size;
#     status.actualSize).
#
# Each returns the reason the disk refuses, in Longhorn's words — the
# FIRST one, as the Go returns at its first failed condition — as a list
# that is empty when it takes the bytes, or one "unreported" reason when a field
# it needs is missing — no evidence is not a fit. storageAvailable and
# actualSize move every second on a busy volume, so a caller judges the
# live terms at every render and signs only that they were judged.
#
# lh_disks($items) turns a nodes.longhorn.io list's items into one object
# per disk: node, name, uuid, type, tags, the ledger fields as numbers
# (null when unreported), the node's and the disk's evictionRequested,
# and the reasons Longhorn's scheduler would skip it — `node_why` for the
# node (allowScheduling, eviction, Ready, Schedulable; a cordoned node
# reads Schedulable False) and `why` for the disk (allowScheduling,
# eviction, its Schedulable condition, no diskStatus). v1beta2 serves
# conditions as a list, v1beta1 as a map; lh_cond reads both.
LONGHORN_LEDGER_JQ='
def lh_conds: (. // []) | if type == "object" then [.[]] else . end;
def lh_cond($t): [lh_conds[] | select(.type == $t) | .status] | .[0] // "unreported";
def lh_num: if . == null then null else (tostring | tonumber? // null) end;
def lh_failed: (.spec.failedAt // "") != "";
def lh_deleting: (.metadata.deletionTimestamp // "") != "";
def lh_inactive: .spec.active != true;
def lh_healthy: (.spec.healthyAt // "") != "" and (lh_failed | not) and (lh_inactive | not) and (lh_deleting | not);
def lh_disks($items):
    [$items[] | . as $N | .metadata.name as $node
     | ([(if .spec.allowScheduling != true then "spec.allowScheduling is false" else empty end),
         (if .spec.evictionRequested == true then "an eviction is requested" else empty end),
         ((.status.conditions | lh_cond("Ready")) as $r | if $r != "True" then "Ready is \($r)" else empty end),
         ((.status.conditions | lh_cond("Schedulable")) as $s | if $s != "True" then "Schedulable is \($s)" else empty end)]) as $nwhy
     | (.spec.disks // {}) | to_entries[] | .key as $d | .value as $ds
     | (($N.status.diskStatus // {})[$d]) as $st
     | {node: $node, name: $d,
        uuid: (if $st == null then null else ($st.diskUUID // null) end),
        type: ($ds.diskType // "filesystem"),
        tags: ($ds.tags // []), node_tags: ($N.spec.tags // []),
        max: (if $st == null then null else ($st.storageMaximum | lh_num) end),
        reserved: ($ds.storageReserved | lh_num),
        scheduled: (if $st == null then null else ($st.storageScheduled | lh_num) end),
        available: (if $st == null then null else ($st.storageAvailable | lh_num) end),
        node_evict: ($N.spec.evictionRequested == true),
        evict: ($ds.evictionRequested == true),
        node_why: $nwhy,
        why: [(if $ds.allowScheduling != true then "allowScheduling is false" else empty end),
              (if $ds.evictionRequested == true then "an eviction is requested" else empty end),
              (if $st == null then "no diskStatus reported"
               else (($st.conditions | lh_cond("Schedulable")) as $s | if $s != "True" then "Schedulable is \($s)" else empty end) end)]}];
def lh_limit($opct): if .max == null or .reserved == null or $opct == null then null
    else (((.max - .reserved) * $opct / 100) | floor) end;
def lh_room($opct): lh_limit($opct) as $l | if $l == null or .scheduled == null then null else $l - .scheduled end;
def lh_floor($mpct): if .max == null or $mpct == null then null else ((.max * $mpct / 100) | floor) end;
def lh_unreported($opct; $mpct):
    .max == null or .reserved == null or .scheduled == null or .available == null or $opct == null or $mpct == null;
def lh_schedulable($size; $required; $opct; $mpct):
    if lh_unreported($opct; $mpct) or $size == null or $required == null
    then ["unreported: a ledger field, a setting or the size is missing (IsSchedulableToDisk)"]
    elif .max <= 0 then ["Storage Max must be greater than 0 (IsSchedulableToDisk)"]
    elif .available <= 0 then ["Storage Available must be greater than 0 (IsSchedulableToDisk)"]
    else lh_floor($mpct) as $floor | lh_limit($opct) as $limit
        | [(if .available - $required <= $floor
            then "Actual space usage condition failed: CurrentAvailable = \(.available - $required) (StorageAvailable \(.available) - Required \($required)) is less than or equal to MinimalAvailable = \($floor) (IsSchedulableToDisk)" else empty end),
           (if $size + .scheduled > $limit
            then "Scheduling space condition failed: ScheduledTotal = \($size + .scheduled) (Size \($size) + StorageScheduled \(.scheduled)) is greater than ProvisionedLimit = \($limit) (IsSchedulableToDisk)" else empty end)]
        | .[0:1]
    end;
def lh_validate_expansion($required; $opct; $mpct):
    if $required == null then ["unreported: the bytes required are missing (ValidateDiskAvailableForExpansion)"]
    elif $required <= 0 then []
    elif lh_unreported($opct; $mpct) then ["unreported: a ledger field or a setting is missing (ValidateDiskAvailableForExpansion)"]
    elif .max <= 0 then ["storage maximum must be greater than 0 (ValidateDiskAvailableForExpansion)"]
    elif .available <= 0 then ["storage available must be greater than 0 (ValidateDiskAvailableForExpansion)"]
    else lh_floor($mpct) as $floor | lh_limit($opct) as $limit
        | (.max - .available - .reserved) as $used | ($used + $required) as $after | (.max - $after) as $left
        | [(if $left < $floor
            then "physical free space would drop below minimal: left=\($left) < minimal=\($floor) (physicalUsed \($used) + required \($required); ValidateDiskAvailableForExpansion)" else empty end),
           (if $after > $limit
            then "expansion exceeds physical over-provisioning limit: usedAfter=\($after) > limit=\($limit) (ValidateDiskAvailableForExpansion)" else empty end)]
        | .[0:1]
    end;
def lh_expansion($required; $opct; $mpct):
    lh_validate_expansion($required; $opct; $mpct) as $v
    | if $v != [] then $v else lh_schedulable($required; 0; $opct; $mpct) end;
def lh_placement($size; $actual; $opct; $mpct): lh_schedulable($size; $actual; $opct; $mpct);
def lh_physical($required; $opct; $mpct):
    if .max == null or .available == null or .reserved == null or $opct == null or $mpct == null
    then ["its live storageAvailable, storageMaximum or storageReserved is unreported"]
    else lh_floor($mpct) as $floor | lh_limit($opct) as $limit
        | (.max - .available - .reserved) as $used
        | ($used + $required) as $after
        # In the order Go checks, stopping at the first failure:
        # ValidateDiskAvailableForExpansion (nil for required <= 0), then
        # the live half of IsSchedulableToDisk with requiredStorage 0.
        | [(if $required <= 0 then empty
            elif .max <= 0 or .available <= 0
            then "storageMaximum \(.max), storageAvailable \(.available): nothing schedules here (ValidateDiskAvailableForExpansion)"
            elif .max - $after < $floor
            then "physical left \(.max) - \($after) = \(.max - $after) is under the floor \($floor) (ValidateDiskAvailableForExpansion: physicalUsed \($used) + \($required))"
            elif $after > $limit
            then "physical used after \($after) is over ProvisionedLimit \($limit) (ValidateDiskAvailableForExpansion: physicalUsed \($used) + \($required))"
            else empty end),
           (if .max <= 0 or .available <= 0
            then "storageMaximum \(.max), storageAvailable \(.available): nothing schedules here (IsSchedulableToDisk)"
            elif .available <= $floor
            then "storageAvailable \(.available) is not above the floor \($floor) (storageMaximum x storage-minimal-available-percentage \($mpct)%; IsSchedulableToDisk, which the growth check calls with requiredStorage 0)"
            else empty end)]
        | .[:1]
    end;
def lh_physical_room($opct; $mpct):
    if .max == null or .available == null or .reserved == null or $opct == null or $mpct == null then null
    else lh_floor($mpct) as $floor
        | if .max <= 0 or .available <= 0 or .available <= $floor then 0
          else ([lh_limit($opct) - (.max - .available - .reserved), .available + .reserved - $floor] | min)
               | if . < 0 then 0 else . end end
    end;
# What read-only DISCOVERY asks of its inputs before it may name an
# argument (a size, a replica): exact non-negative integers jq holds
# without rounding, and identities that resolve ONE way. Written for the
# largest-fit read (backlog 53c8cb72) and moved here, unchanged, when the
# decisive-move read needed the same guard (design ada8f698): $vol the
# volume name, $V the volume, $R the replica items, $N the node items,
# $O and $M the two settings values. Any error inside is CANNOT ANSWER
# to the caller: no evidence is not a proposal.
def lh_uint:
    (type == "number" or (type == "string" and test("^[0-9]+$")))
    and (try (tonumber | . >= 0 and . <= 9007199254740991 and . == floor) catch false);
def lh_safe: type == "number" and . >= 0 and . <= 9007199254740991 and . == floor;
def lh_exact_inputs($vol; $V; $R; $N; $O; $M):
    ($O | lh_uint) and ($M | lh_uint)
    and ($M | tonumber <= 100)
    and ($V.spec.size | lh_uint)
    and all($R[]; (.spec.volumeName | type == "string"))
    and all($R[] | select(.spec.volumeName == $vol);
        (.spec.nodeID | type == "string")
        and (.spec.nodeID == "" or (.spec.diskID | type == "string" and length > 0)))
    and all($N[]; (.status.diskStatus | type == "object"))
    and all($N[]; . as $n
        | all(.status.diskStatus | to_entries[]; . as $D
            | all([$D.value.storageMaximum, $D.value.storageScheduled,
                $D.value.storageAvailable, $n.spec.disks[$D.key].storageReserved][]; lh_uint)))
    and ([ $N[].status.diskStatus[] | .diskUUID ]
        | all(.[]; type == "string" and length > 0)
          and length == (unique | length))
    and all($R[] | select(.spec.volumeName == $vol and .spec.nodeID != "");
        . as $r | [$N[] | select(.metadata.name == $r.spec.nodeID)
            | .status.diskStatus[] | select(.diskUUID == $r.spec.diskID)] | length == 1)
    and (lh_disks($N) | all(.[];
        all([.max, .reserved, .scheduled, .available][]; lh_uint)
        and .reserved <= .max and .available <= .max
        and ((.max - .reserved) * ($O | tonumber) | lh_safe)
        and (.max * ($M | tonumber) | lh_safe)));
'

# The fewest replicas a retirement may leave (retire-volume-replica.sh,
# which reads it as its default and lets BOSS_RETIRE_FLOOR declare another).
# The decisive-move read holds a volume against the same number: above it,
# retiring the replica on the short disk is ALSO admissible by count, and
# which of the two is a person's choice (design ada8f698, option A).
LONGHORN_RETIRE_FLOOR=3

# lh_one_object <file> <Kind> <name> <namespace, or ""> — the file is ONE
# JSON document, an object of that kind and identity. lh_whole_list <file>
# <Kind> <namespace> — ONE complete list (no continue token) of uniquely
# named objects of that kind there. Discovery's read guards, one copy; the
# caller has sourced infra/lib/jq.sh (jq_doc_file).
lh_one_object() {
    jq_doc_file "$1" && jq -e -s --arg kind "$2" --arg name "$3" --arg ns "$4" '
        length == 1 and (.[0] | type == "object"
            and .kind == $kind
            and .metadata.name == $name
            and ($ns == "" or .metadata.namespace == $ns))' "$1" >/dev/null 2>&1
}
lh_whole_list() {
    jq_doc_file "$1" && jq -e -s --arg kind "$2" --arg ns "$3" '
        length == 1 and (.[0] | type == "object" and (.items | type == "array")
            and (.kind == "List" or .kind == ($kind + "List"))
            and ((.metadata.continue // "") == "")
            and all(.items[]; .kind == $kind and .metadata.namespace == $ns
                and (.metadata.name | type == "string" and length > 0))
            and ([.items[].metadata.name] | length == (unique | length)))' "$1" >/dev/null 2>&1
}
