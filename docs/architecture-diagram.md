# BOSS Architecture Diagrams

Four views of the system, ordered from conceptual to concrete. Each
diagram exists once, as Mermaid source in `docs/architecture/*.mmd`.
The in-app IT Knowledge Base (`/it/kb`) draws them in the browser from
that source every time it opens, and any Mermaid renderer
([mermaid.live](https://mermaid.live), `mmdc`) draws them from the
same file. No rendered SVG or PNG is committed.

Renders used to be committed here and copied into the web app, three
files per diagram with nothing comparing them. Nobody's environment
could regenerate them, so after a service was retired from the source
every picture went on drawing it (backlog 4718d918). A picture made at
view time has no copy to fall behind, and
`apps/web/src/it/kbDiagrams.test.ts` fails if a render is committed
again or a source is added that the page does not draw.

---

## 0. The framing — State · Surfaces · Work

**BOSS models the whole company as a software system.** At the
highest level it splits into three things:

- **State** — what is true about the company right now (and what has
  happened). Subjects (tracked entities with event logs) + Knowledge
  Base (classes + types) + `audit_log` (the append-only source of
  truth) + Ledger projection (financial state).
- **Surfaces** — how humans see and act on the state. The web SPA,
  step-plugin bundles, the unified Messages inbox, the `boss` CLI.
- **Work** — how state changes. Jobs + Steps (coordination), the
  Workflow / StepPlugin / StepType registries (workflows as data),
  automation runners that turn events into work (`boss-dispatcher`
  step side-effect rules, tenant tick engines), agents claiming steps
  through the same claim door as humans (`boss dispatch`), and policy (row-level authorization as rows, not
  code).

Diagram: [`architecture/00-state-surfaces-work.mmd`](architecture/00-state-surfaces-work.mmd) (drawn at `/it/kb`, §1).

This is MVC stretched to company scale, with one important caveat:
classic MVC's "Controller" is a thin router between Model and View.
BOSS's Work layer is a **substantive coordination layer** — Jobs
are stateful, registries are authoring surfaces, the dispatcher reacts
to events with new work. So we use MVC only as a *shape* analogy;
the company-native vocabulary (State / Surfaces / Work) is clearer.

Every subsequent diagram zooms progressively inward from this
framing. Diagram 1 opens up the State + Work primitives. Diagram 2
shows the services that implement them. Diagram 3 shows where those
services run.

---

## 1. Primitives & cross-cutting abstractions

**The "why it stays simple" picture.** BOSS models every business
concept with four primitives, declares new work as registry data
instead of new code paths, puts hexagonal ports between domain and
infrastructure, and emits every state change as an immutable fact onto
a single event backbone.

Diagram: [`architecture/01-primitives.mmd`](architecture/01-primitives.mmd) (drawn at `/it/kb`, §2).

**Load-bearing choices:**

- **Four primitives** (Subjects · Jobs · Steps · Events) carry the
  state-machine vocabulary. New entities are modeled as Subject kinds;
  new work as Workflows; new transitions as StepTypes. The Class
  registry, StepPlugins, and Policy are supporting concepts on top.
- **Registries over match branches.** A new work type is a `workflows`
  row, a new step UX is a `step_plugins` row + a JS bundle. Zero core
  code changes. Workflows are version-pinned so in-flight Jobs keep the
  graph they were opened under.
- **Hexagonal.** Each domain crate defines a port trait (`AssetsRepository`,
  `JobsRepository`, …) and never imports the Postgres / reqwest /
  in-memory adapters that implement it. The adapters are swappable;
  the domain doesn't know which one it got.
- **Event backbone.** Every primitive write emits an immutable fact
  through NATS and lands in `audit_log`. The log is the source of
  truth; projections rebuild from it. If current state disagrees with
  a replay, the log wins.
- **Cross-cutting rails.** Policy gates every write. Ledger projects
  `financial_facts` into the GL. Messages carries both direct messages
  and system signals on a unified inbox. Each rail is one crate that
  every domain can call through a client port — domains don't know
  about each other's data, only their own.

---

## 2. Service map

**"Which thing calls which thing."** Draws every service in the port
registry (`crates/core/boss-ports`), grouped by crate tier, and shows
how cross-service calls are shaped. Each service node names its
registry row as `<name> :<port>`, and `boss-ports`'s
`service_map_agreement` test holds the drawing equal to the registry:
a row not drawn, or a node with no row, fails by name (backlog
f1d84e3f).

Diagram: [`architecture/02-service-map.mmd`](architecture/02-service-map.mmd) (drawn at `/it/kb`, §3).

**How to read it:**

- **Edge** — the browser and CLI enter through `boss-gateway`, which
  serves the SPA, gates auth via the configured provider (`local-auth`
  for v0.1 — file-backed email/password credentials), and
  reverse-proxies to every `/api/*` route. The gateway is HTTP-only —
  TLS termination is the reverse proxy's job, not the gateway's.
- **Tier 1** (yellow) is the core state-machine OS — jobs, policy,
  the registries, events, clock, calendar, content, search, views —
  plus its runtime daemons (purple): `boss-dispatcher` runs the step
  side-effect rules off `step.done.<kind>`, and `boss-event-relay`
  moves the event outbox into `audit_log` and NATS.
- **Tier 2** (blue) is the company-modeling modules — people,
  accounts, commerce, inventory, shipping, ledger, messages and the
  rest. Each has its own Postgres schema + event stream; no shared
  tables across services.
- **Orchestrators** (pale yellow) fan out across both tiers — the ML
  API and the `/simulator` UX.
- **Tier 3** (pink) is the one example tenant, Algedonic Ales: the
  brewery sim daemon, which drives the public API.
- **Cross-service client crates** are the narrow HTTP contract. If
  `boss-assets-api` needs to ask `boss-people-api` "does this employee
  exist," it calls `PeopleClient::employee_exists()` — a trait method
  on a tiny crate, backed by reqwest in prod and a fake in tests. No
  service ever talks to another service's database directly.

**The tiers, crate by crate.** Four tiers in the workspace today:

- **Tier 1 — core state-machine OS** (`crates/core/`, 25 crates).
  `boss-gateway`, `boss-jobs-api`, `boss-dispatcher`, `boss-policy-api`,
  `boss-classes-api`, `boss-locations-api`,
  `boss-subject-kinds-api`, `boss-calendar-api`,
  `boss-content-api`, plus the libraries
  (`boss-core`, `boss-events`, `boss-ml`,
  `boss-testing`, `boss-ports`, `boss-nats`) and matching
  `*-client` crates. Yellow +
  most of the rails. **Tightest review bar; correctness protocol
  non-negotiable.**
- **Tier 2 — company-modeling layer** (`crates/modules/`,
  18 crates). `boss-people-api`, `boss-commerce-api`,
  `boss-inventory-api`, `boss-shipping-api`, `boss-ledger-api`,
  `boss-messages-api`, `boss-catalog-api`, `boss-assets-api`,
  `boss-products-api`, `boss-accounts`, plus matching `*-client`
  crates. Most of the blue + green clusters.
  Inherits the core's correctness contracts but the domain
  surface evolves at business speed.
- **Orchestrators** (`crates/orchestrators/`, 6 crates). Cross-
  tier binaries that fan out across both: `boss-rebuild`,
  `boss-cli`, `boss-sim`,
  `boss-ml-api` (wires the Tier-1 ML framework + Tier-2 plugins),
  `boss-simulator` (the standalone `/simulator` UX service).
- **Tenants** (`crates/tenants/`, 1 crate).
  `boss-brewery-engine` (Algedonic Ales). Outside the tier system;
  tenant-shaped.

A Tier-1 LIBRARY crate must NOT depend on a Tier-2 crate
(orchestrators are exempt by design). Enforcement landed in
[`infra/lint/tier-import-audit.sh`](../infra/lint/tier-import-audit.sh)
which runs cleanly today (0 violations across 27 core crates).

---

## 3. Deployment topology

**Where the bits run** — for a deployer of the open-source release:
the quickstart, `infra/oss-quickstart/` (backlog 34717528, decided
2026-09-25).

Diagram: [`architecture/03-deployment.mmd`](architecture/03-deployment.mmd) (drawn at `/it/kb`, §4).

**Key facts:**

- Docker compose runs Postgres, NATS and one `boss-services`
  container. Its launcher (`services-launcher.sh`) starts every
  service in the port registry that the tenant manifest's `[modules]`
  asks for, platform services always, and the gateway last.
- A one-shot `boss-init` runs on every start: it converges the
  schema and seeds the platform Workflow bundle, and on first start
  provisions the bootstrap-admin credential and primes the sim clock.
- The gateway listens on host port 4443 over plain HTTP. TLS belongs
  to a reverse proxy in front of it (`infra/caddy/Caddyfile` is the
  reference); the compose file does not bundle one.
- Credentials and attachment bytes live in the `boss-auth` and
  `boss-files` volumes; back up `boss-files` with `postgres-data`.
- Where a given instance actually runs — ours is a cluster — is data
  in its estate registry, rendered live at `/it/estate`. The diagram
  does not draw it, because a drawing would be a second copy.

---

## Maintenance

These diagrams are part of the repo. Update the `.mmd` source in the
same commit as whatever architectural change triggered the update —
if a new client crate lands, the service map should reflect it; if a
new primitive or rail lands, the primitives diagram should too. There
is nothing to regenerate: the `.mmd` is the diagram, and `/it/kb`
shows the edit on the next deploy.

Stale architecture diagrams are worse than none — if this file drifts
from reality, mark it so and open a TODO to resync.
