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

## The migration is a copy of the log

David's directive (2026-08-08), superseding any heavier plan sketched
earlier: when the cluster is ready, migration = **clone main from
algedonic-dev, copy `audit_log`, rebuild** — everything else is
rebuildable. Verified against the actual state inventory, that holds,
with a short named remainder.

**The copy-set** (beyond `git clone` + `migrate.sh` from empty):

- **`audit_log`** — the system of record, and the copy is
  *self-verifying*: the hash chain travels with the rows, so
  `boss-audit-integrity-check` green on the destination proves the
  copy faithful end to end. The correctness protocol paying off at
  migration time.
- **The small non-derived registry tables**: `workflows`,
  `step_plugins`, `classes`, `policy_rules` (+ `policy_rule_audit`),
  `dispatcher_rules`. Workflow publishes do land in the log
  (`jobs.kind.published`) but no rebuilder consumes them; classes
  writes are eventless today — the same no-provenance class the
  design-docs finding exposed, and the same territory
  design-docs-as-data Q2 will settle. Until then: copy the tables.
- **`design_pending_decisions` / `design_flush_jobs`** if any are
  open — non-event-sourced by design (they survive epoch trims by
  living outside the log).
- **`sim_clock`** — the epoch baseline row; its
  `epoch_baseline_audit_id` references audit ids, which copy
  verbatim, so it stays coherent.
- **`/var/lib/boss/auth/credentials.toml`** — the one file outside
  both git and Postgres.

**Procedure**: quiesce writers and drain the outbox first
(`event_outbox` rows are pre-log; copying around a non-empty outbox
loses staged events — the epoch-trim quiescence machinery is the
model), copy, `boss-rebuild-all`, integrity check green, then
`deploy-services` + `deploy-web` (which regenerate the SPA and the
step-plugin bundles from the repo; `ensure_stream` recreates
JetStream and durable consumers re-anchor on an empty stream).

**Everything else regenerates.** Every projection rebuilds from the
copied log — the rebuilder's full domain list, messages included
(they rebuild from `audit_log`; the separate `messages_events`
retention log needs copying only if message history beyond the
projection matters). No snapshot, no export bundle, no
service-by-service migration: the company is its log plus its rules,
and moving the company is copying them.

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
