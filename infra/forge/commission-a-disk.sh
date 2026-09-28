#!/usr/bin/env bash
# commission-a-disk — turn a raw block device into mounted capacity.
#
#   commission-a-disk.sh --plan <device-by-id> /srv/<name>
#   commission-a-disk.sh <device-by-id> /srv/<name> <plan-sha256>
#
# MUTATING, AND THE MOST DESTRUCTIVE THING IN THIS TREE: it writes a
# partition table, a filesystem and /etc/fstab. Everything below exists
# to make the WRONG TARGET UNREPRESENTABLE rather than merely
# disapproved, which is the claim design 17835005 turns on — an approval
# that authorises "format the disk you meant" is weaker than a command
# that cannot name the disk you did not.
#
# WHY THAT IS NOT PARANOIA. On 2026-09-21 a second NVMe went into the
# forge and the kernel RENUMBERED the devices: the new blank drive came
# up as nvme0n1 and the live root filesystem moved to nvme1n1p2. The
# disk that looked like "the new one" by number was the one carrying the
# system. The host booted only because fstab resolves by UUID. Any
# operator or agent reaching for a kernel name that day would have been
# aiming at the wrong device while believing otherwise.
#
# SO THE TARGET IS A STABLE NAME FOR A WHOLE RAW DISK, and every
# precondition is checked IMMEDIATELY BEFORE the write, never once at
# plan time:
#
#   1. the target is a /dev/disk/by-id/ name (a kernel name is refused
#      outright — it is not a stable identity), and not one ending in
#      -partN, which names a partition;
#   2. it resolves to a block device lsblk types as `disk` — not a
#      partition, a device-mapper node (LUKS, LVM), an md array or a
#      loop device, each of which is part of somebody's storage;
#   3. it has no partitions and wipefs finds NO signature on it — no
#      filesystem written straight onto it, no LUKS header, no LVM, md
#      or ZFS membership, no empty partition table;
#   4. nothing on it is mounted, and it is not in the chain of devices
#      backing / (lsblk --inverse, by kernel name, not a string prefix);
#   5. the mount path is one new directory under /srv — empty or absent,
#      not a symlink, not mounted on, not already an fstab target — and
#      /etc/fstab verifies as it stands, so a later verify failure can
#      only be this verb's own line.
#
# Findings 1 to 3 of the adversarial review of car 0556935a (2026-09-27)
# are why 1-5 read as they do: a by-id `...-part1` of the root NVMe, or
# an unmounted dm or md node, had passed every check this verb had and
# reached parted, and the mount path was unbounded. The judgements of 2-5
# live in commission-a-disk.judge.sh, sourced here, so a test can run
# every one of them on facts it chooses — no test box has a block device.
#
# A refusal is exit 78 (EX_CONFIG, `die`): the request was wrong or no
# longer true, and NOTHING WAS WRITTEN. Exit 1 (`fail`) is a step that
# genuinely failed — a fact that could not be read, or anything after
# the first write, when the host has already changed and the record must
# not say otherwise.
#
# --plan RENDERS AND DOES NOT ACT (design 17835005, answered by David
# 2026-09-21). It evaluates exactly the preconditions the write path
# evaluates, observes the same facts, and prints a PLAN DOCUMENT: the
# resolved target, what was observed, and the act — the parted, mkfs
# and mount argv and the fstab line, rendered from the SAME arrays the
# write runs. Nothing is written. It is the "plan" of plan-then-approve,
# and it is what a passkey signs over — q1 settled that the signature
# binds a rendered plan rather than a verb call, because a verb call
# authorises an intent whose target can still resolve differently at
# execution time, which is exactly how the device renumbering would
# have gone wrong.
#
# THE PLAN IS HASHED OVER ITS OWN BYTES, so there is one definition of
# what was approved and no canonicalisation to drift (§9a). That is why
# this document carries NO timestamp and nothing else that varies
# between two renders of the same true state — the render time belongs
# on the packet, outside what is signed. Two plans of the same disk in
# the same state are byte-identical; if any OBSERVED fact moves, the
# bytes move with it, which is the drift q4 says must void an approval.
# Its sha256 rides stderr as `plan-sha256: <hex>`, because a hash cannot
# be inside the bytes it hashes, and the ops runner refuses a plan verb
# whose stdout does not hash to the value it names.
#
# THE WRITE RE-RENDERS AND COMPARES (backlog b2d5b546, 2026-09-26). The
# runner hands this script sha256 of the SIGNED plan as its last arg,
# and refused this verb outright — it was INERT — while the script took
# no hash, because a write that cannot compare could act on a state the
# signature never saw. So the write path renders the plan with the same
# command `--plan` uses, from the same args, immediately before the
# write, and writes nothing unless those bytes hash to the approved
# value. The same shape as merge-tenant-main.sh and
# reap-terminated-pods.sh.
#
# THE FSTAB EDIT CANNOT LEAVE THE HOST UNBOOTABLE (review finding 2).
# The entry is by UUID and carries nofail with a ten-second device
# timeout, so a disk that later fails to appear delays the boot by
# seconds instead of stopping it; the edit is made on a copy in /etc
# whose last line is terminated first, verified with findmnt, and only
# then renamed over /etc/fstab; and only the one new mount is mounted,
# never `mount -a`.
set -euo pipefail

ME=commission-a-disk
die() { echo "$ME: $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
# A WRONG DEVICE IS REFUSED WITH THE RIGHT ONES (backlog 88eda2eb,
# 2026-09-27): learning the new drive's by-id name took a person running
# ls on the host. So a device refusal carries what list-block-devices
# marks as candidates — marked by the judge this script sources, from
# facts read the way this script reads them. It only reads, it runs
# before anything is written, and a listing that cannot run leaves the
# refusal exactly as it was, still exit 78.
die_device() {
    local c rc=0
    c=$(bash "$(dirname "$0")/list-block-devices.sh" --candidates 2>/dev/null) || rc=$?
    if [ -n "$c" ]; then
        c="the disks this verb would accept (list-block-devices):
$(printf '%s\n' "$c" | sed 's/^/    /')"
    else
        c="list-block-devices marks no disk this verb would accept"
    fi
    [ "$rc" -eq 0 ] || c="$c
    (the listing could not read every fact, so it may be short — the list-block-devices verb prints what it could not read)"
    die "$1
  $c"
}

# THE ACT, defined once. The plan renders these and the write runs them.
TABLE=gpt
FS=ext4
START=0%
END=100%
LABEL=boss-data
FSTAB_OPTS="defaults,noatime,nofail,x-systemd.device-timeout=10s"

PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi

if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 2 ] || die "usage: commission-a-disk.sh --plan <device-by-id> /srv/<name>"
else
    [ $# -eq 3 ] || die "usage: commission-a-disk.sh <device-by-id> /srv/<name> <plan-sha256> — this writes only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-disk-commission verb)"
    APPROVED="$3"
    case "$APPROVED" in
      *[!0-9a-f]*) die "the plan hash must be 64 lowercase hex characters, got '$APPROVED'" ;;
    esac
    [ ${#APPROVED} -eq 64 ] || die "the plan hash must be 64 lowercase hex characters, got '$APPROVED'"
fi
BY_ID="$1"
MOUNT="$2"

# ---- the request's shape: refused before anything is resolved --------
case "$BY_ID" in
  /dev/disk/by-id/*) : ;;
  *) die_device "target must be a /dev/disk/by-id/ path — '$BY_ID' is not a stable identity, and kernel names move when a disk is added (measured on the forge, 2026-09-21)" ;;
esac
suffix=${BY_ID##*-part}
if [ "$suffix" != "$BY_ID" ]; then
    case "$suffix" in
      ''|*[!0-9]*) : ;;
      *) die_device "'$BY_ID' names a partition, not a whole disk — this verb commissions only a whole raw disk, and a partition is part of somebody's" ;;
    esac
fi
# One new directory under /srv, the shape the verb file's pattern holds
# too: no traversal, no nesting, no other root (review finding 3).
name=""
case "$MOUNT" in /srv/*) name=${MOUNT#/srv/} ;; esac
case "$name" in
  ''|[!A-Za-z0-9]*|*[!A-Za-z0-9._-]*)
    die "mount path must be /srv/<name>, one component of letters, digits, '.', '_' or '-' starting with a letter or digit — got '$MOUNT'" ;;
esac
[ ${#name} -le 41 ] || die "mount path must be /srv/<name> with a name of at most 41 characters — got '$MOUNT'"

# ---- the device --------------------------------------------------------
[ -e "$BY_ID" ] || die_device "no such device: $BY_ID"
DEV=$(readlink -f "$BY_ID") || die "cannot resolve $BY_ID"
[ -b "$DEV" ] || die "$BY_ID resolves to $DEV, which is not a block device"

for t in lsblk wipefs blockdev findmnt blkid jq sha256sum parted "mkfs.$FS" udevadm systemctl mount; do
    command -v "$t" >/dev/null 2>&1 \
        || fail "$t is not on this host — the facts this verb judges cannot be read or its act cannot run, and no evidence is not a pass"
done
# shellcheck source=commission-a-disk.judge.sh
. "$(dirname "$0")/commission-a-disk.judge.sh"

# EVERY FACT FAILS CLOSED (review finding 5): a read that fails is exit
# 1 naming the read, never an empty string judged as "nothing there".
dtype=$(lsblk -dno TYPE "$DEV") || fail "lsblk could not read the type of $DEV"
knames=$(lsblk -nro KNAME "$DEV") || fail "lsblk could not list $DEV"
parts=$(( $(printf '%s\n' "$knames" | awk 'NF {n++} END {print n+0}') - 1 ))
sigs=$(wipefs -n -i -O TYPE "$DEV") || fail "wipefs could not read the signatures on $DEV"
mps=$(lsblk -nro MOUNTPOINTS "$DEV") || fail "lsblk could not read the mounts on $DEV"
mounted=$(printf '%s\n' "$mps" | awk 'NF {n++} END {print n+0}')
root_src=$(findmnt -nvro SOURCE /) || fail "findmnt could not read the source of /"
[ -b "$root_src" ] \
    || fail "/ is mounted from '$root_src', which is not a block device — which disk backs it cannot be established, so $DEV cannot be ruled out"
root_chain=$(lsblk -nrso KNAME "$root_src") || fail "lsblk could not trace what backs / ($root_src)"
backs_root=$(printf '%s\n' "$root_chain" | awk -v d="${DEV##*/}" '$1 == d {f = 1} END {print f ? "yes" : "no"}')
size=$(blockdev --getsize64 "$DEV") || fail "blockdev could not read the size of $DEV"
case "$size" in ''|*[!0-9]*) fail "blockdev read '$size' as the size of $DEV" ;; esac

why=$(disk_refusal "$DEV" "$dtype" "$parts" "$sigs" "$mounted" "$backs_root") || die_device "$why"

# ---- the mount path ----------------------------------------------------
if [ -L "$MOUNT" ]; then m_state=symlink
elif [ -d "$MOUNT" ]; then m_state=dir
elif [ -e "$MOUNT" ]; then m_state=other
else m_state=absent
fi
m_entries=0
if [ "$m_state" = dir ]; then
    listing=$(ls -A -- "$MOUNT") || fail "cannot list $MOUNT"
    m_entries=$(printf '%s' "$listing" | awk 'END {print NR}')
fi
targets=$(findmnt -rno TARGET) || fail "findmnt could not list the mounts"
m_mp=$(printf '%s\n' "$targets" | awk -v m="$MOUNT" '$0 == m {f = 1} END {print f ? "yes" : "no"}')
[ -r /etc/fstab ] || fail "/etc/fstab is not readable"
m_fstab=$(awk -v m="$MOUNT" '$1 !~ /^#/ && NF >= 2 { t = $2; sub(/\/+$/, "", t); if (t == m) f = 1 } END {print f ? "yes" : "no"}' /etc/fstab) \
    || fail "cannot read /etc/fstab"

why=$(mount_refusal "$MOUNT" "$m_state" "$m_entries" "$m_mp" "$m_fstab") || die "$why"

verify=$(findmnt --verify --tab-file /etc/fstab 2>&1) \
    || die "/etc/fstab does not verify as it stands, so a failure after this verb's edit could not be told apart from what is already wrong — fix it first: $verify"

# ---- the act, as arrays: run by the write, rendered into the plan -----
PART="${BY_ID}-part1"
PARTED=(parted -s "$BY_ID" mklabel "$TABLE" mkpart "$LABEL" "$FS" "$START" "$END")
MKFS=("mkfs.$FS" -F -q -L "$LABEL" "$PART")
MOUNT_CMD=(mount "$MOUNT")
fstab_line() { printf '%s' "UUID=$1 $MOUNT $FS $FSTAB_OPTS 0 2"; }

# ---- the plan, rendered ONCE for both paths --------------------------
# Reached only with every precondition holding: a plan for a target that
# cannot be commissioned is not a plan, it is a refusal, and the `die`
# calls above have already made it one.
#
# ONE render command for --plan and the write alike, so the bytes a
# passkey signed and the bytes compared below cannot come from two
# definitions (§9a). Rendered to a FILE, never through $( ), which would
# strip the trailing newline the runner hashed. The template is a FILE
# too, so the test runs the bytes the script runs.
WORK=$(mktemp -d)
FSTAB_TMP=""
trap 'rm -rf "$WORK"; if [ -n "$FSTAB_TMP" ]; then rm -f "$FSTAB_TMP"; fi' EXIT
jq -n --arg by_id "$BY_ID" --arg dev "$DEV" --arg mount "$MOUNT" --arg size "$size" \
      --arg dtype "$dtype" --arg parts "$parts" --arg sigs "$sigs" --arg mounted "$mounted" \
      --arg backs_root "$backs_root" --arg m_state "$m_state" --arg m_entries "$m_entries" \
      --arg m_mp "$m_mp" --arg m_fstab "$m_fstab" \
      --arg fstab_line "$(fstab_line '<uuid of the new filesystem>')" \
      --argjson parted "$(as_json "${PARTED[@]}")" --argjson mkfs "$(as_json "${MKFS[@]}")" \
      --argjson mount_cmd "$(as_json "${MOUNT_CMD[@]}")" \
      -f "$(dirname "$0")/commission-a-disk.plan.jq" > "$WORK/plan"
HASH=$(sha256sum "$WORK/plan" | cut -d' ' -f1)

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# ---- the write, only under the plan that was approved ----------------
# The preconditions above were evaluated moments ago, by this run, and
# the render carries what they observed: a match means the disk and the
# mount path are in exactly the state the approver saw.
[ "$HASH" = "$APPROVED" ] || {
    cat "$WORK/plan" >&2
    die "the plan now hashes to $HASH, not the approved $APPROVED — something it names has moved since it was rendered (above: the plan as it stands). Nothing was written; render and approve it again"
}
[ "$(readlink -f "$BY_ID")" = "$DEV" ] \
    || die "$BY_ID resolved to $DEV a moment ago and does not now — nothing was written"
# The capture before the write: what ran rides the request's output.
cat "$WORK/plan"
echo "$ME: plan $HASH still holds — writing to $BY_ID ($DEV)"

# FROM HERE ON THE HOST HAS CHANGED, so every failure is `fail` (exit 1)
# and names what state it left (review finding 6).
"${PARTED[@]}" || fail "parted failed on $BY_ID; the disk may carry a partial label, and nothing else was written"
udevadm settle --timeout=30 || fail "udevadm settle did not finish after partitioning $BY_ID"
[ -e "$PART" ] || fail "partition did not appear at $PART after partitioning"
PART_DEV=$(readlink -f "$PART") || fail "cannot resolve $PART"
parent=$(lsblk -dno PKNAME "$PART_DEV") || fail "lsblk could not read the parent of $PART_DEV"
[ "$parent" = "${DEV##*/}" ] \
    || fail "$PART resolves to $PART_DEV, whose parent is '$parent', not ${DEV##*/} — it was not formatted"
"${MKFS[@]}" || fail "mkfs failed on $PART; the disk is partitioned, and fstab was not touched"
udevadm settle --timeout=30 || fail "udevadm settle did not finish after mkfs on $PART"
UUID=$(blkid -s UUID -o value "$PART_DEV") || fail "cannot read the new filesystem's UUID on $PART_DEV"
[ -n "$UUID" ] || fail "the new filesystem on $PART_DEV has no UUID"
mkdir -p "$MOUNT" || fail "cannot create $MOUNT"

# The fstab edit: a copy in /etc (so the rename is atomic, one
# filesystem), its last line terminated, the new line appended, the copy
# verified, then renamed over the original. A failure before the rename
# leaves /etc/fstab exactly as it was.
FSTAB_TMP=$(mktemp /etc/.fstab.commission-a-disk.XXXXXX) || fail "cannot create a copy of /etc/fstab in /etc"
cp -p /etc/fstab "$FSTAB_TMP" || fail "cannot copy /etc/fstab"
if [ -n "$(tail -c 1 "$FSTAB_TMP")" ]; then
    printf '\n' >> "$FSTAB_TMP"
fi
{ fstab_line "$UUID"; printf '\n'; } >> "$FSTAB_TMP"
out=$(findmnt --verify --tab-file "$FSTAB_TMP" 2>&1) \
    || fail "the edited fstab does not verify, so /etc/fstab was left as it was (the disk is formatted, UUID=$UUID, and not mounted): $out"
mv -f "$FSTAB_TMP" /etc/fstab || fail "cannot rename the edited copy over /etc/fstab"
FSTAB_TMP=""
systemctl daemon-reload || fail "systemctl daemon-reload failed; /etc/fstab now names $MOUNT"
"${MOUNT_CMD[@]}" || fail "mount $MOUNT failed; /etc/fstab names it, with nofail"
got=$(findmnt -rno UUID --mountpoint "$MOUNT") || fail "nothing is mounted at $MOUNT after mounting it"
[ "$got" = "$UUID" ] || fail "$MOUNT is mounted from UUID '$got', not the new $UUID"

echo "$ME: $MOUNT is live on UUID=$UUID"
df -h "$MOUNT"
