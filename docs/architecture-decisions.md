# BOSS — Baseline Architecture Decisions

This is the **consolidated decision record** for BOSS: one thematic
walk through every load-bearing choice in the running system,
written as current truth. It absorbs the v0.1 pre-release record
(~180 decisions), the v1.1 ADR catalog (the step-UX plugin model,
the dispatcher-as-event-router and JobKind-v2 decision sets, step
types as property bundles, the Intangible subject root), and the
design documents whose work has shipped. There is no separate
history to cross-reference: what this document says is what the
code does.

**How decisions evolve.** Open questions are authored as
`### Qn:` anchors in living docs under `docs/design/`; the in-app
decision tracker (`/system/design`, backed by `boss-docs`) manages
them; resolutions flush into the source doc's Decision history.
Each release, settled material folds into this document and the
source doc is deleted — the baseline is the canonical post-flatten
record. Docs that survive under `docs/design/` are living
references (reading frames, contracts, governance rules), not
decision archives.

---

## Thesis & positioning

BOSS is a **technical proof of a simple thesis**: model the
operating system of a company directly as a state machine, and the
abstraction layers traditional ERPs/workflow platforms accumulated
fall out as scaffolding around a missing primitive rather than as
load-bearing structure. The codebase is small on purpose — small
enough that a human reviewer can audit the entire production
output in a sitting, which makes it a substrate that pairs well
with modern AI authoring tools. The running system stays plain
Rust + Postgres + SPA with no model in the request path; AI
mediates at authoring time, not at runtime. The forward direction
is **modeling UX and experimentation**, not new domain modules.
The correctness goal is **TLA+ provability** — every state-machine
transition, projection, and invariant small and clean enough to
specify formally if pushed (`docs/formal/` carries the first two
specs: Step lifecycle, ledger period locking).

Three intellectual lineages anchor the design (CLAUDE.md
§Founding ideas): **Stafford Beer** (a company is a viable system
describable in feedback loops; the dispatcher is the feedback
layer; *algedonic* signals are rules firing on threshold events),
**Rich Hickey** (information is simple; data is primary; the audit
log is the system of record and projections are pure functions of
it), and **George Orwell** (language anchored to reality; the log
holds what *did* happen so the words operators use stay anchored
to facts — and the repo's own vocabulary is held to the same bar:
one word per concept, enforced by rename passes and lints).

**The public repo carries no inherited git history** — every
release is a fresh rooted commit cut from the working tree. The
working tree is the canonical record; docs are self-contained; no
"see commit X" references survive into the public repo. Public
demo tenants are **Algedonic Ales** (the brewery) and the
**used-device-shop**, both instantiations of company-management on
the same state-machine abstraction.

## Primitives & information architecture

Four primitives model everything: **Subjects** (identity-bearing
things work is about), **Jobs** (bounded units of coordinated
work), **Steps** (typed transitions inside a Job), **Events** (the
immutable record — the system of record). Three supporting
concepts hang off them: the **Class registry** (every taxonomy as
data), **StepPlugins** (step UX as data), and **Policy**
(row-level privilege rules).

**Subject is a trait, not an enum.** Each kind implements it with
its own KB view; the wire shape is a flattened
`{ subject_kind, id }` pair (the old per-kind tagged-enum payload
keys are gone). `subject_kind` is an open string validated against
the SubjectKind registry; the platform ships its kinds as registry
rows, and tenants add kinds without touching core. The expression
language reads `subject.kind` / `subject.id` — that DSL surface is
stable independent of wire serde.

**Five roots** seed the SubjectKind taxonomy: the four noun axes
**Person** (`boss-people`), **Place** (`boss-locations`), **Thing**
(`boss-assets` for tracked units, `boss-catalog` for the model
registry), and **Intangible** (identity-bearing things with no
physical embodiment — agreements, campaigns, workflow documents
like purchase orders; the home for future contract/SLA/lease
kinds), plus **Calendar**, the time-coordination primitive. A
`NULL parent_kind` on a platform row means "TBD", never "special".
The `custom` kind stays deliberately outside the taxonomy as the
escape hatch. The tracked physical unit is an **asset** at every
layer — crate, routes, `asset.*` event kinds, tables, types,
subject kind — and the word "system" means exactly one thing in
this repo: the organization being modeled.

**Subject creation is identity-first.** A Subject can exist from its
stable id alone, before everything about it is known, and accrete
data incrementally — the Subject-level form of the Step rule
"required-at-done, not required-at-create." The asset is the worked
example: an asset is born by a `Registered` event carrying only its
id (`phase = registered`, no `sku`), and its catalog model, custody
(`Received`), and location arrive later as enrichment events — sku is
nullable, identity is not. A registered-but-unidentified asset
honestly has no model-derived attributes (no depreciation basis, no
Equipment-KB model view) until an `Identified` event sets the model.
The general principle: the only hard constraint on creating a Subject
is its identity; any further "required at create" constraint is data,
not a baked-in NOT NULL or event field.

A **Class** is not a Subject: Classes are typed reference data
keyed `(subject_kind, code)` that each Subject kind owns — roles,
account types, asset models, departments all land in the one
`classes` table (living reference:
`docs/design/class-registry.md`). Parts are Subjects in their own
right; the Composite primitive is heterogeneous and laws-checked
via proptest at the trait boundary.

The system is laid out on a **three-axis information
architecture**: *Knowledge Bases* (durable queryable state),
*Surfaces* (operator UI), and *Work* (Jobs + Steps that change
state). Every KB-exposing domain implements the shared `KB` trait
from `boss-core`; facts live in domain tables, not a global facts
table; aggregations rebuild on-demand + periodically.

## Jobs, JobKinds, Steps

A **Job** is a bounded unit of coordinated work: stable identity,
owner, subject, status, and a structured list of Steps. The
**JobKind registry** is append-only and versioned; in-flight Jobs
pin to the version they opened under; creation is blocked against
`draft` and `retired` kinds. Adding a new workflow means adding a
JobKind row — never a `match` branch in core code.

**The DAG is implicit in predicates.** Each step declares
`ready_when` — a pure expression over
`(subject, job, prior step states)` — and an edge A → B exists iff
B's predicate references A. `blocked_by` is a derived,
denormalized edge list for rendering, recovered from the
predicates. Predicates are **pure over immutable inputs**:
external state (clock, inventory, balances) is out of bounds —
reactions to external state belong in dispatcher rules, so replay
is deterministic across evaluator versions. Materialization is
**eager with status**: every step exists from Job creation
(Pending → Ready → Active → Completed, plus Skipped), the
re-evaluator is the readiness authority, and structural transition
events emit on every status change. **No loops at the workflow
layer** — iteration lives inside a step or in a sub-Job.
**Terminals are explicit** (a `terminal` flag on the step spec;
multiple per kind), and the **viability lint** proves structural
invariants, reachability, and fork coverage at publish time; fork
coverage over open-ended fields requires a wildcard fallback. A
predicate dependency index is built at publish; runtime
re-evaluation is incremental. The predicate DSL is a tiny custom
language shared verbatim with the dispatcher's `when` clauses and
handler args.

**Sub-Jobs are a typed contract** (`delegate-subjob` — the one
spelling): the parent step's completion *is* the child Job's
close, parents must handle every possible child outcome, and the
dispatcher performs the close → resolve write-back. Required
metadata is checked **at done, not at create**;
`PUT /api/jobs/{id}/steps/{step_id}` has PATCH semantics
(top-level fields replace wholesale; clients merge metadata keys).
The Jobs list takes exactly one subject filter — `?subject_id=` —
and the Job's subject column is `subject_id`.

**JobKinds bootstrap through Jobs.** The system-owned
`job-kind-design` kind authors new JobKinds inside a Job (draft
edits live in the authoring Job; the terminal `job-kind-publish`
step writes the registry row), so the platform's own catalog is
published with full audit provenance — the system models its own
development. Platform kinds ship in code (`platform_kinds()`);
tenant kinds load from `examples/<tenant>/seeds/job_kinds.toml`
(governance rule: `docs/design/platform-vs-tenant-jobkinds.md`).

**Authoring is graphical and author-gated.** The `job-kind-design`
surface is an interactive trigger→outcome canvas (Svelte Flow + dagre,
code-split onto the editor route): steps are nodes (trigger / terminal
/ fork / work), an edge A→B *is* `steps.A.done` in B's `ready_when`, and
a structured predicate builder emits the boss-expr behind a
live-validated raw "advanced" escape hatch. A non-persisting dry-run
(`POST /api/jobs/kinds/_validate`) runs the publish-path lint against
the same in-process `StepRegistry::v1()`, so editor-green publishes by
construction; the SPA persists drafts as `metadata.job_kind_spec`
PATCHes on the design Job and never calls the direct `/api/jobs/kinds`
create/update/publish handlers (kept only for bootstrap + tests). The
design **approve** step requires a `job-kind-approver` capability —
authoring a work-type is operational leadership's call, not the deploy
operator's alone (core policy grants it to `platform-admin`; tenants
grant it to their leaders; `design-doc-review` stays `platform-admin`).

## Step types are property bundles; the alphabet is the mechanisms

A step *type* enforces rules, and each rule is an orthogonal,
data-expressible property. **What stays code is the closed
mechanism set**: the completion authorities, the validator engine,
the lint protocols, the expression DSL, the surface host, and the
side-effect handler verbs. Every named StepType is a **property
bundle over those mechanisms** — an append-only, versioned,
tenant-authorable registry row carrying:

- **Completion contract** — a `fields` schema (required-at-done +
  per-type value checks). Steps may also author `fields` inline in
  the JobKind; validation is the union, so single-use vocabulary
  needs no registry row at all.
- **Completion authority** — one enum: `human` (an operator
  holding `authority_role`; default), `agent` (a computed
  decision; with an `outcome` enum field it is a gate resolved by
  the dispatcher's gate handler), `child-job` (the delegate
  contract), `external` (a bound counterparty event completes the
  step — **binding**: the jobs API rejects manual completion; the
  policy-gated operator override is its own audited action; the
  source is named by dispatcher rule), and `auto-on-materialize`
  (the `trigger` special case — a trigger describes job-creation
  conditions and has no completion logic of its own, so it is
  resolved at materialization: the firing trigger is born
  `Completed`, its alternatives `Skipped`. Which one fired is read
  from the Job's `metadata.trigger_name` — stamped by the
  `jobs.spawn` rule that opened the Job — so a Job authored with
  several triggers records only the one that actually fired, never
  all of them. Downstream steps fan in with `steps.a.done OR
  steps.b.done`).
- **Sign-off requirements** — see below.
- **Render surface** — a surface id into the surface table
  (platform-shipped components and tenant StepPlugins are the two
  suppliers); plus layout (`ux`), category, and a duration model
  (typical hours + jitter) the simulator reads.

**No core code may match on a step-kind name** — enforced from day
one by `infra/lint/no-step-kind-match.sh` (ratchet allow-list:
exactly the two platform-pinned rows, `job-kind-publish` and
`review-design`). The registry ships 43 bundles; identical-property
bundles merge on sight (`approval` folded into `sign-off`,
`generic` into `task`, `sub-job` into `delegate-subjob`), and row
count is editorial — rows are cheap shared vocabulary, code seats
are what the lint forbids.

**Sign-off is a completion property, not a kind.** A sign-off is
the stamping of a step, *in its current shape*, by an
authenticated authority — policy-enforced — so steps can require
that multiple authorities agree before completion. Requirements
are a role list (`sign_offs_required`, requirement-object shaped
so k-of-n can land without a wire break); stamps are
`(authority_id, role, stamped_at, shape_hash)` where `shape_hash`
binds the stamp to the title + canonically-serialized metadata it
attested. Stamping is its own act (`POST …/sign-offs`), authorized
against the role-scoped policy resource `step-signoff:<role>`,
emitting `jobs.step.signed_off`, idempotent per (role, shape).
Completion requires every required role to hold a current-shape
stamp; editing a stamped step emits `jobs.step.stamps_invalidated`
(loud) and stale stamps stay recorded as provenance. Two storage
invariants back this: **stamps are append-only at the row**
(`sign_offs || stamp`; no generic write path carries stamp
fields), and **terminal statuses are immutable at the row** (a
write merged against a stale pre-completion fetch cannot demote
Completed/Skipped) — both proven necessary by race forensics
against the live dispatcher.

The v1 step-type catalog derives from the traditional software
stack BOSS replaces (CRM/ITSM/ERP/HR/comms); the canonical source
is data (`crates/core/boss-jobs/seeds/step_types.toml`), loaded by
`StepRegistry::v1()`, with `core_v1()` as the company-free tier.

## Dispatcher — the event router

Side effects are data: **steps emit events; rules in the
dispatcher's registry watch for those emissions and invoke
handlers.** Rules are rows in the append-only versioned **`dispatcher_rules`
registry** (`on_event`, `when`, `do`, over the shared expression
DSL) — the step_plugins-style draft → active → retired lifecycle,
authored in-app at `/system/dispatcher/rules` (`infra/dispatcher/rules.toml`
is now just the human-authored seed source, not the runtime read).
The reactive wiring is visualized as a cascade — trigger event →
rule → handler → emitted event → re-triggered rule, feedback cycles
highlighted, filterable by trigger event — at `/system/dispatcher`. The
dispatcher is reactive, not a catalog of everything the system can
produce. Each rule is an
**actor**: every side effect it fires is attributed
`automation:rule:<name>`, so "why did this Job spawn?" is a query
over data. Sim and prod run the **same dispatcher binary**;
operator-initiated Jobs bypass the dispatcher (it routes
reactions, not commands). Clock interaction is an ephemeral stream
plus one-off queries. The handler vocabulary (`po.place`,
`invoice.issue`, `jobs.spawn`, `gate.resolve`, `webhook.notify`,
…) is the adapter edge — the verbs that touch the world stay code;
which verb fires when is data.

Where the mechanisms live now: step side-effects are rules keyed
`step.done.<kind>` (the old `StepType.side_effects` field and the
step-effects runner are gone); inventory auto-restock is a rule
whose open-PO predicate is the idempotency check; sim Job rates
ride `clock.tick.daily` rules; and the **CounterpartyEngine stays
in the simulator deliberately** — its probabilistic choices model
external actors, the dispatcher is deterministic, and the
`webhook.notify` handler forwards triggering events to the
engine's callback server, which replies over the public API only.
The sim/system boundary **is** the HTTP API: one set of surfaces
serves real actors, the simulator, and side-effect handlers
identically; the simulator presents as the role-matched humans it
assigns, with no exemptions anywhere in policy or validation.

## Correctness protocol & the audit log

The five-property protocol — **provenance, conservation, closure,
idempotence, determinism** — is a first-class invariant (living
reference: `docs/design/correctness-protocol.md`). The audit log
is the system of record; projections are pure functions of it;
rebuilders reproduce truth from it
(`docs/design/projection-rebuilders.md` is the living contract);
the system contributes zero error of its own. Every projection row
representing a sim-time event stamps **the sim-day the engine
emitted**, never wall-clock `NOW()`.

The log itself is tamper-evident in three layers: append-only
enforcement (BEFORE-triggers reject UPDATE/DELETE), a **hash
chain** (each row stores its predecessor's hash and its own,
computed by a trigger that assigns ids post-advisory-lock so the
verifier's id-walk matches commit order; the chain columns ship in
the schema, chained from the genesis row), and a **daily
checkpoint** that emits the chain head outside the database for
auditor comparison. `boss-audit-integrity-check` walks the chain
on a timer; the release's validation gate (`validate-brewery-sim.sh`)
hard-fails unless the full replay (every rebuilder, from the log
alone) and the integrity check both pass. Every event names its actor —
**there is no anonymous "system" actor**; the four deliberate
spellings (`ActorId` type, `actor` publisher param, `_actor`
payload key, `actor_id` boundary field) are documented in
`boss-core::actor` and must not be flattened. Origin markers on
registry rows use `owning_team = 'platform'`, not 'system'.

## Finance & ledger

The ledger is a dedicated crate consuming `financial_facts` via a
`FactSink`; the same facts also project from `audit_log` via
data-driven `gl_fact_projection_rules`, so the
rooted-at-audit-log replay check stays viable. RuleSets are
versioned per-RuleSet; rebuild has online and offline modes;
periods are monthly with a fiscal-year close pass; the chart of
accounts is seeded and admin-authored. Financial statements read a
**`gl_account_daily` rollup** (per-account/day debit + credit +
attributed-cash totals) instead of scanning `gl_journal_lines ×
gl_journal_entries` per request; the rollup is incremented live in
`post_fact_in_tx` (same tx as the journal write) and re-derived on
rebuild, so it stays a pure function of the log. Money is an inline
TEXT currency column on every money-bearing row; `Currency` lives
in `boss-core::money`; column prefixes (`amount_`, `price_`,
`cost_`) distinguish kind, not currency.

**Counterparty prices are data; our costs emerge.** The vendor's
agreed price (`inventory_items.vendor_price_cents`, seeded per
part) prices the PO **once, at placement** (qty from our
reorder_qty, unit price theirs; an unpriced part refuses placement
loudly). Receiving and bill-approval read the PO's lines — the
purchasing contract — so receipt value, the vendor bill, and the
emergent weighted-average `avg_cost_cents` chain from the same
numbers; `avg_cost` is never an input to purchasing. COGS is
modeled directly from the bill-of-materials × input prices —
margins emerge, never hard-coded. Revenue recognition: hardware at
shipment; `service` defers via `revenue_schedules`; `parts` and
`new-sales` recognize immediately; the recognition scheduler runs
daily and respects locked periods. Sales tax rides
`tax_lines` on the issued-invoice fact and remits per
jurisdiction. The single-shot "DR Cash / CR AR" invoice-paid rule
is deliberately not mapped for tenants whose bank-clearing chain
emits the canonical two-phase pair — double-crediting AR was
observed live and the projection mapping is the cut point.
Finished products are tracked per-location with cost basis
(produce/consume handlers + the products KB); invoices are
line-item based with header rollups checked on write.

## Policy & auth

Every write passes `boss-policy` via the `PolicyClient` port.
Rules are row-level grants of `(action, resource)` within a scope;
user overrides take precedence; every decision is auditable. Scope
predicates are named in code (`Self_`/`Team` compile to
**owner_id** predicates — a Job's *owner* is who is responsible;
a Step's *assignee* is who executes; the distinction is
load-bearing and deliberately not flattened). Sign-off authority
is policy: stamping authorizes against `step-signoff:<role>`
resources, uniformly — simulator included. The policy client
**fails closed**; a 60s TTL cache floors correctness with NATS
invalidation as the convenience overlay. SPA auth is file-backed
credentials managed by the gateway's admin CLI; SSH is
bring-your-own-keys with the SSH-CA flow parked as an opt-in
blueprint.

## Calendar

Reservations store **UTC**; `strength` defaults `hard` for
subjects that can't double-book, `soft` for advisory holds.
Multi-occupancy resources are distinct subjects, not capacity-N; one
reservation per subject per event (sharing `reason_ref_id`).
**A reservation is on a `Subject`** `{kind, id}` — not a closed
resource enum. Which kinds may be reserved is data: a
`calendar_reservable` flag on the subject_kinds registry (employee,
asset, account at v1), enforced by the calendar on reserve; the GIST
exclusion constraint guarantees one hard reservation per individual
subject per overlapping window.
The `reason_kind` is likewise a **free-form tag**, not a closed enum —
the conventional values BOSS emits (`job-step`, `pto`, `meeting`, …)
live as consts in `boss_core::calendar::reason`; a tenant uses its own
reason without a core change.
Cancellation is synchronous with the step update; PTO lives in HR
and the calendar sees only approved PTO; the jobs↔calendar hook
reserves before persistence so a hard conflict can 409 without
half-writing the step.

## Locations

Locations are a Subject kind with a parent hierarchy (no hard
depth cap; warn at 8). Address is free text at v1. Location-Part
singleton enforcement is a write-path helper; movement history is
event-log only.

## Simulator

One **shape-driven engine** drives both tenants; per-tenant flow
is data (`job_kinds.toml` step graphs; `tenant.toml` rates, ramps,
anomalies, shocks, counterparties, periodic and batch cycles). The
workforce executor claims and completes **assigned** steps through
the public API as the role-matched employees, filling
required-at-done fields (bundle + step-authored) and collecting
sign-off stamps before completing — metadata first, stamps
attesting the final shape, then the status flip. Gates are
agent-executed by the dispatcher reading real stock — the
workforce never sees them. Batch engines (payroll, taxes) are
generic over Population + Rule traits. Warp is honest: the sim
runs at the throughput the serial write path sustains, and the
canonical 365-day world must pass hard-fail (any non-2xx aborts),
queue drain, full rebuild parity, and chain integrity for the
validation gate to go green. The scratch stack mirrors prod at +1000 ports
for experiments. The daemon is a **cursor-gated auto-tick loop**:
clock-authoritative time, each sim-day processed exactly once
(`days_to_run`) — which fixed the cold-start over-firing (periodics +
rate engines re-firing on overlapping day windows) without the
heap-scheduler refactor that was prototyped (`boss-sim/scheduler.rs`)
but deliberately not adopted, the simpler cursor gate being sufficient.

## ML platform

`boss-ml` + `boss-ml-api` (gateway-proxied under `/api/ml/*`);
inference plugins live in `boss-ml-plugins` and register via
constructor wiring — no dynamic loading. Models bootstrap from
embedded TOML seeds; predictions store as JSONB; scheduling is
systemd cron. Next-action rules and risk scoring are declarative
rule models with plain string-template substitution — no embedded
scripting.

## Content, files, knowledge

Bulletins and the company manual are separate tables in
`boss-content`, Markdown-authored, searched via the shared FTS;
the manual writes a history row per edit; bulletin audiences are
JSONB predicates evaluated in Rust. **File attachments are
first-class auditable artifacts**: a two-port design (metadata
rows + content storage) with upload/GC lifecycle, served through
the gateway, rebuild-deterministic like every projection. Each
domain's KB documents hang off the `Document` type; the Equipment
KB keeps typed columns for stable queried fields and a
schema-validated `extras` blob for tenant-specific evolution; the
event stream remains the source of truth for asset state.

## Step UX & frontend

Step surfaces ship as **data**: the registry row names a
`surface` id; the SPA loads the step-type registry once and mounts
tenant StepPlugin → the platform surface the registry names →
the generic fields/notes card. Plugins are JS bundles in rows
(`step_plugins`, append-only versioned; steps pin the plugin
version at creation), served by the gateway at `/plugins/<path>`,
mounted framework-free with declarative validation — new step UX
never requires a core SPA change. The frontend is **Svelte 5 with
Runes** (no stores), one Bun bundler, in-app router, CSS grid
layout. Live views poll at 60s with SSE push where the policy doc
says push pays (`docs/design/sse-policy.md`); the System Diagram
complements the HQ map; account detail composes KB panels
(devices, invoices, shipments, agreements, notes) over the
domain APIs. The ports table (`boss-ports`) is the single source
of truth for service names/ports — the SPA's generated copy is
lint-checked against the Rust registry.

## OSS posture & tier boundaries

Two install paths: single-VM bare metal (`infra/oss-quickstart/`)
and Docker compose. File-backed auth is for evaluation; HA
topologies return as opt-in blueprints under `infra/blueprints/`.
Crates split into **Tier 1 — core state-machine OS**
(`crates/core/`, 27 crates: the four primitives' services, policy,
gateway, dispatcher, clock, expression DSL, taxonomy registries,
calendar, content, docs, ML stack, cybernetics, testing, ports,
plus `*-client` crates) and **Tier 2 — company-modeling layer**
(`crates/modules/`, 16 crates: people, accounts, commerce,
inventory, shipping, ledger, products, messages, catalog, assets,
clients, ML plugins). A non-company tenant deploys Tier 1 alone.
**Orchestrators** (`crates/orchestrators/`: `boss-rebuild`,
`boss-cli`, `boss-sim`, `boss-ml-api`, `boss-simulator`) fan out
across tiers by design; **tenants** (`crates/tenants/`: brewery
engine, used-device-shop engine) carry tenant binaries.
`infra/lint/tier-import-audit.sh` enforces
Tier-1-never-imports-Tier-2 for libraries. Seeds never write
emergent state — if a seed wants to `INSERT INTO invoices`, the
answer is a JobKind (`docs/design/seed-vs-emergent-state.md`,
enforced by `seed-bypass-smell.sh`); the canonical demo world is
**built live, not migrated**: the install starts the sim and it
generates 365 simulated days of events against the live API.
