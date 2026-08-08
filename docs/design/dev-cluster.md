# Design: the dev cluster — build and pipeline off the demo's machine

**Status**: in-review — open questions tracked at `/system/design`.
**Source**: backlog `ad2e28ab` (decisions recorded there 2026-08-07) and
the operator's 2026-08-08 direction: the twice-daily train and its
deployments should run against infrastructure BOSS models, on the five
local Linux machines.
**Related**: [schema-migrations.md](./schema-migrations.md) (the
expand/contract convention that unblocked rolling updates) ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

---

## What is already decided (recorded on `ad2e28ab`)

- **Option (a) first**: self-hosted runners on the cluster, GitHub
  stays the git host + PR surface for now. Cheap, reversible, and
  separable from the modelling question.
- **Direction after that**: review moves off GitHub; GitHub becomes a
  daily mirror. Five machines, one LAN, 4–8 cores each.
- **No k3s.** It would rewrite the deploy model the fingerprint/
  pre-flight work just made honest. If orchestration ever lands, it is
  a later decision made against measured pain.
- **Don't distribute the build — share its cache.** sccache, not a
  build farm: the ~50-minute release build is embarrassingly parallel
  per crate, and a warm shared cache captures most of the win without
  new failure modes.
- **Runner → jobs-API early**: measuring the dev pipeline in BOSS's
  own event log is the point, not an afterthought. The pr-train
  Workflow is that surface; the cluster gives it hardware.

## Why now

The named blocker is gone: rolling updates needed N-1 compatibility,
which the old drop-and-regen schema policy forbade — expand/contract
([schema-migrations.md](./schema-migrations.md)) replaced it, and the
playground is baselined. And the standing operational hazard remains
measured and real: the release build shares a disk with the running
demo (backlog `884488c4`), and the day the build volume filled was the
day the demo's disk did.

## Topology

The playground stays where it is for now — a GCP VM behind the
Cloudflare tunnel. The cluster machines join a WireGuard-family mesh
(Tailscale unless Q2 says otherwise) so the GCP box and the LAN see
each other privately; nothing on the cluster is exposed publicly.
Because the public demo is served through the tunnel — and `cloudflared`
runs anywhere — moving the playground onto the strongest LAN machine
later is a migration, not a redesign (Q4).

Roles, smallest-first:

1. **build-1** — the strongest box: GitHub Actions self-hosted runner
   (repo-scoped), sccache server, Rust toolchain. CI and release
   builds move here; the demo's disk stops paying for them.
2. **build-2..n** — additional runners pointed at the same sccache
   cache, added only when queue depth says so.
3. **Later, per the recorded direction**: the forge/mirror machine
   (Forgejo + the daily GitHub mirror) and the train conductor's home,
   once review moves in-house.

## Bring-up mechanics

`infra/cluster/join-build-node.sh` is the one-command join for a
build node: idempotent, checks-then-installs (toolchain, sccache,
runner), and refuses loudly where a human credential is needed (the
Tailscale login, the runner registration token) rather than
half-completing. **First-contact honesty: the script is untested on
real hardware until machine #1 joins** — the OSS-quickstart VM
validations each surfaced install bugs only contact finds, and this
will too. Budget for a first-contact fix pass.

The train composes with this in two steps: first the conductor's
`ci` step starts reading checks that ran on cluster runners (no
conductor change — gh reports them the same way); later the deploy
phase consumes artifacts built on the cluster instead of building on
the playground, which is the moment `884488c4` closes for good.

## Open questions

### Q1: What are the five machines?

Per box: hostname, cores/RAM/disk, OS + version, and how they are
reached today. This gates everything; the design assumes only "Linux,
4–8 cores, one LAN".

### Q2: Tailscale or bare WireGuard?

Tailscale: zero-config mesh, MagicDNS, ACLs, a third-party control
plane. Bare WireGuard: fully self-hosted, more per-node bookkeeping.
The recorded instinct (self-host the pipeline) pulls toward WireGuard;
the bring-up-cost instinct pulls toward Tailscale. Either satisfies
the topology; picking is a values call.

### Q3: Runner scope and trust

A self-hosted runner executes workflow code from PRs. Repo-scoped
runner + no fork PRs on it (the fork model here means train branches
come from `dauld/boss-fork`) needs an explicit decision: which events
may run on cluster runners, and does the runner user get the same
sudo-less containment the deploy scripts assume?

### Q4: When does the playground move?

Staying on GCP costs the VM and keeps build/demo separation as a
LAN→cloud deploy. Moving to the strongest LAN box removes the cloud
dependency and puts the demo where the cores are, behind the same
tunnel. Suggest deciding after build-1 has run for a week.

## Decision history

_None yet._
