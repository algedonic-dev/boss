#!/usr/bin/env bash
# wipe-a-partition.sh — drop ONE Talos user-volume partition on ONE
# worker, after the volume that owned it is gone, through a rendered plan
# a passkey signs. The ops verbs `plan-a-partition-wipe` (--plan) and
# `wipe-a-partition` (the write).
#
#   wipe-a-partition.sh --plan <node> <partition>
#   wipe-a-partition.sh <node> <partition> <plan-sha256>
#
# WHY IT EXISTS (backlog c98e3425; David's steer on 88b35891, ordered
# 2026-11-07 ~02:00Z). Moving w-1's gate volume off the Kingston onto a
# Micron 7400 without a reboot is a chain of doors: retire-a-user-volume
# deletes the live UserVolumeConfig (Talos unmounts /var/mnt/gate), THIS
# verb drops the u-gate partition — Talos will not provision a user
# volume anew while a partition already carries its label — then
# node-converge applies the then-ABSENT entry and Talos provisions it by
# the new selector. node-converge.sh refuses a DRIFT, so there is no
# other way to move a provisioned volume. Until this verb the drop was
# `talosctl wipe disk` typed by hand with the admin talosconfig.
#
# IT IS THE MOST DESTRUCTIVE VERB AFTER commission-a-disk, so it is
# shaped on that one and on shutdown-node: the WRONG TARGET is made
# unrepresentable rather than disapproved (design 17835005), and every
# precondition is evaluated IMMEDIATELY BEFORE the act, never once at
# plan time. The kernel name is all a packet supplies, and kernel names
# move when a disk is added (the forge, 2026-09-21), so the plan names
# the partition by what Talos says it IS — the parent disk's model,
# serial and wwid, its size, its partition label, index and uuid, its
# filesystem and that filesystem's uuid — and the passkey signs those
# bytes. A disk that renumbered under the same name hashes differently.
#
# THE BOUND, every line a refusal (exit 78, nothing done):
#   * the node is a `talos-worker` in the estate registry and the cluster
#     agrees (cluster-node-lib.sh) — a control plane is never touched;
#   * the name is one NVMe partition's kernel name, ^nvme[0-9]+n[0-9]+p[0-9]+$
#     — never a whole disk, a path, or two names (talosctl takes a list);
#   * its disk is not the system disk — neither the disk Talos's
#     `systemdisk` names nor the one EPHEMERAL lives on;
#   * Talos discovers it, as a partition of that disk, labelled u-<name>
#     (a user volume's — EFI, META, STATE and EPHEMERAL never qualify);
#   * that disk carries NO OTHER partition Talos discovers, so only a
#     whole disk's single user partition can go;
#   * no volume Talos holds still owns it — none named its label, located
#     on it, or living anywhere on its disk (volumestatus);
#   * nothing is mounted from the disk or any partition of it (`talosctl
#     mounts` — a pod still bind-mounting the old volume shows up here);
#   * /var/mnt/<name> is not a Longhorn disk on the node (nodes.longhorn.io
#     spec.disks — a Longhorn disk there would lose every replica on it).
#
# EVERY READ FAILS CLOSED (exit 1, nothing done): a talosctl or kubectl
# read that cannot look, a mounts listing with no device in it at all, a
# Longhorn node that does not answer. An unread fact is never "none".
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) carries no clock,
# no resource version and no usage figure, so two renders of one true
# state are byte-identical and any moved fact moves the bytes.
#
# THE WRITE re-renders, refuses (exit 78, naming both hashes) unless
# today's plan hashes to the approved one, prints the plan (the capture
# before the act), and runs exactly
#     talosctl -n <address> wipe disk <partition> --drop-partition
# through the ops_talosctl door (the default FAST method, which clears
# the partition's signatures; --drop-partition then removes its entry
# from the table). The subcommand and the flag were read from the
# pinned client's own help: `talosctl wipe disk --help` of the v1.13.8
# talosctl-linux-amd64 that install-cluster-operator.sh pins by sha256
# (406b56f9…), checked against that sha in an uncredentialed build
# worker (run c440c2f1, 2026-11-07) — "Wipe a block device (disk or
# partition) which is not used as a volume", usage `talosctl wipe disk
# <device names>... [flags]`, `--drop-partition  drop partition after
# wipe (if applicable)`, `--method [FAST ZEROES] (default "FAST")`. The
# same binary carries, as compiled struct tags, every field name this
# script reads (diskID, parent_dev_path, partition_label,
# partition_index, partition_uuid, parentLocation, serial, wwid…).
# Talos refuses on its own a device a volume still uses.
#
# THE EFFECT IS READ BACK. Every BOSS_WIPE_POLL_S (default 5) up to
# BOSS_WIPE_WAIT_S (default 120), Talos's discoveredvolumes and disks:
# only when the partition is no longer discovered, no partition on its
# disk carries its index or its partition uuid, and the disk itself is
# still there under the same serial, is the line the verb file declares
# as `effect` printed. Otherwise exit 1, saying it is NOT proven gone.
# A second run of an applied plan re-renders to a refusal (the partition
# is not discovered), so no approval is spent twice.
set -uo pipefail

ME=wipe-a-partition
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document,
# so every `jq -e` here first asks whether there was one. Without it an
# empty Longhorn answer read as "no Longhorn disk" (backlog d96e38ab;
# pinned by a_longhorn_read_that_answers_silence_is_not_a_clean_read).
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

PART_RE='^nvme[0-9]+n[0-9]+p[0-9]+$'
LABEL_RE='^u-[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
LH=longhorn-system

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || node_refuse "usage: wipe-a-partition.sh --plan <node> <partition> — the plan takes the node and one partition, nothing else"
else
    [ $# -eq 3 ] || node_refuse "usage: wipe-a-partition.sh <node> <partition> <plan-sha256> — this wipes only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-partition-wipe verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 lowercase hex characters, got '$3'"
    APPROVED="$3"
fi
NODE="$1"
PART="$2"
node_check_id "$NODE"
[[ "$PART" =~ $PART_RE ]] \
    || node_refuse "the partition must be one NVMe partition's kernel name, matching $PART_RE (e.g. nvme1n1p1) — got '$PART'. A whole disk, a path or a list is never wiped through this door"
PARENT="${PART%p*}"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing this verb judges can be read, and no evidence is not a pass"

WAIT_S="${BOSS_WIPE_WAIT_S:-120}"
POLL_S="${BOSS_WIPE_POLL_S:-5}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_WIPE_WAIT_S and BOSS_WIPE_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# talos_list <resource> <file> — every <resource> of the node, as one
# JSON array in $WORK/<file>, or exit 1 naming the read. Only the
# answers of this node's address are kept (talosctl prints `node`).
talos_list() {
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" get "$1" -o json < /dev/null > "$WORK/$2.raw" 2> "$WORK/$2.err"; then
        node_fail "talosctl -n $NODE_ADDRESS get $1 could not read: $(tr '\n' ' ' < "$WORK/$2.err" | cut -c1-300) — an unread fact is not 'none', so there is no plan"
    fi
    jq -s -c --arg a "$NODE_ADDRESS" '[.[] | select(.node == $a)]' "$WORK/$2.raw" > "$WORK/$2.json" 2> "$WORK/$2.jq" \
        || node_fail "Talos's $1 on $NODE did not read back as a resource list ($(tr '\n' ' ' < "$WORK/$2.jq" | cut -c1-200))"
}

# --- the plan ---------------------------------------------------------------
render() {
    node_read_cluster "$NODE"
    local uid sysid ephpar n ptype ppath pdev label name mnt fs fsuuid psize ppretty pidx puuid
    local others owners hits ndev lhdisks lhhit model serial wwid dsize dpretty transport
    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"

    # The system disk, two ways: Talos's own systemdisk, and where
    # EPHEMERAL lives. Either one is a disk this verb never touches.
    talos_list systemdisk sys
    sysid="$(jq -r '[.[] | select(.metadata.id == "system-disk") | (.spec.diskID // .spec.disk_id // "")] | if length == 1 then .[0] else "" end' "$WORK/sys.json")"
    [[ "$sysid" =~ ^[a-z0-9]+$ ]] || node_fail "Talos's systemdisk on $NODE names no disk ('$sysid') — the system disk cannot be ruled out, so there is no plan"
    talos_list volumestatus vs
    ephpar="$(jq -r '[.[] | select(.metadata.id == "EPHEMERAL") | (.spec.parentLocation // "")] | .[0] // ""' "$WORK/vs.json")"
    [[ "$ephpar" == /dev/* ]] || node_fail "Talos's volumestatus on $NODE names no disk under EPHEMERAL ('$ephpar') — a volume list without the node's own data volume is not a read of this node"
    [ "$PARENT" != "$sysid" ] \
        || node_refuse "$PART is a partition of $PARENT, the system disk Talos's systemdisk names — EFI, BOOT, META, STATE and EPHEMERAL live there, and this verb never touches it"
    [ "/dev/$PARENT" != "$ephpar" ] \
        || node_refuse "$PART is a partition of $PARENT, the disk EPHEMERAL lives on ($ephpar) — the kubelet, containerd and every pod's scratch live there, and this verb never touches it"

    # The partition, as Talos discovers it.
    talos_list discoveredvolumes dv
    jq_doc_file "$WORK/dv.json" && jq -e --arg s "$sysid" 'any(.[]; .metadata.id == $s)' "$WORK/dv.json" > /dev/null \
        || node_fail "Talos's discoveredvolumes on $NODE does not list the system disk $sysid — a discovery that misses the disk the node boots from is not a read of this node"
    n="$(jq --arg p "$PART" '[.[] | select(.metadata.id == $p)] | length' "$WORK/dv.json")"
    [ "$n" != 0 ] || node_refuse "Talos discovers no partition $PART on $NODE (talosctl get discoveredvolumes) — nothing to wipe, or it is already gone"
    [ "$n" = 1 ] || node_fail "Talos discovers $n volumes named $PART on $NODE — one name, one device, or nothing is done"
    jq -c --arg p "$PART" '.[] | select(.metadata.id == $p) | .spec' "$WORK/dv.json" > "$WORK/part.json"
    ptype="$(jq -r '.type // ""' "$WORK/part.json")"
    [ "$ptype" = partition ] || node_refuse "Talos discovers $PART on $NODE as type '$ptype', not a partition"
    ppath="$(jq -r '.parent_dev_path // (if (.parent // "") == "" then "" else "/dev/" + .parent end)' "$WORK/part.json")"
    [ "$ppath" = "/dev/$PARENT" ] || node_refuse "Talos places $PART on '$ppath', not /dev/$PARENT — the name and the device disagree, and a disagreement acts on nothing"
    pdev="$(jq -r '.dev_path // ""' "$WORK/part.json")"
    [ "$pdev" = "/dev/$PART" ] || node_refuse "Talos discovers $PART at '$pdev', not /dev/$PART — a disagreement acts on nothing"
    label="$(jq -r '.partition_label // ""' "$WORK/part.json")"
    [[ "$label" =~ $LABEL_RE ]] \
        || node_refuse "$PART on $NODE is labelled '$label', not u-<name> — only a Talos user volume's partition is wiped through this door; EFI, BOOT, META, STATE, EPHEMERAL and an unlabelled partition are not"
    name="${label#u-}"
    mnt="/var/mnt/$name"
    fs="$(jq -r 'if (.name // "") == "" then "none" else .name end' "$WORK/part.json")"
    fsuuid="$(jq -r 'if (.uuid // "") == "" then "none" else .uuid end' "$WORK/part.json")"
    psize="$(jq -r '.size // "unreported"' "$WORK/part.json")"
    ppretty="$(jq -r '.pretty_size // "unreported"' "$WORK/part.json")"
    pidx="$(jq -r '.partition_index // ""' "$WORK/part.json")"
    puuid="$(jq -r '.partition_uuid // ""' "$WORK/part.json")"
    case "${pidx:-empty}" in empty|*[!0-9]*) node_fail "Talos reports no partition index for $PART ('$pidx') — the read-back could not tell it from another" ;; esac
    [ -n "$puuid" ] || node_fail "Talos reports no partition uuid for $PART — the read-back could not tell it from another"

    # The disk carries this partition and nothing else.
    others="$(jq -r --arg d "$PARENT" --arg me "$PART" '
        [.[] | select(.metadata.id != $me)
         | select((.spec.parent // "") == $d or (.spec.parent_dev_path // "") == "/dev/" + $d)
         | "\(.metadata.id) (label \(.spec.partition_label // "none"), \(if (.spec.name // "") == "" then "no filesystem" else .spec.name end))"]
        | sort | join("; ")' "$WORK/dv.json")"
    [ -z "$others" ] \
        || node_refuse "$PARENT on $NODE carries other partitions Talos discovers: $others — only a disk whose single partition is $PART can lose it here"

    talos_list disks disks
    n="$(jq --arg d "$PARENT" '[.[] | select(.metadata.id == $d)] | length' "$WORK/disks.json")"
    [ "$n" = 1 ] || node_fail "Talos's disks on $NODE lists $n disk(s) named $PARENT — the disk under $PART cannot be named"
    jq -c --arg d "$PARENT" '.[] | select(.metadata.id == $d) | .spec' "$WORK/disks.json" > "$WORK/disk.json"
    model="$(jq -r '.model // "no model"' "$WORK/disk.json")"
    serial="$(jq -r '.serial // ""' "$WORK/disk.json")"
    wwid="$(jq -r '.wwid // "none"' "$WORK/disk.json")"
    dsize="$(jq -r '.size // "unreported"' "$WORK/disk.json")"
    dpretty="$(jq -r '.pretty_size // "unreported"' "$WORK/disk.json")"
    transport="$(jq -r '.transport // "unreported"' "$WORK/disk.json")"
    [ -n "$serial" ] || node_fail "Talos reports no serial for $PARENT — the disk cannot be named beyond its kernel number, which moves"

    # No volume still owns it.
    owners="$(jq -r --arg l "$label" --arg loc "/dev/$PART" --arg par "/dev/$PARENT" '
        [.[] | select(.metadata.id == $l or (.spec.location // "") == $loc
                      or (.spec.parentLocation // "") == $par or (.spec.location // "") == $par)
         | "\(.metadata.id) (phase \(.spec.phase // "unreported"), at \(if (.spec.location // "") == "" then "no location" else .spec.location end))"]
        | sort | join("; ")' "$WORK/vs.json")"
    [ -z "$owners" ] \
        || node_refuse "$PART on $NODE is still owned by a live volume: $owners — retire-a-user-volume $NODE $name first, and let Talos unmount it"

    # Nothing mounted from the disk.
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" mounts < /dev/null > "$WORK/mounts.out" 2> "$WORK/mounts.err"; then
        node_fail "talosctl -n $NODE_ADDRESS mounts could not read: $(tr '\n' ' ' < "$WORK/mounts.err" | cut -c1-300) — an unread mount table is not 'nothing mounted'"
    fi
    ndev="$(awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\/dev\//) { n++; break } } END { print n + 0 }' "$WORK/mounts.out")"
    [ "$ndev" -gt 0 ] || node_fail "talosctl mounts on $NODE lists no device-backed mount at all — the node's own /var is one, so this read saw nothing and says nothing"
    hits="$(awk -v d="/dev/$PARENT" '{ for (i = 1; i <= NF; i++) if ($i == d || (index($i, d "p") == 1 && substr($i, length(d) + 2) ~ /^[0-9]+$/)) { print; break } }' "$WORK/mounts.out")"
    [ -z "$hits" ] \
        || node_refuse "$PARENT on $NODE is mounted (talosctl mounts): $(printf '%s' "$hits" | tr -s ' ' | tr '\n' ';') — a pod or a volume still holds it; nothing is wiped under a mount"

    # Not a Longhorn disk.
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/lh.json" 2> "$WORK/lh.err"; then
        node_fail "kubectl get nodes.longhorn.io $NODE -n $LH could not read: $(tr '\n' ' ' < "$WORK/lh.err" | cut -c1-300) — whether $mnt is a Longhorn disk is unread, and unread is not 'no'"
    fi
    jq_doc_file "$WORK/lh.json" && jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1 \
        || node_fail "nodes.longhorn.io $NODE answered no document, or one with no spec.disks map — whether $mnt is a Longhorn disk is unread"
    lhdisks="$(jq -r '[.spec.disks | to_entries[] | "\(.key) at \(.value.path // "no path")"] | sort | join("; ")' "$WORK/lh.json")"
    lhhit="$(jq -r --arg m "$mnt" '
        def norm: (. // "" | sub("/+$"; ""));
        [.spec.disks | to_entries[] | (.value.path | norm) as $p
         | select($p == $m or ($p | startswith($m + "/")))
         | "Longhorn disk \(.key) at \(.value.path)"] | join("; ")' "$WORK/lh.json")"
    [ -z "$lhhit" ] \
        || node_refuse "$mnt on $NODE is registered with Longhorn: $lhhit — wiping it would lose every replica Longhorn keeps there; remove-longhorn-disk first"

    echo "plan: wipe-a-partition"
    echo "node: $NODE"
    echo "address: $NODE_ADDRESS"
    echo "estate role: $NODE_ROLE"
    echo "kubernetes node uid: $uid"
    echo "partition: $PART (/dev/$PART)"
    echo "partition label: $label (the Talos user volume '$name', mounted at $mnt when it lives)"
    echo "partition index: $pidx"
    echo "partition uuid: $puuid"
    echo "partition size: $ppretty ($psize bytes)"
    echo "filesystem: $fs (uuid $fsuuid)"
    echo "parent disk: $PARENT (/dev/$PARENT) — $model, serial $serial, wwid $wwid, $dpretty ($dsize bytes), transport $transport"
    echo "system disk: $sysid (Talos's systemdisk; EPHEMERAL on $ephpar) — not this disk"
    echo "other partitions on $PARENT: none"
    echo "volumes on $PARENT (volumestatus): none — no volume named $label, located on /dev/$PART, or living on /dev/$PARENT"
    echo "mounted from $PARENT (talosctl mounts): nothing"
    echo "longhorn disks on $NODE: ${lhdisks:-none} — none at $mnt"
    echo "command: talosctl -n $NODE_ADDRESS wipe disk $PART --drop-partition — clears the partition's signatures (FAST) and removes its entry from $PARENT's partition table. The data on it is gone; the disk itself stays."
}

# The registry first, so a control plane is refused before any door opens.
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
T="$(ops_talosctl)"
render > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    node_refuse "today's plan for $PART on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the partition, its disk, a volume, a mount or a Longhorn disk moved since the plan was signed. Nothing was wiped; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — wiping $PART on $NODE ($NODE_ADDRESS)"

PIDX="$(jq -r '.partition_index' "$WORK/part.json")"
PUUID="$(jq -r '.partition_uuid' "$WORK/part.json")"
SERIAL="$(jq -r '.serial' "$WORK/disk.json")"
MODEL="$(jq -r '.model // "no model"' "$WORK/disk.json")"
# The wipe gets a longer bound inside its container than a read: FAST
# clears a few MiB, then Talos rereads the table.
TW="$(BOSS_TALOS_TIMEOUT_S="${BOSS_WIPE_TALOS_TIMEOUT_S:-60}" ops_talosctl)"
# shellcheck disable=SC2086 # TW is one line the door prints to be word-split
if ! $TW -n "$NODE_ADDRESS" wipe disk "$PART" --drop-partition < /dev/null > "$WORK/wipe.out" 2> "$WORK/wipe.err"; then
    sed 's/^/    /' "$WORK/wipe.out" "$WORK/wipe.err" >&2
    node_fail "talosctl -n $NODE_ADDRESS wipe disk $PART --drop-partition did not succeed (its words above) — whether $PART was touched is unread; render plan-a-partition-wipe $NODE $PART before anything else"
fi
sed 's/^/    /' "$WORK/wipe.out"
echo "$ME: talosctl accepted the wipe of $PART on $NODE — reading it back (up to ${WAIT_S}s: Talos's discoveredvolumes and disks)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
last="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $T -n "$NODE_ADDRESS" get discoveredvolumes -o json < /dev/null > "$WORK/back-dv.raw" 2> "$WORK/back-dv.err" \
        && jq -s -c --arg a "$NODE_ADDRESS" '[.[] | select(.node == $a)]' "$WORK/back-dv.raw" > "$WORK/back-dv.json" 2> /dev/null \
        && $T -n "$NODE_ADDRESS" get disks -o json < /dev/null > "$WORK/back-disks.raw" 2> "$WORK/back-dv.err" \
        && jq -s -c --arg a "$NODE_ADDRESS" '[.[] | select(.node == $a)]' "$WORK/back-disks.raw" > "$WORK/back-disks.json" 2> /dev/null; then
        still="$(jq -r --arg p "$PART" --arg d "$PARENT" --arg i "$PIDX" --arg u "$PUUID" '
            [.[] | select(.metadata.id == $p
                          or (((.spec.parent // "") == $d or (.spec.parent_dev_path // "") == "/dev/" + $d)
                              and (((.spec.partition_index // "") | tostring) == $i or (.spec.partition_uuid // "") == $u)))
             | .metadata.id] | join(", ")' "$WORK/back-dv.json")"
        remaining="$(jq -r --arg d "$PARENT" '
            [.[] | select((.spec.parent // "") == $d or (.spec.parent_dev_path // "") == "/dev/" + $d) | .metadata.id]
            | if length == 0 then "none" else join(", ") end' "$WORK/back-dv.json")"
        disk_serial="$(jq -r --arg d "$PARENT" '[.[] | select(.metadata.id == $d) | .spec.serial // ""] | if length == 1 then .[0] else "" end' "$WORK/back-disks.json")"
        if [ -z "$still" ] && [ "$disk_serial" = "$SERIAL" ]; then
            echo "$ME: $PART is gone from $NODE — read back: Talos discovers no $PART, and $PARENT ($MODEL, serial $SERIAL) carries no partition table entry for it (no partition with index $PIDX or uuid $PUUID; partitions on $PARENT now: $remaining)"
            exit 0
        fi
        last="Talos still discovers: ${still:-nothing of it}; $PARENT reads serial '${disk_serial:-absent}' (planned $SERIAL); partitions on $PARENT: $remaining"
    else
        last="unread: $(tr '\n' ' ' < "$WORK/back-dv.err" | cut -c1-200)"
    fi
    elapsed=$(( $(date -u +%s) - start ))
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "talosctl accepted the wipe of $PART on $NODE, and after ${elapsed}s it is NOT proven gone: $last — read it again with plan-a-partition-wipe $NODE $PART or talos-get $NODE disks"
    fi
    sleep "$POLL_S"
done
