# The plan document `move-forgejo-data.sh --plan` prints, and the exact
# bytes a passkey signs over (design 17835005 q1; backlog 016aeea3).
#
# ONE FILE, ONE DEFINITION. The script renders this file for --plan and
# renders it AGAIN, from the same observations, immediately before the
# write — and writes nothing unless the two hash alike. The tests run
# this file too, never a copy of it (§9a).
#
# WHAT IS IN THE BYTES: the act itself — the literal rsync argv, the
# rename target, the fstab line with its options, the order of every
# mutating command — from the SAME shell variables the write executes,
# so what was approved and what runs cannot come from two spellings
# (adversarial review of commission-a-disk, 2026-09-27: put the act in
# the plan). And every stable fact the approver needs to judge it: the
# source's top-level entries, the target's UUID, mount, fstab entry and
# free space.
#
# WHAT IS NOT: a clock, a run id, a container id, a kernel device name
# (kernel names move when a disk is added — commission-a-disk's
# 2026-09-21 lesson), and any byte or file count of a RUNNING Forgejo.
# The runner re-renders this plan when it executes, and Forgejo writes
# to its data every minute (sessions, queues, every train's registry
# push), so an exact count here would void every approval before it
# could run. The source's size is therefore a CEILING, rounded up to a
# whole bucket, and the write holds the measured size to it AFTER the
# stop, when the count means something; the exact per-directory counts
# of both sides are measured there and ride the request's output.
{
  plan_version: 1,
  verb: "move-forgejo-data",
  effect: ("Forgejo's data moves off the root filesystem onto the boss-data drive. "
           + $src + " keeps its path and becomes a bind mount of " + $tgt
           + ", so the compose file and every script that reads " + $src
           + " is unchanged. The old copy stays on / as " + $pre
           + " until a later, separate, approved step deletes it, so / gets its space back only then."),
  source: {
    path: $src,
    on_root_filesystem: true,
    is_a_mount: false,
    mounts_beneath: 0,
    top_level: $entries,
    size_at_most_gib: ($src_ceil_gib | tonumber),
    size_rule: ("the write measures the size after Forgejo stops and refuses one over "
                + $src_ceil_gib + " GiB (the ceiling rounds up to a " + $bucket_gib + " GiB step)")
  },
  target: {
    "label": $fslabel,
    uuid: $uuid,
    fstype: $fstype,
    mount: $mnt,
    fstab_entry: $uuid_line,
    backs_root: false,
    free_at_least_gib: ($free_gib | tonumber),
    margin_gib: ($margin_gib | tonumber),
    space_rule: "free space on the target must cover the source's measured size plus the margin, judged again after the stop",
    directory: $tgt,
    directory_state: $tgt_state,
    served_marker: $marker,
    served_marker_rule: "written immediately before the rename, from which point a Forgejo dockerd starts on its own may write to the directory, and removed again if the rename does not happen; a later move onto a marked directory is refused, because a rollback leaves Forgejo's writes there and --delete would destroy them",
    served_marker_record: "this plan's sha256, the converge stamp and the sha256 of its manifest as the before half read them — the manifest the rollback proves off the pre-move copy"
  },
  forgejo: {
    compose_file: $compose,
    service: $service,
    binds_source_at: "/data",
    running: true,
    restart_policy: $restart_policy,
    restart_rule: "dockerd may start the container again on its own under this policy, so its running state, StartedAt and RestartCount are read at the stop and must read the same immediately before the rename and again before the start",
    other_running_containers_mounting_source: 0,
    other_running_containers_rule: "a running container mounting the source, anything under it, or a directory above it is refused at the plan and again after the stop"
  },
  converge_hold: {
    file: $hold_file,
    reason: "forgejo-data-move-<first 12 hex of this plan's sha256>",
    rule: "placed before Forgejo stops unless a hold already stands; released after the proof only if it is this move's own; a failed proof keeps it"
  },
  act: [
    ("hold the converge: infra/forge/converge-hold.sh hold forgejo-data-move-<hash12>"),
    ("docker compose -f " + $compose + " stop -t 60 " + $service + ", then confirm it is not running and record its StartedAt and RestartCount"),
    ("read refs/heads/main of the repository with host git, off the disk, with " + $service + " stopped"),
    ("confirm no running container mounts " + $src + " and no host process holds anything under it"),
    ("measure " + $src + " (per top-level entry: files and bytes) and hold it to the ceiling and the target's free space"),
    ("mkdir " + $tgt + " (owner and mode of " + $src + ") when absent"),
    ($copy | join(" ")),
    ($verify | join(" ") + "  -- must list nothing"),
    ("measure " + $tgt + " the same way; every top-level entry's files and bytes must equal the source's"),
    ("write the new fstab to a temporary file beside " + $fstab + " (a newline first when the file lacks a trailing one), append the fstab line, and findmnt --verify --tab-file it"),
    ("confirm " + $service + " is still exactly as the stop left it (not running, same StartedAt and RestartCount)"),
    ("write " + $marker + " (this plan, the stamp, the manifest sha)"),
    ("mv -T " + $src + " " + $pre),
    ("mkdir " + $src + " (owner and mode of " + $pre + "), then chattr +i " + $src + " so an unmounted bind leaves an unwritable directory, never an empty Forgejo"),
    ("mv the verified temporary file over " + $fstab),
    "systemctl daemon-reload",
    ("mount --fstab " + $fstab + " " + $src + ", then findmnt must show UUID " + $uuid + " at filesystem root /" + $tgt_name),
    ("confirm " + $service + " is still exactly as the stop left it; if it is not, stop it and fail"),
    ("docker compose -f " + $compose + " start " + $service + ", which must be a real start (StartedAt moves)"),
    "the proof; a proof that fails stops Forgejo again",
    "release the converge hold if it is this move's own"
  ],
  copy: $copy,
  verify: $verify,
  rename_to: $pre,
  fstab_line: $fstab_line,
  proof: {
    healthz: $healthz,
    repository: ("refs/heads/main of " + $repo + ", read by host git off the disk after the stop and inside the container after the start; the two must be the same sha"),
    served_copy: ("the device and inode of " + $repo + " inside the container must be those of the same path under " + $src + " before anything is touched (a refusal, so a leg that cannot be read stops nothing), and under " + $tgt + " after the start — the one leg that tells the moved copy from " + $pre),
    registry: ("the manifest of " + $registry + ":<the converge's rollback target, its stamp>, read before the stop and after the start; the two bodies must hash alike, and the marker records both for the rollback"),
    root_df: "df on / before and after, printed; the space returns when the pre-move copy is deleted"
  },
  # Which layer of forge-repo-path.sh named the repository the proof
  # reads — derived from the compose file, or an override — so a path
  # that was named rather than derived is in the signed bytes (backlog
  # ed7702c3, finding 5).
  repository: {host_path: $repo_host, in_container: $repo, from: $repo_from},
  on_failure: ("before the rename: Forgejo is started again on the untouched " + $src
               + " and this move's hold released, exit 1. After it — and a run a signal stops once the rename has happened is after it: nothing is undone by hand or by guess; "
               + "the state is printed, the hold stays, a failed proof stops Forgejo again, exit 1, and the rollback is its own approved verb; so is the restart of a stopped Forgejo"),
  rollback: {plan_verb: "plan-a-forgejo-data-rollback", verb: "roll-back-forgejo-data-move"},
  restart: {plan_verb: "plan-a-forgejo-restart", verb: "restart-forgejo"},
  argv: ["infra/forge/move-forgejo-data.sh", "<sha256 of this plan>"]
}
