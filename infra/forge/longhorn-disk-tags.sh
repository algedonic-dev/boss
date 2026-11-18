#!/usr/bin/env bash
# longhorn-disk-tags.sh — set the tags of ONE Longhorn disk a worker
# already has, through a rendered plan a passkey signs. The ops verbs
# `plan-a-longhorn-disk-tags` (--plan) and `set-longhorn-disk-tags` (the
# write).
#
#   longhorn-disk-tags.sh --plan <node> <disk> <tags>
#   longhorn-disk-tags.sh <node> <disk> <tags> <plan-sha256>
#
# WHY IT EXISTS (backlog aa650aaf). Design 3c475a19 (disk placement for
# the incoming hardware) puts the system of record's volume on w-3's
# u-postgres, tagged [durable, postgres], durable replicas on u-data
# [durable], and tags every current default disk durable; its car 4 then
# selects with diskSelector [postgres]. Measured on origin/main 6b2bd7c48
# (2026-11-12T15:00Z, triage run c9fd27a7): add-longhorn-disk writes every
# disk with tags [], reregister-longhorn-disk re-adds the tags a record
# already had, and read-longhorn-node-tags only reads — so no door could
# set a disk's tags, and the design could not be carried out without the
# admin kubeconfig by hand. This is that door, for new u-<volume> disks
# and for the default disks alike, which is why it is a verb of its own
# and not a parameter of add-longhorn-disk.
#
# THE SHAPE is design 17835005's, as the rest of the longhorn-disk
# family's: the verb file declares requires_approval and names
# plan-a-longhorn-disk-tags as its plan_verb, so the runner renders this
# script's --plan onto the request's approve step and hands the write
# sha256 of the SIGNED plan. The write re-renders and patches nothing
# unless today's plan hashes to it.
#
# THE BOUND is this script's, never a parameter beyond the three it checks:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) as a talos-worker, and the cluster agrees — the registry's
#     address as its InternalIP and no control-plane label
#     (cluster-node-lib.sh, before any door opens; the-controls-are-bounded-
#     verbs holds every mutating node verb but node-converge to that). The
#     design's interim durable tag on the control planes' default disks is
#     NOT reachable here: D5 empties those disks, and a control plane's
#     disk is a design decision, not a packet argument.
#   * THE DISK IS ALREADY ONE OF THE NODE'S, by one of two names: `u-<volume>`
#     at exactly /var/mnt/<volume> (what add-longhorn-disk places), or the
#     node's ONE `default-disk-<hex>` at exactly /var/lib/longhorn (what
#     Longhorn made; a node with two is refused). A trailing slash aside,
#     the path is derived from the name and must be the disk's.
#   * THE TAGS are one of ADMITTED_TAGS, spelled exactly: design 3c475a19's
#     vocabulary, durable / durable,postgres / scratch, and `none` for the
#     untagged disk add-longhorn-disk writes (the way back). A scratch disk
#     is never also durable, and a postgres disk is always durable; any
#     other spelling, order or repeat is refused. longhorn_disk_tags_sh.rs
#     holds every class diskSelector in the tree to these words.
#   * Longhorn's node reads Ready=True, and Longhorn reads the disk itself
#     Ready=True and Schedulable=True with a diskUUID: a disk Longhorn does
#     not read has no read-back.
#   * THE TAGS CHANGE: a disk that already carries them is refused, which
#     is also how a second run of an applied plan is refused.
#   * NO PLACED REPLICA IS STRANDED. Longhorn moves nothing when a disk's
#     tags change, and will not rebuild a replica on a disk its volume's
#     diskSelector no longer selects. So every replicas.longhorn.io on the
#     disk — by its diskID, or by its node and a diskPath at or under the
#     disk's path — must belong to a volume whose spec.diskSelector the new
#     tags still hold. One that does not is refused, named; one naming a
#     volume the volume list does not hold cannot be judged (exit 1).
#     A volume with NO diskSelector is judged by Longhorn's setting
#     allow-empty-disk-selector-volume, READ here and never assumed
#     (review 7469ab28 B1): true, it places on a disk whatever its tags;
#     false, only on a disk with no tags, so tags that are not none strand
#     its replica exactly as a selector they stop holding does. The tree
#     never sets that setting, and a default is not a reading (the node
#     analog, backlog e14a472d). An unreadable setting is CANNOT ANSWER;
#     one Longhorn does not hold is refused, named.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, role and Node uid, the Longhorn node's disks now (their
# spec only), the disk with its path and diskUUID, its tags now and after,
# the value allow-empty-disk-selector-volume was read at, every volume
# with a replica on the disk and its diskSelector (what the verdict
# judged), the volumes with a diskSelector the new tags newly admit or no
# longer admit, what placement the tags change, the one patch and the
# read-back. No clock, no live free space and no replica name or count in
# the bytes (they move on their own; the count rides stderr). A volume
# gaining its first replica on the disk, or losing its last, between the
# render and the write is a different plan, and the write refuses it. The
# disks before are IN the bytes, so a second run of an applied plan
# re-renders to a refusal: at most once.
#
# THE WRITE is one `kubectl patch nodes.longhorn.io --type=json`: a `test`
# that spec.disks is still exactly the map the plan read, then a `replace`
# of spec.disks/<disk>/tags — an `add` when the record carries no `tags`
# key, which a `replace` would fail on; the `test` has already said which.
# A compare-and-set, so a disk changed between the render and the patch
# fails the patch rather than being overwritten. Nothing else is written.
# The `test` binds the disks, not the replica set: a replica Longhorn
# schedules onto the disk in the seconds between the write's re-render
# and its patch is not re-judged (review 7469ab28 N4).
#
# THE EFFECT IS READ BACK. Every BOSS_LONGHORN_DISK_POLL_S (default 10) up
# to BOSS_LONGHORN_DISK_WAIT_S (default 300, inside the verb's timeout):
# the Longhorn node. Only when spec.disks/<disk>/tags reads the planned
# tags, every other field of the disk and every other disk read as the
# plan did, and Longhorn's own status for the disk still reads Ready=True
# and Schedulable=True under the plan's diskUUID is the line the verb file
# declares as `effect` printed. Otherwise exit 1 saying the tags are NOT
# proven, with what was read; the patch stands, and its inverse is this
# verb again with the tags before.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal (the request was wrong,
# or is no longer true).
set -uo pipefail

ME=set-longhorn-disk-tags
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

LH=longhorn-system
# The one definition of what a packet may ask for (design 3c475a19's
# words); longhorn_disk_tags_sh.rs reads this line.
ADMITTED_TAGS='durable durable,postgres scratch none'
# add-longhorn-disk's disk name (u- and a Talos user volume name), and the
# name Longhorn gives a node's default disk.
U_RE='^u-([a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?)$'
DEFAULT_RE='^default-disk-[0-9a-f]+$'
# The Longhorn setting that decides where a volume with no diskSelector
# may place (review 7469ab28 B1).
EMPTY_SEL_SETTING=allow-empty-disk-selector-volume

unread() { node_fail "CANNOT ANSWER — $* — an unread disk has no plan, and nothing was written"; }

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 3 ] || node_refuse "usage: longhorn-disk-tags.sh --plan <node> <disk> <tags> — the plan takes the node, one of its Longhorn disks and the tags, and nothing else"
else
    [ $# -eq 4 ] || node_refuse "usage: longhorn-disk-tags.sh <node> <disk> <tags> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-longhorn-disk-tags verb)"
    [[ "$4" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$4'"
    APPROVED="$4"
fi
NODE="$1"
DISK="$2"
TAGS="$3"
node_check_id "$NODE"
if [[ "$DISK" =~ $U_RE ]]; then
    MNT="/var/mnt/${BASH_REMATCH[1]}"
elif [[ "$DISK" =~ $DEFAULT_RE ]]; then
    MNT=/var/lib/longhorn
else
    node_refuse "the disk must be u-<volume> (a disk add-longhorn-disk placed) or default-disk-<hex> (the node's default disk), got '$DISK'"
fi
admitted=0
for t in $ADMITTED_TAGS; do
    [ "$TAGS" = "$t" ] && admitted=1
done
[ "$admitted" -eq 1 ] || node_refuse "the tags must be one of: $ADMITTED_TAGS (exactly so spelled; none is the untagged disk), got '$TAGS'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — nothing can be read, and no evidence is not a plan"

if [ "$TAGS" = none ]; then
    WANT='[]'
else
    WANT="$(jq -c -n --arg t "$TAGS" '$t | split(",")')"
fi
# The tags as the plan and the effect line print them: [durable, postgres].
SAID="[$(jq -r -n --argjson w "$WANT" '$w | join(", ")')]"
WAIT_S="${BOSS_LONGHORN_DISK_WAIT_S:-300}"
POLL_S="${BOSS_LONGHORN_DISK_POLL_S:-10}"
case "${WAIT_S:-empty}${POLL_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_LONGHORN_DISK_WAIT_S and BOSS_LONGHORN_DISK_POLL_S must be whole seconds, got '$WAIT_S' and '$POLL_S'" ;;
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

# --- the plan -----------------------------------------------------------------
render() {
    local held disks path defaults cond now uid judged admits empty
    k_get lh.json nodes.longhorn.io "$NODE" -n "$LH"
    [ $? -eq 2 ] && node_refuse "Longhorn runs no node $NODE (nodes.longhorn.io $NODE in $LH is NotFound) — there is no disk here to tag"
    [ "$(jq -r '.metadata.name // ""' "$WORK/lh.json")" = "$NODE" ] || unread "nodes.longhorn.io $NODE did not read back as that node"
    # Every jq -e below asks this first (backlog d96e38ab: on jq-1.6 an
    # input with no document exits 0, and a guard handed silence passes).
    jq_doc_file "$WORK/lh.json" || unread "nodes.longhorn.io $NODE answered no document"
    jq -e '.spec.disks | type == "object"' "$WORK/lh.json" > /dev/null 2>&1 \
        || unread "nodes.longhorn.io $NODE carries no spec.disks map"
    [ "$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status][0] // "unreported"' "$WORK/lh.json")" = True ] \
        || node_refuse "Longhorn's node $NODE does not read Ready=True ($(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | "\(.status) \(.reason // "")"][0] // "unreported"' "$WORK/lh.json")) — a disk's tags set there could not be read back"

    disks="$(jq -r '[.spec.disks | keys[]] | join(", ")' "$WORK/lh.json")"
    jq -e --arg d "$DISK" '.spec.disks | has($d)' "$WORK/lh.json" > /dev/null \
        || node_refuse "Longhorn's node $NODE holds no disk $DISK (its disks: ${disks:-none}) — this verb tags a disk the node already has; add one with plan-a-longhorn-disk and add-longhorn-disk first"
    path="$(jq -r --arg d "$DISK" '.spec.disks[$d].path // ""' "$WORK/lh.json")"
    [ "${path%/}" = "$MNT" ] \
        || node_refuse "disk $DISK on $NODE is at '$path', not $MNT, the path its name derives — this verb tags only a disk add-longhorn-disk placed or the node's default disk"
    if [[ "$DISK" =~ $DEFAULT_RE ]]; then
        defaults="$(jq -r '[.spec.disks | keys[] | select(startswith("default-disk-"))] | join(", ")' "$WORK/lh.json")"
        [ "$defaults" = "$DISK" ] \
            || node_refuse "Longhorn's node $NODE holds more than one default disk ($defaults) — which one is the node's is a question for a person, not for this verb"
    fi

    # Longhorn's own reading of the disk.
    jq_doc_file "$WORK/lh.json" || unread "nodes.longhorn.io $NODE answered no document"
    jq -e --arg d "$DISK" '(.status.diskStatus // {}) | has($d)' "$WORK/lh.json" > /dev/null \
        || node_refuse "Longhorn reports no status for disk $DISK on $NODE — a disk Longhorn does not read cannot be read back"
    cond="$(jq -r --arg d "$DISK" '
        def cond($t): [(. // [])[] | select(.type == $t)][0] // {status: "unreported"};
        .status.diskStatus[$d] as $s
        | ($s.conditions | cond("Ready")) as $r | ($s.conditions | cond("Schedulable")) as $sc
        | if $r.status == "True" and $sc.status == "True" then "ok"
          else "Ready=\($r.status) (\($r.reason // "") \($r.message // "")) and Schedulable=\($sc.status) (\($sc.reason // ""))" end' "$WORK/lh.json")"
    [ "$cond" = ok ] || node_refuse "Longhorn reads disk $DISK on $NODE $cond — tags on a disk Longhorn does not read Ready and Schedulable could not be read back; repair the disk first"
    UUID="$(jq -r --arg d "$DISK" '.status.diskStatus[$d].diskUUID // ""' "$WORK/lh.json")"
    [ -n "$UUID" ] || node_refuse "Longhorn reads disk $DISK on $NODE with no diskUUID — a disk with no identity cannot be read back as the same disk"

    now="$(jq -c --arg d "$DISK" '.spec.disks[$d].tags // []' "$WORK/lh.json")"
    [ "$now" != "$WANT" ] \
        || node_refuse "disk $DISK on $NODE already carries tags $SAID — nothing to change (or this plan already ran)"
    NOW_SAID="[$(jq -r -n --argjson w "$now" '$w | join(", ")')]"
    # The tags before as this verb's argument spells them: the way back.
    NOW_ARG="$(jq -r -n --argjson w "$now" 'if $w == [] then "none" else $w | join(",") end')"

    # Whether a tagged disk takes replicas of a volume with no diskSelector
    # is Longhorn's allow-empty-disk-selector-volume, read the way
    # read-longhorn-setting reads a setting (the stored object, by name,
    # with a string value) and never assumed from Longhorn's default.
    k_get setting.json settings.longhorn.io "$EMPTY_SEL_SETTING" -n "$LH"
    [ $? -eq 2 ] && node_refuse "Longhorn holds no setting $EMPTY_SEL_SETTING (settings.longhorn.io $EMPTY_SEL_SETTING in $LH is NotFound) — whether a tagged disk still takes replicas of a volume with no diskSelector is not known on this Longhorn, and is not assumed"
    EMPTY_SEL="$(jq -r --arg s "$EMPTY_SEL_SETTING" 'if .metadata.name == $s and (.value | type == "string") then .value else "unread" end' "$WORK/setting.json")"
    case "$EMPTY_SEL" in
        true | false) ;;
        *) unread "settings.longhorn.io $EMPTY_SEL_SETTING reads '$EMPTY_SEL', neither true nor false (its first 200 bytes: '$(head -c 200 "$WORK/setting.json" | tr '\n' ' ')')" ;;
    esac

    # The replicas on the disk, and whether the new tags still hold each
    # one's volume's diskSelector — or, for a volume with none, whether
    # the setting lets a disk with those tags take it.
    k_get replicas.json replicas.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "replicas.longhorn.io answered NotFound"
    k_get volumes.json volumes.longhorn.io -n "$LH"
    [ $? -eq 2 ] && unread "volumes.longhorn.io answered NotFound"
    jq -c --slurpfile vols "$WORK/volumes.json" --arg n "$NODE" --arg u "$UUID" --arg m "$MNT" --argjson want "$WANT" \
        --argjson anywhere "$EMPTY_SEL" '
        def norm: (. // "" | sub("/+$"; ""));
        ([$vols[0].items[] | {key: .metadata.name, value: (.spec.diskSelector // [])}] | from_entries) as $sel
        | [.items[] | . as $r
           | select(($r.spec.diskID // "") == $u
                    or (($r.spec.nodeID // "") == $n
                        and (($r.spec.diskPath | norm) as $p | $p == $m or ($p | startswith($m + "/")))))
           | ($r.spec.volumeName // "") as $v
           | ($sel[$v] // []) as $s
           | {replica: $r.metadata.name, volume: $v, known: ($sel | has($v)),
              selects: $s,
              strands: (if $s == [] then ($anywhere | not) and $want != [] else $s - $want != [] end)}]' "$WORK/replicas.json" > "$WORK/held.json" 2> "$WORK/held.jq" \
        || unread "the replica and volume lists would not parse: $(tr '\n' ' ' < "$WORK/held.jq" | cut -c1-200)"
    held="$(jq -r '[.[] | select(.known | not) | "replica \(.replica) names volume \(.volume)"] | join("; ")' "$WORK/held.json")"
    [ -z "$held" ] || unread "$held, which the volume list does not hold — a replica whose volume cannot be read cannot be judged"
    held="$(jq -r --arg w "$SAID" --arg s "$EMPTY_SEL_SETTING" '[.[] | select(.strands)
        | if .selects == []
          then "replica \(.replica) of volume \(.volume), which names no diskSelector, and with \($s) false Longhorn places it only on a disk with no tags"
          else "replica \(.replica) of volume \(.volume), whose diskSelector \(.selects | tojson) tags \($w) would not hold" end] | join("; ")' "$WORK/held.json")"
    [ -z "$held" ] || node_refuse "retagging $DISK on $NODE would strand what is placed on it: $held — Longhorn moves nothing when tags change and will not rebuild there; move the replica first (move-volume-replica) or keep the tag"
    jq -c -S '.spec.disks' "$WORK/lh.json" > "$WORK/disks.json"

    # What the signer reads (review 7469ab28 N2, N3): every volume the
    # verdict judged, and the volumes with a diskSelector whose placement
    # on this disk the tags change — adding a tag widens a disk too.
    judged="$(jq -r 'unique_by(.volume) | .[] | "  volume \(.volume): diskSelector \(.selects | tojson)"' "$WORK/held.json")"
    admits="$(jq -r --argjson now "$now" --argjson want "$WANT" '
        [.items[] | {v: .metadata.name, s: (.spec.diskSelector // [])} | select(.s != [])] | sort_by(.v)
        | [(.[] | select((.s - $want == []) and (.s - $now != [])) | "newly admit volume \(.v) (diskSelector \(.s | tojson))"),
           (.[] | select((.s - $now == []) and (.s - $want != [])) | "no longer admit volume \(.v) (diskSelector \(.s | tojson)), which has no replica here")]
        | if . == [] then "admit exactly the volumes with a diskSelector the tags now admit" else join("; ") end' "$WORK/volumes.json")"
    if [ "$EMPTY_SEL" = true ]; then
        empty="A volume with no diskSelector places on this disk whatever its tags, because $EMPTY_SEL_SETTING reads true."
    elif [ "$WANT" = '[]' ]; then
        empty="$EMPTY_SEL_SETTING reads false, so a volume with no diskSelector places only on a disk with no tags: untagged, this disk takes such replicas again."
    else
        empty="$EMPTY_SEL_SETTING reads false, so a volume with no diskSelector places only on a disk with no tags: tagged $SAID, this disk takes no replica of one, and holds none now."
    fi

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    {
        echo "plan: set-longhorn-disk-tags"
        echo "node: $NODE"
        echo "address: $NODE_ADDRESS"
        echo "estate role: $NODE_ROLE"
        echo "kubernetes node uid: $uid"
        echo
        echo "== longhorn node $NODE: its disks now (spec; allowScheduling on the node: $(jq -r '.spec.allowScheduling' "$WORK/lh.json")) =="
        jq -r '.spec.disks | to_entries | sort_by(.key)[]
            | "disk \(.key): path \(.value.path), allowScheduling \(.value.allowScheduling), evictionRequested \(.value.evictionRequested // false), storageReserved \(.value.storageReserved // 0) bytes, tags \(.value.tags // [] | tojson)"' "$WORK/lh.json"
        echo
        echo "disk: $DISK at $path, diskUUID $UUID, which Longhorn reads Ready=True and Schedulable=True"
        echo "tags now: $NOW_SAID"
        echo "tags after: $SAID"
        echo "longhorn setting $EMPTY_SEL_SETTING: $EMPTY_SEL (settings.longhorn.io in $LH, read with this plan)"
        echo "replicas: every replica on $DISK belongs to a volume the tags after still place there (a volume's diskSelector is the tags a disk must carry for Longhorn to place its replicas there). The volumes with a replica on $DISK:"
        echo "${judged:-  none}"
        echo "placement: tags move nothing that is already placed. A volume whose diskSelector names a tag places replicas only on disks carrying every tag it names; on this disk, the tags after $admits. $empty No node's allowScheduling and no other disk changes."
        echo "command: kubectl patch nodes.longhorn.io $NODE -n $LH --type=json — test that spec.disks is still exactly the disks above, set spec.disks/$DISK/tags to $SAID; the one field this writes. Nothing is moved and nothing is deleted."
        echo "read-back: until spec.disks/$DISK/tags reads $SAID, every other field of $DISK and every other disk reads as above, and Longhorn's status for $DISK reads Ready=True and Schedulable=True under diskUUID $UUID."
        echo "disks before (the compare-and-set's value): $(cat "$WORK/disks.json")"
    } > "$WORK/plan"
}

render
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
jq -r --arg me "$ME" --arg d "$DISK" '.status.diskStatus[$d] | "\($me): live, not signed: disk \($d) storageScheduled \(.storageScheduled // "unreported") and storageAvailable \(.storageAvailable // "unreported") bytes of \(.storageMaximum // "unreported")"' "$WORK/lh.json" >&2
echo "$ME: live, not signed: $(jq 'length' "$WORK/held.json") replica(s) on $DISK" >&2

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    node_refuse "today's plan for $DISK on $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the node's disks, the tags asked for or what is placed on the disk moved since the plan was signed. Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — setting the tags of Longhorn disk $DISK on $NODE to $SAID"

PATCH="$(jq -c -n --slurpfile d "$WORK/disks.json" --arg k "$DISK" --argjson want "$WANT" '
    [{op: "test", path: "/spec/disks", value: $d[0]},
     {op: (if $d[0][$k] | has("tags") then "replace" else "add" end),
      path: "/spec/disks/\($k)/tags", value: $want}]')" \
    || node_fail "could not compose the patch — nothing was written"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch nodes.longhorn.io "$NODE" -n "$LH" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    node_fail "kubectl patch nodes.longhorn.io $NODE did not succeed (its words above) — this run cannot say the tags were set; read the node again with plan-a-longhorn-disk-tags before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: Longhorn accepted tags $SAID for disk $DISK on $NODE — reading it back (up to ${WAIT_S}s: the disk's tags, every other field and disk unchanged, and Longhorn's Ready and Schedulable for $DISK)"

# --- the read-back ------------------------------------------------------------
start="$(date -u +%s)"
said="not read yet"
while :; do
    # shellcheck disable=SC2086
    if $K get nodes.longhorn.io "$NODE" -n "$LH" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
        && jq_doc_file "$WORK/back.json"; then
        said="$(jq -r --slurpfile before "$WORK/disks.json" --arg k "$DISK" --arg u "$UUID" --argjson want "$WANT" '
            def cond($t): [(. // [])[] | select(.type == $t)][0] // {status: "unreported"};
            (.spec.disks // {}) as $d
            | ($d[$k] // null) as $mine
            | (.status.diskStatus // {})[$k] as $st
            | if $mine == null then "spec.disks holds no \($k)"
              elif ($mine.tags // []) != $want then "spec.disks/\($k)/tags reads [\(($mine.tags // []) | join(", "))]"
              elif ($mine | del(.tags)) != ($before[0][$k] | del(.tags)) then "another field of \($k) changed: \($mine | del(.tags) | tojson) against \($before[0][$k] | del(.tags) | tojson)"
              elif ($d | del(.[$k])) != ($before[0] | del(.[$k])) then "another disk on the node changed: \($d | del(.[$k]) | tojson) against \($before[0] | del(.[$k]) | tojson)"
              elif $st == null then "Longhorn reports no status for \($k)"
              elif ($st.diskUUID // "") != $u then "Longhorn reads \($k) under diskUUID \($st.diskUUID // "none"), not the planned \($u)"
              else ($st.conditions | cond("Ready")) as $r | ($st.conditions | cond("Schedulable")) as $sc
                | if $r.status != "True" or $sc.status != "True"
                  then "Longhorn reads \($k) Ready=\($r.status) (\($r.reason // "") \($r.message // "")) and Schedulable=\($sc.status) (\($sc.reason // ""))"
                  else "OK \([$d | keys[] | select(. != $k)] | length)" end
              end' "$WORK/back.json" 2> "$WORK/back.jq")" || said="the Longhorn node would not parse: $(tr '\n' ' ' < "$WORK/back.jq" | cut -c1-200)"
    else
        said="the Longhorn node did not read: $(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)"
    fi
    elapsed=$(($(date -u +%s) - start))
    case "$said" in
        "OK "*)
            # The tags as the packet spelled them (one of ADMITTED_TAGS):
            # a bracket in this line would read as a class to the pin
            # that holds the declared effect to text this script prints.
            echo "$ME: $NODE has Longhorn disk $DISK tagged $TAGS — read back: Ready=True, Schedulable=True, diskUUID $UUID (unchanged), and its ${said#OK } other disk(s) unchanged (after ${elapsed}s)"
            exit 0 ;;
    esac
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        node_fail "Longhorn accepted tags $SAID for disk $DISK on $NODE, and after ${elapsed}s they are NOT proven: $said. The patch stands — read it with read-longhorn-node-tags $NODE, and if they must go, the tags before are $ME $NODE $DISK $NOW_ARG"
    fi
    sleep "$POLL_S"
done
