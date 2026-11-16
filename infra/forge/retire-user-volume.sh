#!/usr/bin/env bash
# retire-user-volume.sh — delete ONE live Talos UserVolumeConfig on one
# worker so Talos unmounts the volume and KEEPS its partition, through a
# rendered plan a passkey signs. The ops verbs
# `plan-a-user-volume-retirement` (--plan) and `retire-a-user-volume`
# (the write).
#
#   retire-user-volume.sh --plan <node> <volume>
#   retire-user-volume.sh <node> <volume> <plan-sha256>
#
# WHY IT EXISTS (backlog 412d504c). Door 1 of the volume-move protocol
# David asked for on 88b35891 ("our control should be sufficient to
# manage moving the workloads") and ordered on 2026-11-07 ~02:00Z: w-1's
# gate volume moves from the Kingston SNV3S1000G (nvme1n1p1, label
# u-gate) to a Micron 7400 PRO WITHOUT a reboot, each step a door on the
# record. node-converge can only ADD (it refuses a DRIFT, and Talos does
# not re-provision a volume whose selector changed under it), so the
# provisioned volume must leave the live config first: this door
# deletes its document, wipe-a-partition frees the old partition, and
# node-converge then provisions the declared volume — ABSENT live by
# then — by its new selector. Until this verb that delete was a
# `talosctl patch machineconfig` typed by hand with the admin
# talosconfig.
#
# THE SHAPE is design 17835005's, as node-converge's and
# add-longhorn-disk's: the verb file declares requires_approval and
# names plan-a-user-volume-retirement as its plan_verb, so the runner
# renders this script's --plan onto the request's approve step and hands
# the write sha256 of the SIGNED plan. The write re-renders and deletes
# nothing unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the two it checks:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) as a talos-worker, and the cluster agrees — its address as
#     the Node's InternalIP, no control-plane label (cluster-node-lib.sh's
#     node_require_worker before any door, then node_read_cluster). A
#     control plane's volume is a design decision, not a packet argument.
#   * THE TREE HAS STATED THE MOVE. The decision this door owes (the
#     packet asked for it to be decided and documented): the declaration
#     infra/cluster/talos/patches/<node>.yaml in the checkout the runner
#     converged to (the forge's HEAD — the file beside this script, as
#     longhorn-disk.sh reads it) must STILL name the volume, and pass
#     check-declared.sh --only-read-classes, because node-converge
#     provisions it again from exactly that declaration the moment this
#     door has run; a door that retired a volume nothing would bring back
#     strands the gate. And the live UserVolumeConfig must DRIFT from it
#     IN ITS diskSelector: the tree wants the volume on another disk. A
#     live document that MATCHes its declaration is refused — the selector
#     car has not landed, and retiring the volume would only provision it
#     again where it is. A drift in anything but the selector is refused
#     too: that is not a move, and node-converge, not a delete, is how a
#     declaration reaches a node. The plan records BOTH selectors, as
#     check-declared.sh parsed them. No tree sha is taken (node-converge
#     takes one): the declaration is the one this checkout's HEAD holds,
#     and its sha256 is in the signed bytes, so a declaration that changes
#     between plan and write re-renders differently and is refused.
#   * TALOS SAYS WHERE THE DATA IS. volumestatus u-<volume> reads phase
#     ready, mounted at /var/mnt/<volume>, on a partition of a disk; and
#     discoveredvolumes names that partition with partition label
#     u-<volume> — the label wipe-a-partition and every later read will
#     find it by once the volume is gone.
#   * NOTHING USES THE PATH. No pod on the node that is still Pending or
#     Running mounts /var/mnt/<volume>, anything under it, or a path that
#     holds it, judged by each container's EFFECTIVE host path: the
#     volume's hostPath (the daily trim pod mounts /var/mnt/gate/seed), or
#     the local/hostPath path of the PersistentVolume its claim is bound
#     to (the gate's local PV gate-w-1, claim boss-dev/gate: a gate run in
#     flight), plus the volumeMount's subPath — so hostPath /var/mnt with
#     subPath gate is the volume (review f1d1c254, R1 P1), and every path
#     is compared canonical, `.` components cleaned as the kubelet cleans
#     them, so subPath ./gate is too (review e25dd362). A subPathExpr,
#     or a subPath that is absolute or climbs, is not resolved and refuses
#     whenever its source is related. Talos would unmount a filesystem a
#     running pod writes to.
#   * NOT A LONGHORN DISK, AND ONE EXEMPTION (backlog 76c8b647). The
#     node's nodes.longhorn.io registers no disk that is /var/mnt/<volume>,
#     sits under it, or holds it — Longhorn writes only its registered
#     disks. With that read, Longhorn's instance-manager (on every node)
#     mounting host `/` whole is not a user of the volume: that one mount
#     is exempt and printed in the plan; every other mount still counts.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, role and Node uid, the declaration (path, sha256, the
# UserVolumeConfig as declared), both selectors, what Talos reads of the
# volume and its partition, the PersistentVolumes at the path, the one
# command and the exact document it applies, and the read-back. No clock
# and no free space in the bytes. The volume is IN the bytes, so a second
# run of an applied plan re-renders to a refusal (the live document is
# gone): at most once.
#
# THE WRITE is one `talosctl -n <address> patch machineconfig
# --mode=no-reboot --patch=<document>` through the ops_talosctl door,
# where the document is
#
#     apiVersion: v1alpha1
#     kind: UserVolumeConfig
#     name: <volume>
#     $patch: delete
#
# — Talos's strategic merge removes that one document and nothing else,
# and in no-reboot mode Talos refuses a change it cannot apply live. It
# writes no partition, no filesystem and no other document.
#
# THE EFFECT IS READ BACK. Every BOSS_RETIRE_VOLUME_POLL_S (default 5) up
# to BOSS_RETIRE_VOLUME_WAIT_S (default 180, inside the verb's timeout),
# three reads, each through the doors: the live machine config through
# check-declared.sh reads UserVolumeConfig[<volume>] ABSENT; Talos holds
# no volumestatus u-<volume> (the volume is torn down and unmounted); and
# discoveredvolumes still names the partition with label u-<volume> (the
# data is kept). Only then is the line the verb file declares as `effect`
# printed, naming the partition that still holds the data; otherwise exit
# 1 saying the retirement is NOT proven, with what each read said.
#
# NO CREDENTIAL IS EVER PRINTED. The live machine config carries the
# cluster's CA keys and join tokens; it is only ever piped into
# check-declared.sh, and of its findings the plan shows the two selectors
# alone, as JSON strings. talosctl's words on a failure are printed with
# any token-shaped run withheld (node-converge's mask).
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=retire-a-user-volume
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

# A Talos user volume name, and so the last segment of /var/mnt/<name>
# (longhorn-disk.sh's rule): one lowercase word, inner dashes only.
VOL_RE='^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$'

unread() { node_fail "CANNOT ANSWER — $* — an unread node has no plan, and nothing was retired"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || node_refuse "usage: retire-user-volume.sh --plan <node> <volume> — the plan takes the node and the name of the user volume, and nothing else"
else
    [ $# -eq 3 ] || node_refuse "usage: retire-user-volume.sh <node> <volume> <plan-sha256> — this deletes only an APPROVED plan's document, and the hash is what the approval signed. Render one with --plan (the plan-a-user-volume-retirement verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
NODE="$1"
VOL="$2"
node_check_id "$NODE"
[[ "$VOL" =~ $VOL_RE ]] || node_refuse "the volume must be one Talos user volume name (lowercase letters, digits and inner dashes, as the UserVolumeConfig names it), got '$VOL'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing can be read, and no evidence is not a plan"
command -v python3 >/dev/null 2>&1 || node_fail "python3 is not on PATH — check-declared.sh cannot read the live config, and no evidence is not a plan"

MNT="/var/mnt/$VOL"
DISK="u-$VOL"
WAIT_S="${BOSS_RETIRE_VOLUME_WAIT_S:-180}"
POLL_S="${BOSS_RETIRE_VOLUME_POLL_S:-5}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_RETIRE_VOLUME_WAIT_S and BOSS_RETIRE_VOLUME_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The one document the write applies. Its bytes are in the plan.
DELETE_DOC="apiVersion: v1alpha1
kind: UserVolumeConfig
name: $VOL
\$patch: delete"

# mask — talosctl's words, with anything shaped like a credential withheld
# (node-converge.sh's rule).
mask() {
    sed -E 's#[A-Za-z0-9+/=_-]{40,}#<withheld: a token-shaped run>#g'
}

# --- the declaration, before any door opens ----------------------------------
# BOSS_TALOS_PATCHES is check-declared.sh's own seam; unset, both read the
# patches directory of the tree this script runs from.
PATCHES="${BOSS_TALOS_PATCHES:-$HERE/../cluster/talos/patches}"
CHECK="$HERE/../cluster/talos/check-declared.sh"
REL="infra/cluster/talos/patches/$NODE.yaml"
[ -r "$CHECK" ] || node_fail "the declaration reader $CHECK is not in this checkout"
[ -r "$PATCHES/$NODE.yaml" ] || node_refuse "$REL does not exist — $NODE declares no user volume, so there is nothing node-converge would provision again after a retirement"
BOSS_TALOS_PATCHES="$PATCHES" bash "$CHECK" --only-read-classes "$NODE" > "$WORK/classes.out" 2>&1
case $? in
    0) ;;
    2 | 3) node_refuse "$REL is not a declaration node-converge would apply, so a volume retired now could not be provisioned again from it: $(tr '\n' ' ' < "$WORK/classes.out")" ;;
    *) node_fail "check-declared.sh could not judge $REL: $(tr '\n' ' ' < "$WORK/classes.out")" ;;
esac
# Canonical form (just proven) puts every document's keys at column 0, so
# a document is the lines between `---` lines (longhorn-disk.sh's reading).
awk -v want="$VOL" '
    function flush() { if (uvc && named) { n++; printf "%s", doc } doc = ""; uvc = 0; named = 0 }
    /^---$/ { flush(); next }
    /^[ ]*#/ || /^[ ]*$/ { next }
    { doc = doc $0 "\n" }
    $0 == "kind: UserVolumeConfig" { uvc = 1 }
    $0 == "name: " want { named = 1 }
    END { flush(); if (n != 1) exit 1 }' "$PATCHES/$NODE.yaml" > "$WORK/uvc.yaml" \
    || node_refuse "$REL declares no UserVolumeConfig named '$VOL' (exactly one is required) — this door retires a volume the tree still declares, so node-converge can provision it again where the declaration now says"
DECL_SHA="$(sha256sum < "$PATCHES/$NODE.yaml" | cut -d' ' -f1)"

# --- the registry, the doors, the cluster's agreement -------------------------
node_read_registry "$NODE"
node_require_worker "$NODE"
node_door
T="$(ops_talosctl)"
node_read_cluster "$NODE"

# read_live <tag> — the live machine config, piped straight into the check
# against the declaration and never written down: sets GET_RC and
# CHECK_RC, the findings in $WORK/<tag>.out, and VERDICT, the check's
# verdict for UserVolumeConfig[<volume>] ("" when it printed none;
# "DOUBLED" when the live config holds it twice).
read_live() {
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    $T -n "$NODE_ADDRESS" get machineconfig -o yaml < /dev/null 2> "$WORK/$1.get" \
        | BOSS_TALOS_PATCHES="$PATCHES" bash "$CHECK" "$NODE" > "$WORK/$1.out" 2> "$WORK/$1.err"
    local st=("${PIPESTATUS[@]}")
    GET_RC="${st[0]}"
    CHECK_RC="${st[1]}"
    grep -E "^(MATCH|DRIFT|ABSENT|DOUBLED) +UserVolumeConfig\[$VOL\]( |$)" "$WORK/$1.out" \
        | awk '{ print $1 }' | sort -u > "$WORK/$1.verdict"
    if grep -qx DOUBLED "$WORK/$1.verdict"; then
        VERDICT=DOUBLED
    else
        VERDICT="$(tr -d '\n' < "$WORK/$1.verdict")"
    fi
}

# has_summary <file> — the check's closing line for THIS node is there;
# without it the check did not finish a comparison, whatever its exit.
has_summary() {
    grep -Eq "^check-declared: $NODE: [0-9]+ match, " "$1"
}

# talos_get <resource> <id> <file> — one Talos resource of this node into
# $WORK/<file>.json, one object. 0 read, 2 NotFound, 1 could not look
# (TALOS_ERR says why). Only the two status resources this door reads.
talos_get() {
    case "$1" in
        volumestatus | discoveredvolumes) ;;
        *) node_fail "talos_get reads volumestatus and discoveredvolumes only, not '$1'" ;;
    esac
    TALOS_ERR=""
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    if ! $T -n "$NODE_ADDRESS" get "$1" "$2" -o json < /dev/null > "$WORK/$3.raw" 2> "$WORK/$3.err"; then
        grep -q 'NotFound' "$WORK/$3.err" && return 2
        TALOS_ERR="talosctl get $1 $2 on $NODE: $(mask < "$WORK/$3.err" | tr '\n' ' ' | cut -c1-300)"
        return 1
    fi
    if ! jq -s -c --arg id "$2" --arg a "$NODE_ADDRESS" \
            '[.[] | select(.metadata.id == $id and .node == $a)] | if length == 1 then .[0] else error("\(length) answers") end' \
            "$WORK/$3.raw" > "$WORK/$3.json" 2> "$WORK/$3.jq"; then
        TALOS_ERR="Talos's $1 $2 on $NODE did not read back as one resource of that node ($(tr '\n' ' ' < "$WORK/$3.jq" | cut -c1-200))"
        return 1
    fi
}

# partition_label <file> — the partition label discoveredvolumes states,
# under either spelling a talosctl release prints it with.
partition_label() {
    jq -r '.spec.partition_label // .spec.partitionLabel // ""' "$WORK/$1.json"
}

# selectors <file> — the two diskSelector matches out of the check's DRIFT
# line for UserVolumeConfig[<volume>] (`declared {json}  live {json}`, as
# check-declared.sh's show() writes them), one JSON string per line into
# $WORK/selectors: declared, then live. Exit 0 they differ, 3 they are
# equal, 4 a side names no selector; anything else could not be read.
# Only the selector leaves this function — the live document may carry
# an encryption key, and nothing else of it is printed.
SELECTORS_PY='
import json, sys
line = sys.stdin.read().rstrip("\n")
at = line.index("  declared ") + len("  declared ")
dec, end = json.JSONDecoder().raw_decode(line, at)
sep = "  live "
if line[end:end + len(sep)] != sep:
    sys.exit(2)
live, end = json.JSONDecoder().raw_decode(line, end + len(sep))
if line[end:].strip():
    sys.exit(2)
def sel(d):
    p = d.get("provisioning") if isinstance(d, dict) else None
    s = p.get("diskSelector") if isinstance(p, dict) else None
    m = s.get("match") if isinstance(s, dict) else None
    return m if isinstance(m, str) and m.strip() else None
a, b = sel(dec), sel(live)
print(json.dumps(a))
print(json.dumps(b))
if a is None or b is None:
    sys.exit(4)
sys.exit(3 if a == b else 0)
'
selectors() {
    grep -m 1 -E "^DRIFT +UserVolumeConfig\[$VOL\] " "$1" \
        | python3 -I -c "$SELECTORS_PY" > "$WORK/selectors" 2> "$WORK/selectors.err"
}

# RELATED — jq definitions: `related($m)` holds for a host path that IS
# $MNT, sits under it, or HOLDS it (an ancestor, "/" included — a mount of
# /var/mnt sees /var/mnt/gate and everything Talos does to it). Review
# f1d1c254 (R1 P1) is why the ancestor half exists: a pod mounting
# hostPath /var/mnt with subPath gate mounts the volume, and a guard that
# judged only the source passed it.
#
# Paths are compared CANONICAL, as the kubelet's filepath.Join leaves
# them: empty and `.` components dropped, so "/var/mnt/", "/var/./mnt"
# and "/var/mnt/./gate" are /var/mnt and /var/mnt/gate (review e25dd362:
# Kubernetes admits subPath ./gate, rejecting only absolute paths and
# `..`, and a lexical compare matched it in neither direction). A path
# with a `..` component is not resolved here: `canon` answers null and
# `related` holds for it, so it refuses rather than guess.
RELATED='
def canon: if test("(^|/)[.][.](/|$)") then null
           else "/" + (split("/") | map(select(. != "" and . != ".")) | join("/")) end;
def within($a; $b): $a == $b or $b == "/" or ($a | startswith($b + "/"));
def related($m): canon as $p | if $p == null then true else within($p; $m) or within($m; $p) end;'

# --- the plan -----------------------------------------------------------------
render() {
    local rc phase loc parent size fs mnt puuid part label pfs dec_sel live_sel uid claims used lhdisks lhhit

    # The tree's intent against the live config.
    read_live before
    [ "$GET_RC" -eq 0 ] \
        || unread "could not read $NODE's machine config through the talosconfig door ($(mask < "$WORK/before.get" | tr '\n' ' ' | cut -c1-300))"
    case "$CHECK_RC" in
        0 | 1) has_summary "$WORK/before.out" \
            || unread "check-declared.sh exited $CHECK_RC without its summary line: $(mask < "$WORK/before.err" | tr '\n' ' ' | cut -c1-300)" ;;
        2) node_refuse "check-declared.sh would not compare $NODE's live config with $REL: $(mask < "$WORK/before.err" | tr '\n' ' ' | cut -c1-300)" ;;
        *) unread "check-declared.sh could not read $NODE's live config (exit $CHECK_RC): $(mask < "$WORK/before.err" | tr '\n' ' ' | cut -c1-300)" ;;
    esac
    case "$VERDICT" in
        DRIFT) ;;
        MATCH) node_refuse "the live UserVolumeConfig $VOL on $NODE MATCHes $REL — the tree still wants the volume where it is (its diskSelector unchanged), so there is no move to make room for: land the declaration that changes its selector first, and retiring it here would only have node-converge provision it again on the same disk. Nothing was retired" ;;
        ABSENT) node_refuse "$NODE's live config holds no UserVolumeConfig $VOL — there is nothing to retire (it may already have been: plan-a-node-converge $NODE <sha> provisions the declared one). Nothing was retired" ;;
        DOUBLED) node_refuse "$NODE's live config holds UserVolumeConfig $VOL more than once — a delete of a doubled document is not one this door can read back. Nothing was retired" ;;
        *) unread "check-declared.sh printed no single verdict for UserVolumeConfig[$VOL] ('$VERDICT')" ;;
    esac
    selectors "$WORK/before.out"
    rc=$?
    case "$rc" in
        0) ;;
        3) node_refuse "the live UserVolumeConfig $VOL on $NODE drifts from $REL, but not in its diskSelector — both select $(head -n 1 "$WORK/selectors"), the same diskSelector, so the tree does not want the volume on another disk. A drift in anything else is node-converge's to report, never a reason to retire a volume. Nothing was retired" ;;
        4) node_refuse "the live UserVolumeConfig $VOL on $NODE or its declaration names no diskSelector match (declared $(sed -n 1p "$WORK/selectors"), live $(sed -n 2p "$WORK/selectors")) — a move is a change of selector, and neither side can show one. Nothing was retired" ;;
        *) unread "the drift of UserVolumeConfig[$VOL] could not be read as two selectors ($(tr '\n' ' ' < "$WORK/selectors.err" | cut -c1-200))" ;;
    esac
    dec_sel="$(sed -n 1p "$WORK/selectors")"
    live_sel="$(sed -n 2p "$WORK/selectors")"

    # Where Talos says the data is.
    talos_get volumestatus "$DISK" uv
    rc=$?
    [ "$rc" -eq 2 ] && node_refuse "Talos holds no volume $DISK on $NODE, though its live config holds the UserVolumeConfig — nothing is mounted to retire (talos-get $NODE volumestatus $DISK names why)"
    [ "$rc" -eq 0 ] || unread "$TALOS_ERR"
    phase="$(jq -r '.spec.phase // "unreported"' "$WORK/uv.json")"
    [ "$phase" = ready ] || node_refuse "Talos reads $DISK on $NODE as phase '$phase', not ready — a volume that is not mounted is not one this door can retire and read back (talos-get $NODE volumestatus $DISK names why)"
    mnt="$(jq -r '"\(.spec.mountSpec.parentId // "")/\(.spec.mountSpec.targetPath // "")"' "$WORK/uv.json")"
    [ "$mnt" = "$MNT" ] || node_refuse "Talos mounts $DISK at '$mnt', not $MNT, the mount a user volume named $VOL is given — this door retires only that mount"
    loc="$(jq -r '.spec.location // ""' "$WORK/uv.json")"
    parent="$(jq -r '.spec.parentLocation // ""' "$WORK/uv.json")"
    size="$(jq -r '.spec.prettySize // .spec.size // "unreported"' "$WORK/uv.json")"
    fs="$(jq -r '.spec.filesystem // "unreported"' "$WORK/uv.json")"
    puuid="$(jq -r '.spec.partitionUUID // "unreported"' "$WORK/uv.json")"
    [[ "$loc" =~ ^/dev/[a-z0-9]+$ && "$parent" =~ ^/dev/[a-z0-9]+$ ]] \
        || unread "Talos reads $DISK on $NODE with no plain device ('$loc' on '$parent')"
    part="${loc#/dev/}"
    talos_get discoveredvolumes "$part" pv
    rc=$?
    [ "$rc" -eq 0 ] || unread "${TALOS_ERR:-Talos names no discovered volume $part on $NODE, the partition $DISK is mounted from}"
    label="$(partition_label pv)"
    [ -n "$label" ] || unread "Talos's discoveredvolumes $part on $NODE states no partition label"
    [ "$label" = "$DISK" ] || node_refuse "the partition $loc that Talos mounts as $DISK carries partition label '$label', not $DISK — after the delete nothing could name it as this volume's, so it is not retired here"
    pfs="$(jq -r '.spec.name // "unreported"' "$WORK/pv.json")"

    # Longhorn's disks on the node, before any pod is judged (backlog
    # 76c8b647). Longhorn writes only the disks registered on its node
    # object (spec.disks) — so if /var/mnt/<vol>, a path under it, or a
    # path that holds it is one of them, Longhorn writes the volume and the
    # retire is refused here. This read is also what the instance-manager
    # exemption below rests on, so it fails CLOSED: a Longhorn node that
    # cannot be read is CANNOT ANSWER, never "no disks". NotFound is
    # Longhorn running no node here, which registers no disk.
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K get nodes.longhorn.io "$NODE" -n longhorn-system -o json --request-timeout=30s \
            > "$WORK/lh.json" 2> "$WORK/lh.err"; then
        { jq_doc_file "$WORK/lh.json" && jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1; } \
            || unread "nodes.longhorn.io $NODE carries no spec.disks map"
        lhdisks="$(jq -r '[.spec.disks | to_entries | sort_by(.key)[] | "\(.key) at \(.value.path // "")"] | join(", ")' "$WORK/lh.json")"
        lhhit="$(jq -r --arg m "$MNT" "$RELATED"'
            [.spec.disks | to_entries | sort_by(.key)[] | select((.value.path // "") | related($m))
             | "\(.key) at \(.value.path // "")"] | join(", ")' "$WORK/lh.json")" \
            || unread "nodes.longhorn.io $NODE's disks would not parse"
        [ -z "$lhhit" ] || node_refuse "$MNT on $NODE is a Longhorn disk, sits under one, or holds one: $lhhit — Longhorn writes there; remove the disk first (plan-a-longhorn-disk-removal). Nothing was retired"
        lhdisks="${lhdisks:-none}"
    elif grep -q 'NotFound' "$WORK/lh.err"; then
        lhdisks="none (Longhorn runs no node $NODE)"
    else
        unread "kubectl get nodes.longhorn.io $NODE: $(tr '\n' ' ' < "$WORK/lh.err" | cut -c1-300)"
    fi

    # Nothing may use the path: every pod on the node still Pending or
    # Running, judged by the host path each container ACTUALLY mounts
    # (review f1d1c254, R1 P1). A volume's host source is its hostPath, or
    # the local/hostPath path of the PersistentVolume its claim is bound
    # to; a volumeMount's effective path is that source plus its subPath.
    # The pod is refused when an effective path is $MNT, under it, or
    # holds it. A subPathExpr cannot be expanded here, and a subPath that
    # is absolute or climbs (`..`) is not resolved, so either refuses
    # whenever its SOURCE is related to $MNT — a subPath never leaves its
    # source, so an unrelated source is safe. A host volume no container
    # mounts is judged by its source. Containers, init containers and
    # ephemeral containers alike.
    #
    # ONE EXEMPTION (backlog 76c8b647; the first live plan, ops-request
    # cc8d602e, refused on it, and it runs on every node): Longhorn's
    # instance-manager mounts host `/` at /host. A mount is exempt only
    # when ALL hold — the pod is in longhorn-system, carries the label
    # longhorn.io/component=instance-manager AND is named
    # instance-manager-*; the mount's source is a hostPath (never a claim);
    # and its effective path, canonical, is exactly `/` (so a subPath into
    # the volume, a subPathExpr, or a mount of /var/mnt or /var/mnt/<vol>
    # is not exempt). It rests on the Longhorn-disk read above having found
    # $MNT related to none of Longhorn's disks, which is the only place
    # Longhorn writes. Every exempt mount is printed in the plan, so a
    # pod that changes between plan and write changes the signed bytes.
    # Every hit is {pod, t, x}: x marks the exempt ones.
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    $K get pv -o json --request-timeout=30s > "$WORK/pvs.json" 2> "$WORK/pvs.err" \
        || unread "kubectl get pv: $(tr '\n' ' ' < "$WORK/pvs.err" | cut -c1-300)"
    # Every PersistentVolume with a host source: {name, path, claim}.
    jq -c '
        [.items[] | . as $v
         | ([.spec.local.path, .spec.hostPath.path] | map(select(. != null and . != "")) | .[0]) as $path
         | select($path != null)
         | {name: $v.metadata.name, path: $path,
            claim: (if $v.spec.claimRef == null then null
                    else "\($v.spec.claimRef.namespace // "-")/\($v.spec.claimRef.name // "-")" end)}]
        | sort_by(.name)' "$WORK/pvs.json" > "$WORK/hostpvs.json" 2> "$WORK/claims.err" \
        || unread "the PersistentVolume list would not parse ($(tr '\n' ' ' < "$WORK/claims.err" | cut -c1-200))"
    claims="$(jq -r --arg m "$MNT" "$RELATED"'
        [.[] | select(.path | related($m)) | "PersistentVolume \(.name) (claim \(.claim // "none")) at \(.path)"]
        | join("; ")' "$WORK/hostpvs.json")"
    node_pods "$NODE" || unread "$NODE_ERR"
    jq -c --arg m "$MNT" --slurpfile c "$WORK/hostpvs.json" "$RELATED"'
        ([$c[0][] | select(.claim != null) | {key: .claim, value: .path}] | from_entries) as $pv
        | [.items[] | select((.status.phase // "") != "Succeeded" and (.status.phase // "") != "Failed") | . as $p
           | "\($p.metadata.namespace)/\($p.metadata.name)" as $who
           | (($p.metadata.namespace == "longhorn-system")
              and ((($p.metadata.labels // {})["longhorn.io/component"] // "") == "instance-manager")
              and ($p.metadata.name | startswith("instance-manager-"))) as $im
           | ([.spec.volumes[]?
               | {key: .name,
                  value: (if (.hostPath.path // "") != "" then {src: "hostPath \(.hostPath.path)", path: .hostPath.path, hp: true}
                          elif (.persistentVolumeClaim.claimName // "") != "" then
                              ("\($p.metadata.namespace)/\(.persistentVolumeClaim.claimName)") as $k
                              | if $pv[$k] != null then {src: "claim \($k) (\($pv[$k]))", path: $pv[$k], hp: false} else null end
                          else null end)}
               | select(.value != null)] | from_entries) as $vols
           | ([(.spec.containers // []), (.spec.initContainers // []), (.spec.ephemeralContainers // [])]
              | add | [.[] | .volumeMounts[]?]) as $mounts
           | ([$mounts[] | . as $vm | $vols[$vm.name // ""] as $v | select($v != null)
               | if ($vm.subPathExpr // "") != "" then
                     select($v.path | related($m))
                     | {t: "\($v.src) with subPathExpr \($vm.subPathExpr), which is not resolved here, at \($vm.mountPath)", x: false}
                 elif ($vm.subPath // "") != "" then
                     if ($vm.subPath | startswith("/") or (split("/") | any(. == ".."))) then
                         select($v.path | related($m))
                         | {t: "\($v.src) with subPath \($vm.subPath), which is not resolved safely, at \($vm.mountPath)", x: false}
                     else
                         "\($v.path)/\($vm.subPath)" as $j | ($j | canon // $j) as $e
                         | select($e | related($m))
                         | {t: "\($v.src) subPath \($vm.subPath), that is \($e), at \($vm.mountPath)",
                            x: ($im and $v.hp and $e == "/")}
                     end
                 else
                     select($v.path | related($m))
                     | {t: "\($v.src) at \($vm.mountPath)", x: ($im and $v.hp and ($v.path | canon) == "/")}
                 end]
              + [$vols | to_entries[] | .key as $n
                 | select(any($mounts[]; .name == $n) | not)
                 | select(.value.path | related($m))
                 | {t: "\(.value.src), mounted by no container", x: ($im and .value.hp and (.value.path | canon) == "/")}])
           | .[] | . + {pod: $who}]' "$WORK/pods.json" > "$WORK/hits.json" 2> "$WORK/hits.err" \
        || unread "the pod list for $NODE would not parse ($(tr '\n' ' ' < "$WORK/hits.err" | cut -c1-200))"
    used="$(jq -r '[group_by(.pod)[] | map(select(.x | not)) | select(. != [])
                    | "pod \(.[0].pod) mounts \(map(.t) | join(", "))"] | sort | join("; ")' "$WORK/hits.json")"
    [ -z "$used" ] || node_refuse "$MNT on $NODE is in use: $used — Talos would unmount a filesystem a running pod writes to or sees (a gate run in flight, or a mount of a path that holds it). Hold the gates and let the bays empty, then plan again. Nothing was retired"
    jq -r --arg m "$MNT" '[.[] | select(.x)
        | "exempt: \(.pod) mounts \(.t) (Longhorn writes only its registered disks; \($m) is not one)"] | sort[]' \
        "$WORK/hits.json" > "$WORK/exempt"

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    {
        echo "plan: retire-a-user-volume"
        echo "node: $NODE"
        echo "address: $NODE_ADDRESS"
        echo "estate role: $NODE_ROLE"
        echo "kubernetes node uid: $uid"
        echo "user volume: $VOL — Talos volume $DISK, mounted at $MNT"
        echo "declaration: $REL in this checkout (sha256 $DECL_SHA) declares UserVolumeConfig $VOL as:"
        sed 's/^/    /' "$WORK/uvc.yaml"
        echo "selector declared: $dec_sel"
        echo "selector live: $live_sel"
        echo "why: the live UserVolumeConfig $VOL drifts from its declaration in its diskSelector (check-declared.sh: DRIFT UserVolumeConfig[$VOL]) — the tree wants the volume on another disk. Talos does not move a provisioned volume; this delete is what lets node-converge provision the declared one, ABSENT live then, by the declared selector."
        echo "talos: $DISK phase $phase, $fs, mounted at $MNT, partition $loc on disk $parent ($size), partition uuid $puuid"
        echo "the data: partition $loc on disk $parent, partition label $label, filesystem $pfs — the delete unmounts it and writes nothing to it; freeing it is wipe-a-partition, a door of its own"
        echo "claims on the path: ${claims:-none}"
        echo "longhorn disks on $NODE: $lhdisks — none is $MNT, sits under it, or holds it, and Longhorn writes only these"
        echo "in use: no pod on $NODE mounts $MNT, anything under it, or a path that holds it — each container's mount judged by its effective host path (the hostPath, or the PersistentVolume its claim is bound to, plus its subPath) — but the exempt mounts below"
        if [ -s "$WORK/exempt" ]; then cat "$WORK/exempt"; else echo "exempt: none"; fi
        echo "command: talosctl -n $NODE_ADDRESS patch machineconfig --mode=no-reboot --patch=<the document below> — Talos's merge removes that one document and unmounts the volume live, or refuses if it would need a reboot. Nothing else in the config changes."
        echo
        echo "== the patch =="
        printf '%s\n' "$DELETE_DOC"
        echo
        echo "read-back: until check-declared.sh reads UserVolumeConfig[$VOL] ABSENT in the live config, Talos holds no volume $DISK, and discoveredvolumes still names $loc with partition label $DISK."
        echo "after: until node-converge provisions the declared volume and its proof is written, a pod mounting a claim at $MNT finds the empty mountpoint left on EPHEMERAL — keep the gates held (infra/gate-runner/run.sh refuses without its /gate-runs/.gate-volume marker)."
    } > "$WORK/plan"
    PART_LOC="$loc"
    PART_PARENT="$parent"
    PART_ID="$part"
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
    node_refuse "today's plan for $DISK on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the declaration, the live config, the volume, its partition or what uses the path moved since the plan was signed (or this plan already ran). Nothing was retired; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — deleting UserVolumeConfig $VOL on $NODE ($NODE_ADDRESS); partition $PART_LOC is kept"

# The apply is given a minute inside its container, as node-converge's is.
TA="$(BOSS_TALOS_TIMEOUT_S="${BOSS_RETIRE_VOLUME_APPLY_TIMEOUT_S:-60}" ops_talosctl)"
# shellcheck disable=SC2086 # TA is one line the door prints to be word-split
if ! $TA -n "$NODE_ADDRESS" patch machineconfig --mode=no-reboot "--patch=$DELETE_DOC" < /dev/null > "$WORK/apply.out" 2> "$WORK/apply.err"; then
    mask < "$WORK/apply.err" | sed 's/^/    /' >&2
    node_fail "talosctl -n $NODE_ADDRESS patch machineconfig failed (its words above) — this run cannot say whether Talos removed UserVolumeConfig $VOL; read it with plan-a-user-volume-retirement $NODE $VOL (which refuses once the document is gone) before anything else"
fi
mask < "$WORK/apply.out" | sed 's/^/    /'
echo "$ME: Talos accepted the delete of UserVolumeConfig $VOL on $NODE — reading it back (up to ${WAIT_S}s: the live config, volumestatus $DISK, and partition $PART_LOC's label)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
while :; do
    elapsed=$(($(date -u +%s) - start))
    read_live after
    if [ "$GET_RC" -ne 0 ]; then
        cfg="the machine config did not read: $(mask < "$WORK/after.get" | tr '\n' ' ' | cut -c1-200)"
    elif [ "$CHECK_RC" -gt 1 ] || ! has_summary "$WORK/after.out"; then
        cfg="check-declared.sh exit $CHECK_RC: $(mask < "$WORK/after.err" | tr '\n' ' ' | cut -c1-200)"
    else
        cfg="UserVolumeConfig[$VOL] ${VERDICT:-unreported}"
    fi
    talos_get volumestatus "$DISK" back
    case $? in
        2) vs=gone ;;
        0) vs="still there, phase $(jq -r '.spec.phase // "unreported"' "$WORK/back.json")" ;;
        *) vs="unread: $TALOS_ERR" ;;
    esac
    talos_get discoveredvolumes "$PART_ID" backpart
    case $? in
        0) lbl="$(partition_label backpart)"; pfs="$(jq -r '.spec.name // "unreported"' "$WORK/backpart.json")" ;;
        2) lbl="gone"; pfs="" ;;
        *) lbl="unread: $TALOS_ERR"; pfs="" ;;
    esac
    if [ "$cfg" = "UserVolumeConfig[$VOL] ABSENT" ] && [ "$vs" = gone ] && [ "$lbl" = "$DISK" ]; then
        echo "$ME: $DISK on $NODE is retired with its data kept — read back: the live machine config holds no UserVolumeConfig $VOL, Talos holds no volume $DISK, and partition $PART_LOC on disk $PART_PARENT still carries partition label $lbl ($pfs, after ${elapsed}s)"
        exit 0
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "Talos accepted the delete of UserVolumeConfig $VOL on $NODE, and after ${elapsed}s the retirement is NOT proven: live config $cfg; volumestatus $DISK $vs; partition $PART_LOC label $lbl. Read it again with talos-get $NODE volumestatus $DISK"
    fi
    sleep "$POLL_S"
done
