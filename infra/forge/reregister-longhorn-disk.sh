#!/usr/bin/env bash
# reregister-longhorn-disk.sh — re-register ONE worker's Longhorn DEFAULT
# disk whose record no longer matches the disk under it, through a
# rendered plan a passkey signs. The ops verbs
# `plan-a-longhorn-disk-reregistration` (--plan) and
# `reregister-longhorn-disk` (the write).
#
#   reregister-longhorn-disk.sh --plan <node>
#   reregister-longhorn-disk.sh <node> <plan-sha256>
#
# WHY IT EXISTS (backlog 36d2c484). Read 2026-10-21T23:45Z on w-2's
# nodes.longhorn.io: its default disk default-disk-080400000000 at
# /var/lib/longhorn reads Ready=False reason DiskFilesystemChanged
# ("record diskUUID does not match the one on the disk"), Schedulable
# False, storageMaximum 0 — while the node's filesystem is 929 GiB with
# 886 free. The record predates the 1 TB disk that replaced w-2's system
# disk on 2026-10-19 (join-a-node 829f75a8): Talos reinstalled onto the
# new disk, Longhorn wrote a fresh longhorn-disk.cfg there, and the node
# object kept the OLD diskUUID in status.diskStatus, so Longhorn places
# nothing on the largest free disk in the cluster. The longhorn-disk
# doors (add-longhorn-disk, remove-longhorn-disk) act only on u-<volume>
# at /var/mnt/<volume> and refuse the default-disk shape by design, so no
# door reached this record. Longhorn's own repair is to remove the disk
# from spec.disks and add the same path back: a disk added at a path
# that carries a longhorn-disk.cfg is recorded under the UUID the file
# names. This is that repair, bounded.
#
# THE SHAPE is design 17835005's, as the longhorn-disk family's: the verb
# file declares requires_approval and names
# plan-a-longhorn-disk-reregistration as its plan_verb, so the runner
# renders this script's --plan onto the request's approve step and hands
# the write sha256 of the SIGNED plan. The write re-renders and patches
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the one it checks:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) as a talos-worker, and the cluster agrees — the registry's
#     address as its InternalIP and no control-plane label
#     (cluster-node-lib.sh, before any door opens).
#   * THE DISK IS THE NODE'S DEFAULT DISK, derived, never named: exactly
#     one disk in spec.disks sits at /var/lib/longhorn (a trailing slash
#     aside), and its name is Longhorn's default-disk-<hex>. The packet
#     names a node and nothing else, so no u-<volume> disk, and no disk
#     an operator made at another path, can be reached through this verb.
#   * Longhorn's node reads Ready=True: its diskStatus is what proves the
#     disk holds nothing, and a manager that is down reports a stale one.
#   * THE RECORD HAS DRIFTED, by Longhorn's word and the disk's: Longhorn
#     reads the disk Ready=False with reason DiskFilesystemChanged, and
#     /var/lib/longhorn/longhorn-disk.cfg — read from the node through
#     the Talos door (`talosctl read`) — names a diskUUID that is NOT the
#     one the record carries. A disk Longhorn reads Ready, or one whose
#     file agrees with its record, has nothing this repairs; a disk with
#     no such file would be given a NEW UUID by the re-add, which is not
#     the repair the plan promises.
#   * THE RECORD HOLDS NOTHING, by four readings, refused on ANY one
#     (remove-longhorn-disk.sh's readings, over BOTH UUIDs):
#       - no replicas.longhorn.io whose spec.diskID is the record's
#         diskUUID or the file's, or whose spec.nodeID is the node and
#         spec.diskPath is /var/lib/longhorn or under it;
#       - Longhorn's status.diskStatus for the disk lists no
#         scheduledReplica and no scheduledBackingImage;
#       - its storageScheduled is zero;
#       - no backingimages.longhorn.io keeps a file on either UUID.
#     A replica that names the FILE's UUID is refused too: re-adopting the
#     disk would bring it back into Longhorn's books, and that is a choice
#     the plan does not make.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, role and Node uid, the Longhorn node's disks now (spec
# only), the record with Longhorn's reason and its diskUUID, the diskUUID
# the file on the disk carries, what it holds (nothing), the three patches
# and the read-back. No clock and no live free space in the bytes (it
# rides stderr). Once applied, Longhorn reads the disk Ready, so a second
# run of the same approval re-renders to a refusal: at most once.
#
# THE WRITE is three `kubectl patch nodes.longhorn.io --type=json`, each a
# compare-and-set (a `test` that spec.disks is exactly what it should be):
#   1. replace spec.disks/<disk>/allowScheduling with false — skipped when
#      the plan read it false already — so nothing is placed on the record
#      between the plan's read and its removal. After
#      BOSS_LONGHORN_DISK_SETTLE_S (default 15) the holdings are read
#      AGAIN, as the plan read them; anything on it now stops the run
#      (exit 1) with the record unschedulable and nothing removed.
#   2. remove spec.disks/<disk>. Then, every BOSS_LONGHORN_DISK_POLL_S
#      (default 10) up to BOSS_LONGHORN_DISK_WAIT_S (default 300), the
#      node is read until Longhorn's node controller drops its status for
#      the disk — the stale diskUUID lives there, and a record re-added
#      while it stands would keep it.
#   3. add spec.disks/<disk> back: the same name, path, storageReserved,
#      tags and diskType the plan read, with allowScheduling true. The add
#      is tried three times, because between 2 and 3 the node has no
#      default disk record (it placed nothing on the old one either). If
#      Longhorn never dropped the stale status, the record is re-added all
#      the same — which leaves the node as it read before this run — and
#      the run fails saying so.
#   Nothing else is written: the files under /var/lib/longhorn, and every
#   other disk on the node, are untouched.
#
# THE EFFECT IS READ BACK. Every BOSS_LONGHORN_DISK_POLL_S up to
# BOSS_LONGHORN_DISK_WAIT_S: the Longhorn node. Only when spec.disks reads
# as the plan said it would, and Longhorn's own status for the disk reads
# Ready=True and Schedulable=True with the diskUUID the file on the disk
# carries — which no other disk on the node has — and a storageMaximum
# above zero, is the line the verb file declares as `effect` printed.
# Otherwise exit 1 saying the re-registration is NOT proven, with
# Longhorn's own reason.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=reregister-longhorn-disk
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
# Longhorn's default data path on a Talos node, and the file Longhorn
# keeps at the root of every disk it has registered.
MNT=/var/lib/longhorn
CFG="$MNT/longhorn-disk.cfg"
# The name Longhorn gives a node's default disk (default-disk-<hex>).
DISK_RE='^default-disk-[0-9a-f]+$'
UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

# What an unread node means changes once the first patch has landed: a
# bound that no longer holds is then a FAILURE naming the state it left,
# never a refusal that reads as nothing done.
UNREAD_SAYS="an unread node has no plan, and nothing was written"
PATCHED=0
unread() { node_fail "CANNOT ANSWER — $* — $UNREAD_SAYS"; }
bound_fails() {
    [ "$PATCHED" -eq 1 ] && node_fail "$* — $UNREAD_SAYS"
    node_refuse "$*"
}

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 1 ] || node_refuse "usage: reregister-longhorn-disk.sh --plan <node> — the plan takes the node and nothing else; the disk is the node's default disk, derived, never named"
else
    [ $# -eq 2 ] || node_refuse "usage: reregister-longhorn-disk.sh <node> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-longhorn-disk-reregistration verb)"
    [[ "$2" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$2'"
    APPROVED="$2"
fi
NODE="$1"
node_check_id "$NODE"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing can be read, and no evidence is not a plan"

WAIT_S="${BOSS_LONGHORN_DISK_WAIT_S:-300}"
POLL_S="${BOSS_LONGHORN_DISK_POLL_S:-10}"
SETTLE_S="${BOSS_LONGHORN_DISK_SETTLE_S:-15}"
case "${WAIT_S:-empty}${POLL_S:-empty}${SETTLE_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_LONGHORN_DISK_WAIT_S, _POLL_S and _SETTLE_S must be whole seconds, got '$WAIT_S', '$POLL_S' and '$SETTLE_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the registry, the doors, the cluster's agreement -------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
T="$(ops_talosctl)"
node_read_cluster "$NODE"

# k_get <file> <args…> — one kubectl get into $WORK/<file>, read as a
# document or CANNOT ANSWER. NotFound is returned as status 2.
k_get() {
    local f="$1"
    shift
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get "$@" -o json --request-timeout=30s > "$WORK/$f" 2> "$WORK/$f.err"; then
        if grep -q 'NotFound' "$WORK/$f.err"; then
            return 2
        fi
        unread "kubectl get $*: $(tr '\n' ' ' < "$WORK/$f.err" | cut -c1-300)"
    fi
    jq_doc_file "$WORK/$f" || unread "kubectl get $* answered no document"
}

# read_node — the Longhorn node into $WORK/lh.json, judged: present, a
# spec.disks map, Ready=True. The plan's read and the write's second read.
read_node() {
    k_get lh.json nodes.longhorn.io "$NODE" -n "$LH"
    [ $? -eq 2 ] && bound_fails "Longhorn runs no node $NODE (nodes.longhorn.io $NODE in $LH is NotFound) — there is no Longhorn disk here to re-register"
    [ "$(jq -r '.metadata.name // ""' "$WORK/lh.json")" = "$NODE" ] || unread "nodes.longhorn.io $NODE did not read back as that node"
    jq_doc_file "$WORK/lh.json" && jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1 \
        || unread "nodes.longhorn.io $NODE carries no spec.disks map"
    [ "$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status][0] // "unreported"' "$WORK/lh.json")" = True ] \
        || bound_fails "Longhorn's node $NODE does not read Ready=True ($(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | "\(.status) \(.reason // "")"][0] // "unreported"' "$WORK/lh.json")) — its diskStatus is what proves the record holds nothing, and a node Longhorn cannot reach reports a stale one"
}

# read_cfg — the longhorn-disk.cfg at the root of the default disk, from
# the node itself through the Talos door, into DISK_UUID. A file that is
# not there is a refusal (the re-add would mint a new UUID); a read that
# could not look cannot answer.
read_cfg() {
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" read "$CFG" < /dev/null > "$WORK/cfg.raw" 2> "$WORK/cfg.err"; then
        grep -qi 'no such file' "$WORK/cfg.err" \
            && node_refuse "$NODE carries no $CFG (Talos: $(tr '\n' ' ' < "$WORK/cfg.err" | cut -c1-200)) — a disk re-added without one is given a NEW diskUUID, which is not the drift this verb repairs"
        unread "talosctl read $CFG on $NODE: $(tr '\n' ' ' < "$WORK/cfg.err" | cut -c1-300)"
    fi
    jq_doc_file "$WORK/cfg.raw" || unread "$CFG on $NODE did not read back as one JSON document, as Longhorn writes it"
    DISK_UUID="$(jq -r '.diskUUID // ""' "$WORK/cfg.raw" 2> "$WORK/cfg.jq")" \
        || unread "$CFG on $NODE is not the JSON Longhorn writes: $(tr '\n' ' ' < "$WORK/cfg.jq" | cut -c1-200)"
    [[ "$DISK_UUID" =~ $UUID_RE ]] || unread "$CFG on $NODE names no diskUUID Longhorn could adopt ('$DISK_UUID')"
}

# holdings — everything that names the record or the disk under it, one
# "; "-joined line into HELD (empty when nothing), from the Longhorn node
# already read plus a fresh read of the replicas and the backing images.
# Each reading is the bound's.
holdings() {
    k_get replicas.json replicas.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "replicas.longhorn.io answered NotFound"
    k_get bi.json backingimages.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "backingimages.longhorn.io answered NotFound"
    HELD="$(jq -n -r --arg k "$DISK" --arg m "$MNT" --arg n "$NODE" --arg u "$RECORD_UUID" --arg v "$DISK_UUID" \
        --slurpfile lh "$WORK/lh.json" --slurpfile reps "$WORK/replicas.json" --slurpfile bis "$WORK/bi.json" '
        def norm: (. // "" | sub("/+$"; ""));
        def names($id): $id != "" and ($id == $v or ($u != "" and $id == $u));
        ($lh[0].status.diskStatus[$k] // {}) as $st
        | [ ($reps[0].items[]
              | select(names(.spec.diskID // "")
                       or ((.spec.nodeID // "") == $n
                           and ((.spec.diskPath | norm) as $p | $p == $m or ($p | startswith($m + "/")))))
              | "replica \(.metadata.name) of volume \(.spec.volumeName // "unknown") (diskID \(.spec.diskID // "none"), diskPath \(.spec.diskPath // "none"))"),
            (($st.scheduledReplica // {}) | keys[] | "scheduled replica \(.) in Longhorn diskStatus"),
            (($st.scheduledBackingImage // {}) | keys[] | "scheduled backing image \(.) in Longhorn diskStatus"),
            (($st.storageScheduled // 0) | tonumber? // 0 | select(. > 0) | "storageScheduled \(.) bytes"),
            ($bis[0].items[]
             | . as $b
             | ([(.spec.diskFileSpecMap // {}), (.spec.disks // {}), (.status.diskFileStatusMap // {})]
                | map(keys) | add | map(select(names(.)))) as $hit
             | select($hit != [])
             | "backing image \($b.metadata.name) keeps a file on disk \($hit | join(", "))")
          ] | join("; ")')" || unread "the replicas, backing images or Longhorn node $NODE would not parse"
}

# --- the plan -----------------------------------------------------------------
render() {
    local uid ready reason sched
    read_node
    # The default disk, derived: every disk at the default path, by any name.
    DISK="$(jq -r --arg m "$MNT" '[.spec.disks | to_entries[] | select((.value.path // "" | sub("/+$"; "")) == $m) | .key] | join(" ")' "$WORK/lh.json")"
    case "$DISK" in
        "") node_refuse "Longhorn's node $NODE has no disk at $MNT (its disks: $(jq -r '[.spec.disks | keys[]] | join(", ")' "$WORK/lh.json")) — there is no default disk record to re-register" ;;
        *" "*) node_refuse "Longhorn's node $NODE has more than one disk at $MNT ($DISK) — this verb re-registers exactly one default disk, and which record is the stale one is not a choice it makes" ;;
    esac
    [[ "$DISK" =~ $DISK_RE ]] \
        || node_refuse "the disk at $MNT on $NODE is named '$DISK', not Longhorn's default-disk-<hex> — this verb re-registers only the default disk Longhorn made"
    jq_doc_file "$WORK/lh.json" && jq -e --arg k "$DISK" '.status.diskStatus | has($k)' "$WORK/lh.json" > /dev/null 2>&1 \
        || node_refuse "Longhorn reports no status for $DISK on $NODE — there is no record to compare with the disk; read it with node-disk-usage $NODE"
    ready="$(jq -r --arg k "$DISK" '[(.status.diskStatus[$k].conditions // [])[] | select(.type == "Ready") | .status][0] // "unreported"' "$WORK/lh.json")"
    reason="$(jq -r --arg k "$DISK" '[(.status.diskStatus[$k].conditions // [])[] | select(.type == "Ready") | .reason // ""][0] // ""' "$WORK/lh.json")"
    [ "$ready" = False ] && [ "$reason" = DiskFilesystemChanged ] \
        || node_refuse "Longhorn reads $DISK on $NODE Ready=$ready${reason:+ ($reason)}, not Ready=False (DiskFilesystemChanged) — this verb re-registers only a record Longhorn says no longer matches its disk"
    RECORD_UUID="$(jq -r --arg k "$DISK" '.status.diskStatus[$k].diskUUID // ""' "$WORK/lh.json")"
    read_cfg
    [ "$DISK_UUID" != "$RECORD_UUID" ] \
        || node_refuse "$CFG on $NODE names diskUUID $DISK_UUID, the one Longhorn's record carries — nothing drifted that a re-registration repairs, so Longhorn's DiskFilesystemChanged has another cause; read it with node-disk-usage $NODE"
    holdings
    [ -z "$HELD" ] || node_refuse "Longhorn disk $DISK at $MNT on $NODE is named by: $HELD — this verb re-registers only a record that holds nothing; move what is on it first (move-volume-replica, retire-volume-replica), then plan again"
    sched="$(jq -r --arg k "$DISK" '.spec.disks[$k].allowScheduling' "$WORK/lh.json")"
    jq -c -S '.spec.disks' "$WORK/lh.json" > "$WORK/disks.json"
    jq -c -S --arg k "$DISK" '.spec.disks | .[$k].allowScheduling = false' "$WORK/lh.json" > "$WORK/disks-off.json"
    jq -c -S --arg k "$DISK" '.spec.disks | del(.[$k])' "$WORK/lh.json" > "$WORK/disks-gone.json"
    jq -c -S --arg k "$DISK" '.spec.disks[$k] | .allowScheduling = true' "$WORK/lh.json" > "$WORK/record.json"
    jq -c -S --arg k "$DISK" '.spec.disks | .[$k].allowScheduling = true' "$WORK/lh.json" > "$WORK/disks-after.json"

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    {
        echo "plan: reregister-longhorn-disk"
        echo "node: $NODE"
        echo "address: $NODE_ADDRESS"
        echo "estate role: $NODE_ROLE"
        echo "kubernetes node uid: $uid"
        echo
        echo "== longhorn node $NODE: its disks now (spec; allowScheduling on the node: $(jq -r '.spec.allowScheduling' "$WORK/lh.json")) =="
        jq -r '.spec.disks | to_entries | sort_by(.key)[]
            | "disk \(.key): path \(.value.path), allowScheduling \(.value.allowScheduling), evictionRequested \(.value.evictionRequested // false), storageReserved \(.value.storageReserved // 0) bytes, tags \(.value.tags // [] | tojson)"' "$WORK/lh.json"
        echo
        echo "record: $DISK at $MNT — the node's default disk; Longhorn reads it Ready=False (DiskFilesystemChanged) under recorded diskUUID ${RECORD_UUID:-(none)}"
        echo "on the disk: $CFG (read from $NODE through the Talos door) names diskUUID $DISK_UUID — not the record's, so Longhorn places nothing on the disk under it"
        echo "holds: nothing — no replica with diskID ${RECORD_UUID:-(none)} or $DISK_UUID or on $NODE at $MNT, no scheduled replica or backing image in Longhorn's diskStatus, storageScheduled 0, no backing image file on either UUID"
        if [ "$sched" = true ]; then
            echo "command 1: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is still exactly the disks above, replace spec.disks/$DISK/allowScheduling with false, so Longhorn places nothing on the record before it goes"
        else
            echo "command 1: none — $DISK already reads allowScheduling false"
        fi
        echo "then: read the replicas, the backing images and Longhorn's diskStatus for $DISK again; if anything names it now, stop — $DISK stays in spec.disks with allowScheduling false, and nothing is removed"
        echo "command 2: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is the disks above with $DISK at allowScheduling false, remove spec.disks/$DISK; then read the node until Longhorn drops its status for $DISK, where the stale diskUUID ${RECORD_UUID:-(none)} lives"
        echo "command 3: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is the disks above without $DISK, add spec.disks/$DISK = $(cat "$WORK/record.json") — the same name and path, schedulable, so Longhorn reads $CFG and records diskUUID $DISK_UUID. Tried three times; until it lands $NODE has no default disk record. If Longhorn has not dropped the stale status, the record is re-added all the same, which leaves $NODE as it reads now, and the run fails."
        echo "placement: once Longhorn reads $DISK Ready, it may place a replica of any volume on it by its own rules, as it did before the record drifted; no node's allowScheduling changes. Nothing else is written: the files under $MNT and every other disk are untouched."
        echo "read-back: until spec.disks reads as the disks above with $DISK at allowScheduling true, and Longhorn's status for $DISK reads Ready=True and Schedulable=True with diskUUID $DISK_UUID and a storageMaximum above zero."
        echo "disks before (the first compare-and-set's value): $(cat "$WORK/disks.json")"
    } > "$WORK/plan"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
jq -r --arg me "$ME" '.status.diskStatus // {} | to_entries[] | "\($me): live free space, not signed: disk \(.key) storageAvailable \(.value.storageAvailable // "unreported") bytes of \(.value.storageMaximum // "unreported")"' "$WORK/lh.json" >&2

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    node_refuse "today's plan for $DISK on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the node's disks, the record, the file on the disk or what names them moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — re-registering Longhorn disk $DISK at $MNT on $NODE (recorded diskUUID ${RECORD_UUID:-none}, on the disk $DISK_UUID)"

# lh_patch <file-of-ops> — one compare-and-set patch; the error words in
# $WORK/patch.err on failure.
lh_patch() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K patch nodes.longhorn.io "$NODE" -n "$LH" --type=json -p "$(cat "$1")" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err" || return 1
    sed 's/^/    /' "$WORK/patch.out"
}
# lh_patch_or_fail <file-of-ops> <what this run cannot say when it fails>
lh_patch_or_fail() {
    lh_patch "$1" && return 0
    sed 's/^/    /' "$WORK/patch.err" >&2
    node_fail "kubectl patch nodes.longhorn.io $NODE did not succeed (its words above) — $2; read the node again with plan-a-longhorn-disk-reregistration before anything else"
}

if [ "$(jq -r --arg k "$DISK" '.[$k].allowScheduling' "$WORK/disks.json")" = true ]; then
    jq -c -n --slurpfile d "$WORK/disks.json" --arg k "$DISK" \
        '[{op: "test", path: "/spec/disks", value: $d[0]},
          {op: "replace", path: "/spec/disks/\($k)/allowScheduling", value: false}]' > "$WORK/p1.json" \
        || node_fail "could not compose the scheduling patch — nothing was written"
    lh_patch_or_fail "$WORK/p1.json" "nothing was changed by this run"
    echo "$ME: record $DISK on $NODE is unschedulable — reading what names it again after ${SETTLE_S}s, before the removal"
    UNREAD_SAYS="$DISK on $NODE was left at allowScheduling false, and nothing was removed"
    PATCHED=1
    sleep "$SETTLE_S"
else
    UNREAD_SAYS="nothing was written"
fi

# The second read: what Longhorn placed between the plan's read and the
# scheduling patch would lose its record to the removal.
read_node
holdings
[ -z "$HELD" ] || node_fail "Longhorn disk $DISK at $MNT on $NODE is now named by: $HELD. It stays in spec.disks with allowScheduling false, so Longhorn places nothing new there, and nothing was removed — move what is on it, then plan again"

jq -c -n --slurpfile d "$WORK/disks-off.json" --arg k "$DISK" \
    '[{op: "test", path: "/spec/disks", value: $d[0]},
      {op: "remove", path: "/spec/disks/\($k)"}]' > "$WORK/p2.json" \
    || node_fail "could not compose the removal patch — $DISK on $NODE stays, unschedulable"
lh_patch_or_fail "$WORK/p2.json" "this run cannot say $DISK was removed (it stays at allowScheduling false if it is there), and nothing was re-added"
echo "$ME: Longhorn accepted the removal of record $DISK from $NODE — waiting up to ${WAIT_S}s for Longhorn to drop its status for $DISK before the record is added back"

# --- the stale status goes ----------------------------------------------------
start="$(date -u +%s)"
said="not read yet"
while :; do
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/gone.json" 2> "$WORK/gone.err" \
        && jq_doc_file "$WORK/gone.json"; then
        said="$(jq -r --slurpfile others "$WORK/disks-gone.json" --arg k "$DISK" '
            (.spec.disks // {}) as $d
            | if $d | has($k) then "spec.disks still holds \($k) (allowScheduling \($d[$k].allowScheduling))"
              elif $d != $others[0] then "another disk on the node changed: \([$d | to_entries[] | select(.value != $others[0][.key]) | .key] | join(", ")) against the plan"
              elif (.status.diskStatus // {}) | has($k) then "Longhorn still reports status for \($k) (diskUUID \(.status.diskStatus[$k].diskUUID // "none"))"
              else "OK" end' "$WORK/gone.json" 2> "$WORK/gone.jq")" \
            || said="the Longhorn node would not parse: $(tr '\n' ' ' < "$WORK/gone.jq" | cut -c1-200)"
    else
        said="the Longhorn node did not read: $(tr '\n' ' ' < "$WORK/gone.err" | cut -c1-200)"
    fi
    [ "$said" = OK ] && break
    elapsed=$(($(date -u +%s) - start))
    [ "$elapsed" -ge "$WAIT_S" ] && break
    sleep "$POLL_S"
done
# Only a stale status may still stand here: the add's compare-and-set is
# the disks above without the record, so anything else would refuse it.
case "$said" in
    OK | "Longhorn still reports status"*) ;;
    *) node_fail "Longhorn accepted the removal of record $DISK from $NODE, and after $(($(date -u +%s) - start))s the node does not read as the plan said it would: $said. Nothing was re-added — read it with plan-a-longhorn-disk-reregistration $NODE and node-disk-usage $NODE" ;;
esac
STALE="$said"

# --- the record comes back ----------------------------------------------------
jq -c -n --slurpfile d "$WORK/disks-gone.json" --slurpfile r "$WORK/record.json" --arg k "$DISK" \
    '[{op: "test", path: "/spec/disks", value: $d[0]},
      {op: "add", path: "/spec/disks/\($k)", value: $r[0]}]' > "$WORK/p3.json" \
    || node_fail "could not compose the re-add patch — $DISK was removed from $NODE and is NOT back; its record is in the plan above"
added=0
for attempt in 1 2 3; do
    if lh_patch "$WORK/p3.json"; then
        added=1
        break
    fi
    sed 's/^/    /' "$WORK/patch.err" >&2
    echo "$ME: the re-add of $DISK on $NODE was refused (attempt $attempt of 3)" >&2
    [ "$attempt" -lt 3 ] && sleep "$POLL_S"
done
[ "$added" -eq 1 ] \
    || node_fail "the stale record $DISK was removed from $NODE and could NOT be added back (Longhorn's words above, three attempts) — $NODE has no default disk record now, and placed nothing on the old one either. The record to add is in the plan above"
if [ "$STALE" != OK ]; then
    node_fail "Longhorn accepted the removal of record $DISK from $NODE, and after ${WAIT_S}s it still read: $STALE. The record was added back all the same, so $NODE reads as it did before this run — the re-registration is NOT proven; read it with node-disk-usage $NODE"
fi
echo "$ME: Longhorn accepted record $DISK back on $NODE at $MNT — reading it back (up to ${WAIT_S}s: the disks as planned, and Longhorn's Ready, Schedulable and diskUUID for $DISK)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
said="not read yet"
while :; do
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        said="$(jq -r --slurpfile after "$WORK/disks-after.json" --arg k "$DISK" --arg v "$DISK_UUID" '
            def cond($t): [(. // [])[] | select(.type == $t)][0] // {status: "unreported"};
            (.spec.disks // {}) as $d
            | (.status.diskStatus // {}) as $s
            | ($s[$k] // null) as $st
            | ([$s | to_entries[] | select(.key != $k) | .value.diskUUID // empty]) as $others
            | if ($d | del(.[$k])) != ($after[0] | del(.[$k])) then "another disk on the node changed: \([$d | del(.[$k]) | to_entries[] | select(.value != $after[0][.key]) | .key] | join(", ")) against the plan"
              elif $d[$k] != $after[0][$k] then "spec.disks/\($k) reads \($d[$k] // "nothing" | tojson), not the planned record"
              elif $st == null then "Longhorn reports no status for \($k) yet"
              else ($st.conditions | cond("Ready")) as $r | ($st.conditions | cond("Schedulable")) as $sc
                | if $r.status != "True" or $sc.status != "True"
                  then "Longhorn reads \($k) Ready=\($r.status) (\($r.reason // "") \($r.message // "")) and Schedulable=\($sc.status) (\($sc.reason // ""))"
                  elif ($st.diskUUID // "") != $v then "Longhorn records \($k) under diskUUID \($st.diskUUID // "none"), not \($v), the one on the disk"
                  elif ($others | index($v)) != null then "Longhorn gives \($k) diskUUID \($v), which another disk on the node already has — one filesystem counted twice"
                  elif (($st.storageMaximum // 0) | tonumber? // 0) <= 0 then "Longhorn reads \($k) with no storageMaximum"
                  else "OK \($v) \($st.storageMaximum) \([$d | keys[] | select(. != $k)] | length)" end
              end' "$WORK/back.json" 2> "$WORK/back.jq")" || said="the Longhorn node would not parse: $(tr '\n' ' ' < "$WORK/back.jq" | cut -c1-200)"
    else
        said="the Longhorn node did not read: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    elapsed=$(($(date -u +%s) - start))
    case "$said" in
        "OK "*)
            # shellcheck disable=SC2086 # four words the jq above printed: OK, uuid, maximum, count
            set -- $said
            echo "$ME: $NODE has Longhorn disk default-disk-${DISK#default-disk-} at /var/lib/longhorn re-registered — read back: Ready=True, Schedulable=True, diskUUID $2 (the one on the disk), storageMaximum $3 bytes, and its $4 other disk(s) unchanged (after ${elapsed}s)"
            exit 0 ;;
    esac
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "Longhorn accepted record $DISK back on $NODE, and after ${elapsed}s it is NOT proven: $said. The record stands — read it with node-disk-usage $NODE"
    fi
    sleep "$POLL_S"
done
