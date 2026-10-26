#!/usr/bin/env bash
# remove-longhorn-disk.sh — take ONE disk add-longhorn-disk placed back out
# of a node's Longhorn disks, through a rendered plan a passkey signs. The
# ops verbs `plan-a-longhorn-disk-removal` (--plan) and
# `remove-longhorn-disk` (the write).
#
#   remove-longhorn-disk.sh --plan <node> <volume>
#   remove-longhorn-disk.sh <node> <volume> <plan-sha256>
#
# WHY IT EXISTS (backlog b791c5b4). Found 2026-10-19 by run cfaad67f while
# building add-longhorn-disk (item 493a02b6, infra/forge/longhorn-disk.sh):
# that door adds `u-<volume>` at `/var/mnt/<volume>` to a node's
# nodes.longhorn.io spec.disks, and NO door took one back out. A disk that
# never read Ready after the add stayed in spec.disks for good (Longhorn
# schedules nothing to it, and nothing removed it), and a disk that did
# read Ready could be retired only by hand with the admin kubeconfig.
# This is the bounded inverse — the shape of the add, run backwards.
#
# THE SHAPE is design 17835005's, as the add's: the verb file declares
# requires_approval and names plan-a-longhorn-disk-removal as its
# plan_verb, so the runner renders this script's --plan onto the
# request's approve step and hands the write sha256 of the SIGNED plan.
# The write re-renders and patches nothing unless today's plan hashes to
# it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) as a talos-worker, and the cluster agrees — the registry's
#     address as its InternalIP and no control-plane label (the add's
#     rule, cluster-node-lib.sh, before any door opens).
#   * THE DISK IS ONE THE ADD DOOR PLACED: named `u-<volume>` and at
#     exactly `/var/mnt/<volume>` (a trailing slash aside), the name and
#     mount add-longhorn-disk derives from the volume. The packet names a
#     volume, never a disk or a path, so a node's default disk — or any
#     disk an operator made by another name or at another path — cannot
#     be reached through this verb at all.
#   * Longhorn's node reads Ready=True: its diskStatus is what proves the
#     disk holds nothing, and a manager that is down reports a stale one.
#   * THE DISK HOLDS NOTHING, by four readings, refused on ANY one:
#       - no replicas.longhorn.io whose spec.diskID is the disk's diskUUID,
#         or whose spec.nodeID is the node and spec.diskPath the mount or
#         under it (a disk Longhorn never read Ready has no diskUUID, so
#         the node and path are the only name a replica could carry);
#       - Longhorn's status.diskStatus/u-<volume> lists no scheduledReplica
#         and no scheduledBackingImage;
#       - its storageScheduled is zero;
#       - no backingimages.longhorn.io keeps a file on the disk's UUID
#         (spec.diskFileSpecMap, spec.disks or status.diskFileStatusMap).
#     Eviction is NOT done here: a disk that holds a replica is refused,
#     and the replica is moved first by the doors that move replicas
#     (move-volume-replica, retire-volume-replica). Longhorn-driven
#     eviction is the larger option the item names, for a later car.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, role and Node uid, the Longhorn node's disks now (spec
# only), the disk to remove with its diskUUID, what it holds (nothing,
# or no plan), the two patches and the read-back. No clock and no live
# free space in the bytes (it rides stderr). The disks before are IN the
# bytes, so a second run of an applied plan re-renders to a refusal (the
# disk is gone): at most once.
#
# THE WRITE is two `kubectl patch nodes.longhorn.io --type=json`, each a
# compare-and-set (a `test` that spec.disks is exactly what it should be):
#   1. replace spec.disks/u-<volume>/allowScheduling with false — skipped
#      when the plan read it false already. Longhorn's scheduler then
#      places nothing new on the disk, which closes the window between the
#      plan's read and the removal: a replica Longhorn placed in it would
#      otherwise lose its disk.
#   2. after BOSS_LONGHORN_DISK_SETTLE_S (default 15), the holdings are
#      read AGAIN, exactly as the plan read them. Anything now on the disk
#      stops the run (exit 1): the disk stays in spec.disks, unschedulable,
#      and nothing is removed — that state is safe, said in the plan, and
#      undone by moving what landed there and planning again.
#   3. remove spec.disks/u-<volume>, testing the map is the plan's with
#      that one disk unschedulable. Nothing else is written: the Talos
#      user volume stays mounted at /var/mnt/<volume> with whatever files
#      Longhorn left there, and every other disk is untouched.
#
# THE EFFECT IS READ BACK. Every BOSS_LONGHORN_DISK_POLL_S (default 10) up
# to BOSS_LONGHORN_DISK_WAIT_S (default 180, inside the verb's timeout):
# the Longhorn node. Only when spec.disks holds no u-<volume>, every
# other disk's spec reads as the plan did, and Longhorn's status.diskStatus
# no longer reports u-<volume> (its node controller drops a removed
# disk's status — Longhorn's own word that it let go) is the line the
# verb file declares as `effect` printed. Otherwise exit 1 saying the
# removal is NOT proven, with what was read.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=remove-longhorn-disk
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
# The add door's volume grammar (longhorn-disk.sh VOL_RE): one Talos user
# volume name, and so the last segment of /var/mnt/<name>.
VOL_RE='^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$'

# What an unread node means changes once the first patch has landed: a
# bound that no longer holds is then a FAILURE naming the state it left
# (the disk unschedulable), never a refusal that reads as nothing done.
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
    [ $# -eq 2 ] || node_refuse "usage: remove-longhorn-disk.sh --plan <node> <volume> — the plan takes the node and the name of the user volume add-longhorn-disk made a disk of, and nothing else"
else
    [ $# -eq 3 ] || node_refuse "usage: remove-longhorn-disk.sh <node> <volume> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-longhorn-disk-removal verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
NODE="$1"
VOL="$2"
node_check_id "$NODE"
[[ "$VOL" =~ $VOL_RE ]] || node_refuse "the volume must be one Talos user volume name (lowercase letters, digits and inner dashes, as add-longhorn-disk took it), got '$VOL'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing can be read, and no evidence is not a plan"

MNT="/var/mnt/$VOL"
DISK="u-$VOL"
WAIT_S="${BOSS_LONGHORN_DISK_WAIT_S:-180}"
POLL_S="${BOSS_LONGHORN_DISK_POLL_S:-10}"
SETTLE_S="${BOSS_LONGHORN_DISK_SETTLE_S:-15}"
case "${WAIT_S:-empty}${POLL_S:-empty}${SETTLE_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_LONGHORN_DISK_WAIT_S, _POLL_S and _SETTLE_S must be whole seconds, got '$WAIT_S', '$POLL_S' and '$SETTLE_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the registry, the door, the cluster's agreement --------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
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
    [ $? -eq 2 ] && bound_fails "Longhorn runs no node $NODE (nodes.longhorn.io $NODE in $LH is NotFound) — there is no Longhorn disk here to remove"
    [ "$(jq -r '.metadata.name // ""' "$WORK/lh.json")" = "$NODE" ] || unread "nodes.longhorn.io $NODE did not read back as that node"
    jq_doc_file "$WORK/lh.json" && jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1 \
        || unread "nodes.longhorn.io $NODE carries no spec.disks map"
    [ "$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status][0] // "unreported"' "$WORK/lh.json")" = True ] \
        || bound_fails "Longhorn's node $NODE does not read Ready=True ($(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | "\(.status) \(.reason // "")"][0] // "unreported"' "$WORK/lh.json")) — its diskStatus is what proves the disk holds nothing, and a node Longhorn cannot reach reports a stale one"
}

# holdings — everything on the disk, one "; "-joined line into HELD (empty
# when nothing), from the Longhorn node already read plus a fresh read of
# the replicas and the backing images. Each reading is the bound's.
holdings() {
    local uuid
    uuid="$(jq -r --arg k "$DISK" '.status.diskStatus[$k].diskUUID // ""' "$WORK/lh.json")"
    k_get replicas.json replicas.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "replicas.longhorn.io answered NotFound"
    k_get bi.json backingimages.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "backingimages.longhorn.io answered NotFound"
    HELD="$(jq -n -r --arg k "$DISK" --arg m "$MNT" --arg n "$NODE" --arg u "$uuid" \
        --slurpfile lh "$WORK/lh.json" --slurpfile reps "$WORK/replicas.json" --slurpfile bis "$WORK/bi.json" '
        def norm: (. // "" | sub("/+$"; ""));
        ($lh[0].status.diskStatus[$k] // {}) as $st
        | [ ($reps[0].items[]
              | select(($u != "" and (.spec.diskID // "") == $u)
                       or ((.spec.nodeID // "") == $n
                           and ((.spec.diskPath | norm) as $p | $p == $m or ($p | startswith($m + "/")))))
              | "replica \(.metadata.name) of volume \(.spec.volumeName // "unknown") (diskID \(.spec.diskID // "none"), diskPath \(.spec.diskPath // "none"))"),
            (($st.scheduledReplica // {}) | keys[] | "scheduled replica \(.) in Longhorn diskStatus"),
            (($st.scheduledBackingImage // {}) | keys[] | "scheduled backing image \(.) in Longhorn diskStatus"),
            (($st.storageScheduled // 0) | tonumber? // 0 | select(. > 0) | "storageScheduled \(.) bytes"),
            (if $u == "" then empty else
               ($bis[0].items[]
                | select(([(.spec.diskFileSpecMap // {}), (.spec.disks // {}), (.status.diskFileStatusMap // {})]
                          | map(keys) | add | index($u)) != null)
                | "backing image \(.metadata.name) keeps a file on disk \($u)") end)
          ] | join("; ")')" || unread "the replicas, backing images or Longhorn node $NODE would not parse"
}

# --- the plan -----------------------------------------------------------------
render() {
    local path sched uid uuid
    read_node
    jq_doc_file "$WORK/lh.json" && jq -e --arg k "$DISK" '.spec.disks | has($k)' "$WORK/lh.json" > /dev/null \
        || node_refuse "Longhorn's node $NODE holds no Longhorn disk $DISK (its disks: $(jq -r '[.spec.disks | keys[]] | join(", ")' "$WORK/lh.json")) — nothing to remove: it was never placed, or is already gone"
    path="$(jq -r --arg k "$DISK" '.spec.disks[$k].path // ""' "$WORK/lh.json")"
    [ "${path%/}" = "$MNT" ] \
        || node_refuse "Longhorn disk $DISK on $NODE is at '$path', not $MNT — not the shape add-longhorn-disk places (u-<volume> at /var/mnt/<volume>), and this verb removes only that shape"
    holdings
    [ -z "$HELD" ] || node_refuse "Longhorn disk $DISK at $MNT on $NODE holds: $HELD — this verb removes only a disk that holds nothing; move what is on it first (move-volume-replica, retire-volume-replica), then plan again"
    sched="$(jq -r --arg k "$DISK" '.spec.disks[$k].allowScheduling' "$WORK/lh.json")"
    uuid="$(jq -r --arg k "$DISK" '.status.diskStatus[$k].diskUUID // ""' "$WORK/lh.json")"
    jq -c -S '.spec.disks' "$WORK/lh.json" > "$WORK/disks.json"
    jq -c -S --arg k "$DISK" '.spec.disks | .[$k].allowScheduling = false' "$WORK/lh.json" > "$WORK/disks-off.json"

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    {
        echo "plan: remove-longhorn-disk"
        echo "node: $NODE"
        echo "address: $NODE_ADDRESS"
        echo "estate role: $NODE_ROLE"
        echo "kubernetes node uid: $uid"
        echo
        echo "== longhorn node $NODE: its disks now (spec; allowScheduling on the node: $(jq -r '.spec.allowScheduling' "$WORK/lh.json")) =="
        jq -r '.spec.disks | to_entries | sort_by(.key)[]
            | "disk \(.key): path \(.value.path), allowScheduling \(.value.allowScheduling), evictionRequested \(.value.evictionRequested // false), storageReserved \(.value.storageReserved // 0) bytes, tags \(.value.tags // [] | tojson)"' "$WORK/lh.json"
        echo
        echo "disk to remove: $DISK at $MNT — the shape add-longhorn-disk places for user volume $VOL; Longhorn reads it $(if [ -n "$uuid" ]; then echo "diskUUID $uuid"; else echo "with no diskUUID (it never read the disk Ready)"; fi)"
        echo "holds: nothing — no replica with diskID ${uuid:-(none)} or on $NODE at $MNT, no scheduled replica or backing image in Longhorn's diskStatus, storageScheduled 0, no backing image file on it"
        if [ "$sched" = true ]; then
            echo "command 1: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is still exactly the disks above, replace spec.disks/$DISK/allowScheduling with false, so Longhorn places nothing new on it"
        else
            echo "command 1: none — $DISK already reads allowScheduling false"
        fi
        echo "then: read the replicas, the backing images and Longhorn's diskStatus for $DISK again; if anything is on it now, stop — $DISK stays in spec.disks with allowScheduling false, and nothing is removed"
        echo "command 2: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is the disks above with $DISK at allowScheduling false, remove spec.disks/$DISK. Nothing else is written: the Talos volume stays mounted at $MNT with its files, and every other disk is untouched."
        echo "read-back: until spec.disks holds no $DISK, every other disk reads as above, and Longhorn's diskStatus no longer reports $DISK."
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
    node_refuse "today's plan for $DISK on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the node's disks or what is on $DISK moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — removing Longhorn disk $DISK at $MNT from $NODE"

# lh_patch <file-of-ops> <what this run cannot say when it fails>
lh_patch() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K patch nodes.longhorn.io "$NODE" -n "$LH" --type=json -p "$(cat "$1")" --request-timeout=20s \
            > "$WORK/patch.out" 2> "$WORK/patch.err"; then
        sed 's/^/    /' "$WORK/patch.err" >&2
        node_fail "kubectl patch nodes.longhorn.io $NODE did not succeed (its words above) — $2; read the node again with plan-a-longhorn-disk-removal before anything else"
    fi
    sed 's/^/    /' "$WORK/patch.out"
}

if [ "$(jq -r --arg k "$DISK" '.[$k].allowScheduling' "$WORK/disks.json")" = true ]; then
    jq -c -n --slurpfile d "$WORK/disks.json" --arg k "$DISK" \
        '[{op: "test", path: "/spec/disks", value: $d[0]},
          {op: "replace", path: "/spec/disks/\($k)/allowScheduling", value: false}]' > "$WORK/p1.json" \
        || node_fail "could not compose the scheduling patch — nothing was written"
    lh_patch "$WORK/p1.json" "nothing was changed by this run"
    echo "$ME: disk $DISK on $NODE is unschedulable — reading what is on it again after ${SETTLE_S}s, before the removal"
    UNREAD_SAYS="$DISK on $NODE was left at allowScheduling false, and nothing was removed"
    PATCHED=1
    sleep "$SETTLE_S"
else
    UNREAD_SAYS="nothing was written"
fi

# The second read: what Longhorn placed between the plan's read and the
# scheduling patch would lose its disk to the removal.
read_node
holdings
[ -z "$HELD" ] || node_fail "Longhorn disk $DISK at $MNT on $NODE now holds: $HELD. It stays in spec.disks with allowScheduling false, so Longhorn places nothing new there, and nothing was removed — move what is on it, then plan again"

jq -c -n --slurpfile d "$WORK/disks-off.json" --arg k "$DISK" \
    '[{op: "test", path: "/spec/disks", value: $d[0]},
      {op: "remove", path: "/spec/disks/\($k)"}]' > "$WORK/p2.json" \
    || node_fail "could not compose the removal patch — $DISK on $NODE stays, unschedulable"
lh_patch "$WORK/p2.json" "this run cannot say $DISK was removed (it stays at allowScheduling false if it is there)"
echo "$ME: Longhorn accepted the removal of disk $DISK from $NODE — reading it back (up to ${WAIT_S}s: gone from spec.disks and from Longhorn's diskStatus, every other disk unchanged)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
said="not read yet"
while :; do
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        said="$(jq -r --slurpfile before "$WORK/disks.json" --arg k "$DISK" '
            (.spec.disks // {}) as $d
            | ($before[0] | del(.[$k])) as $others
            | if $d | has($k) then "spec.disks still holds \($k) (allowScheduling \($d[$k].allowScheduling))"
              elif $d != $others then "another disk on the node changed: \([$d | to_entries[] | select(.value != $others[.key]) | .key] | join(", ")) against the plan"
              elif (.status.diskStatus // {}) | has($k) then "Longhorn still reports status for \($k)"
              else "OK \($others | length)" end' "$WORK/back.json" 2> "$WORK/back.jq")" \
            || said="the Longhorn node would not parse: $(tr '\n' ' ' < "$WORK/back.jq" | cut -c1-200)"
    else
        said="the Longhorn node did not read: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    elapsed=$(($(date -u +%s) - start))
    case "$said" in
        "OK "*)
            echo "$ME: $NODE no longer has Longhorn disk u-$VOL at /var/mnt/$VOL — read back: gone from spec.disks and from Longhorn's diskStatus, and its ${said#OK } other disk(s) unchanged (after ${elapsed}s)"
            exit 0 ;;
    esac
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "Longhorn accepted the removal of disk $DISK from $NODE, and after ${elapsed}s it is NOT proven: $said — read it with node-disk-usage $NODE"
    fi
    sleep "$POLL_S"
done
