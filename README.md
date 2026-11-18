# BOSS

[![CI](https://github.com/algedonic-dev/boss/actions/workflows/ci.yml/badge.svg)](https://github.com/algedonic-dev/boss/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/algedonic-dev/boss/badge)](https://scorecard.dev/viewer/?uri=github.com/algedonic-dev/boss)

**BOSS is open-source software for one coordination problem: how
to use a limited set of assets, plus a work force of people,
robots and automatons, to perform complex jobs.** Its answer is to
break every complex job into simple steps, write down exactly what
each step needs and how to tell it is done, and then, as each step
comes up, reserve the right resources and the right executor for
it — and keep a record that proves what happened.

> ⚠ **Preliminary release.** An early, opinionated cut shared to
> get reactions to the idea — the coordination model below (resource
> pools, executors, jobs of simple steps, stations, protocols, an
> audit record), run by people and AI agents together. A prototype
> that proves the shape, not a stable product:
> APIs, schemas, and registry contracts will move before the first
> non-preliminary tag. Expect rough edges — half-built surfaces,
> wired-but-unsurfaced code, intentional `TODO`s
> ([`TODO.md`](TODO.md) is the roadmap). Resonates or doesn't?
> Open an issue.
>
> **Agent-authored.** Every line of syntax — Rust, TypeScript,
> Svelte, SQL, prose — is written by coding agents under a human
> designer's direction (who has read the whole tree); expect the
> odd quirk.
>
> **The audit record is load-bearing, not the code.** It's the
> event-sourced system of record, third-party-verifiable without
> running any BOSS software (the [five-property correctness
> protocol](docs/design/correctness-protocol.md)).

## The coordination problem

Every organization that does real work has the same problem. It has
**constrained assets** — vessels, machines, floor space, servers,
stock on the shelf, money — and a **work force** to use them with:
people, and more and more often software that can do part of the
work itself. It has to put the two together to get **complex jobs**
done: brew today's batch, fill an order, hire a brewer, ship a
change to its software. No single person can hold all of that in
their head, so the real plan usually lives in spreadsheets, chat
threads and whoever happens to remember.

BOSS models the problem with eight things:

- **Resource pools** — the constrained assets. A pool is something
  work draws on: hours on the brewhouse, a fermenter, CPU and memory
  on a server, kegs in the warehouse, a daily budget. Some of it
  comes back when the work ends (a vessel, a CPU), some is used up
  (malt, disk space), and some is spent (dollars).
- **Executors** — whoever or whatever does the work, and there are
  exactly three kinds: a **human**; a **robot**, meaning an AI
  agent; and an **automaton**, meaning ordinary deterministic
  software. People are pool capacity too: a role has as many people
  as the roster says, and every human step waits on one of them.
- **Jobs**, made of **steps**. A job is one bounded piece of work.
  It is broken into simple steps, and each step names what it
  needs — which kind of executor, which resources — and ends with an
  objective test that says it is done.
- **Protocols** — the exact instructions a job follows: which steps,
  in what order, under what conditions, with each step's completion
  test. A protocol is versioned data, not code, and a job stays on
  the version it started under, so changing how the company works
  is publishing a new version.
- **Stations** — just a place to temporarily reserve the right
  resources and executors to do a step according to its protocol.
  A station holds a share of the pools, given to it by the station
  above it, and seats the executors who work there.
- **Machines** — software set up at a station to support the work
  there: a controller, a test runner, a dispatcher that routes
  waiting steps. A machine serves its station; it never takes a step
  itself.
- **The audit record** — every change, written down once and never
  edited: each job opened, each step claimed and finished, each
  reservation, release and wait. Every screen, report and ledger in
  BOSS is computed from it, which is how BOSS proves what happened
  rather than asserting it.

### A worked example

The demo tenant is a brewery, Algedonic Ales. Its daily brewing job,
shortened and written in those terms:

| Step | Executor | Needs | Done when |
|---|---|---|---|
| 1. Check demand | automaton | — | the brew-or-skip decision is recorded with the stock figures behind it |
| 2. Plan the brew | human: senior brewer | — | the recipe and batch size are set |
| 3. Mash in | human: brewer | the brewhouse; 196 bags of pale malt and the rest of the grain bill | the ingredients have left inventory |
| 4. Boil | human: brewer | the brewhouse | the boil is logged and the gravity measured |
| 5. Ferment | human: brewer | one fermenter, for seven days | the yeast is pitched and the vessel named |
| 6. Check quality | human: QA technician | — | pH, final gravity and the microbial test are inside tolerance |
| 7. Package | human: packaging crew | the packaging line; empty kegs | the kegs are counted into finished stock |

When step 3 comes up, the brewhouse station — which holds the
brewhouse and seats the brewing crew — reserves the brewhouse for
it. A brewer claims the step, does the work and records the result.
When the completion test passes, the station releases the brewhouse
and the protocol makes step 4 ready. If the brewhouse is still busy
with yesterday's batch, step 3 waits, and the record says what it is
waiting on. If the quality check fails, the protocol opens a hold job
for the batch instead of sending it to packaging.

Nobody had to decide who does what next: the protocol and the pools
did. And the record now holds the whole batch — who did each step,
on which vessel, how much malt left inventory, what the gravity read
— written as it happened, not reconstructed afterwards. Inventory,
cost of goods and the dashboards are all computed from that same
record. (The full job is the demo's `morning-brew` protocol, in
[`examples/brewery/seeds/workflows.toml`](examples/brewery/seeds/workflows.toml).)

### Stations form a tree

Stations nest. The company is the root station and holds every
constraint: all the hardware, the whole roster, the budget. Each
station below it is defined by what its parent gives it — a
department gets a share, a standing station inside the department
(the brewhouse, the packaging line) gets a share of that, and a
station can be set up for a single step and released afterwards. No
station can be given more than its parent holds, so nothing is
double-booked. When nothing is free, the step waits, and the record
names the constraint it is waiting on — so the constraint that work
waits on most is a number you can read, and the one worth buying
more of. Reorganizing — a new department, moving capacity between
two — is editing station rows, not code.

BOSS's own development runs the same way: coding agents are robot
executors, the build servers are pools, and every change is tested
at a standing station with a fixed number of seats, so a change
that finds them full waits its turn. Read further:

- [docs/design/factory-taxonomy-v2.md](docs/design/factory-taxonomy-v2.md)
  — the eight primitives and the rules that connect them.
- [docs/design/station-tree.md](docs/design/station-tree.md) — the
  tree applied to the company that builds BOSS: which stations
  exist, what each holds, who owns which protocol.
- [docs/design/the-three-layers.md](docs/design/the-three-layers.md)
  — the reading frame (below).

**Where the code stands.** This model is the decided design, and
the code is moving onto it one file at a time. BOSS's own
development is where the station tree is first being put to work;
the brewery demo still runs its jobs without stations of its own,
so the example above is the model's reading of it, not yet its
configuration. BOSS's first model named four primitives —
Subjects, Jobs, Steps and Events — and the source still uses those
names in many places: a *Workflow* is a protocol, the *event log*
(`audit_log`) is the audit record, and a *Subject* is the thing a
job is about (an account, an asset, an employee).

## Why

**BOSS exists to help the people running a business operate it
better — and to make the basic software that running a business
needs reachable for organizations that can't afford a bespoke
enterprise stack.** Beer Open Source Software for System
Modeling, named in tribute to **Stafford Beer**, the British
cybernetician whose work on the Viable System Model and Project
Cybersyn shaped how I thought about modeling organizations while
designing BOSS.

**The thesis: a reasonably-sized business — a brewery, a
device-refurb shop, a dental practice, a 200-tech field-service
operation — can be modeled directly in software, in its own
terms, with a handful of primitives.**

Most businesses don't run on a model of themselves. They run on
a patchwork — a CRM that knows accounts, a ticketing tool that
knows work, an accounting package that knows money, none of which
knows the others. The real shape of the operation — who owns
what, what must happen before what, what state each thing is in —
lives in the seams: in spreadsheets, in people's heads, in
reconciliation nobody has time for. Each tool holds a partial,
approximate shadow of the business, and keeping the shadows in
sync *is* the overhead.

BOSS collapses the patchwork into one model. The pools, the
executors, the jobs, the protocols they follow and the stations
where they are worked all write to one audit record — so an
account, the job running against it, and the ledger entry that job
posted cross-reference in a single record. Modeling directly,
instead of bending the business into generic tools, buys three
things the patchwork can't:

- **Fidelity — the model is the business, not an approximation
  of it.** A step is a real person signing off; a job is a real
  unit of work. The audit record holds what the company did in the
  company's own vocabulary, not an analytics derivative
  reconstructed afterward. Operators reason about the software
  the way they reason about the operation, because they're the
  same shape.
- **Adaptability — the model bends as the business does, in
  data, not code.** New work types are protocol rows; new
  categories — roles, account tiers, asset models — are registry
  entries; a reorganization is station rows; new step UX is a
  plugin. Reshaping the model is editing data, not forking a
  codebase — which is what keeps a small team able to follow the
  operation as it changes.
- **Coherence — one model where a generic stack keeps three.**
  Sales, work, assets, people, and the ledger share one audit
  record, so the cross-system reconciliation that eats
  operational time mostly stops existing.

Underneath, the model is event-sourced and third-party-verifiable
(above) — replay rebuilds every projection from t=0 — so
durability, auditability, and a one-policy-gate security posture
come from the foundation, not from operator discipline. It reads
in three layers, each replaceable without disturbing the others —
**the network is the substrate, the fat protocols dictate the
current operating model, the actors run it**
([the-three-layers.md](docs/design/the-three-layers.md)). The
**network** is jobs, the stations that hold them until there is
capacity, the routes between stations, and the record, and it has
no opinion about what the work means; the **protocols** are fat and
carry the meaning, which is why changing how the company works is
publishing a versioned registry row rather than shipping code; and
the **actors** — the human, robot and automaton executors — run it,
working steps inside the same schema, policy gate, and sign-off
rules, never in the request path.

The codebase is structured to be conducive to rapid customization
with AI coding agents — with a bit of care to preserve the audit-
log + correctness-protocol contracts at the bottom, the rest is
malleable.

## See it locally

Install it locally and open it in a browser: the demo tenant,
Algedonic Ales, **builds itself live**. A fresh install starts
with an empty audit log (schema + reference data only); the
brewery simulator then ticks sim-days forward from the demo
epoch and generates the operation as it
goes — jobs, orders, invoices, ledger entries, projections. So
the SPA is **sparse on first load and fills in as the sim runs**,
and the audit log grows while you click around. The install (see
[Quick start](#quick-start) below) runs the one BOSS container
image and seeds the tenant through the public API; the
bootstrap-admin email you set in `.env` becomes your login.

A few specific places that land the design quickly (all served
by the local install on `:4443`):

| If you want to see … | Open |
|---|---|
| The home dashboard with live system activity | <http://localhost:4443/> |
| A Job from the brewery flow with its step graph | <http://localhost:4443/ux/jobs/> (click any open Job) |
| The full event log streaming as the sim ticks | <http://localhost:4443/system/monitoring/events> |
| The System Atlas — every service + its event topics | <http://localhost:4443/system/monitoring/atlas> |
| The brewery's people, with role-based scoped views | <http://localhost:4443/ux/people> |
| A workflow's anatomy (Workflow authoring surface) | <http://localhost:4443/system/workflows> |

The install lands on `:4443` behind local-auth: log in as the
bootstrap-admin for write access, or use the **Browse as a
guest** button for read-only. What you see is the head of `main`
plus the data the simulator has generated since you started the
stack.

## Where to start

| If you want to … | Read |
|---|---|
| See it in a browser | [Quick start](#quick-start) below — `docker compose up` |
| Understand the coordination model | [docs/design/factory-taxonomy-v2.md](docs/design/factory-taxonomy-v2.md) — the eight primitives and their rules; [docs/design/station-tree.md](docs/design/station-tree.md) — a worked station tree |
| Understand the architecture | [docs/architecture-diagram.md](docs/architecture-diagram.md) — four diagrams, conceptual to concrete |
| Read the BOSS core domain | [CLAUDE.md §Primitives + §Supporting concepts](CLAUDE.md#primitives) |
| See the public demo tenant | [examples/brewery/DOMAIN.md](examples/brewery/DOMAIN.md) — Algedonic Ales |
| Read the design pattern docs | [docs/design/](docs/design/) — load-bearing patterns referenced from CLAUDE.md and the decision record |
| Run BOSS locally | [Quick start](#quick-start) below |
| Set up a toolchain to work on BOSS itself | [docs/runbooks/dev-environment-bootstrap.md](docs/runbooks/dev-environment-bootstrap.md) |
| Operate a deployed BOSS | [docs/runbooks/operator.md](docs/runbooks/operator.md), [`boss` CLI](crates/orchestrators/boss-cli/README.md) |
| Track open work | [TODO.md](TODO.md) |
| Read the decision record | [docs/architecture-decisions.md](docs/architecture-decisions.md) |

## What's in the box

The workspace splits into four tiers (see CLAUDE.md):

- **Core state-machine OS** — `crates/core/`. The generic primitives every deployment uses:
  jobs and their steps, the audit log + projection rebuilders,
  the protocol (Workflow) / StepType / StepPlugin registries, policy,
  gateway/auth/NATS, calendar, and the two taxonomy registries.
  Tenant-neutral.
- **Company-modeling layer** — `crates/modules/`. Useful for modeling a company on top of the core:
  people, accounts, commerce, inventory, ledger, products,
  shipping, messages, catalog, assets, plus their `*-client`
  HTTP-contract crates and the `boss-ml-plugins` extension
  surface. A non-company tenant (a research lab, a robot fleet)
  can deploy without these.
- **Orchestrators** — `crates/orchestrators/`. Binaries that fan out across both tiers:
  `boss-rebuild`, `boss-cli`, `boss-sim`, `boss-ml-api`, `boss-simulator`.
- **Tenants** — `crates/tenants/`.
  - **Algedonic Ales** (`boss-brewery-engine`) — the public OSS
    demo tenant. Data-first seeds at `examples/brewery/` (TOML +
    JSON) plus the brewery-specific Workflows. Industrial-scale
    brewer modeled across 5 beer styles.

A tenant is seed data — Workflows, policy grants, Class rows, a
roster — published through shared bootstrap doors
(`boss_jobs::bootstrap`, `boss_policy::bootstrap`, the classes batch
API). Algedonic Ales is the one shipped example. A company's own model
belongs in its tenant repository; [the tenant contract](docs/tenant-contract.md)
describes the scaffold, validation and publication doors. The company
modules remain available to other organizations, with tenant-specific
meaning supplied by their registry data.

The full crate-by-crate map and service topology live in
[docs/architecture-diagram.md](docs/architecture-diagram.md).
Port assignments are the canonical
[`crates/core/boss-ports/src/lib.rs`](crates/core/boss-ports/src/lib.rs).

## Quick start

One way to run BOSS — the container image, under Docker compose
(the same image the cluster runs; there is no host-native install):

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss/infra/oss-quickstart
cp .env.example .env
# edit .env — set BOSS_BOOTSTRAP_ADMIN_EMAIL=you@example.com
docker compose up
```

Open `http://localhost:4443` and log in with the bootstrap-admin
email you set. The install seeds the brewery tenant through the
public API and starts the live sim, which builds the demo from an
empty log. Expected timings, the init-chain log checkpoints, the
guest button, troubleshooting, and how to run the image you built
from your own tree all live in the one install runbook:
[`infra/oss-quickstart/README.md`](infra/oss-quickstart/README.md).

> **The demo builds itself live.** The install starts the brewery sim
> against an empty `audit_log`, so the SPA is sparse on first load and
> fills in as the sim runs — which keeps the clone small and the install
> fast. Maintainers validate a full sim-year with
> `sudo ./infra/postgres/validate-brewery-sim.sh`: it runs the sim and
> asserts a clean rebuild + conservation/integrity.

> ⚠  **Not production-ready.** The quickstart runs the whole
> platform on one machine behind file-backed local-auth — no
> SSO, no MFA, no account lockout, no rate limiting, no
> edge-tier hardening (see the quickstart README's
> Authentication section). An integrated IAM (Authelia or any
> OIDC IDP via forward-auth) and a hardened
> production-infrastructure template are queued under
> "Post-release" in [`TODO.md`](TODO.md).

## Production posture

For deployments past the eval stage, the recipe is **the same
container image on Kubernetes, with a backup CronJob**. One pod
runs the full stack — every `boss-*-api`, the gateway, the static
SPA, the dispatcher — beside a Postgres and a NATS StatefulSet;
[`infra/cluster/manifests/`](infra/cluster/manifests/) is the
declared shape, and the nightly `boss-pg-backup` CronJob uploads
pg_dump snapshots to an offsite GCS bucket. The `audit_log` is the
disaster-recovery primitive (any snapshot replays cleanly via
`boss-rebuild-all`).

The operator runbook is at
[`docs/runbooks/operator.md`](docs/runbooks/operator.md); the
cluster's bring-up and topology at
[`docs/design/dev-cluster.md`](docs/design/dev-cluster.md).
Cloud-provider provisioning recipes (Azure, GCP, AWS,
Cloudflare, Hetzner) are queued post-release under
`infra/blueprints/<provider>/` per the TODO entry.

Multi-VM topologies (warm-standby Postgres replication,
active/active data planes, edge load balancers) are deliberately
out of scope for v1 — they get reconsidered when a real tenant's
SLAs force the conversation. The post-release **Production
Infrastructure Template** TODO entry is where that work lands.

## Building & quality

Test discipline matters because the platform claims correctness as
a first-class invariant (see *Founding ideas* above). The full
strategy — what each layer catches, where it runs, what fails the
build vs what triggers an incident — lives at
[docs/design/testing-strategy.md](docs/design/testing-strategy.md).
The summary:

| Layer | Mechanism | Where it runs |
|---|---|---|
| Static checks | `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo fmt --check`, `bun run typecheck` (svelte-check, strict TS) | CI on every push + PR |
| Unit + integration tests | ~1,640 Rust `#[test]` cases across the workspace + Svelte component tests; `cargo test --all-features` | CI + local |
| Lints beyond the type system | `infra/lint/seed-bypass-smell.sh` rejects seed scripts that bypass the Workflow path; `cargo clippy` lint set is the strict superset | CI |
| Audit-log integrity | `boss-audit-integrity-check` verifies the per-row hash chain on `audit_log` and the `REVOKE UPDATE, DELETE, TRUNCATE` schema-level append-only enforcement | cluster CronJob (daily) in prod |
| Conservation invariants | `infra/lint/conservation-invariants.sh` proves the five-property correctness protocol — provenance, conservation, closure, idempotence, determinism — across every projection vs. the `audit_log` it derives from | cluster CronJob in prod, on-demand locally |
| Replay rebuild | `boss-rebuild-all` reconstructs every projection from `audit_log` alone; `infra/postgres/validate-brewery-sim.sh` runs a sim-year and asserts 0 net drift across every rebuilder | on-demand (maintainers, before a release cut) |
| Binary drift | `infra/check-binary-build-coverage.sh` catches `*-api` binaries that would silently boot in-memory because their `postgres` feature isn't activated; the launcher's roster is pinned to the port registry by `boss-ports`' tests | gate + on-demand |

The "always green" claim isn't aspirational. Audit-log integrity
runs every 24 h; conservation invariants run on a schedule; every
scheduled run leaves a packet in the system of record and a
cadence-silence sweep alarms when one stops arriving. A red signal
anywhere is an incident, not a flaky test to retry.

```sh
# Workspace build (15–20 min cold, ~30 s warm).
cargo build --release --workspace

# Full test sweep — ~1,640 Rust tests + the bun web suite (~3 min cold).
cargo test --all-features
( cd apps/web && bun run typecheck && bun test )

# Run the in-tree integrity checks against a local Postgres.
infra/lint/conservation-invariants.sh
```

CI configuration: [`.forgejo/workflows/ci.yml`](.forgejo/workflows/ci.yml),
which runs on the internal forge and invokes the one gate definition,
[`infra/gate.sh`](infra/gate.sh). The public GitHub mirror runs the same
script, full mode, on every publish PR
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)), beside the
CodeQL and Scorecard security scans — so its green check means the tree
passed the gate, not only that static analysis found nothing. It gates
nothing: changes merge on the internal forge. Every merge converges the cluster on its own
(the forge builds the image for the merge sha and rolls it;
[`docs/runbooks/operator.md`](docs/runbooks/operator.md) §Deploy).

## Founding ideas

Three intellectual lineages sit behind the design: **Stafford
Beer** (cybernetics — companies as viable systems shaped by
feedback loops and *algedonic* signals), **Rich Hickey**
("information is simple" — data is primary, mutation pays
forever, event-sourced + immutable values everywhere), and
**George Orwell** (*Politics and the English Language* —
communication decays when words drift from reality, so the audit
log + the five-property correctness protocol keep the language
operators use anchored to the facts).

All three converge at the same claim from different angles: the
company *is* its event log + its current state + the rules
connecting them. Full elaboration in
[CLAUDE.md](CLAUDE.md#founding-ideas--what-every-load-bearing-decision-is-measured-against).

## License + acknowledgements

Apache-2.0. Named in tribute to Stafford Beer. The brewery demo
data — every employee, account, vendor, invoice, and recipe in
the Algedonic Ales seed — was fabricated by Claude (Anthropic's
LLM) as part of building this demo. Names and figures are
LLM-generated; any coincidence with a real person, business, or
document is unintentional.
