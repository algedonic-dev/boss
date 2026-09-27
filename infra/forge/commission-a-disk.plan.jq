# The plan document `commission-a-disk --plan` prints, and the exact
# bytes a passkey signs over (design 17835005, q1).
#
# IT LIVES IN ITS OWN FILE so there is ONE definition of the shape: the
# script runs this file, and the test runs this file. An inline program
# would have to be extracted to be tested, and an extracted copy is a
# copy (§9a).
#
# NOTHING HERE MAY VARY BETWEEN TWO RENDERS OF THE SAME TRUE STATE.
# No timestamp, no hostname, no run id. The plan is hashed over its own
# bytes, so anything that moves on its own breaks an approval that is
# still valid; and conversely, if any OBSERVED fact moves, the bytes
# move with it, which is the drift that q4 says must void an approval.
# The render time belongs on the packet, outside what is signed.
#
# Version 2 (the adversarial review of car 0556935a, 2026-09-27): the
# observed facts include what the whole-disk and mount-path judgements
# read, and `act` carries the argv of every command the write runs and
# the fstab line it adds — handed in by the script from the same arrays
# it runs, so the approver signs the act, not only the target.
{
  plan_version: 2,
  verb: "commission-a-disk",
  target_by_id: $by_id,
  resolves_to: $dev,
  size_bytes: ($size | tonumber),
  mount_path: $mount,
  # The write's argv as the runner builds it: the hash is this plan's
  # own, appended after the signature, so it can only be named here.
  argv: ["infra/forge/commission-a-disk.sh", $by_id, $mount, "<sha256 of this plan>"],
  observed: {
    device_type: $dtype,
    partition_count: ($parts | tonumber),
    signatures: [$sigs | splits("\n") | select(length > 0)],
    mounted_filesystems: ($mounted | tonumber),
    backs_root: ($backs_root == "yes"),
    mount_path: {
      state: $m_state,
      entries: ($m_entries | tonumber),
      is_mountpoint: ($m_mp == "yes"),
      in_fstab: ($m_fstab == "yes")
    },
    fstab_verifies: true
  },
  act: {
    partition: $parted,
    mkfs: $mkfs,
    fstab_line: $fstab_line,
    fstab_edit: ("a copy of /etc/fstab made in /etc, its last line terminated, the line above "
                 + "appended, the copy checked with findmnt --verify, then renamed over /etc/fstab"),
    mount: $mount_cmd
  },
  effect: ("a GPT label, one ext4 partition labelled boss-data spanning the disk, "
           + "an /etc/fstab entry BY UUID that cannot stop a boot (nofail), and the "
           + "filesystem mounted at " + $mount)
}
