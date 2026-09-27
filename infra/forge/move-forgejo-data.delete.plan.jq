# The plan document `move-forgejo-data.sh --plan-delete` prints: what
# `delete-forgejo-premove-copy` WOULD delete, and the bytes a passkey
# signs (backlog 192a9003).
#
# Rendered by ONE command for --plan-delete and the write alike, as the
# move's plan is (§9a): the act from the arrays the write executes, and
# no clock, run id, container id or reading of a running Forgejo. The
# pre-move copy is not served, so its size and entry count are exact —
# and a copy that changed after the signature voids it.
#
# WHAT IS DELETED IS DERIVED, NEVER NAMED BY A PARAM. The path is the
# move's own rename target, derived the way the move derived it — the
# first 8 hex of the UUID of the drive the live bind is on — and the
# only other name it accepts is that path's `deleting-` twin, which
# only this verb makes, an instant before it deletes.
{
  plan_version: 1,
  verb: "delete-forgejo-premove-copy",
  effect: ("Deletes " + $victim + " from the root filesystem, " + $bytes + " bytes in " + $entries
           + " entries. It is the copy the move left behind; Forgejo serves " + $tgt + " through the bind at " + $src
           + ". After this, roll-back-forgejo-data-move has no copy to put back: " + $tgt
           + " on the boss-data drive is the only copy of Forgejo's data outside a backup."),
  delete: {
    path: $victim,
    state: $victim_state,
    pre_move_name: $pre,
    deleting_name: $doomed,
    allocated_bytes: ($bytes | tonumber),
    entries: ($entries | tonumber),
    top_level: $top,
    on_filesystem: "/"
  },
  moved_copy: {
    path: $tgt,
    mount: $mnt,
    uuid: $uuid,
    drive_fstab_line: $uuid_line,
    bind: $src,
    bind_fstab_line: $fstab_line
  },
  # When the move went live, and the soak after it: the window in which
  # the rollback could still want the pre-move copy.
  served_marker: {
    file: $marker,
    record: {plan: $marker_plan, stamp: $marker_stamp, manifest: $marker_manifest},
    written_at: $marked_at,
    soak_hours: ($soak_hours | tonumber),
    deletable_from: $deletable_from
  },
  forgejo: {compose_file: $compose, service: $service, binds_source_at: "/data", running: true},
  refused_while: [
    "Forgejo is not running, or its healthz does not pass",
    ("the container's /data is not " + $tgt + " by device and inode, or is " + $victim + "'s"),
    ("the repository the container serves is not the one under " + $tgt + " by device and inode"),
    "the manifest of the converge's current stamp is not served by the registry",
    ($src + " is not mounted as the bind of /" + $tgt_name + " on UUID " + $uuid + ", or " + $fstab + " does not carry exactly the move's bind line and the drive's own line"),
    ("the served marker is missing, is not a move's record, or is younger than " + $soak_hours + " hours"),
    "the converge is held by a move or a rollback that did not finish",
    ("more than one copy matches, or the one found is not exactly the move's rename target, a real directory, on /, and not a mount or under one"),
    ("a running container mounts it or anything under it, or a host process holds anything under it")
  ],
  act: [
    {step: ("rename: " + ($rename | join(" "))), applies: ($rename | length > 0),
     why: "atomic: from here no rollback plan can find a pre-move copy, so a copy half deleted is never put back"},
    {step: ("remove: " + ($remove | join(" "))), applies: true},
    {step: ("confirm " + $doomed + " is gone, then that / has at least " + $need + " more bytes free than before (re-read up to six times while the filesystem settles)"), applies: true},
    {step: "the proof, read again: healthz, and the container's /data and repository by device and inode", applies: true}
  ],
  rename: $rename,
  remove: $remove,
  free_space: {filesystem: "/", must_grow_by_at_least_bytes: ($need | tonumber),
               slack: "the larger of a tenth of the size and the floor, for what else writes to / meanwhile"},
  proof: {
    healthz: $healthz,
    served_copy: ("the container's /data and " + $repo + " must be " + $tgt + " and the same path under it, by device and inode"),
    registry: ("the manifest of " + $registry + ":<the converge's current stamp> must be served — the rollback target, which the registry prune always keeps")
  },
  repository: {host_path: $repo_host, in_container: $repo, from: $repo_from},
  never: "Forgejo is never stopped or started, the converge is never held, and nothing but the one path above is deleted",
  on_failure: ("a failure before the rename changes nothing (exit 78); after it, exit 1 with what stands printed, and a remainder at "
               + $doomed + " is finished by rendering this plan again"),
  argv: ["infra/forge/move-forgejo-data.sh", "--delete", "<sha256 of this plan>"]
}
