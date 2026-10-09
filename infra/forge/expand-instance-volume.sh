#!/usr/bin/env bash
# expand-instance-volume.sh — grow ONE instance's PersistentVolumeClaim on
# Longhorn, through a rendered plan a passkey signs. The ops verbs
# `plan-an-instance-volume-expansion` (--plan) and `expand-instance-volume`.
#
#   expand-instance-volume.sh --plan <namespace> <pvc> <size>
#   expand-instance-volume.sh --plan-largest <namespace> <pvc>
#   expand-instance-volume.sh <namespace> <pvc> <size> <plan-sha256>
#
# --plan-largest is read-only argument discovery (backlog 53c8cb72): one
# JSON proposal carries the resolved EXPLICIT size and the existing plan.
# It is not approval or execution; a later request freezes those arguments,
# and the ordinary write still requires the hash David's passkey signed.
# When NO growth fits and a replica disk is what binds, it asks
# move-volume-replica.sh --plan-decisive the one further question (design
# ada8f698, option A): would exactly one replica move free the volume? That
# script answers with its own existing plan for that replica, or refuses.
#
# WHY IT EXISTS (backlog ebbb923f, incident d3c0a67c, David 2026-10-01).
# The system of record's database volume, boss/pgdata-postgres-0, filled,
# and the only way to grow it was David's hand `kubectl patch` — which
# must be the last. His first patch, 20Gi -> 40Gi, was REFUSED by
# Longhorn's admission webhook:
#
#   CheckReplicasSizeExpansion for volume pvc-93e11a6e-… cannot schedule
#   21474836480 more bytes to disk bf045701-… (StorageMaximum 117656518656,
#   StorageReserved 35296955596, StorageScheduled 70866960384,
#   OverProvisioningPercentage 100, MinimalAvailablePercentage 25):
#   ScheduledTotal 92341796864 > ProvisionedLimit 82359563060
#
# One replica of the volume sat on a ~110 GiB disk with ~10.7 GiB of
# room in Longhorn's scheduling ledger, and 30Gi was what fit. A verb
# that patched first and read the refusal afterwards would have done the
# same; this one computes the webhook's sum for EVERY replica's disk
# BEFORE it writes, refuses what the webhook would refuse, and names the
# largest size that fits.
#
# THE SHAPE is design 17835005's, as set-volume-replicas' and
# commission-a-disk's: the verb file declares requires_approval and names
# plan-an-instance-volume-expansion as its plan_verb, so the runner
# renders this script's --plan onto the request's approve step and hands
# the write sha256 of the SIGNED plan. The write re-renders and patches
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the three it checks:
#   * the namespace is an INSTANCE — one of infra/cluster/instances.toml's
#     namespaces, derived through render-instance.sh --instances as
#     pod-logs derives it (boss-dev and every system namespace refused);
#   * the claim exists, is Bound, and has no expansion in flight (its
#     request equals its capacity);
#   * its StorageClass is Longhorn's (driver.longhorn.io) and says
#     allowVolumeExpansion: true;
#   * the Longhorn volume behind it is this claim's, its spec.size is the
#     claim's capacity, and its robustness is healthy;
#   * the size is whole GiB, LARGER than the capacity now (grows only —
#     Kubernetes refuses a shrink, and this verb never asks), at most
#     twice it, and at most CEILING_GIB below. Raising the ceiling is a
#     reviewed change to this file;
#   * for EVERY disk holding a replica of the volume, Longhorn's own
#     admission check as v1.11.3 runs it (the version the estate runs,
#     longhorn-drain-policy.sh): the PVC validator calls
#     CheckReplicasSizeExpansion, which runs TWO checks per disk, with
#     required = replicas-on-that-disk x growth and
#     ProvisionedLimit = (storageMaximum - storageReserved)
#                        x storage-over-provisioning-percentage / 100,
#     floor = storageMaximum x storage-minimal-available-percentage / 100:
#
#     1. ValidateDiskAvailableForExpansion — the disk's PHYSICAL space:
#          physicalUsed = storageMaximum - storageAvailable - storageReserved
#          physicalUsed + required <= ProvisionedLimit
#          storageMaximum - (physicalUsed + required) >= floor
#        (at 100% the first is simply required <= storageAvailable);
#     2. IsSchedulableToDisk — the scheduling LEDGER:
#          storageScheduled + required <= ProvisionedLimit
#          storageAvailable > floor
#
#     Adversarial review adc832e2 found the first missing: the ledger
#     admitted a growth the disk could not hold, the plan said "fits",
#     and the signed write would have met the webhook's refusal.
#     storageMaximum, storageAvailable and storageScheduled are the
#     node's diskStatus, storageReserved the node's disk spec, the two
#     percentages Longhorn's settings; a replica names its disk by UUID
#     (spec.diskID = diskStatus.diskUUID), and one with no node yet is
#     skipped, as the webhook skips it.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the claim,
# its class, the volume, the size before and after, every bound, every
# replica, every replica disk's sum, the largest size its ledger admits, the one
# patch and the read-back. No clock and no LIVE free space in the bytes:
# storageAvailable moves every second on a busy disk, and a plan that
# signed it could never be run — it rides stderr, and both checks that
# read it are re-run when the write re-renders. The signed size ceiling
# comes only from the ledgers (backlog 64e00b78): the physical ceiling is
# observed on stderr and can refuse the requested growth, but admissible
# free-space changes do not void an approval. The size before is IN
# the bytes, so a second run of an applied plan re-renders to a refusal:
# at most once.
#
# THE WRITE is one `kubectl patch pvc --type=json` that TESTS
# spec.resources.requests.storage is still the planned value and replaces
# it — a compare-and-set on the one field. The webhook runs its own sum
# again; the CSI resizer grows the Longhorn volume, then the kubelet the
# filesystem.
#
# THE EFFECT IS READ BACK. Every BOSS_EXPAND_POLL_S (default 10) up to
# BOSS_EXPAND_WAIT_S (default 600, inside the verb's 900 s timeout so the
# verdict is this script's, not the runner's kill): the claim's
# status.capacity.storage and conditions, and the Longhorn volume's
# spec.size and robustness. Only when the capacity AND spec.size read the
# new size in bytes is the line the verb file declares as `effect`
# printed. A stall exits 1 saying the claim is NOT proven, naming what it
# read — FileSystemResizePending is the kubelet's half not done yet — and
# the request stands.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=expand-instance-volume
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
# The retire verb already reads these growth checks; the move verb's
# placement checks are a separate interface, never a second growth sum.
# shellcheck source=infra/lib/longhorn-ledger.sh
. "$HERE/../lib/longhorn-ledger.sh"

LH=longhorn-system
# The ceiling one run may grow a claim to. 100 GiB is about the smallest
# replica disk in the estate (StorageMaximum 117656518656 on 2026-10-01):
# a claim past it outgrows a disk, and that is a decision, not a run.
CEILING_GIB=100
GIB=1073741824
LABEL='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
SIZE_RE='^[1-9][0-9]{0,3}Gi$'
VOL_RE='^pvc-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
# A Kubernetes quantity, in bytes; null when it is not one this verb
# reads (a fraction, an exponent). One definition, for the plan and the
# read-back.
JQ_BYTES='def bytes: tostring
    | [capture("^(?<n>[0-9]+)(?<u>Ki|Mi|Gi|Ti|Pi|k|M|G|T|P)?$")]
    | if length == 0 then null
      else .[0] | (.n | tonumber) * ({"": 1, "Ki": 1024, "Mi": 1048576, "Gi": 1073741824,
            "Ti": 1099511627776, "Pi": 1125899906842624, "k": 1000, "M": 1000000,
            "G": 1000000000, "T": 1000000000000, "P": 1000000000000000}[.u // ""]) end;'

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread claim has no plan, and nothing was written"; }

PLAN=0
DISCOVERY=0
if [ "${1-}" = "--plan-largest" ]; then
    PLAN=1
    DISCOVERY=1
    shift
    [ $# -eq 2 ] || refuse "usage: expand-instance-volume.sh --plan-largest <namespace> <pvc> — read-only discovery takes only the claim"
elif [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 3 ] || refuse "usage: expand-instance-volume.sh --plan <namespace> <pvc> <size> — the plan takes the claim and the size and nothing else"
else
    [ $# -eq 4 ] || refuse "usage: expand-instance-volume.sh <namespace> <pvc> <size> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-an-instance-volume-expansion verb)"
    [[ "$4" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$4'"
    APPROVED="$4"
fi
NS="$1"
PVC="$2"
SIZE="${3-}"
[[ "$NS" =~ $LABEL ]] || refuse "the namespace must be one DNS label, got '$NS'"
[[ "$PVC" =~ $LABEL ]] || refuse "the claim must be one DNS label, got '$PVC'"
if [ "$DISCOVERY" -eq 0 ]; then
    [[ "$SIZE" =~ $SIZE_RE ]] || refuse "the size must be whole GiB written <n>Gi (1Gi..9999Gi, no leading zero), got '$SIZE'"
fi
WANT_GIB="${SIZE%Gi}"
# Zero is only an internal facts input before read-only discovery resolves
# a size. It never becomes an argument to the mutation or its signed plan.
[ "$DISCOVERY" -eq 0 ] || WANT_GIB=0
WANT_B=$((WANT_GIB * GIB))
[ "$WANT_GIB" -le "$CEILING_GIB" ] \
    || refuse "$SIZE is over the ceiling ${CEILING_GIB}Gi (CEILING_GIB in infra/forge/expand-instance-volume.sh) — a claim past it outgrows a replica disk, and raising it is a reviewed change to that file"

# --- the namespace, against instances.toml through the renderer ------------
INSTANCES="$("$REPO/infra/cluster/render-instance.sh" --instances)" \
    || unread "infra/cluster/instances.toml would not render (see above), so the namespace bound cannot be read"
ALLOWED="$(printf '%s\n' "$INSTANCES" | cut -f2)"
ok=no
while IFS= read -r ns; do
    [ -n "$ns" ] && [ "$ns" = "$NS" ] && ok=yes
done <<<"$ALLOWED"
[ "$ok" = yes ] || refuse "the namespace '$NS' is not an instance in infra/cluster/instances.toml; this verb grows claims in $(printf '%s' "$ALLOWED" | tr '\n' ' ' | sed 's/ $//; s/ /, /g')"

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the cluster's answer cannot be read, and no evidence is not a plan"

WAIT_S="${BOSS_EXPAND_WAIT_S:-600}"
POLL_S="${BOSS_EXPAND_POLL_S:-10}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_EXPAND_WAIT_S and BOSS_EXPAND_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
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

# read_obj <missing: refuse|unread> <file> <kubectl get args…> — one
# object into $WORK/<file>. NotFound is a refusal for what the request
# named and CANNOT ANSWER for what Longhorn always holds.
read_obj() {
    local missing="$1" file="$2"
    shift 2
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get "$@" -o json --request-timeout=20s > "$WORK/$file" 2> "$WORK/$file.err"; then
        if [ "$missing" = refuse ] && grep -q NotFound "$WORK/$file.err"; then
            refuse "$* is not there ($(tr '\n' ' ' < "$WORK/$file.err"))"
        fi
        unread "$*: $(tr '\n' ' ' < "$WORK/$file.err")"
    fi
    jq_doc_file "$WORK/$file" && jq -e 'type == "object"' "$WORK/$file" >/dev/null 2>&1 \
        || unread "$* did not read back as one object (its first 200 bytes: '$(head -c 200 "$WORK/$file" | tr '\n' ' ')')"
    if [ "$DISCOVERY" -eq 1 ]; then
        local ns="$LH" kind
        case "$1" in
            pvc) kind=PersistentVolumeClaim ;;
            storageclass) kind=StorageClass ;;
            volumes.longhorn.io) kind=Volume ;;
            settings.longhorn.io) kind=Setting ;;
            *) unread "discovery does not know the typed resource $1" ;;
        esac
        [ "$1" != pvc ] || ns="$NS"
        [ "$1" != storageclass ] || ns=""
        lh_one_object "$WORK/$file" "$kind" "$2" "$ns" \
            || unread "$* is not exactly one object with the requested identity"
    fi
}

# read_list <resource> <file> — a Longhorn list into $WORK/<file>.
read_list() {
    # shellcheck disable=SC2086
    if ! $K get "$1" -n "$LH" -o json --request-timeout=30s > "$WORK/$2" 2> "$WORK/$2.err"; then
        unread "$1: $(tr '\n' ' ' < "$WORK/$2.err")"
    fi
    jq_doc_file "$WORK/$2" && jq -e '.items | type == "array"' "$WORK/$2" >/dev/null 2>&1 \
        || unread "$1 did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/$2" | tr '\n' ' ')')"
    if [ "$DISCOVERY" -eq 1 ]; then
        local kind
        case "$1" in
            replicas.longhorn.io) kind=Replica ;;
            nodes.longhorn.io) kind=Node ;;
            *) unread "discovery does not know the typed list $1" ;;
        esac
        lh_whole_list "$WORK/$2" "$kind" "$LH" \
            || unread "$1 is not exactly one complete list with unique named objects"
    fi
}

# Discovery must not choose an argument from ambiguous identities or rounded
# numeric input. This guard is scoped to the new read-only path; generic
# jq_doc_file keeps its NDJSON contract and the existing explicit plan stays
# the signed-plan definition. All arithmetic below remains the shared library's.
discovery_inputs() {
    jq -e -n --arg vol "$VOL" \
        --slurpfile p "$WORK/pvc.json" --slurpfile v "$WORK/vol.json" \
        --slurpfile r "$WORK/replicas.json" --slurpfile n "$WORK/lhnodes.json" \
        --slurpfile o "$WORK/overprov.json" --slurpfile m "$WORK/minavail.json" \
        "$JQ_BYTES$LONGHORN_LEDGER_JQ"'
        all([$p[0].spec.resources.requests.storage, $p[0].status.capacity.storage][];
            type == "string" and (bytes | lh_safe))
        and lh_exact_inputs($vol; $v[0]; $r[0].items; $n[0].items; $o[0].value; $m[0].value)' >/dev/null 2>&1 \
        || unread "largest-fit discovery needs complete unique identities and nonnegative exact integer sizes, disk fields and settings; no proposal can be inferred"
}

# facts — one JSON object judged from the reads: the claim, its class,
# the volume, the settings, and every replica disk's webhook sum for the
# growth asked.
facts() {
    jq -n --arg vol "$VOL" --argjson want "$WANT_B" --argjson gib "$GIB" \
        --slurpfile pvc "$WORK/pvc.json" \
        --slurpfile sc "$WORK/sc.json" \
        --slurpfile v "$WORK/vol.json" \
        --slurpfile reps "$WORK/replicas.json" \
        --slurpfile nodes "$WORK/lhnodes.json" \
        --slurpfile op "$WORK/overprov.json" \
        --slurpfile ma "$WORK/minavail.json" "$JQ_BYTES$LONGHORN_LEDGER_JQ"'
        def num: tostring | tonumber? // null;
        def healthy: (.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "";
        def failed: (.spec.failedAt // "") != "";
        $pvc[0] as $P | $v[0] as $V
        | ($V.spec.size | num) as $size
        | ($want - ($size // 0)) as $grow
        | ($op[0].value | num) as $opct
        | ($ma[0].value | num) as $mpct
        | [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
        | [$mine[] | select((.spec.nodeID // "") != "")] as $placed
        | [$placed | group_by(.spec.diskID // "")[] | . as $g
            | ($g[0].spec.diskID // "") as $uuid
            | ($g[0].spec.nodeID) as $node
            | ([$nodes[0].items[] | select(.metadata.name == $node)] | .[0] // null) as $N
            | ([($N.status.diskStatus // {}) | to_entries[] | select(.value.diskUUID == $uuid)] | .[0] // null) as $ds
            | if $ds == null then {uuid: $uuid, node: $node, known: false}
              else
                ($ds.key) as $dname
                | ($ds.value.storageMaximum | lh_num) as $max
                | (($N.spec.disks // {})[$dname].storageReserved | lh_num) as $res
                | ($ds.value.storageScheduled | lh_num) as $sched
                | ($ds.value.storageAvailable | lh_num) as $avail
                | ($g | length) as $c
                | if $max == null or $res == null or $sched == null or $avail == null or $opct == null or $mpct == null
                  then {uuid: $uuid, node: $node, known: false, name: $dname}
                  else
                    {max: $max, reserved: $res, scheduled: $sched, available: $avail} as $disk
                    | ($disk | lh_limit($opct)) as $limit
                    | ($disk | lh_room($opct)) as $room
                    | ($sched + $c * $grow) as $total
                    | ($disk | lh_floor($mpct)) as $floor
                    # ValidateDiskAvailableForExpansion (v1.11.3), on the
                    # disk itself: live, so judged on stderr at both renders.
                    | ($c * $grow) as $req
                    | ($max - $avail - $res) as $pused
                    | ($pused + $req) as $pafter
                    | ($max - $pafter) as $pleft
                    | ($disk | lh_physical_room($opct; $mpct)) as $proom
                    | (if $proom <= 0 then 0 else ($proom / $c | floor) end) as $pper
                    | ("disk \($uuid) (\($dname) on \($node)): ValidateDiskAvailableForExpansion: "
                       + "physicalUsed = \($max) - \($avail) - \($res) = \($pused); "
                       + "physicalUsed \($pused) + \($c) x \($grow) = \($pafter)") as $phead
                    | {uuid: $uuid, node: $node, name: $dname, known: true, count: $c,
                       limit: $limit, total: $total, room: $room,
                       fits: ($req <= $room),
                       per_replica: (if $room <= 0 then 0 else ($room / $c | floor) end),
                       phys_per_replica: $pper,
                       phys_refused: ($disk | lh_physical($req; $opct; $mpct)
                            | map("disk \($uuid) (\($dname) on \($node)): \(.) — the webhook refuses this growth")),
                       physical: "\($phead) <= \($limit) and leaves \($pleft) >= \($floor); this disk can take \($proom) more bytes, \($pper) per replica",
                       pressure: (($disk | lh_physical(0; $opct; $mpct)) != []),
                       line: ("disk \($uuid) (\($dname) on \($node)): \($c) replica(s) of this volume; "
                              + "ProvisionedLimit (\($max) - \($res)) x \($opct)% = \($limit); "
                              + "ScheduledTotal \($sched) + \($c) x \($grow) = \($total)"
                              + (if $total <= $limit
                                 then " <= \($limit) — fits, leaving \($limit - $total) bytes"
                                 else "; ScheduledTotal \($total) > ProvisionedLimit \($limit) — the webhook refuses this growth" end)),
                       live: "disk \($uuid) (\($dname) on \($node)): storageAvailable \($avail) bytes against the floor \($floor) (storageMaximum \($max) x storage-minimal-available-percentage \($mpct)%)"}
                  end
              end] | sort_by(.uuid) as $disks
        | {phase: ($P.status.phase // "unreported"),
           pvc_name: ($P.metadata.name // ""), pvc_ns: ($P.metadata.namespace // ""),
           request: ($P.spec.resources.requests.storage // ""),
           capacity: ($P.status.capacity.storage // ""),
           request_b: (($P.spec.resources.requests.storage // "") | bytes),
           capacity_b: (($P.status.capacity.storage // "") | bytes),
           sc_name: ($sc[0].metadata.name // ""),
           provisioner: ($sc[0].provisioner // "unreported"),
           expand: (if $sc[0] | has("allowVolumeExpansion") then $sc[0].allowVolumeExpansion else "unset" end),
           size: $size, grow: $grow,
           bound_to: (($V.status.kubernetesStatus // {}) | "\(.namespace // "")/\(.pvcName // "")"),
           state: ($V.status.state // "unreported"),
           robustness: ($V.status.robustness // "unreported"),
           opct: $opct, mpct: $mpct,
           replicas: [$mine[] | "replica \(.metadata.name) on \(.spec.nodeID // "no node") (disk \(.spec.diskID // "none")): \(if failed then "failed" elif healthy then "healthy" elif (.spec.nodeID // "") == "" then "not scheduled — the webhook skips it" else "not yet healthy" end)"],
           disks: $disks,
           unknown: [$disks[] | select(.known | not) | "disk \(.uuid) on \(.node)"],
           pressed: [$disks[] | select(.known and .pressure) | .live],
           refused: ([$disks[] | select(.known) | .phys_refused[]]
                     + [$disks[] | select(.known and (.fits | not)) | .line]),
           physical: [$disks[] | select(.known and .phys_refused == []) | .physical],
           ledger_fit_b: (if [$disks[] | select(.known)] == [] then null
                   else ($size + ([$disks[] | .per_replica] | min)) end),
           phys_fit_b: (if [$disks[] | select(.known)] == [] then null
                   else ($size + ([$disks[] | .phys_per_replica] | min)) end),
           live_fit_b: (if [$disks[] | select(.known)] == [] then null
                   else ($size + ([$disks[] | .per_replica, .phys_per_replica] | min)) end)}' 2> "$WORK/facts.err"
}

# render — read the cluster now, apply the bounds, and write the plan to
# $WORK/plan. Sets CUR_REQ (the request before). Nothing in the bytes is
# a clock or live free space.
render() {
    read_obj refuse pvc.json pvc "$PVC" -n "$NS"
    [ "$(jq -r '"\(.metadata.namespace // "")/\(.metadata.name // "")"' "$WORK/pvc.json")" = "$NS/$PVC" ] \
        || unread "pvc $PVC -n $NS did not read back as that claim: $(head -c 200 "$WORK/pvc.json" | tr '\n' ' ')"
    local sc
    sc="$(jq -r '.spec.storageClassName // ""' "$WORK/pvc.json")"
    VOL="$(jq -r '.spec.volumeName // ""' "$WORK/pvc.json")"
    [ -n "$sc" ] || refuse "$NS/$PVC names no storage class, so nothing says it may grow"
    [[ "$sc" =~ $LABEL ]] || refuse "$NS/$PVC names the storage class '$sc', which is not one DNS label"
    [ -n "$VOL" ] || refuse "$NS/$PVC is bound to no volume (phase $(jq -r '.status.phase // "unreported"' "$WORK/pvc.json")) — only a Bound claim grows"
    [[ "$VOL" =~ $VOL_RE ]] || refuse "$NS/$PVC is bound to '$VOL', which is not the pvc-<uuid> name Longhorn provisions"
    read_obj refuse sc.json storageclass "$sc"
    read_obj refuse vol.json volumes.longhorn.io "$VOL" -n "$LH"
    read_list replicas.longhorn.io replicas.json
    read_list nodes.longhorn.io lhnodes.json
    read_obj unread overprov.json settings.longhorn.io storage-over-provisioning-percentage -n "$LH"
    read_obj unread minavail.json settings.longhorn.io storage-minimal-available-percentage -n "$LH"
    [ "$DISCOVERY" -eq 0 ] || discovery_inputs
    if ! facts > "$WORK/facts.json"; then
        unread "the cluster's answer would not parse: $(tr '\n' ' ' < "$WORK/facts.err")"
    fi
    local f="$WORK/facts.json" phase cap_b req_b size twice fit_b fit_gib grant
    q() { jq -r "$1" "$f"; }
    phase="$(q .phase)"
    [ "$phase" = Bound ] || refuse "$NS/$PVC is $phase, not Bound — only a Bound claim grows"
    CUR_REQ="$(q .request)"
    CUR_CAP="$(q .capacity)"
    req_b="$(q '.request_b // "none"')"
    cap_b="$(q '.capacity_b // "none"')"
    case "$req_b$cap_b" in '' | *[!0-9]*) unread "$NS/$PVC reports a request '$CUR_REQ' and a capacity '$CUR_CAP' this verb cannot read as bytes" ;; esac
    [ "$req_b" = "$cap_b" ] \
        || refuse "$NS/$PVC requests $CUR_REQ and has $CUR_CAP — an expansion is already in flight (or stalled); read its conditions before asking for another: $(jq -c '.status.conditions // []' "$WORK/pvc.json")"
    [ "$(q .provisioner)" = driver.longhorn.io ] \
        || refuse "$NS/$PVC's class $sc is provisioned by $(q .provisioner), not driver.longhorn.io — this verb sums Longhorn's disks and no other"
    [ "$(q .expand)" = true ] \
        || refuse "$NS/$PVC's class $sc says allowVolumeExpansion $(q .expand) — the API server would refuse the patch"
    [ "$(q .bound_to)" = "$NS/$PVC" ] \
        || refuse "Longhorn volume $VOL says it is bound to $(q .bound_to), not $NS/$PVC"
    size="$(q '.size // "none"')"
    case "$size" in '' | *[!0-9]*) unread "Longhorn volume $VOL reports no spec.size ('$size')" ;; esac
    [ "$size" = "$cap_b" ] \
        || refuse "Longhorn volume $VOL has spec.size $size and $NS/$PVC a capacity of $cap_b bytes — the two disagree, so an expansion is half done; read it before asking for another"
    [ "$(q .robustness)" = healthy ] \
        || refuse "Longhorn volume $VOL reads robustness $(q .robustness) (state $(q .state)) — only a healthy volume grows: $(q '.replicas | join("; ")')"
    if [ "$DISCOVERY" -eq 0 ]; then
        [ "$WANT_B" -gt "$cap_b" ] \
            || refuse "$NS/$PVC is $CUR_CAP ($cap_b bytes) and $SIZE would not grow it — this verb grows only"
    fi
    twice=$((2 * cap_b / GIB))
    if [ "$DISCOVERY" -eq 0 ]; then
        [ "$WANT_GIB" -le "$twice" ] \
            || refuse "$SIZE is more than twice $NS/$PVC's $CUR_CAP — one run grows a claim at most 2x, to ${twice}Gi"
    fi
    [ "$(q '.disks | length')" -gt 0 ] || unread "Longhorn volume $VOL has no replica on any disk: $(q '.replicas | join("; ")')"
    [ "$(q '.unknown | length')" -eq 0 ] \
        || unread "no Longhorn node reports the disk of $(q '.unknown | join(", ")') (spec.diskID against status.diskStatus[].diskUUID), or a ledger field or setting is missing — the webhook could not sum it either"
    jq -r --arg me "$ME" '.disks[] | "\($me): live, not signed (re-checked when the write re-renders): \(.live)"' "$f" >&2
    [ "$(q '.pressed | length')" -eq 0 ] \
        || refuse "a replica disk is under Longhorn's storage-minimal-available-percentage floor, so the webhook admits no growth on it at all: $(q '.pressed | join("; ")')"
    if [ "$DISCOVERY" -eq 1 ]; then
        WANT_GIB="$(jq -r --argjson twice "$twice" --argjson ceiling "$CEILING_GIB" \
            '[ (.live_fit_b / 1073741824 | floor), $twice, $ceiling ] | min' "$f")" \
            || unread "largest-fit size could not be read from the disk facts"
        case "$WANT_GIB" in '' | *[!0-9]*) unread "largest-fit size is not a nonnegative whole GiB" ;; esac
        WANT_B=$((WANT_GIB * GIB))
        SIZE="${WANT_GIB}Gi"
        if [ "$WANT_B" -le "$cap_b" ]; then
            # NO GROWTH FITS. A bound no replica's placement decides —
            # the ceiling — refuses here, without asking about a move
            # (design ada8f698: "Growth blocked only by the 100Gi ceiling
            # or another non-placement bound must refuse without movement").
            [ "$((CEILING_GIB * GIB))" -gt "$cap_b" ] \
                || refuse "no whole-GiB growth fits $NS/$PVC at $CUR_CAP: it is at the ceiling ${CEILING_GIB}Gi (CEILING_GIB in infra/forge/expand-instance-volume.sh), and no replica move would change that — no replica is selected or moved by discovery"
            # Otherwise a replica disk is what binds, and the one further
            # question discovery may ask is whether EXACTLY ONE replica
            # move would free the volume (option A, David 2026-10-07).
            # The move script owns that judgement and every bound on it;
            # it reads the cluster again, proposes its existing plan for
            # that one replica, or refuses saying which fact was missing.
            # Its answer and its exit code are this run's.
            echo "$ME: no whole-GiB growth fits $NS/$PVC at $CUR_CAP; largest fit is $SIZE — asking move-volume-replica.sh --plan-decisive whether exactly one replica move would free $VOL to grow; discovery moves nothing" >&2
            bash "$HERE/move-volume-replica.sh" --plan-decisive "$VOL" "$NS" "$PVC"
            exit $?
        fi
        facts > "$f" || unread "the resolved explicit size could not be judged: $(tr '\n' ' ' < "$WORK/facts.err")"
    fi
    jq -r --arg me "$ME" '.physical[] | "\($me): live, not signed (re-checked when the write re-renders): \(.)"' "$f" >&2
    fit_b="$(q '.ledger_fit_b')"
    fit_gib=$((fit_b / GIB))
    live_fit_b="$(q '.live_fit_b')"
    live_fit_gib=$((live_fit_b / GIB))
    echo "$ME: live, not signed: the largest size the replica disks' ledgers admit is ${fit_gib}Gi, the largest their physical space admits is $(q '.phys_fit_b / 1073741824 | floor')Gi; together they admit ${live_fit_gib}Gi now; the plan signs the ledger ceiling only" >&2
    if [ "$(q '.refused | length')" -gt 0 ]; then
        refuse "Longhorn's admission webhook would refuse $SIZE: $(q '.refused | join("; ")') — so the largest size that fits every replica's disk is ${live_fit_gib}Gi$( [ "$live_fit_gib" -le "$((cap_b / GIB))" ] && echo ", which is no growth at all: a replica must move off that disk first")"
    fi
    grant="$fit_gib"
    [ "$twice" -lt "$grant" ] && grant="$twice"
    [ "$CEILING_GIB" -lt "$grant" ] && grant="$CEILING_GIB"
    {
        echo "plan: expand-instance-volume"
        echo "claim: $NS/$PVC"
        echo "storage class: $sc ($(q .provisioner), allowVolumeExpansion true)"
        echo "volume: $VOL"
        echo "state: $(q .state)"
        echo "robustness: $(q .robustness)"
        echo "size: $CUR_CAP ($cap_b bytes) -> $SIZE ($WANT_B bytes), +$((WANT_B - cap_b)) bytes"
        echo "bounds: grows only; at most 2x the size now (${twice}Gi); the ceiling ${CEILING_GIB}Gi (CEILING_GIB in infra/forge/expand-instance-volume.sh)"
        echo "longhorn settings: storage-over-provisioning-percentage $(q .opct), storage-minimal-available-percentage $(q .mpct) (each disk's live storageAvailable is held to it on stderr, not signed)"
        echo "the largest size the replica disks' ledgers admit is ${fit_gib}Gi; physical space is re-checked before any write"
        echo "the largest size one run grants is ${grant}Gi"
        echo "command: kubectl patch pvc $PVC -n $NS --type=json — test that spec.resources.requests.storage is still $CUR_REQ, replace it with $SIZE; the one field this writes. Longhorn's admission webhook sums every replica's disk again; the CSI resizer grows the volume, then the kubelet the filesystem."
        echo "read-back: until status.capacity.storage reads $WANT_B bytes and Longhorn volume $VOL reads spec.size $WANT_B."
        echo
        echo "== replicas of $VOL ($(q '.replicas | length')) =="
        q '.replicas[]'
        echo
        echo "== each replica disk, summed as Longhorn's admission webhook sums it (CheckReplicasSizeExpansion) =="
        q '.disks[] | .line'
    } > "$WORK/plan"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    if [ "$DISCOVERY" -eq 1 ]; then
        jq -n --arg ns "$NS" --arg pvc "$PVC" --arg size "$SIZE" --arg hash "$HASH" \
            --rawfile plan "$WORK/plan" \
            '{verb:"plan-an-instance-volume-expansion", args:[$ns,$pvc,$size], plan:$plan, plan_sha256:$hash}' \
            || fail "the proposal could not be encoded — nothing was written"
    else
        cat "$WORK/plan"
    fi
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $NS/$PVC (above) hashes to $HASH, not the approved plan $APPROVED — its size, its volume, its replicas or a disk's ledger moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — growing $NS/$PVC from $CUR_REQ to $SIZE"

PATCH="$(jq -cn --arg cur "$CUR_REQ" --arg new "$SIZE" \
    '[{op: "test", path: "/spec/resources/requests/storage", value: $cur},
      {op: "replace", path: "/spec/resources/requests/storage", value: $new}]')" \
    || fail "could not build the patch — nothing was written"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch pvc "$PVC" -n "$NS" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch pvc $PVC -n $NS did not succeed (its words above) — this run cannot say the claim grew; read it again with plan-an-instance-volume-expansion before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: the API server accepted spec.resources.requests.storage=$SIZE for $NS/$PVC — reading it back (up to ${WAIT_S}s: status.capacity.storage, and Longhorn's spec.size)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
while :; do
    cap="not read"
    cap_b=""
    req="not read"
    conds="not read"
    lsize="not read"
    rob="not read"
    # shellcheck disable=SC2086
    if $K get pvc "$PVC" -n "$NS" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        cap="$(jq -r '.status.capacity.storage // "unreported"' "$WORK/back.json")"
        cap_b="$(jq -r "$JQ_BYTES"' (.status.capacity.storage // "") | bytes // ""' "$WORK/back.json" 2>/dev/null)"
        req="$(jq -r '.spec.resources.requests.storage // "unreported"' "$WORK/back.json")"
        conds="$(jq -r '[.status.conditions // [] | .[] | "\(.type)=\(.status)\(if (.message // "") != "" then " (\(.message))" else "" end)"] | if . == [] then "none" else join("; ") end' "$WORK/back.json")"
    else
        cap="unread: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    # shellcheck disable=SC2086
    if $K get volumes.longhorn.io "$VOL" -n "$LH" -o json --request-timeout=20s > "$WORK/vback.json" 2> "$WORK/vback.err" \
        && jq_doc_file "$WORK/vback.json"; then
        lsize="$(jq -r '.spec.size // "unreported"' "$WORK/vback.json")"
        rob="$(jq -r '.status.robustness // "unreported"' "$WORK/vback.json")"
    else
        lsize="unread: $(tr '\n' ' ' < "$WORK/vback.err" | cut -c1-200)"
        rob="unread"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$cap_b" = "$WANT_B" ] && [ "$lsize" = "$WANT_B" ]; then
        echo "$ME: $NS/$PVC is ${WANT_GIB}Gi — read back: status.capacity.storage $cap ($cap_b bytes), Longhorn volume $VOL spec.size $lsize, robustness $rob (after ${elapsed}s)"
        exit 0
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        fail "the API server accepted spec.resources.requests.storage=$SIZE for $NS/$PVC, and after ${elapsed}s it is NOT proven: status.capacity.storage $cap, spec.resources.requests.storage $req, conditions $conds; Longhorn volume $VOL spec.size $lsize, robustness $rob. The request stands — FileSystemResizePending is the kubelet's half not done yet; read it again with plan-an-instance-volume-expansion, whose refusal names a stalled expansion"
    fi
    sleep "$POLL_S"
done
