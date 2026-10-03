#!/usr/bin/env bash
# retire-volume-replica.sh — retire ONE replica of ONE Longhorn volume,
# lowering its count by one (or resuming after that lower), through a new
# rendered plan a passkey signs. The
# ops verbs `plan-a-volume-replica-retirement` (--plan) and
# `retire-volume-replica`.
#
#   retire-volume-replica.sh --plan <volume> <replica>
#   retire-volume-replica.sh <volume> <replica> <plan-sha256>
#
# WHY IT EXISTS (backlog 5ef9d2d9, incident d3c0a67c, 2026-10-01). The
# system of record's database volume, boss/pgdata-postgres-0 (Longhorn
# volume pvc-93e11a6e-…, 30 GiB), holds four replicas, on cp-1, cp-2, cp-3
# and w-2 (read through plan-a-volume-replica-change, ops-request
# 5b61b067). The w-2 replica, r-ae04deab, sits on disk bf045701-… with
# ~755 MB left in Longhorn's scheduling ledger, so the admission webhook
# refuses every growth of the volume while it stands. David's decision:
# retire that replica and run at three, on the control planes, rather
# than move it. No verb lowered a replica count — set-volume-replicas
# only raises — and the hand path is a patch and a delete in an order
# that matters. This verb is that order, bounded. It is also the one door
# back down after a move that raised a count and stalled
# (move-volume-replica follow-up 2; review run 091904d3, N2).
#
# LOWERING A COUNT REMOVES NOTHING BY ITSELF. Longhorn v1.11.3 trims a
# replica on a lowered count only through cleanupEvictionRequestedReplicas,
# cleanupDataLocalityReplicas (data locality best-effort) or
# cleanupAutoBalancedReplicas (replica auto-balance not disabled) —
# controller/volume_controller.go:1129-1157, read by review run 091904d3
# (finding B2) — and each picks a replica this plan could not name:
# best-effort the smallest non-local name, auto-balance effectively at
# random. So the plan REFUSES all three, and the write DELETES the named
# replica itself. Under the settings this verb admits, its delete is the
# only removal; anything else that goes is a foreign act, and the write
# stops on it.
#
# THE SHAPE is design 17835005's, as set-volume-replicas' and
# move-volume-replica's: the verb file declares requires_approval and
# names plan-a-volume-replica-retirement as its plan_verb, so the runner
# renders this script's --plan onto the request's approve step and hands
# the write sha256 of the SIGNED plan. The write re-renders and changes
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * the volume is one pvc-<uuid> Longhorn holds in longhorn-system, and
#     the replica is one of ITS replicas, named <volume>-r-<8 hex>;
#   * the volume is attached, holds exactly spec.numberOfReplicas = N
#     replicas, none being deleted, and every replica BUT the named one is
#     healthy as Longhorn counts it (isHealthyAndActiveReplica: healthyAt
#     set, failedAt empty, spec.active true), on N-1 distinct
#     nodes. Then one of two:
#       - RETIRE: the named replica is healthy too, on an N-th node, and
#         robustness reads healthy. This costs a healthy copy;
#       - BACK DOWN: the named replica is the ONE not healthy (rebuilding
#         or failed — what a stalled move leaves) and robustness reads
#         degraded. It may rebuild healthy before the delete, which then
#         costs that healthy copy; the remaining N-1 must stay healthy;
#       - RESUME (077adeb3): the count is already N-1, with exactly N
#         healthy+active replicas on N distinct nodes, robustness healthy,
#         and the named one present. A NEW plan signs this state; no old
#         approval continues it. Step 1 tests N-1 without lowering again;
#   * N - 1 is at least the FLOOR: 3 unless BOSS_RETIRE_FLOOR declares
#     another, and never below 2 — a floor under 2 is refused before any
#     read, so no plan this verb renders leaves a volume one failure from
#     losing its data. The floor is in the signed bytes;
#   * Longhorn will remove nothing by itself (above): data locality
#     disabled, replica auto-balance disabled (spec.replicaAutoBalance, or
#     the replica-auto-balance setting when the volume says `ignored`),
#     and no eviction requested on any replica of the volume, its node or
#     its disk;
#   * when it RETIRES a healthy copy, every remaining replica's disk has
#     room for the volume's next growth — one GiB per replica on it, the
#     unit expand-instance-volume grants — in the scheduling ledger AND in
#     live free space, by Longhorn v1.11.3's own growth check
#     (CheckReplicasSizeExpansion: ValidateDiskAvailableForExpansion, and
#     IsSchedulableToDisk with requiredStorage 0). A retirement costs a
#     copy; it is paid for growth, and one after which the volume still
#     could not grow by a GiB has bought nothing. The arithmetic is
#     infra/lib/longhorn-ledger.sh's, the one copy. storageAvailable is
#     live, so the physical half is judged at every render and refuses
#     there, and the signed bytes carry only that it was judged. BACKING
#     DOWN is not held to it: the full disk a stalled move was leaving is
#     among the remaining ones, and that is why the door must open;
#   * every remaining replica's disk is one a node reports (spec.diskID
#     against diskStatus[].diskUUID), or the read cannot answer.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`, since a hash
# cannot be inside what it hashes) names the volume, its size, the three
# settings that would let Longhorn trim, the replica to retire and its
# disk's ledger, the count's path N -> N-1 and the floor, the replicas
# that remain and the growth their disks' ledgers admit, every step, and
# every replica. No clock and no LIVE free space in the bytes. The count
# and the replicas before are IN the bytes, so a second run of an applied
# plan re-renders to a refusal: at most once.
#
# THE WRITE, in this order, each step only when the one before read back:
#   1. one `kubectl patch volumes.longhorn.io --type=json` that TESTS
#      spec.numberOfReplicas is still N and replaces it with N-1 — the
#      compare-and-set set-volume-replicas makes;
#   2. for BOSS_RETIRE_SETTLE_S (default 30) watch the replicas. If the
#      named replica goes by anything else — gone, or still listed with
#      its deletionTimestamp set, as a replica held by its finalizer reads
#      while it is torn down — there is nothing to delete. If ANY OTHER
#      replica goes (either way), a new one appears, or another stops
#      reading healthy, the named replica is NOT deleted and nothing more
#      is: the run exits 1 naming it. Otherwise, straight off the last
#      read, the count, trim settings and robustness must still hold,
#      then `kubectl delete replicas.longhorn.io <replica>` BY NAME.
#      LOWER FIRST, THEN DELETE: Longhorn v1.11.3's replenish count
#      (getReplenishReplicasCount) counts a replica still terminating, so
#      a delete under a count of N would build a replacement the moment
#      its finalizer clears, onto a node holding none — the node just
#      vacated. Lowered first, N replicas stand against N-1, and the
#      COUNT never drops below N-1. The healthy count can, in one way
#      kubectl cannot close: an original replica failing in the instant
#      between the last read and the delete. Longhorn then rebuilds from
#      the rest, and the read-back below names it;
#   3. read back (BOSS_RETIRE_READBACK_S, default 180): the named replica
#      gone — a terminating one is waited for, never counted gone —
#      spec.numberOfReplicas N-1, exactly the planned replicas but the
#      named one, each healthy, on N-1 distinct nodes, robustness healthy,
#      and, for a RETIRE, every remaining disk still with room for the
#      next GiB. Only then is the line the verb file declares as `effect`
#      printed.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan; a read that fails between steps stops the
# run before the next mutation. Exit 78 is a refusal (the request was
# wrong, or is no longer true).
set -uo pipefail

ME=retire-volume-replica
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
# LONGHORN_LEDGER_JQ: the webhook's disk arithmetic, one copy.
# shellcheck source=infra/lib/longhorn-ledger.sh
. "$HERE/../lib/longhorn-ledger.sh"

LH=longhorn-system
GIB=1073741824
UUID_RE='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
VOL_RE="^pvc-${UUID_RE}\$"
REP_RE="^pvc-${UUID_RE}-r-[0-9a-f]{8}\$"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread volume has no plan, and nothing was written"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || refuse "usage: retire-volume-replica.sh --plan <volume> <replica> — the plan takes the volume and the replica and nothing else"
else
    [ $# -eq 3 ] || refuse "usage: retire-volume-replica.sh <volume> <replica> <plan-sha256> — this retires only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-volume-replica-retirement verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
VOL="$1"
REP="$2"
[[ "$VOL" =~ $VOL_RE ]] || refuse "the volume must be one Longhorn volume name, pvc-<uuid> in lower case, got '$VOL'"
[[ "$REP" =~ $REP_RE ]] || refuse "the replica must be one Longhorn replica name, <volume>-r-<8 hex> in lower case, got '$REP'"
[ "${REP%-r-*}" = "$VOL" ] || refuse "the replica $REP is named for volume ${REP%-r-*}, not $VOL"

# The floor: unset is 3; set, it must be a whole number of 2 or more. An
# empty value is refused rather than read as the default — a floor someone
# meant to declare and did not is not a floor of 3.
FLOOR="${BOSS_RETIRE_FLOOR-3}"
case "${FLOOR:-empty}" in
    *[!0-9]*) refuse "BOSS_RETIRE_FLOOR must be a whole number of replicas, never below 2, got '$FLOOR'" ;;
esac
[ "$FLOOR" -ge 2 ] || refuse "BOSS_RETIRE_FLOOR is $FLOOR — a retirement leaves at least 2 replicas, never below 2"
FLOOR=$((10#$FLOOR))

command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not a plan"

SETTLE_S="${BOSS_RETIRE_SETTLE_S:-30}"
READBACK_S="${BOSS_RETIRE_READBACK_S:-180}"
POLL_S="${BOSS_RETIRE_POLL_S:-15}"
case "${SETTLE_S:-empty}${READBACK_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_RETIRE_SETTLE_S, BOSS_RETIRE_READBACK_S and BOSS_RETIRE_POLL_S must be whole seconds, got '$SETTLE_S', '$READBACK_S' and '$POLL_S'" ;;
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

# A Kubernetes response is ONE typed document for the requested subject.
# A different volume's safe-looking fields cannot authorize this deletion
# or prove its readback (077adeb3, review f486f963). jq_doc_file also serves
# NDJSON readers, so its shared at-least-one-document contract stays intact.
typed_doc_file() {
    jq_doc_file "$1" && jq -e -s --arg shape "$2" --arg vol "$VOL" '
        length == 1 and (.[0] | type == "object" and
            if $shape == "list" then
                (.items | type == "array" and all(.[]; type == "object"))
            elif $shape == "volume" then
                .metadata.name == $vol and
                (.spec | type == "object") and (.status | type == "object")
            elif $shape == "setting" then (.value | type == "string")
            else true end)
    ' "$1" >/dev/null
}

# read_obj <missing: refuse|unread> <file> <kubectl get args…> — one
# object into $WORK/<file>. NotFound is a refusal for what the request
# named and CANNOT ANSWER for what Longhorn always holds.
read_obj() {
    local missing="$1" file="$2" shape=object
    shift 2
    case "$1" in volumes.longhorn.io) shape=volume;; settings.longhorn.io) shape=setting;; esac
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get "$@" -o json --request-timeout=20s > "$WORK/$file" 2> "$WORK/$file.err"; then
        if [ "$missing" = refuse ] && grep -q NotFound "$WORK/$file.err"; then
            refuse "$* is not there ($(tr '\n' ' ' < "$WORK/$file.err"))"
        fi
        unread "$*: $(tr '\n' ' ' < "$WORK/$file.err")"
    fi
    typed_doc_file "$WORK/$file" "$shape" 2>> "$WORK/$file.err" \
        || unread "$* did not read back as one object (its first 200 bytes: '$(head -c 200 "$WORK/$file" | tr '\n' ' ')')"
}

# read_list <resource> <file> — a Longhorn list into $WORK/<file>.
read_list() {
    # shellcheck disable=SC2086
    if ! $K get "$1" -n "$LH" -o json --request-timeout=30s > "$WORK/$2" 2> "$WORK/$2.err"; then
        unread "$1: $(tr '\n' ' ' < "$WORK/$2.err")"
    fi
    typed_doc_file "$WORK/$2" list 2>> "$WORK/$2.err" \
        || unread "$1 did not read back as a list (its first 200 bytes: '$(head -c 200 "$WORK/$2" | tr '\n' ' ')')"
}

# Shared jq: a replica's state in words, and the remaining disks' growth
# check — the plan and the read-back judge room by the one definition.
STATE_JQ='
def state_of: if lh_deleting then "being deleted" elif lh_failed then "failed"
              elif lh_healthy then "healthy"
              elif (.spec.healthyAt // "") != "" and lh_inactive then "healthy but not active (spec.active is not true, so Longhorn does not count it)"
              else "not yet healthy (rebuilding, or never finished)" end;
def disk_of($all): (.spec.diskID // "") as $u | [$all[] | select(.uuid == $u)] | .[0] // null;
def ledger_line($opct): "ProvisionedLimit (\(.max) - \(.reserved)) x \($opct)% = \(.limit); storageScheduled \(.scheduled); room \(.room) bytes";
# room_of($all; $opct; $mpct; $grow): each replica in the input list, its
# disk, the ledger and live verdicts on a growth of $grow bytes.
def room_of($all; $opct; $mpct; $grow):
    [.[] | {node: (.spec.nodeID // "no node"), uuid: (.spec.diskID // "none"), d: disk_of($all)}
     | . + {known: (.d != null and .d.room != null)}
     | if .known then . + {phys: (.d | lh_physical($grow; $opct; $mpct)),
                           live_fit: (.d | lh_physical_room($opct; $mpct))}
       else . end];
# The same trim predicates at plan time and immediately before deletion.
def trim_state($V; $mine; $nodes; $all; $abset):
    ($V.spec.replicaAutoBalance // "ignored") as $abvol
    | {locality: ($V.spec.dataLocality // "unreported"),
       autobalance: (if $abvol == "ignored" or $abvol == "" then $abset else $abvol end),
       autobalance_why: "the volume says \($abvol), the setting replica-auto-balance says \($abset)",
       evicting: ([$mine[] | . as $r
         | ([$nodes[] | select(.metadata.name == ($r.spec.nodeID // ""))] | .[0] // null) as $N
         | (if $r.spec.evictionRequested == true then "replica \($r.metadata.name)" else empty end),
           (if $N != null and $N.spec.evictionRequested == true then "node \($N.metadata.name)" else empty end),
           ($all[] | select(.uuid == ($r.spec.diskID // "")) | . as $d
            | select(($N.spec.disks // {})[$d.name].evictionRequested == true) | "disk \($d.uuid) on \($d.node)")]
         | unique)};
'

# facts — one JSON object judged from the reads: the volume, its
# replicas, the one to retire, the settings that would let Longhorn trim,
# and the ledger of every remaining replica's disk.
facts() {
    jq -n --arg vol "$VOL" --arg rep "$REP" --argjson grow "$GIB" \
        --slurpfile v "$WORK/vol.json" \
        --slurpfile reps "$WORK/replicas.json" \
        --slurpfile nodes "$WORK/lhnodes.json" \
        --slurpfile op "$WORK/overprov.json" \
        --slurpfile ma "$WORK/minavail.json" \
        --slurpfile ab "$WORK/autobalance.json" "$LONGHORN_LEDGER_JQ$STATE_JQ"'
        def gibs: if . % 1073741824 == 0 then "\(. / 1073741824) GiB" else "~\(. / 1073741824 | floor) GiB" end;
        $v[0] as $V
        | ($V.spec.size | lh_num) as $size
        | if $size == null then {size: null} else (
          ($op[0].value | lh_num) as $opct
        | ($ma[0].value | lh_num) as $mpct
        | [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
        | lh_disks($nodes[0].items) as $all
        | [$all[] | . + {room: lh_room($opct), limit: lh_limit($opct)}] as $all
        | ([$mine[] | select(.metadata.name == $rep)] | .[0] // null) as $R
        | [$mine[] | select(.metadata.name != $rep)] as $keep
        | trim_state($V; $mine; $nodes[0].items; $all; ($ab[0].value // "unreported" | tostring)) as $trim
        | ($keep | room_of($all; $opct; $mpct; $grow)) as $kd
        | [$kd[] | select(.known) | . + {line: "disk \(.uuid) (\(.d.name) on \(.d.node)): \(.d | ledger_line($opct)) — \(if .d.room >= $grow
                          then "admits growth of \(.d.room) bytes"
                          else "SHORT: admits growth of \(.d.room) bytes, short of \($grow)" end)"}]
            | sort_by(.node) as $known
        | (if $R == null then null else ($R | disk_of($all)) end) as $rd
        | {size: $size, opct: $opct, mpct: $mpct,
           size_gib: ($size | gibs),
           current: ($V.spec.numberOfReplicas // null),
           claim: (($V.status.kubernetesStatus // {}) as $k
                   | if ($k.pvcName // "") != "" then "\($k.namespace)/\($k.pvcName)" else "none" end),
           state: ($V.status.state // "unreported"),
           on: ($V.status.currentNodeID // ""),
           robustness: ($V.status.robustness // "unreported"),
           engine: ($V.spec.dataEngine // "v1"),
           locality: $trim.locality,
           autobalance: $trim.autobalance,
           autobalance_why: $trim.autobalance_why,
           evicting: $trim.evicting,
           n: ($mine | length),
           assigned: all($mine[]; . as $r
                         | (.spec.nodeID | type) == "string" and (.spec.nodeID | length) > 0
                           and (.spec.diskID | type) == "string" and (.spec.diskID | length) > 0
                           and any($all[]; .node == $r.spec.nodeID and .uuid == $r.spec.diskID)),
           nhealthy: ([$mine[] | select(lh_healthy)] | length),
           distinct: ([$mine[] | select(lh_healthy) | .spec.nodeID] | unique | length),
           deleting: [$mine[] | select(lh_deleting) | .metadata.name],
           keep_healthy: ([$keep[] | select(lh_healthy)] | length),
           keep_distinct: ([$keep[] | select(lh_healthy) | .spec.nodeID] | unique | length),
           names: [$mine[] | .metadata.name],
           keep: [$keep[] | .metadata.name],
           keep_placements: [$keep[] | {name: .metadata.name, node: .spec.nodeID, disk: .spec.diskID}],
           keep_nodes: ([$keep[] | .spec.nodeID // "no node"] | sort),
           replicas: [$mine[] | "replica \(.metadata.name) on \(.spec.nodeID // "no node") (disk \(.spec.diskID // "none")): \(state_of)\(if .metadata.name == $rep then " — TO RETIRE" else "" end)"],
           rep: (if $R == null then null
                 else {node: ($R.spec.nodeID // "no node"), disk: ($R.spec.diskID // "none"),
                       healthy: ($R | lh_healthy), state: ($R | state_of)} end),
           rep_line: (if $R == null then null
                      elif $rd == null or $rd.room == null then "disk \($R.spec.diskID // "none") on \($R.spec.nodeID // "no node"): the retired replica'"'"'s disk — no node reports its ledger"
                      else "disk \($R.spec.diskID) (\($rd.name) on \($rd.node)): the retired replica'"'"'s disk — \($rd | ledger_line($opct)); the retirement frees \($size) bytes of it" end),
           unknown: [$kd[] | select(.known | not) | "disk \(.uuid) on \(.node)"],
           disk_lines: [$known[] | .line],
           ledger_short: [$known[] | select(.d.room < $grow) | .line],
           phys_short: [$known[] | select(.phys != []) | "disk \(.uuid) (\(.d.name) on \(.d.node)): \(.phys | join("; "))"],
           after_fit: (if $known == [] then null else ([$known[] | .d.room] | min) + $size end),
           after_fit_gib: (if $known == [] then null else (([$known[] | .d.room] | min) + $size | gibs) end),
           live: [$known[] | "disk \(.uuid) (\(.d.name) on \(.d.node)): storageAvailable \(if .d.available == null then "unreported" else "\(.d.available) bytes" end) against the floor \((.d | lh_floor($mpct)) // "unknown"); by live free space it takes growth of \(.live_fit // "unknown") bytes"]}) end' 2> "$WORK/facts.err"
}

# render — read Longhorn now, apply the bounds, and write the plan to
# $WORK/plan. Sets N (the count before), DOWN, SIZE and MODE. Nothing in
# the bytes is a clock or live free space.
render() {
    read_obj refuse vol.json volumes.longhorn.io "$VOL" -n "$LH"
    [ "$(jq -r '.metadata.name // ""' "$WORK/vol.json")" = "$VOL" ] \
        || unread "volumes.longhorn.io $VOL did not read back as that volume: $(head -c 200 "$WORK/vol.json" | tr '\n' ' ')"
    read_list replicas.longhorn.io replicas.json
    read_list nodes.longhorn.io lhnodes.json
    read_obj unread overprov.json settings.longhorn.io storage-over-provisioning-percentage -n "$LH"
    read_obj unread minavail.json settings.longhorn.io storage-minimal-available-percentage -n "$LH"
    read_obj unread autobalance.json settings.longhorn.io replica-auto-balance -n "$LH"
    if ! facts > "$WORK/facts.json"; then
        unread "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/facts.err")"
    fi
    local f="$WORK/facts.json" n_reps nh dist rob trims=()
    q() { jq -r "$1" "$f"; }
    SIZE="$(q '.size // "none"')"
    N="$(q '.current // "none"')"
    DECLARED="$N"
    OPCT="$(q '.opct // "none"')"
    MPCT="$(q '.mpct // "none"')"
    case "$SIZE" in '' | *[!0-9]*) unread "volume $VOL reports no spec.size ('$SIZE')" ;; esac
    case "$N" in '' | *[!0-9]*) unread "volume $VOL reports no spec.numberOfReplicas ('$N')" ;; esac
    case "$OPCT$MPCT" in '' | *[!0-9]*) unread "Longhorn's storage-over-provisioning-percentage or storage-minimal-available-percentage did not read as a number" ;; esac
    [ "$(q '.rep != null')" = true ] \
        || refuse "$REP is not a replica of $VOL — its replicas are: $(q '.replicas | join("; ")')"
    [ "$(q .state)" = attached ] \
        || refuse "$VOL is $(q .state), not attached — only an attached volume reports the health this retirement stands on"
    n_reps="$(q .n)"
    nh="$(q .nhealthy)"
    dist="$(q .distinct)"
    rob="$(q .robustness)"
    RESUME=0
    if [ "$n_reps" != "$N" ]; then
        if [ "$n_reps" = "$((N + 1))" ] && [ "$nh" = "$n_reps" ] \
            && [ "$dist" = "$n_reps" ] && [ "$rob" = healthy ] && [ "$(q .assigned)" = true ]; then
            # The only extra-count state this door admits: lower already
            # happened, every copy still stands healthy, and NEW bytes
            # require a new passkey approval. N remains the replica count.
            RESUME=1
            N="$n_reps"
        else
            refuse "$VOL declares spec.numberOfReplicas $N and has $n_reps replica(s), $nh healthy on $dist distinct node(s) — a count its replicas do not match is no count to lower; resuming requires exactly one extra healthy replica on a distinct node, robustness healthy: $(q '.replicas | join("; ")')"
        fi
    fi
    [ "$(q '.deleting | length')" -eq 0 ] \
        || refuse "$(q '.deleting | join(", ")') is being deleted already — a volume mid-removal is no volume to remove from: $(q '.replicas | join("; ")')"
    if [ "$(q .keep_healthy)" != "$((N - 1))" ] || [ "$(q .keep_distinct)" != "$((N - 1))" ]; then
        refuse "$VOL declares spec.numberOfReplicas $N and has $n_reps replica(s), $nh healthy on $dist distinct node(s) — every replica but the one retired must be healthy, one per node, or retiring it leaves fewer healthy than the count: $(q '.replicas | join("; ")')"
    fi
    if [ "$(q .rep.healthy)" = true ]; then
        MODE=retire
        [ "$nh" = "$N" ] && [ "$dist" = "$N" ] \
            || refuse "$VOL declares spec.numberOfReplicas $N and has $n_reps replica(s), $nh healthy on $dist distinct node(s) — two share a node: $(q '.replicas | join("; ")')"
        [ "$rob" = healthy ] \
            || refuse "$VOL reads robustness $rob with every replica healthy — only a healthy volume retires a healthy copy: $(q '.replicas | join("; ")')"
    else
        MODE=back-down
        [ "$rob" = degraded ] \
            || refuse "$VOL reads robustness $rob — a volume backs down from its one unhealthy replica only when it reads degraded: $(q '.replicas | join("; ")')"
    fi
    DOWN=$((N - 1))
    [ "$DOWN" -ge "$FLOOR" ] \
        || refuse "$VOL holds $N replicas, and $N -> $DOWN is under the floor of $FLOOR (BOSS_RETIRE_FLOOR, default 3, never below 2) — nothing is retired"
    case "$(q .locality)" in
        disabled) ;;
        *) trims+=("data locality $(q .locality) (Longhorn v1.11.3 trims a non-local replica itself on a lowered count, cleanupDataLocalityReplicas, and a strict-local volume keeps its replica on its node)") ;;
    esac
    [ "$(q .autobalance)" = disabled ] \
        || trims+=("replica auto-balance $(q .autobalance) ($(q .autobalance_why)) — cleanupAutoBalancedReplicas picks the replica itself")
    [ "$(q '.evicting | length')" -eq 0 ] \
        || trims+=("an eviction requested on $(q '.evicting | join(", ")') (cleanupEvictionRequestedReplicas removes the evicting replica, and a replacement is built)")
    [ "${#trims[@]}" -eq 0 ] \
        || refuse "Longhorn may remove a replica of $VOL by itself when its count is lowered, and then not the one this plan names: $(printf '%s; ' "${trims[@]}")only a volume with data locality disabled, replica auto-balance disabled and no eviction requested retires a replica by this verb"
    [ "$(q '.unknown | length')" -eq 0 ] \
        || unread "no Longhorn node reports the disk of $(q '.unknown | join(", ")') (spec.diskID against status.diskStatus[].diskUUID), or its ledger is missing — the webhook could not sum it either"
    jq -r --arg me "$ME" '.live[] | "\($me): live free space, not signed (Longhorn checks it when the volume grows): \(.)"' "$f" >&2
    if [ "$MODE" = retire ]; then
        [ "$(q '.ledger_short | length')" -eq 0 ] \
            || refuse "a remaining replica's disk has no ledger room for the volume's next growth ($GIB bytes, the GiB expand-instance-volume grants in), so retiring $REP would cost a copy and free the volume for nothing: $(q '.ledger_short | join("; ")')"
        # The live half, judged now and again when the write re-renders;
        # the figures ride the refusal and stderr, never the signed bytes.
        [ "$(q '.phys_short | length')" -eq 0 ] \
            || refuse "a remaining replica's disk has no live free space for the volume's next growth ($GIB bytes) — Longhorn v1.11.3 refuses on physical space as well as on the ledger, so retiring $REP would free the volume for nothing: $(q '.phys_short | join("; ")')"
    fi
    {
        echo "plan: retire-volume-replica"
        echo "volume: $VOL"
        echo "claim: $(q .claim)"
        echo "size: $SIZE bytes ($(q .size_gib))"
        echo "state: $(q .state)$( [ -n "$(q .on)" ] && echo " on $(q .on)")"
        echo "robustness: $rob"
        echo "data engine: $(q .engine)"
        echo "data locality: disabled"
        echo "replica auto-balance: disabled ($(q .autobalance_why))"
        echo "eviction: none requested on any replica of the volume, its node or its disk"
        echo "longhorn trims: none — under those three, Longhorn v1.11.3 removes no replica on a lowered count, so the delete in step 2 is the only removal"
        echo "longhorn settings: storage-over-provisioning-percentage $OPCT, storage-minimal-available-percentage $MPCT (each remaining disk's live storageAvailable is held to it on stderr, not signed)"
        if [ "$MODE" = retire ]; then
            echo "retire: replica $REP on $(q .rep.node) (disk $(q .rep.disk)): healthy"
        else
            echo "retire: replica $REP on $(q .rep.node) (disk $(q .rep.disk)): $(q .rep.state) — the one replica not healthy now; it may rebuild healthy before the delete, which then costs that healthy copy (the door back down after a raised count)"
        fi
        if [ "$RESUME" -eq 1 ]; then
            echo "numberOfReplicas: $DOWN -> $DOWN — the floor is $FLOOR, and never below 2"
            echo "replicas before: $N healthy replicas on $N distinct nodes; one named removal remains"
        else
            echo "numberOfReplicas: $N -> $DOWN — the floor is $FLOOR, and never below 2"
        fi
        echo "remain: $DOWN healthy replicas on $DOWN distinct nodes ($(q '.keep_nodes | join(", ")'))"
        if [ "$MODE" = retire ]; then
            echo "after: every remaining replica's disk admits the volume's growth by at least $GIB bytes (1 GiB); their ledgers admit growth to $(q .after_fit) bytes ($(q .after_fit_gib))"
            echo "physical space: each remaining disk's live storageAvailable holds that growth by ValidateDiskAvailableForExpansion and IsSchedulableToDisk (Longhorn v1.11.3) — judged at this render and again at the write's and the read-back's; the figures ride stderr, not these bytes"
        else
            echo "after: the retired replica is not healthy, so the room on the remaining disks is not a bound; their ledgers admit growth to $(q .after_fit) bytes ($(q .after_fit_gib))"
        fi
        if [ "$RESUME" -eq 1 ]; then
            echo "resume: count already lowered to $DOWN, with $N healthy replicas on $N distinct nodes — a new plan and approval, never the old authorization"
            echo "step 1: kubectl patch volumes.longhorn.io $VOL -n $LH --type=json — test-only: spec.numberOfReplicas is still $DOWN; no replace and no further lower"
        else
            echo "step 1: kubectl patch volumes.longhorn.io $VOL -n $LH --type=json — test that spec.numberOfReplicas is still $N, replace it with $DOWN"
        fi
        echo "before delete: re-read spec.numberOfReplicas $DOWN, the named and remaining replicas on their signed nodes and disks, data locality disabled, replica auto-balance disabled, no eviction, and robustness $( [ "$MODE" = retire ] && echo healthy || echo 'healthy or degraded') — an unreadable judgement stops the run"
        echo "step 2: watch the replicas for ${SETTLE_S}s: if $REP goes by anything else, nothing is deleted; if any other replica goes, a new one appears or another stops reading healthy, $REP is NOT deleted and the run stops naming it; otherwise kubectl delete replicas.longhorn.io $REP -n $LH (lowered first: a replica deleted under a count of $N is replaced once its finalizer clears)"
        echo "step 3: read back for up to ${READBACK_S}s: $REP gone, spec.numberOfReplicas $DOWN, exactly the $DOWN replicas below but $REP, each healthy, on $DOWN distinct nodes, robustness healthy$( [ "$MODE" = retire ] && echo ", and every remaining disk with room for the next GiB")"
        echo
        echo "== replicas of $VOL ($(q .n)) =="
        q '.replicas[]'
        echo
        echo "== each replica disk, summed as Longhorn's admission webhook sums it =="
        q '.rep_line'
        q '.disk_lines[]'
    } > "$WORK/plan"
    cp "$WORK/facts.json" "$WORK/planned.json"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $VOL (above) hashes to $HASH, not the approved plan $APPROVED — its count, its replicas, a setting or a disk's ledger moved since the plan was signed (or this plan already ran). Nothing was changed; render and approve it again"
fi
cat "$WORK/plan"
echo
ORIG="$(jq -c '.names' "$WORK/planned.json")"
KEEP="$(jq -c '.keep' "$WORK/planned.json")"
KEEP_PLACEMENTS="$(jq -c '.keep_placements' "$WORK/planned.json")"
REP_PLACEMENT="$(jq -c '.rep | {node, disk}' "$WORK/planned.json")"

# cas <from> <to> — the compare-and-set on spec.numberOfReplicas.
cas() {
    local patch
    if [ "$RESUME" -eq 1 ]; then
        patch="[{\"op\":\"test\",\"path\":\"/spec/numberOfReplicas\",\"value\":$DOWN}]"
    else
        patch="[{\"op\":\"test\",\"path\":\"/spec/numberOfReplicas\",\"value\":$1},{\"op\":\"replace\",\"path\":\"/spec/numberOfReplicas\",\"value\":$2}]"
    fi
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K patch volumes.longhorn.io "$VOL" -n "$LH" --type=json --request-timeout=20s \
        -p "$patch" \
        > "$WORK/patch.out" 2> "$WORK/patch.err"
}

# snapshot — read the volume, its replicas and the nodes and judge them
# into $WORK/now.json. Returns 1, with the reason in $WORK/snap.why, when
# a read could not look: a caller about to MUTATE stops on that.
snapshot() {
    local r shape
    : > "$WORK/snap.why"
    for r in "volumes.longhorn.io $VOL:s-vol.json" "replicas.longhorn.io:s-reps.json" "nodes.longhorn.io:s-nodes.json" "settings.longhorn.io replica-auto-balance:s-ab.json"; do
        case "${r##*:}" in s-vol.json) shape=volume;; s-ab.json) shape=setting;; *) shape=list;; esac
        # shellcheck disable=SC2086
        if ! $K get ${r%%:*} -n "$LH" -o json --request-timeout=30s > "$WORK/${r##*:}" 2> "$WORK/snap.err" \
            || ! typed_doc_file "$WORK/${r##*:}" "$shape" 2>> "$WORK/snap.err"; then
            echo "${r%%:*} unread or not exactly one $shape document: $(tr '\n' ' ' < "$WORK/snap.err" | cut -c1-200)" > "$WORK/snap.why"
            return 1
        fi
    done
    jq -n --arg vol "$VOL" --arg rep "$REP" --argjson orig "$ORIG" --argjson keep "$KEEP" --argjson placements "$KEEP_PLACEMENTS" \
        --argjson rep_placement "$REP_PLACEMENT" \
        --argjson opct "$OPCT" --argjson mpct "$MPCT" --argjson grow "$GIB" \
        --slurpfile v "$WORK/s-vol.json" --slurpfile reps "$WORK/s-reps.json" --slurpfile nodes "$WORK/s-nodes.json" \
        --slurpfile ab "$WORK/s-ab.json" \
        "$LONGHORN_LEDGER_JQ$STATE_JQ"'
        [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
        | [$mine[] | select(lh_healthy)] as $h
        | [$mine[] | .metadata.name] as $names
        | [lh_disks($nodes[0].items)[] | . + {room: lh_room($opct), limit: lh_limit($opct)}] as $all
        | ([$mine[] | select(.metadata.name != $rep)] | room_of($all; $opct; $mpct; $grow)) as $kd
        | {spec: ($v[0].spec.numberOfReplicas // "unreported"),
           robustness: ($v[0].status.robustness // "unreported"),
           trim: trim_state($v[0]; $mine; $nodes[0].items; $all; ($ab[0].value // "unreported" | tostring)),
           keep_distinct: ([$mine[] | select(.metadata.name != $rep and lh_healthy) | .spec.nodeID] | unique | length),
           names: $names,
           keep_exact: ($names == $keep),
           keep_placements_match: ([$mine[] | select(.metadata.name != $rep)
                                   | {name: .metadata.name, node: .spec.nodeID, disk: .spec.diskID}] == $placements),
           # The signed plan names the disk this deletion will free.
           # A stable CR name does not authorize deleting it elsewhere.
           rep_placement_match: any($mine[]; .metadata.name == $rep and
                                   {node: .spec.nodeID, disk: .spec.diskID} == $rep_placement),
           rep_present: ($names | index($rep) != null),
           # A replica being torn down stamps its deletionTimestamp and is
           # held on a finalizer: leaving, though still listed.
           rep_leaving: ([$mine[] | select(.metadata.name == $rep and lh_deleting)] != []),
           nhealthy: ($h | length),
           healthy_nodes: ([$h[] | .spec.nodeID // "no node"] | unique),
           others_gone: [$orig[] | select(. != $rep) | . as $n
                         | select(($names | index($n) == null)
                                  or ([$mine[] | select(.metadata.name == $n and lh_deleting)] != []))],
           others_unwell: [$mine[] | select(.metadata.name != $rep and (lh_healthy | not))
                           | "\(.metadata.name) on \(.spec.nodeID // "no node") (\(state_of))"],
           new: [$names[] | . as $n | select($orig | index($n) == null)],
           short: [$kd[] | select((.known | not) or .d.room < $grow or .phys != [])
                   | "disk \(.uuid)\(if .known | not then " on \(.node): no node reports its ledger"
                     else " (\(.d.name) on \(.d.node)): \(if .d.room < $grow then "ledger room \(.d.room) bytes, short of \($grow)" else "ledger room \(.d.room) bytes" end)\(if .phys != [] then "; live free space: \(.phys | join("; "))" else "" end)" end)"]}' \
        > "$WORK/now.json" 2> "$WORK/snap.err" || {
        echo "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/snap.err" | cut -c1-200)" > "$WORK/snap.why"
        return 1
    }
}
n() { jq -r "$1" "$WORK/now.json"; }
none() { n "$1 | join(\", \") | if . == \"\" then \"none\" else . end"; }

echo "$ME: plan $APPROVED still holds — retiring replica $REP of $VOL: numberOfReplicas $DECLARED -> $DOWN"
# --- step 1: lower -------------------------------------------------------------
if ! cas "$N" "$DOWN"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch volumes.longhorn.io $VOL ($DECLARED -> $DOWN) did not succeed (its words above) — nothing else was done and $REP stands; read the volume again with plan-a-volume-replica-retirement before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted spec.numberOfReplicas=$DOWN for $VOL — watching ${SETTLE_S}s for a replica removed by anything but this run before deleting $REP"

# --- step 2: settle, then delete exactly the named replica ----------------------
start="$(date -u +%s)"
trimmed=""
while :; do
    snapshot || fail "a read after lowering the count could not look ($(cat "$WORK/snap.why")) — $REP is NOT deleted; spec.numberOfReplicas is $DOWN with the replicas it had. Read the volume again with plan-a-volume-replica-retirement"
    # Judge once, positively; a jq error was formerly an empty integer
    # comparison that fell through to DELETE (077adeb3, N4).
    jq_doc_file "$WORK/now.json" || fail "the safety check has no document — $REP is NOT deleted; the lowered count stands"
    if ! judgement="$(jq -er --arg mode "$MODE" --argjson down "$DOWN" '
        if (.others_gone | length) > 0 then "others-gone"
        elif .rep_present == false or .rep_leaving == true then "named-left"
        elif (.others_unwell | length) > 0 or (.new | length) > 0 then "replicas-moved"
        elif .spec != $down or .keep_distinct != $down or .keep_placements_match != true
             or .rep_placement_match != true
             or .trim.locality != "disabled" or .trim.autobalance != "disabled"
             or (.trim.evicting | length) != 0
             or (if $mode == "retire" then .robustness != "healthy"
                 else .robustness != "healthy" and .robustness != "degraded" end)
        then "bounds-moved" else "safe" end' "$WORK/now.json" 2> "$WORK/judge.err")"; then
        cat "$WORK/judge.err" >&2
        fail "cannot judge the safety checks after lowering the count — $REP is NOT deleted; the lowered count stands. Read the volume again with plan-a-volume-replica-retirement"
    fi
    if [ "$judgement" = others-gone ]; then
        fail "after spec.numberOfReplicas was lowered to $DOWN, something other than this run removed (or is removing) $(none .others_gone) — not $REP — so $REP is NOT deleted, and nothing more is: a delete now would leave $((DOWN - 1)). $VOL holds $(n .nhealthy) healthy replica(s) on $(none .healthy_nodes) ($(none .names)). Read it again with plan-a-volume-replica-retirement"
    fi
    if [ "$judgement" = named-left ]; then
        trimmed=yes
        echo "$ME: something other than this run removed (or is removing) $REP on the lowered count — nothing to delete"
        break
    fi
    if [ "$judgement" = replicas-moved ]; then
        fail "after spec.numberOfReplicas was lowered to $DOWN, $VOL no longer holds the replicas it was signed with (not healthy: $(none .others_unwell); new: $(none .new); healthy on $(none .healthy_nodes)) — $REP is NOT deleted, and nothing more is. Read it again with plan-a-volume-replica-retirement"
    fi
    [ "$judgement" = safe ] || fail "the count, signed replica placements, remaining distinct healthy replicas, trim settings or robustness moved before deleting $REP: $(cat "$WORK/now.json") — $REP is NOT deleted; the lowered count stands. Read the volume again with plan-a-volume-replica-retirement"
    [ $(($(date -u +%s) - start)) -ge "$SETTLE_S" ] && break
    sleep "$POLL_S"
done
if [ -z "$trimmed" ]; then
    # Straight off the read above: every replica but the named one
    # healthy, the named one still there.
    # shellcheck disable=SC2086
    if ! $K delete replicas.longhorn.io "$REP" -n "$LH" --wait=false --request-timeout=20s \
            > "$WORK/delete.out" 2> "$WORK/delete.err"; then
        sed 's/^/    /' "$WORK/delete.err" >&2
        fail "kubectl delete replicas.longhorn.io $REP did not succeed (its words above) — spec.numberOfReplicas is $DOWN with $N replicas, $REP among them; read the volume again with plan-a-volume-replica-retirement"
    fi
    sed 's/^/    /' "$WORK/delete.out"
    echo "$ME: deleted $REP — reading it back (up to ${READBACK_S}s: $REP gone, $DOWN healthy replicas on $DOWN distinct nodes)"
fi

# --- step 3: read back -----------------------------------------------------------
start="$(date -u +%s)"
while :; do
    if snapshot; then
        if [ "$(n .rep_present)" = false ] && [ "$(n .spec)" = "$DOWN" ] && [ "$(n .keep_exact)" = true ] \
            && [ "$(n .keep_placements_match)" = true ] \
            && [ "$(n .nhealthy)" = "$DOWN" ] && [ "$(n '.healthy_nodes | length')" = "$DOWN" ] \
            && [ "$(n .robustness)" = healthy ]; then
            if [ "$MODE" = retire ]; then
                if ! jq_doc_file "$WORK/now.json" || ! jq -e '.short | type == "array" and length == 0' "$WORK/now.json" > "$WORK/room.out" 2> "$WORK/room.err"; then
                    cat "$WORK/room.err" >&2
                    fail "$REP is gone and $VOL has $DOWN healthy replicas on $DOWN distinct nodes ($(n '.healthy_nodes | join(", ")')), but NOT every remaining replica's disk has proven room for the next GiB of growth now: $(n '.short | join("; ")'). The retirement stands; the growth it was for does not fit yet"
                fi
            fi
            echo "$ME: $VOL has $DOWN healthy replicas on $DOWN distinct nodes ($(n '.healthy_nodes | join(", ")')) — $REP is gone — read back: spec.numberOfReplicas=$DOWN, robustness healthy (after $(($(date -u +%s) - start))s)"
            exit 0
        fi
        why="$REP $(if [ "$(n .rep_present)" = true ]; then echo "still present"; else echo gone; fi), spec.numberOfReplicas $(n .spec), robustness $(n .robustness), healthy on $(none .healthy_nodes), replicas $(none .names) (planned to remain: $(jq -r 'join(", ")' <<<"$KEEP")); not healthy: $(none .others_unwell)"
    else
        why="$(cat "$WORK/snap.why")"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$elapsed" -ge "$READBACK_S" ]; then
        fail "the retirement of $REP is NOT proven after ${elapsed}s: $why. Read the volume again with plan-a-volume-replica-retirement"
    fi
    sleep "$POLL_S"
done
