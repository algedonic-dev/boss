# The plan document `move-forgejo-data.sh --plan-restart` prints: what
# `restart-forgejo` WOULD do to start a STOPPED Forgejo, and the bytes a
# passkey signs (backlog 85d29e33).
#
# WHY IT EXISTS. A proof that fails — the move's or the rollback's —
# stops Forgejo, because a Forgejo whose data a run cannot vouch for
# must not take pushes. Until this verb nothing could start it again
# but a root shell on the forge. It starts it on exactly what the source
# is as observed: the move's bind, or a directory on the root
# filesystem; every other state is refused before a plan exists.
#
# Rendered by ONE command for --plan-restart and the write alike, as
# the move's and the rollback's plans are (§9a): no clock, run id or
# container id in the bytes, and the one act from the variables the
# write executes.
{
  plan_version: 1,
  verb: "restart-forgejo",
  effect: ("Forgejo starts again on " + $src + ", which is " + $src_state
           + ". Nothing else changes: no data moves, fstab and the mounts stay as they are, and the converge hold is left as it is found."),
  source: {
    path: $src,
    state: $src_state,
    mounted: ($mounted == "true"),
    mount_uuid: (if $mounted == "true" then $mount_uuid else null end)
  },
  forgejo: {
    compose_file: $compose,
    service: $service,
    binds_source_at: "/data",
    running: false,
    restart_policy: $restart_policy
  },
  # WHAT A MOVE LEFT BEHIND (backlog ed7702c3, finding 2): the typical
  # restart follows a move whose proof failed, and the copy it starts
  # Forgejo on then never passed that proof. The plan says so from what
  # it read — any pre-move copy, the moved copy's served marker and its
  # record, the hold's contents — rather than leaving it to be inferred.
  pre_move_copies: $pre_copies,
  served_marker: (if $marker == "" then null
                  else {file: $marker,
                        present: ($marker_state != "absent"),
                        record: (if $marker_state == "record"
                                 then {plan: $marker_plan, stamp: $marker_stamp, manifest_sha256: $marker_manifest}
                                 elif $marker_state == "absent" then null
                                 else "present, but not the three-line record a move writes" end)}
                  end),
  converge_hold: {
    file: $hold_file,
    holds: (if $hold_state == "held" then $hold_body else null end),
    rule: "neither placed nor released: a hold the move or the rollback kept stays until release-converge lifts it"
  },
  unfinished_run: (if $unfinished == "" then null else $unfinished end),
  # Which layer of forge-repo-path.sh named the repository the proof
  # reads (backlog ed7702c3, finding 5).
  repository: {host_path: $repo_host, in_container: $repo, from: $repo_from},
  act: [
    ("confirm " + $service + " is not running, and record its StartedAt and RestartCount"),
    ("docker compose -f " + $compose + " start " + $service + ", which must be a real start (StartedAt moves)"),
    ("wait for " + $healthz + " to pass")
  ],
  proof: {
    started: "running, with a StartedAt other than the one read before the start",
    healthz: $healthz,
    reported: ("refs/heads/main of " + $repo + " inside the container, and the device and inode of " + $repo
               + " inside it against the same path under " + $src
               + " — printed; a leg that cannot be read or differs is a FINDING, never a stop, and any FINDING makes the run exit 1: started, not proven")
  },
  on_failure: "a start that fails or is not a real start, a healthz that does not pass, or a reported leg that is a FINDING, is exit 1 with what stands printed; this verb never stops Forgejo",
  argv: ["infra/forge/move-forgejo-data.sh", "--restart", "<sha256 of this plan>"]
}
