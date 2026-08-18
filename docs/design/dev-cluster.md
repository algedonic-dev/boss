# Design: the dev cluster — build and pipeline off the demo's machine

**Status**: approved — in-review — open questions tracked at `/system/design` (2026-08-18).
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
- **No k3s** — *superseded 2026-08-08: the cluster is Talos Linux*,
  which is Kubernetes beyond what this decision declined. What
  survives of the rationale is its real content: the **playground's**
  deploy model (systemd units, deploy-services.sh, the fingerprint
  pre-flight) is untouched until Q4 moves it; the cluster's own
  workloads are K8s-native from day one because Talos offers no other
  mode — no shell, no SSH, no host packages, machine config by API.
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

## Bring-up mechanics (Talos)

Talos supersedes the join-script model wholesale — there is no shell
for `join-build-node.sh` to run in (the script is deleted with this
revision; its checks-then-install, refuse-loudly spirit carries into
the machine-config flow). Bring-up becomes:

1. **Machine configs in the repo** (`infra/cluster/talos/`): a
   control-plane patch and a worker patch, applied with `talosctl` —
   the node inventory (Q1) fills in the addresses. Talos's KubeSpan
   gives the WireGuard mesh natively (see Q2).
2. **One builder image**, not per-node toolchains: Rust + sccache
   client — `infra/cluster/builder/Dockerfile`, sharing the
   OSS-quickstart image's digest-pinned toolchain base (one rustc
   truth; the two diverge deliberately past the base). The only
   image build-1 needs.
3. **Runners as workloads**: actions-runner-controller (repo-scoped,
   same trust question as before — now Q3 is pod-security-shaped
   rather than unix-user-shaped).
4. **sccache server as a Deployment** with a PVC for the cache.

**First-contact honesty transfers**: none of this has touched real
hardware; the OSS-quickstart VM validations each surfaced bugs only
contact finds, and Talos adds an image/PKI bootstrap with its own
first-contact class. Budget the fix pass.

What deliberately does NOT containerize now: the ~30 BOSS services.
Build-1's job is CI, and CI needs one image. Service images become
real work only if Q4 moves the playground onto the cluster — decide
there, not here.

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

All 2 open questions were resolved 2026-08-18 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---

## Decisions

### Q1: What are the five machines? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Per box: hostname, cores/RAM/disk, OS + version, and how they are
> reached today. This gates everything; the design assumes only "Linux,
> 4–8 cores, one LAN".

Answered by the build rather than by choosing. The cluster runs three Talos control-plane nodes (cp-1/2/3 at 10.20.0.11-13 behind API VIP 10.20.0.10), the mini PC `david-asus-minipc` at 10.20.0.15 hosting Forgejo, its OCI registry, the CI runner and cluster-deploy-runner, and boss-gcp (34.45.110.40) which still carries the train pipeline and the public demo. The question asked what to build; this is what runs.


### Q4: When does the playground move? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Staying on GCP costs the VM and keeps build/demo separation as a
> LAN→cloud deploy. Moving to the strongest LAN box removes the cloud
> dependency and puts the demo where the cores are, behind the same
> tunnel. Suggest deciding after build-1 has run for a week.

It moved. The cluster became the system of record on 2026-08-12 and the human door (playground.algedonic.dev) routes to the cluster gateway; the forge cutover followed. What stayed on GCP is deliberately narrower than 'the playground': the train pipeline and the public demo. The 'decide after build-1 has run for a week' condition was met and the decision was taken by doing it.

### Q2: Tailscale or bare WireGuard? (resolved)

Resolved 2026-08-18 — override.

**The question was:**

> Resolved 2026-08-10 — David: **bare WireGuard from the GCP box to the
> cluster** (node↔node inside the cluster stays KubeSpan). The GCP box
> is the hub — stable public IP, UDP 51820, overlay `10.99.0.0/24`,
> hub at `10.99.0.1`; cluster nodes are spokes that dial OUT (no
> inbound hole in the home router; `PersistentKeepalive` holds the NAT
> mapping). **The hub is already live**: `infra/cluster/wireguard/`
> holds `setup-hub.sh` (ran 2026-08-10; key generated, `wg-quick@wg0`
> enabled, GCP firewall rule `allow-wireguard` created),
> `peer-template.conf` (the spoke shape Talos machine config consumes),
> and `add-peer.sh` (append + hot-add; re-running setup cannot drop
> peers). Registering a node is: generate a spoke key on the node, fill
> the template with the hub pubkey + endpoint, `add-peer.sh <name>
> <pubkey> <10.99.0.N>` on the hub. Kanidm and the log-copy migration
> both ride this wire: the cluster reaches `id.algedonic.dev` and the
> export tarball over the overlay if the public path is ever down.

Wireguard


### Q3: Runner scope and trust (resolved)

Resolved 2026-08-18 — accept.

**The question was:**

> A self-hosted runner executes workflow code from PRs. Repo-scoped
> runner + no fork PRs on it (the fork model here means train branches
> come from `dauld:boss-fork`) needs an explicit decision: which events
> may run on cluster runners — and under Talos the containment question
> becomes pod-shaped: what securityContext/namespace isolation the
> runner pods get, rather than which unix user the runner daemon runs
> as.
>
> Proposed: **the question has two premises and the tree contradicts
> both, so answer the one that is left.** Measured 2026-08-16 rather
> than reasoned about.
>
> *"No fork PRs on it"* is already true, and not by policy. Every pull
> request the forge has ever seen — 38 through 47, the full history —
> is `david/boss -> david/boss`. The fork model the question assumes
> is not how the conductor works: it pushes each car branch to the
> forge repo itself and opens a same-repo PR. `.forgejo/workflows/ci.yml`
> triggers on `push: [main]` and `pull_request: [main]`, with no
> `pull_request_target`, so there is no path today by which a fork's
> code reaches the runner. The decision to make is not "should we
> allow fork PRs to run" but "do we ever intend to accept an outside
> contribution on the forge" — and the answer is no, because that is
> what the GitHub mirror is for. Write that down and the trust
> question closes.
>
> *"Under Talos the containment question becomes pod-shaped"* is not
> true either. The runner is not on Talos. It is a Docker runner on
> the forge host (10.20.0.15), `runs-on: docker`, pulling
> `10.20.0.15:3000/david/boss-ci:rust1.96` from the registry beside
> it. The only cluster-touching automation is
> `infra/forge/cluster-deploy-runner.sh`, which is a systemd unit on
> that same host, not a workflow job — and it is deliberately
> one-directional: "no ssh from the conductor, no shared credentials;
> each side touches only what it owns." So containment is
> Docker-shaped, and the kubeconfig that could reach the cluster is
> held by a unit CI jobs cannot invoke.
>
> What is left, and is a real decision: the runner is **not registered
> at repo scope**. `GET /repos/david/boss/actions/runners` returns
> `[]` — 200, empty, with a repo-scoped token that reads everything
> else fine — so it is registered at user or instance level. That is
> the actual trust surface: a runner shared across every repo the
> instance will ever hold, on the host that also serves the forge, the
> registry, and the deploy unit's kubeconfig. Today there is one repo,
> so nothing is exposed. The recommendation is to re-register it
> repo-scoped now, while that is a one-line operation, rather than at
> the moment a second repo makes it urgent.
>
> The Talos framing should be struck from this question rather than
> answered. If runner pods ever move onto the cluster that is a new
> decision with a new threat model, and leaving the sentence here
> implies a containment story that does not exist.

**the question has two premises and the tree contradicts both, so answer the one that is left.** Measured 2026-08-16 rather than reasoned about.


## Decision history

_None yet._
