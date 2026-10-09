//! Every MUTATING ops verb declares how a run of it SHOWS ITS EFFECT —
//! the line its script prints only after it has read back what it
//! changed — or says, in its own file, that it cannot yet.
//!
//! WHY (backlog fdbb447e part 1, design 3036296f mechanism B, approved
//! by David 2026-09-27). The day's reds and review holds were mostly one
//! class: silence that reads as success. The daily prune's proof read
//! the ops-request's OUTCOME and counted a refused run as proof; the
//! data move's proof could not tell the old copy from the new one. An
//! ops-request closes `answered` whenever its verb RAN, and `exit_code`
//! says only that the script did not fail — neither says that the thing
//! the verb exists to change changed. So:
//!
//! - a verb file declares `effect`, the regex of that read-back line
//!   (or, for a dry run, of the line saying it changed nothing);
//! - `infra/ops/ops-runner.sh` judges it ONCE, on the run's own output,
//!   and records `effect` / `effect_unproven` beside `exit_code` on the
//!   execute step (pinned by `ops_runner_sh.rs`);
//! - every reader takes that verdict: `boss ops --wait` fails an exit 0
//!   whose effect was not shown, and `verb_failure` (the answered-ops-
//!   request judges) treats it as a failure.
//!
//! A verb whose script has NO read-back yet declares `effect_unread`, a
//! reason naming what would read it, and the runner copies that reason
//! onto every clean run — loud, never refused (DR rule 62dac114: no new
//! refusal on a human path before one-person DR is proven). That set is
//! a RATCHET: [`EFFECT_UNREAD`] names every verb admitted without a
//! read-back on the day this landed, measured script by script; a new
//! MUTATING verb cannot join it, and a verb that gains a read-back must
//! leave it. It only shrinks.
//!
//! WHICH VERBS. Derived, not listed: a verb whose `about` says MUTATING
//! or that declares `requires_approval` — the predicate
//! `boss-cli::mutating_verb::is_mutating` and the lint
//! `the-controls-are-bounded-verbs.sh` both read.

use boss_testing::repo_root;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

/// The MUTATING verbs admitted with no read-back of their effect, as
/// measured on 2026-09-29 (origin/main a25d5db4), each with its reason
/// in its own verb file. Only shrinks: see the module doc.
/// retire-second-stack joined in this car, before landing: its adversarial
/// review found its read-back checks "stopped" and never "disabled".
/// It, converge and mirror-base-images left together (backlog 1058e686,
/// cars C+D+E), each for a read-back of its own; rollback-to and
/// delete-orphan-object left before them (cars A and B).
/// A slice, not an array, so a car that removes one entry touches only
/// that entry's line and two such cars ride one train (backlog 1058e686).
const EFFECT_UNREAD: &[&str] = &["publish-github-pr"];

/// Lines each verb's script prints BEFORE, or instead of, the change it
/// exists to make — the plan-still-holds line before the write, a mode
/// the verb's argv cannot reach, a legacy path that does not read back.
/// None may match that verb's `effect`: a regex loose enough to match
/// one would certify a run that changed nothing (the adversarial review
/// of this car mutated reclaim-gcp-root's effect to its plan line and
/// every other test here still passed). Each line is the script's own
/// format string with its values filled in. Every effect-declaring verb
/// is also checked against its own `FAILED — ` and `REFUSED — ` lines
/// (see the test).
const NOT_EFFECTS: &[(&str, &str)] = &[
    // trim-build-node (backlog 30a6bf39), at the head as the entries
    // below are: the bounds holding and the create's answer (a Job made
    // is not a trim run), and a trim that succeeded whose Job could not
    // be read back gone.
    (
        "trim-build-node",
        "trim-build-node: every bound holds — CronJob boss-node-trim on the server is the one this checkout declares (image reg/david/alpine-k8s:1.33.3@sha256:00, deadline 1800s), binding node-maintenance-holds-one-workload denies, no trim in flight. Creating Job boss-node-trim-manual-20261008010000 from it.",
    ),
    (
        "trim-build-node",
        "trim-build-node: created Job boss-node-trim-manual-20261008010000 (uid 6f1e9b99-0000-4000-8000-000000000000)",
    ),
    (
        "trim-build-node",
        "trim-build-node: FAILED — the trim itself SUCCEEDED (Job boss-node-trim-manual-20261008010000 completed, pod exit 0, both mounts trimmed, closing OK) — but Job boss-node-trim-manual-20261008010000 was NOT proven removed — read it: kubectl -n boss-node-maintenance get job boss-node-trim-manual-20261008010000",
    ),
    // expand-instance-volume (backlog ebbb923f), at the head beside the
    // drain-policy car's: the plan-still-holds line, the API server's
    // acceptance of the patch (what a verb reading kubectl's exit code
    // alone would call success), and the read-back's stall — the kubelet's
    // half of an online resize not done.
    (
        "expand-instance-volume",
        "expand-instance-volume: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — growing boss/pgdata-postgres-0 from 30Gi to 40Gi",
    ),
    (
        "expand-instance-volume",
        "expand-instance-volume: the API server accepted spec.resources.requests.storage=40Gi for boss/pgdata-postgres-0 — reading it back (up to 600s: status.capacity.storage, and Longhorn's spec.size)",
    ),
    (
        "expand-instance-volume",
        "expand-instance-volume: FAILED — the API server accepted spec.resources.requests.storage=40Gi for boss/pgdata-postgres-0, and after 601s it is NOT proven: status.capacity.storage 30Gi, spec.resources.requests.storage 40Gi, conditions FileSystemResizePending=True; Longhorn volume pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 spec.size 42949672960, robustness healthy. The request stands — FileSystemResizePending is the kubelet's half not done yet; read it again with plan-an-instance-volume-expansion, whose refusal names a stalled expansion",
    ),
    // retire-ops-runner (backlog 98eb9349, design a79a8067), at the head
    // for the reason the entry below gives: the plan-still-holds line,
    // the stop itself (systemctl's exit, not a reading), the dry run's
    // verdict (a plan verb's, never this write's), and each way the
    // read-back refuses to vouch.
    (
        "retire-ops-runner",
        "retire-ops-runner: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds",
    ),
    (
        "retire-ops-runner",
        "retire-ops-runner: DRY RUN — every bound passed; would stop+disable boss-ops-runner.timer on forge. Nothing was stopped.",
    ),
    (
        "retire-ops-runner",
        "retire-ops-runner: FAILED — disable --now returned success and boss-ops-runner.timer still answers is-enabled enabled, so it would start again at the next boot. The marker was removed; this verb will not try again.",
    ),
    (
        "retire-ops-runner",
        "retire-ops-runner: FAILED — disable --now returned success and boss-ops-runner.timer answers is-enabled disabled but is-active active, so the door is still open. The marker stands: the disable is this request's act, and the converge credits it if the timer stops.",
    ),
    (
        "retire-ops-runner",
        "retire-ops-runner: FAILED — disable --now exited 1, yet boss-ops-runner.timer reads off (is-enabled disabled, is-active inactive). The marker stands, so the converge credits the off timer to ops-request 5eed0000-1111-4222-8333-444455556666 (RETIRED by it), not to a hand; this run is recorded FAILED because its one act did not report success.",
    ),
    (
        "retire-ops-runner",
        "retire-ops-runner: CANNOT ANSWER — disable --now returned success but systemctl gave no reading of boss-ops-runner.timer afterwards (is-enabled: ; is-active: ), so it is not proven off. The marker stands: the converge's own reading settles it.",
    ),
    // set-longhorn-drain-policy (backlog 46584350), at the head so it
    // does not share the tail line with the volume-replicas car: the
    // plan-still-holds line, Longhorn's acceptance of the patch, and the
    // read-back's refusal when the API never reports the value applied.
    (
        "set-longhorn-drain-policy",
        "set-longhorn-drain-policy: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — setting node-drain-policy from block-if-contains-last-replica to allow-if-replica-is-stopped",
    ),
    (
        "set-longhorn-drain-policy",
        "set-longhorn-drain-policy: Longhorn accepted node-drain-policy=allow-if-replica-is-stopped — reading it back (up to 90s: the stored setting, and Longhorn's API applying it)",
    ),
    (
        "set-longhorn-drain-policy",
        "set-longhorn-drain-policy: FAILED — Longhorn accepted node-drain-policy=allow-if-replica-is-stopped, and after 91s it is NOT proven: settings.longhorn.io value allow-if-replica-is-stopped, Longhorn's API value allow-if-replica-is-stopped, applied false. The patch stands — read it again with check-longhorn-drain-policy",
    ),
    (
        "reclaim-gcp-root",
        "reclaim-gcp-root: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds",
    ),
    (
        "reclaim-gcp-root",
        "reclaim-gcp-root: DRY RUN — every bound passed: 2 backup directories (300 MiB), 1 second-stack capture(s) (9 bytes) and the journal beyond 1G. Nothing was removed.",
    ),
    (
        "commission-a-disk",
        "commission-a-disk: plan 0123456789abcdef still holds — writing to /dev/disk/by-id/nvme-x (/dev/nvme1n1)",
    ),
    (
        "merge-tenant-main",
        "merge-tenant-main: plan 0123456789abcdef still holds — landing 0123456789abcdef0123456789abcdef01234567 on algedonic/tenant main (was 89abcdef0123456789abcdef0123456789abcdef)",
    ),
    (
        "move-forgejo-data",
        "move-forgejo-data: plan 0123456789abcdef still holds",
    ),
    (
        "move-forgejo-data",
        "move-forgejo-data: OK — Forgejo serves from /opt/forgejo on the root filesystem again; /srv/fj is left in place",
    ),
    (
        "restart-forgejo",
        "move-forgejo-data: plan 0123456789abcdef still holds",
    ),
    (
        "roll-back-forgejo-data-move",
        "move-forgejo-data: plan 0123456789abcdef still holds",
    ),
    (
        "roll-back-forgejo-data-move",
        "move-forgejo-data: OK — Forgejo serves from /srv/fj through /opt/forgejo",
    ),
    (
        "delete-forgejo-premove-copy",
        "move-forgejo-data: plan 0123456789abcdef still holds",
    ),
    (
        "github-create-repository",
        "github-act: plan 0123456789abcdef still holds",
    ),
    (
        "github-delete-refs",
        "github-act: plan 0123456789abcdef still holds",
    ),
    (
        "github-set-branch-protection",
        "github-act: plan 0123456789abcdef still holds",
    ),
    (
        "github-disable-actions",
        "github-act: plan 0123456789abcdef still holds",
    ),
    (
        "github-disable-actions",
        "github-act: PUT /repos/algedonic-dev/boss-dr/actions/permissions answered 204",
    ),
    // A repository created and read back is not the whole act: its Actions
    // must read back disabled too (backlog e727fcfd).
    (
        "github-create-repository",
        "github-act: proven — GET /repos/algedonic-dev/boss-dr reads back as algedonic-dev/boss-dr, private true, fork false",
    ),
    (
        "reap-terminated-pods",
        "reap-terminated-pods: plan 0123456789abcdef still holds — reaping 3 pod(s)",
    ),
    // delete-orphan-object (backlog 1058e686, car B): the line before the
    // delete, the OK it printed before gone meant NotFound, and the two
    // ways the read-back refuses to vouch.
    (
        "delete-orphan-object",
        "delete-orphan-object: deleting Service `boss-docs-internal` in `boss` — undeclared by infra/cluster/manifests at 0123456",
    ),
    (
        "delete-orphan-object",
        "delete-orphan-object: OK — Service `boss-docs-internal` is gone from `boss`, and no manifest declared it.",
    ),
    (
        "delete-orphan-object",
        "delete-orphan-object: CANNOT ANSWER — deleted, not proven gone: the delete of Service/boss/boss-docs-internal returned success, and the read-back answered something other than NotFound:",
    ),
    (
        "delete-orphan-object",
        "delete-orphan-object: the delete returned success and Service/boss/boss-docs-internal is STILL THERE — something is recreating it.",
    ),
    (
        "prune-registry-versions-daily",
        "prune-registry-versions: DRY RUN — would delete 12 version(s) across boss boss-ci; the keep set held 10 sha(s), latest, and 24 h; judged. Nothing was deleted.",
    ),
    (
        "retire-example-reference-rows",
        "retire-example-reference-rows: for-real namespace=boss db=boss tenant=t declared=0 candidates=3 present=3 deletable=3 kept=0 deleted=3 FAILED",
    ),
    (
        "hold-converge",
        "converge-hold: HELD — carried from /home/david/.converge-hold (owned by uid 1000, 1 link(s)), contents not read — release-converge lifts it (/var/lib/boss/converge.hold)",
    ),
    (
        "release-converge",
        "converge-hold: NOT released — /var/lib/boss/converge.hold still stands (was: train-800)",
    ),
    (
        "publish-drift",
        "publish-drift: not yet: checkout at cb053ed6, main at 4cb3d3a7 — nothing compared, nothing published",
    ),
    (
        "cut-recovery-kit",
        "INCOMPLETE — kit 0123456789ab is cut and writable, and 2 item(s) are MISSING (named below). A stick written from it is named -INCOMPLETE and restores everything else.",
    ),
    (
        "cut-recovery-kit",
        "recovery-kit: replacing kit 0123456789ab, which was still in /run/boss-recovery-kit",
    ),
    (
        "cut-recovery-kit",
        "recovery-kit: REFUSED — MANIFEST.txt does not read back as vouching for kit.tar (sha256 00) — nothing is kept",
    ),
    (
        "discard-recovery-kit",
        "recovery-kit: kit 0123456789ab discarded; /run/boss-recovery-kit unmounted",
    ),
    (
        "discard-recovery-kit",
        "recovery-kit: could not unmount /run/boss-recovery-kit — its contents were removed; a reboot clears it",
    ),
    (
        "discard-recovery-kit",
        "recovery-kit: FAILED — kit 0123456789ab is NOT discarded: /run/boss-recovery-kit still holds /run/boss-recovery-kit/kit.tar",
    ),
    // rollback-to (backlog 1058e686, car A): the lines before the patch,
    // the rollout's own Ready (what the verb used to print as success),
    // and each way the read-back refuses to vouch.
    (
        "rollback-to",
        "rollback-to: deploy/boss serves registry.example/boss:bad0000 — rolling to registry.example/boss:abc1234",
    ),
    (
        "rollback-to",
        "rollback-to: deploy/boss is already on registry.example/boss:abc1234 — applying it again, then reading it back",
    ),
    (
        "rollback-to",
        "rollback-to: rollout status says deploy/boss is Ready on registry.example/boss:abc1234 — reading the image back",
    ),
    (
        "rollback-to",
        "rollback-to: deploy/boss is Ready on registry.example/boss:abc1234",
    ),
    (
        "rollback-to",
        "rollback-to: Ready pod(s) of replicaset boss-7d9f, name@imageID: boss-7d9f-x@sha256:feed",
    ),
    (
        "rollback-to",
        "rollback-to: FAILED — the rollback to registry.example/boss:abc1234 was applied and rollout status said Ready, but replicaset boss-7d9f has no Ready pod; deploy/boss is NOT proven restored; hands needed",
    ),
    (
        "rollback-to",
        "rollback-to: CANNOT ANSWER — the rollback to registry.example/boss:abc1234 was applied and rollout status said Ready, but deploy/boss could not be read back: refused ; deploy/boss is not proven restored",
    ),
    (
        "rollback-to",
        "rollback-to: registry.example/boss:abc1234 never went Ready — deploy/boss is NOT restored; hands needed",
    ),
    // retire-second-stack (backlog 1058e686, car C): the OK line as it
    // read before is-enabled was read back, the per-unit lines, and each
    // way the new read-back refuses to vouch.
    (
        "retire-second-stack",
        "retire-second-stack: OK — stopped+disabled 54 units of the second stack on boss-gcp, in the order above; captured first at /var/backups/boss/second-stack/second-stack-20260915T211123Z.sql. Unit files, binaries and the database are untouched.",
    ),
    (
        "retire-second-stack",
        "retire-second-stack: plan: 54 of 54 listed units are present",
    ),
    (
        "retire-second-stack",
        "retire-second-stack: FAILED — disable --now returned success and 1 unit(s) are STILL active: boss-docs-api.service",
    ),
    (
        "retire-second-stack",
        "retire-second-stack: FAILED — stopped 54 units, but 1 still answer is-enabled with something other than disabled, masked or static, so each would start again at the next boot: boss-docs-api.service=enabled",
    ),
    (
        "retire-second-stack",
        "retire-second-stack: CANNOT ANSWER — stopped 54 units, but systemctl is-enabled gave no answer for 1, so they are not proven disabled: boss-jobs-api.service (Failed to get unit file state: Connection timed out )",
    ),
    // converge (backlog 1058e686, car D): the accepted-start line that
    // used to be all it printed, the before-read, and every way the
    // invocation read-back declines to claim a new run.
    (
        "converge",
        "converge-now: cluster-deploy-runner.service started (--no-block); its verdict lands on the maintenance-cluster-converge packet and on this request",
    ),
    (
        "converge",
        "converge-now: cluster-deploy-runner.service start accepted (--no-block) — reading the new invocation back",
    ),
    (
        "converge",
        "converge-now: before the start, cluster-deploy-runner.service is inactive (invocation 0123456789abcdef0123456789abcdef)",
    ),
    (
        "converge",
        "converge-now: not yet: cluster-deploy-runner.service is still invocation 0123456789abcdef0123456789abcdef (activating) 20s after the start — a converge already running took this start into itself, so no new run began; this request is noted, and the next run answers it",
    ),
    (
        "converge",
        "converge-now: FAILED — the run this start began (invocation fedcba9876543210fedcba9876543210) has already failed: Result=exit-code; its stage and exit land on the maintenance-cluster-converge packet",
    ),
    (
        "converge",
        "converge-now: FAILED — the run this start began (invocation fedcba9876543210fedcba9876543210) has already ended: ActiveState=inactive, Result=timeout",
    ),
    (
        "converge",
        "converge-now: CANNOT ANSWER — the start was accepted, but cluster-deploy-runner.service could not be read back: Failed to connect to bus: Connection refused",
    ),
    // mirror-base-images (backlog 1058e686, car E): the done line as it
    // read on exit codes alone, the per-image lines, and each refusal.
    (
        "mirror-base-images",
        "mirror-base-images: done — 10 image(s) mirrored to 10.20.0.15:3000/david",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: done — 1 image(s) mirrored to 10.20.0.15:3000/david (9 already in the registry)",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: docker.io/oven/bun:1.3-slim  ->  10.20.0.15:3000/david/bun:1.3-slim",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: read back 10.20.0.15:3000/david/bun:1.3-slim — the registry's tag serves sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef, the digest the pulled docker.io/oven/bun:1.3-slim records for it",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: FAILED — the push of 10.20.0.15:3000/david/bun:1.3-slim exited 0, but the registry's tag serves a different manifest than 10.20.0.15:3000/david/bun@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef, the digest the pulled docker.io/oven/bun:1.3-slim records — the tag was not moved to what was pushed",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: CANNOT ANSWER — pushed 10.20.0.15:3000/david/bun:1.3-slim, but the pulled docker.io/oven/bun:1.3-slim records no digest for 10.20.0.15:3000/david/bun (RepoDigests), so there is nothing to compare the registry with",
    ),
    (
        "mirror-base-images",
        "mirror-base-images: FAILED — pushed 10 image(s) to 10.20.0.15:3000/david, and 10 did not read back from the registry (each named above): 10.20.0.15:3000/david/bun:1.3-slim",
    ),
    // cordon-node / uncordon-node (backlog f0aaa72f): the line before the
    // write, each way the read-back refuses to vouch, and the OTHER verb's
    // effect — one script prints both, so neither may certify the other.
    (
        "cordon-node",
        "cordon-node: w-1 is a talos-worker at 10.20.0.14 (the estate registry and the cluster agree); spec.unschedulable=false before the cordon",
    ),
    (
        "cordon-node",
        "cordon-node: FAILED — kubectl cordon w-1 answered, and spec.unschedulable reads back 'unset' — the node is NOT cordoned",
    ),
    (
        "cordon-node",
        "cordon-node: FAILED — CANNOT ANSWER — kubectl cordon w-1 answered, but the node could not be read back: connection refused",
    ),
    (
        "cordon-node",
        "uncordon-node: w-1 is schedulable — read back spec.unschedulable=unset (was true)",
    ),
    (
        "uncordon-node",
        "uncordon-node: w-1 is a talos-worker at 10.20.0.14 (the estate registry and the cluster agree); spec.unschedulable=true before the uncordon",
    ),
    (
        "uncordon-node",
        "uncordon-node: FAILED — kubectl uncordon w-1 answered, and spec.unschedulable reads back 'true' — the node is NOT schedulable",
    ),
    (
        "uncordon-node",
        "cordon-node: w-1 is cordoned — read back spec.unschedulable=true (was false)",
    ),
    // shutdown-node (backlog f0aaa72f): the plan-still-holds line before
    // the shutdown, talosctl's acceptance (what a verb reading its exit
    // code alone would call success), and the read-back's refusals.
    (
        "shutdown-node",
        "shutdown-node: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — shutting down w-1 (10.20.0.14)",
    ),
    (
        "shutdown-node",
        "shutdown-node: talosctl accepted the shutdown of w-1 — reading it back (up to 720s: Kubernetes' Ready condition, and whether the Talos API still answers)",
    ),
    (
        "shutdown-node",
        "shutdown-node: FAILED — talosctl accepted the shutdown of w-1, and after 721s it is NOT proven down: Kubernetes Ready=True, Talos API answers. The drain may still be running, or the shutdown was never taken — read it again with talos-get w-1 machinestatus",
    ),
    (
        "shutdown-node",
        "shutdown-node: FAILED — talosctl accepted the shutdown of w-1, and after 721s it is NOT proven down: Kubernetes Ready=unread: connection refused, Talos API silent: error: rpc error: code = Unavailable",
    ),
    // node-converge (backlog 9d56c616): the plan-still-holds line before
    // the apply, Talos's acceptance (what a verb reading talosctl's exit
    // code alone would call success), and the read-back's refusal.
    (
        "node-converge",
        "node-converge: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — applying infra/cluster/talos/patches/w-1.yaml at 0123456789abcdef0123456789abcdef01234567 to w-1 (10.20.0.14)",
    ),
    (
        "node-converge",
        "node-converge: Talos accepted the patch on w-1 — reading it back (up to 120s: the live machine config through check-declared.sh, then the Node Ready with its kubelet lease renewed at least 20s after the apply)",
    ),
    // Review 9cb9af67 finding 3: the config reads MATCH and the kubelet
    // never came back — the line a config-only read-back would call done.
    (
        "node-converge",
        "node-converge: FAILED — Talos accepted the patch on w-1, and after 121s it is NOT proven to carry its declaration: the machine config reads every declared entry MATCH, and the Node is NOT proven back: Ready=False, kubelet lease renewTime 2026-09-30T15:00:00.000000Z (it must be at least 20s after the apply). The patch stands — read it again with plan-a-node-converge w-1 0123456789abcdef0123456789abcdef01234567",
    ),
    (
        "node-converge",
        "node-converge: FAILED — Talos accepted the patch on w-1, and after 61s it is NOT proven to carry its declaration: check-declared.sh exit 1: ABSENT     machine.kubelet.extraConfig.imageGCHighThresholdPercent  declared, not live: 50;. The patch stands — read it again with plan-a-node-converge w-1 0123456789abcdef0123456789abcdef01234567",
    ),
    // Review be5ba8f7 finding 5: the Node was back at the first fresh
    // lease and NotReady at the end of the bound.
    (
        "node-converge",
        "node-converge: FAILED — Talos accepted the patch on w-1, and after 121s it is NOT proven to carry its declaration: the machine config reads every declared entry MATCH, and the Node was back and is NOT any more: Ready=False, kubelet lease renewTime 2026-09-30T15:00:40.000000Z. The patch stands — read it again with plan-a-node-converge w-1 0123456789abcdef0123456789abcdef01234567",
    ),
    // move-volume-replica (backlog ab39a34e), above set-volume-replicas'
    // so neither shares the other's line: the plan-still-holds line, the
    // raise Longhorn accepted, the new replica healthy before the old one
    // goes, the delete, and the read-back's two refusals — the volume not
    // yet free to grow, and a move not proven.
    (
        "move-volume-replica",
        "move-volume-replica: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — moving replica pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab of pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56: numberOfReplicas 4 -> 5",
    ),
    (
        "move-volume-replica",
        "move-volume-replica: Longhorn accepted spec.numberOfReplicas=5 for pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 — waiting up to 1200s for a new replica healthy on a target disk (5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10)",
    ),
    (
        "move-volume-replica",
        "move-volume-replica: new replica pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-e0e0e0e1 is healthy on w-1 (disk 5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10) — 5 healthy on cp-1, cp-2, cp-3, w-1, w-2; lowering spec.numberOfReplicas to 4, then deleting pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab",
    ),
    (
        "move-volume-replica",
        "move-volume-replica: deleted pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab — reading it back (up to 180s: pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab gone, 4 healthy replicas on 4 distinct nodes, every replica's disk with room for growth to 64424509440 bytes)",
    ),
    (
        "move-volume-replica",
        "move-volume-replica: FAILED — pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab is gone and pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-e0e0e0e1 is healthy on w-1 (disk 5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10) — pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 has 4 healthy replicas on 4 distinct nodes (cp-1, cp-2, cp-3, w-1), but NOT every replica's disk has room for growth to 64424509440 bytes: disk 9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1 (default-disk-cp3 on cp-3), 1 replica(s) growing 32212254720 each: Scheduling space condition failed: ScheduledTotal = 386547056640 (Size 32212254720 + StorageScheduled 354334801920) is greater than ProvisionedLimit = 375809638400 (IsSchedulableToDisk). The move stands; that disk is the next to clear",
    ),
    // set-volume-replicas (backlog f5f182fc): the plan-still-holds line,
    // Longhorn's acceptance of the patch (what a verb reading kubectl's
    // exit code alone would call success), and the read-back's refusal.
    (
        "set-volume-replicas",
        "set-volume-replicas: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — setting spec.numberOfReplicas of pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091 from 1 to 2",
    ),
    (
        "set-volume-replicas",
        "set-volume-replicas: Longhorn accepted spec.numberOfReplicas=2 for pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091 — reading it back (up to 1500s: the spec, and 2 healthy replicas on 2 distinct nodes)",
    ),
    (
        "set-volume-replicas",
        "set-volume-replicas: FAILED — Longhorn accepted spec.numberOfReplicas=2 for pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091, and after 1501s it is NOT proven: spec.numberOfReplicas=2, robustness degraded, healthy on 1 distinct node(s) (w-1); not yet healthy: pvc-dd7b5ac3-884e-485f-8c73-92b87ce77091-r-5e6f on w-2; failed: none. The patch stands and Longhorn goes on rebuilding — read it again with plan-a-volume-replica-change, whose refusal names the replicas",
    ),
    // retire-volume-replica (backlog 5ef9d2d9), below set-volume-replicas'
    // so it shares no line with move-volume-replica's entries above them:
    // the plan-still-holds line, the lower Longhorn accepted, the named
    // replica removed by something else, the delete, the refusal when
    // another went, the read-back's room refusal, and one that did not
    // settle.
    (
        "retire-volume-replica",
        "retire-volume-replica: plan 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef still holds — retiring replica pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab of pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56: numberOfReplicas 4 -> 3",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: Longhorn accepted spec.numberOfReplicas=3 for pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 — watching 30s for a replica removed by anything but this run before deleting pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: something other than this run removed (or is removing) pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab on the lowered count — nothing to delete",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: FAILED — pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab is gone and pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 has 3 healthy replicas on 3 distinct nodes (cp-1, cp-2, cp-3), but NOT every remaining replica's disk has room for the next GiB of growth now: disk 9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1 (default-disk-cp3 on cp-3): ledger room 268435456000 bytes; live free space: storageAvailable 1073741824 is not above the floor 134217728000. The retirement stands; the growth it was for does not fit yet",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: deleted pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab — reading it back (up to 180s: pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab gone, 3 healthy replicas on 3 distinct nodes)",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: FAILED — after spec.numberOfReplicas was lowered to 3, something other than this run removed (or is removing) pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-03f4dac5 — not pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab — so pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab is NOT deleted, and nothing more is: a delete now would leave 2. pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56 holds 3 healthy replica(s) on cp-1, cp-2, w-2 (pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-1269eb6e, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-7c1934ba, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab). Read it again with plan-a-volume-replica-retirement",
    ),
    (
        "retire-volume-replica",
        "retire-volume-replica: FAILED — the retirement of pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab is NOT proven after 181s: pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-ae04deab gone, spec.numberOfReplicas 3, robustness degraded, healthy on cp-1, cp-2, cp-3, replicas pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-03f4dac5, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-1269eb6e, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-7c1934ba (planned to remain: pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-03f4dac5, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-1269eb6e, pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-7c1934ba); not healthy: none. Read the volume again with plan-a-volume-replica-retirement",
    ),
];

/// Does the pattern (after `^`) open with `<name>: `, a script's voice —
/// lowercase words joined by `-`, then a colon and a space?
fn opens_with_a_voice(rest: &str) -> bool {
    rest.split_once(": ").is_some_and(|(v, _)| {
        !v.is_empty() && v.chars().all(|c| c.is_ascii_lowercase() || c == '-')
    })
}

/// Does `line` match `re` in the runner's own engine (jq's `test`)?
/// `None` when jq is absent.
fn jq_matches(re: &str, line: &str) -> Option<bool> {
    let out = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "re",
            re,
            "--arg",
            "l",
            line,
            "$l | test($re)",
        ])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "jq could not judge {re:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

#[test]
fn a_declared_effect_matches_no_line_printed_before_the_change() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!(
            "a_mutating_verb_declares_its_effect: SKIPPED — no jq on this box; the gate image has it"
        );
        return;
    }
    let all = verbs();
    let effect_of = |verb: &str| {
        all.iter()
            .find(|(n, _)| n == verb)
            .and_then(|(_, s)| text_key(s, "effect"))
            .map(str::to_string)
    };
    let mut wrong = Vec::new();
    for (verb, line) in NOT_EFFECTS {
        let Some(re) = effect_of(verb) else {
            wrong.push(format!(
                "{verb}: named in NOT_EFFECTS but declares no `effect` — move the line with the verb or drop it"
            ));
            continue;
        };
        if jq_matches(&re, line) == Some(true) {
            wrong.push(format!(
                "{verb}: {re:?} matches a line printed before the change: {line:?}"
            ));
        }
    }
    // Every declared effect: not the bare voice, not the voice and any
    // word, and not the script's own FAILED / REFUSED lines.
    for (verb, spec) in &all {
        let Some(re) = text_key(spec, "effect") else {
            continue;
        };
        let rest = re.strip_prefix('^').unwrap_or(re);
        // The voice and its separator, or — for a bare verdict line —
        // its opening literal: the prefix a line must say MORE than.
        let lead = if opens_with_a_voice(rest) {
            rest.split_once(": ")
                .map(|(v, _)| format!("{v}: "))
                .unwrap_or_default()
        } else {
            literal_runs(rest).into_iter().next().unwrap_or_default()
        };
        for line in [
            lead.clone(),
            format!("{lead}x"),
            format!("{lead}FAILED — the read-back disagreed"),
            format!("{lead}REFUSED — a bound could not be evaluated"),
        ] {
            if jq_matches(re, &line) == Some(true) {
                wrong.push(format!(
                    "{verb}: {re:?} matches {line:?} — an effect must name what the run read back, after the voice"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a declared effect must match only the line printed after the read-back:\n  {}",
        wrong.join("\n  ")
    );
}

/// Every shipped verb, `(name, spec)`, sorted by name.
fn verbs() -> Vec<(String, serde_json::Value)> {
    let dir = repo_root().join("infra/ops/verbs");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("a verb file is named by its verb")
                .to_string();
            let text = std::fs::read_to_string(&p).expect("a verb file is readable");
            let spec = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not JSON: {e}", p.display()));
            (name, spec)
        })
        .collect()
}

fn is_mutating(spec: &serde_json::Value) -> bool {
    spec.get("about")
        .and_then(|a| a.as_str())
        .is_some_and(|a| a.contains("MUTATING"))
        || spec.get("requires_approval").and_then(|v| v.as_bool()) == Some(true)
}

fn mutating() -> Vec<(String, serde_json::Value)> {
    verbs()
        .into_iter()
        .filter(|(_, s)| is_mutating(s))
        .collect()
}

fn text_key<'a>(spec: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    spec.get(key).and_then(|v| v.as_str())
}

#[test]
fn every_mutating_verb_declares_its_effect_or_says_it_reads_none() {
    let verbs = mutating();
    assert!(
        verbs.len() >= 20,
        "the MUTATING roster is derived from infra/ops/verbs/ and read {} — the derivation broke",
        verbs.len()
    );
    let wrong: Vec<String> = verbs
        .iter()
        .filter_map(|(name, spec)| {
            let effect = text_key(spec, "effect").filter(|s| !s.trim().is_empty());
            let unread = text_key(spec, "effect_unread").filter(|s| !s.trim().is_empty());
            match (effect, unread) {
                (Some(_), None) | (None, Some(_)) => None,
                (Some(_), Some(_)) => Some(format!(
                    "{name}: declares BOTH `effect` and `effect_unread` — a run either shows its effect or says it cannot"
                )),
                (None, None) => Some(format!(
                    "{name}: declares neither `effect` (the regex of the line its script prints only after reading back what it changed) nor `effect_unread` (why it cannot yet)"
                )),
            }
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "a MUTATING verb must say how a run proves its effect (backlog fdbb447e):\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_verbs_admitted_without_a_read_back_only_shrink() {
    let roster: BTreeSet<&str> = EFFECT_UNREAD.iter().copied().collect();
    let all = verbs();
    let unread: BTreeSet<String> = all
        .iter()
        .filter(|(_, s)| s.get("effect_unread").is_some())
        .map(|(n, _)| n.clone())
        .collect();
    let joined: Vec<&String> = unread
        .iter()
        .filter(|n| !roster.contains(n.as_str()))
        .collect();
    assert!(
        joined.is_empty(),
        "{joined:?} declare `effect_unread` but were not admitted without a read-back: a new MUTATING verb prints a line that reads its effect back, and declares it as `effect`"
    );
    let stale: Vec<&&str> = roster.iter().filter(|n| !unread.contains(**n)).collect();
    assert!(
        stale.is_empty(),
        "{stale:?} no longer declare `effect_unread` (a read-back landed, or the verb is gone): take them out of EFFECT_UNREAD so the ratchet holds where it stands"
    );
}

#[test]
fn a_declared_effect_compiles_where_the_runner_judges_it() {
    // The runner judges with jq's `test` (Oniguruma), not Rust's regex:
    // a pattern is proven compilable by the engine that will read it.
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!(
            "a_mutating_verb_declares_its_effect: SKIPPED — no jq on this box; the gate image has it"
        );
        return;
    }
    let broken: Vec<String> = verbs()
        .iter()
        .filter_map(|(name, spec)| {
            let re = text_key(spec, "effect")?;
            let out = Command::new("jq")
                .args(["-n", "--arg", "re", re, r#""" | test($re)"#])
                .output()
                .expect("jq runs");
            (!out.status.success()).then(|| {
                format!(
                    "{name}: {re:?} — {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            })
        })
        .collect();
    assert!(
        broken.is_empty(),
        "an `effect` the runner cannot compile records every run unproven:\n  {}",
        broken.join("\n  ")
    );
}

/// The literal runs of a pattern: what is left between regex syntax once
/// bracket classes and `{m,n}` counts are taken out.
fn literal_runs(pattern: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut chars = pattern.chars().peekable();
    let flush = |cur: &mut String, runs: &mut Vec<String>| {
        if !cur.is_empty() {
            runs.push(std::mem::take(cur));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                flush(&mut cur, &mut runs);
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                }
            }
            '{' => {
                flush(&mut cur, &mut runs);
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                }
            }
            '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '|' | '\\' => {
                flush(&mut cur, &mut runs);
            }
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut runs);
    runs
}

#[test]
fn a_declared_effect_is_text_its_own_script_prints() {
    // A regex that names a line nobody prints proves nothing on any run,
    // and would read every clean run as unproven. So the pattern opens
    // with `^<name>: ` in the script's own voice (the `$ME` / `$NAME`
    // every script prefixes its lines with), and every stretch of
    // literal text in it of six characters or more is text the script
    // named by `argv` carries.
    let mut wrong = Vec::new();
    for (name, spec) in verbs() {
        let Some(re) = text_key(&spec, "effect") else {
            continue;
        };
        let script = spec
            .get("argv")
            .and_then(|a| a.get(0))
            .and_then(|a| a.as_str())
            .filter(|a| a.contains('/'))
            .unwrap_or_else(|| panic!("{name}: an effect-declaring verb runs a script"));
        let text = std::fs::read_to_string(repo_root().join(script))
            .unwrap_or_else(|e| panic!("{name}: read {script}: {e}"));
        let Some(rest) = re.strip_prefix('^') else {
            wrong.push(format!("{name}: {re:?} is not anchored with ^"));
            continue;
        };
        // A verdict line the script prints bare (recovery-kit's
        // `COMPLETE — kit … is cut`) has no voice; it must OPEN with a
        // literal stretch the script carries instead.
        if !opens_with_a_voice(rest) {
            let lead = literal_runs(rest).into_iter().next().unwrap_or_default();
            if lead.chars().count() < 6 || !rest.starts_with(&lead) || !text.contains(&lead) {
                wrong.push(format!(
                    "{name}: {re:?} opens with neither `<name>: ` nor six characters of literal text {script} carries"
                ));
            }
            for run in literal_runs(rest) {
                if run.chars().count() >= 6 && !text.contains(&run) {
                    wrong.push(format!(
                        "{name}: {re:?} expects {run:?}, which {script} does not carry"
                    ));
                }
            }
            continue;
        }
        let Some((voice, body)) = rest.split_once(": ") else {
            wrong.push(format!(
                "{name}: {re:?} does not open with `<name>: `, the voice its script prints in"
            ));
            continue;
        };
        if voice.is_empty() || !voice.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            wrong.push(format!(
                "{name}: {re:?} opens with {voice:?}, not a script's name"
            ));
            continue;
        }
        if !text.contains(voice) {
            wrong.push(format!(
                "{name}: {script} never names {voice:?}, the voice {re:?} expects"
            ));
        }
        for run in literal_runs(body) {
            if run.chars().count() >= 6 && !text.contains(&run) {
                wrong.push(format!(
                    "{name}: {re:?} expects {run:?}, which {script} does not carry"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a declared effect must be a line its own script prints:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn literal_runs_skip_regex_syntax() {
    assert_eq!(
        literal_runs("OK — deleted [0-9]+ of [0-9]{1,3} planned version[(]s[)]|DRY RUN"),
        vec![
            "OK — deleted ".to_string(),
            " of ".to_string(),
            " planned version".to_string(),
            "s".to_string(),
            "DRY RUN".to_string(),
        ]
    );
}
