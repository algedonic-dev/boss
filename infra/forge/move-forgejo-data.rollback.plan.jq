# The plan document `move-forgejo-data.sh --plan-rollback` prints: what
# `roll-back-forgejo-data-move` WOULD do to put Forgejo back on the
# pre-move copy, and the bytes a passkey signs (backlog 016aeea3).
#
# Rendered by ONE command for --plan-rollback and the write alike, as
# the move's plan is (§9a), and carrying the same things: the act from
# the variables the write executes, and no clock, run id, container id
# or count of a running Forgejo.
#
# THE ROLLBACK READS THE STATE, NOT A SCRIPT OF THE MOVE. A move that
# failed after its rename leaves one of several states — renamed but
# not yet in fstab, in fstab but not mounted, mounted and running — and
# a rollback that assumed the finished one would fail half-way through
# a repair. So each undo step is named with whether it applies, from
# what was observed, and the steps that do not apply are not run.
{
  plan_version: 1,
  verb: "roll-back-forgejo-data-move",
  effect: ("Forgejo goes back onto " + $pre + ", renamed to " + $src
           + ". Anything Forgejo wrote since the move (pushes, registry uploads, issues) stays in "
           + $tgt + " and is NOT copied back; that directory is left in place, untouched."),
  source: {
    path: $src,
    state: $src_state,
    mounted: ($mounted == "true"),
    mount_uuid: (if $mounted == "true" then $mount_uuid else null end)
  },
  pre_move_copy: {path: $pre, on_root_filesystem: true},
  moved_copy: $tgt,
  # The drive, found through its own by-UUID fstab line from the
  # pre-move name. Absent, its fstab errors are the only ones the
  # rollback's findmnt --verify tolerates.
  drive: {
    uuid: (if $drive_uuid == "" then null else $drive_uuid end),
    present: (if $drive_present == "" then null else ($drive_present == "true") end)
  },
  fstab_line: (if $fstab_line == "" then null else $fstab_line end),
  forgejo: {
    compose_file: $compose,
    service: $service,
    binds_source_at: "/data",
    running: ($running == "true"),
    restart_policy: $restart_policy
  },
  converge_hold: {
    file: $hold_file,
    reason: "forgejo-data-rollback-<first 12 hex of this plan's sha256>",
    rule: "placed first unless a hold already stands; released after the proof only if it is this rollback's own; a failed proof keeps it"
  },
  act: [
    {step: "hold the converge", applies: true},
    {step: ("docker compose -f " + $compose + " stop -t 60 " + $service + ", then confirm it is not running and record its StartedAt and RestartCount"),
     applies: ($running == "true")},
    {step: ("umount " + $src + " (up to three tries, each refusal naming its holders with fuser -vm), then confirm it is no longer a mount"),
     applies: ($mounted == "true")},
    {step: ("remove exactly the line " + $fstab_line + " from " + $fstab
            + " through a temporary file checked with findmnt --verify --tab-file (only the absent drive's own errors tolerated), then systemctl daemon-reload"),
     applies: ($fstab_line != "")},
    {step: ("chattr -i " + $src + ", then rmdir " + $src + " (it must be empty)"),
     applies: ($src_state != "absent")},
    {step: ("mv -T " + $pre + " " + $src), applies: true},
    {step: ("confirm " + $service + " is still as the stop left it, then docker compose -f " + $compose + " start " + $service + ", which must be a real start"),
     applies: true},
    {step: "the proof; a proof that fails stops Forgejo again", applies: true},
    {step: "release the converge hold if it is this rollback's own", applies: true}
  ],
  proof: {
    healthz: $healthz,
    repository: ("refs/heads/main of " + $repo + " must be readable inside the container after the start"),
    served_copy: ("the device and inode of " + $repo + " inside the container must be those of the same path under " + $src),
    # The stamp THE MOVE recorded, never the current one: after a later
    # converge the current image exists only in the moved copy, and its
    # manifest would 404 off the copy this puts back (backlog 85d29e33).
    registry: (if $prove_stamp == ""
               then ("NOT PROVED, reported: " + $prove_why)
               else ("the manifest of " + $registry + ":" + $prove_stamp + " must be served after the start and hash to "
                     + $prove_manifest + ", as the move recorded it off this copy in " + $record)
               end)
  },
  # Which layer of forge-repo-path.sh named the repository the proof
  # reads, so a path that was named rather than derived is in the signed
  # bytes (backlog ed7702c3, finding 5).
  repository: {host_path: $repo_host, in_container: $repo, from: $repo_from},
  registry_record: (if $prove_stamp == "" then null
                    else {file: $record, stamp: $prove_stamp, manifest_sha256: $prove_manifest} end),
  on_failure: "any failure stops and prints what stands, and a failed proof stops Forgejo again; restart-forgejo (plan-a-forgejo-restart) starts a stopped Forgejo under its own approved plan",
  argv: ["infra/forge/move-forgejo-data.sh", "--rollback", "<sha256 of this plan>"]
}
