# commission-a-disk.judge.sh — the refusals commission-a-disk.sh makes
# from what it OBSERVED, as functions of those facts alone. Sourced; it
# runs nothing on its own.
#
# WHY A SEPARATE FILE (the adversarial review of car 0556935a,
# 2026-09-27). Observing the facts needs a raw block device under
# /dev/disk/by-id/, and no test box has one — the dev pod and the gate
# pod have no block device at all (measured 2026-09-27: `find /dev -type
# b` is empty). The JUDGEMENT needs none. So the judgement lives here,
# the script sources it, and the test sources the same file and runs
# every refusal for real — one definition, not a copy the test keeps in
# step with the script (§9a).
#
# Each function prints the refusal and returns 1, or prints nothing and
# returns 0. EVERY FACT FAILS CLOSED: a count that is not a number, or a
# yes/no that is not exactly "no", is a refusal — never read as zero.

# disk_refusal <device> <lsblk TYPE> <partitions> <signatures> <mounted> <backs-root>
#   <partitions>  how many children lsblk lists under the device
#   <signatures>  what `wipefs` found on the whole device, one per line
#   <mounted>     how many mountpoints lsblk lists on it and its children
#   <backs-root>  "no" unless the device is in the chain backing /
disk_refusal() {
    case "$1" in '') echo "no device named"; return 1 ;; esac
    # A partition, a device-mapper node (LUKS, LVM), an md array or a
    # loop device is PART of somebody's storage: its own children count
    # and its own mounts say nothing about the disk it lives on. Found by
    # the review: a `...-part1` of an unmounted partition, or an
    # unmounted dm or md node, passed every check this verb had.
    if [ "$2" != disk ]; then
        echo "$1 is a '${2:-unknown type}', not a whole disk — a partition, a device-mapper, md or loop node is part of somebody's storage; this verb commissions only a whole raw disk"
        return 1
    fi
    case "$3" in ''|*[!0-9]*) echo "cannot count the partitions on $1 (read '$3')"; return 1 ;; esac
    if [ "$3" -ne 0 ]; then
        echo "$1 already carries $3 partition(s) — a disk with partitions is somebody's disk; this verb only commissions a raw one"
        return 1
    fi
    # A whole disk with no partitions can still hold data: a filesystem
    # written straight onto it, a LUKS header, an LVM physical volume, an
    # md or ZFS member, or a partition table with no entries yet.
    if [ -n "$4" ]; then
        echo "$1 carries a signature ($(printf '%s' "$4" | tr '\n' ' ')) — a filesystem, LUKS, LVM, RAID or ZFS member, or a partition table, is somebody's data whether or not it is mounted"
        return 1
    fi
    case "$5" in ''|*[!0-9]*) echo "cannot count the mounts on $1 (read '$5')"; return 1 ;; esac
    if [ "$5" -ne 0 ]; then
        echo "$1 has $5 mounted filesystem(s) — refusing"
        return 1
    fi
    if [ "$6" != no ]; then
        echo "$1 backs the root filesystem — refusing"
        return 1
    fi
    return 0
}

# as_json <word>... — the words as one JSON array, exactly: how the
# arrays the write runs reach the plan's `act`. Here rather than in the
# script so the test runs the one serialiser the script uses. The `--`
# is load-bearing: jq goes on parsing its own options among --args
# words, so without it `parted -s` rendered as `parted` (measured on jq
# 1.6, 2026-09-27 — `-s` taken as --slurp) and `mkfs -F -q -L` lost
# flags the write passes.
as_json() { jq -cn '$ARGS.positional' --args -- "$@"; }

# mount_refusal <mount path> <state> <entries> <is-mountpoint> <in-fstab>
#   <state>          absent | dir | symlink | other
#   <entries>        how many entries the directory holds (0 when absent)
#   <is-mountpoint>  "no" unless something is mounted there now
#   <in-fstab>       "no" unless /etc/fstab already names it as a target
mount_refusal() {
    case "$2" in
      absent|dir) : ;;
      symlink) echo "$1 is a symlink — a mount path must be the directory it names, not a pointer to another one"; return 1 ;;
      *) echo "$1 exists and is not a directory (${2:-unknown})"; return 1 ;;
    esac
    case "$3" in ''|*[!0-9]*) echo "cannot count what is in $1 (read '$3')"; return 1 ;; esac
    if [ "$3" -ne 0 ]; then
        echo "$1 is not empty ($3 entries) — mounting over it would hide them"
        return 1
    fi
    if [ "$4" != no ]; then
        echo "$1 is already a mountpoint — refusing to mount over it"
        return 1
    fi
    if [ "$5" != no ]; then
        echo "$1 is already an fstab target — two entries for one path, and the other one wins at boot"
        return 1
    fi
    return 0
}
