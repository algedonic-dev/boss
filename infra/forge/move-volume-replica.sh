#!/usr/bin/env bash
# move-volume-replica.sh — move ONE replica of ONE Longhorn volume off its
# disk, through a rendered plan a passkey signs. The ops verbs
# `plan-a-volume-replica-move` (--plan) and `move-volume-replica`.
#
#   move-volume-replica.sh --plan <volume> <replica>
#   move-volume-replica.sh --plan-decisive <volume> <namespace> <pvc>
#   move-volume-replica.sh <volume> <replica> <plan-sha256>
#
# --plan-decisive is read-only argument discovery (backlog 53c8cb72, design
# ada8f698 option A, David 2026-10-07): asked only by
# expand-instance-volume.sh --plan-largest after it found that no growth
# fits a claim, it answers whether EXACTLY ONE replica move would free the
# volume to grow, and prints that move's plan as one JSON proposal — or
# refuses, saying which fact was missing. See THE DECISIVE READ below. It
# is not approval or execution: the proposal names plan-a-volume-replica-move
# and its two words, and the move itself is still this script's write,
# behind the hash David's passkey signed.
#
# WHY IT EXISTS (backlog ab39a34e, incident d3c0a67c, 2026-10-01). The
# system of record's database volume, boss/pgdata-postgres-0 (Longhorn
# volume pvc-93e11a6e-…, 30 GiB after David's hand patch), held four
# replicas, on cp-1, cp-2, cp-3 and w-2 (read through
# plan-a-volume-replica-change, ops-request 5b61b067), and the w-2
# replica's disk, bf045701-… (StorageMaximum 117656518656), had ~755 MB
# left in Longhorn's scheduling ledger, so the admission webhook refused
# every growth of the volume. David chose to RETIRE that replica instead
# (a separate verb); this one is the general door: a replica cannot be
# "moved" in Longhorn — a new one is built and the old one removed — and
# by hand that is two patches and a delete in an order that matters.
# It takes any Longhorn volume, not only an instance's: moving a replica
# is volume-agnostic, and David's passkey reads the claim in the plan.
#
# THE SHAPE is design 17835005's, as set-volume-replicas' and
# expand-instance-volume's: the verb file declares requires_approval and
# names plan-a-volume-replica-move as its plan_verb, so the runner renders
# this script's --plan onto the request's approve step and hands the
# write sha256 of the SIGNED plan. The write re-renders and changes
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks.
# Every Longhorn rule cited is longhorn-manager v1.11.3's, the version the
# tree records (infra/forge/longhorn-drain-policy.sh), read from its source
# by adversarial review 091904d3:
#   * the volume is one pvc-<uuid> Longhorn holds in longhorn-system, and
#     the replica is one of ITS replicas, named <volume>-r-<8 hex>;
#   * the volume is HEALTHY at the start: attached (Longhorn rebuilds an
#     attached volume), robustness healthy, and exactly
#     spec.numberOfReplicas replicas, every one healthy AND active
#     (isHealthyAndActiveReplica: spec.healthyAt set, spec.failedAt empty,
#     spec.active, not being deleted) on a distinct node;
#   * one replica per node — replica-soft-anti-affinity resolves to false
#     (the volume's spec.replicaSoftAntiAffinity, or the setting when the
#     volume says `ignored`). Only then is the set of disks Longhorn can
#     put the new replica on a thing this plan can NAME: the disks of the
#     nodes holding no replica of the volume;
#   * LONGHORN TRIMS NOTHING ON ITS OWN when the count is lowered. In
#     v1.11.3, cleanupExtraHealthyReplicas (controller/volume_controller.go)
#     removes a replica a lowered count leaves over only through
#     cleanupEvictionRequestedReplicas, cleanupDataLocalityReplicas
#     (data locality not disabled, with a local replica — it deletes the
#     lexicographically smallest non-local one, which can be the NEW
#     replica) or cleanupAutoBalancedReplicas (auto-balance not disabled —
#     an effectively arbitrary choice). So the plan refuses unless the
#     volume's spec.dataLocality is "" or disabled
#     (isDataLocalityDisabled), replica auto-balance resolves to disabled
#     (GetAutoBalancedReplicasSetting: the volume's spec.replicaAutoBalance
#     unless "" or ignored, else the replica-auto-balance setting, with
#     ignored and anything invalid read as disabled), and no eviction is
#     requested on any replica, or on any replica's node or disk — an
#     evicting replica also makes Longhorn build a SECOND extra replica
#     (getReplenishReplicasCount). Under those three, the only hand that
#     removes a replica during the settle below is a foreign one;
#   * every disk Longhorn could put the new replica on — node and disk
#     schedulable, the volume's node and disk selectors held, the data
#     engine's disk type, and ledger room for the replica — has ledger
#     headroom for TWICE the volume (room >= 2 x size): the replica, and
#     the one growth expand-instance-volume grants a run. That list is
#     signed, and it is a superset of where Longhorn can place (Longhorn
#     also needs live free space), which is the safe direction;
#   * and each such disk passes, on LIVE figures (infra/lib/longhorn-ledger.sh,
#     pinned to v1.11.3 by tests/longhorn_ledger_sh.rs): the scheduler's
#     own placement test, lh_placement = IsSchedulableToDisk(spec.size,
#     status.actualSize) (replica_scheduler.go:578); and the next growth
#     on the disk as it would stand with the replica on it — storageScheduled
#     plus the size, storageAvailable less the actualSize the rebuild
#     writes — by lh_expansion = CheckReplicasSizeExpansion's per-disk
#     test (ValidateDiskAvailableForExpansion, then IsSchedulableToDisk with
#     requiredStorage 0). That projection is this plan's, named as one:
#     Longhorn judges the growth when it is asked for, on the disk as it
#     then is. storageAvailable and actualSize move every second, so these
#     are judged at every render (the plan's and the write's) and refuse
#     there; the signed bytes say only that they were judged;
#   * every disk of the volume's replicas is one a node reports (the
#     replica's spec.diskID against diskStatus[].diskUUID), or the read
#     cannot answer.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`, since a hash
# cannot be inside what it hashes) names the volume, its size, the three
# trim settings, the replica to move and its disk's ledger, the count's
# path (N -> N+1 -> N), every target disk with its ledger sum, what the
# remaining replicas' disks admit after the move, every step and every
# Longhorn disk's verdict. No clock, no live free space and no actualSize
# in the bytes. The count and the replicas before are IN the bytes, so a
# second run of an applied plan re-renders to a refusal: at most once.
#
# THE WRITE, in this order, each step only when the one before read back:
#   1. one `kubectl patch volumes.longhorn.io --type=json` that TESTS
#      spec.numberOfReplicas is still N and replaces it with N+1 — the
#      compare-and-set set-volume-replicas makes;
#   2. wait (BOSS_MOVE_REBUILD_S, default 1200) for a replica that was
#      not in the plan to read healthy on one of the plan's TARGET disks.
#      A new replica healthy anywhere else, or none in time — stalled, or
#      only slower than the wait — stops here: the count STAYS N+1 (more
#      copies, never fewer), the named replica is NOT deleted, and the run
#      exits 1 saying so and naming the replica it waited on;
#   3. with N+1 healthy on N+1 distinct nodes read back, a second
#      compare-and-set lowers the count from N+1 to N; then, after
#      BOSS_MOVE_SETTLE_S (default 30) of reads in which no replica was
#      removed, the named replica is deleted. LOWER FIRST, THEN DELETE: a
#      replica deleted while the count says N+1 leaves one short, and
#      v1.11.3 replenishes once its finalizer clears (getReplenishReplicasCount
#      counts a replica being deleted, so not before), onto a node holding
#      none — the node just vacated, so the move would rebuild itself back
#      onto the full disk. Under the plan's settings Longhorn trims nothing
#      on the lowered count, so the settle watches for a foreign hand: the
#      named replica removed (or terminating, a deletionTimestamp set) is
#      the move done by someone else, and anything else removed stops the
#      run before the delete, which would leave N-1;
#   4. read back (BOSS_MOVE_READBACK_S, default 180), waiting for the named
#      replica's finalizer: the replica gone from the list,
#      spec.numberOfReplicas N, N healthy replicas on N distinct nodes
#      including the new one, robustness healthy, and EVERY replica's disk
#      admitting the volume's growth to twice its size on live figures
#      (lh_expansion of replicas-on-disk x size: the expand verb's own
#      question). Only then is the line the verb file declares as `effect`
#      printed; a disk still short is exit 1 naming it.
#
# HOW FEW HEALTHY REPLICAS, AT WORST. spec.numberOfReplicas never goes
# below N: N -> N+1 -> N, each a compare-and-set. The delete follows a
# read, in the same pass, of N+1 healthy replicas against a count of N,
# so after it N stand. An original replica that fails in the second
# between that read and the delete leaves N-1 for as long as Longhorn
# takes to rebuild; no kubectl call can close that window.
#
# THE DECISIVE READ (--plan-decisive), each rule the design's own line
# (design-doc ada8f698, "A tight claim proposes one evidenced replica
# move"; its question's option A):
#   * "a replica belongs to that exact claim-bound volume": the volume's
#     kubernetesStatus names the namespace/pvc asked about, or it refuses;
#   * "Unknown/malformed/duplicate/partial lists refuse rather than create
#     a proposal": every object is ONE document of its kind and name, each
#     list is one complete list (no continue token) of uniquely named
#     objects, and every size, ledger field and setting is an exact
#     non-negative integer (infra/lib/longhorn-ledger.sh, the guards the
#     largest-fit read already used) — else CANNOT ANSWER, exit 1;
#   * "the existing move plan admits that named source, every allowed
#     new-target disk passes existing placement and projected next-growth
#     checks": THE SAME bounds, run by the same lines, as --plan runs;
#     "failed health/trim/anti-affinity checks ... make the whole judgment
#     unavailable" — any of them refuses for every candidate at once;
#   * "all remaining original replica disks admit the projected next growth
#     on the same conservative shared arithmetic": for each replica in
#     turn as the source, the plan's own `short_after` (ledger) and
#     `after_live` (live) lists must both be empty. That is the typed
#     judgement the design asks for ("not grep plan prose"): a plan that
#     renders with "these are SHORT" is not a decisive move;
#   * "if exactly one eligible replica exists, select it. Zero eligible
#     replicas produces a named refusal. Multiple eligible replicas also
#     refuse with the complete candidate identities" — in byte order, which
#     is presentation and never a choice. Nothing ranks.
# Two premises are held as well, because a proposal that passes the lines
# above and misses either would not be DECISIVE; each refuses, neither
# chooses:
#   * something must be short: when no replica disk is short of the
#     volume's next growth, a move frees nothing ("Growth blocked only by
#     the 100Gi ceiling or another non-placement bound must refuse without
#     movement");
#   * the volume holds no more replicas than the retirement floor
#     (LONGHORN_RETIRE_FLOOR, 3). Above it, retiring the replica on the
#     short disk is admissible too, and which of the two is a person's
#     choice — it was David's on 2026-10-01, for boss/pgdata-postgres-0's
#     four replicas, and he chose to retire. The design does not rule on
#     this case, so it refuses and says so.
# The proposal is `{verb, args, target, plan, plan_sha256}` on stdout:
# plan-a-volume-replica-move, [volume, replica], the [namespace, pvc] it
# answers, and the plan exactly as --plan renders it for that replica.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan; a read that fails between steps stops the
# run before the next mutation. Exit 78 is a refusal (the request was
# wrong, or is no longer true).
set -uo pipefail

ME=move-volume-replica
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
# LONGHORN_LEDGER_JQ: Longhorn v1.11.3's disk arithmetic, one copy.
# shellcheck source=infra/lib/longhorn-ledger.sh
. "$HERE/../lib/longhorn-ledger.sh"

LH=longhorn-system
UUID_RE='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
VOL_RE="^pvc-${UUID_RE}\$"
REP_RE="^pvc-${UUID_RE}-r-[0-9a-f]{8}\$"

LABEL='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'

# WHOSE is empty except in the decisive read, where every refusal and
# every unread answer opens by saying that nothing is proposed, and for
# which claim: one line a reader of the discovery request can act on.
WHOSE=""
refuse() { echo "$ME: REFUSED — $WHOSE$*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $WHOSE$*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread volume has no plan, and nothing was written"; }

PLAN=0
DECISIVE=0
if [ "${1-}" = "--plan-decisive" ]; then
    PLAN=1
    DECISIVE=1
    shift
    [ $# -eq 3 ] || refuse "usage: move-volume-replica.sh --plan-decisive <volume> <namespace> <pvc> — read-only discovery takes the volume and the claim it must be bound to, and nothing else"
elif [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || refuse "usage: move-volume-replica.sh --plan <volume> <replica> — the plan takes the volume and the replica and nothing else"
else
    [ $# -eq 3 ] || refuse "usage: move-volume-replica.sh <volume> <replica> <plan-sha256> — this moves only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-volume-replica-move verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
VOL="$1"
[[ "$VOL" =~ $VOL_RE ]] || refuse "the volume must be one Longhorn volume name, pvc-<uuid> in lower case, got '$VOL'"
if [ "$DECISIVE" -eq 1 ]; then
    # The replica is what this read is asked to find. Until it has, REP is
    # empty and names nothing.
    REP=""
    CLAIM_NS="$2"
    CLAIM_PVC="$3"
    [[ "$CLAIM_NS" =~ $LABEL ]] || refuse "the namespace must be one DNS label, got '$CLAIM_NS'"
    [[ "$CLAIM_PVC" =~ $LABEL ]] || refuse "the claim must be one DNS label, got '$CLAIM_PVC'"
    WHOSE="no replica move is proposed for $CLAIM_NS/$CLAIM_PVC — "
else
    REP="$2"
    [[ "$REP" =~ $REP_RE ]] || refuse "the replica must be one Longhorn replica name, <volume>-r-<8 hex> in lower case, got '$REP'"
    [ "${REP%-r-*}" = "$VOL" ] || refuse "the replica $REP is named for volume ${REP%-r-*}, not $VOL"
fi
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not a plan"

REBUILD_S="${BOSS_MOVE_REBUILD_S:-1200}"
SETTLE_S="${BOSS_MOVE_SETTLE_S:-30}"
READBACK_S="${BOSS_MOVE_READBACK_S:-180}"
POLL_S="${BOSS_MOVE_POLL_S:-15}"
case "${REBUILD_S:-empty}${SETTLE_S:-empty}${READBACK_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) fail "BOSS_MOVE_REBUILD_S, BOSS_MOVE_SETTLE_S, BOSS_MOVE_READBACK_S and BOSS_MOVE_POLL_S must be whole seconds, got '$REBUILD_S', '$SETTLE_S', '$READBACK_S' and '$POLL_S'" ;;
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
    if [ "$DECISIVE" -eq 1 ]; then
        local kind
        case "$1" in
            volumes.longhorn.io) kind=Volume ;;
            settings.longhorn.io) kind=Setting ;;
            *) unread "the decisive read does not know the typed resource $1" ;;
        esac
        lh_one_object "$WORK/$file" "$kind" "$2" "$LH" \
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
    if [ "$DECISIVE" -eq 1 ]; then
        local kind
        case "$1" in
            replicas.longhorn.io) kind=Replica ;;
            nodes.longhorn.io) kind=Node ;;
            *) unread "the decisive read does not know the typed list $1" ;;
        esac
        lh_whole_list "$WORK/$2" "$kind" "$LH" \
            || unread "$1 is not exactly one complete list with unique named objects"
    fi
}

# facts — one JSON object judged from the reads: the volume, its
# replicas, the replica to move, the trim settings, every Longhorn disk's
# verdict, and the sums. `live*` fields carry storageAvailable and
# actualSize and never reach the signed bytes.
facts() {
    jq -n --arg vol "$VOL" --arg rep "$REP" \
        --slurpfile v "$WORK/vol.json" \
        --slurpfile reps "$WORK/replicas.json" \
        --slurpfile nodes "$WORK/lhnodes.json" \
        --slurpfile op "$WORK/overprov.json" \
        --slurpfile ma "$WORK/minavail.json" \
        --slurpfile aa "$WORK/antiaff.json" \
        --slurpfile ab "$WORK/autobal.json" "$LONGHORN_LEDGER_JQ"'
        def state_of: if lh_deleting then "being deleted" elif lh_failed then "failed"
                      elif lh_healthy then "healthy" elif .spec.active != true and (.spec.healthyAt // "") != "" then "healthy but not active"
                      else "not yet healthy (rebuilding, or never finished)" end;
        $v[0] as $V
        | ($V.spec.size | lh_num) as $size
        | if $size == null then {size: null} else (
          ($op[0].value | lh_num) as $opct
        | ($ma[0].value | lh_num) as $mpct
        | ($V.status.actualSize | lh_num) as $actual
        | ($V.spec.replicaSoftAntiAffinity // "ignored") as $avol
        | ($aa[0].value // "unreported" | tostring) as $aset
        | ($V.spec.dataLocality // "") as $loc
        | ($V.spec.replicaAutoBalance // "") as $abvol
        | ($ab[0].value // "" | tostring) as $abset
        | (if $abvol != "" and $abvol != "ignored" then $abvol
           elif $abset == "ignored" then "disabled" else $abset end) as $abraw
        | (if $abraw == "least-effort" or $abraw == "best-effort" then $abraw else "disabled" end) as $abal
        | ($V.spec.dataEngine // "v1") as $engine
        | ($V.spec.nodeSelector // []) as $nsel
        | ($V.spec.diskSelector // []) as $dsel
        | [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
        | [$mine[] | .spec.nodeID // "" | select(. != "")] as $occ
        | lh_disks($nodes[0].items) as $all
        | [$all[] | . + {room: lh_room($opct), limit: lh_limit($opct)}] as $all
        | ([$mine[] | select(.metadata.name == $rep)] | .[0] // null) as $R
        # Every eviction that would make Longhorn trim (or build an extra
        # replica) on its own: a replica'"'"'s own flag, its node'"'"'s, its disk'"'"'s.
        | [$mine[] | . as $r
            | ([$all[] | select(.uuid == ($r.spec.diskID // ""))] | .[0] // null) as $d
            | (if $r.spec.evictionRequested == true then "replica \($r.metadata.name) has evictionRequested" else empty end),
              (if $d != null and $d.node_evict then "node \($d.node), which holds \($r.metadata.name), has evictionRequested" else empty end),
              (if $d != null and $d.evict then "disk \($d.uuid) (\($d.name) on \($d.node)), which holds \($r.metadata.name), has evictionRequested" else empty end)]
            | unique as $evictions
        # Every disk a replica of the volume names, by UUID.
        | [$mine | group_by(.spec.diskID // "")[] | . as $g | ($g[0].spec.diskID // "") as $u
            | ([$all[] | select(.uuid == $u)] | .[0] // null) as $d
            | {uuid: $u, node: ($g[0].spec.nodeID // "no node"), count: ($g | length),
               names: [$g[] | .metadata.name], d: $d}] as $rdisks
        | [$rdisks[] | select(.d == null or .d.room == null) | "disk \(.uuid) on \(.node)"] as $unknown
        | [$rdisks[] | select(.d != null and .d.room != null)
            | . + {per: ((.d.room / .count) | floor)}
            | . + {line: "disk \(.uuid) (\(.d.name) on \(.d.node)): \(.count) replica(s) of this volume; ProvisionedLimit (\(.d.max) - \(.d.reserved)) x \($opct)% = \(.d.limit); storageScheduled \(.d.scheduled); room \(.d.room) bytes, \(.per) per replica — \(if .per >= $size then "admits growth to twice the volume (\(2 * $size) bytes)" else "SHORT: admits growth of \(.per) bytes, not the volume'"'"'s \($size)" end)"}]
            | sort_by(.uuid) as $known
        # The disks after the move: every replica disk but the moving
        # replica'"'"'s, one fewer on that disk.
        | [$known[] | select(.names != [$rep])
            | if (.names | index($rep)) != null then .count -= 1 | .per = ((.d.room / .count) | floor) else . end] as $remain
        | [$remain[] | select(.per < $size) | .line] as $short_after
        # The replica disks short of the next growth TODAY, by the same
        # two sums: what the decisive read holds a move against. Never in
        # the signed bytes.
        | [$known[] | . as $k | ($k.d | lh_expansion($k.count * $size; $opct; $mpct)) as $p
            | select($k.per < $size or $p != [])
            | {uuid: $k.uuid, names: $k.names,
               say: "\($k.names | join(", ")) on disk \($k.uuid) (\($k.d.name) on \($k.d.node)): \(if $k.per < $size then "its ledger admits growth of \($k.per) bytes, not \($size)" else ($p | join("; ")) end)"}] as $blocked
        # The same disks, live: would the expand verb'"'"'s growth to twice
        # pass there today? Stderr only.
        | [$remain[] | . as $r | ($r.d | lh_expansion($r.count * $size; $opct; $mpct)) as $p | select($p != [])
            | "disk \($r.uuid) (\($r.d.name) on \($r.d.node)): \($p | join("; "))"] as $after_live
        # Which disks Longhorn can put the new replica on: a node holding
        # no replica of the volume, node and disk schedulable, selectors
        # held, the engine'"'"'s disk type, a UUID, and ledger room for it.
        | [$all[] | . as $d
            | ([(if ($occ | index($d.node)) != null then "node \($d.node) holds a replica of this volume" else empty end)]
               + $d.node_why
               + [$d.why[] | "disk: \(.)"]
               + [(if $d.uuid == null then "reports no diskUUID" else empty end),
                  (if ($engine == "v2" and $d.type != "block") or ($engine != "v2" and $d.type != "filesystem")
                   then "disk type \($d.type) does not serve a \($engine) volume" else empty end),
                  (if ([$nsel[] | select(($d.node_tags | index(.)) == null)] | length) > 0
                   then "node tags \($d.node_tags) do not hold the volume'"'"'s nodeSelector \($nsel)" else empty end),
                  (if ([$dsel[] | select(($d.tags | index(.)) == null)] | length) > 0
                   then "disk tags \($d.tags) do not hold the volume'"'"'s diskSelector \($dsel)" else empty end),
                  (if $d.room == null then "its ledger is unreported"
                   elif $d.room < $size then "room \($d.room) bytes is short of the replica'"'"'s \($size)" else empty end)]) as $why
            | {uuid: $d.uuid, node: $d.node, name: $d.name, room: $d.room,
               target: ($why == []),
               ok: ($why == [] and $d.room >= 2 * $size),
               # Live: Longhorn'"'"'s placement test, then the next growth on
               # the disk as it would stand with the replica on it.
               live_why: (if $why != [] then []
                          else ($d | lh_placement($size; $actual; $opct; $mpct) | map("placement: \(.)"))
                             + ($d | .scheduled += $size | .available = (if .available == null or $actual == null then null else .available - $actual end)
                                   | lh_expansion($size; $opct; $mpct) | map("the next growth, with the replica on it: \(.)")) end),
               line: (if $why == []
                      then "disk \($d.uuid) (\($d.name) on \($d.node)): TARGET — ProvisionedLimit (\($d.max) - \($d.reserved)) x \($opct)% = \($d.limit); storageScheduled \($d.scheduled); headroom \($d.room) bytes \(if $d.room >= 2 * $size then ">=" else "<" end) 2 x \($size) = \(2 * $size)\(if $d.room >= 2 * $size then "" else " — Longhorn may place the replica here, and the volume'"'"'s next growth would fail the webhook on this disk" end)"
                      else "disk \($d.uuid // $d.name) (\($d.name) on \($d.node)): not a target — \($why | join("; "))" end),
               live: "disk \($d.uuid // $d.name) (\($d.name) on \($d.node)): storageAvailable \(if $d.available == null then "unreported" else "\($d.available) bytes" end) against the floor \(($d | lh_floor($mpct)) // "unknown"); the volume'"'"'s actualSize \($actual // "unreported")"}]
            | sort_by(.node, .name) as $verdicts
        | {size: $size, opct: $opct, mpct: $mpct,
           current: ($V.spec.numberOfReplicas // null),
           claim: (($V.status.kubernetesStatus // {}) as $k
                   | if ($k.pvcName // "") != "" then "\($k.namespace)/\($k.pvcName)" else "none" end),
           state: ($V.status.state // "unreported"),
           on: ($V.status.currentNodeID // ""),
           robustness: ($V.status.robustness // "unreported"),
           locality: (if $loc == "" then "unset" else $loc end),
           locality_off: ($loc == "" or $loc == "disabled"),
           autobal: "the volume says \(if $abvol == "" then "nothing" else $abvol end), the setting replica-auto-balance says \(if $abset == "" then "nothing" else $abset end); it resolves to \($abal)",
           autobal_off: ($abal == "disabled"),
           evictions: $evictions,
           engine: $engine,
           anti: "the volume says \($avol), the setting replica-soft-anti-affinity says \($aset)",
           anti_hard: (if $avol == "disabled" then true elif $avol == "enabled" then false
                       else ($aset == "false") end),
           n: ($mine | length),
           nhealthy: ([$mine[] | select(lh_healthy)] | length),
           distinct: ([$mine[] | select(lh_healthy) | .spec.nodeID] | unique | length),
           names: [$mine[] | .metadata.name],
           replicas: [$mine[] | "replica \(.metadata.name) on \(.spec.nodeID // "no node") (disk \(.spec.diskID // "none")): \(state_of)"],
           rep: (if $R == null then null
                 else {node: ($R.spec.nodeID // "no node"), disk: ($R.spec.diskID // "none"), healthy: ($R | lh_healthy)} end),
           unknown: $unknown,
           disk_lines: [$known[] | .line],
           short_after: $short_after,
           blocked: $blocked,
           after_fit: (if $remain == [] then null else ([$remain[] | .per] | min) + $size end),
           targets: [$verdicts[] | select(.target) | .uuid],
           short_targets: [$verdicts[] | select(.target and (.ok | not)) | .line],
           live_short: [$verdicts[] | select(.target and .live_why != []) | "disk \(.uuid) (\(.name) on \(.node)): \(.live_why | join("; "))"],
           after_live: $after_live,
           verdicts: [$verdicts[] | .line],
           live: [$verdicts[] | .live]}) end' 2> "$WORK/facts.err"
}

# render — read Longhorn now, apply the bounds, and write the plan to
# $WORK/plan. Sets N (the count before), SIZE, OPCT, MPCT and TARGETS.
# Nothing in the bytes is a clock, live free space or actualSize. It is
# four steps so that the decisive read runs the SAME lines between them:
# read_all, bounds_volume (what must hold of the volume whichever replica
# moves), bounds_targets (where the new replica can land — the same for
# every replica of the volume) and write_plan.
F="$WORK/facts.json"
q() { jq -r "$1" "$F"; }

read_all() {
    read_obj refuse vol.json volumes.longhorn.io "$VOL" -n "$LH"
    [ "$(jq -r '.metadata.name // ""' "$WORK/vol.json")" = "$VOL" ] \
        || unread "volumes.longhorn.io $VOL did not read back as that volume: $(head -c 200 "$WORK/vol.json" | tr '\n' ' ')"
    read_list replicas.longhorn.io replicas.json
    read_list nodes.longhorn.io lhnodes.json
    read_obj unread overprov.json settings.longhorn.io storage-over-provisioning-percentage -n "$LH"
    read_obj unread minavail.json settings.longhorn.io storage-minimal-available-percentage -n "$LH"
    read_obj unread antiaff.json settings.longhorn.io replica-soft-anti-affinity -n "$LH"
    read_obj unread autobal.json settings.longhorn.io replica-auto-balance -n "$LH"
    [ "$DECISIVE" -eq 1 ] || return 0
    local file
    for file in vol.json replicas.json lhnodes.json overprov.json minavail.json; do
        jq_doc_file "$WORK/$file" || unread "$file holds no document for the exact-input guard"
    done
    jq -e -n --arg vol "$VOL" \
        --slurpfile v "$WORK/vol.json" --slurpfile r "$WORK/replicas.json" \
        --slurpfile n "$WORK/lhnodes.json" \
        --slurpfile o "$WORK/overprov.json" --slurpfile m "$WORK/minavail.json" \
        "$LONGHORN_LEDGER_JQ"'
        lh_exact_inputs($vol; $v[0]; $r[0].items; $n[0].items; $o[0].value; $m[0].value)' >/dev/null 2>&1 \
        || unread "the decisive read needs complete unique identities and nonnegative exact integer sizes, disk fields and settings; no replica can be named from less"
}

# judge — the facts for REP as the source, into $F.
judge() {
    if ! facts > "$F"; then
        unread "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/facts.err")"
    fi
}

bounds_volume() {
    local n_reps nh dist
    judge
    SIZE="$(q '.size // "none"')"
    N="$(q '.current // "none"')"
    OPCT="$(q '.opct // "none"')"
    MPCT="$(q '.mpct // "none"')"
    case "$SIZE" in '' | *[!0-9]*) unread "volume $VOL reports no spec.size ('$SIZE')" ;; esac
    case "$N" in '' | *[!0-9]*) unread "volume $VOL reports no spec.numberOfReplicas ('$N')" ;; esac
    case "$OPCT$MPCT" in '' | *[!0-9]*) unread "Longhorn's storage-over-provisioning-percentage or storage-minimal-available-percentage did not read as a number" ;; esac
    if [ -n "$REP" ]; then
        [ "$(q '.rep != null')" = true ] \
            || refuse "$REP is not a replica of $VOL — its replicas are: $(q '.replicas | join("; ")')"
    fi
    [ "$(q .state)" = attached ] \
        || refuse "$VOL is $(q .state), not attached — Longhorn rebuilds a replica of an attached volume, and a move is a rebuild"
    [ "$(q .robustness)" = healthy ] \
        || refuse "$VOL reads robustness $(q .robustness) — only a healthy volume moves a replica: $(q '.replicas | join("; ")')"
    n_reps="$(q .n)"
    nh="$(q .nhealthy)"
    dist="$(q .distinct)"
    if [ "$n_reps" != "$N" ] || [ "$nh" != "$N" ] || [ "$dist" != "$N" ]; then
        refuse "$VOL declares spec.numberOfReplicas $N and has $n_reps replica(s), $nh healthy and active on $dist distinct node(s) — only a volume whose every declared replica is healthy, one per node, moves one: $(q '.replicas | join("; ")')"
    fi
    [ "$N" -ge 1 ] || refuse "$VOL declares spec.numberOfReplicas $N"
    [ "$(q .anti_hard)" = true ] \
        || refuse "$VOL may place two replicas on one node ($(q .anti)) — then this plan cannot name the disks the new replica can land on, and a move could leave two copies on one node"
    [ "$(q .locality_off)" = true ] \
        || refuse "$VOL has data locality $(q .locality) — on a lowered count Longhorn deletes the smallest-named replica not on the engine's node (cleanupDataLocalityReplicas), which can be the new one; only a volume whose data locality is disabled moves a replica here"
    [ "$(q .autobal_off)" = true ] \
        || refuse "replica auto-balance is on for $VOL ($(q .autobal)) — on a lowered count Longhorn deletes a replica of its own choosing (cleanupAutoBalancedReplicas); only a volume whose auto-balance resolves to disabled moves a replica here"
    [ "$(q '.evictions | length')" -eq 0 ] \
        || refuse "an eviction is requested where $VOL's replicas live — Longhorn trims an evicting replica itself and builds an extra one for it (cleanupEvictionRequestedReplicas, getReplenishReplicasCount), so a move here would race it: $(q '.evictions | join("; ")')"
    [ "$(q '.unknown | length')" -eq 0 ] \
        || unread "no Longhorn node reports the disk of $(q '.unknown | join(", ")') (spec.diskID against status.diskStatus[].diskUUID), or its ledger is missing — the webhook could not sum it either"
}

bounds_targets() {
    jq -r --arg me "$ME" '.live[] | "\($me): live, not signed (judged again when the write re-renders): \(.)"' "$F" >&2
    [ "$(q '.targets | length')" -gt 0 ] \
        || refuse "no disk on a node without a replica of $VOL can take a $SIZE-byte replica: $(q '.verdicts | join("; ")')"
    [ "$(q '.short_targets | length')" -eq 0 ] \
        || refuse "Longhorn could place the new replica on a disk without ledger headroom for twice the volume ($((2 * SIZE)) bytes), and the volume's next growth would then fail on THAT disk: $(q '.short_targets | join("; ")')"
    # The live half, judged now and again when the write re-renders; the
    # figures ride the refusal and stderr, never the signed bytes.
    [ "$(q '.live_short | length')" -eq 0 ] \
        || refuse "a disk Longhorn could place the new replica on fails Longhorn v1.11.3's live test, for the placement or for the volume's next growth there: $(q '.live_short | join("; ")')"
    TARGETS="$(q '.targets | join(" ")')"
}

write_plan() {
    jq -r --arg me "$ME" '.after_live[] | "\($me): live, not signed — a remaining replica disk that would refuse the volume'"'"'s growth to twice today, so the read-back will name it: \(.)"' "$F" >&2
    {
        echo "plan: move-volume-replica"
        echo "volume: $VOL"
        echo "claim: $(q .claim)"
        echo "size: $SIZE bytes ($(q '.size | if . % 1073741824 == 0 then "\(. / 1073741824) GiB" else "~\(. / 1073741824 | floor) GiB" end'))"
        echo "state: $(q .state)$( [ -n "$(q .on)" ] && echo " on $(q .on)")"
        echo "robustness: healthy"
        echo "data engine: $(q .engine)"
        echo "anti-affinity: one replica per node ($(q .anti))"
        echo "trims: none of Longhorn's own — data locality $(q .locality); replica auto-balance: $(q .autobal); no eviction requested on any replica, or its node or disk"
        echo "longhorn settings: storage-over-provisioning-percentage $(q .opct), storage-minimal-available-percentage $(q .mpct)"
        echo "move: replica $REP on $(q .rep.node) (disk $(q .rep.disk))"
        echo "numberOfReplicas: $N -> $((N + 1)) -> $N — the count never below $N, and $REP deleted only after a read of $((N + 1)) healthy replicas"
        echo "targets: the new replica lands on a node holding none, on one of these $(q '.targets | length') disk(s), each with ledger headroom for at least twice the volume ($((2 * SIZE)) bytes): $TARGETS"
        echo "live: each target passes Longhorn v1.11.3's placement test, IsSchedulableToDisk(size, actualSize), and the volume's next growth there by CheckReplicasSizeExpansion's per-disk test — judged at this render and again at the write's; the figures ride stderr, not these bytes"
        if [ "$(q '.short_after | length')" -eq 0 ]; then
            echo "after: every replica's disk admits growth to twice the volume in its ledger; the remaining disks admit up to $(q .after_fit) bytes"
        else
            echo "after: the remaining replicas' disks admit up to $(q .after_fit) bytes, and these are SHORT of twice the volume, so the read-back will say the volume is not yet free to grow: $(q '.short_after | join("; ")')"
        fi
        echo "step 1: kubectl patch volumes.longhorn.io $VOL -n $LH --type=json — test that spec.numberOfReplicas is still $N, replace it with $((N + 1))"
        echo "step 2: wait up to ${REBUILD_S}s for a replica not listed below to read healthy on a target disk; otherwise the count STAYS $((N + 1)), $REP is NOT deleted, and the run says so"
        echo "step 3: kubectl patch volumes.longhorn.io $VOL -n $LH --type=json — test that spec.numberOfReplicas is still $((N + 1)), replace it with $N; then, after ${SETTLE_S}s in which no replica was removed, kubectl delete replicas.longhorn.io $REP -n $LH (lower first: a replica deleted under a count of $((N + 1)) is replenished onto the node it left)"
        echo "step 4: read back for up to ${READBACK_S}s: $REP gone, spec.numberOfReplicas $N, $N healthy replicas on $N distinct nodes including the new one, robustness healthy, and every replica's disk admitting the volume's growth to $((2 * SIZE)) bytes on live figures"
        echo
        echo "== replicas of $VOL ($(q .n)) =="
        q '.replicas[]'
        echo
        echo "== each replica disk, summed as Longhorn's admission webhook sums it =="
        q '.disk_lines[]'
        echo
        echo "== every Longhorn disk: where the new replica can land =="
        q '.verdicts[]'
    } > "$WORK/plan"
    cp "$WORK/facts.json" "$WORK/planned.json"
}

render() {
    read_all
    bounds_volume
    bounds_targets
    write_plan
}

# decide — the decisive read (THE DECISIVE READ, in the header): hold the
# volume to the claim, to the two premises and to the plan's own bounds,
# judge every replica as the source, and set REP only when exactly one is
# eligible. Every other outcome refuses, naming what was read.
decide() {
    local claim nb ne name
    read_all
    bounds_volume
    claim="$(q .claim)"
    [ "$claim" = "$CLAIM_NS/$CLAIM_PVC" ] \
        || refuse "Longhorn volume $VOL is bound to $claim, not $CLAIM_NS/$CLAIM_PVC — the claim asked about and the volume read are not one thing"
    nb="$(q '.blocked | length')"
    case "$nb" in '' | *[!0-9]*) unread "the replica disks short of growth could not be counted ('$nb')" ;; esac
    [ "$nb" -gt 0 ] \
        || refuse "no replica disk is short of the volume's next growth (to $((2 * SIZE)) bytes, by the move plan's own ledger and live sums), so no move would free it; whatever bounds this claim is not a replica's placement: $(q '.disk_lines | join("; ")')"
    [ "$N" -le "$LONGHORN_RETIRE_FLOOR" ] \
        || refuse "$VOL holds $N replicas, above the retirement floor of $LONGHORN_RETIRE_FLOOR (LONGHORN_RETIRE_FLOOR in infra/lib/longhorn-ledger.sh), so retiring a replica on a short disk is as admissible by count as moving it, and which of the two is a person's choice, never a discovery's (design ada8f698 does not rule on it). Short of growth: $(q '.blocked | map(.say) | join("; ")'). Read plan-a-volume-replica-retirement or plan-a-volume-replica-move for the replica named"
    bounds_targets
    q '.names[]' > "$WORK/names" || unread "the volume's replicas could not be listed"
    : > "$WORK/candidates"
    while IFS= read -r name; do
        [[ "$name" =~ $REP_RE ]] && [ "${name%-r-*}" = "$VOL" ] \
            || unread "replica '$name' is not named <volume>-r-<8 hex> for $VOL, so it cannot be an argument of plan-a-volume-replica-move"
        REP="$name"
        judge
        jq -c --arg rep "$name" '
            if .rep == null or (.short_after | type) != "array" or (.after_live | type) != "array"
            then error("no typed judgement") else
            {rep: $rep, node: .rep.node, disk: .rep.disk,
             blocked: ([.blocked[] | select(.names | index($rep) != null)] | length > 0),
             remaining: (.short_after + .after_live)} end' "$F" >> "$WORK/candidates" 2> "$WORK/cand.err" \
            || unread "the move of $name could not be judged: $(tr '\n' ' ' < "$WORK/cand.err")"
    done < "$WORK/names"
    REP=""
    [ -s "$WORK/candidates" ] || unread "volume $VOL listed no replica to judge"
    jq -s -r '[.[] | select(.remaining == []) | .rep] | sort | .[]' "$WORK/candidates" > "$WORK/eligible" \
        || unread "the candidates could not be compared"
    ne="$(grep -c . "$WORK/eligible")"
    if [ "$ne" -eq 0 ]; then
        refuse "$nb replica disks are short of the volume's next growth (to $((2 * SIZE)) bytes), so no single move frees it and none is chosen: $(jq -s -r '[.[] | select(.blocked)] | sort_by(.rep) | map("moving \(.rep) off \(.node) would leave \(.remaining | join("; "))") | join(" | ")' "$WORK/candidates")"
    fi
    # Not reachable while the plan holds one replica per node (then one
    # short disk has one replica, and moving any other leaves it short).
    # Kept because a selector must never fall through to a choice.
    [ "$ne" -eq 1 ] \
        || refuse "$ne replicas are each a move after which every remaining replica disk admits growth, and listing order is not a reason to choose one: $(tr '\n' ' ' < "$WORK/eligible")"
    REP="$(cat "$WORK/eligible")"
    judge
    [ "$(q '.rep != null and ((.short_after + .after_live) == [])')" = true ] \
        || unread "the chosen replica $REP did not judge the same way twice"
    echo "$ME: decisive: $REP on $(q .rep.node) (disk $(q .rep.disk)) is the one replica whose move leaves every remaining replica disk admitting growth to $((2 * SIZE)) bytes — proposed as plan-a-volume-replica-move $VOL $REP. Nothing was moved; the move is move-volume-replica, behind the passkey that signs this plan" >&2
    write_plan
}

if [ "$DECISIVE" -eq 1 ]; then
    decide
    HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
    jq -n --arg vol "$VOL" --arg rep "$REP" --arg ns "$CLAIM_NS" --arg pvc "$CLAIM_PVC" \
        --arg hash "$HASH" --rawfile plan "$WORK/plan" \
        '{verb: "plan-a-volume-replica-move", args: [$vol, $rep], target: [$ns, $pvc],
          plan: $plan, plan_sha256: $hash}' \
        || fail "the proposal could not be encoded — nothing was written"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi


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
    refuse "today's plan for $VOL (above) hashes to $HASH, not the approved plan $APPROVED — its count, its replicas, a setting, a disk's ledger or the disks that can take a replica moved since the plan was signed (or this plan already ran). Nothing was changed; render and approve it again"
fi
cat "$WORK/plan"
echo
UP=$((N + 1))
ORIG="$(jq -c '.names' "$WORK/planned.json")"
TARGETS_JSON="$(jq -c '.targets' "$WORK/planned.json")"

# cas <from> <to> — the compare-and-set on spec.numberOfReplicas.
cas() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K patch volumes.longhorn.io "$VOL" -n "$LH" --type=json --request-timeout=20s \
        -p "[{\"op\":\"test\",\"path\":\"/spec/numberOfReplicas\",\"value\":$1},{\"op\":\"replace\",\"path\":\"/spec/numberOfReplicas\",\"value\":$2}]" \
        > "$WORK/patch.out" 2> "$WORK/patch.err"
}

# snapshot — read the volume, its replicas and the nodes, and judge them
# into $WORK/now.json. Returns 1, with the reason in $WORK/snap.why, when
# a read could not look: a caller about to MUTATE stops on that.
snapshot() {
    local r
    : > "$WORK/snap.why"
    for r in "volumes.longhorn.io $VOL:s-vol.json" "replicas.longhorn.io:s-reps.json" "nodes.longhorn.io:s-nodes.json"; do
        # shellcheck disable=SC2086
        if ! $K get ${r%%:*} -n "$LH" -o json --request-timeout=30s > "$WORK/${r##*:}" 2> "$WORK/snap.err" \
            || ! jq_doc_file "$WORK/${r##*:}"; then
            echo "${r%%:*} unread: $(tr '\n' ' ' < "$WORK/snap.err" | cut -c1-200)" > "$WORK/snap.why"
            return 1
        fi
    done
    jq -n --arg vol "$VOL" --arg rep "$REP" --argjson orig "$ORIG" --argjson targets "$TARGETS_JSON" \
        --arg pinned "${NEW:-}" --arg pinned_node "${NEW_NODE:-}" --arg pinned_disk "${NEW_DISK:-}" \
        --argjson opct "$OPCT" --argjson mpct "$MPCT" --argjson size "$SIZE" \
        --slurpfile v "$WORK/s-vol.json" --slurpfile reps "$WORK/s-reps.json" --slurpfile nodes "$WORK/s-nodes.json" \
        "$LONGHORN_LEDGER_JQ"'
        lh_disks($nodes[0].items) as $all
        | [$reps[0].items[] | select(.spec.volumeName == $vol)] | sort_by(.metadata.name) as $mine
        | [$mine[] | select(lh_healthy)] as $h
        | [$mine[] | select(.metadata.name as $n | $orig | index($n) | not)] as $new
        # CheckReplicasSizeExpansion counts every assigned CR, including
        # failed, inactive and terminating copies (v1.11.3). A replacement
        # does not cancel growth reserved by the retained copy (8f52f401).
        | [$mine[] | select((.spec.nodeID // "") != "")] | group_by(.spec.diskID // "") as $groups
        | [$groups[] | . as $g | ($g[0].spec.diskID // "") as $uuid
            | ([$all[] | select(.uuid == $uuid)] | .[0] // null) as $d
            | {uuid: $uuid, count: ($g | length), d: $d,
               why: (if $d == null then ["no node reports it"]
                     elif any($g[]; .spec.nodeID != $d.node) then ["its assigned node does not report this disk"]
                     else ($d | lh_expansion(($g | length) * $size; $opct; $mpct)) end)}] as $rd
        | {spec: ($v[0].spec.numberOfReplicas // "unreported"),
           robustness: ($v[0].status.robustness // "unreported"),
           names: [$mine[] | .metadata.name],
           rep_present: (($mine | map(.metadata.name) | index($rep)) != null),
           rep_leaving: ([$mine[] | select(.metadata.name == $rep)] | (. == [] or (.[0] | lh_deleting))),
           nhealthy: ($h | length),
           healthy_nodes: ([$h[] | .spec.nodeID] | unique),
           pinned_proven: any($new[]; .metadata.name == $pinned and lh_healthy and
                              .spec.nodeID == $pinned_node and .spec.diskID == $pinned_disk and
                              ((.spec.diskID // "") as $u | $targets | index($u) != null)),
           orig_gone: [$orig[] | . as $n | select(($mine | map(.metadata.name) | index($n)) == null)],
           orig_unwell: [$mine[] | select(.metadata.name as $n | $orig | index($n)) | select(lh_healthy | not) | .metadata.name],
           # Removed or terminating, other than the named replica.
           others_leaving: ([$orig[] | select(. != $rep) | . as $n
                              | select(([$mine[] | select(.metadata.name == $n)] | . == [] or (.[0] | lh_deleting)))]
                            + [$new[] | select(lh_deleting) | .metadata.name]),
           new_healthy: [$new[] | select(lh_healthy) | {name: .metadata.name, node: (.spec.nodeID // "no node"), disk: (.spec.diskID // "none"),
                                                         target: ((.spec.diskID // "") as $u | $targets | index($u) != null)}],
           new_pending: [$new[] | select(lh_healthy | not) | "\(.metadata.name) on \(.spec.nodeID // "no node yet") (disk \(.spec.diskID // "none")): \(if lh_deleting then "being deleted" elif lh_failed then "failed" else "rebuilding, or not yet active" end)"],
           short: [$rd[] | select(.why != []) | "disk \(.uuid)\(if .d == null then "" else " (\(.d.name) on \(.d.node))" end), \(.count) replica(s) growing \($size) each: \(.why | join("; "))"]}' \
        > "$WORK/now.json" 2> "$WORK/snap.err" || {
        echo "Longhorn's answer would not parse: $(tr '\n' ' ' < "$WORK/snap.err" | cut -c1-200)" > "$WORK/snap.why"
        return 1
    }
}
n() { jq -r "$1" "$WORK/now.json"; }

echo "$ME: plan $APPROVED still holds — moving replica $REP of $VOL: numberOfReplicas $N -> $UP"
# --- step 1: raise -------------------------------------------------------------
if ! cas "$N" "$UP"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch volumes.longhorn.io $VOL ($N -> $UP) did not succeed (its words above) — nothing else was done and $REP stands; read the volume again with plan-a-volume-replica-move before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted spec.numberOfReplicas=$UP for $VOL — waiting up to ${REBUILD_S}s for a new replica healthy on a target disk ($(jq -r 'join(", ")' <<<"$TARGETS_JSON"))"

# --- step 2: a new replica, healthy, on a target -------------------------------
start="$(date -u +%s)"
why="not read yet"
NEW=""
while :; do
    if snapshot; then
        if [ "$(n '.new_healthy | length')" -gt 0 ]; then
            if [ "$(n '[.new_healthy[] | select(.target | not)] | length')" -gt 0 ]; then
                fail "a new replica of $VOL read healthy on a disk the plan did not name as a target: $(n '[.new_healthy[] | select(.target | not) | "\(.name) on \(.node) (disk \(.disk))"] | join("; ")') — spec.numberOfReplicas STAYS $UP, and $REP is NOT deleted: a replica that landed off the plan is not a move this approval covered. Read the volume again with plan-a-volume-replica-move"
            fi
            NEW="$(n '.new_healthy[0].name')"
            NEW_NODE="$(n '.new_healthy[0].node')"
            NEW_DISK="$(n '.new_healthy[0].disk')"
            NEW_AT="$NEW_NODE (disk $NEW_DISK)"
            break
        fi
        why="spec.numberOfReplicas $(n .spec), robustness $(n .robustness), healthy on $(n '.healthy_nodes | join(", ")'); new and not yet healthy: $(n 'if .new_pending == [] then "none — Longhorn has not scheduled one" else .new_pending | join(", ") end')"
    else
        why="$(cat "$WORK/snap.why")"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$elapsed" -ge "$REBUILD_S" ]; then
        fail "after ${elapsed}s no new replica of $VOL reads healthy on a target disk — the rebuild STALLED, never started, or is slower than the wait: $why. spec.numberOfReplicas STAYS $UP (more copies, never fewer) and $REP is NOT deleted; Longhorn goes on rebuilding. Read it again with plan-a-volume-replica-move"
    fi
    sleep "$POLL_S"
done

# --- step 3: lower, then delete ------------------------------------------------
snapshot || fail "the read before lowering the count could not look ($(cat "$WORK/snap.why")) — spec.numberOfReplicas STAYS $UP and $REP is NOT deleted"
if [ "$(n .spec)" != "$UP" ] || [ "$(n .nhealthy)" != "$UP" ] || [ "$(n '.healthy_nodes | length')" != "$UP" ] \
    || [ "$(n '.orig_unwell | length')" -ne 0 ] || [ "$(n '.orig_gone | length')" -ne 0 ]; then
    fail "the new replica $NEW is healthy on $NEW_AT, but $VOL does not read $UP healthy replicas on $UP distinct nodes (spec.numberOfReplicas $(n .spec), healthy on $(n '.healthy_nodes | join(", ")'); not healthy: $(n '.orig_unwell | join(", ") | if . == "" then "none" else . end'); gone: $(n '.orig_gone | join(", ") | if . == "" then "none" else . end')) — spec.numberOfReplicas STAYS $(n .spec) and $REP is NOT deleted"
fi
echo "$ME: new replica $NEW is healthy on $NEW_AT — $UP healthy on $(n '.healthy_nodes | join(", ")'); lowering spec.numberOfReplicas to $N, then deleting $REP"
if ! cas "$UP" "$N"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch volumes.longhorn.io $VOL ($UP -> $N) did not succeed (its words above) — spec.numberOfReplicas STAYS $UP and $REP is NOT deleted"
fi
sed 's/^/    /' "$WORK/patch.out"
start="$(date -u +%s)"
gone_already=""
while :; do
    snapshot || fail "a read after lowering the count could not look ($(cat "$WORK/snap.why")) — $REP is NOT deleted; read the volume again with plan-a-volume-replica-move"
    if [ "$(n .rep_leaving)" = true ]; then
        gone_already=yes
        echo "$ME: $REP was removed by another hand after the count was lowered (it is $(if [ "$(n .rep_present)" = true ]; then echo terminating; else echo gone; fi)) — nothing to delete; Longhorn trims nothing itself under this plan's settings"
        break
    fi
    if [ "$(n '.others_leaving | length')" -gt 0 ] || [ "$(n .nhealthy)" != "$UP" ]; then
        fail "after spec.numberOfReplicas was lowered to $N, $VOL no longer holds $UP healthy replicas (removed or terminating: $(n '.others_leaving | join(", ") | if . == "" then "none" else . end'); healthy on $(n '.healthy_nodes | join(", ")')) — a replica that is not $REP left or failed, so $REP is NOT deleted: that would leave fewer than $N. The volume holds $(n .nhealthy) healthy; read it again with plan-a-volume-replica-move"
    fi
    [ $(($(date -u +%s) - start)) -ge "$SETTLE_S" ] && break
    sleep "$POLL_S"
done
if [ -z "$gone_already" ]; then
    # shellcheck disable=SC2086
    if ! $K delete replicas.longhorn.io "$REP" -n "$LH" --wait=false --request-timeout=20s \
            > "$WORK/delete.out" 2> "$WORK/delete.err"; then
        sed 's/^/    /' "$WORK/delete.err" >&2
        fail "kubectl delete replicas.longhorn.io $REP did not succeed (its words above) — spec.numberOfReplicas is $N with $UP healthy replicas, $REP among them; read the volume again with plan-a-volume-replica-move"
    fi
    sed 's/^/    /' "$WORK/delete.out"
    echo "$ME: deleted $REP — reading it back (up to ${READBACK_S}s: $REP gone once its finalizer clears, $N healthy replicas on $N distinct nodes, every replica's disk admitting growth to $((2 * SIZE)) bytes)"
fi

# --- step 4: read back -----------------------------------------------------------
start="$(date -u +%s)"
while :; do
    if snapshot; then
        if [ "$(n .rep_present)" = false ] && [ "$(n .spec)" = "$N" ] && [ "$(n .nhealthy)" = "$N" ] \
            && [ "$(n '.healthy_nodes | length')" = "$N" ] && [ "$(n .robustness)" = healthy ] \
            && [ "$(n .pinned_proven)" = true ]; then
            if [ "$(n '.short | length')" -eq 0 ]; then
                echo "$ME: $VOL has $N healthy replicas on $N distinct nodes ($(n '.healthy_nodes | join(", ")')) — $REP is gone, $NEW is healthy on $NEW_NODE (disk $NEW_DISK), and every replica's disk has room for growth to $((2 * SIZE)) bytes — read back: spec.numberOfReplicas=$N, robustness healthy (after $(($(date -u +%s) - start))s)"
                exit 0
            fi
            fail "$REP is gone and $NEW is healthy on $NEW_AT — $VOL has $N healthy replicas on $N distinct nodes ($(n '.healthy_nodes | join(", ")')), but NOT every replica's disk has room for growth to $((2 * SIZE)) bytes: $(n '.short | join("; ")'). The move stands; that disk is the next to clear"
        fi
        why="$REP $(if [ "$(n .rep_present)" = true ]; then echo "still listed"; else echo gone; fi), spec.numberOfReplicas $(n .spec), robustness $(n .robustness), healthy on $(n '.healthy_nodes | join(", ")'), pinned new replica $NEW healthy on its observed target $NEW_AT: $(n .pinned_proven), replicas $(n '.names | join(", ")'); pending or failed: $(n '.new_pending | join("; ")')"
    else
        why="$(cat "$WORK/snap.why")"
    fi
    elapsed=$(($(date -u +%s) - start))
    if [ "$elapsed" -ge "$READBACK_S" ]; then
        fail "the move of $REP to $NEW on $NEW_AT is NOT proven after ${elapsed}s: $why. Read the volume again with plan-a-volume-replica-move"
    fi
    sleep "$POLL_S"
done
