# BOSS launched agent controller

This is the source implementation for design21062f95 and partial3869107c.
The shipped harness still selects `process`. Selecting `cluster` resolves
the model through the agents registry and its reviewed harness, then reads
the corresponding versioned row under `infra/platform/agent-controllers`.
That row is disabled. No builder sandbox row is added.

David's recorded decision
[`6f3ef63c`](https://boss.algedonic.dev/ux/jobs/6f3ef63c-469e-47e9-a48c-0eb7cbcb3214)
amends only design21062 Q2: the issuer mount remains read-only, while
one serialized run may retain renewed managed login in a private writable
cache. `launch_controller::managed_cache` prepares the trusted lease,
native incarnation binding, terminal collection, principal validation,
generation CAS and quarantine contract behind one private broker port.
Its state carries metadata only; cache bytes have no journal/serialization
path in that component. A successful `finish` stages a validated collection
in `Collected`, which remains unavailable to new runs. A save that commits
before its acknowledgement fails stays unavailable too; no rollback is
inferred. The kernel has no `Collected` to `Ready` release. Exact durable
private publication/recovery remains source work. Descriptor-confined private
filesystem persistence and authenticated metadata IPC now implement the kernel's
store boundary; neither is installed or configured natively.

The native managed path refuses before compute while that adapter and
its independently observed scope and private collector placement are absent.
The image helper can prepare a new private writable cache from the pinned
read-only issuer projection only with an exact run/incarnation lease plan.
Preparing that helper does not enable the native managed path. This checkpoint installs no enrollment surface,
credential, cache or activation selector. Dedicated private human
enrollment UX and genuine issuer/consumer installation, expiry/revocation,
collection/recovery and runtime acceptance remain parent386 obligations.

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

The forge converge now runs `publish-image.sh publish <full source sha>` as a
separate stage after core convergence. It uses the broker's existing registry
push login, verifies the retained platform manifest and every configuration and
layer blob through the shared registry transport, and probes the digest-pinned
image without network access. The required CLI version comes from the controller
row. A failed optional publication leaves core BOSS delivery and previous image
witnesses intact; its complete streams and typed receipt remain under the
root-owned converge state, with `agent_image_publication` on the maintenance
packet. This finding is separate from core convergence success.

Successful records live at
`/var/lib/boss/converge/agent-images/<full sha>/agent/image-witness.json`.
Schema 1, kind `agent-image`, binds the source generation, platform digest,
verified blob descriptors, full original hashes, and actual native command
exits to the CLI version, help, package closure and helper witnesses.
The read-only `agent-image-status <full sha>` ops verb rechecks those originals.
`launch_ready` remains false: this artifact cannot substitute for the genuine
issuer-installed login generation, controller/kubectl witness or authorized
native runtime acceptance. Parent3869107c remains open.

Verified layer staging is removed only after the digest-pinned daemon pull,
inspect and executable probes succeed. Exact expected/observed byte counts and
SHA256 values remain in `blob-verification.json`; raw manifests, configuration
and all native streams remain. Failed transfers keep their complete bodies.
A matching recipe/helper/controller/harness and resolved toolchain fingerprint
can reuse an existing immutable image. Each new source generation still fetches,
verifies, pulls and probes that digest; `image_generation` preserves the original
build generation and `reused_publication` copies its full witness. No build or
push exit is invented for a reused artifact. Missing registry content refuses
publication. The optional stage reserves 20 GiB before admission and bounds its
retained store to 64 GiB; exhaustion refuses further publication without deleting
a previous original or blocking core convergence.

Npm downloads ignore lifecycle scripts. Downloaded Codex version/help commands
run only in BuildKit `RUN --network=none` stages and the subsequent native
network-none probes. The publisher requires that build mode and never falls
back to networked vendor execution. Help flags come from the harness registry,
and the native pre-push image ID must equal the verified registry configuration
digest and the post-pull image ID. The [OCI image configuration contract](https://github.com/opencontainers/image-spec/blob/main/config.md#imageid)
defines that content identity.

The optional stage records an unconfirmed receipt before invoking the publisher;
a kill before terminal acknowledgement cannot look like publication success.
When no immutable witness exists, `agent-image-status` returns the latest
attempt receipt with its complete stdout/stderr as an
`agent-image-publication-attempt`, which cannot serve as an image binding.
