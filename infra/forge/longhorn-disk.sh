#!/usr/bin/env bash
# longhorn-disk.sh — make ONE node's declared Talos user volume a Longhorn
# disk, through a rendered plan a passkey signs. The ops verbs
# `plan-a-longhorn-disk` (--plan) and `add-longhorn-disk` (the write).
#
#   longhorn-disk.sh --plan <node> <volume>
#   longhorn-disk.sh <node> <volume> <plan-sha256>
#
# WHY IT EXISTS (backlog 493a02b6). Measured 2026-10-19T15:18Z while
# planning the third 1 TB disk for w-2 (David, ~15:05Z: one more disk for
# a one-disk machine), after w-1's Longhorn disk reached its scheduling
# floor (67e98635): every door stopped one step short of capacity.
# node-converge applies a UserVolumeConfig, which Talos mounts at
# /var/mnt/<name>; NOTHING then declared that mount as a Longhorn disk
# (nodes.longhorn.io spec.disks), and set-volume-replicas and
# move-volume-replica act only on disks that already exist. So the window
# David was about to run for w-2 (cordon, plan, shutdown, fit the disk,
# uncordon) ended at a mounted volume nothing used. That window then
# found w-2 has ONE drive bay: the 1 TB disk replaced its system disk and
# Longhorn adopted it as the default disk (join-a-node 829f75a8), so no
# declaration in the tree names a volume for this door yet. It is the
# door for the next worker that gains a second disk (review 77ee3857).
#
# THE SHAPE is design 17835005's, as set-volume-replicas' and
# node-converge's: the verb file declares requires_approval and names
# plan-a-longhorn-disk as its plan_verb, so the runner renders this
# script's --plan onto the request's approve step and hands the write
# sha256 of the SIGNED plan. The write re-renders and patches nothing
# unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) as a talos-worker, and the cluster agrees — the registry's
#     address as its InternalIP and no control-plane label
#     (cluster-node-lib.sh's node_require_worker and node_read_cluster,
#     before any door opens; the-controls-are-bounded-verbs holds every
#     mutating node verb but node-converge to that). The disks this is
#     for are workers' (a worker's second disk; w-1's gate NVMe is refused
#     below); a control plane's disk is a design decision, not a packet
#     argument.
#   * THE VOLUME IS DECLARED. infra/cluster/talos/patches/<node>.yaml —
#     the file beside this script, the tree the runner converged to —
#     names a UserVolumeConfig of that name, and the declaration passes
#     check-declared.sh --only-read-classes: canonical form, and a
#     diskSelector in the allowlist grammar that carries exactly one
#     `!system_disk` (the rule node-converge applies before it applies the
#     volume at all). The mount is DERIVED: Talos mounts a user volume at
#     /var/mnt/<name>, and the packet never names a path.
#   * TALOS SAYS IT IS THERE, AND NOT ON THE SYSTEM DISK. `talosctl get
#     volumestatus u-<name>` reads phase ready, mounted at /var/mnt/<name>
#     (mountSpec parentId /var/mnt, targetPath <name>), on a partition of
#     a disk that is NOT the disk EPHEMERAL lives on — read from Talos as
#     `volumestatus EPHEMERAL` in the same run. EPHEMERAL carries the
#     kubelet, containerd and Longhorn's default disk; a second Longhorn
#     disk there is one filesystem counted twice.
#   * NOTHING ELSE USES THE PATH. No PersistentVolume (local or hostPath)
#     and no pod on the node mounts /var/mnt/<name> or anything under it.
#     That is how w-1's gate volume is refused: the gate's local PV
#     gate-w-1 is /var/mnt/gate, and the daily trim pod mounts
#     /var/mnt/gate/seed (infra/cluster/manifests/gate-seed-local.yaml and
#     the trim CronJob's manifest). It is refused by what uses it, not by
#     its name, so a volume nothing claims yet is admitted.
#   * NOT ALREADY A LONGHORN DISK. The node's Longhorn node holds no disk
#     named u-<name>, and no disk whose path is /var/mnt/<name>, under it,
#     or over it (a disk at /var/mnt would hold this one).
#   * Longhorn's node reads Ready=True: a disk added to a node Longhorn
#     cannot reach has no read-back.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, role and Node uid, the UserVolumeConfig as declared, what
# Talos reads of it and of the system disk, the Longhorn node's disks
# now (their spec only), the disk to add, the one patch, what placement
# it opens, and the read-back. No clock and no live free space in the
# bytes (storageAvailable moves every second; it rides stderr). The
# disks before are IN the bytes, so a second run of an applied plan
# re-renders to a refusal: at most once.
#
# PLACEMENT, said in the plan because a passkey signs it: the disk is
# added UNTAGGED and schedulable, so Longhorn may place a replica of ANY
# volume on it — production volumes included — by its own rules. It does
# not change which NODES a volume's replicas may use: the node's
# allowScheduling is untouched, and Longhorn's replica-soft-anti-affinity
# (false) keeps one replica of a volume per node. It adds a disk to a
# node, and storageReserved 0, because the floor that protects a disk is
# Longhorn's storage-minimal-available-percentage, which applies to every
# disk.
#
# THE WRITE is one `kubectl patch nodes.longhorn.io --type=json`: a
# `test` that spec.disks is still exactly the map the plan read, then an
# `add` of spec.disks/u-<name> — a compare-and-set, so a disk changed
# between the render and the patch fails the patch rather than being
# overwritten. Nothing else is written.
#
# THE EFFECT IS READ BACK. Every BOSS_LONGHORN_DISK_POLL_S (default 10) up
# to BOSS_LONGHORN_DISK_WAIT_S (default 300, inside the verb's timeout):
# the Longhorn node. Only when spec.disks/u-<name> reads the planned path,
# every other disk's spec reads as the plan did, and Longhorn's own
# status for the new disk reads Ready=True and Schedulable=True with a
# diskUUID no other disk on the node has and a storageMaximum above zero
# is the line the verb file declares as `effect` printed. Longhorn marks a
# disk not Ready when the path is not the filesystem it recorded (a
# volume that failed to mount leaves its mountpoint directory on
# EPHEMERAL), so Ready is the proof the disk is the volume. Otherwise
# exit 1 saying the disk is NOT proven, with Longhorn's own reason; the
# disk stands, and Longhorn schedules nothing to a disk it does not read
# Ready. Its inverse, for that disk or any other this door placed, is
# remove-longhorn-disk.sh (backlog b791c5b4).
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=add-longhorn-disk
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
# A Talos user volume name, and so the last segment of /var/mnt/<name>:
# one lowercase word, no dot, no slash, no leading or trailing dash.
VOL_RE='^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$'

unread() { node_fail "CANNOT ANSWER — $* — an unread node has no plan, and nothing was written"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || node_refuse "usage: longhorn-disk.sh --plan <node> <volume> — the plan takes the node and the name of the user volume its declaration names, and nothing else"
else
    [ $# -eq 3 ] || node_refuse "usage: longhorn-disk.sh <node> <volume> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-longhorn-disk verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
NODE="$1"
VOL="$2"
node_check_id "$NODE"
[[ "$VOL" =~ $VOL_RE ]] || node_refuse "the volume must be one Talos user volume name (lowercase letters, digits and inner dashes, as the UserVolumeConfig names it), got '$VOL'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing can be read, and no evidence is not a plan"

MNT="/var/mnt/$VOL"
DISK="u-$VOL"
WAIT_S="${BOSS_LONGHORN_DISK_WAIT_S:-300}"
POLL_S="${BOSS_LONGHORN_DISK_POLL_S:-10}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_LONGHORN_DISK_WAIT_S and BOSS_LONGHORN_DISK_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the declaration, before any door opens ----------------------------------
# BOSS_TALOS_PATCHES is check-declared.sh's own seam; unset, both read the
# patches directory of the tree this script runs from.
PATCHES="${BOSS_TALOS_PATCHES:-$HERE/../cluster/talos/patches}"
CHECK="$HERE/../cluster/talos/check-declared.sh"
REL="infra/cluster/talos/patches/$NODE.yaml"
[ -r "$CHECK" ] || node_fail "the declaration reader $CHECK is not in this checkout"
[ -r "$PATCHES/$NODE.yaml" ] || node_refuse "$REL does not exist — $NODE declares no user volume, so there is no mount to make a Longhorn disk"
BOSS_TALOS_PATCHES="$PATCHES" bash "$CHECK" --only-read-classes "$NODE" > "$WORK/classes.out" 2>&1
case $? in
    0) ;;
    2 | 3) node_refuse "$REL is not a declaration node-converge would apply: $(tr '\n' ' ' < "$WORK/classes.out")" ;;
    *) node_fail "check-declared.sh could not judge $REL: $(tr '\n' ' ' < "$WORK/classes.out")" ;;
esac
# Canonical form (just proven) puts every document's keys at column 0, so
# a document is the lines between `---` lines; comments and blanks drop.
awk -v want="$VOL" '
    function flush() { if (uvc && named) { n++; printf "%s", doc } doc = ""; uvc = 0; named = 0 }
    /^---$/ { flush(); next }
    /^[ ]*#/ || /^[ ]*$/ { next }
    { doc = doc $0 "\n" }
    $0 == "kind: UserVolumeConfig" { uvc = 1 }
    $0 == "name: " want { named = 1 }
    END { flush(); if (n != 1) exit 1 }' "$PATCHES/$NODE.yaml" > "$WORK/uvc.yaml" \
    || node_refuse "$REL declares no UserVolumeConfig named '$VOL' (exactly one is required) — the mount is read from the declaration, never from the packet; declare it, land it, and apply it with node-converge first"

# --- the registry, the doors, the cluster's agreement -------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
T="$(ops_talosctl)"
node_read_cluster "$NODE"

# talos_volume <id> <file> — Talos's volumestatus <id> into $WORK/<file>,
# one object. NotFound is returned as status 2; any other failure cannot
# answer.
talos_volume() {
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" get volumestatus "$1" -o json < /dev/null > "$WORK/$2.raw" 2> "$WORK/$2.err"; then
        grep -q 'NotFound' "$WORK/$2.err" && return 2
        unread "talosctl get volumestatus $1 on $NODE: $(tr '\n' ' ' < "$WORK/$2.err" | cut -c1-300)"
    fi
    jq -s -c --arg id "$1" --arg a "$NODE_ADDRESS" \
        '[.[] | select(.metadata.id == $id and .node == $a)] | if length == 1 then .[0] else error("\(length) answers") end' \
        "$WORK/$2.raw" > "$WORK/$2.json" 2> "$WORK/$2.jq" \
        || unread "Talos's volumestatus $1 on $NODE did not read back as one volume of that node ($(tr '\n' ' ' < "$WORK/$2.jq" | cut -c1-200))"
}

# k_get <file> <args…> — one kubectl get into $WORK/<file>, read as a
# document or CANNOT ANSWER.
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

# under <path> — a jq predicate source: the path is $MNT or below it.
UNDER='def under($m): (. // "" | sub("/+$"; "")) as $p | $p == $m or ($p | startswith($m + "/"));'

# --- the plan -----------------------------------------------------------------
render() {
    local rc phase used uid
    talos_volume "$DISK" uv
    rc=$?
    [ "$rc" -eq 2 ] && node_refuse "Talos holds no volume $DISK on $NODE: $REL declares it, and it has not been applied (plan-a-node-converge / node-converge $NODE) or its disk is not in the machine — nothing to make a Longhorn disk"
    talos_volume EPHEMERAL eph
    [ $? -eq 2 ] && unread "Talos holds no EPHEMERAL volume on $NODE, so the system disk cannot be named"
    phase="$(jq -r '.spec.phase // "unreported"' "$WORK/uv.json")"
    [ "$phase" = ready ] || node_refuse "Talos reads $DISK on $NODE as phase '$phase', not ready — a volume that is not mounted is not a disk (talos-get $NODE volumestatus $DISK names why)"
    [ "$(jq -r '"\(.spec.mountSpec.parentId // "")/\(.spec.mountSpec.targetPath // "")"' "$WORK/uv.json")" = "$MNT" ] \
        || node_refuse "Talos mounts $DISK at '$(jq -r '"\(.spec.mountSpec.parentId // "")/\(.spec.mountSpec.targetPath // "")"' "$WORK/uv.json")', not $MNT, the mount a user volume named $VOL is given — this verb adds only that mount"
    local loc parent sys fs
    loc="$(jq -r '.spec.location // ""' "$WORK/uv.json")"
    parent="$(jq -r '.spec.parentLocation // ""' "$WORK/uv.json")"
    fs="$(jq -r '.spec.filesystem // "unreported"' "$WORK/uv.json")"
    sys="$(jq -r '.spec.parentLocation // ""' "$WORK/eph.json")"
    [[ "$sys" == /dev/* ]] || unread "Talos reads EPHEMERAL on $NODE with no parent disk ('$sys'), so the system disk cannot be named"
    [[ "$parent" == /dev/* && "$loc" == /dev/* ]] || unread "Talos reads $DISK on $NODE with no device ('$loc' on '$parent')"
    [ "$parent" != "$sys" ] || node_refuse "$DISK on $NODE is $loc on $parent, the system disk (the disk EPHEMERAL lives on, with the kubelet, containerd and Longhorn's default disk) — a second Longhorn disk there counts one filesystem twice"

    # Nothing else may use the path: PersistentVolumes, and pods on the node.
    k_get pvs.json pv || unread "the PersistentVolume list could not be read"
    used="$(jq -r --arg m "$MNT" "$UNDER"'
        [.items[] | . as $v
         | ([.spec.local.path, .spec.hostPath.path] | map(select(. != null)) | map(select(under($m)))) as $hit
         | select($hit != [])
         | "PersistentVolume \($v.metadata.name) (claim \(($v.spec.claimRef // {}) | "\(.namespace // "-")/\(.name // "-")")) mounts \($hit | join(", "))"]
        | join("; ")' "$WORK/pvs.json")" || unread "the PersistentVolume list would not parse"
    [ -z "$used" ] || node_refuse "$MNT on $NODE is in use, and a Longhorn disk would share it: $used — this verb adds a volume nothing claims, and the gate volume is the one it exists to refuse"
    node_pods "$NODE" || unread "$NODE_ERR"
    used="$(jq -r --arg m "$MNT" "$UNDER"'
        [.items[] | select((.status.phase // "") != "Succeeded" and (.status.phase // "") != "Failed") | . as $p
         | ([.spec.volumes[]?.hostPath.path] | map(select(. != null)) | map(select(under($m)))) as $hit
         | select($hit != [])
         | "pod \($p.metadata.namespace)/\($p.metadata.name) mounts \($hit | join(", "))"]
        | join("; ")' "$WORK/pods.json")" || unread "the pod list for $NODE would not parse"
    [ -z "$used" ] || node_refuse "$MNT on $NODE is in use, and a Longhorn disk would share it: $used"

    # Longhorn's node, and what it already holds.
    k_get lh.json nodes.longhorn.io "$NODE" -n "$LH"
    [ $? -eq 2 ] && node_refuse "Longhorn runs no node $NODE (nodes.longhorn.io $NODE in $LH is NotFound) — there is no Longhorn here to give a disk"
    [ "$(jq -r '.metadata.name // ""' "$WORK/lh.json")" = "$NODE" ] || unread "nodes.longhorn.io $NODE did not read back as that node"
    jq_doc_file "$WORK/lh.json" && jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1 \
        || unread "nodes.longhorn.io $NODE carries no spec.disks map"
    [ "$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status][0] // "unreported"' "$WORK/lh.json")" = True ] \
        || node_refuse "Longhorn's node $NODE does not read Ready=True ($(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | "\(.status) \(.reason // "")"][0] // "unreported"' "$WORK/lh.json")) — a disk added there could not be read back"
    used="$(jq -r --arg m "$MNT" --arg d "$DISK" '
        def norm: (. // "" | sub("/+$"; ""));
        [.spec.disks | to_entries[] | (.value.path | norm) as $p
         | select(.key == $d or $p == $m or ($p | startswith($m + "/")) or ($m | startswith($p + "/")))
         | "disk \(.key) at \(.value.path)"] | join("; ")' "$WORK/lh.json")"
    [ -z "$used" ] || node_refuse "$MNT is already a Longhorn disk on $NODE, or shares its path or its name ($DISK): $used — nothing to add"
    jq -c -S '.spec.disks' "$WORK/lh.json" > "$WORK/disks.json"

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    {
        echo "plan: add-longhorn-disk"
        echo "node: $NODE"
        echo "address: $NODE_ADDRESS"
        echo "estate role: $NODE_ROLE"
        echo "kubernetes node uid: $uid"
        echo "user volume: $VOL — declared in $REL:"
        sed 's/^/    /' "$WORK/uvc.yaml"
        echo "talos: $DISK $phase, $fs, mounted at $MNT, partition $loc on disk $parent$(jq -r 'if (.spec.prettySize // "") != "" then " (\(.spec.prettySize))" else "" end' "$WORK/uv.json")"
        echo "system disk: $sys (the disk EPHEMERAL lives on) — not this volume's"
        echo "in use: no PersistentVolume and no pod on $NODE mounts $MNT or anything under it"
        echo
        echo "== longhorn node $NODE: its disks now (spec; allowScheduling on the node: $(jq -r '.spec.allowScheduling' "$WORK/lh.json")) =="
        jq -r '.spec.disks | to_entries | sort_by(.key)[]
            | "disk \(.key): path \(.value.path), allowScheduling \(.value.allowScheduling), evictionRequested \(.value.evictionRequested // false), storageReserved \(.value.storageReserved // 0) bytes, tags \(.value.tags // [] | tojson)"' "$WORK/lh.json"
        echo
        echo "disk to add: $DISK at $MNT — allowScheduling true, evictionRequested false, storageReserved 0 bytes, tags [], diskType filesystem"
        echo "placement: the disk is untagged and schedulable, so Longhorn may place a replica of ANY volume on it, production volumes included, by its own rules; it changes no node's allowScheduling, and replica-soft-anti-affinity keeps one replica of a volume per node — this adds a disk to $NODE, not a node to any volume. Longhorn's storage-minimal-available-percentage floors it as it floors every disk."
        echo "command: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is still exactly the disks above, add spec.disks/$DISK; the one field this writes. Nothing is moved and nothing is deleted."
        echo "read-back: until spec.disks/$DISK reads path $MNT, every other disk reads as above, and Longhorn's status for $DISK reads Ready=True and Schedulable=True with a diskUUID of its own and a storageMaximum."
        echo "disks before (the compare-and-set's value): $(cat "$WORK/disks.json")"
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
    node_refuse "today's plan for $DISK on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the declaration, the volume, the node's disks or what uses the path moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — adding Longhorn disk $DISK at $MNT to $NODE"

PATCH="$(jq -c -n --slurpfile d "$WORK/disks.json" --arg k "$DISK" --arg p "$MNT" \
    '[{op: "test", path: "/spec/disks", value: $d[0]},
      {op: "add", path: "/spec/disks/\($k)",
       value: {path: $p, allowScheduling: true, evictionRequested: false, storageReserved: 0, tags: [], diskType: "filesystem"}}]')" \
    || node_fail "could not compose the patch — nothing was written"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch nodes.longhorn.io "$NODE" -n "$LH" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    node_fail "kubectl patch nodes.longhorn.io $NODE did not succeed (its words above) — this run cannot say the disk was added; read the node again with plan-a-longhorn-disk before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted disk $DISK on $NODE — reading it back (up to ${WAIT_S}s: the disk's spec, every other disk unchanged, and Longhorn's Ready and Schedulable for $DISK)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
said="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        said="$(jq -r --slurpfile before "$WORK/disks.json" --arg k "$DISK" --arg p "$MNT" '
            def cond($t): [(. // [])[] | select(.type == $t)][0] // {status: "unreported"};
            (.spec.disks // {}) as $d
            | (.status.diskStatus // {}) as $s
            | ($s[$k] // null) as $st
            | ([$s | to_entries[] | select(.key != $k) | .value.diskUUID // empty]) as $others
            | if ($d[$k].path // "") != $p then "spec.disks/\($k) reads path \($d[$k].path // "none")"
              elif ($d | del(.[$k])) != $before[0] then "another disk on the node changed: \([$d | del(.[$k]) | keys[]] | join(", ")) against \([$before[0] | keys[]] | join(", "))"
              elif $st == null then "Longhorn reports no status for \($k) yet"
              else ($st.conditions | cond("Ready")) as $r | ($st.conditions | cond("Schedulable")) as $sc
                | if $r.status != "True" or $sc.status != "True"
                  then "Longhorn reads \($k) Ready=\($r.status) (\($r.reason // "") \($r.message // "")) and Schedulable=\($sc.status) (\($sc.reason // ""))"
                  elif ($st.diskUUID // "") == "" then "Longhorn reads \($k) Ready with no diskUUID"
                  elif ($others | index($st.diskUUID)) != null then "Longhorn gives \($k) diskUUID \($st.diskUUID), which another disk on the node already has — one filesystem counted twice"
                  elif (($st.storageMaximum // 0) | tonumber? // 0) <= 0 then "Longhorn reads \($k) with no storageMaximum"
                  else "OK \($st.diskUUID) \($st.storageMaximum) \([$d | keys[] | select(. != $k)] | length)" end
              end' "$WORK/back.json" 2> "$WORK/back.jq")" || said="the Longhorn node would not parse: $(tr '\n' ' ' < "$WORK/back.jq" | cut -c1-200)"
    else
        said="the Longhorn node did not read: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    elapsed=$(($(date -u +%s) - start))
    case "$said" in
        "OK "*)
            # shellcheck disable=SC2086 # four words the jq above printed: OK, uuid, maximum, count
            set -- $said
            echo "$ME: $NODE has Longhorn disk u-$VOL at /var/mnt/$VOL — read back: Ready=True, Schedulable=True, diskUUID $2, storageMaximum $3 bytes, and its $4 other disk(s) unchanged (after ${elapsed}s)"
            exit 0 ;;
    esac
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "Longhorn accepted disk $DISK on $NODE, and after ${elapsed}s it is NOT proven: $said. The disk stands, and Longhorn schedules nothing to a disk it does not read Ready — read it with node-disk-usage $NODE, and take it back out with plan-a-longhorn-disk-removal and remove-longhorn-disk"
    fi
    sleep "$POLL_S"
done
