# BOSS launched agent controller

This is the source implementation for design21062f95 and partial3869107c.
The shipped harness still selects `process`. Selecting `cluster` resolves
the model through the agents registry and its reviewed harness, then reads
the corresponding versioned row under `infra/platform/agent-controllers`.
That row is disabled. No builder sandbox row is added.

A launch needs a protected, publisher-produced
`agent-controller-<harness>.json` beside the verified installed CLI generation.
Its typed `Bindings` contract names the OCI platform digest, exact binary
version and help/helper witnesses, existing published kubectl witness,
issuer/principal/scopes, declared consumer, read-only login item and native
storage generation. This is an input contract, not a hand-placement door.
The image publisher must conserve the complete OCI manifest and platform
selection, verify every blob before extraction, and produce those witnesses
from the resulting image. This source car does not build or publish it.
The recipe requires the publisher's verified forge-mirrored toolchain image
and records the complete package closure and exact help/version output.
An absent or stale witness refuses before the source claim. The controller
reads credential registry metadata, never Secret data or a dev home login.

The existing activation selector must actually name that same generation,
with its successful machine-run contract record; David's retained activation
hold must be absent. This reader neither activates nor lifts the hold.
Issuer-owned login decision85c991d1, publication and authorized runtime
rollout remain separate obligations. A source gate or fixture supplies none
of them.

The trusted launcher retains original request/response/stdout/stderr bytes,
the exact source bundle and member manifest, and every native terminal.
Normalization v1 removes only named fields with pinned default values;
unknown admission additions survive and refuse complete intent equality.
JobUID, PodUID and container incarnation come from native observations.
Each exec reobserves those bindings. Before opening input, the image helper
also compares the API-owned DownwardAPI PodUID and kernel PID1 start ticks,
PID namespace inode and boot ID with the recorded supervisor handshake.
Its isolated Python interpreter ignores candidate startup code. This is a
supervisor incarnation fence; Kubernetes exec has no atomic UID precondition.

The first native assistant acknowledgment binds START, run, prompt and
source digests. Schema2 is a launcher observation under its own signature,
distinct from unaffected schema1 worker self-claims. Original streams are
conserved while execution continues. A native exit, EOF, completed-turn
event, transcript and artifact status are judged separately. A partial pipe
tail, missing file or unknown transport is not successful execution. The
original last message and uniquely matched transcript remain data; they
authorize no policy, source-step completion, approval or release.

One private retained PVC belongs to the run. Compute keeps its independent
deadline, restart prohibition and short TTL. No owner reference lets that
TTL erase unarchived evidence. Normal exit, interruption and failure request
foreground deletion with the original JobUID, then independently observe
original Job/Pod absence. Accepted deletion or a dark read is not stop proof.
Available artifacts survive launcher loss. `boss dispatch <original-run>
--recover-launch` requires a fresh own active registered caller. Its bounded
collector has no login and mounts the original UID-checked workspace read-only;
it preserves available and missing/partial statuses without rewriting the old
terminal. Unwitnessed tails are identified as retained worker data.

`boss dispatch <original-run> --replay-launch` is the bounded receipt-only
door for a dark system of record. A fresh active caller must be the original
registered observer. It validates the retained START expectation, refuses
different existing receipts or terminals, copies original values and checks
the native readback. A retained death can finish only the matching building
step as died; it never declares source completion or successful execution.
Its attempt is durable and performs no Kubernetes effect.

The workspace remains retained with a loud receipt. This implementation
grants no direct PVC deletion and does not infer garbage collection from
copying only message/transcript files: unarchived worktree products remain.
Cleanup needs an independently bound complete archive acknowledgment and an
authorized collection path. Source fixtures establish controller behavior;
real network/kernel READ/USE, cancellation and escaped-descendant observations
remain owed after genuine activation. Parent3869107c stays open.
