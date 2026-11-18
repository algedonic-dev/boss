# BOSS — Baseline Architecture Decisions

This is the **consolidated decision record** for BOSS: one thematic
walk through every load-bearing choice in the running system,
written as current truth. It absorbs the v0.1 pre-release record
(~180 decisions), the v1.1 ADR catalog (the step-UX plugin model,
the dispatcher-as-event-router and Workflow-v2 decision sets, step
types as property bundles, the Intangible subject root), and the
design documents whose work has shipped. There is no separate
history to cross-reference: what this document says is what the
code does.

**How decisions evolve.** A design doc **is a packet**, not a file.
Its prose and its open questions ride on a `design-doc` Job, so it is
reviewable the moment it exists rather than after it ships, and the
answers are recorded on the packet — which is the record. A revision
is a **new packet carrying `translated_from`**, never a mutation, so a
doc's life is a chain and the chain is its decision history, with an
actor and a timestamp on every answer. **This document is the fold**:
current truth per topic, assembled from those packets, with the
history beside it rather than inlined. It stays hand-maintained
because merging prose has nuance a generator cannot judge — but the
`fold` step of the `design-doc` workflow makes updating it an
obligation the protocol enforces rather than an intention somebody
holds. Files under `docs/design/` are the legacy corpus, being
translated into packets; a generated tree at release keeps `git grep`
and the OSS install honest, and because it is generated nobody
hand-edits it and it cannot drift.

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
example tenant is **Algedonic Ales** (the brewery), an instantiation of
company-management on the same state-machine abstraction. David decided
on 2026-09-24 to retire the used-device-shop example and retain modules
used by Ales or Algedonic, LLC. Its engine, seed bundle, refurb lifecycle,
service-history and launch-calendar projections have left the product.
The generic company modules remain; retired example reference rows stay
available to the bounded eviction reader, and applied migrations and
recorded history keep their provenance.

### The Viable System Model, mapped — which BOSS concept plays which

Beer is the namesake and cybernetics is first among the three
lineages, but **VSM vocabulary does not appear in BOSS code**. It used
to, in exactly two crates — `boss-cybernetics` (whose own header read
"Per-VM Cybernetics coordinator (VSM S2/S3)") and
`boss-observability` — and both retired under design 8382bbb2 on
2026-09-23, each in its own car (below). The
mapping is recorded here so the correspondence outlives the code that
carried the words, because a reader arriving from Beer must be able to
find it and a reader arriving from the code must not have to learn a
second vocabulary to work (backlog 6872efd4). Retiring the only S2
implementation without writing this down is how a founding idea
quietly becomes decoration.

| VSM | BOSS | where it lives |
|---|---|---|
| **S1** — the operational units that do the work | **Actors executing Steps.** Humans and registered agents are the CPUs; nothing moves without one claiming a step and doing it | the claim door; `agents` registry |
| **S2** — coordination, damping oscillation between units | **Stations, and the bounds enforced at the claim.** A station is a data-defined priority queue that holds a packet until there is bandwidth or capability — which is S2's job stated in the network's own words | station rows; `agent_budget.rs` |
| **S3** — internal control, resource allocation, here-and-now | **The dispatcher, plus the registry rows that bound it.** Rules fire on `step.done.<kind>` and threshold events; the numbers they enforce are registry data, not code | `infra/dispatcher/rules/`; `agents.max_concurrent_runs` |
| **S3\*** — the audit channel that bypasses the line | **The audit log and sensor readings.** The log holds what *did* happen; readings are measurements *about* the system rather than work it performed | `audit_log`; `agent_runs`; sensors |

**S4 and S5 are deliberately unmapped.** Designs and the backlog do
S4-ish work and `boss-policy` does S5-ish work, but neither
correspondence is tight enough to assert, and a mapping asserted
loosely is the decoration this section exists to prevent.

**`boss-cybernetics` is retired as SUPERSEDED-BY, not deleted as
dead** (2026-09-23, backlog 467175e7, design 8382bbb2). Its design was
right and was overtaken: every responsibility it stated now has an
owner elsewhere, and the reason it goes is **coherence and one owner
per rule** — two implementations of one budget gate, one dispatch door
and one queue are two places for the same rule to drift. It is **not**
a line-count win, and recording it as one would misrepresent why it
went. What superseded what, so the record answers the question
directly:

| `boss-cybernetics` responsibility | superseded by |
|---|---|
| budget caps before dispatch | `boss-jobs/src/agent_budget.rs` — the claim door reserves the step's budget against the actor's hour, before the CAS |
| one-at-a-time dispatch | `boss dispatch` — one `agent-run` packet per step, its `building` claim bounded by a slot of its `robots:<model>` pool (the per-actor `agents.max_concurrent_runs` cap retired, backlog 3e927996) |
| lifecycle telemetry | `agent_runs` plus the run packet's terminals |
| chaining on completion | the `agent-run` workflow's terminals and the dispatcher rules firing off them |
| the per-agent durable inbox | a **station** (923b6571) — since design 6e42c6de the executor's own tree station and its subtree, filtered by the role, model and assignment it may claim — pulled through the same claim door by `boss dispatch --next` |

The inbox was **the real gap, not a translation**, and the ordering
this decision set was held: the crate did not leave the tree until the
station inbox (923b6571), this mapping (6872efd4) and the reclaim of
work a dead executor had claimed (a3397b01) had all landed, because
retiring the old mechanism before the new one is live is how a
capability is lost by accident. The inbox's caller is the operator's
session: David decided on 2026-09-23 that running `boss dispatch
--next` by hand counts as the runner having run (the session is the
CPU, 57c108c2), so no headless runner stands in for the old loop. Its
config (`infra/cybernetics/`) and its systemd unit left with it. The
direction has since moved: design `506e0a1c` (2026-11-09) seats each
executor at stations and moves Codex and Claude B onto a supervised pull
loop once `boss station pull` exists (§The network substrate).

**`boss-observability` is retired as SUPERSEDED-BY, not deleted as
dead** (2026-09-23, backlog 467175e7, design 8382bbb2 — the second car,
which closed the item). It was the read side of the same design: a
cross-VM rollup of each `boss-cybernetics` coordinator, fanned out to
browsers. The reason is the same one, **coherence and one owner per
rule** — a second, parallel read of agent state beside the one the
claim door writes is two answers to one question. It is also
**not** a line-count win. What superseded what:

| `boss-observability` responsibility | superseded by |
|---|---|
| the cross-VM view (`/api/snapshot`, `/api/vms/*`) | a region of the IT world map (5082a08b) — the map reads the system's own regions (`/api/yard/regions`) rather than a per-VM rollup |
| per-agent runs, queues and costs (`/api/agents`, `/api/runs`, `/api/costs`) | `/api/agent-runs` and `/api/agent-runs/cost` on the jobs API, the record the claim door writes |
| its health alias (`/api/observability/health`) | each service's own `/api/<service>/health` |
| the `cybernetics.>` SSE fan-out | nothing, deliberately: its only publisher was `boss-cybernetics`, retired above |
| the playground's synthetic agents (`[demo_agents]`, the brewery's `seeds/demo_agents.toml`) | nothing, deliberately: they existed so a dashboard could show what oversight looks like before real agents ran, and real agents now run and are recorded |

Removed with it: the `observability` row (7880) in `boss-ports`, and so
its line in the container launcher and its block in the config
generator; the gateway's three routes and its proxy target; the
brewery's demo roster and its entry in the tenant contract; and its
bare-metal config and setup (`infra/observability/`).

**An unattended cadence will start work; until it lands, the session is
the only supply** (design `9e1de851`, David 2026-09-22; three questions
accepted as proposed, eligibility widened). Measured over 5.5 days and
132 trains: gate launches fell to zero for six hours every night, 07Z to
12Z, and five of the six longest gaps between trains ended between
13:55Z and 14:21Z — when the operator woke, not when a machine
recovered. Of 61 dispatcher rules, four manage an agent run's lifecycle
and none starts one; all 125 runs on record were filed by an operator
session. A skipped car becomes a total stop only when it is the last one
on the dock, which is the overnight condition, so the seven-hour stall of
2026-09-22 was downstream of supply rather than a separate fault.
Decided: a cadence rule dispatching ready steps (1) **up to a
concurrency starting at three**, half the observed session peak, raised
on a reading; (2) **until a per-night USD ceiling declared in the rule
row**, where it stops and files one packet saying so — never a silent
stop, which would look exactly like the failure it fixes; (3) **from any
queue the actor has policy to work** (David: "Any queue that the actor
has policy to work should be available to be worked overnight too");
(4) **with the bound read from the agents registry and enforced server
side**, never a second copy in the rule row (§9a). The claim door
judges a run's `building` against its `robots:<model>` pool (the
per-actor `agents.max_concurrent_runs` cap of backlog `57c108c2`
retired under 3e927996); the cadence itself is not built, which is why the paragraph above records
the session as the CPU.

*Algedonic* signals keep their Beer meaning throughout: rules firing
on threshold events, routed past the normal reporting line because
they are urgent.

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

**Identity has a home: the `subjects` table** (R1, approved
2026-07-15; live contract:
`docs/design/subject-identity-and-relationships.md`). One thin
`(kind, id, label)` row per subject — identity only, no attributes —
that every domain mint upserts in the same transaction as its domain
row AND the rebuilder reproduces from `*.created` events (the
dual write-through/projection contract, Q1; the deep replay-check
owns its correctness). The uniform existence gate is one indexed
lookup against it, for every kind including tenant-defined ones.
Kinds with no rebuild source are **rollover landmines**: any subject
minted only at prepare time vanishes when the epoch rollover
reprojects — the class bit three times (company → the `companies`
reference-table pass, birth-by-job kinds → the nested-payload jobs
pass, assets → the TOML identity sources), so a new kind lands with
its rebuild source or not at all.

**Relationships are one registry: `subject_edges`** (R2, shipped in
two passes 2026-07-17/29). A declared edge — `source_kind` event,
dotted `field_path`, target kind either pinned (`target_kind`) or
read from the payload (`target_kind_path`, the typed-pair shape:
job.subject's own subject_kind, the asset custody holder_kind) — is
enforced by the `check_subject_edges()` trigger on `event_outbox` +
`audit_log`, aborting inside the domain transaction (Q2:
abort-everywhere, no warn-mode legacy), and swept nightly by
conservation invariant Y. It superseded the three partial registries
(audit_log_ref_checks survives only for non-subject residuals like
part_sku). Absent refs skip — identity-first — and an id whose
dynamic kind half is absent skips with it; a kind-mismatched pair
aborts. Custody is subject-valued: the asset holder is a typed
(holder_kind, holder_id) pair (Q5), not an account id. Org-level
work is about the **company** Subject (Q6) — one row per tenant,
reproduced from the `companies` reference table. Jobs are owned by
humans, never automations (Q7) — steps may be automation-executed,
but the Job's owner resolves to a person (the owner-resolution
module's role-holder spread). Deploy-order rule for new abort
edges on a live system: backfill `subjects` first, then apply the
edge.

A **Class** is not a Subject: Classes are typed reference data
keyed `(subject_kind, code)` that each Subject kind owns — roles,
account types, asset models, departments all land in the one
`classes` table (living reference:
`docs/design/class-registry.md`). Parts are Subjects in their own
right; the Composite primitive is heterogeneous and laws-checked
via proptest at the trait boundary.

**A person who writes to us is a contact, found by the address they
wrote from** (design `5548d85f`, 2026-09-21; David answered two
questions and delegated two). Measured: `accounts` has no email column,
and `account_facts` is a projection that answers what happened to an
account, never which account an address is — so `receive-a-sponsorship`'s
rule "customer id or email already on an account: use that account"
describes a lookup the schema cannot perform, and every sponsorship so
far has landed on `acct-anonymous-sponsor`. A support inbox makes the
gap load-bearing: an address that finds nobody makes every thread an
orphan and the second email from a customer a stranger. Decided:
(1) **contact first**, which neither offered option was — the PERSON is
primary, handles attach to the contact (`(kind, value) -> contact_id`,
unique on `(kind, value)`), a contact MAY point at an organisation when
known rather than guessed, and `accounts` is untouched and becomes the
organisation half; "contact" because it names the relationship, the word
a CRM reader already knows (David: "contact works"); (2) **handles are
additive and never reassigned by a protocol** — anyone can put any
address in a From header, so letting inbound mail repoint a handle makes
takeover a matter of sending an email; a wrong handle is corrected by a
human act with a record; (3) the anonymous-counterparty rule is
**dissolved, not answered** — mail always carries an address, so a real
message gets a contact keyed on it; (4) **the tables land as their own
car, then `receive-a-sponsorship`'s reconciliation is repointed as its
own small car** right after, because a sponsor is a person who may
belong to an organisation and that repoint re-models a live revenue
path. Identification comes after triage, never at receipt, so spam
provisions nothing. Not yet built.

**The employee Class drawer stays one drawer, told apart by axis; only
the departments move out, onto the tenant's own roster** (design
`3dff7577`, David 2026-09-23, all three questions accepted as proposed;
it amends the four-kind split design `32f18167` signed off on
2026-09-19, carried by backlog `a45ab09d`). Measured live that day: the
drawer holds 22 `employee` Classes on four axes (role 7, department 9,
status 3, employment_type 3), and every row already names its axis in
`member_attribute` — the explicit axis the split was weighing. What was
broken was the readers, which asked only whether `(employee, code)`
existed, so `role=terminated` or `department=platform-admin` passed.
Decided: (1) **role, status and employment_type stay `employee`
Classes** told apart by `member_attribute` — four subject kinds would
register three with no Subjects behind them and contradict
roles-are-Classes-of-employees, and the drift the split was meant to
stop is refused by reading the axis instead; (2) **the department
roster is the tenant's org chart**: the 13 catalog-derived rows of the
`department` subject kind (migration `20260919181324`) become the
product default a tenant starts from, and a tenant adds and retires its
own — Algedonic's engineering, product, hosting and operations exist
today only in the drawer, with two live actors on them — which needs a
tenant write door and a publish path before the validators move to
department Subjects and the drawer's department Classes retire, moved
rather than copied, with the two actors reassigned through the API so
the log records it; (3) **a role Class's `metadata.department`
retires with them** — a role does not belong to a department, the
person holding it does. Landed: the axis readers — boss-people's
employee checks (`a45ab09d`), then the agents batch door, account-team
roles and `GET /api/classes?member_attribute=` (`ab1e6ff8`). Not built:
the department write door, the publish path and the move; the
`departments` table still has no write door. Algedonic's own roster
was answered on design `8c3e9599` (2026-09-25; §Step UX & frontend).
It has nine live rows, `engineering` folds into `it`, `people` stays
as a row, and the six physical-operations rows are retired on this
instance. That design also gives a department a second axis,
`practices`, beside its single `function`. **The move landed with
backlog `c87e3d6d`** (page audit `9f7ba57d`, decided 2026-09-27; the
departments write door and publish path had landed with `7edf0e97`):
an employee's `department` validates against the un-retired
`departments` rows (boss-people `departments`) and an agent's against
the same rows at its batch door; no seed declares a department Class;
the SPA's roster, employee page, HR headcount and policy scope picker
label departments from `GET /api/departments`; the platform's own `it`
is a `departments` row no example declares, so the migration's `it`
department Class is eviction residue like its ten siblings; and the
eviction keeps a department an employee, agent or requisition sits in.
The live rows move through the evented doors, not a migration: the
founder to `executive` (David 2026-09-27), the agent to `it`, then the
nine department Classes retired one by one. Point (3), the role
Classes' `metadata.department`, is still open (backlog `bac1d71d`).

**A department is a tier-one station; People is a future one and
Service is none** (design-doc "People stays a department; Service is not
one at Algedonic, LLC", `9e9f1037`, David 2026-09-27: "People is a
department. Service can be removed [for] Algedonic, LLC."). Applied that
day in the tenant repo (commit `90b437d` un-retires `people` in
`seeds/departments.toml`, `service` stays retired) and live through
`boss tenant publish --take departments`, with `/ux/service` answering
"not a department on this instance". Read in station-tree words, which
supersede the v1 department roster (`docs/design/station-tree.md` §2):
the tier-one stations under the root are engineering, executive, finance
and operations; People is one of the future producing departments the
tree places as their sibling when it has work of its own to queue, and
Service is not one at Algedonic, LLC.

**A department under review is a parameter, not a place work queues**
(design-doc "Department retros: clear the pile the name-nominations
left", `41b4eebe`, 2026-11-13; Q1 accepted as proposed). Measured that
day: 37 department-retros open, every ready step still named to
agent-claude by the dispatcher nominator that e1eaf72b retired, and 12
of them pinned to versions whose `opus-5[1m]` model class no station
holds. Decided: (1) no department-retro row change and no department
stations for retros — every retro step is the platform analyst's, the
department rides as `metadata.department`, and the role audience already
stands the step at `a.platform-admin.opus-5-5`, which `boss dispatch
--next` walks; in the station tree `department-retro` is a protocol of
the company root, so a retro never needs a tier-one station of its own;
(2) the v8–v12 names go back through `boss step unname --kind
department-retro --holder agent-claude` (and `--kind protocol-retro` for
the one `gaps` step), an audited ops act, no car; (3) Q1: run only W46
for each of the five departments and close the 32 older weeks (W39–W45)
as cancelled, naming this design; (4) a ready agent step that no station
queue lists is an alarm's to count (49b1742b), not a stall to discover.

**The People roster holds people** (design `7aa2d1c5`, David
2026-09-23; answers backlog `6a123f1f`, gap 12 of the `/ux/people`
page audit). Measured: the page's "active employees" headcount was 2 —
the founder, and `emp-audit`, the "System Audit Account" the operator
baseline (`boss-people/src/operator_baseline.rs`) seeds as an employee
row — while every registered agent, which claims steps and is
capability-checked at the claim, was absent. The proposal was to show
agents on the roster as their own kind with a split headcount; David
answered instead: "People should just be people. Let's actually move
the system-audit account to the same locale as agent and system
accounts." So the employee roster is humans only, agents stay in the
agents registry rather than joining the headcount, and `emp-audit`
leaves the employee table for wherever agents and system accounts are
held — an actor being a CPU in the same machine does not make it a
person on the org chart. Not built: the audit account is still seeded
as an employee.

**People and HR summaries need a supported producer** (design
`93789cf4`, recorded without open questions on 2026-10-11; answers
backlog `9a0e18d5`). The company frame and the item's existing fix
shape settle this: remove the certification quantities from their
shared header, Skills and Tenure numeric columns from People, and
average tenure and unsupported certification summary/urgent panels
from HR. Do not invent a workflow, metric or value to fill them.
Employee records and their detail data remain; existing role-based
headcount and department registry labels keep their settled meaning.
Resolve an employee's location through the locations registry's
`name`: distinguish unspecified, an unresolved recorded id, and a
failed registry read rather than guessing a label or painting failure
as absence. Both pages' tests must preserve those distinctions and
prevent removed quantities returning as zeros during loading or
failure. New certification, skills or tenure summaries require a
separately evidenced producer contract. Implementation remains open.

The design packet retains the measurement and decision provenance:
origin/main `920c8eeb` still rendered these summaries and raw location
ids; the roster read had two rows and no skills or certifications.
Hire dates existed and the operator baseline writes them, so the
historical claim that no field has any writer is not adopted. A
baseline date does not establish an employment-start protocol, and a
missing date converted to numeric zero is not measured tenure. This
fold records the display contract; it changes no UI, stored data,
policy, live service or credential.

The system is laid out on a **three-axis information
architecture**: *Knowledge Bases* (durable queryable state),
*Surfaces* (operator UI), and *Work* (Jobs + Steps that change
state). Every KB-exposing domain implements the shared `KB` trait
from `boss-core`; facts live in domain tables, not a global facts
table; aggregations rebuild on-demand + periodically.

**Other people's agents reach BOSS through the primitives, as
themselves** (public-api-mcp, five questions decided by David
2026-08-14 from feedback `78207000`; folded from
`docs/design/public-api-mcp.md`, deleted 2026-11-18, backlog 924de432).
An outside agent gets no side door. It gets an actor identity, the same
alphabet of legal transitions and the same provenance obligations as a
person or the simulated workforce, which already runs the whole company
through the public API. MCP is a transport for that, not an exemption
from it. The shape is three layers, each useful without the next: a
published reference for the generic layer, pinned to the router by an
equality test; a Tier-1 `boss-mcp` adapter in front of the gateway,
every tool call an ordinary HTTP call as the acting identity, so policy,
validation and audit apply unchanged; and agent identity. Decided:
**the tools are the primitives plus per-Workflow specializations**
generated from the registries (Q1); **an agent acts as itself and a
human only as the human** — an agent may carry a human's authorization,
but the record names the agent as the actor (Q2); **the playground
endpoint is public, behind a guest token obtained through a captcha**
(Q3); **the in-repo reference is canonical**, with the KB page and the
tool descriptions derived from it (Q4); and **a published tool contract
follows the schema's expand/contract convention**, additive freely,
destructive in two steps, roll-forward only (Q5). Not built: no
`boss-mcp` crate or `/mcp` route exists on 2026-11-18.

## Jobs, Workflows, Steps

A **Job** is a bounded unit of coordinated work: stable identity,
owner, subject, status, and a structured list of Steps. The
**Workflow registry** is append-only and versioned; in-flight Jobs
pin to the version they opened under unless an actor explicitly moves
them (the re-pin door, below); creation is blocked against
`draft` and `retired` kinds. Adding a new workflow means adding a
Workflow row — never a `match` branch in core code.

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

**A closed record on another packet is checked by the actor that
relies on it, not by `ready_when`** (design-doc *Make closed native
evidence a Workflow readiness condition*, `69c0ace7`, David
2026-10-04, its one question accepted as proposed). The design asked
for a registry-declared external-evidence condition: a jobs-adapter
verifier that reads a referenced native run at the transition,
server-stamps an event-backed receipt that alone opens readiness, and
demotes the step when a bound fact moves. Not built, and no protocol
now needs it. `build_context` still hands `ready_when` only the
subject, the job's metadata and the packet's own steps, and the one
use the design named, the tenant merge's independent review (design
`9de0cd1c`), was built on ONE packet instead (backlog `84e7c3e4`,
2026-10-09): ops-request carries a `review` step wherever the verb's
file declares `requires_review` (today `merge-tenant-main` alone),
`approve` is ready over that step's `release`, and the ops runner,
not the predicate, holds the cross-packet facts. It renders the plan
onto `approve` only for a landed `tenant-reviewer` run that is not
the source run and a plan that still hashes to the reviewed bytes,
and checks both again before the claim. The plan keys on both steps
declare `writer = "runner:ops"`, resolvable since 2026-10-10 under
row E of design `b08725c2`, the separate credential decision Q1
asked for. The branch that began this design's core change
(`fix/closed-native-evidence-gates-tenant-approval`) was never pushed.
A protocol that later needs a closed cross-packet record before READY
takes this decision's shape; until one is measured, no item is filed.

**Sub-Jobs are a typed contract** (`delegate-subjob` — the one
spelling): the parent step's completion *is* the child Job's
close, parents must handle every possible child outcome, and the
dispatcher performs the close → resolve write-back. Required
metadata is checked **at done, not at create**;
`PUT /api/jobs/{id}/steps/{step_id}` has PATCH semantics
(top-level fields replace wholesale; clients merge metadata keys).
The Jobs list takes exactly one subject filter — `?subject_id=` —
and the Job's subject column is `subject_id`.

**The step PUT will refuse a metadata body that drops a stored key; it
will not refuse every metadata body** (design `baf738b7`, David
2026-09-23, accepted as proposed; answers backlog `e39a9d2a`). The item
asked the PUT to refuse any `metadata` and send every caller to the
merge door (`PATCH …/steps/{id}/metadata`). Measured before any code:
about 100 writers send metadata with the status flip, because the
checks that run at done read it, so that rule was five cars and made
completion two writes that are not atomic. Decided: **refuse only a
body that omits a key the stored step holds** — clearing by omission is
the actual data loss, and a stale read that would drop a concurrently
added key is caught with it — route that caller to the merge door with
an explicit `null`, and delete the three keys `update_step` carries by
hand (`authority_role`, `human_only`, `agent_run`); the
refuse-every-body plan stays unbuilt unless a two-write completion is
wanted for its own sake. **The premise under the one-car estimate was
false, and the rule stands with a new order** (correction recorded on
`e39a9d2a` the same afternoon): the registry materializes
`metadata_defaults` and the audience keys into every step, so ANY PUT
built without a prior read omits stored keys — the gate runner's
verdict report and `boss car open` / park among them — and landing the
refusal first would have stopped every gate and every park. The order
is now (1) `boss hold` / `boss release` through the merge door —
landed (#586); (2..n) every fresh-metadata writer moved to
read-merge-write or the merge door, the gate runner and `car.rs` first,
each converged on its host before the next; (last) the server refusal,
which closes the item. Until it lands, the PATCH semantics above hold,
wipe of unmentioned keys included.

**And then the step PUT refused every metadata body after all** (design
`93d2bddb`, decided_2026_09_24b on `e39a9d2a`: the same end state, reached
in two stages). Stage 1 shipped the omission refusal above; Stage 2
moved every read-merge-write writer — boss-cli, the dispatcher handlers,
boss-sim, the tenant walk, the web surfaces, the step plugins, the host
scripts, and last the gateway's break-glass spend and passkey promotion —
to the merge door, and its last car made `update_step` refuse ANY body
carrying `metadata`, 409, naming that step's merge door. A read-merge-
write was correct under Stage 1 but still a second metadata writer that
raced a concurrent key; with one writer the class is gone rather than
guarded, and the four metadata guards the PUT carried (the
`authority_role` carry, the `human_only`, protocol-key and
declared-writer refusals) now stand only at the merge door. A completion
is two writes, the keys then the status; the break-glass spend, the one
writer that had been one PUT for atomicity (review F2), makes a refused
close fatal and withdraws the spend it followed.

**First step evidence has an opt-in immutable recording door**
(design `3bb90d7b`, generic source car `b2a4c481`, merged as
`d1d242927172`). Ordinary metadata PATCH still replaces supplied keys
and deletes a key supplied as null. A first observation instead uses
`POST /api/jobs/{job}/steps/{step}/metadata/records`, with exactly a
nonblank bounded `key`, a `value`, and `expected_absence: true`. Null
is a present recorded value. An ordinary key already present cannot be
adopted or overwritten through this door.

The first successful insertion stores the full value and its original
server receipt: actor, timestamp, Job/Step/key, event identity,
canonical representation and digest. Object-key order does not change
equality; array order and scalar representation do. Equal replay returns
the original receipt and emits no new event; a different candidate
refuses with a conflict. The checked HTTP call verifies the judged step
version before insertion **or replay**, so a changed declaration requires
a fresh read and retry. A terminal step permits equal replay of an
existing record, but refuses absent new evidence and conflicting replay.

The record and completion share the adapter's serialization boundary.
Postgres locks the step row and commits metadata, receipt and outbox
events together; the in-memory adapter acquires state and event locks
before mutation. Recorded keys remain immutable through ordinary writes
and workflow reprojection. Rebuild checks the receipt against the full
step value and immutable event's actor, timestamp and identity, and
refuses a contradictory receipt rather than replacing it.

The door retains existing policy, row scope, authored-field, declared
writer and human admission checks; it grants no authority. Policy denial
or outage precedes Jobs repository access. Other authorization paths read
the versioned step at most once and its Job at most once; human admission
may separately read the people roster. Missing or incorrectly bound steps
and missing or narrowed Jobs receive the same protected not-found
response. Rejected recording requests change no Job or Step state and
emit no recording event. This is a bounded-read contract, not a
constant-time or universal pre-read
identity guarantee, and ordinary step-write refusal telemetry is unchanged.

This primitive records evidence; it does not complete the downstream
conditional publication lifecycle, establish provider attestation, make
Secret and Job writes one distributed transaction, grant installation
permissions, or prove a public publication succeeded. Those obligations
remain with their owning packets.

**A workflow-publish step cannot yet be completed conditionally.**
Design-doc *Commit conditional workflow publication without publishing
past a lost claim*
([bbaa0cac](https://boss.algedonic.dev/ux/jobs/bbaa0cac-f573-43bd-9775-a62db426be8d),
2026-10-03, no questions) holds the half of the conditional-completion
contract that `POST …/complete-if` does not meet. Each StepType declares
`conditional_completion`: `row-and-outbox`, the default, or
`inline-effect`, a kind whose completion acts outside the step row and
its outbox. `workflow-publish` is the one inline-effect kind, and the
conditional door refuses it, and any kind the registry does not know,
409 `unsupported_capability` before any evidence, status, event or
registry write. The reason is ordering: the status PUT still runs
`dispatch_workflow_publish`, whose registry transaction commits on its
own, before the step's version check (`update_step_if_unchanged_at`), so
a claim lost between them leaves a published version beside an unmoved
step, and publishing after completion would instead record a completion
whose effect failed. What holds today: a re-send of the same spec from
the same authoring packet is answered with the row it already wrote, not
another version (backlogs `558396ff`, `4bdb8150`, `50ded63d`), and
`publish_authored_if_absent` makes absent-only publication one atomic
adapter operation. Not built: one committing boundary for the claim
judgement, the publication and the completion (a shared transaction, or
an intent/effect protocol whose consumer records the publication before
the completion), with races, exact replay, retained failure causes and
rebuild proven on both adapters. That build is owed on backlog item
`0071b36f`, which stays open until it lands.

**A Job carries the instant it was admitted, not only the day**
(design `f2cdff23`, David 2026-09-20, all three questions accepted as
proposed; landed as backlog `6c2eba00`). `opened_on` is a date, and
every surface that asks how long something has waited — the
ops-runner's `oldest_wait_s`, dock wait and gate duration on the
region map, the overdue alarms, the silence sweep that settles a dead
agent run — needs the instant, which was `metadata.opened_at`: a
convention the doors followed and a raw `POST` did not, and a missing
stamp read as decades old rather than unknown. Decided: (1)
**`opened_at` is a column**, server-stamped at admission and kept out
of every UPDATE the way `partition` is; (2) **the back-fill is a
projection of the log** — each packet's own `jobs.job.created` event,
the derivation the rebuilder also applies — and a packet whose create
event the log does not hold keeps `NULL`, because an instant nobody
observed is worse than none; (3) **admission grows no create-time
check** for declared-required fields as part of this — validators
still run at done, and a server-stamped field needs none. The metadata
stamp is still written for the readers already on it.

**A packet stays on its admission version unless an actor moves it, and
the move is on the record** (design `7cf202a9`, David 2026-09-23, all
five questions accepted as proposed; answers backlog `4347a1af`). The
item said the re-pin machinery had no door; measured, it had one —
`POST /api/jobs/{id}/convert`, landed in train #150 on 2026-08-30,
gated on the ordinary job-write permission, answering 409 with the
obstacles when `convertibility_for_packet` is not automatic and
otherwise updating `jobs.workflow_version` under a plain `job.updated`.
No CLI verb calls it, no test covers it, and no open packet carries a
re-pin's signature. It also moves less than it reports: a step's
`procedure` is projected into the step row at materialisation, so
converting page-audit `c0d2caf0` from v1 to v3 would answer
`converted: true` while its `measure` and `file` steps kept v1's text —
the only thing that changed between those versions — and a target that
inserts a step (backlog-item v1 → v7 adds `draft-design`) would walk the
packet to a step no row holds. 112 of 296 open packets were pinned
behind their kind's active version that evening. Decided: (1) **the
guarantee, restated so the code can hold it** — a packet stays on its
admission version unless an actor explicitly moves it; a move is
refused when it would retroactively demand evidence or strand a step;
the move is on the record — with `/convert` the one door and a CLI twin,
`boss job convert <packet> [--to vN] [--dry-run]`, whose dry run
returns the verdict without writing; (2) **a re-pin re-projects the
target's step defaults, procedure included, onto every step not yet
completed, materialises any step the target inserts, and leaves
completed steps with the text they ran under** — and until that lands
the door refuses any move whose target changes a pending step's
procedure or inserts a step; (3) **a `job.repinned` event** (from, to,
actor, steps re-projected, steps inserted) plus a `repins` list appended
to the packet's metadata, so admitted-at-v3 and moved-to-v3 read
differently without diffing `job.updated` payloads; (4) **authority
narrows to `platform-admin`**, the role that owns the Workflow registry,
from any job writer; (5) **never automatic, never on publish** — a
cohort move (the 47 page-audits on v1) is the per-packet door run in a
loop by an operator after a dry run, each packet its own event,
revisited once moves have been counted. Built in two cars: the first
refused every move the one-column door could not carry (1e973965); the
second (backlog `4347a1af`) is the door as decided — `boss job convert`
and its `GET` dry run, the plan in `boss_jobs::repin` written in one
transaction, the `jobs.job.repinned` marker beside the row state, and
`repins` reserved against the metadata PATCH and carried through the
job PUT the way `corrections` is. Q4's "platform-admin" is held as
policy data rather than a role name: the caller needs `publish` on
`workflow` — the permission that makes a protocol version live, which
the core defaults give platform-admin alone — as well as the job write.
A projected key the executor has since written is not overwritten; the
record lists it as `kept`.

The brewery's `wholesale-keg-order` is the worked example of
agent-gated fulfillment: an `availability-gate` reads finished-goods
for the order's lines and forks fulfill|backorder — an order the cooler
can't cover exits to a terminal `backordered` outcome instead of
marching through pick → ship → bill against stock that isn't there —
then a human `pull-and-stage` pick precedes delivery and billing. The
gate is the release valve that bounds open WIP the way human
stock-judgment does in a real brewery; finished-goods / COGS draw on the
billing line items.

**Support's `receive-a-message` is the first protocol of a department
that is not IT** (design `bffc0aba`, David 2026-09-21, all four
questions accepted as proposed; the first of the six first protocols in
the order `3613f0af` set). Measured that day: 116 of 126 open
backlog-items named no department, and about five were genuinely non-IT.
The loop adds two ends to standing machinery — the sensor cadence, one
packet per reading, `sensor_unreadable` filing an urgent deduped packet
for a source that refuses — namely a mail source adapter idempotent by
`Message-ID` and the protocol row. The mailbox is a dedicated Agent B
account on Proton Mail Business, `support@algedonic.dev` routed to it
with no forward to a human, and Proton has no plain IMAP, so inbound
needs Proton Bridge. Decided: (1) **Bridge runs on boss-gcp**, installed
by its converge with the keyring login a one-time human act — and before
building, establish whether a logged-out Bridge can answer politely with
an EMPTY mailbox; if it can, the adapter asserts reachability separately
from message count, or a dead daemon reads as a quiet inbox; (2) **the
body lives on the packet**, and the inbox message carries a subject and
a pointer until replies land; (3) v1's terminal was **answered in BOSS**,
superseded the same day by the send loop below; (4) **a spam terminal
reached by a disposition at the first step**, and no automatic
classifier on day one, because a wrong auto-close loses a real customer.
Landed: a sensor may carry a `selector` naming one stream within its
source, so two inboxes on one mailbox open two protocols
(`20260921034443-a-sensor-selects-within-its-source.sql`). The adapter,
the Bridge host and the protocol row are not built; the tenant's
`support` module is on (design `1054c099`).

**Agent B sends nothing without the founder's passkey on the words**
(design `3a89774d`, David 2026-09-21: "I do not want any outbound email
sent by this protocol, or more broadly Agent B, without my sign off by
pressing a UI button on a gating step"). No new control is needed:
`assurance_required = "presence"` on a `sign-off` is enforced with no
bypass, and the stamp is re-checked against the step's CURRENT shape, so
an edit after approving voids the approval. The protocol is `received`
→ `triage` (real or spam) → `draft` (agent) → `approve` (sign-off,
presence; the founder reads and edits HERE, so what he signs is what he
ended with, and there is no revise loop) → `sent` (agent; apart from
`approve` so a failed send never reads as a withheld approval) →
`answered` | `spam`. Decided: (1) **the transport does not refuse a
send on its own** — the requirement is scoped to this protocol and the
ones Agent B will help run, the way the conductor runs the yard, so a
code path that never enters a step is not closed by it, knowingly;
(2) **presence on every outbound** for this protocol ("passkey entry is
as fast as touching my fingerprint"); (3) **the agent drafts**, fetching
from the record to support the draft rather than writing words alone;
(4) **a failed send leaves `sent` open and loud**, the sensor's shape,
and the approval stays valid because the words did not change — the
packet must never reach `answered` when nothing left the building. Not
yet built; the send needs an SMTP submission transport behind
`MailTransport`, beside the log and HTTP-API ones.

**The support row writes down what bffc0aba and 3a89774d decided, and the
drafting agent reads a bounded set.** Design `770c352c` drafts the
`receive-a-message` Workflow row (seven steps: received by the
support-inbox sensor, triage by the founder as real, spam or duplicate,
draft by the agent, approve as a presence-backed sign-off on the exact
words, sent by the transport exactly as approved, answered or spam),
adding no decision those two designs had not made. Its one question, a
data boundary, was accepted on 2026-11-02: to support a reply the drafting
agent may read **the packet and its thread, the correspondent's own
account and packets when the sender address matches a known account, the
manual and the published catalog**, and NEVER another correspondent's
packets, other accounts, credentials or unpublished prices; the draft's
`sources` object names every read, so the approver can see the boundary
was kept. The row lands after the mail adapter (492a61ae) and the Bridge
host car (67e08c6a).

**Workflows bootstrap through Jobs.** The system-owned
`workflow-design` kind authors new Workflows inside a Job (draft
edits live in the authoring Job; the terminal `workflow-publish`
step writes the registry row), so the platform's own catalog is
published with full audit provenance — the system models its own
development. Platform kinds ship in code (`platform_kinds()`);
tenant kinds load from `examples/<tenant>/seeds/workflows.toml`
(governance rule: `docs/design/platform-vs-tenant-jobkinds.md`).

**Authoring is graphical and author-gated.** The `workflow-design`
surface is an interactive trigger→outcome canvas (Svelte Flow + dagre,
code-split onto the editor route): steps are nodes (trigger / terminal
/ fork / work), an edge A→B *is* `steps.A.done` in B's `ready_when`, and
a structured predicate builder emits the boss-expr behind a
live-validated raw "advanced" escape hatch. A non-persisting dry-run
(`POST /api/workflows/_validate`) runs the publish-path lint against
the same in-process `StepRegistry::v1()`, so editor-green publishes by
construction; the SPA persists drafts as `metadata.workflow_spec`
PATCHes on the design Job and never calls the direct `/api/workflows`
create/update/publish handlers (kept only for bootstrap + tests). The
design **approve** step requires a `workflow-approver` capability —
authoring a work-type is operational leadership's call, not the deploy
operator's alone (core policy grants it to `platform-admin`; tenants
grant it to their leaders; `design-doc-review` stays `platform-admin`).

**What an actor holds and is owed is answered by one server reader.**
Design `7526f859` (2026-11-01, no questions; extends 347d14ee) replaces six
independent computations (the idle-actor watch, the urgent-item watch, the
urgent-first door, `boss orient` MY WORK, the top board with
`outranks::claim_run`, and `boss step release`), which had collected
eleven filed defects between them. `boss_jobs::holdings::for_actor` returns
every step the actor holds or is owed, typed: how it is held (claimed with
an open, gate-waiting or absent run; nominated; via a role or a station),
what carries it (a car in flight, held, landed awaiting proof, partial, or
none) and what it awaits (gate, a person, a packet, capacity, nothing).
Outcome steps and human-act steps are never lines. **Waiting is a fact**:
`waiting_on {kind, id, since}` is a reserved step key written by the door
that made the step wait, so a watch never re-alarms a line that awaits a
person or a packet; it alarms when the awaited thing resolved and the line
did not move. Every watch, orient, the board and outranks read the reader;
`boss step release` refuses a line whose claim has an open run, naming it,
and completes only on a read-back with no holder. A pin names every file
that computes a holding without the reader. Under the station tree, the
capacity case of `awaits` is the named constraint that station-spec
resolution records (4b27a65a, T6).

**Every machine-owned metadata key has one registered writer, and every
door enforces it.** Design `7fc4fb2e` (2026-10-31, no questions) makes the
reserved keys a registry in boss-jobs, each key with its ONE writer and
shape (`release`, `hold`, `corrections`, `repins`, `priority_changes`,
`opened_by`, `outcome`, `authoring_job_id`, `selected_by` as a bounded
ring, a step's `title`, the gate attempt, and the `waiting_on` key of
7526f859, among others). Job POST and PUT, step PUT, the merge door, the
first-record door, the conditional door and the workflow POST all run the
same check: a reserved key from a caller that is not its writer is refused
409 naming the writer; a job PUT never carries a reserved key forward and a
create cannot carry one in. The merge door and the job PUT take the
version the caller read (`If-Match`), and a write against a moved version
is refused with the current one. A declared field's type is judged when it
is written, not only at completion. A person's `hold` and the gate's
`gate_hold` are two keys, and the release rule lifts only `gate_hold`.
Every car is a mutating-door change and parks held for a distinct review.

**A packet keeps a reserve under the event bound, and keeps its
originals by verified reference** (design `6ab85352`, *Keep every Job
writable under the event bound*, 2026-11-13, its three questions
accepted as proposed; answers backlog `4f8783fb`). Every Job or Step
write stages one state event carrying the whole row, and the outbox
refuses an event over `MAX_STAGED_EVENT_BYTES` (983,040: NATS's 1 MiB
less a 64 KiB margin), so a write costs the size of the packet, not of
the change, and a packet that reaches the bound takes no write at all —
no annotation, no step move, no close. Measured over all 1,551 open Jobs
that day, two were over, in the two shapes that matter: backlog-item
`6cd6c935`, a few large evidence values, and work-session `044b189b`,
762 small keys, whose silence alarm refiled every fifteen minutes
because it could not close. Decided: (1) **the bound stays and a packet
gets a budget below it**, judged on the encoded event about to be staged
with the outbox's own constant — at half the bound a write lands and
says so; at three quarters a write that grows `metadata` is refused 413,
naming the size, the budget, the bound and the repair door; a write that
holds or shrinks the row (a status move, a step transition, a close) is
never refused by the budget, so the 25% reserve guarantees closure by
construction; (2) **one reference shape, owned by `boss-core`**,
`{"$evidence": {file_id, sha256, size_bytes, mime, encoding}}`, the hash
over the exact stored bytes, while the six hand spellings found on about
40 packets stay readable and are not rewritten; (3) **externalise at the
writer, never silently at the server** — a door moves any single
undeclared value over 64 KiB to the file store, reads it back and
verifies the hash, and only then writes the reference, so the jobs
service gains no file-store dependency, and a declared or
required-at-done field is never moved; (4) **evidence bytes are pinned
forever** (Q1): a referenced `file_refs` row is skipped by the 30-day GC
sweep and its detach is refused, because a hash naming bytes a sweep may
delete is a promise that expires; (5) **a recorded repair door, `boss
job externalise <packet> [--dry-run]`**, open to any actor with update
authority on the Job, agents included (Q2), because it deletes nothing:
per key, largest first, until the row is under half, it uploads,
verifies, and emits one `jobs.job.evidence_externalised` event listing
every moved key beside the shrunken row, which replay applies as it
applies `jobs.job.updated`, and the originals stay byte-for-byte in the
earlier events and in the store; (6) a `packet-size` sensor reading
counts open and closed Jobs per band and files one deduped alarm per Job
over half; (7) `jobs.job.updated` stays a full row rather than a delta,
because the budget bounds every state event without changing the replay
contract every projection reads. Order (Q3): the budget, the repair door
and its two specimen runs go ahead of other standard work. Built: the
budget (`boss_jobs::packet_budget`, its warning a `boss-size-warning`
header because the merge doors answer 204, pinned by
`a_packet_keeps_a_reserve_under_its_event_bound`), and the shape with
its writer and reader helpers behind the `EvidenceStore` port
(`boss_core::evidence`). Not built: the CLI adapter and
`dispatch_brief.rs` moving onto the helper, the `files.ref.pinned` fact
and its GC skip, the repair door (every budget refusal already names
it), the sensor, and the two specimen runs.

**A step may name a list of people as its audience.** Design `a00427ab`
(backlog 4c550a4a): the ops runner can assign a rendered approval only when
its verb names exactly one approver, and all 31 shipped approval verbs
named `emp-david` alone. The proposal was to lint one approver per verb
until a second person joins; David decided otherwise on 2026-11-05: a
**fourth audience variant** beside individual, role and station, a list of
employee ids. The step shows in every named person's queue, is held by
none of them, and any one valid passkey signature satisfies it. Quorum is
not new: a verb wanting two of three declares its approvers plus the
existing `sign_offs_required`, and the step completes on the Nth
signature. Named ids, not a role, because a role is registry data a policy
writer can widen while a named list is a tree change (his reasoning of
2026-09-22). The approve step's passkey signature stays the only approval.

**Platform protocols are data: a bundle in the tree, seeded insert-only**
(protocols-as-data, Q1–Q5 resolved 2026-08-16 and confirmed 2026-08-18;
folded from `docs/design/protocols-as-data.md`, deleted 2026-11-18,
backlog 924de432). The leak it closed was measured: twelve platform
workflows were Rust literals in `platform_workflows()`, and
`bootstrap_reconcile` republished the code default over any
bootstrap-owned row whose steps differed, so two operator edits were
silently reverted on boot (`68331085`). They are now TOML under
`infra/platform/workflows/`, loaded through `POST /api/workflows` by a
seed binary like every tenant bundle. **Q1: the seed inserts what is
missing and touches nothing that exists** (409 on a duplicate means
"already there, skip"). Drift-healing is removed on purpose, because it
was the feature that reverted operator edits; the honest replacement
for "every deployment runs the same platform protocols" is a visible
report of kinds whose active version differs from the bundle, never a
rewrite. **Q2: the viability lint runs at every publish**, including
the bundle loader's, and a test parses the bundle itself and lints it,
replacing the compile-time `every_shipped_platform_seed_is_viable`.
**Q3: a registry whose rows are flat seeds by migration; a registry
whose rows are documents seeds by bundle**, which is why workflows
differ from the other registries and is not drift. **Q4: a conversion
changes nothing in flight**: moving a kind from code to bundle must
reproduce the live row byte for byte at its current version rather
than publish a new one, and `crates/core/boss-jobs/tests/platform_bundle.rs`
holds it. **Q5: no tenant noun lives in a platform bundle.** `sale` and
`morning-brew` were already brewery-seeded when this was checked; David,
2026-08-16: *"We don't want brewery nouns in core no matter what."*
Tenant nouns in core is a layer question, while hardcoded nouns is a
form question, and the bundle answers only the second.

**What triage may decide is protocol and policy data, not a design**
(feedback-triage, decided by David 2026-08-18; folded from
`docs/design/feedback-triage-agent.md`, deleted 2026-11-18). The
question was whether feedback triage needs a language model or rules,
and whether an agent may close an item. Across 135 triaged real items,
2 were mechanical (duplicate or decline) and the rest turned on
reading the repository, so no rule-based triage engine was built. David
answered both questions the same way: *"This is matter of protocol and
policy, which is expressed in data and so should not affect any
designs"*, and *"The protocol will allow what the protocol allows."*
Who may complete a triage step is its `authority_role`, and who it is
handed to is the step's audience. Both are registry rows, so the choice
is changed by publishing a version, not by building an agent. What a
triage pass finds is recorded on the step that produced it, in the
optional `finding` field the `triage` and `investigate` steps declare,
with `finding_by` as provenance. A finding is evidence, not a decision:
writing one leaves the step open, and it stays visible after the item
is routed. Closing on evidence that the work was done, such as a merged
car naming the item, remains a separate power from closing on a
judgement that an item is worthless.

## The network substrate — packets, stations, routes

The reading frame is `docs/design/the-three-layers.md`: *"The network
is the substrate, the fat protocols dictate the current operating
model, the actors run it."* That doc is the one place the statement
is made; this section is what it resolves to in the running system.

**A Job is a packet.** The envelope is the Job row — identity,
subject, headers, protocol set; the payload is the accretion of
writes the log holds for that job_id. The projection rows mutate;
the log accretes. Two columns were asked to justify themselves and
both survived. **`owner_id` stays** as the accountable human of
record but stops pretending to be routing: stage 1 is that demotion,
stage 2 re-keys the `Self_`/`Team` policy scopes onto queue-derived
ownership and leaves `owner_id` a derived accountability lens ("who
owns the queue this sits in" plus "who has written to this packet").
The 2026-07-15 rule that a Job with no resolvable human owner is
refused is **relocated, not repealed** — the protocol names an
accountable requirement owner. **`Job.status` stays** as a
materialized cache of what `compute_job_status(steps)` derives,
recomputed on every step write; the manual status PUT dies except
for `released` and `cancelled`, the two imperative states that
resist derivation and become explicit packet writes.

**The protocol set is fixed at creation.** A packet declares a set
of compatible protocols composed exactly once, at admission —
requirements conjoin, obligations union, the viability lint runs
over the composed set — and the envelope never mutates after;
layering was rejected for v1. A packet needing different governance
mid-life is **translated**: a new packet under the new set, admitted
through the same edge, carrying a `translated_from` edge back and
leaving a `translated` terminal on the source. Translation is also
how a packet crosses fabrics (instances) — one mechanism, not two.

**Headers are declared data; undeclared metadata stays payload.**
`job_edges` is the shipped half: which metadata field of which Job
kind references another Job, enforced on the write path with
`subject_edges`' `on_missing` dial and prefix-aware resolution for
the folklore it inherited — seeded `warn` while the values were
dirty, turned to `abort` once the three real edges (`backlog_item`,
`train`, `boarded_jobs`) were cleaned and the machine writers
audited; `spec`, `branch` and `merge_ref` point outside the Job
graph and are deliberately out of scope. It generalizes into a
**header registry** carrying name, value shape, edge-ness
(resolution + `on_missing`), and which protocol reads the header;
`authority_role`'s triple duty gets named and split as it lands.

**A relationship between packets is a declared fact, not a trigger's
precondition** (design `c0d2787a`, David 2026-09-21, all five questions
accepted as proposed). Measured that afternoon: six packets filed in one
session recorded how they related four ways — `prerequisite_for`,
`prerequisite`, `prerequisite_of`, `related` — none declared in
`job_edges`, so none resolved, rendered in a Links panel, was ref-checked
or could be queried; meanwhile the one declared spelling, `answers`, was
REFUSED by `boss design --answers` because its target was a build
disposition with no `design-review` to complete. The system permitted the
weak spelling of the fact and forbade the strong one; the verb's guard was
doing model work. Decided: (1) **a small typed set of relation edges**,
declared like the others and grown on evidence, because the four
spellings meant different things; (2) **a decision edge records and says
what it causes** — "completes nothing" is printed, never a refusal,
since declining a true fact loses more than a side effect that does not
fire; (3) **stored once on the source, resolved both ways by readers** —
a reverse copy is §9a drift; (4) **a job id in an undeclared metadata
field is counted**, a warning first, as the measure of whether the habit
changed; (5) **declarations stay in migrations**, because an edge changes
what the write path refuses, which is schema. Landed: three
behaviourless `'*'` relations — `occasioned_by`, `duplicate_of`,
`supersedes` (`20260921162511-a-packet-relation-is-declared.sql`),
`on_missing = abort`; the proposed prerequisite relation is deliberately
absent, because `waiting_on` already is one and the dispatcher wakes its
waiter when the blocker closes — two of the four freeform spellings meant
exactly that, and would never have woken anyone. `boss design` records
the edge and prints what it will not cause, and `boss census` reports
undeclared job-id fields without raising.

**Stations are the network's nodes, and everything about one is
registry data** (living reference: `docs/design/station-tree.md`; the
v1 design, `stations.md`, is folded here and deleted). A
station is an abstract priority queue that routes or holds packet
traffic until there is bandwidth or capability to handle it —
**queuing separated from dispatching**: a station holds and orders,
the router moves. The registry lives in `boss-jobs` beside the
workflow registry, same append-only versioned posture, one active
row per name. Four kinds share one row shape, in two families. The
GENERIC station, sorted by who serves it: **actor** (every executor
has one), **group** (departments and teams), and **constraint**
(membership by capability predicate, not an enumerated roster). And
**equipment** (factory taxonomy M2, item 0a78f8f8, 2026-11-03): a
station that is shared process equipment, naming the automaton that
operates it and its **bays** — required there and refused on every
other kind. The dock is equipment (the conductor, one bay) and so are
the gates (the gate runner), whose bays ARE the gate capacity — the
delivery policy's `gate_max_concurrent` was dropped for them. The
kind equipment replaced, **batch**, was deleted: a batch station is an
equipment station whose automaton's program is a cadence (Q1, decided
2026-11-02). **Membership is derived and motion is evented**: the row
carries a predicate evaluated over open packets at read time, so no
mutable current-station field exists to drift from `steps`, and the
map and flow metrics read motion off each step's own ready and
completion stamps without one (§Station membership is read, not
replayed). Per-actor stations need no per-actor rows —
the predicate carries a literal `"@me"` that the evaluator binds to
the requesting actor once, before any packet is compared, and both
failure modes fail closed (an unbindable placeholder answers with an
empty queue; an unbound one matches nothing, so it can never hand
one packet to everybody). **Ordering is data**: a `discipline` array
on the row, default `priority, then age`, ties broken on job id, and
shown in the lens header so an operator never wonders why a queue is
in this order. **Capability gates at the claim** — checked against
the station the claim *names*, before the compare-and-set decides
anything — and **`wip_limit` is advisory first**: a lens warning and
telemetry, enforcing later only if the data says it matters.
`terminal_window_days` sits on the row rather than inside the
predicate because it is retention, not membership (and keeps
predicate evaluation clockless): a watchlist read by the person who
filed the packet is empty at exactly the moment it matters if
departed packets vanish at closure. Stations ship read-only and
barely seeded — two platform `batch` rows, no authoring API — so
"every executor has one" is the design, not today's data.

Decision history: the v1 taxonomy is design `4348b857`, decided by
David on 2026-11-02 (Q1: the dock became equipment with the conductor as
its automaton, and `batch` was retired; Q2: a person's capacity is a pool
whose capacity is a roster read, `people:<role>` rows re-derived when the
roster changes and never hand-set), superseded by the station tree,
design `4b27a65a`, 2026-11-06 (`docs/design/factory-taxonomy-v2.md`),
which keeps v1's census, diagnosis, executor kinds and two failure facts
and retires the equipment/generic split, bays as a kind and
automation-as-group. Q2 survives into v2 unchanged as the `people:<role>`
pool.

Six superseded design files were folded and deleted (backlog
`a884874a`, fold batch F2, 2026-11-17); their readers now cite
`docs/design/factory-taxonomy-v2.md` or `docs/design/station-tree.md`.
`factory-taxonomy.md` is v1 above, superseded by v2. `stations.md` (Q1–Q4
ratified 2026-08-13) is the registry paragraph above. `queue-visibility.md`
said of itself that `stations.md` superseded it; its five questions were
resolved by building them: My Day reads the assignments lens (Q1); the
claim door is a compare-and-set on Ready→Active, checking station
membership and capability first (Q2, car `a4c8f910`); queue depth is an
advisory `wip_limit` signal, never a block (Q4); `employee_skills` is to
be deleted rather than wired, and a skill would be a Class read by a
station's `capability` (Q3, 2026-08-18; not done: on origin/main
`b0aa242e2` boss-people still writes and rebuilds the table); assignment
strategy is rule data (Q5). Its
claim that a queue is never a reified structure is the open finding
below (§Open findings). `department-flow-dashboards.md` contributed one
decision, job links as data (Q1, 2026-08-09: the `job_edges` registry
above); its department dashboards are superseded by station views, a
department being a tier-one station. `networks-meet-at-boundaries.md`
was settled by the second tenant's retirement (packet `a8991c86`,
§OSS posture). `framing-convergence.md` was an audit plan written to be
worked through and deleted; its frame is `the-three-layers.md`.

**The doors read data, so the taxonomy is used by writing rows.** By
2026-11-05 the v1 taxonomy's mechanisms were on main (executor kinds, the
claim door judging a holder against a step's declared executor, robots
nominated by model class, the two failure facts, the vocabulary lint), and
almost none of the data they read existed: no robot carried a model class,
no step declared an executor, only the gates and the dock were declared
with capacity, pools named no station and stations declared no edges.
Design `ccfe87ff`, its three questions decided as proposed on 2026-11-05,
orders one data pass: model classes on the robots and `executor =
model:<class>` on **build and the two reviews** (the adversarial review and
the landed-review); `executor = equipment:<station>` on the gate-run's
`record-verdict` and the car's probe step; everything else stays
undeclared until a constraint is real. The converge is **three** stations
(deploy, probe shed, converge), each with its own capacity, because
different automata operate them and they hold different resources. A pool
names the station it serves, a station declares its edges, and departments
declare their stations, and these **are** the console's remaining
bindings: the console binds by reading them, with no further console car.

**A robot's model class is derived from its claimant's default model,
not declared beside it.** Design-doc *Five further collapses of
duplicated factory facts and packet handoffs* (`942dc0ac`, Q3, David
2026-11-08) found the model-to-class fact in two places: an agent row
declares `model_class` beside `default_model`, and each `robots:<class>`
pool lists its models, so claiming, nomination and billing could read
different paths to one class. Decided, for today's fixed-model robots:
the pool's models list is the one mapping, the agent's class is derived
from it by one pure resolver, and the separately authored class column
goes once every reader has moved. The robot pool draw resolves the
ACTUAL claimant's default model, never the step's requested model; the
resolved class is frozen on each resource claim; claimant-first billing
stays; an unknown or multiply mapped model refuses rather than falling
back. A future multi-model executor needs an observed model selection,
not a second editable class (item `d0315c8d`, not yet built).

In v2's words (4b27a65a): "equipment station" is a standing station with
machines, its "bays" are the station's capacity, and "a pool names its
station" is the station holding an allocation from its parent.

**An executor is seated at stations and pulls its next step; the
coordinator allocates and never assigns** (design `506e0a1c`, David
2026-11-09, its three questions decided as proposed). David asked
whether Claude B and Codex should work within a station, so the station
decides which of its steps each takes, rather than a coordinator session
choosing their work — which is v2's rule that an executor claims a step
at a station and a robot is pool capacity. Measured on main that day:
the pull exists, `POST /api/stations/{name}/claim-next`, an atomic claim
of the eligible head of a station's queue with a conserved selection
receipt, and the dispatcher re-offers a station's oldest waiting step
when a pool releases (`jobs-reoffer-waiting-on-release`); but nothing
called it, no executor was seated anywhere, the station queues had last
been generated on 2026-10-30, and a held car's review was handed out by
a coordinator rather than queued. Decided: (1) **a seat is data on the
executor's agents-registry row** — no new table, no per-step executor
key — and a station's who-pool is the set of executors seated there; an
executor may hold **several seats, in order**, and pulls from the first
with admissible work, so a reviewer serves landed-review first and build
second rather than idling when no review waits; (2) **reviewer distinct
from builder is a station rule**: a held car's review is a step queued at
landed-review or design-review, and the claim refuses a pull by the actor
who built the car, so the distinctness no longer rests on a coordinator
picking a reviewer; (3) **Codex and Claude B leave their interactive
sessions for a supervised pull loop** once the pull works — both sat idle
at a prompt from 21:04Z that evening — and switching them ends their
current sessions, which is David's go to give. One verb, `boss station
pull <station>`, wraps claim-next as the first act of every executor
turn and returns the step with its dispatch brief. The coordinator's
remaining job is allocation — how many seats, at which stations — plus
releases and holds; a dark station with no seated executor is normal.
Not built when decided: the projection repair the stale queues need
(item `b3572a26`, the prerequisite), the seat key, the verb, the
builder-distinct refusal in the claim, review steps at the review
stations, and the loop. This is the pull half of M above (who acts next),
answered by the station's discipline at the claim rather than by the
dispatcher's reflex on `step.ready`.

**The queue coordinator's cycle is a rules document; choosing each
executor's work has since moved to the station** (design-doc `8cc2c71f`,
"Queue coordinator cycle: advance, refill, isolate holds, and prove wake
capability", opened by agent-codex 2026-10-02 with no open questions;
folded 2026-11-12). An actor explicitly assigned to coordinate the
queues runs one cycle: observe through `boss orient`, advance completed
work before refilling, refill by urgency then age, keep gate submissions
coherent, keep each hold on its own packet with an owner and a next
observable check, and leave a cycle receipt. Its five-minute recheck is
an operating target, not a wake source: prose implements no timer, and
a wake capability is claimed only from machine evidence. Built: the
text, as `infra/platform/documents/coordinator-rules.md`, which
CLAUDE.md's session-startup section names for that assignment (car
`6893e4ba`, train #900), since amended for option A's review holds
(`bd331d38`, train #1015) and the owed landed reviews (`c851ed24`,
train #1042). Not built: the design's own verification, which is two
evidenced cycles, the measured delays and a recorded wake capability.
That stays owed on item `6cd6c935`, which the car left open as a
partial (its build step still ready on 2026-11-12). Superseded in part:
the cycle's assigning half, where the coordinator picks each executor's
next packet and its distinct reviewer, gives way to `506e0a1c` above
(the coordinator allocates seats, the executor pulls through `boss
dispatch --next`, design `6e42c6de`), and that decision's answer to the
wake question is the supervised pull loop, not yet built. Until those
land, the rules document still speaks in the older words.

**A step is a station address, a completion contract and a readiness
predicate; the station answers who and where, the packet what,
and why stays David's** (design `771f83ca`, David 2026-11-08, its
three questions answered as proposed on the reviewed text; Q2's
representation since left open, below). David asked whether a step can be essentially an
address plus formal completion checks, then framed doing work as
five question words: a station is the virtual space that brings WHO
(its admitted executors) and WHERE (the resource pools and machines
it reserves durably, or draws and releases) together; its priority
admission queue orders WHEN; the packet and its encoded step are WHAT;
and WHY is David's intent, carried on the packet with its design
and decision links for people to read, never evaluated or compiled
into a predicate. Stations and protocols are both data. So a step
keeps its stable identity (today's `title` slug, which predicates
reference: an id, not display text) and its `ready_when` (dependency,
not capacity), names where its work can be done, and declares what
proves it done. Measured on origin/main `d88cf723`: 89 active workflow
rows and 542 steps live, 79 and 478 in source, where 269 top-level
placement and execution keys across 231 source steps (`audience`,
`authority_role`, `agent`, `executor`, `resources`, `claimable`)
mix routing, execution and authority, and the 37 source `agent`
blocks hold only eight distinct configurations.

**Execution policy is the station's, and the station cannot negotiate
the contract** (Q1). A station service owns its admissible executor
kinds and model classes, the agent profile, model preference, budget
and effort, its machines, its claim discipline and its resource bill;
allocation stays on the station tree and its pools. A step carries
no executor, profile or bill that overrides or widens the station's:
a protocol that needs different execution addresses a different
station service. What the completion contract requires (business
authority, a person-only decision, a named signer, independence
from the builder, fresh evidence, an assurance floor) stays on the
contract and is checked at claim and at completion; a station that
can host robots cannot relax it, and a queue address is never a
permission grant. The census holds 48 `human_only` defaults and 11
`assurance_required` steps, and both meanings survive any move. Read
against the station tree's T6 (4b27a65a), the step's station spec narrows to an
address: executor, machine and bill defaults belong to the station
service it names, and only the contract's non-negotiable conditions
ride the step. Model-specific work is station structure: work David
means to require Opus addresses a station whose admissible robots are
`robots:opus`, with no model key on the step. A requested model is
not an observed claimant, so the 41 live agent-block preferences do
not become enforced Opus-only constraints, and the five hard executor
declarations do not become preferences.

**A published address must be able to fulfil its contract.** The
authoring test is that the addressed station's WHO and WHERE can do
the step's WHAT. Publishing resolves a static address against the
active station tree and refuses, naming the protocol, step, station
and version and each unsatisfied requirement, unless the station
advertises a service with an admissible executor, the required machine
interface, a compatible Step UX (a builtin renderer whose schema covers
the fields suffices) and verifiers for the evidence and assurance
asked. For a dynamic address whose candidate set is enumerable,
every permitted candidate is validated, not one convenient station;
an unenumerable set records its declared bound and leaves the check
to resolution. Resolution and admission re-read the actual station
against the pinned contract and live allocation, and refuse an unknown,
ambiguous, retired or incompatible destination by name rather than
routing elsewhere. A busy station is capable and waiting; free capacity
does not make an unsupported contract executable. Admission records
the capability witness (station and service versions, contract kind and
version, executor identity and class, machine and UX bindings); a live
station edit never widens a pinned contract; and moving claimed work
is an explicit recorded transition that keeps its claim, resources and
signed evidence. StepType keeps contract identity, fields, evidence,
sign-off and assurance semantics, while its completion dispatch mode,
executor defaults and duration model are to split out into station
execution policy: this is not the StepType registry's retirement. Step
UX resolves from the contract kind and its pinned plugin version;
Station UX starts from the existing station lens plus a registered
renderer, and a station row never supplies arbitrary fetch URLs or
grants endpoint permissions (uploaded station code needs its own
sandbox design first).

**A station may advertise a child's capability only as delegated,
with a witness** (Q3, under the station tree's R8). The parent names
the child service or protocol that fulfils the contract; resolution
records the chosen child and its completion evidence; the child claims
its own allocation; delegation is acyclic and bounded; an unavailable
child refuses; and the station page shows delegated capability apart
from direct. There is no flat union of the descendants' capabilities,
and a parent never claims to operate a child's machines.

**A dark station is a normal state.** A station with zero durable
allocation and no admitted work is a declared space that keeps its
identity, purpose and capability declaration and an entitlement to draw
from permitted floating pools. It is not a missing row, an outage or an
unreadable observation, and it needs no resident worker. Capability is
not possession: admission draws atomically under the ancestor and pool
bounds and records the acquisition before work starts, and that is what
wakes the station; viewing it, queueing work for it or advertising its
capability does not. When the last temporary draw is released and no
durable allocation remains, it darkens again. The view draws darkness
as normal, and parent summaries do not count it as an outage; it shows
declared draw sources apart from resources held, keeps queued work and
its named wait visible while dark, and draws stale, unknown and failed
reads distinctly from intentional dark. Unknown capacity is never zero.

Decision history and what is open: the review answered Q2 as its
proposal then read, typed predecessor or packet references constrained
to a station subtree, resolved once at readiness and readdressed
only by an audited move while unclaimed. David's five-question
framing arrived as that review completed, and the amended design
(2026-11-09) leaves the addressing representation (static identities,
packet or predecessor references, declared selection rules) and its
binding moment open as a data-design choice, claiming no retrospective
approval. Design `305cf4ce` (2026-11-12, §Claims stand at the station
the tree resolves) selects the scheme: one static `at = <station id>`
on the step, `under` dropped. The publish, resolution and witness
requirements above bind it. Not built
when decided: StepSpec has no address or completion wrapper, nothing
validates a station's capability against a contract, and filing
authorized no protocol publish or build. The design estimates 400
to 700 gross lines of selector, projection and backfill retirement
once readers move, against new validation cost, and promises no net
deletion before the old readers retire. The census, both inventories
and exhibit E1 (backlog-item v20 and ship-a-change v15 rewritten
losslessly) stay on the packet.

**P, Q and M name the three domains of getting work to an actor, and
Q's constraint stations are projected from P's protocols** (folded
from `p-q-m.md`, decided in packet `73697169`, David 2026-08-16).
**P** (Protocol / Policy / Publish) owns what a protocol means and who
may act, and publishes a hand-off the protocol names straight into its
queue; **Q** owns where work waits; **M**, the matchmaker, owns who
acts next — as a query over Q's load, scheduled and ad hoc, rather than
a reflex on `step.ready`. The measurement that decided it: 45 active
workflows declared 51 distinct `(step-kind, role)` constraints and three
stations had been authored, so a step saying "a `bookkeeper` does a
`bill-approval`" had declared a queue nobody could see. Decided: (1)
**constraint stations are derived**, regenerated when protocols change,
with authored rows kept for queues no constraint implies
(`my-watchlist`, `loading-dock`) and an authored row winning over a
derived one of the same name; (2) the dispatcher's `step.done` rules
are P's and its `step.ready` rules are M's, and the claim CAS belongs
to neither; (3) **a hand-off a protocol names bypasses M entirely**, so
a route into a queue no actor matches must be refused at publish — no
matchmaking can rescue it later; (4) the letters are **vocabulary
first, crate boundaries when one is violated, separate services
probably never**; (5) `design-review` and `repair` become derived.
Built: `boss_jobs::station_projection` projects the `q.<role>.<kind>`
constraint stations (and a `(role, model)` projection since
`c87fb59b`), and `q.platform-admin.task` is the worked authored-over-
derived row. Not built: M as a query — assignment is still the
dispatcher's reflex on `step.ready` — the publish-time refusal of an
unmatched route, and (5): both rows are still authored files under
`infra/platform/stations/`. Whether Q becomes an owning domain was
narrowed by the dispatcher/station boundary (§Dispatcher) and stays
open there.

**A role or a model is an executor filter, not a station** (design-doc
`6e42c6de`, "A role or model is an executor filter, not a station",
David 2026-11-10; it supersedes decision (1) above). Who may execute a
step — a role, a model, a person — is checked at the claim. Stations
are only the declared nodes of the one tree (4b27a65a), and no station
name is generated: `q.<role>.<kind>` and `a.<role>.<model>` were a view
of everything a role or a model could do, standing outside the tree
with no parent, capacity or allocation, and on 2026-11-10 they held the
yard's two largest piles (1,164 packets at `q.platform-admin.task`, 724
at `a.platform-admin.opus-5-5`) while no work flowed through either.
Decided: (1) `boss dispatch --next` pulls from the executor's own tree
station and its subtree, filtered by what it may claim; (2) MY WORK is
a filter on step audience and assignee, not a station read; (3) the
yard and the map draw only tree stations, and a role or model view is
a filter on the claims and queue reads; (4) generation stops. Built:
(1) and (2) (backlog `d49e5ddb`, train #1680) — `--next --station`
refuses a `q.`/`a.` name and names the tree form, the authored
`q.platform-admin.task` row included, whose work an unseated executor
now pulls from the root rather than executive (design 2910ec3d's
hand-off). Not built: the readers that still name a
generated station — the claims console, `station_reach`, gate queue
observation and the regions fixtures (backlog `ed2a3e34`) — and the
retirement of `station_projection`'s generation with a pin that every
station a reader sees is a declared row (backlog `2eb78346`); live
`GET /api/stations` still answered 14 `q.`/`a.` names among 38 on
2026-11-12. `--next` sends no `?station=` on its claim: car 1 was built
while the claim door judged a station by predicate alone, and the door
has used the queue's own membership since train #1676 (backlog
`1e7f5372`); the comments still saying otherwise, and `--next --model`
being ignored by the filter, are backlog `844f2cea`.

**An address is a requirement, not a destination** (folded from
`requirements-based-addressing.md`, all six questions decided in the
in-app decision tracker on 2026-08-14, which predates design packets,
so no packet id exists). David, 2026-08-11: "we let people define
addresses based on requirements instead of a known destination". A
step states what its payload requires — a predicate over actor
attributes: role, department, clearance, capacity — and the pool of
actors satisfying it is where the payload may go, the move `ready_when`
already made for *when*. Decided: (1) the predicate is **inline on the
step first**, and a named-queue registry is sugar once the same
requirement is written a third time; (2) **`boss-expr` gains a
set-valued result and a small closed function registry**
(`in_department`, `has_role`, `has_clearance`, `has_capacity`) rather
than compiling addresses to SQL — one language, and no query execution
driven by tenant data; (3) **the pool is resolved at hop time and
emitted as an event**, re-resolved on a schedule while the payload
waits and emitted only on change, so determinism holds and the log can
answer "who could have done this"; (4) **pull is the default, push
opt-in per requirement** (the measured inbox was 1,016 unread, 619 of
them machine notifications); (5) **an empty pool fails at address time
to the payload's owner**, and an unattended one raises after a
per-requirement threshold to the requirement's owner — the undrained
queue is the algedonic signal; (6) **a layered protocol only narrows
the pool**, and escalation is a sub-job fired into a wider queue, so
composition stays commutative (the packet model above composes the
protocol set once, at admission, and rejected mid-flight layering for
v1). Address resolution stays inside `boss-policy`'s row-level
model rather than becoming a second authorization path. What is left:
the address mechanism is not built — `boss-expr` has no set-valued
functions, steps address by `authority_role` and stations by their
constraint predicates, and the admission edge's pool events are the
forward direction in §Dispatcher. The doc's larger inversion, a queue
that exists only while something satisfies it, is **not** settled: the
station registry shipped as rows, a disagreement recorded under §Open
findings.

**A platform station is declared in `infra/platform/stations/`, a
step plugin's row in `infra/platform/step-plugins/`, a cadence rule in
`infra/platform/cadence/`, the delivery policy in
`infra/platform/delivery-policy/`, and a migration newer than
20260918112134 declares schema only** (2026-09-18, backlog 393d3234,
consolidation H4, four registries in four cars). Measured on that day,
seven migrations were the only place a platform station existed, seven
the only place a step plugin's row did, nine the only place a cadence
rule did (six of the nine re-versioning one integer, the boarding
threshold, and one of those a silent no-op), and two the only place the
delivery policy did (the second a policy edit that rode the pipeline it
was re-tuning), and a
migration is the wrong home for a registry row: it runs once, a fresh
instance cannot re-declare the row without replaying history, nothing
drift-checks it against the live row, and every edit is a contended
timestamped file. The dispatcher rules above and the Workflow bundle
before them already answered this; stations and step plugins take the
same shape: one `<name>.toml` per row carrying every column, published
insert-if-missing by (name, version) at every start by the same seed
the Workflow bundle rides (each directory is found beside
`--seed-path`, so no launcher changed), through one decision table
(`boss_jobs::bundle_seed`) rather than one per registry. A bundle row
that differs from the live active row of the same (name, version) is a
refusal naming the field — a version bump is the edit path — and
`infra/lint/migrations-declare-schema-only.sh` holds the cutover stamp
once, with the registry tables listed beside it. The historical
inserts stay as history; each bundle is pinned equal to the rows they
produce, and proven able to rebuild an emptied table alone. A step
plugin's row lives under `infra/platform/` rather than beside its JS in
`infra/step-plugins/` because that is what the launchers already read:
the image copies `infra/platform` whole and the JS reaches the cluster
as a ConfigMap built from `*.js`, so a row beside the JS would have
needed a Dockerfile COPY, a seed flag and an init.sh edit to be found
at all. `infra/lint/step-plugin-bundle-exists.sh` holds the row to its
JS across the two directories. The cadence bundle differs in one
posture the table's design demands: `cadence_rules` is live-editable
protocol data (measure, experiment, update — without a deploy), so the
bundle is the declared BASELINE, not a lock — a rule an operator
re-versions live is reported as ahead of its file and left alone, a
rule the operator retired stays retired, and only the one refusal
every bundle has (a file edited at the same (name, version) the live
row holds) stops a boot. The conductor's loop reads the live table
over `/api/cadence/rules` and did not change. The bundle carries three
of the four rules the migrations seed: `protocol-retro-daily` is
retired by decision the same day (the weekly department retro opens
IT's retro through a dispatcher clock rule), and the equality pin
compares the bundle to the migrations' active rows minus a
`RETIRED_BY_DECISION` list that names it and why — a bundle must not
carry a row the operator is retiring. The delivery policy is the
`workflows` shape at one row: a train pins the version it departed
under, so a bump in the bundle changes the NEXT boarding's rules and
never a train in flight. Its compiled fallback in `boss-cli` stays —
the conductor runs outside the cluster and must board when the
registry is dark — and is now held equal to the bundle with no
database (`the_bundle_equals_the_compiled_fallback`), closing a
triangle the migration-side pin had left open on one edge.

**Priority is a number on the packet, and the words are bands in a
registry.** David decided on 2026-10-31 (backlog 9ca42fe5) that priority is
a number each station queue orders by, that labels are bands over it held
in a registry, and that there are three bands for now, Critical ("the job
is required for factory operations to come back online normally"), High
("prioritized ahead of Standard, not blocking") and Standard. Design
`67f17da0` settles the rest with no open questions. The number is an
integer **0..=1000, higher drains first**; a station whose discipline names
`priority` sorts by it descending, then by age. The bands are
`infra/platform/priority-bands/<name>.toml`, one file per band, published
like stations, each with `name`, `label`, `floor`, `default`, `meaning`,
the flags `outranks_standard`, `triggers_escalation` and
`first_dispatch_guard`, and `migrates_from`: Critical floor 700 (default
800, all three flags; emergency → 900, urgent → 800), High floor 400
(default 500, no flags), Standard floor 0 (default 200; standard → 200,
scheduled → 100). One reader, `boss_core::priority`, answers every rank and
word question, and a lint refuses a comparison outside it. `PUT
/api/jobs/{id}/priority` takes a number or a band name and refuses the
legacy words (no alias period before 1.0.0); every job read returns the
number and a `priority_band` computed on the server. The migration goes
through the record's own doors: a schema column backfilled through
`migrates_from`, the rebuilder replaying old words through the same map,
the 2026-10-31 sweep's items moved to High by the verb with a reason, and
no `priority_changes` history rewritten. More bands later are registry
rows. Decision history: this replaces the earlier decision that priority
would become Class-registry data (Classes of `job`-kind Subjects retiring
the `CHECK` constraint, the closed enum and the TS union); escalation
stays a hop between stations, never a discipline.

**A protocol gets a page.** Each workflow kind grows the network
vocabulary — name, version, purpose, demands, and usage read from
the `jobs_kind_version` index — so a protocol is presented as a
protocol rather than only as a DAG, and the canvas's route ghost
becomes a per-protocol tint. The system diagram redraws on the same
vocabulary and becomes the one diagram the README and the canvas
legend both cite (resolved 2026-08-12; the redraw itself is not yet
executed). The job-packet-network decisions (Q1–Q7, decided
2026-08-12) are the paragraphs opening **A Job is a packet**, **The
protocol set is fixed at creation** and **Headers are declared data**
at the head of this section, the priority decision above (which
replaced its Q5), and this one. Its file,
`docs/design/job-packet-network.md`, was folded here and deleted on
2026-11-18 (backlog 924de432). The target shape it carried beyond what
had shipped in August, the header registry that generalizes
`job_edges` and the stage-2 re-keying of the `Self_`/`Team` scopes onto
queue-derived ownership, is stated in those paragraphs.

**A protocol variant is a workflow version, and its verdict is read
from the log** (folded from `protocol-experiments.md`, decided in the
in-app decision tracker on 2026-08-14; no packet id exists). The
registry is append-only and versioned, so an experiment arm is a draft
published beside the incumbent, not a fork; cohort membership is
decided once, at admission, because the protocol set is fixed there;
the verdict is a query over terminals filtered by workflow version; and
the conclusion is the registry's own publish or retire. Decided: (Q2)
**an arm passes the same publish lints as any protocol, and killing it
is retiring its version** — packets in flight finish under their pin —
while an arm whose depth or failure rate exceeds the incumbent's by a
declared margin raises to the experiment's owner; David's rider, that
shadow traffic should be tried before real traffic, became the shadow
lane below; (Q3) **the verdict renders as a lens** — two arms as two
queue predicates over the same stations, cohorts always visible.
Superseded: Q1's `experiments` registry row, by packet `574c2adf` Q3
below (an experiment is a packet), and Q4's first experiment. The doc's
worked case — the boarding threshold raised from four cars to eight by
a registry edit with no deploy, kept "until we get evidence that our
repair rate is rising" (David, 2026-08-13) — named an exit condition
with no instrument, and the threshold has since moved again
(`min_dock_depth = 1` in
`infra/platform/cadence/train-board-on-dock-depth.toml`). Not built:
the margin raise and the verdict lens.

**Experiments run on the network's own machinery: version against
version, a third partition, and the experiment is a packet** (folded
from `network-experiments.md`, all five questions decided by David in
packet `574c2adf`, 2026-08-22; Tier 2 built as `6ea5a12a`). Every
brewery experiment that week had been run by hand, with no record of
which packets were the experiment and no control group. Decided: (1)
the experiment unit is **workflow version against workflow version**;
(2) **shadow is a third partition value**, `real | simulated |
shadow`, so fail-closed stays provable at every boundary consumer; (3)
**an experiment is a `protocol-experiment` packet** with `promoted` and
`retired` terminals, not a table; (4) **only IT and platform-admins
may experiment**, for now; (5) **the sim never participates** — "The
sim should be entirely independent from any experimentation
capabilities in the network" — so splits are unpaired and the sim is
load, never an arm. Built in tiers: **Tier 1**, a terminal report per
`(kind, version)` — `GET /api/workflows/{kind}/terminal-report`,
outcome distribution, count and median cycle time, grouped by arm; and
**Tier 2**, split admission: the split declaration is the experiment
packet's job metadata (`kind_under_test`, `control_version`,
`candidate_version`, `split`), and admission hash-splits each new
packet of that kind by its own id with a fixed FNV-1a
(`boss-jobs/src/experiments.rs`), pins it to the arm's version and
stamps `experiment_arm` / `experiment_id` before `JOB_CREATED` is
built, so rebuilders replay the recorded choice. A malformed
declaration admits under the active version, unstamped, because an
experiment must never break the kind it measures; promote and retire
stay registry verbs an operator runs. What is left is **Tier 3, the
shadow lane**: the partition value exists and a shadow chain's writes
fail closed in the dispatcher exactly as a simulated one's do, but
nothing admits a shadow packet — the create door refuses
`partition=shadow` until handlers skip shadow entirely (car 2 of
`508cc38c`) and shadow admission mirrors a real trigger into the
candidate (car 3). When experimentation starts at all is the threshold
decision that follows.

**The experiments program starts at a named threshold, not now**
(design `d8771dec`, David 2026-09-23, accepted as proposed; answers
backlog `8af3aed1`). Measured against the system of record:
`kind=protocol-experiment` answers 0 — the "iterated live" history in
the workflow's header, and the experiment it cites, belong to the
retired second stack; opening one needs no code, but a malformed split
fails silently, so an experiment can sit open splitting nothing; and a
before-and-after reading cannot credit a protocol change —
`backlog-item` went v1→v7 in seven days with the same ten steps while
its median cycle days swung 13×, so the period drives the number and
only a concurrent split can tell. Decided: **experimentation begins at
the first draft workflow version, on a kind with 20+ terminals a day,
that adds, removes or reorders a step**; its author opens it as a Tier
2 split (`boss-jobs/src/experiments.rs`) instead of publishing, and
that experiment's first car is a verb that refuses a malformed split;
the workflow header's second-stack history is corrected. Only that
correction is built (backlog `1baeb936`: the header of
`infra/platform/workflows/protocol-experiment.toml` now says the body
was seeded here as v1 and names this threshold): no experiment has run
on this instance, and the refusing verb does not exist.

**A flight ships a change to our own instance behind a flag, and the
flight is a packet** (design `c4c2a607`, David 2026-09-24, the one
question accepted as proposed; answers backlog `73c31776`). David, that
day: "We need to be able to flight ideas safely too." Measured then: the
only switch was the tenant manifest's `[modules]` — per tenant, changed
by a redeploy — so a new idea landed for everyone or not at all.
Decided: (1) **a flight is an open packet whose job metadata carries a
`flight` block** (`code`, `hypothesis`, `signal`, `audience = {actors,
roles}`, `owner`, `observe_days`) **and nothing else stores it** — no
flights table, since the packet is already versioned, audited and
rebuildable (§9a); the read is generic, every open packet carrying the
block whatever its kind, so the protocol is replaceable without a
deploy; (2) **one server read, resolved for the viewer**: `GET
/api/flights/mine` (boss-jobs, fronted by the gateway) answers
`{flights: [codes]}` — the codes on for the session's actor, on for all
or on for an audience naming the viewer's id or role — and the gateway
inlines it as `window.__BOSS_FLIGHTS__` so the first paint takes the
right path, read in the SPA through web-kit's `flightOn(code)`. **A
code the read does not list is off** — retired, unknown, ambiguous, or
a dark read — the modules rule (`ce68f137`), so a failure shows the old
path; the audience is judged on the server and the SPA never sees one.
The code in `{#if flightOn('<code>')}` is the flight's identity, never
its state; (3) **the protocol, `flight-a-change`**: filed with its
hypothesis and deciding signal before any reading, the fork shipped on
a merged car, turned on for David first, observed for the declared
period, optionally widened to everyone, then promote or pull — and
either way a cleanup car deletes the fork and the losing path, and
**retired means the code no longer names the flag, proven by a machine
probe** (`git grep -n "flightOn('<code>')"` on the converged main
printing nothing), never by anyone's statement; turning a flight off
needs no car; (4) **flags cannot rot**: an hourly dispatcher threshold
files an alarm naming the flight, its owner and its days over, for a
flight past `observe_days` + 7 with no verdict or decided 14 days ago
and not cleaned up; (5) **flights stay apart from protocol
experiments** (above): a protocol split is decided per packet at
admission and is never removed, a flight per viewer at read time and
must be removed. They share the discipline — hypothesis and signal
before the reading, a declared period, an exclusive verdict with a
fallback — and if the two workflows keep the same step list for a
quarter, one workflow over `code-flag | workflow-version` is the
collapse; (6) **core, per instance**: the read and `flightOn` ship in
core and the Workflow in the platform bundle; a hosted instance runs
its own flights, and a flight across instances waits for one to exist;
(7) **the first flight is `it-map-motion`**, the moving IT map (design
`31bade8f`), audience David, seven days. The review's one answer: **an
agent may file, ship, turn on for David, record the reading and pull at
any time; widening to everyone and the verdict are David's** while he
is the only human user, to revisit when a second joins. Not decided:
percentage rollouts (with one human there is nobody to randomise) and a
flight carrying a value (a value is a registry row).
**IT map graduation, 2026-10-23.** David promoted both `it-map-transit`
(flight `54c04706`) and `it-map-live` (flight `71c4d42e`): the desktop
Department Map always renders the transit monitor with recorded packet
motion, the phone keeps its strip, and the viewer keeps their crossing
time choice. The WorldMap and rate-replay rendering paths are retired.
Superseded since: R3 of backlog `23a4b4d7` deleted the transit monitor
itself (trains #1638 and #1639, 2026-11-09), so `/it` no longer draws a
map; see the System Network Map paragraph at the end of Step UX &
frontend.

Built, 2026-09-24: car 1 (#637) is `boss_jobs::flights` behind `GET
/api/flights/mine`, the gateway's inline, web-kit's
`flights.svelte.ts`, `infra/platform/workflows/flight-a-change.toml`
and the rule `a-flight-past-its-period-is-an-alarm`
(`jobs.flight_overdue`, whose step names ride the rule row). Where the
build departs from the doc, the build is current truth: a flight's
state is **not stored in the block but read from its completed steps
against the pinned protocol** — each state-setting step declares
`metadata_defaults.flight_state` (`turn-on` on for the audience,
`widen` on for all, `pull` off, and off is final), so a completion
cannot claim a state its step does not declare; `widen` and `decide`
are `human_only`, and `widen` counts only when `completed_by` is a
person; `extend` is its own once-only step followed by `reobserve`,
not a third verdict; `pull` is open from filing; closing the packet
retires the flight; and a code two open packets both declare answers
off. Car M2 (#639) put the moving map behind
`flightOn('it-map-motion')`, and that flight's packet was filed and
turned on for `emp-david` at 20:55Z the same evening; its `observe`
step is ready. Not built: `observe` does not refuse a reading before
the period has passed, and a code's uniqueness among open flights is
not checked at admission (both named in the workflow's header); no Rust
client reads flights — `boss-jobs-client` retired in #634 — so a
service flight reads `GET /api/flights/mine` itself, as the `ship`
procedure says; and backlog `73c31776` still stands at `build`, its one
car parked as a part.

**A step declares its audience once, and every surface derives its
selector from that** (design `f5ebd2e1`, David 2026-09-11; three
questions accepted as proposed, the fourth a constraint). The
measurement: a backlog item routed to `design` produced a decision step
at slug `design-review` that no station predicate claimed — invisible
on `/it/design` by construction — while My Day's endpoint listed it
among eleven rows for `assignee_id=emp-david&roles=platform-admin`, and
the operator found neither. Read from the code, placement is three
mechanisms keyed on three different things: an individual on
`step.assignee_id`, a role on `metadata.authority_role` (with the
unclaimed-or-active rider), a queue on a `StationPredicate` over
`job.kind` + `step.slug`/`step.kind`; My Day unions the first two, a
station page reads only the third, and nothing asserts that a ready
step is claimed by any of them. Decided: (1) **one required audience
field** on a step's declaration — a closed set of shapes: an
individual, a role, a department, a named station — from which all
three selectors derive, because a closed set makes an audience nobody
reads *impossible* rather than unlikely; (2) **department is Class
registry data** on workflows and/or stations, never a match in core
code (CLAUDE.md §9), which also answers "every human-only step in the
department regardless of assignee" without a bespoke query; (3) **no
orphan steps**: a check, computable from registry data alone, that
every ready step in an open packet is selected by at least one station
predicate or carries an assignee — the mechanism that would have caught
this the day it was introduced; whether it warns or refuses at publish
is open until the station set is complete. The fourth answer is a
constraint on any build, not a choice between them: **surfaces never
show inconsistent representations of the same step.** Not yet built;
until it lands, a decision that must reach a person is assigned to
them by id, which is the one selector every surface honours today
(2026-09-15: six operator decisions assigned that way, each with the
ask written on the step as `context_md`).

**My Day separates what only you can move from what an agent could,
and a requirement for a person is protocol data** (folded from
`my-day.md`, all five questions decided in packet `e4270060`, David
2026-08-16): "It makes sense for 'My Day' to be showing both jobs in my
personal queue or jobs that I am eligible / matched for... a special
separation between jobs that are in a queue with a human-only policy
with jobs that agents are also eligible for." Measured that day: all
48 items in his day carried `authority_role: null`, the system had no
human-only concept anywhere, and the claim gated on role, which agents
hold exactly as people do. Decided: (1) **actor category becomes a
Class**, while the `ActorId` enum survives as the wire and log form,
because its variants are the shape of immutable provenance; (2)
**agents and automations become registered Subjects**, so `kind =
human` is the same shape of clause as `role = platform-admin`; (3)
**the requirement widens `authority_role` into a small predicate over
actor attributes** rather than adding a sibling boolean, which answers
one question and cannot answer the next; (4) **three sections, ordered
by what they demand** — *Yours to decide* (only you can move it, and
the only section allowed to be bold), *Yours* (assigned to you, an
agent is eligible, shown so you can hand it off), *Open to you*
(unassigned and you match: a pull queue, not an obligation, labelled
empty until pull exists); (5) **human-required is protocol data**, so
whether it spreads is measured by querying active protocols, not
policed. Built: agents are registered, with a role and a department,
and an agent's login resolves to its registered identity; *Yours to
decide* is live and is the founder's watch list below; and a person is
required by the step key `metadata_defaults.human_only`, enforced at
assignment, claim and completion by `boss_jobs::human_only`. That key
is the boolean (3) argued against, kept as the one declaration until
the predicate exists, as the module itself says. Not built: category
as a Class, the widened predicate, and a server-side partition — the
rest of the sections are the client code named in the dispatcher/
station boundary (§Dispatcher).

**The founder's watch list is his own assigned queue, aged, on the
surface he already reads** (design `5877860d`, David 2026-09-21, all
four questions accepted as proposed; built as backlog `3bc896be`).
Measured: David routed a drive purchase (`b2d5b546`) to design review
because `/it/design` was the one surface that showed what was waiting
on HIM; it sat there with no `design_id`, at a step nothing could
complete, while the work it described happened and was recorded on it —
visible and inert, which looks handled. Decided: (1) **a per-actor
queue** — what `boss orient`'s MY WORK already is for an agent — rather
than a `watched_by` flag (one more thing to remember at the worst
moment) or a derived "needs him" list (the receiving queue under another
name, and the right SECOND step once measured); (2) **age is not
optional**: every row carries how long it has waited, oldest first, and
crossing a declared threshold changes how the row reads, because an
accurate signal that never changes stops being read (learning
`84f8f9b9`: the garage read TROUBLED identically for nine hours);
(3) **no new page** — it renders where he already looks; (4) **assigned
steps first, measured second** — what he routes to design without a
design is the evidence for any wider category. Landed on My Day (`/`):
"Yours to decide" orders actionable verdicts oldest `opened_on` first
and bands each row by the receiving yard's own age thresholds (past 3
days aging, past 14 stale) — one definition of old, not a second copy.
The derived list waits on that measurement.

**`/it/design` is the design-doc review queue and nothing wider; when
it is empty it says so of itself and points at My Day** (backlog
`8c0e11d8`, David 2026-10-11, "Drop the stale declaration"; the plan is
design `f3cd65ad`, which asked no new question). Measured by page-audit
`79db48ae` on 2026-09-23: the page read "Nothing is waiting on a
decision" while six page-audit sign-offs waited on emp-david, and five
workflow rows declared `surfaces = ["system-design"]`, a hint no reader
of the page consumes. Of the two options on the item — render every
decision those rows route to Design, or drop the hint — he chose the
drop, which keeps the watch list above as the one place an actor's
decisions gather. Decided: (1) Design keeps its station, its review
progress and its decided, folding and settled panels; pending builds
and generic human approvals do not become Design rows; (2) the stale
hint leaves `department-retro`, `doc-flatten`, `page-audit` and
`protocol-retro` (`incident` had already moved to `system-incidents`)
as new append-only versions, and in-flight packets stay on the version
they were admitted under; (3) the empty state claims only that no
design doc waits, and links to My Day's *Yours to decide* for the rest,
because an empty station is not an actor with no work. Not built: on
2026-11-13 the four rows still carry the hint and the page's empty line
is unqualified.

**Stuck is a packet past its own place's bound whose next move is ours,
counted per third and never summed** (design `cf820810`, David
2026-09-23, all seven questions accepted as proposed; answers backlog
`4142d821`, two of the figures for the HUD frame `c1253e50`). Not a
composite score: each number keeps its name and its denominator and
clicks back to the region that owns it. Measured on `/api/yard/regions`
that evening: the delivery half already existed — every region carries
a `trend`, this window against the previous with sample counts, and
arrivals read 30 a day against 29 — and the shed already told stuck
from waiting (`PROOF_STALE_HOURS = 24`; `NOT_YET_STARVED_HOURS = 72`,
measured over 1,812 ops-requests; `waits_on` with `car::starved`;
"troubled means ours", `3881f5c9`). The gap was the incident's
population: three cars could not board for 9.5 hours while the dock
said the boarding depth was met, because only the conductor reads the
declared `boards_after` edge (`boards_after_outcome` in
`boss-cli/src/train/boarding.rs`) and the dock region counts parked
cars against `threshold_met` without asking whether they can board.
Decided: (1) **stuck is past the declared bound AND the next move is
ours**; past the bound but waiting on a declared outside owner — the
world, an event, a named human's act — is **waiting**, shown beside
stuck and never summed into it, the shed's rule extended to every
region; (2) **the population is the union of five that already have
owners**, with no new measure: receiving past the 3-day triage band,
stations not draining or over WIP, hand-held cars and held greens
(garage), parked cars whose `boards_after` predecessor has not landed
(dock), and landed cars past 24 hours whose move is ours (shed), each
count clicking through to its region; (3) **counted per third** — queue
management, actors building, delivery — never across them, since one
total would read 271 receiving packets plus one shed car and hide which
half moved; (4) **the paired delivery number is arrivals per day
against the previous window**, read from the arrivals trend, while dock
wait and time at CI stay on their own regions; (5) **unknown is not
zero** — a station whose flow cube is blind contributes `?` and its
third reads `≥ n + ?` (one station, 170 standing, read that way); (6)
**one `stuck` block on `GET /api/yard/regions`** — per third: stuck,
waiting, oldest age, owning regions — read by the HUD and `boss orient`
and recomputed by no client; (7) **the dock is repaired first, as its
own car**: `boards_after_outcome` moves into `boss_jobs::car` so the
dock and the conductor share one reading, and the dock says "N parked,
M cannot board" and stays busy rather than claiming a train is due.
Built (7), 2026-09-24: `boss_jobs::car::boards_after_outcome` is the
one judgement, the regions read fetches each parked car's declared
predecessor by id, and the dock says "N parked, M cannot board (waiting
behind X)" — busy, or troubled when the edge can never clear. Built
(6) too: `GET /api/yard/regions` carries the `stuck` block
(`boss_jobs::regions::stuck`, `ThirdStuck`), and the HUD frame below and
`boss orient` read it through the `thirds` rows.

### The company station hosts the company's decisions

**The executive station is the root's own function, so it retires into
the root** (design-doc "The executive station is the company station",
`2910ec3d`, David 2026-11-10: "the Executive station is actually the
same as the Algedonic station in theory"; no open questions). The human
decision steps — approval, scope sign-off, decided designs, the
operator's task queue and watchlist, passkey promotion, break-glass
enrolment, publish requests and operator confirmations — are hosted at
the root, `algedonic`. The department tier directly under it is the
units that do the work: Engineering and Operations, and Finance since
design `c0ae096f`. The reasons: a station is defined by what its parent
allocates to it, and executive was allocated nothing (0 reserved, one
floating draw per human step) and had no executor the root does not
hold; receiving and marshalling then both read as waiting at the
company, which is what they meant; and in the design's reading of Beer,
policy and identity close the whole system rather than sit beside its
operations as a sibling department. That last is a reason for the
shape, not a mapping: §The Viable System Model, mapped, still leaves S5
unmapped. A second human, such as a hosting customer's operator or an
employee, is a `people:*` pool at the root or a station under the
department they serve, never a peer of the company, and a desk is split
out on the day it has an allocation of its own. Only the station moves:
`executive` as a role or a Class is untouched.

Not built as of 2026-11-12 (origin/main `e8bb76713`; the live registry
agrees). `executive.toml` v2 is live, `design-decided`, `my-watchlist`
and `q.platform-admin.task` still name it as their parent, the
`surface-usage` automation is hosted there, `GET /api/departments` still
answers an `executive` row, and the web's station tree draws an
executive card. `design-review` and `landed-review` have already left it
for engineering's `design` and `review` stations (design `c0ae096f`).
Until the cars land, `docs/design/station-tree.md` describes the tree as
it runs, not as decided. The cars are backlog items, one each: the
registry (`878fd1e7`: re-parent or retire the three rows, delete the
executive row, take it out of the departments registry, and pin that no
station names a parent with no row), the web (`cd3fa679`: the tree
layout and the Exec page render the root), and the station-tree.md fold
(`c353bbc5`, written after both have landed).

### Station membership is read, not replayed

**A station answers who, where and when from its current state, and its
history from stamps the step record already holds** (design-doc "Station
membership is read, not replayed", `7b8ed079`, 2026-11-10; no open
questions). A packet is at a station only while one of its steps is: its
membership is the station queue read, `GET /api/stations/{id}/queue`, and
nothing else computes it. Flow is counted over each step's two stamps,
`became_ready_at` and `completed_at` (`boss_jobs::station_window`; the
`step-window-v1` basis of `GET /api/stations/flow` and the yard's
marshalling read): an arrival is a step that became ready at the station
in the window, a departure a step that completed there, and a skipped
step is neither; waiting is the current queue, and age is now minus the
oldest standing step's ready flip. Nothing replays the audit log to draw a
picture; the log stays the system of record, and `?basis=step-events`
keeps the ready/done event cube only as a separate unit, never as a
fallback. What is given up is the span of a packet that passed through a
station with no step addressed to it. If a reader ever needs a step's
station-at-the-time, the step records it at completion, as one column; no
station the map judges needs it today. Built: car 1 (backlog `95a9eca2`)
moved the readers, car 2 (`3db0db44`, train #1681, 2026-11-11) deleted the
~31k-line replay, its observation doors and its tables, and on 2026-11-12
the live yard read every station judged with `flow_as_of` equal to
`map_as_of` and all 38 flow rows on `step-window-v1`.

Decision history: the windowed packet-membership replay — its
conserved-original evidence contract (design `aa0f88c1`), witnessed-cut
bound (`c195e83e`), constant-outcome proof (`16fadf07`), witnessed commit
order (`ea650502`), lagging-relay prefix (`a0a5758b`), scope-bound
checkpoints (`8e7d8364`) and indexed-row generation (`6b00612f`), parked by
design `a3c5e58a` on 2026-11-08 — was retired with its code, its
observation doors and its tables by design `7b8ed079` (2026-11-10), under
which a station's membership is its queue read and its flow is a windowed
count over the step record's own timestamps. Design `a8463bec` ("Station
membership census", 2026-10-20, Q1 decided) belongs to that history:
it measured that `c195e83e`'s census over a 14-day terminal window meant
about 223k snapshot rows and 265 MB against the reader's 250k-row and
128 MiB bounds, so every station answered "exceeds row bound", and it
chose a trigger-maintained, insert-time index of each packet's latest
snapshot. It was never built, and it is superseded by `7b8ed079`: a read
that replays nothing needs no census.

### Every protocol names the station that owns it

**A protocol belongs to the station whose work it is, and its row names
that station** (design-doc "Every protocol names its owner station;
Engineering and Operations gain stage stations; Finance returns as a
department", `c0ae096f`, David 2026-11-10; no open questions). It is T6
of the station tree (`4b27a65a`) with its ownership decided from the
company frame. A workflow row's `metadata.owner` is the id of a
standing station, so moving a protocol is changing that one value, and
each step names where it runs (`at` in its `metadata_defaults`;
`under` dropped by design `305cf4ce`) or a `fulfilled_by` hand-off.
**Engineering** owns
changing the system (ship-a-change, gate-run, pr-train, agent-run,
design-doc and the other delivery and design protocols); **Operations**
owns running it (every `maintenance-*` row but the ledger two, nodes,
disks, credentials, recovery sheets, incident); **Finance** owns money
in and out (the two ledger rows); the root, `algedonic`, owns intake and
decisions (backlog-item, user-feedback, approval, publish-request,
passkey-promotion, department-retro, correct-the-record, work-session,
learning). The design's table decided each row; the rows and the pin
`every_platform_protocol_names_a_standing_owner_station`
(`crates/core/boss-jobs/tests/station_tree.rs`) are the record.

**Engineering and Operations hold stations for their stages, and Finance
is a department again.** Engineering, where most of the volume runs,
holds `design` (design docs, workflow design, protocol experiments;
`design-review` sits under it) and `review` (landed and held car
reviews; `landed-review` sits under it) beside build, gates,
loading-dock, probe-shed and forge. Operations holds `maintenance` for
the `maintenance-*` family; a child per family is added only when its
allocation differs. `finance` is a tier-one station under the root,
because the company now holds real money: it owns the company's money
as data (the Stripe balance, ledger account 1010; every purchase,
recorded by a human step; and the budget a review reads, available
equals balance minus committed spend). It never spends: purchases happen
outside BOSS, by David, and are recorded back. It hosts the Finance
pages, and its first open finding is backlog `5c1c435d` (account 1010's
-100 cent credit balance through 2026-10-31).

Where it stands on 2026-11-12 (origin/main `de93d1291`). **Landed:** the
stations — `finance`, `design`, `review`, `maintenance` and the
re-parenting of `design-review` and `landed-review` — car `aa478558`
(backlog `599fec3e`, train #1672, 2026-11-10), and `GET
/api/departments` answers a `finance` row; every one of the 79 platform
rows names an owner station (car `c5894471`, 2026-11-08, from T6's first
assignment); and T6 car B (`a44830d5`, train #1698) lets a step name its
station. **Pending:** (a) the twelve rows whose owner this table changed
— break-glass-enrolment, correct-the-record, enroll-a-managed-agent,
incident, maintenance-codebase-metrics, maintenance-protocol-drift, the
two ledger rows, protocol-experiment, protocol-retro, work-session and
workflow-design — ride car `db099d49`, held with a CHANGES verdict
(three passkey-protected rows lack hold files), and `publish-a-protocol`,
a held kind, moves at the platform owner's own publish; (b) only one
platform step names a station (gate-run's receipt step, at `gates`), and
sweep 2 places every step of engineering's protocols (backlog
`baac0b65`); (c) packets pinned to versions that predate the owners
stand at no tree station (595 of 727 measured on 2026-11-10) until the
cohort convert moves them (car `d59e29b5`, backlog `56b81861`); (d)
`metadata.department` stays beside the owner on 77 rows while
`GET /api/jobs?department=` reads it, until IT retires (`df0ecd8b`);
(e) the Finance page reading the balance and the budget from the ledger,
with the deposit step, is backlog `9d284874`, sharing the capacity
review's purchase step (`eea00a8b`) and balance read (`b77c52c1`).

### Claims stand at the station the tree resolves

**Every claim records the tree station it stands at, and the claim door
resolves it** (design-doc "Claims stand at stations: closing the station
tree open edges", `305cf4ce`, David 2026-11-12: "Work is supposed to sit
at the admission queue of a station now, not at a name or robot or
executor"). It closes the open edges of T6 (`4b27a65a`) and of the owners
above. Current truth:

- **A step names its station with one static `at = <station id>`, and
  `under` is dropped** (D2; Q1 approved). The protocol knows where its
  steps run; a tree station's predicate shrinks to tags, and a pin is to
  hold that none names a workflow kind.
- **The claim door resolves the station** (D1, D7): the step's `at`,
  else the deepest tree station whose queue holds the packet, and never
  an `a.*` or `q.*` name. A claim made for someone else judges the
  nominee against that station rather than refusing it. `boss dispatch
  --next` claims through `claim-next`. Resolution is a pure function of
  the tree at a sequence; the dispatcher only re-offers on release.
- **The record names a station by id** (D3): claim facts carry
  `_station_id`, and the name is a display copy until readers move.
- **One unit of build work holds one claim** (D4): the item's `build`
  step is `fulfilled_by` its agent-run, whose `building` step stands at
  `build` and draws from build's allocation, not the protocol owner's.
- **A station's `capability` (executor kinds, roles, model classes)
  decides whom it admits**, checked at the claim; a seat is pull order,
  not permission (D5). Publish refuses an address whose station cannot
  satisfy the step's contract, and at runtime a step no executor may
  take waits with `waiting_on {kind: executor}`.
- **One share function serves every pool** (D6), evaluated at the
  claim's station and walking up to the root. An allocation entry is
  `reserved` or `floating`, and a floating draw rises to the nearest
  ancestor. Station `capacity` is enforced as seats and `bays` retires
  into it; `model-slot:*` gives way to `robots:*` plus `budget:robots`;
  a run bills vCPU only when it binds a worker. Robot pools are
  allocated at the company root, and whether a robot floats company-wide
  or is seated in Engineering, which then sub-allocates, is a data
  choice: the share function expresses both, so it can be settled
  empirically (Q2). The root allocates `people:platform-admin`; a
  station that needs human work, such as an approval, declares a human
  resource slot drawn from it, and the operator's executor view orders
  his human steps across stations (Q3).
- **The generated stations retire in order** (D8): cohort convert, steps
  declare `at`, the door resolves from the tree, readers move,
  generation stops. The end state is pinned: every claim fact's station
  id is a declared active tree row.

Built as of 2026-11-12: none of it. `GET /api/claims` at 18:01Z held 97
claims (not truncated): none at a tree station, 76 at
`a.platform-admin.opus-5-5`, 11 at `q.platform-admin.task`, 10 with no
station, and 2 carrying an allocation. On origin/main `3f27bf5b4` no
claim fact writes `_station_id`, boss-cli calls no `claim-next`, and no
station row declares `capability`. In flight: car `541c8c8d` (parked)
records a tree station, by name, only for a step queued at none; sweep 2
(car `89c105cd`) places engineering's steps with `at`; the cohort
convert is car `d59e29b5`. The cars are backlog items, one each: the
door's resolution (`5662d4b6`, on top of `541c8c8d`); `claim-next` and
`_station_id` (`df5c2ee6`); one build claim and its vCPU bill
(`b5c815e4` part 2, with `db88f6a9`); capability (`21b0b946`); the share
function (`5da9495d`); the living docs and the invariant register,
riding with the first or second (`2864e177`); then `ed2a3e34` and
`2eb78346`. The measure is the share of claims at a declared tree
station.

Decision history: the website recorded the proposals for Q2 (robots
float on Engineering's share) and Q3 (human steps draw nothing) as
accepted, by mistake; David's answers, given in chat at about 17:50Z,
stand as corrections on the review step and are what D6 reads above.

**A step becomes Active only through a claim** (design-doc "A step
becomes Active only through a claim", `611fbffd`, clause 2 of backlog
`6ef4a36b`, David 2026-09-26, Q1 as proposed). Until then a step PUT of
`{status: active, assignee_id: X}` made X the holder of any Ready step
without X claiming it, and every surface's Start and the simulator's
workforce did exactly that, so the record could not tell "X took this
work" from "someone assigned X and started the clock". Current truth:
one door starts a step, `POST /api/jobs/{id}/steps/{step}/claim`; the
PUT refuses `active` naming that door, and a POSTed step cannot be born
Active (backlog `e639899a`). Both former doors reserved through one
transition function, so the move cost no Schedule hold, and the claim
door is now its one caller. A claimant claims for itself; only the
executor the pinned protocol's step declares, or a holder of the
`step-assign` authority (platform-admin today), may claim for someone
else, and that claim records both (`claimed_by`, `claimed_for`). The
executor is read from the Workflow row the packet is pinned to, never
from step metadata a writer can PATCH. Built: trains #721 (2026-09-26)
and #752; the web kit's Start and boss-sim's workforce call the claim
door; `a_step_becomes_active_through_a_claim.rs` pins it.

**A station pull validates against its owners inside the claim's own
transaction, and refuses an owner it cannot fence** (design-doc
"Selection validation keeps its owner through the claim commit",
`4e61511c`, opened by agent-codex and answered by David 2026-10-04, both
questions as proposed). Design `f31d94ac` made the selection and its
claim one transaction. That left the actor, policy, calendar and
run-count facts the claim checks, which a second HTTP read, a copied hash
or a matching DSN cannot hold still until COMMIT. Decided: (1) only an
owner adapter that joins the selector's native transaction, and proves it
fences the same store, validates a fresh selection. A remote or
unsupported owner is refused and the specific-step claim door is
unchanged. A distributed prepare/commit must declare its actor,
revocation and crash semantics before it is built. (2) Each owner
declares its linearization instant, its clock and expiry inputs and its
runtime-mode boundary. A decision lease would be a separate authority
decision.

Built as source by car `1ef711ce` (train #1135, 2026-10-13), in
`crates/core/boss-jobs/src/native_selection_store.rs`:

- The receipt copies the agent, alias, automation, rule and override rows
  the claim consumed.
- The claim's transaction share-locks `agents`, `actor_aliases`,
  `automation_actors`, `policy_rules` and `policy_user_overrides`, and
  re-judges the grant and its expiry under those locks.
- A calendar hold is judged in the same transaction.
- The hour's budget stays advisory: it reads `unknown`, never zero.
- The ingress fixes the machine mode and a five-second pre-commit budget.
- The same-store proof is the constructor's own identity, not a DSN.
- `PgJobs` without that store refuses a fresh selection.

No roster read sits on the claim path, so no roster fence was owed.

**The caller's identity and the machine mode are fixed at ingress and
last until COMMIT dispatch, inside that one five-second budget**
(design-doc "Define native selector identity and mode lifetime at commit
dispatch", `ce447ce3`, opened by agent-codex and answered by David
2026-10-05, both questions as proposed). Neither is a writer fence. A
credential or session retired, or a mode moved, after ingress governs
later requests and cancels no admitted one. Expiry before dispatch rolls
back and never sends COMMIT; a COMMIT already dispatched is reconciled
from its original receipt, never cancelled or retried under a new
request id. Actor aliases, policy rules, scope and expiry are still
locked and judged fresh at commit, and the mode's LAN ConfigMap rollback
stays independent of the jobs API. Built by the same car, `1ef711ce`:
`AuthenticatedSelectionIngress` carries the resolved actor and observed
mode, `SELECTION_PRE_COMMIT_BUDGET` is the five seconds, and
`a_mode_move_changes_new_ingress_without_rewriting_the_admitted_operation`
and `commit_completion_is_not_canceled_by_the_pre_dispatch_sql_budget`
pin it. This settles the caller and mode halves of the writer-fence
condition design `9c554429` places on the wiring, `1f04d9c6`; the
policy half stands.

Not live: the production jobs API builds `PgJobs::with_urls`, which holds
no native store. On the running build (`b92a4399d`, 2026-11-13), a
claim-next that finds a head therefore answers 409 "native selection store
is not configured"; this was read from source, not exercised. The pulls
this section and `506e0a1c` build on (`df5c2ee6`, `dbb9c722`) need two
things first: the lock-order fix `f4479975`, then the wiring, item
`1f04d9c6`.

**The same store is one constructor's handle, and the serving
composition stays refused until its policy-mode owner fences writers**
(design-doc "Bind native station selection to its actual store and
runtime authority owners", `9c554429`, opened by agent-codex and answered
by David 2026-10-05, both questions as proposed). It named the two
owners `4e61511c` left open. (1) One private `NativeSelectionStore`,
built by a constructor, is shared by the selector and every owner
adapter in one transaction. Its identity cannot be deserialized or
derived from a DSN, so a second handle over the same pool, or matching
rows, is refused. A cross-process owner observation would first need a
declared generic record with its own authority, never a decision id
taken from `policy_rule_audit`. (2) The serving composition, with its
mounted role mode and remote policy, roster and calendar clients, stays
refused for a fresh selection until its policy, caller and mode owner
fences writers. Authority is not read off SQL rows, a cached mode, a
file hash or an open descriptor; a move to a native mode authority is
its own reviewed activation. Built: (1), by car `1ef711ce` (the
constructor-identity bullet above), pinned by
`actual_shared_constructor_joins_one_transaction_but_same_pool_new_handle_refuses`;
no cross-process record exists. (2) holds today only because nothing
serves the store. It is therefore a condition on the wiring, `1f04d9c6`,
and not a car of its own: before that item serves the store it must say
whether the policy rows read under share locks and the machine mode the
ingress fixes are that fence, or whether the activation needs one.

### IT retires as a department

**IT is not a department; its work belongs to the stations that do it
now** (design-doc "IT retires as a department: its work now lives in
Engineering, Operations and the company", `df0ecd8b`, David 2026-11-10:
"we have broken up its old responsibilities into Engineering and
Operations"; no open questions). `it` is not a tier-one station and gets
no row. Engineering builds, gates and ships changes; Operations runs the
estate, deploys and incidents; the root, `algedonic`, hosts the
company's own intake and decisions. Each IT surface moves to a host
station (design `deba3000`) by this table, any line of which David may
amend:

| old IT surface | host station |
|---|---|
| backlog, receiving, feedback, tree | the root, `algedonic` |
| design | the root for decided designs, engineering for design work |
| yard, marshalling, step-plugins, registry, experiments, agents, crew | engineering |
| estate, sensors, monitoring, credentials, incidents | operations |
| metrics, subjects | the root, until a station's executors claim them |

Pages move a few per car, usually when a car touches the page anyway.
Each move deletes the `/it` route it replaces, with no redirect shim
before 1.0.0, and the IT sidebar group goes when it is empty. The order
of retirement follows the readers. A row's `department = "it"` leaves
it when the row moves to its owner station, and a sweep finishes the
rest once `GET /api/jobs?department=` reads owner ancestry. The
departments registry's `it` row goes only after that, because a station
row's `department` is validated against that registry
(`validate_department` in `stations.rs`). `docs/design/station-tree.md`
changes last, once the rest is built. Until then, the 2026-09-25 roster
under design `8c3e9599`, which made `it` the department with engineering
and operations as its two halves, is history.

Not built as of 2026-11-12 (origin/main `50f9be981`; the live registry
agrees): 13 station rows and 75 workflow rows under `infra/platform/`
still carry `department = "it"`; `GET /api/departments` still answers
an `it` row, declared through `department/declare.rs`; `nav-catalog.ts`
still spells 22 `/it` entries over 19 page folders under
`apps/web/src/it/`. The cars are backlog items, one each: the rows and
the department reader (`8b144a6e`), after the T6 owner sweep
(`baac0b65`); the registry's `it` row (`aaf4ca88`); the page moves
(`92bfcb04`), which use the host field (`03fb75c8`) and the derived URL
(`0bc2eb68`) of design `deba3000`; and the station-tree.md fold
(`0378b763`).

### Station alarms state the action the factory needs

Design `5da17ace-7e35-4eae-a7c6-20e063c1791a`, settled with no open
questions on 2026-10-14 from David's coordination-session instruction,
answers urgent `df8fe965-8d0c-407b-a47b-532b5da20c0e`. **Red means
immediate action required.** An urgent packet waiting for proof or a
clean window can remain open without making its station red. Severity
describes the present station condition; job priority is separate.

Keep the four existing wire states, with no fifth waiting state:
**Clear** and **Full** retain their healthy-flow meanings; **Attention**
is yellow, an understood wait or warning that needs watching;
**Troubled** is red, a specific fault, an overdue or unowned action, or
a required reading that cannot be taken. These meanings refine the
earlier map decision's three displayed state words; retaining Full
does not restore the retired busy vocabulary.

A qualified wait identifies its packet, actual owner, required
condition, supporting evidence and next observable check. A world
event needs a working observer. A clean window needs its recorded
beginning, requested duration and coverage. A failed proof is a fault;
a green gate is neither deployment nor runtime proof. No boolean
waiting flag may exempt a packet from a genuine fault.

Use structured `waits_on`, `owned_wait`, `proof_attempt` and registered
step claims wherever they already hold the facts. Internal gate and
review waits derive from their actual registered run, exact head and
recorded bound. Missing owner or bound belongs in an extension to the
appropriate versioned workflow or station contract, written through
its audited door. Do not derive an exemption by parsing hold prose.
Observe the next check when due: a failed observer, missed check,
missing owner or satisfied condition without subsequent action names
work to do now. A finished run with an overdue handback is such work;
an active exact-head review inside its bound is an expected wait.

The supported proof observer distinguishes a negative reading from a
failed read: exit 0 means the condition is seen; exit 1 qualifies as
unseen only when its executed stdout is JSON containing
`"observation":"unseen"`. The same JSON may carry clean-window coverage.
Other exits, missing tools, refused execution and legacy nonzero outputs
remain unread. Existing records are not relabeled or backfilled. The
proof attempt retains the complete observer stdout, stderr and any actual
process exit separately from admission, environment and missing-tool
diagnoses; a refusal invents no process streams. The shared wait judge
consumes this outcome before qualifying a world or named actor's wait.

Preserve the worst genuine finding in mixed fault-and-wait membership.
Healthy activity remains healthy. An external event not yet observed
inside its recorded wait is not a failed probe. A scheduled proof or
covered clean window is not overdue before its actual deadline.

Implementation remains separate work. The accepted plan inventories
each Troubled band with its packet, source, action owner and observable
clearance condition (urgent `a4de1e72`), repairs measured placement and
intake faults first, then shares one pure fault/wait classification
across shed, garage, marshalling and their border and alert consumers.
Owner, condition and check evidence travel in the payload so the UI
and alarms read the same judgment. Registry contracts extend only for
information that the existing records lack.

Each actual misclassification requires failing-first tests with paired
controls: an observed event wait, a covered window before its deadline,
an active exact-head review, a missing observer, overdue internal work,
a failed proof, unknown input and mixed membership. Clear/Full healthy
controls and Attention/Troubled styling remain distinct. Source is
gated and independently reviewed under existing trust rules; deployment
must then be observed in fresh native region and alert output. An alarm
clears when its tested predicate clears, while an open urgent packet
keeps its owner until its protocol actually finishes.

Undeclared moves remain engineering findings: urgent `814e6c66`
records the measured non-packet sensor and lost-carriage cause; old
`c8d22e63` is the distinct historical mover-provenance obligation.
Repair and prove the transitions rather than colouring them yellow.
Retain immutable history and record any correction or supersession
explicitly. Overdue retros require real department evidence; an
inapplicable generated retro requires a truthful supported outcome
and a fix to the registered producer. The existing fourteen-day and
three-day intake thresholds remain until separately measured evidence
justifies changing them.

**The undeclared band counts only the current mover's readings** (David,
2026-10-17, on `c8d22e63`; supersedes in part the coordination rule
below). Every `yard_moves` row carries `mover_version`, a content
address of the source that decides placement (`moves.rs` and
`regions/partition.rs`, computed like the diagnosis model, never bumped
by hand), and `crossings` — what the band and the routes' observed
counts read — counts only rows the deployed version wrote. So the
deploy that carries a mover fix clears what its predecessor misread,
instead of leaving a region TROUBLED for the 24h window. Nothing is
rewritten or deleted: superseded rows, and rows written before the
stamp (`NULL`, unknown), stay in the record and on `/api/yard/moves`
naming their mover, and the correction ledger below still governs a
current mover's row. The price is that any edit to either file restarts
the band's window; a genuine undeclared route is re-observed the next
time a packet takes it, while a misreading could only ever age out.

**Among the current mover's rows, a historical finding is superseded only by an exact diagnosis**
(operational design `a34bc69e`, 2026-10-14, answers `c8d22e63`). This
conservative coordination decision follows the already authorized alarm
repairs and correctness rules; it does not claim a separate approval
from David. A producer SHA, reader-model version, deployment or changed
route map never clears findings — except the mover's own version, as
David decided above. Originals and their audit causes stay
immutable. Genuine historical faults recorded by the current mover, and
missing proof, remain loud; a row written by a superseded mover version
stops counting toward the band on the deploy that supersedes it, though
it stays in the raw record (David's decision above).

Among the current mover's rows, only a falsely interpreted crossing may
stop driving the current fault,
after an append-only diagnosis binds its exact sequence, packet, audit
cause and canonical original-record hash to the corrective artifact and
an independently recorded machine-run proof. Resolve that proof from the
record: require actual completion, successful execution, exact observed
artifact, readable context and complete structured output naming those
originals. Pasted output, missing tools, an override, clipped output,
unrelated proof and a later correct crossing do not prove the past
reading false. One invalid original refuses the entire bounded batch.

Record the admitted diagnosis as an audit event and derive its projection
from that event. Copy the validated original bindings and proof into the
record so replay does not consult a changed reference, rerun a live probe
or guess past placement from today's map. Retries are idempotent;
withdrawals append records; unresolved conflicts remain visible. Use the
existing packet update policy and exact packet/proof scope, with no new
alarm-muting privilege or bypass.

Raw reads retain every move. Give readers the separate correction and its
evidence; partition counts into active, diagnosed-superseded and unknown
or conflicted dispositions, conserving the raw total. Rows written by a
superseded mover version are a fourth class: held in the raw record and
the conserved total, excluded from the band. Among the current mover's
rows, only a validated, unwithdrawn false reading leaves the fault count. Never rewrite its
original declaration as true or infer missing dispatch context. The
workflow version retained by `e95433aa` is dispatch provenance, not a
reader version. No legacy row is automatically superseded in the ledger.

**A historical placement reading is judged only against witnesses the
record kept** (design `ef8f9488`, David 2026-10-16, both questions
accepted as proposed; answers `c8d22e63`). Q1: the v2 handoff model
(`registered-agent-handoff-v2`) and its cause cutoff stay as they are,
and new evidence is never fed into it as invented historical state. A
separately named observed-placement model may judge a crossing by the
map's own read only when the original successful before and after read
witnesses and their snapshot bounds were conserved. A later
repeatable-read observation cannot stand in for an old witness, and
where either witness is missing the reading stays UNKNOWN. So original
13538, admitted after its cause, stays UNKNOWN: that ordering proves
neither a phantom move nor an invalid dispatch. Had cause-cutoff-only
semantics been chosen instead, such pre-admission cases would stay
uncorrectable; v2 is never widened to reach them. Q2: reading a
recorded admission observation needs less authority than creating one.
The GET on `/api/yard/moves/{seq}/diagnoses/{diagnosis}/admission-observations/{request}`
asks for a named actor and the whole-map ReadJob scope the moves record
already uses (`move_admission.rs::get`, pinned by
`exact_recorded_admission_is_readable_without_correction_authority`);
the POST that creates one keeps the correction authority. No role, grant
or credential was added. Q2 is built; Q1's model is not. No
observed-placement or `historical-placement-snapshot-v1` evaluator
exists, and `move_diagnosis.rs` still supports only the v2 handoff
family. The design's next step was a witness inventory of the 41 legacy
rows outside v2. It was never run: `c8d22e63` closed through the
version stamp instead (David, 2026-10-17, above). Those rows predate
the stamp and no longer count toward the band, so no evaluator is owed
for them. Q1 binds any future historical model; none is pending.

The ledger's typed validation, immutable projection and admission
observation landed under `c8d22e63`. Its further native-proof
acquisition (branch `feat/acknowledge-exact-historical-evidence-loss`)
was set aside by David on 2026-10-17 in favour of the version stamp
above, which is how `c8d22e63` closes.

That branch was building design `84ae79ae` (David, 2026-10-16), which
accepted an append-only "acknowledged historical evidence loss"
disposition for exactly the 42 legacy UNKNOWN crossings whose original
before/after reads the mover never retained: bound to that one manifest
by a human authorization, never meaning corrected or genuine, and
shown as a historical warning only after native proof of the repaired
mover, a capturing producer and a clean window. It was never built: no
acknowledgement disposition exists on main. The stamp made it
unnecessary. Those 42 rows predate the stamp, so their `mover_version`
is unknown and they no longer count toward the band, while they stay
in the raw record unchanged. The decision admitted no reuse for later
incidents, so any future evidence-loss disposition needs a decision of
its own.

This fold records accepted meaning, not an implemented alarm classifier
or runtime clearance. Urgent `df8fe965` stays open for implementation
and proof. No alarm is muted, no red threshold is relaxed to meet a
colour target, and no private, provider, credential or activation
authority is granted.

**The HUD frame answers about the whole system, one fixed row per third;
the map answers about the parts** (design `00774ca8`, David 2026-09-24,
recorded as decisions with no open questions; answers backlog
`c1253e50`, the frame design `17567423` left open). Measured that
morning: the only whole-system figure on `/it` was
`borders.ts::summaryLine`, a client sum of border rows that
double-counted about 260 backlog items, and marshalling's own trend
(68/day) disagreed with its border (237/day) — two per-place numbers
under similar names. Decided: (1) **the thirds are a partition held by
the server** — queue management = receiving, marshalling; actors
building = shop floor, gates, garage; delivery = dock, track, arrivals,
shed, publish — so `THIRDS` holds every region once and the web
hardcodes no row; (2) **each row carries two figures no territory can
answer**: a **balance** (net per day, then its in and out rates in ONE
unit per third, each packet counted once, closes inside the third
counted as out) and **stuck · waiting** exactly as `ThirdStuck` gives
them, never summed; (3) **machines are one whole-system cell** beside
the rows — failed and unjudged against a total, each listed machine
linking to its map — not split by third, since the plant strip serves
every region; (4) **arrivals per day is the one stated exception** to
"no territory number in the HUD", read from the same `arrivals.trend`
field the arrivals territory reads, so one field rendered twice cannot
drift; (5) **unknown keeps its three pictures** — `≥ n ?` for a floor,
`?` in a dashed housing for an unread input, a plain `0` only for a true
zero — and a failed read turns every cell `?` under "read failed HH:MMZ
· last good HH:MMZ", never leaving the last values on screen as current;
(6) **the frame is fixed**, does not follow the zoom, carries the read's
age and window, and reserves the height of one contextual strip for the
current selection (a clicked border or machine at `/it`, the region's
in and out rails at `/it/yard/<region>`); (7) **a band above the map**
on desktop, Enamel plates on tokens, *troubled* only for a stuck count
above zero or a failed machine, no colour on balance until a band is
declared, **no lamp for a row as a whole** (a worst-of hides which
figure moved); on a phone each row becomes a group header of the strip
map; (8) it supersedes the CONTENT of `62de32ae` decision 10's one-line
summary, not its place; (9) **one server read**: a `thirds` block and a
`machines` summary on `/api/yard/regions`, printed by `boss orient`, and
no client adds anything up; (10) it retires `summaryLine` and adds no
route. Built in one car (train #632, 2026-09-24): `boss_jobs::thirds`
(`BALANCE`, `MachineSummary`, pinned by
`every_region_stands_in_exactly_one_third` and
`a_thirds_two_edges_count_one_unit`), `boss orient`'s THIRDS and
MACHINES lines, and `HudFrame.svelte` over `hud.ts`, mounted on every
map page, whose `hud.test.ts` pins the payload's row order,
`arrivals.trend`, the four pictures and the retired border sum. Not
built: the contextual strip is reserved but empty, because `MapPage`
passes it nothing — a region's rails still render under its map — and
on a phone the frame stacks its rows above the strip map, which groups
under the server's thirds but does not yet use the HUD rows as its
headers.

**An alarm is its own packet kind, owned end to end by the observer that
raised it.** Design `97cb3a87` (2026-11-01, engineering decisions, no
questions) gives alarms a platform kind `alarm`: `raised` (observer,
condition, subject, evidence, severity), a `watching` step the observer
re-completes on each observation (`last_seen`, `count`, `state` alarming,
warning or silenced), and terminals `resolved` (by the observer, when the
condition clears), `stale` (by the silence sweep, when the observer stops
reporting) and `escalated` (files the repair and nominates the person the
condition names). There is **one open alarm per (observer, condition,
subject)**: a re-observation updates `watching`, so one dark episode is one
packet with a count. The observer that raised an alarm resolves it. Filing
returns the id or fails loudly on the observer's own packet. Silence is a
relation (`silenced_by = <packet>`, for a maintenance window or a declared
quiet): the alarm still exists and counts, and nothing decides not to
file. Severity on the boards reads `state`, so a factory whose open alarms
are all `warning` is not red. Once the kind lands, the backlog-item door
refuses `input_channel = telemetry/monitoring`: an alarm is not intake.

**Registry drift is a fact observed by one comparator and carried by an
alarm.** Design `aeb6e66a` (2026-11-01, no questions):
`boss_jobs::registry::compare(tree_row, live_row)` is the one comparator,
answering `Same`, `Differs {fields}` or `Unreadable {why}` (a crash is
unreadable with a distinct exit code, never a diff), called by the publish
verb, the lint, the drift sweep, the experiment door and `publish-drift
--check`. Whoever sees drift writes `registry.drift.observed`, carried by
an `alarm` packet (97cb3a87) with one open per (kind, direction); the
comparator that next finds `Same` resolves it, and a publish that lands the
named fields writes `retired_by` on it in the same act. Every publish
records `published_from = tree_sha`, and a publish over a live version the
tree's history never contained refuses, naming the live edit, unless
`--take` is given and recorded, so which side is ahead is a read. A held
kind's hold text is rendered from the open drift fact, never written by
hand.

**Live registry content is authoritative after bootstrap; git holds its
recovery export.** Design-doc *Five further collapses of duplicated
factory facts and packet handoffs* (`942dc0ac`, Q2, David 2026-11-08)
found workflow and pool rows acting as current truth in two places, the
append-only live registry and the mutable `infra/platform/workflows/`
and `pools/` files, reconciled by a comparator walking git history and
a planned write-back of live edits into the tree. Decided: after
bootstrap the registry's append-only row, version and publication event
are the authority, and packets keep the version they were admitted
under. Git holds the initial tenant data and a generated, versioned
recovery export naming its source version and hash, never a competing
live edit; a desired tree edit becomes a signed row proposal through the
registry road, not an implicit republish on converge. This replaces the
bidirectional write-back strategy (the pool write-back item `add6ac6e`
was declined for it), and it is conditional: an offline restore from
the export must be tested and a deliberate import road must exist
before the drift loop and its history arbitration retire. Validation,
signed snapshots, approval holds, append-only versions and read-back
stay. Until then the comparator above is the live mechanism (item
`c26aacaa`, in build when this was folded on 2026-11-12).

### Claims and reservations are the factory's state

**The factory at any instant is its table of open claims.** Design
`a236103a` (David, 2026-10-31; all six questions accepted as proposed)
reframes the operations console: an actor holding a step of a packet,
under a pinned protocol version, at a station, since a time, with what it
reserved. Every console reading (queue depth, utilisation, burn, who is
where, what is stuck) is an aggregation of that table joined to the
record, so the console is derived, not instrumented. A robot exists only
while it holds a step, so "which robot is busy" has no answer; the scarce
things are human attention, money per verified unit, time and, only when a
pool runs dry, compute. Resources are reserved and released as part of
step claim and completion; stations carry no tolerances of their own (the
protocol does, and the machinery is injected).

The data half: a step declares a **bill of resources** (absent means zero);
**pools are a registry** (`infra/platform/pools/`), each naming a capacity
and, for a stock, its reclaimer; the claim door **reserves the step's
vector in the claim transaction** or refuses with the first failing
dimension recorded as the step's `waiting_on`; every exit **releases**, and
a reservation carries a **lease** (`claimed_at + duration_hours × 2`);
`GET /api/claims` answers open claims, reservations and the server-computed
**binding** constraint, and the client adds nothing up. The six decisions:

- **Model slots are both an actor property and a pool.** The actor row
  (`agents.max_concurrent_runs`) bounds an identity, the pool row bounds
  the class, and a claim passes both. Provider rate limits are observed,
  never declared, and drawn as headroom. *Superseded by the station tree
  (design `a3c5e58a`, 2026-11-08):* the per-actor cap is retired (item
  3e927996, 2026-11-15) now that `robots:<model>` pools carry concurrency,
  so one mechanism bounds a robot. The column survives as a stored
  declaration nothing judges, until the tenant seed stops writing it.
- **A stock orphan is a row the reclaimer reads**, not a step on the
  packet that orphaned it: the orphan outlives the packet. The packet
  records that it orphaned; the pool records who discharges.
- **A human station reserves a person** through the same
  `jobs.step.reserved` event, against a people pool whose capacity comes
  from the roster, never from presence.
- Decision history: `as_of` over a materialised claims history, and the
  review screen that replays an incident from it (P6, console car 7),
  was retired from current truth by design `a3c5e58a` on 2026-11-08; the
  console keeps the current claims read (`/api/claims`), the alarm
  packet is the incident record, and car 7 is not built.
- **The IT world stays IT's landing.** The factory view is `CompanyMap`
  with roll-ups at its own route and IT one card on it: absorb an API,
  federate a page. *Superseded by the station tree (design `a3c5e58a`,
  2026-11-08):* there is no separate IT landing beside a `CompanyMap`
  factory view; the landing is the algedonic station page (design
  `c4854fd4` car 2).
- **The transit map retires behind its flight** only once the floor draws
  every station and route it draws today, keeping live motion's real-moves
  layer. The station tree's T7 sharpens the condition: not until the new
  floor shows jobs moving.

Superseded by the station tree (design 4b27a65a, 2026-11-06; decided
for this design by `a3c5e58a`, 2026-11-08): pools were "factory-level
only" here, and are now allocations drawn from a parent station; in v2
a station is defined by its allocation and every pool is held by a
station drawn from its parent, and
the step's `resources` block becomes a station spec whose resolution
replaces `waiting_on` with the named constraint.

**A claim, its reservations and its refusal are one transaction.** Design
`acd41b4c` (item b6cf3513), its one question accepted as proposed on
2026-11-01, is the trust boundary for the console's reserve/release door
(a236103a D3/D4). The resource transition joins the jobs repository's own
write transaction, not a second service: under locks taken in one
documented order (actor admission owner, pool rows, step generation) the
claim rechecks pool versions, roster facts, the run cap, live
reservations and outstanding stock obligations, then either CASes the step
and writes the whole vector, lease and `jobs.step.reserved` facts in the
same commit, or commits a bounded `waiting_on` refusal naming the first
failing dimension in stable registry order and acquires nothing. Unknown
capacity is never free: positive demand against it refuses as
capacity-unknown. Quantities are exact decimals. A retry returns the
original acquisition. Every real exit releases through the same
transition, compared on claim id, generation and holder, so a stale
completion never releases a newer claimant's resources.

The boundary's rules: **leases are generation-fenced with no implicit
renewal** and grant no process-kill authority, and a logical release is
not evidence that a pod stopped consuming CPU or RAM; **stock stays
charged until the named reclaimer discharges it** with verified machine
evidence (a server-side `ResourceReturnVerifier`, never a caller's
`returned=true`); **people capacity comes from a versioned, qualified
roster witness**, never presence or the absence of an agent row; money
stays advisory and the actor cap keeps counting OPEN agent-run packets
(*superseded by the station tree*, design `a3c5e58a`, 2026-11-08: the cap
is retired now that `robots:<model>` pools carry concurrency, item 3e927996);
absent amounts stay zero and in-flight packets keep their pinned
protocols. Billed protocol versions are enabled only after their
capacities and evidence adapters are qualified. `jobs.step.reserved` and
`released` are replay inputs, so a rebuild recreates the same rows and
totals. The invariant is live reservations plus undischarged stock
≤ each qualified capacity, never an assertion over an UNKNOWN denominator.
The decision approved this boundary for engineering review and
implementation, not credentials, cleanup authority or production
activation.

**Three decided designs that pulled against the 2026-11-07/08 steers are
retired, parked or superseded** (design `a3c5e58a`, David 2026-11-08,
all three questions decided as proposed). (1) **The console's history
scrub is retired**: `as_of` over a materialised claims history and the
review screen that replays an incident (a236103a Q4, P6 car 7) leave
current truth, because they are history replay; the console keeps the
current claims read and the alarm packet is the incident record.
(2) **Membership replay**: parked here, then retired outright by design
`7b8ed079` (§Station membership is read, not replayed). (3) **Factory-level-only
pools, the separate IT landing with `CompanyMap` as the factory view,
and the per-actor run cap** (a236103a D2 and Q5, acd41b4c, d996ce3d) are
superseded by the station tree: pools are allocations drawn from a
parent station, the landing is the algedonic station page (design
c4854fd4 car 2, item 72d58e00), and the per-actor cap is retired now
that `robots:<model>` pools carry concurrency (T5, item 3e927996), so one
mechanism bounds a robot.

**Work waiting for capacity stands in line at the station that holds the
capacity** (design-doc "A gate-run waiting for a bay stands in line at
the gates", `85c3f793`, 2026-09-28, no open questions; backlog
`357fe1da`). A queued gate-run was claimed by no yard region, so the map
dropped it while the gates' own headline counted it. Decided and built
(train #1052, 2026-10-10): queued gate-runs are placed at the gates in
queue order; the line is a second figure, `waiting`, beside the bays in
use and never counted against them, so three behind four full bays never
reads "7 of 4"; the region's `queue wait` is the median of `launched_at
- opened_at`, with a run that died in line counted `lost in line` (since
`bce67f09`, only one that took a place); `queued_at` is the waiting
marker, cleared at launch and never kept as history; and the
shop-floor → garage hand-off row went, because a run that dies in line
now leaves from the gates. In station-tree words a gate bay is the
`gates` station's capacity.

**A waiting claim records only its changes, and its liveness is read
off the clock rule that keeps it** (design-doc "The dock bay claim is
written on change and read alive off the refresh rule last firing",
`38f3a488`, 2026-09-26, no open questions; backlog `b15b0f4e`; follows
D3 of `42279fb2`). A dock car waiting for a gate bay re-stamped
`regate_waiting.at` on every two-minute pass, one audit event each: 7
waiting cars would have written 210 events an hour that said only "still
waiting". The rule is David's (2026-09-16): the audit log records work,
and a liveness heartbeat is telemetry. Decided and built (train #696,
2026-09-26): (D1) the claim is `{main, since}`, written when the car
starts waiting on a main and deleted when it stops, so at most two
events per car per main; (D2) builders count a dock claim only while the
`train-dock-refresh` rule's last firing (`GET
/api/cadence/rules/train-dock-refresh/last-firing`) is inside 300 s with
`rc` null or 0, and a failed or stale read counts no claims; (D3) a
refresh held by an occupied track withdraws the claims rather than let
them stand through a transit, since amended to keep one, which keeps
first place when the track clears (backlog `6990037a`); (D4) the
conductor merges metadata through `PATCH /api/jobs/{id}/metadata`, not
a read-modify-PUT. A separate sensor table was rejected: the cadence
registry already holds the timestamp. In station-tree words this is a
claim waiting on the `gates` station's capacity, and the dock's `bays`
is its v1 spelling.

### Every remnant leaves with its owner

**A remnant is an allocation, not stock.** Design `777016f1` (David,
2026-11-09; its one question settled as proposed) applies the station
rule of 2026-11-08 (a station finishes its work and sends the job on,
releasing its allocations) to what agent-run v14 did not yet release.
The station that makes a remnant owns it and releases it when the job
departs, and the release is recorded on the job, so a release that
fails is a red record someone reads rather than a restart count. The
owners: the **worktree** is the build step's; the **worker volume** and
the run's **launch refs and scratch targets** are the run's; the
**forge branch** is the car's, released when it lands or is retired.
Stock is only what no step owns: disk as capacity, credentials, shared
build caches. A reclaimer survives only as a safety net for orphans
whose owner packet is gone: an automaton reacting to the record (packet
closed, remnant present), not a timer, and each orphan it takes is
filed as a leak against the station that made it. The dev pod's reclaim
script (`infra/cluster/dev-scratch-reclaim.sh`) retires into rows, one
car per job it does (a dispatcher rule or a cadence verb each), and is
deleted when it holds nothing. Priority: standard consolidation work
beside the station-flow cars, worktree release first, urgent only where
a remnant is filling a disk.

**Car 1: the worktree** (backlog `f8d2bffc`). Measured 2026-11-09: 727
worktrees, 622 locked (317 by the launchers' `git worktree add --lock`,
whose reason is git's own `added with --lock`; 32 by the agent harness,
naming a parent session that outlives the run by days), and /work at 80%.
`boss dispatch --report` releases the tree its run's record names
(`gate_run.worktree` on `verdict` or `building`, else the
`worker_session.local_root` its worker was bound from) once the run has
departed (`gated`, `delivered`, `refused` or `died`), and writes
`worktree_release` on the run: the bytes, or why it was kept, or the
refusal. It releases at the run's close and not at the hand-off, which
the design's table also names: at the hand-off the builder still stands
in the tree (`boss gate --wait` runs there and the report follows), and
a run awaiting its verdict has not departed. The release never discards
work. It keeps, with the reason on the run, a tree that is the main
checkout, names no branch or gated sha, has moved to another branch,
holds any uncommitted or untracked path (enumerated explicitly despite
user configuration; ignored build output does not count), has a head that
is on no forge branch and is neither the gated sha nor carried by it
(`git cherry` only after excluding unmatched merges, whose resolutions
patch ids omit), or carries a lock other than git's `added with --lock`.
Harness locks always keep the tree: they belong to a session across runs,
and one departed run cannot release that session's working directory.
A green nobody
reported after stays until the orphan reclaimer (car 3) reads it.

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
  the Workflow; validation is the union, so single-use vocabulary
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
exactly the two platform-pinned rows, `workflow-publish` and
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

**A stamp dies when the shape it signed leaves the step** (design
`87329a13`, David 2026-09-25, option C; answers backlog `c085256d`,
found in the round-4 review of car 7abc0154). "Current-shape stamp"
used to mean only `shape_hash == current`, so an edit that went A to
B and back to A revived the stamp on A: after a withdrawal, anyone who
could write the step's metadata could restore the approved shape from
the audit log, and the old stamp satisfied the server and the ops
runner again. There are two ways to withdraw and only one writes a
stamp (Reject stamps the new shape; Request changes saves the decision
and writes none), so a rule keyed on a LATER stamp — the runner alone
(A), or the latest stamp per role (B) — misses the second; dropping
superseded stamps (D) deletes the provenance above. Decided: (1)
**the edit that changes a stamped step's shape voids every stamp on
the old shape, permanently, in the edit's own transaction** — the
stamp stays in `sign_offs`, annotated `voided_at` / `voided_by_event`,
and a stamp counts only when it is not voided AND its `shape_hash` is
the current shape; (2) **one predicate** — `Step::live_stamps()` — is
read by `sign_offs_satisfied`, the completion 409's
`missing_or_stale_roles`, and the sign-offs POST's idempotent-re-stamp
check, so a re-sign of a restored shape is a fresh stamp and not an
"idempotent" answer; (3) **`jobs.step.stamps_invalidated` is
load-bearing**: it names the voided stamps (role, authority,
`stamped_at`, `shape_hash`), the rebuild applies it, and the metadata
merge door writes it in the edit's transaction rather than best-effort
— existing rows are re-judged by replaying the events already in the
log, and a merge-door event the old best-effort path lost cannot be
recovered; (4) it holds for **session stamps as well as presence
ones** — the cost is one re-tap after a reverted edit; (5) **the ops
runner mirrors it**: `verify_approval`'s `$bound` excludes a stamp
carrying `voided_at`, and, as defence in depth, refuses when a named
approver's latest stamp for the role is on a shape other than the
current one — it reads only server-minted fields, because the runner
judges the record independently of the server (`17835005`). The build
is test-first at both judges (an A-B-A through Request changes and
through Reject answers the completion 409 naming the role, a rebuild
reproduces the voids, and `ops_runner_approval_sh` refuses the A-B-A
job), and, as a trust-boundary car, is held for an adversarial review
before it boards. Out of scope: replaying a presence nonce within its
ticket's life (`3977b3d2`) and the per-key writer rule (`6c9183de`).
Design `f623e425` reserves the runner's keys to `runner:ops` and
`decision` and `comment` to `signer`, a holder of a required sign-off
role through the gateway session. That restriction narrows who may
edit the shape; stamp voiding still prevents a signer's A-B-A edit
from reviving an earlier stamp.

**A presence ticket stamps its step once** (`3977b3d2`, 2026-09-26).
The void above left one replay: stamp on A, the content moves to B
(the stamp dies), the content returns to A, and the original ticket —
same step, person and shape, still inside its 120 s — wrote a fresh
live stamp no passkey touched. `append_sign_off` now refuses, 422 at the
sign-off door, a stamp whose `presence_nonce` is already on ANY stamp
of the step, live or voided, judged under the row lock it already takes
for the shape check. The step's own stamps are the consumed-nonce
record, because a ticket is bound to one step: no table, no expiry
sweep. A completion writes no stamp, so it consumes no nonce.

**A stamped presence step completes on its live stamps** (design
`1ce67f7e`, decided by David 2026-10-07; backlog `570c66e9`). Presence
stays where a step's completion needs it and is removed where it adds
nothing: a step whose approval IS its passkey stamps takes no second
passkey proof to complete. Until then the completing request was judged
on its own ticket alone and the stamps the step held were never
consulted, so a step a passkey had just stamped refused a bare
completion, and both surfaces kept the stamp's ticket client-side to
send it again — a credential held between two requests, a two-minute
race, and a failure whenever the stamp and the completion happened on
different surfaces. The rule, judged in one place for every request
that moves a step out of its open states (`judge_completion` in
`boss-jobs/src/http/steps.rs`, reached by the status PUT and by
`POST …/complete-if`; pinned by `an_assurance_holds_on_every_path`):

1. **A presence step that declares sign-off roles** completes when every
   role in `sign_offs_required` holds a LIVE stamp (`Step::live_stamps`:
   never voided, on the step's current shape) that a passkey produced,
   and that stamp is no older than **72 hours** on the clock that wrote
   it. The completing request then needs a session only, and **any actor
   the step-update policy admits may send it** ("think about an admin
   grabbing 5 sign-offs"). Collecting a further sign-off voids none of
   the earlier ones; any other change to the signed content voids every
   stamp taken before it. A ticket on the completion is not an
   alternative to a missing stamp.
2. **A presence step with no sign-off roles** keeps the earlier rule: its
   completion carries a verifying ticket for that step, shape and actor,
   because that completion is the only act.
3. **A header that does not verify is refused on every path**, even when
   the stamps would carry the step.
4. Unchanged: the completing request changes nothing the stamps cover
   (title, metadata) nor notes or holder; a presence step is never
   skipped; WRITING a presence stamp still takes a verifying ticket at
   the sign-off door.

The 72 hours is one declared value,
`boss_core::job::PRESENCE_STAMP_COMPLETES_FOR_HOURS` ("for now" — it
will be revisited), and the refusal names the stale stamp's role,
authority and `stamped_at` and asks for a fresh signature; so that it
can be given, the sign-off door answers "already signed" on an OPEN
presence step only for a stamp that still carries its role, and is
unchanged on a finished one. It bounds completing only: a consumer that
ACTS on an approval keeps its own tighter bound, and the ops runner
still runs an approved plan once, within ten minutes of the signature,
judged on the stamp whoever completed the step. How a presence step's
completion was assured — the request's ticket, or the stamps named —
is recorded on its `jobs.step.completed` event as `assured`.

**A human-only step completes on a passkey** (David, 2026-10-07, item
`570c66e9`: "human-only step completion should use passkey for
enforcement", and "we may also be able to update jobs such that I stamp
the instructions so the step no longer requires human completion if it
doesn't make sense"). `human_only` was enforced by the roster alone, on
the id a caller ASSERTS, and the machine door believes any id — review
`3f7b70bc` measured `emp-ghost` and `emp-david`, with no presence,
writing a human-only step; and once a stamped presence step completes
from any actor, that check was the last guard on three of the eight
presence steps. So the declaration **raises the step's required
assurance to presence** (`judge_assurance`, read by the sign-off door
and by the completion alike), and the same one judgement
(`judge_completion`) answers at both doors, with the roster check kept
beside the passkey:

- **with sign-off roles** — the live passkey stamps of every required
  role, each BY A PERSON (`human_only::person_check` on the stamp's
  authority). Any actor that may update the step, a machine included,
  may then send the completion: no second ceremony.
- **with no sign-off roles** — a verifying ticket on the completing
  request, for this step, shape and actor, and the actor is a person. A
  bare completion (`boss step complete`, the machine door, any asserted
  id) is refused 422 `{required: "presence", human_only: true}` naming
  both ways out: complete it from a surface that runs the ceremony, or
  give the step a sign-off role in its Workflow row so it is stamped.
  An automation is still told it is not a person (403) before it is
  asked for a passkey it cannot have.

A human-only step is completed, never skipped, and the request that
completes it changes nothing beside the status, as on any presence step.
The completion's `assured` record carries `human_only` and, with no
roles, the `person`. **A step already open when this landed** needs
nothing done to it: the declaration is read off the stored step at
completion, so its completion simply takes the ticket. Every surface
that renders one of the ten plain human-only steps draws what the
passkey signs and answers the refusal with one tap and one retry, as
the approval surface does: `GenericSurface` for the `task` and
`credential-rotation` steps, and the `incident-review.js` plugin for an
incident's `review`. **The plugins share one copy of the ceremony**,
`infra/step-plugins/passkey-ceremony.js` (how what is signed is drawn,
the check that it was drawn, begin/finish with the off-screen guards,
and the one-tap-one-retry answer): a bundle is a classic script and
cannot import, so `sign-off.js` and `incident-review.js` each add that
file's script tag and register their mount only once it has run. It
registers no step kind and no `step_plugins` row names it; the app's
own copy stays `apps/web/src/steps/presence.ts`, held equal to the
plugins' by `signOffPlugin.test.ts`. The workflow lint's phase 10 is NOT
extended to human-only steps: it reads a step's declared presence, and
the incident `review` step, human-only with no declared field, would be
quarantined by it — a follow-up, once that row declares what its review
signs.

**What an approval depends on is rendered onto the step being signed**
(the same decision). The binding is the step's title and metadata, not
the job: hashing the whole job was rejected, because routine writes
elsewhere on it would void approvals and bring the redundant ceremony
back. So the workflow lint (`workflow_lint`, phase 10 — author time,
every publish, boot) refuses a presence step that declares no field but
the signer's own `decision`, `decided_at` and `comment`, as ops-request
renders its plan and the plan's hash onto `approve`.

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
authored in-app at `/it/registry/rules`. **`infra/dispatcher/rules/`
— one `<rule-name>.toml` per rule — is the registry's DEFINITION, and
the table is derived from it:** the dispatcher publishes every authored
rule the table lacks at boot and retires every enforced rule no file
names (`rules::seed`, 2026-09-11, backlog 41ba00cd). Adding a rule is
dropping a file in; changing one is bumping its `version`; retiring one
is deleting the file; **no migration writes rules**. Until then a rule
was declared twice — a file here and an `INSERT INTO dispatcher_rules`
in a migration, compared by a test and derived from nothing — the worst
shape of CLAUDE.md §9a, since one copy lived in the production database
where no test could reach it. The direction was decided on measurement:
nothing reconciles `dispatcher_rules` at boot (so the hazard that forced
the Workflow move did not apply), a reviewed `why` cannot live in a row,
and only the tree can supply a fresh database. Live authoring is
untouched — the seed is insert-if-absent and never walks a version back,
so `POST /api/dispatcher/rules` + publish still changes a rule with no
deploy, and the tree owns which rules exist rather than moment-to-moment
control of the rows. **One exception to append-only, stated once
(backlog b5f21e82, 2026-09-18): seed residue is deleted, not kept.** The
thirty-one pre-collapse migrations are applied history and still insert
sixty-four names on every fresh database; thirty-five of them no product
file authors any longer (the brewery's reactors moved to the tenant's
`seeds/rules.toml`, the design-doc sweep's retired with it), so the boot
seed retired them on every new instance and a tenant then had to take
each name over at v(n+1), reading `registry ahead, left alone` on every
publish after. Append-only protects decisions — a version someone
published, a rule someone switched off. A row a migration wrote and a
seed retired minutes later, under a name no file ever authored again, is
not a decision anyone made; `20260918022108-seed-residue-is-not-a-retirement.sql`
removes those rows (`source IS NULL`, no tenant row under the name — a
tenant that already took a name over keeps the history it was judged
against), and the list is pinned to its derivation by a test. Live
retirement is still a status flip, and `no-migration-writes-a-dispatcher-rule`
still refuses a migration that inserts or updates a rule; it passes a
DELETE, which cannot open a second home.
The reactive wiring is visualized as a cascade — trigger event →
rule → handler → emitted event → re-triggered rule, feedback cycles
highlighted, filterable by trigger event — at `/it/registry/dispatcher`. The
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

**A product rule is never born in the SPA** (design `ff1c3615`, David
2026-09-23, option (b) as proposed; answers backlog `7d9df2fe`, found
by the `/it/registry/rules` page audit). The page's **+ New rule**
created a rule with no `source` — a product rule — which the boot seed
above retires because no file names it, so it fired until the next
restart and then vanished with only a name in a boot log; on the live
instance 62 of 65 active rules matched the 62 files exactly and the
other three were `tenant:algedonic`. A warning at the button, option
(a), was declined because it keeps a control whose product is temporary
by design — the answer-instead-of-error shape. Landed (#586): the
button opens a page that creates nothing and names the two durable
paths, a file under `infra/dispatcher/rules/` carried by a car or a
tenant's `seeds/rules.toml` published into the instance
(`NewRuleGuide.svelte`); and the draft door refuses a product draft
under a name no authored file declares, reading the names with the
seed's own parser so the door and the seed cannot disagree, and
refusing every product draft when the authored directory cannot be
read. A new version of a rule a file does name is still accepted,
since the seed never walks a live version back — so "live authoring is
untouched" above now holds for rules that exist, not for new ones.

**The dispatcher owns reactions; stations own holding; matchmaking is
station discipline — data, never dispatcher code** (decided by David in
packet `194db591`, 2026-08-19; `docs/design/dispatcher-station-boundary.md`
stays as the living governance page to cite in review). The dispatcher
reacts — to the clock, to thresholds, to `step.done.<kind>` — and every
reaction is a registry row; a routing decision in dispatcher Rust is a
leak from the protocol layer into the substrate. A station holds:
membership is its row's predicate, ordering its discipline, capability
checked at the claim, so "why is this packet in front of this actor" is
answered by reading one row. Which actor a ready step is offered to is
queue policy, so the dispatcher may *execute* a matchmaking rule and
must not *be* one; executing rules, redelivery and dead-lettering stay
substrate. The rule does not adopt Q as an owning domain (question
`1dfed5d6` stays open) and schedules no migration. Measured against the
tree on 2026-09-27, both violations it names stood: the assign strategy
was `pick_employee_with_role_fallback` in the dispatcher's
`dispatcher.rs` (deleted 2026-11-12) — role order, first role
with a holder, spread by step id, all discipline and none of it data.
Its path out was not an `assign` field on the station row: on 2026-11-11
(backlog e1eaf72b) David ruled that "work is supposed to sit at the
admission queue of a station now, not at a name or robot or executor",
and the matchmaker was deleted — a ready step stands at its station
unassigned and an executor pulls it (`boss dispatch --next`), filtered by
role and model class at the claim. My Day's
queue partition is `splitQueues` in `apps/web/src/me/assignments.ts`,
half repaired, since the verdict test now reads the StepType registry's
`decision_shaped` flag first and keeps `VERDICT_KINDS` only as the
fallback.

**A protocol's cadence is protocol data: the clock coordinates, and a
process supervisor only keeps processes alive** (folded from
`protocol-cadence.md`; origin David 2026-08-12, `bacca14e`: "We should
be using dispatcher to coordinate the conductor as well rather than
systemd"; decided in reviews `d23998f7` and `47f3c3d2`, which answered
Q1 and Q4 in opposite directions, settled by David on 2026-08-15: "The
08-14 answer stands, fold cadence into dispatcher rules"). When a
protocol's windows open and how often it reconciles is part of the
protocol, so every firing is an event, a cadence change is a data edit
with an audit trail, and nothing drifts between boxes. Decided: a
cadence row declares its **basis** — `wall` fires on wall time whatever
the warp, `clock` keeps sim-day semantics, and `queue-depth` (added for
the experiments work) fires on a probed depth; **one clock service is
authoritative for both time bases** (Q2); **each firing carries a
deterministic id**, `cadence:<name>:<window-stamp>`, and catch-up emits
at most the single most recent missed window per rule, never a backfill
(Q3); **the conductor is an executor with a queue**, claiming window
packets rather than being exec'd by a timer; **cadence folds into the
dispatcher's rule registry** as a `[[cadence]]` sibling of `[[rule]]`
(Q1 as re-decided, superseding the separate `cadence_rules` table,
which on 2026-08-14 was a second DATABASE: the running loop obeyed a
boarding threshold of 8 while the record said 4); and **a window is
claimed through the claim CAS** (Q4 as re-decided, superseding
primary-key dedupe, because a firing claimed before its verb runs can
be spent by a verb that never ran — the twice-daily boarding window
fired for months without boarding a train, `4ed0e791`). Sequenced so the
loop is never migrated under a moving train: it reads `/api/cadence/*`
and its private sqlx pool is gone (2026-08-31, `a516f1f1`), and the rows
are declared one file per rule under `infra/platform/cadence/`
(2026-09-18, `393d3234`; the platform-registries paragraph in §The
network substrate). What is left: `cadence_rules` is still the live
table — the fold into the rules registry has not happened — and the
claim CAS for windows is not built and comes strictly after it, since
deleting the table while the loop reads it would stop every train at
once. The maintenance family's timers also stay on their systemd
wrapper (§Deployment, "Maintenance stops being invisible work"), so
their move onto wall-basis rows is left too.

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

**A tenant cannot yet register a handler, and the direction to test is
out-of-process** (design `153d49f7`, David 2026-09-23, accepted as
proposed; answers backlog `ec40e269`). Measured (builder run
`950597e7`, on `642c0171`): of 51 handlers, 29 are invoked by product
rules, 21 only by the example tenants' seeds (13 brewery-only, 8 shared
with the used-device shop) and one, `jobs.complete_step_matching`, only
by Algedonic's own tenant rule. All of them live in
`boss-dispatcher-handlers` and ONE binary registers them, so a
tenant's published rules — and `boss tenant check` — resolve against a
roster compiled into every deployment: the brewery's keg-deposit,
excise-accrual and packaging handlers ship everywhere, and a hosted
tenant can use only what the platform compiled in. Where tenant code
runs is the hosting safety levels by crate tier (data-only / tenant /
modules / full, 2026-09-18). Of three sketches — (a) a tenant links its
own dispatcher binary, in-process but a binary per tenant, which a
data-only tenant cannot ship; (b) out-of-process handlers, a rule
naming one the tenant's engine serves over HTTP (`POST
<engine>/handlers/<name>` with the event, returning its writes); (c)
declarative handlers as data, a templated PUT/POST body, which cannot
cover real logic such as packaging allocation — **(b) is spiked
first**, since it runs tenant code in its own process under its own
identity and policy, with idempotence and provenance to be held across
the hop, and the same spike measures how many of the 21 reduce to (c).
It is a direction to test, not a plan: no spike has run. Landed the
same day: the cascade map of what each handler emits moved out of core
into `boss-dispatcher-handlers/src/cascade.rs`, beside the code it
describes, with core keeping only the type; the handlers themselves
have nowhere else to be registered yet.

**The forward direction inverts this contract: reaction becomes
admission.** The network gets one admission edge — **Protocol**
evaluates the write against the packet's pinned protocol set,
**Policy** checks the actor may perform it, **Publish** stages the
consequences in the same transaction as the write — and reaction
survives only where admission cannot see: wall-clock timers,
external ingress, and cross-protocol reactors. Two-thirds already
exist (the `PolicyClient` gate, `ready_when` evaluated inside the
write transaction, the transactional outbox); the missing third is
**protocol-declared consequences**, which live in the WorkflowSpec
as `on` blocks per step transition — notify / spawn / assign /
obligation rows with an optional `when` — versioned with the
workflow and proven resolvable at authoring time by
`workflow_lint`. `on_complete_create` is the shipped precedent. The
edge is **promoted in place**, not extracted to a service: a
`boss-admission` crate boundary inside `boss-jobs`, because a
network hop inside the write transaction buys nothing until a real
second writer exists. Consequences are **decided synchronously and
delivered asynchronously** — jobs-internal ones apply in the same
transaction (exactly-once by construction, replacing the JetStream
consumer's at-least-once), cross-domain ones become **obligation
events** that the existing handler machinery drains instead of
interpreting rules, keeping the decision in the log beside its cause
while delivery stays at-least-once behind the handlers' own
idempotency guards. The queue-placement half publishes what already
exists: admission resolves the next station, records
requirement-addressed pools as events so replay can reproduce who
could have taken the work, and emits the ready/assigned markers —
no new placement state. The census of 2026-08-12 classed 38 rules as
7 jobs-internal consequences, ~22 domain effects, and 9 external-glue
reactions that never migrate; `infra/lint/dispatcher-rules-ratchet.sh`
makes the roster **shrink-only** from that baseline, and every
surviving rule owes a sentence naming why it cannot be a protocol
consequence. Scope is **packet admission only** in v1 — other domains
keep their write paths, with no quiet annexation of other front
doors. Landing the workflow registry's own draft/publish/bootstrap
writes on the outbox is a prerequisite: under this model a protocol
edit *is* a network configuration change, and a protocol the log
cannot witness is not yet data. The build plan is
`docs/design/protocol-policy-publish.md`.

### Durable source-relative guarded admission

**Delayed admission is a durable packet, anchored to its source fact**
(design `f1d66249-4746-4e20-b58d-d5321cd9e308`, technical decisions recorded
2026-10-12; answers `84a010cb`). This is the implementation contract,
**not yet implemented or activated**. The approved first Sales loop
(`3613f0af`, Departments `86f32b7d`) needs follow-up work seven days after
an unanswered inquiry; its tenant companion `aecd8740` remains open.
Current rule parsing stores `delay` but matching drops it before effects.
Accepting the syntax is not scheduling, so until this contract is built
the shared rule validator (`authoring::validate`: draft, dry run, publish,
boot seed, `boss tenant check`) refuses every rule declaring a `delay`,
well-formed or not — the first car fails closed. No in-memory sleep, immediate or
manual reminder, source-age reset, or Sales-specific core branch satisfies
this contract.

Pending work is a generic `delayed-admission` Job running a versioned
platform protocol: immutable intent, due/execution step, admitted,
suppressed or cancelled terminals, and nonterminal held attempts. Rules
declare the child and guard; stations hold the work. Advisory dispatcher
firings and periodic cursors are neither the queue nor its authority.
The envelope retains the complete committed source event, original
timestamp and source Job, whole rule bytes/name/version, handler ordinal,
checked duration and fixed due instant, complete child request with pinned
published workflow version and partition, guard and actor provenance.
Large evidence is retained by immutable verified file reference, never
truncated. Ordinary Job/step writes must not change these bindings.

Intent identity is a domain-separated UUIDv5 of instance/partition,
source event, rule name/version and handler ordinal; child identity is a
separate UUIDv5 of that intent. Retry after child closure still names the
same child and creates no second graph. Conflicting bytes under an identity
are refused. Terminal identity and original evidence survive firing
retention; no purge is authorized. Replay folds recorded intents and
attempts, not today's active rules over historical events. A captured
intent keeps its original rule and child specification through retirement
or publication of a new version; recovery cannot invent a missing binding.

The first supported declaration is one event-driven `jobs.spawn` with a
positive fixed elapsed delay and a typed Jobs-local source guard. Seven
days means exactly 604800 seconds from the recorded source fact, independent
of daylight saving and later triage or heartbeat. Unsupported handlers,
multiple delayed effects, periodic/source-delay combinations, malformed
durations, overflow, missing provenance and unbound guards are refused by
one validator shared by native rule authoring and tenant checks. Immediate
rules and existing periodic semantics stay unchanged.

Only committed intent evidence permits delayed delivery ACK or log-tail
advance. Capture failure retains the full source and pinned plan as a
durable recovery obligation before dead-letter/Term or cursor advance;
best-effort firing records cannot substitute. If that record cannot be
written, advancement is refused and the independent liveness surface says
so. Startup and clock ticks drain authoritative due packets with stable
ordering and complete pagination. Missed ticks or restart cannot lose an
overdue intent. Execution compares `now >= due_at`; lateness is recorded,
never used to move the anchor. Real/sim clock and partition boundaries are
preserved; rewind does not reopen a terminal intent.

A typed Jobs `GuardedAdmission` port, with agreeing in-memory/Postgres
adapters, uses the ordinary admission edge. It rechecks existing kind-scoped
Create and scoped source/intent Read for the registered dispatcher actor;
policy denial or outage produces no effect and grants no new authority.
For **authoritative state wholly in Jobs**, one transaction fences the
intent, source and named guard steps against their real writers, evaluates
the current guard at the due boundary, and commits the complete child
graph, its outbox facts and the intent's terminal/evidence together, or
none of them. A second independently committing create call or GET/POST
with before/after comparisons is not that transaction. Concurrency tests
must prove the actual answer/close writers share the fences. Cross-service
message state cannot be locked by this Jobs transaction: unsupported or
unbound reply evidence stays held, never guessed to mean no answer.

Initial guarded admission requires an OPEN source Job of the pinned
expected source kind, as well as each named guard step's explicit
allowed nonterminal status. A closed or wrong-kind source suppresses
admission without a child; unavailable source facts stay held.

False guards suppress with the exact observed state and reason; unavailable,
missing, duplicate, malformed or ambiguous source/step evidence keeps the
intent open with full attempt diagnostics. A named step must exist under
its source and have an explicit allowed status; absence is not a negative
fact. The Sales consumer must establish its actual received-event anchor
and authoritative answer signal from every supported inquiry reply path.
A proven `sent` completion may establish answered; no invented field or
missing step establishes unanswered. Unfenced reply paths are a consumer
readiness hold. Successful retries reconcile an exact existing child,
including CLOSED, rather than admitting another.

Every attempt preserves source/rule/intent/child identity, original anchor,
due and evaluated time, actor, full authoritative observations/versions,
result and errors. Pending, due, held and oldest-overdue reads show honest
policy withholding and complete counts. Event-log rebuild reproduces the
envelope and state; current guard/time creates a new recorded attempt,
never changes the original evidence. These are provenance, conservation,
closure, idempotence and determinism applied to admission over time.

Implementation remains a coherent source obligation: declaration and
validation, provenance, immutable envelope/write fences, durable capture
and settlement, due drain, atomic graph admission, events/rebuild and honest
native/read surfaces. Any intermediate car fails closed for unsupported
timing. Owning TDD must reproduce the current immediate-effect defect and
cover before/exact/after bounds, late replies and writer races, unchanged
anchor after unrelated work, unreadable guards, crashes at both commits,
missed ticks/restart, redelivery after child closure, concurrent workers,
rule versions, conflicting IDs, mutation attempts, clock/partition bounds,
policy refusal and the unread queue tail, with immediate/periodic positive
controls. New write fences require independent adversarial review.

Source GREEN proves the frozen source only. After arrival/convergence,
installed-contract and actual due-admission receipts must separately prove
runtime scheduling. The tenant Sales source then follows its existing
check, distinct plan review, human publication and first-real-packet proof.
Core admission does not authorize outbound contact. Parent `84a010cb`,
Sales `aecd8740` and Departments `86f32b7d` retain their original source,
runtime and remaining obligations; this documentation fold closes none
of them and activates no rule, credential, grant or tenant publication.

## Executors, runs and seats

An executor is a CPU of the machine, and a run is the record of one
executor doing one step: claimed, briefed, worked, reported, ended on a
terminal. This section holds the decisions about that record, the seat a
session holds while it runs, and the place a worker builds in. Folded on
2026-11-17 by backlog `2bc308d1` (fold batch F1, design `25511015` Q5).
Each paragraph says what is built, measured on origin/main `b0aa242e2`;
"decided, not built" is a decision in force with its car still owed.

**The run protocol is the harness's contract, not one harness's habits**
(design `59459712`, David, 2026-10-02, after the first Codex trial).
Requested execution stays apart from observed execution: a step's model,
effort and budget are the request, a printed brief proves no executor
started, and the model that ran is read from a harness receipt or
transcript, never from the controls submitted. Metering keeps
conservation per harness: Codex usage is read from per-response receipts,
deduplicated by response and filtered to the session's owner, cached
input and reasoning are never added twice, and a format that cannot be
read stays explicitly unmetered rather than becoming a zero. A capability
the harness does not supply stays absent: a parent's observation is not
a child's heartbeat, and a heartbeat is added only with observed
activity, a tested rule and its failure behaviour (the survival rule is
§A run survives silence only on progress the server can attribute). A
READY step's nominee changes through a narrow verb that preserves the
policy and human-only checks and lets a concurrent claim win; an ACTIVE
claim moves only by release then claim. Safety requirements are stated
by operation (isolated worktree, unique scratch path, test first, the
right actor, a copied receipt, a machine-run probe, an independent
review of a trust boundary) and apply to every executor; wording that
names one harness's tools belongs to that harness's adapter. Built: the
nomination verb, `boss step nominate`. The launcher and the remaining
metering ride item `2f7b8c00`.

**A dispatch refused after its claim, on the hook path, denies the call
first, then ends its run and releases the step** (design `11b9d856`,
2026-11-14, answering backlog `63639424`). When the hook-handed dispatch
fails after it has claimed the work step, it prints the same PreToolUse
deny the contradiction branch prints, naming the refusal and the packet,
so no agent starts unhanded. Only then is the half-open run ended
`refused` and the step released with a `why` naming the run, the way
item `f8bed383` already does on the printed and launch paths. A refusal
before any claim still lets the call proceed unrecorded, because a hook
that blocks on an API outage makes every session depend on the API. If
the deny itself cannot be printed, nothing is released, and the run is
left to the died and unreported clocks, so a possibly-live agent never
meets a second builder on its step. Decided, not built: `from_hook_at`
still propagates the refusal with `?`.

**A run's outcome is read from its terminal** (design `da8b2b9d`,
2026-11-14, answering backlog `c1d9eacf`). `run_outcome` reads the run's
completed terminal first (`landed` and `unreported` are success,
`refused` is cancelled, `died` is failed) and falls back to
`building.result` only for a run with no terminal yet, the window between
a report and a green. A skipped step is never reopened to carry a field.
Every reader of an outcome (the board, orient's MY WORK and region
counts, the metering) calls `run_outcome`, and a tree-wide pin names any
other reader of `building`'s `result`. A run ended at dispatch spent
nothing and gets no `agent_runs` row, and now reads `cancelled` rather
than unjudged. `--launch` prints its prompt only after its briefed writes
read back. Decided, not built: `run_outcome` still reads
`building_result` alone.

**A run is watched through an outbound stream, the remote-control shape**
(design `5763e2ca`, David, 2026-10-22, answering feedback `a4e440f3`).
Of the three behaviours that make remote control pleasant, a live
transcript is built first, on a resumable stream, which gives the third
(a reconnectable session) for free. The pod-side run pushes outbound to
the jobs API, `POST /api/agent-runs/{run}/progress`, from a tail
`boss dispatch` starts beside the run, and the browser reads
`GET /api/agent-runs/{run}/stream` with `Last-Event-ID` replay, so no
inbound path to the pod opens and the dev door stays LAN-only (Q1-Q3). A
frame carries only a tool's one-line description or 200 characters of
assistant text, never a command line, tool output or file content, and
frames live on the run, not in the audit log: they are telemetry, and the
work's own events are the record (Q4). Mid-task messages from a browser
into a running agent are a new trust boundary and get their own design
after the transcript has run (Q5). Decided, not built.

**A session and the runs it delegates are recorded by the harness's own
hooks, with nothing typed by hand** (design-doc "The shop floor",
`511fa7d4`, David 2026-09-18, all four questions accepted as proposed;
built as car 2b of `c87fb59b`, backlog `da925366`). (Q1) Every operator
session is a `work-session` packet: opened at SessionStart with its
actor, host and start, given a heartbeat on each prompt that records only
`last_active_at` and a prompt count, and ended at SessionEnd. The
transcript never enters the record; a dispatch's prompt does, because it
is the brief. (Q2) The hooks ship in the repository, in
`.claude/settings.json` and `infra/dev/hooks/`, because a verb a person
must remember to run becomes a gap: PreToolUse on the Agent tool runs
`boss dispatch --from-hook`, which opens or links the run, and
PostToolUse reports it with the Agent tool's own result, copied. A hook
that cannot reach the system of record exits 0, so a dark API never
blocks a session, and a session no hook ended is ended by the clock rule
`work-session-ends-when-silent` six hours after its last heartbeat.
(Q3) Sessions are drawn as crews and runs as cars being built, upstream
of the gates: the shop floor is a region of `GET /api/yard/regions`,
whose reading is still keyed by the IT map's region names rather than the
station tree's (§After the shop floor, the machine carries the work).
Built, measured on origin/main `d30cfb74f`. Later decisions refine the
hook path's refusals (`11b9d856`, above) and the packet's keys
(`e17c81c9`, below).

**A session keeps one current handoff; its history is the log** (design
`e17c81c9`, 2026-11-16, answering backlog `e18b0b44`). A work-session
packet is current state, so it carries ONE `handoff` key, replaced on
every wake, and ONE `progress` key, replaced, for a turn past 30
minutes. Each write's `jobs.job.updated` event carries the prior row, so
`GET /api/jobs/<session>/events` is the handoff history. A key per wake
made the packet a second copy of its own log: work-session `2d013092`
reached 738,325 B, 97% of it handoffs, and the packet budget (design
`6ab85352`) refused the handoff write itself, while the log paid
quadratically for it. An order about an item is a nomination on that
item; an order about the whole session is ONE `order` key `{by, at,
text}`, replaced by whoever gives it. `factory_restart_*` keys and an
item's own daily `handoff_<YYYYMMDD>` key stay, because neither grows per
wake. No migration: a reader takes `handoff`, falling back to the newest
`handoff_*` on a session that predates the change, and a session already
at budget opens a successor named by `previous_session`. Built:
`infra/dev/drain.md` and `infra/dev/factory-drain.md` read and write the
replaced keys.

**A handoff is read and written by verbs, and one coordinator runs at a
time** (design `b40008a0`, 2026-11-14, answering backlog `bb8e56e8`).
`boss orient` prints a HANDOFF section from the same actor's previous
work-session and from each item it names, paging to `.total`. A
`boss handoff --file <F>` verb writes it through the metadata merge door,
with the time taken from the clock and read back; its key is the single
replaced `handoff` of `e17c81c9`, which supersedes this design's
`handoff_<date>_<HHMM>Z` key per write and its refusal of a reused key.
A session that would coordinate first reads the open work-sessions for
the coordinator role: if another actor's session wrote within the bound
the SILENT alarm uses, it starts as a worker (pull `--next`, report,
route nothing). It takes over only after that silence is measured,
records the takeover on both work-session packets naming the silence and
the alarm, and the old coordinator's first live write ends the takeover.
The rule lives in `infra/platform/documents/coordinator-rules.md`, and
the drain prompt points at it. Decided, not built: orient reads no
handoff, no `boss handoff` verb exists, and the coordinator rules carry
no takeover rule.

**A dev-build worker starts from a warm target built from origin/main**
(design `12559782`, 2026-11-13, answering backlog `db949aee`). This is
D5 of §Dev isolation activation follows main from the /work PVC. The
seed holds the target of origin/main and nothing else; a supervised
automaton builds it and no worker ever writes it, the precedent the gate
seed set when a branch could plant in a shared one (`c1e2e951`,
`a016bf27`, `4cdb96f4`, `b4bf8bba`). The seed is a Longhorn volume, and
each worker's volume is created as a clone of it, a private writable copy
that needs no copy code and no shared mount, at the same 40 GiB. A
refresh Job after each train converges builds the converged sha into one
target with the commands a builder's first build runs, writes `.seed-head`
and swaps the volume the clones read. One target per worker (`ad1d559d`)
lands first. A worker whose clone failed or whose volume has no
`.seed-head` says `starting cold` on its receipt with the reason, and a
warm start records the seed's sha beside the admitted head. Acceptance is
a measurement: clone time plus a seeded first build must beat an
unseeded one on the same sha (`e3ec2d95` records the cold number), or
the clone path is withdrawn for an init-container copy from a read-only
seed. Decided, not built: `infra/dev/dev-build.py` still sets
`WT_SEED=/workspace/no-seed`.

**Liveness reads around declared non-writers, never past them by
capability** (design `45ba4201`, 2026-11-14, answering backlog
`f715ff7d`). The dev artifact inventory marks every file
`liveness-unknown` when any process's fd table is unreadable, and an
unknown retains; on the live pod eight such processes made all 167,306
files unknown. The sidecar does not take CAP_SYS_PTRACE, which grants
ptrace across uids, far more than reading fd lists needs. Instead
`infra/cluster/artifact-lifecycle.toml` gains `[[non_writer]]` rows, each
naming a process by uid and comm (and exe when readable), the writable
roots it can reach, and why it is safe. An unreadable process that
matches a row exactly, with roots disjoint from a family's, no longer
makes that family unknown; one that matches nothing, or whose roots
intersect, still does. The report lists the declarations each pass
relied on, by pid and row, and a pin plants both refusing cases.
Directories even root cannot read are reported as a named `unreadable`
family with paths and count, never left uncounted. The policy still
grants NOACTION. Decided, not built.

**The admission step gets a seat, and reviews and repairs route by
station** (design `d32b58b7`, filed 2026-11-17 by the drain session at
David's request, its three questions decided by David on 2026-11-18). It
extends step 1 of the morning plan (design `25511015`: station pull
`f999363f`, seated loops `35f828cf`, seats after a roll `b513514c`).
Measured on 2026-11-17, everything after admission was a coordinator
session's hand work: 224 items waiting at `triage` with no rule triaging
any, about 20 hand-nominated reviewers for held cars, 6 repairs
dispatched by hand, and 7.5 idle hours overnight while the session slept
(`5627a4e1`); `boss-dispatcher` routes events and assigns no work.
Decided: every step after admission is a protocol step at a station, and
the one judgement at the front, `triage`, gets an executor seat like any
other rather than a new component. (Q2) **A robot seat sits triage**:
Codex, the cheaper model, at medium effort, pulls ready `triage` steps
through `--next` at the admission station, measures the claim against
origin/main, and records a disposition with evidence through
`boss triage`, setting the item's class and priority. It never builds and
never closes a step it does not own; a disposition it is unsure of goes
to `/me` as a question, not a guess; and no agent closes an item declined
without evidence that it is a duplicate or stale. (Q1) **The class set
at admission is the product share**: triage labels an item `product`
(Algedonic business software, under Operations or Finance) or `factory`
(BOSS itself), and station ordering gives product one build slot in
three once at least one outcome is named, falling back to factory work
when the product queue is empty, so no slot idles. This replaces the
reserved-gate idea: the mix is set where work is admitted, not where it
is tested. The first outcome is a live Financial Accounts view in the
Finance station that shows the available Stripe balance. **A held car's
review is ready at the review station** and is pulled by a seat whose
actor differs from the car's builder, so distinctness is enforced at the
claim, not by a coordinator's nomination (`802e207d`, `140b279f`); **a
CHANGES verdict or a red gate makes the item's repair ready at the build
station** with the findings attached, for any build seat to pull. A
weekly report reads product cars against factory cars, and intake
against closure, from the train record and the backlog. A coordinator
conducts: it reads the alarms and that report and turns slips into
protocol changes, and it routes no work. (Q3) **Review and repair
routing is built before the triage seat**, after station pull and the
seated loops, because reviews and repairs were the binding constraint on
2026-11-17 and the seat matters most once seats pull on their own; the
weekly report comes last. Decided, not built: on origin/main `24d3c02d5`
there is no triage seat, no `product` or `factory` class, no share in
station ordering, no station routing for reviews or repairs, and no
weekly report.

## Correctness protocol & the audit log

The five-property protocol — **provenance, conservation, closure,
idempotence, determinism** — is a first-class invariant (living
reference: `docs/design/correctness-protocol.md`). The audit log
is the system of record; projections are pure functions of it;
rebuilders reproduce truth from it
(`docs/design/projection-rebuilders.md` is the living contract);
the system contributes zero error of its own.

**Sim time is retired from the record** (David, 2026-08-22, packet
a7a4cae5 — superseding the earlier "every projection row
representing a sim-time event stamps the sim-day the engine
emitted" rule). `Event.timestamp` / `audit_log.timestamp` is
wall-clock, minted by `EventStamp` at emit, whatever mode the
deploy's clock-api runs in — one incident reads as one timeline
(the mixed-clock log had a 19:25 arrival stamped ~03:00 because
clock-routed writers stamped sim while allowlisted writers stamped
wall). The sim timeline survives where it is data, not clock:
**business dates in payloads** (`happened_on`, `issued_on`,
`completed_on`, `{day}` tokens) still read the sim-aware clock
port, and the sim's own scheduling (day cursor, warp, epoch,
`clock.day` rules) is untouched. If a record ever needs a
sim-timeline annotation again, it returns as a protocol field —
explicitly deferred.

**Events become durable atomically with the state they describe**
(the transactional-outbox decision, arc completed 2026-07-29;
living contract:
`docs/design/transactional-audit-log.md`). Writers stage events on
`event_outbox` inside the domain transaction
(`record_event_in_tx`); the single `boss-event-relay` daemon drains
them into `audit_log` + NATS in order. This replaced the original
best-effort post-commit publisher after the 2026-07-13
replay-divergence incident proved the swallowed-write class real
(260+ committed state changes with no event). The decision chain
that got here: relay as a separate binary in the deploy list from
day one; explicit `record_event_in_tx(&mut tx, …)` threading at
call sites rather than publisher magic; epoch trim TRUNCATEs the
outbox before the audit DELETE; ref-checks moved up to the outbox
trigger so phantom-subject writes abort the operation. The interim
"rebuild-consumed kinds only" classification was ultimately
superseded — **every** emit migrated (three recipes: per-kind
stamps on port mutations, jobs' events-ride-the-write
`events: &[Event]` parameter, and the `EventRecorder` port for
row-less telemetry), idempotency guards double as event gates
(replays record nothing), and
`infra/lint/outbox-migration-ratchet.sh` enforces a flat CI ban on
post-commit publishing workspace-wide. Two full-year from-empty
regens (484K and 1.84M outbox events, zero undelivered) are the
acceptance record.

**The relay waits for a locked predecessor; it never skips one**
(design-doc "Preserve pending outbox order before guarded gate
replay", 2026-10-16, landed the same day in train #1206). The relay's
claim in `drain_outbox_once` takes pending rows `ORDER BY id … FOR
UPDATE` without `SKIP LOCKED`. With it, a relay that met an earlier
row held by another transaction audited the later row first: a real
relay test recorded one Job's two committed `jobs.job.updated` facts
in reverse, which replay then reads as the wrong final state. Skipping
bought no concurrency, because the audit insert already serializes
on the hash chain's lock; it only bought a wrong order. The wait is
bounded and visible: the claim runs under `lock_timeout = '2s'`, so a
held predecessor returns a retryable error and leaves every fact
pending, never skipped. This orders the audit of committed pending
rows and nothing more: it does not order independent uncommitted
transactions, repair audit rows already reversed, or make publication
exactly-once (the publish phase stays at-least-once). The outbox
prune keeps `SKIP LOCKED`, since a deletion has no order to keep.
`boss-events/tests/stream_order_pg.rs` pins it: a held predecessor
blocks the later fact, a cancelled relay leaves both facts for a fresh
one, two concurrent relays write one audit row per fact in order, and
a prolonged hold returns the visible error. The design filed no open
questions and its review step was never run, so this records what
landed, not a ruling.

**A truncating rebuild proves the log complete for its window, or
refuses** (design `b046f510`, *A projection rebuild refuses to replay a
log that is missing a committed write*, 2026-09-28, no questions;
answers backlog `d6656496`). A live writer stages its fact on
`event_outbox` in its projection row's transaction and the relay copies
it into `audit_log` later, while every rebuilder reads `audit_log` alone
— so a TRUNCATE-and-replay run between those two moments deleted the row
and had no fact to put it back from, with no signal. No timer ran a
rebuild against the system of record, but nothing refused a hand run
either. Decided: refuse rather than fold the outbox into replay, because
twenty-odd rebuilders reading two sources and de-duplicating by
`event_id` is the second source of truth the protocol exists to remove;
and share no lock with the relay, which would serialise the hot path
behind the longest rebuild.
`boss_events::outbox::lock_and_assert_log_complete`, in each rebuild
step's own transaction before its wipe, takes ACCESS EXCLUSIVE on the
tables the step wipes and SHARE on `event_outbox` (which does not block
the relay's claim), then counts the outbox rows whose event `audit_log`
does not hold — the exact loss condition, not `delivered_at IS NULL`,
which would also refuse rows already copied but not yet stamped — and
refuses on any, naming the count and the oldest row. Before any lock,
`boss-rebuild` and each standalone rebuild binary wait for the drain,
unlocked, up to `--drain-timeout`; only the locked count decides. So a
live run is supported, and correct whenever it succeeds.
`boss-search-reindex` is exempt (its index has no live writer, so a late
fact lands one cycle late, not lost), and append-only catch-ups that
truncate nothing are untouched. One part landed differently from the
design: it tolerated a deadlock as a loud failure, review reproduced
deadlocks in both lock orders, and so the locks are taken under a
`lock_timeout` shorter than Postgres's deadlock check, inside a
savepoint, and the rebuild yields, backs off and retries a bounded
number of times — the live writer is never the side aborted. Pinned by
`rebuild_log_complete_pg.rs` and by
`every_truncating_rebuilder_proves_the_log_complete.rs`, which refuses a
production TRUNCATE that neither calls the helper, nor passes through
`replay::replay_projection`, nor sits on its exemption list with a
reason. The relay-lag reader the triage found missing landed as its own
item (`72c50b8b`): `GET /api/events/outbox/stats`.

**A dark message bus degrades a service; it never exits one**
(design-doc of that title, 2026-11-10, after the 2026-11-09 outage in
which an unreachable NATS crash-looped the boss pod while Postgres
stayed up; its five cars landed by 2026-11-11). The outbox means the
write path no longer needs the bus, so nothing treats the bus as a boot
precondition. `boss_nats::NatsEventBus::connect` is the one constructor,
the dispatcher included: it retries the first connect and reconnects
without bound, and only a malformed URL still exits, as a config error.
JetStream's stream is ensured again on every connect, and each durable
consumer opens and reopens in a capped-backoff loop (`keep_bound`). **A
write on a dark bus commits and the outbox holds its publish — never a
503**: the fact commits in the state's own transaction, so provenance
and conservation hold, and refusing would turn a night's outage into
work re-driven by hand in the morning. Reads come from Postgres either
way. The relay keeps its audit-then-publish order, so `audit_log` lags
the outbox while the bus is dark; split the relay's cursor only if a
reader of `audit_log` is measured to need it current through an outage.
`delivered_at` waits for the stream's PubAck, because the design's
premise was wrong: async-nats queues a publish on a dark bus and answers
Ok, so a stamp on Ok marked delivered events that never left. No write
path's event is dropped by a dark bus any longer; the agents door's
unresolved-login count and the asset event, its last two direct
publishes, stage on the outbox. A degraded service looks degraded:
every `/health` carries `capabilities.bus` (`connected` or
`disconnected`) as a field, never as the status, which stays `ok` and
200, because a readiness probe that failed on the bus would dark the
record exactly as the crash did; the relay's minute heartbeat logs
outbox depth and oldest-undelivered age, and WARNs when a minute
delivered nothing while rows are owed. A vanished delivery-lane
consumer is not recreated, because the cutover fence pins it by
creation time and a recreated one would silently skip what was
published while it was gone; it stays visibly not running until an
operator rebinds. Still open: the alarm on undelivered age belongs to a
reader outside the jobs API (backlog 9574cd04), and the bus's own
availability is the single-replica NATS item, c6fc7326; this decision
makes an outage cost lag instead of the record, not rarer.

The log itself is tamper-evident in three layers: append-only
enforcement (BEFORE-triggers reject UPDATE/DELETE), a **hash
chain** (each row stores its predecessor's hash and its own,
computed by a trigger that assigns ids post-advisory-lock so the
verifier's id-walk matches commit order — uncontended now that the
relay is the log's single writer; the chain columns ship in
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

**Event kinds are the last vocabulary to become a registry.** The
log's own semantic layer was folklore — 120 distinct kinds across 15
sources on the live box against roughly 19 declared as constants —
while every other vocabulary in the system was already
registry-shaped. `event_kinds` is a **table, not a generated
manifest**, and it is compositional because the kind space is:
static kinds are plain rows, and a **dynamic family** is one pattern
row whose `suffix_domain` names the registry that already owns its
suffixes (`step.done.*` ranges over the StepType registry), so
declaring the family once covers every future step kind without a
migration per step type. A declaration carries a **flat field
inventory** (`payload_fields`), starting empty and filled as
consumers need it — the per-field sensitivity classification the
payload-encryption work wants becomes a column on a row that now
exists. Enforcement is deliberately **warn plus a drift guard**
rather than a write-path abort: an emitted kind no pattern matches
is loud in `boss-audit-integrity-check` and in CI, and the log stays
available under drift, because a registry that could refuse an event
would make the system of record refusable. The seed was **harvested
from the live log** rather than hand-authored, which is why it
matched reality on day one; new kinds are born declared in the same
change that emits them.

**Payload confidentiality is policy-scoped field disclosure, classified
in the event-kind registry and staged read path first** (folded from
`payload-encryption.md`, decided by David in review `07c40d86`,
2026-08-19; from his feedback `a32ea8c0`). The log proves plaintext —
the chain's canonical bytes include `payload::text`, and ref-checks,
dispatcher `when` expressions, views and search all read payload
fields — so there is no free layer to encrypt, and
`people.employee.created` carries a salary in the clear in a log that
by design cannot be redacted. Classification belongs in the semantic
layer, and policy maps `(actor, sensitivity class)` to disclosure,
extending the existing `(action, resource, scope)` engine one level
down rather than adding a second. Decided: (1) **stage 2, read-path
redaction with no cryptography, ships first**; field-level encryption
at write (layer 3) follows for fields whose classification proves
stable, and per-actor encryption (layer 4) only if a tenant's threat
model demands it; (2) **sensitivity is a `sensitivity` key on
`event_kinds.payload_fields` entries** — the column created for exactly
this; (3) once fields seal, the integrity checker learns nothing new
and **replay carries a rebuild principal**; (4) the existing plaintext
history is not rewritten, and no epoch reset or baseline is needed now
that the network runs on wall time; (5) **a sealed field reads as
sealed and marked, never absent** — absence lies. The recorded answers
to (3) and (5) are cut off mid-argument in the file, and the review
packet does not read back from the system of record, so what the
sealed marker says is still open. Nothing is built: no entry in the
tree declares a `sensitivity` key.

**Conservation applies to packets: every admitted packet reaches a
terminal, and every non-terminal packet is visible at one station or
more** (folded from `packet-loss.md`, all four questions decided by
David in review `9fb9904f`, 2026-08-19; origin his "I am also afraid
that 'packet loss' is going to be a real issue for us", 2026-08-13).
Five losses on one day, none noticed by the system: bookkeeping written
to the wrong instance for ~14 hours, a branch swept with two unmerged
commits under a record reading "landed", a green train stalled 95
minutes, 16 feedback packets left at `submitted` after their work
shipped, and skipped cars mute at the dock. The taxonomy: **misrouted,
destroyed, stalled, orphaned** (matches no station predicate, so no
lens will ever show it), **unacknowledged**, and **unsent** — the worst
mode, because it leaves no record at all: a sender who has learned the
queue does not move stops sending, which is why flow and transparency
are load-bearing rather than polish. Decided: (1) the invariant above —
conservation over time and over space, computable by a census without
new state; (2) **report first, and raise only once thresholds are
calibrated on real numbers**, and no catch-all station, which would
turn a visible defect into a tidy queue nobody reads; (3) **the census
is a cadence writing its counts to the log**, so loss is a measured
series; (4) **destroyed content is out of the census's scope**, named
so it is not mistaken for covered. Built: the rule
`network-census-daily` fires the `network.census` handler, which lands
one `jobs.network.census` event per firing through `POST
/api/network/census`, and `boss census` reads the same counts on
demand. Not built, by decision: the raiser.

**The end state for the bus is the log itself.** The dispatcher's
two durable consumers move off JetStream onto a cursor over
`audit_log` — both already consume `(kind, event_id, payload)`,
which are the log's exact columns, and the cursor pattern ships
twice already. The swap deletes the duplicate durable log, retires
the delivery machinery behind two real incident classes (the
ack_wait/backoff double-fire; the redelivery state-leak that
receive-dedup compensates for), makes the silent-zero-deliveries
filter trap structurally impossible, and removes a stateful service
from the correctness path. Everything else on the bus either wants
at-most-once (the SSE fan-outs) or is not event-log traffic (the
cybernetics message plane), and stays on NATS. Costs accepted with
the decision: side effects trail the write by relay lag plus a poll
interval (human-timescale irrelevant), the retry/dead-letter budget
and the concurrency fan-out must be rebuilt on a cursor, and epoch
trim re-anchors cursors the way it purges the stream today.
Sequenced `dispatcher-rules` first (its `Settle` outcome is already
transport-agnostic), then `dispatcher-steps`, then the stream
shrinks to fan-out-only retention — **staged with the cluster work,
not standalone**. This and the insert-time chaining above were
settled as one decision, because one writer inserting in sequence is
what makes id order ≡ commit order, which is exactly what log-tailing
needs to never miss a row; if sustained demand ever approaches
~1K/sec, both reopen together.

**A correction names what it corrects, and every reader is handed it**
(design `4105b020`, David 2026-09-23, all six questions accepted as
proposed; answers backlog `56727f95`). A completed step is frozen, and
correctly; a damaged sentence on one could only be corrected BESIDE
it — backlog-item `f3e091f0`'s triage evidence still reads "Ordering
trap confirmed:  is required of every rule", its fix under a job
metadata key, `evidence_correction`, that nothing reads. Measured across
all 15,039 jobs: about 132 corrections under 25 key names — one batch
key on 107 jobs, 24 hand-invented keys on 25 more, and one correction of
a correction joined to it only by its name (gate-run `9a2576fb`,
`verdict_correction` then `verdict_correction_withdrawn`). The server
taught the habit: the terminal-freeze 409 in `http/steps.rs` sends a
correcting author to "the parent job's metadata" and names no key.
`reproof` and `regate_receipt` were already the working shape — a
reserved key plus a reader obliged to look — without the generality.
Decided: (1) **first-class, not a convention readers learn to find**,
because 25 key names are the measurement of a convention authors do
not keep; (2) **one reserved append-only list, `corrections`, in job
metadata**, entries `{step, field, reads, should_read, why, by, at}`,
written only through `POST /api/jobs/{id}/steps/{step_id}/corrections`
and a `boss correct` verb with `-file` twins; the door refuses an open
step, a field the step does not hold, and a `reads` excerpt absent from
the stored text, emits `job.step.corrected`, and the generic metadata
PATCH refuses the key — the step and its audit event are never touched;
(3) **the reader is handed it, not asked to search**: the job GET
attaches each step's entries as `step.corrections`, one shared web
component marks the field, `boss brief` prints each correction under
the field it corrects, and the original is never replaced in the
render — both are shown; (4) **a correction is withdrawn only by
appending** an entry carrying `withdraws: <index>`; (5) **separate from,
and lighter than, `correct-the-record`**, which is for a published
claim whose premise changed decisions, holds 0 packets (control:
`kind=backlog-item` answered 692 on the same connection), and whose
`applied` step will use this door when the false claim sits in a
completed step — a damaged sentence needs a signature, not a review;
(6) **no backfill**, since rewriting the ~132 would mean guessing a step
and field for each; the 409 hint names the new door instead, and two
weeks after it ships the scan is re-run with a target of zero new
ad-hoc keys. `reproof` and `regate_receipt` stay as they are: they
supersede by precedence, and each has its reader. Built (first car,
2026-09-24): the door, its refusals and a withdrawal that must name a
live entry of the same step and say why; the event, spelled
`jobs.step.corrected` in the jobs crate's namespace and recorded beside
the JOB_UPDATED row state in one transaction; the PATCH refusal, and
the job PUT carrying the stored list forward so a whole-metadata body
cannot erase it; `step.corrections` on the job GET and its live stream;
`boss correct`; `boss brief`; and the terminal-freeze hint, one constant
(`boss_jobs::corrections::TERMINAL_STEP_HINT`) at both sites and in the
dispatcher's test double. The shared web marker is shipped:
`StepSurface` mounts `StepCorrections` above both the generic and plugin
surfaces. Each correction names its field, quotes the recorded and
corrected text, and identifies who corrected it and when. The original
stays visible; a withdrawal stays in the list and strikes through the
correction it withdraws. This landed in train `441f2d406` (2026-09-24).

**Evicting an example reference row is a fact, replayed as a delete —
decided, not built** (design `41ee0d95` of that title, 2026-09-28, no
questions; answers lead (2) of backlog `99539bd3`). The two doors that
evict the example reference rows (§OSS posture: the forge verb
`retire-example-reference-rows` and `boss-init`'s first start) both run
`infra/postgres/example-reference-rows.sh delete-sql`: bare DELETEs
across seven tables (companies, locations, departments, tax kinds,
sales-tax rates, GL accounts, classes) that stage nothing on the outbox,
and the fact pin scanned only Rust, so it could not see them. Decided:
(1) **a fact, not a derivation** — the eviction reads the checkout's
example seeds, the instance's own tenant declarations and live
references, so re-running it at replay would evict a different set, and
a state change that depends on when and where it ran is a fact; (2)
**one kind, `reference_rows.evicted`**, one fact per table per run,
`{table, keys, door, ops_request?, tenant_id}`, declared where the
emitted-kinds lint reads; (3) **built from the DELETE's own `RETURNING`
in the same statement**, a CTE that inserts into `event_outbox` only
when it deleted something, inside `delete-sql` so both doors take it at
once; (4) **replay deletes the row rather than marking it retired**,
because the live table holds no row at all — a log-ordered rebuilder
applies it in order (`rebuild_classes` re-creates a key declared again
later, as live did), and one `boss-rebuild` pre-pass applies it for a
table without one; deleting an absent key does nothing, so an in-place
rebuild stays at head; (5) **past `--for-real` runs are backfilled**
from their ops-request records, or as `backfilled_from: live-diff` where
a record names no keys, so the log accounts for every row absent today;
(6) **the pin gains a shell half**, an `sh <file>` scope in
`writes-without-a-fact.txt` scanning `infra/**/*.sh` outside
`infra/lint/`, so a shell door cannot drift again unseen. Car 1
(decisions 1–5) is a trust-boundary car and parks held; car 2 (the shell
half) rides in either order. Neither is built: `delete-sql` stages no
fact and the allowlist has no `sh` scope. Lead (1), the classes `--take`
path, already wrote through the classes door and is not this design's.

**A registry write stages its fact in its own transaction, and one routine
replays every registry.** Design `e5d814d3` (extends 3036296f mechanism A,
"a write leaves a fact") retires fifteen gap items with one mechanism:
`boss_events::stage_fact(tx, kind, row)` writes the outbox row in the same
transaction as the registry write, with the FULL row as payload, named
`<registry>.<row>.declared | updated | retired`. Because every fact carries
the full row, `boss-rebuild` replays every registry with one routine,
registered as (table, key columns, fact prefix): upsert by (table, key,
version) on declared and updated, mark retired on retired, in log order;
per-registry rebuilders that only replay full rows are deleted. Registries
without a write door (subject kinds, credential text, classes) gain a PATCH
door that calls the helper. `migrations-declare-schema-only` refuses INSERT
on registry tables after the cutover as it refuses UPDATE and DELETE, and
the emitted-kinds lint reads `AuthAudit::emit` call sites. When the last
gap closes, `writes-without-a-fact.txt` is deleted.

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

**Ordinary purchase costs admit zero and refuse negatives.** (Design
`0a8e95f1`, David 2026-10-11; source car `b58e8c36`.) Zero-cost Draft
lines are valid. Ordinary create and batch commands reject a negative
unit cost with a visible reason; the first Draft-to-placed transition
checks the same rule under the order's lock or database transaction,
before changing its status or emitting facts. Batch admission keeps its
existing per-order transactions. Historical fact replay restores the
recorded full rows, including signed costs, without invoking ordinary
command validation or rewriting those facts. A credit-specific signed
cost needs an explicit money-policy decision. The physical Warehouse's
activation is a separate decision.

**An order or bill belongs to the vendor its reference names by ID.**
(Design "Vendor dashboards need a stated rule for unresolved financial
references", `ccfa6069`, David 2026-10-11, answered as proposed;
backlog `ddf9d70a`.) Attribution reads the stable vendor Subject ID,
never the display name: vendor creation admits duplicate names, empty
names and a name equal to another vendor's ID, so a join by name
double-counts, lets an empty reference claim an empty-named vendor and
misses an order that names its vendor by ID — each reproduced on
synthetic fixtures, none observed in a live balance. A null or empty
reference is unassigned. A nonempty reference that matches no vendor ID
is unresolved and shown as unresolved, never assigned by name; any
mapping from a legacy name to an ID is explicit and evidenced, and
recorded data is preserved — no automatic migration, no financial
write. The write side already holds the rule: the purchase-order door
accepts a registered vendor ID, null or empty and refuses a display
name 404 with no event, and the PostgreSQL adapter validates a
nonempty reference against the vendor Subject ID (its documented
import escape does not). **Left open**, because the answer accepted the
proposal's own deferral: whether a unique-name fallback is ever
permitted, and how unresolved records count toward the dashboard's
totals and outstanding amounts — both to be answered before the UI
repair. Not built: `apps/web/src/vendors/VendorsList.svelte` still
matches orders and bills to `vendor.name`.

**The chart of accounts is tenant data; the starter chart is the
brewery's.** (Backlog `41af5195`; design `18cf4272`, David 2026-09-17;
amended by `718ac982` the same day.) 40-ledger.sql seeds the brewery's
chart, and since `718ac982` that chart is declared in
`examples/brewery/seeds/chart_of_accounts.toml` and treated as example
residue everywhere else: a fresh company instance evicts it at first
boot and an existing one through `retire-example-reference-rows`
(§OSS posture), so a company's books start empty and hold only what
its tenant declares. A tenant
declares its own accounts in `seeds/chart_of_accounts.toml` and `boss
tenant publish` sends them through `POST /api/ledger/accounts/batch`
after the classes — insert-if-absent by code, the batch-door shape the
classes, locations and agents doors share, one `ledger.account.declared`
fact per inserted row staged on the outbox in the insert's own
transaction. **A code that collides with a starter row is the SAME
account under the starter's name.** The code is the identity every
posting rule and journal line points at, so two rows cannot share one;
insert-if-absent keeps the registered row and the answer names the
difference (`kept: [{code, differs: [field…]}]`, `1000 (name differs)`
in the publish line), and the tenant then adopts the code as it stands
or chooses another. There is deliberately no upsert and no rename in
place: renaming `1000` would re-label every entry already posted
against it, silently. `boss tenant check` judges the file with the
door's own validation (codes unique, kinds and balances inside the
table's CHECK constraints, a parent declared before its child); the
collision itself is only visible at publish, because only the
deployment knows its chart.

**Algedonic's books read the ledger, and its bills get a protocol**
(design `72ccb3b2`, David 2026-09-23, both questions accepted as
proposed; from the `/ux/finance` page audit `3f964c57`). Finance is on
for Algedonic (module decision `1054c099`). Measured: the ledger held
one entry, a $1.00 sponsorship still in Cash in Transit awaiting its
payout; revenue reaches the ledger only through packets
(`receive-a-sponsorship`, `receive-a-payout`), yet the finance page's
headline, AR aging, gross margin and Invoices tab read commerce
invoices, which nothing writes, so the page could never agree with the
ledger's income statement; and payables had no protocol — the ledger's
bill endpoints held 0 bills, while PO Approvals and AP aging read
inventory, which the company does not run. Decided: (1) **payables are
a finance protocol, `receive-a-bill`**, on the ledger's own bill
endpoints — one packet per invoice (cloud, domains, Cloudflare,
software subscriptions, Stripe fees) posting to AP and then to cash on
payment — and the inventory-backed PO Approvals and AP aging come off
the page for a tenant without the warehouse module; (2) **the headline
reads the ledger**, where packets already post revenue, because one
source cannot disagree with itself; commerce invoices stay for tenants
that sell goods, and hosting revenue (`8db1d6a6`) posts to the ledger
the way sponsorships do. Not built: `receive-a-bill` exists in neither
this tree nor the tenant's, and the page still reads commerce and
inventory.

**Finance shows the live Stripe balance as a gauge sensor, read
through a `FinancialAccounts` port.** (Design `02894662`, David
2026-11-18, its one question accepted as proposed; answers backlog
`e955e8e9`, the first product outcome of design `d32b58b7`.) The
world's figure for money held outside BOSS is an observation, not a
posting. Decided: (1) **a `stripe-balance` sensor** — one more
`SensorSource` adapter beside `stripe_payouts.rs`, calling `GET
/v1/balance` every 15 minutes — records one reading per poll in
`sensor_readings` (`available` and `pending` per currency, as Stripe
lists them) and **opens no packet**: it is a third kind of source
beside polled and push-only, `GAUGE_SOURCES`, whose readings owe no
packet and are swept by age, because a packet every 15 minutes would
bury the Finance station. (2) **The readings stay sensor readings, not
audit events**, under David's 2026-09-16 rule that sensor data does
not flow into the audit log: a balance read changes nothing about the
company, and the facts that do — postings — already arrive through the
charge and payout packets. The view is a pure function of the newest
reading. (3) **`FinancialAccounts` is the read port in boss-ledger**
(Finance owns money as data), served at `GET
/api/ledger/financial-accounts` from the jobs API's sensor door, and a
bank account later is the same port with another adapter. (4) **The
page is a "Financial accounts" tab on the Finance station's own page,
`/finance?tab=accounts`**, first in order and no new route, because
`01fa5bdf` keeps `/finance/*` as the station's address. (5) **A stale
or unreadable reading is drawn as trouble, never as zero**: no reading
newer than twice the cadence is stale, and a refused read raises
`sensor_unreadable:stripe-balance` naming `balance: read`, so a failed
read cannot pass for an old figure. (6) **The key is the existing
`stripe-restricted-read`** (Q1): Stripe cannot mint a restricted key,
so it is root material David holds, no broker handler is written, and
the registry row's scopes are corrected in the tenant car to name what
the key already reads. **Slice 2 waits on `5c1c435d`**: joining the
book's figure (ledger 1010, live-named "Cash in Transit") beside
Stripe's, with a persisting difference filed as a finance item, would
today show a number the record already disputes. Decided, being built
as `e955e8e9`.

**Closing and reopening a period goes through a reviewed packet, and
the two closes are two protocols** (design `98781bb9`, David
2026-10-13, both questions accepted as proposed; answers `e536e91a`).
Decided: (1) **the monthly close stays the founder-reviewed `/lock`**
that the native `close-a-period` loop already runs (evidence, founder
approval, lock, ledger event), approved in `3613f0af` and not
re-approved here. **The fiscal-year Close is a different act**:
`POST /api/ledger/periods/{id}/close` refuses any period that is not a
year and posts closing entries into retained earnings, with WIP
treatment, so it becomes its own named, reviewed protocol before any
surface offers it. A monthly period is never sent to `/close`, and a
journal effect is never labelled a monthly freeze. (2) **A locked month
reopens only on a packet** that records a nonempty reason and a genuine
founder approval bound to that period's current lock and checksum. An
approval that comes late or is repeated cannot apply to another period,
a newer lock or a substituted request, and an approval that is
declined, missing or dark does not unlock. **A fiscal year whose closing
entries are posted stays refused** until a reviewed correction or
reversal protocol says how those entries are conserved; clearing the
year and guessing the accounting effect is not an option. The UI opens
and advances these packets and shows their state. It never writes
because a button was enabled, and a caller-provided packet id is not
proof of approval at the write boundary. The policy grants on
`ledger-period` (backlog `25a4f7f9`) say who may write; they are not
that approval. Two questions remain open, because both decisions
accepted their "confirm whether" clause as written: whether the
original Close requirement includes the fiscal-year protocol now or
stays an explicit outstanding obligation, and whether reopening a year
is needed now and under which reversal policy. Item `e536e91a` stays
open until both are answered and built. Not built, measured on main
2026-11-13: `TrialBalanceTab.svelte` still calls `lockPeriod` and
`unlockPeriod` directly, with Unlock behind a `window.confirm`; the
unlock event records the actor, the time and the period but no reason;
and this tree holds neither a reopening protocol nor a fiscal-year
close protocol.

**The tax regime is tenant data; the migration's kinds and rates are
the brewery's.** (Backlog `7f163e58`; design `e187198f`, 2026-09-18.)
40-ledger.sql seeds `tax_kinds` (five filing kinds, each FK-pinned to a
GL account: 2150, 2300, 2310, 2320, 6500) and `sales_tax_rate_by_state`
(27 states) on every instance, and nothing else could declare a kind —
the accrual door refuses an unregistered one with "register it in
tax_kinds first". Measured 2026-09-17: because a tax kind naming an
account keeps it, the eviction above had to leave those five accounts
under the brewery's names on every real instance, for ever. Both
tables are now `seeds/tax.toml` in the tenant contract — `[[tax_kind]]`
(kind, liability_account, expense_account?, derive_basis?) and
`[[sales_tax_rate]]` (state, jurisdiction, rate_bps) — published
through `POST /api/ledger/tax/batch` after the chart, insert-if-absent
by kind and by state, one `ledger.tax_kind.declared` /
`ledger.sales_tax_rate.declared` fact per inserted row, a held row that
differs named in the publish line and never overwritten. The
migration's rows are the brewery's seed (`examples/brewery/seeds/tax.toml`)
and example residue everywhere else: the derivation reads the kinds
and states as candidates, a kind is kept only when a `tax_filings` row
names it, and **a kind that is itself leaving keeps nothing** — so the
five accounts are candidates like the rest of the brewery's chart, and
on a bare schema every account goes. `boss tenant check` holds the
FK in the directory: a kind naming an account the tenant's
`seeds/chart_of_accounts.toml` does not declare is INVALID naming the
kind and the code. An empty file is valid and is what `init` writes:
Algedonic has nothing to file until its first sellable SKU (design
`18cf4272`), and until then the accrual door refuses every kind on its
instance, which is the truth. The used-device shop declares no chart
and so no tax; a tenant that files nothing declares nothing.

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

**Inventory value is primary; the average is display.** Every
inventory-bearing row carries `value_cents`; per-unit averages are
derived (`value / on_hand`), shown but never an input to a GL amount.
Adds (receive/produce) post exact line totals; drains consume
proportional value with the final unit taking the remainder, so
`on_hand → 0` forces `value → 0` and nothing strands. Conservation —
`balance(1300/1320) == Σ row value`, to the cent, live and rebuilt —
holds by construction because every mutation's GL amount IS the row's
value delta, and it is **gated**, not discovered: a per-account
GL-vs-physical reconciliation runs in the sim validation and the
nightly integrity timers, so the class is never findable by hand
again. **The consume owns COGS** through the products surface — one
writer on the 1320 credit; a module reaching into another module's
projection with direct SQL (the invoice-issue path once UPDATEd
`finished_product_inventory` in place) is the prohibited shape.

**Posting rules are registry data; the code rules are the fallback.**
(Backlog a40541cb on design 18cf4272, 2026-09-17.) `gl_posting_rules`
is an append-only registry keyed `(fact_kind, version)`: `lines` is
`[{account_code, side, amount_path, memo?}]` with each amount an RFC
6901 pointer into the fact payload (integer cents), `basis` is the
tenant's declared accounting basis (cash | accrual — recorded, never a
code path), `source` is `NULL` or `tenant:<id>`. `DataRuleSet` is the
one active RuleSet: it evaluates a fact by the NEWEST registry rule for
its kind and by `BossRuleSet` when there is none, so a tenant's own
fact kinds are rows in `seeds/posting_rules.toml` (the tenant contract)
and the product's kinds keep their tested code rules. A data rule is
admitted only when its debit pointers and its credit pointers are the
same multiset — balanced for EVERY payload, a decidable check at
publish, refused 422 naming the rule — and the balanced-draft check
runs again at evaluation; that language is narrower than the code
rules' (payroll's `gross = net + withheld` stays in code) on purpose.
**A data rule's version is not a `gl_rule_versions` row.** That table
names the interpreter that ran and `gl_journal_entries.rule_version_id`
keeps pointing at it (BOSS RuleSet v1); a tenant's `version` is its
edition of one kind's lines, written into the entry's memo, and a
newer edition re-projects OPEN periods on rebuild exactly as the code
rules do under `OPEN_PERIOD_FACTS_SQL`, never a locked one. The
event→fact half gained `when` — `{"/pointer": value}`, every pointer
equal for the rule to fire — so ONE workflow's step can become a fact
out of the `step.done.task` every workflow emits; identity of a
projection is `(event_kind, when)`, and `step.done.<kind>` carries
`workflow_kind` beside `spec_slug` for it. Both registries are
published through insert-if-absent batch doors that record one
`.declared` fact per landed row and name a kept row whose declaration
differs. **The projection runs live as well as at rebuild** (backlog
5621d166, 2026-09-17): `boss-ledger-api` binds a durable consumer
(`ledger-facts`) on the platform event stream over the subject families
the rule set names, re-reads the registry every 60 s, and for each
delivered event runs the SAME `project_event` and `post_fact_in_tx` the
rebuild and the domain writers run — one definition of the fact, one of
the entry. Idempotence makes the two halves one system: the fact's id
is UUIDv5 over `(kind, source_table, source_id)` and the entry is unique
on `(fact_id, rule_version_id)`, so a redelivery writes nothing twice
and a rebuild after a live projection reproduces the same rows; the
live path posts only the fact it inserted, because a pre-existing fact
was handled by whoever wrote it — including an operator's supersede,
which drops the entry that a re-post would resurrect. A
projection that fails deterministically (a missing field, a locked
period, no posting rule) is logged with its event id and acknowledged;
only a storage failure asks for redelivery. The nightly
`maintenance-ledger-replay` chore — `boss-ledger-replay-check --deep`,
a read-only replay comparison — remains the reconciliation that names
what the live path missed. Events on families the stream does not
ingest (`products.*`, whose facts boss-products writes in-tx) are
named at start rather than silently dead air.

## Policy & auth

### Guest access is an instance declaration

The operating instance offers no guest sessions; the playground explicitly
opts into the system-audit read. The OSS default is Basic: a `visitor`
session with Read on published workflows and step plugins, widened only
by the install's policy data. Unpublished drafts and company records are
outside that default. Guests never gain write authority. Decision history:
[2830b6b7](https://boss.algedonic.dev/ux/jobs/2830b6b7-ed66-4b58-bf4c-925afa396836),
review completed by emp-david on 2026-09-25.

The instance declaration is one value, `false | "basic" | "audit"`,
rendered as `BOSS_GUEST_ACCESS=0 | basic | audit`. The gateway also accepts
legacy `1` as Basic. Unknown values refuse guest access rather than
choosing a role; the configuration never accepts an arbitrary role name.
Basic mints `visitor`, Audit mints `audit-readonly`, and both belong to
the shared read-only floor. `guest`, the historical internal no-header
identity, is not the browser visitor role.

This is the access contract, not proof that every module enforces it.
The current gateway source still names module reads that lack policy
checks; backlog `cda177ef` owns that remaining boundary. Offering Basic
must not be described as completed isolation of all company data until
those reads have their own refusal evidence. The playground's deliberate
audit read does not excuse a Basic-mode leak.

### What answers without a session is declared, and hardening has an order

**The reads an instance answers without a session are a per-instance
declaration, defaulting to none** (design-doc "Hardening before BOSS is
its own front door", `11e60367`, David 2026-09-18, answering inventory
item `b4afd7b9`). Measured on prod on 2026-09-17: four demo landing-page
reads were a gateway constant and answered anyone on the company's own
instance, with Cloudflare Access the only thing in front. (Q1) The set
belongs to the tenant, as `[gateway] public_reads` in the `tenant.toml`
the gateway already reads at boot: the brewery demo declares what its
landing page reads, and a company that declares nothing gets nothing.
Built in `boss-gateway/src/public_reads.rs`: `PUBLISHABLE` is the closed
table of GET reads a declaration may choose from, a declared path outside
it refuses the boot, and `PUBLIC_BY_DESIGN` holds the few sessionless
routes every instance answers, each with its reason, so one module is
the whole set. The table has held three reads since `19f08bd6`
(2026-09-26): `/api/jobs/summary` is a caller-scoped read a guest is
refused, so a sessionless door onto it opened onto a refusal.

(Q2) **People come in through the gateway with Kanidm OIDC** (the Kanidm
paragraph below) **and off-LAN machines through a service-token door**,
minted per agents row and rotated by a verb; nothing else listens
publicly. (Q3) **The order.** The inventory is seven rows behind Access.
Rows 2 and 3 rode as ordinary cars because they narrow defaults: an
unnamed CLI read no longer sees the whole world (`d843abf2`, 2026-09-19),
and the dev door became an Access SSH application with short-lived
certificates (`e4cedb46`, 2026-09-22). Row 1 is the declaration above.
Rows 4 to 7, the hardening programme proper, are the estate's own SSH
CA, the service-token machine door, the guest and OIDC exposure
measured, and an audit of every door. David schedules them after the six
department first-protocols are live, because going public is what makes
the front door matter, and Access stays until then. The machine door's
deferral to this programme is recorded under §Deployment, with the LAN
machine door. This fold records the order, not the state of rows 4 to 7.

### A denied job read refuses; a partial station view names its gaps

Decided on design-doc "Denied job reads report refusal, and anonymous
station views disclose their boundary" (`6ec8f538`, review by emp-david
2026-10-11). (Q1) A caller whose effective job-read scope is none —
`Predicate::None`, or a grant that translates to `JobScope::None` such as
another department's — is refused 403 by `GET /api/jobs`, naming the
caller and the action and nothing about the packets hidden, as the
station reads already do through `readable_predicate`. No endpoint
answers that caller an empty 200; an allowed scope that matches nothing
still does, and `/api/jobs/live` keeps its own declared public window. A
future exception is a named, reviewed endpoint contract. Readers render
the refusal as a refusal, never as calm: the Incidents page must not
paint "No active incidents" for it. (Q2) An allowed caller with no
identity a per-actor station can bind to still reads the network's load,
and each such station is marked unavailable for that reason, never
dropped and never given a zero depth, so a partial view cannot read as a
complete, idle network. No grant widens.

Not built, measured on main at `f998bdbaa`: `list_jobs` still answers a
none scope with an empty 200 (its comment calls that a clean empty state
instead of 403 noise), and `stations_load` still drops every station
`bind_self` cannot bind. Q1 is item `18933cc6`, Q2 item `68889216`. Wording
a station 403 as a refusal rather than an outage on the marshalling
board is item `7abb184f`.

### Policy decisions and authority

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
invalidation as the convenience overlay. (The packet model's
stage-2 re-key of `Self_`/`Team` onto queue-derived ownership —
§The network substrate — is the one decision that would move this
predicate; it is resolved but unscheduled, and until it lands the
owner-keyed compilation above is the truth.)

**A policy writer grants only what it holds** (backlog b8e75382,
2026-09-25; the rules are `crates/core/boss-policy/src/authority.rs`).
Every write to the policy table authorizes its caller against
`policy-rule`, and is then judged on the row it changes, inside the
write's transaction: a grant of (action, resource, scope) needs the
caller's own decision on that action and resource to allow a scope that
contains it, and it ends no later than that decision does — authority a
user override gives ends with the override, so it grants no role rule
and no override that outlives it. Ending a live override narrower than
`all` early, by retiring it or by bringing its expiry forward, is a
grant at `all`. Break-glass restores a rule core ships, exactly, and
writes no override; it may retire one. A rule id is derived from its
role, resource and action, and a role never carries the `:` that joins
them. Two limits are deliberate and worth knowing. **Scopes are compared
by breadth, not by reach:** `self`, `team` and `territory` are relative
to whoever holds them, so a granter holding `team` may grant another
role `team`, and that grantee's team is its own reports, not the
granter's. **The caller's authority is read before the transaction:**
only the row being written is locked and judged inside it, so a change
to the caller's own grants that commits between the read and the write
— a revocation racing a write — is not seen by that one write.
The reads that answer about a person — `/check` and a user's override
list, whose rows and denies carry free-text reasons — answer the caller
about itself and anyone else only to a holder of Read on `policy-rule`,
never to an anonymous visitor's identity. `/check` is bounded only when
the request is signed (the gateway signs every session's): every
service asks it with no identity of its own, so an unsigned `/check`
and the rule list stay open until callers sign their policy calls, and
a test pins that gap. `check-batch` and `my-scope` had no caller and
were deleted.

**Opening a packet asks Create of the kind being opened** (design-doc
*Create on job is scoped by kind*, `222fc982`, David 2026-09-27, option
(a), "Let's scope it by job"; answers backlog `6dc75abd`, closed
2026-09-27). Since train #734, `POST /api/jobs` asked Create on `job`,
which only platform-admin and break-glass held, so tenant roles lost the
feedback widget and packet creation. Rather than grant every role that
files feedback the unscoped Create, two grants answer, either one
enough: Create on `job`, the all-kinds grant platform-admin and
break-glass already hold, asked first; and Create on `job:<kind>` for
the one kind being filed, which a tenant grants each role for the kinds
its surfaces file (`user-feedback` from every page). A role that may
file feedback may not thereby file an `ops-request`. Plain `job` is the
superset, not a default the kinds refine: to narrow a role, withdraw its
all-kinds grant and grant it the kinds. Built:
`crates/core/boss-jobs/src/open_authority.rs`, pinned by
`create_on_job_is_scoped_by_kind.rs` and the tenant's
`every_role_may_open_what_its_surfaces_file.rs`.

SPA auth is file-backed
credentials managed by the gateway's admin CLI; SSH is
bring-your-own-keys with the SSH-CA flow parked as an opt-in
blueprint.

**The front door for real people is Kanidm** (living contract:
`docs/design/idm-kanidm.md`), on the GCP box rather than in the
cluster — stable public IP, and rebuilding the cluster must not lose
the company's logins. Two invariants make it BOSS-shaped rather than
bolted on. **Kanidm authenticates; it never provisions**: a login
maps to an *existing* employee Subject, joined on email, or it
**fails closed with an audit event** — people enter the company
through the People domain, where hiring is a Workflow with a trail,
never as a side effect of first login (a pending-access Job is a
later nicety, not v1). And **the policy engine never learns Kanidm
exists**: OIDC is another way to authenticate an email, and
everything after the email is the pipeline local login already uses
— roles are read from the employee row, exactly as they are for a
local session. (The design doc proposed an `idp_group_roles`
registry so Kanidm could own membership; the shipped runtime
deliberately does *not* have one, on the ground that it would be a
second source of role truth. The two statements disagree and the
code is the current truth here — see §Open findings.) The
**gateway holds the session**
— OIDC at login only, every downstream service untouched; per-service
bearer validation waits for service-to-service auth to actually need
it. Agents get Kanidm service accounts in **phase 2**: humans first,
agents while the forged-claim header path still works, then that path
dies. Kanidm's own state is the second member of the
outside-git-and-Postgres class (with `credentials.toml`) and its
online backup rides the existing `backup.sh` timer. It terminates its
own TLS at `id.algedonic.dev`, DNS-only, with the gateway's OIDC
callback staying behind the existing front. **Local auth survives as
break-glass** — an IdP outage must not lock operators out of the
system that runs the company.

**The gateway joins the log.** Auth denials were structured warn
lines; with a real front door they are security telemetry, and "who
tried the door" is a company fact. The gateway therefore gains **one
small Postgres pool used only for audit staging**, on the existing
`EventRecorder`/`PgOutboxRecorder` recipe. It was decided to connect
as a dedicated role with INSERT-only rights on `event_outbox` — least
privilege for the one internet-facing service — and that half was
**reversed on 2026-09-28** (backlogs 7ec7113b, d49b4355): the role's
password was its own name, published in migration 111; only the
retired bare-metal drop-in ever set the URL that carried it, so in
the cluster no auth event ever reached the log; and the privilege it
bounded was never bounded in the container, whose launcher hands the
gateway `BOSS_POSTGRES_URL` along with everything else — a URL for
`boss`, the Postgres SUPERUSER the cluster's database is created with
(`POSTGRES_USER` in boss.yaml). The pool now reads that URL, and a
later migration leaves the role unable to log in. So the internet-
facing gateway now holds two live superuser connections (this pool's
two), where before it held the credential but no connection; that
lasts until the gateway gets its own environment — its own process,
or its own pod — which is a deployment change, not a role, and is
filed as its own item. Its unauthenticated doors take an 8 KB body
and a denied login keeps at most 254 characters of the claimed email,
because the claim becomes an outbox row and the relay cannot publish
one larger than NATS's 1 MiB payload. The alternative, an authenticated
ingest endpoint on events-api, was **rejected**: it either reopens
the measured single-writer decision or reintroduces the retired
post-commit-publish shape over an HTTP hop, spending a new
credential class for strictly worse durability. The `tracing::warn`
line stays as the backstop when the bounded queue is full or the
pool is down — **degrade to today's behavior, never to silence, and
never block a login**. Three kinds ship, registered in `event_kinds`
with `source = 'gateway'`: `auth.login.denied` (a closed reason enum,
and deliberately **no subject reference** — no employee matched, and
a reference is exactly what the ref-check trigger would rightly
refuse), `auth.login.succeeded` (carrying `method`), and
`auth.session.guest`. IdP transport failures — discovery, token
exchange, userinfo — stay warn lines: plumbing facts, not
who-tried-the-door facts. **No per-request events**, ratified as a
standing constraint rather than a deferral.

**A refusing gate's would-refuse is a first-sighting fact, inside that
constraint** (design `21946380`, David 2026-10-01, Q2; backlog
`b0787727`). The machine gate and the policy check's mode earn
`enforce` on a 72-hour clean window, and their tallies lived in process
memory, which every train restarts — so each now states, through its
service's outbox, one fact per new tally KEY per process
(`<gate>.would_refuse` / `.refused`), one per overflow and one per
recording start or mode move (`<gate>.recording_began`), never one per
request: repeats stay in-memory counts. That is a who-tried-the-door
fact bounded by the tally's key cap, not a per-request event, so it
does not reopen the line above. The window is a projection over the log
(`boss_core::gate_evidence::window`) AND every live tally's `not_clean`
empty — a fact the log did not take is one of its reasons — and a
refusing flip reads both halves. No fact is forgiven (review
`e4417d48`): one the recorder refuses is retried, one the queue loses
is stated on the log as `<gate>.facts_lost` and dirties the window, no
tally restart clears a loss the log does not yet state, and a process
joins the next one's watch only across a clean `<gate>.recording_ended`
stated at SIGTERM — one that died holding facts breaks the watch.
The same design-doc (*The enforce windows are read from the log, not
from a process*) settled the rule those facts serve. (Q1) A refusing
flip is earned on the log projection, with no would-refuse or overflow
for its gate in the window and every gated service recording in
`report` or `enforce` across it, AND on every live tally's `not_clean`
empty. A persisted tally table was rejected as a second mutable store
the log cannot rebuild, and N clean process lifetimes as evidence a lost
lifetime erases without a record. (Q3) The 72-hour clock started only
once the projection was live in `report`: time watched by in-memory
tallies alone earned no credit, because none of it is in the record.
Presentations of the `previous` slot are tally keys too, so the
self-issued rotation's drain reads the same projection.

**A window requires the services this deployment launched, read from
the launcher's own record** (design-doc *Gate evidence covers the
authoritative deployment profile*, `3cc6152a`, David 2026-10-05, Q1
accepted as proposed; answers backlog `b0787727`). The required set
had been every gated row of `boss_ports`, so the six services the
Algedonic, LLC instance deliberately does not run (assets, catalog,
inventory, shipping, simulator, sim-control) were six permanent gaps
and the machine-gate window could never read clean. The port registry
stays the inventory of what the software can run; what this instance
must run is what `infra/oss-quickstart/services-launcher.sh` selected
from the tenant manifest's modules and `BOSS_SIM_ENABLED`, written as
a `start`/`skip` line per binary to `BOSS_LAUNCH_RECORD` — one
definition, so the launcher and the requirement cannot disagree. The
record can only EXCUSE, and every excuse is checked: a skipped service
whose port accepts a connection, answers its tally read or has a gate
fact in the window is not clean, and a record that is missing,
unreadable, truncated or names no decision for a service excuses
nothing (`roster_errors`; every service then required). No caller
narrows it: `/api/events/gate-window` takes only `gate` and `hours`,
and a required service that is down stays required and dirty. Its
provenance is on the log: every machine-gate and actor-role
`recording_began` carries `roster` — the sha256 generation of its
launch record's decisions (reasons and order excluded) and its image's
commit, or why it has none — and a window whose hours span two
generations, or whose required service's running process stated
another or none, is not clean and names both (`judged_generations`),
so a new selection never inherits an old clean window and the
mechanism's first boot did not backdate itself. Operating rule: any
change to the launcher's array or the tenant's modules is a new
generation and restarts every counting enforce clock. The policy check
consults no launch record. Built, all three closed: backlog `93e0814a`
(the required set from the record, car `b43f2a32`, train #971),
`14fe115c` (the generation, car `35baf5b0`, #1004) and `b13bd730` (the
actor-role window bound to the same roster, car `5e77881a`, #1018).
The self-issued rotation's revoke, the refusals and asked alarms
(through `gate_window_read`) and the flip's judge
(`infra/ops/gate-window-is-clean.sh`, which requires exactly one
stated generation) read that one answer. Read live 2026-11-13T04:40Z:
21 services required, the six excused with their recorded reasons and
none listening, one generation, `not_clean` empty, and the 72 hours
not yet covered (clean since 2026-11-10T15:01Z). The design authorized
no enforcement flip, rotation or grant, and those stay on their rows.

**A profile adoption and a machine-token mutation are to reserve
through one owner outside the jobs API; none of it is built**
(design-doc *Fence BOSS deployment adoption through an independent
owner*, `51c69035`, opened by agent-codex, David 2026-10-06, Q1
accepted as proposed). It extends the decision above and changes none
of it. Decided: an independently supervised deployment-transition
coordinator on the forge, with its own persistence port on the
existing database, never calling the jobs API before it acts. Converge,
both rollbacks (the deploy lib's and the watchdog's) and the launcher,
before it starts the selected services, reserve an adoption through it,
and the self-issued rotation reserves its mutation through it. The two
never coexist, and an unknown external effect stays unresolved until
bound evidence resolves it. A reservation carries the complete selector
artifact (image, manifest, launcher definition, simulation inputs) and
the original it replaces, so a rollback restores the whole artifact,
never an image alone. While the owner is down, only a same-profile
restart proven against a complete last-converged receipt proceeds; a
profile-changing rollout or rollback holds. Its callers present a
broker-issued credential of their own family, never the estate token it
fences. The design authorized no grant, credential, issuer or
activation. Not built, measured on origin/main `375a3f75` 2026-11-13:
no coordinator, port or `deployment.adoption.*` event exists;
`cluster-deploy-runner.sh` runs `converge_tenant` before
`apply_instance` with no reservation; both rollbacks name an image
only; the self-issued rotation reads the joined gate window and
reserves nothing. The broker has no issuer for the coordinator's
credential or database role, which the design names a prerequisite.
The Pg deployment-selection publication the design was read against
never landed; on main the roster generation above detects a changed
profile after the fact, which is detection, not a fence. The build is
item `ac2084b6`; item `3ff7b8eb` (design `2afd8e21`) names it a
prerequisite.

**That window sees only the listeners the registry names; a complete
listener observation by the instance itself is decided and not built**
(design-doc *Authenticate complete BOSS instance listener inventory*,
`2afd8e21`, David 2026-10-06, Q1 accepted as proposed). The tally
reader's targets are `boss_ports::all()` (`LocalTallies::at_declared_ports`),
so it can refuse a missing known service but cannot discover another
socket: on 2026-10-06 a real private listener started in Off mode with
no recording facts, and the coverage read still answered
`covers_requested_window=true` with no input error (builder run
`ee53b974`). Decided: a replaceable `RuntimeListenerInventory` port
whose native adapter runs in the instance workload and observes its own
network namespace, under a verified instance principal mapped to its
Pod and namespace (never the shared estate token plus an asserted
actor). It records complete IPv4 and IPv6 TCP LISTEN capture, its
bytes, interval and parse; missing, truncated or ambiguous evidence, an
owner the delivered artifact's versioned listener data cannot bind, and
a changed deployment generation each refuse coverage. A familiar port
or a self-reported name is not ownership. An unknown socket is a
discrepancy and gets no machine token, actor header or authenticated
probe. The trust accepted is the co-resident workload's honesty; an
independently isolated observer would need its own decision. Built:
only the source half, the tree-wide pin
`every_service_mounts_the_machine_gate.rs`, which inventories serving
calls and unaccounted TCP binds by syntax, with the tenant's loopback
callback declared in `crates/tenants/boss-brewery-engine/listener-inventory.toml`
(car `83cf7eab`, 2026-10-14), and which states it is not runtime
evidence. Not built: the port, its adapter, the instance principal and
the ownership data. The build waits on two accepted designs, both
folded here and neither built: the adoption owner (`51c69035`, above)
and lossless observation retention (`d4bb88fe`, below). This decision
grants no issuer, credential, deployment or activation; the build is backlog `3ff7b8eb`.

**A machine-door request is judged as the role its registry row holds,
never the role its header asserts** (design-doc *A service reads the
caller role from the registry instead of believing the header*,
`abf9eeae`, David 2026-10-01, both questions accepted as proposed;
answers backlog `ddf0773e` and folds in its duplicate `91971374`).
Measured then: `boss-cli`'s identity header asserted platform-admin for
every named caller, 57 files emitted a platform-admin `x-boss-user`, and
every service believed it, while the gateway already resolved a browser
session's role from the employee row. The machine token proves THAT a
caller is an estate machine, not which role it holds, so this is the
other half of `6805c764`. Decided: one resolver in boss-core behind a
port, `RoleOfRecord`, mounted wherever the machine gate is, makes the
effective role on the machine door the role of record: the agents
registry for every actor that is not a person, the employee row for a
person. Every writing `automation:*` id got a registry row at the role it
already used, so registering changed no one's privilege, and an id with
no row gets the read-only floor once the resolver enforces.
Gateway-forwarded requests keep the gateway's resolution. (Q1) **An
agent acts with least privilege**: it is signed and judged as its
registered role, engineering-agent for agent-claude, and that role's
grants are written from 72 hours of the report tally, exactly what
agent work used; anything an agent should not hold is filed as its own
item rather than granted. (Q2) **Enforcing is row H of `b08725c2`'s
refusing order, after G3**, behind its own `off`/`report`/`enforce` word
beside the machine-gate mode, on a Pacific-hours train, once the tally
has run 72 hours with zero would-deny for every registered actor and
zero unregistered writers and 24 hours after G3; its way back is that
word set to `report`. It follows C and D, because before them any LAN
caller could assert any id. After H has run clean, the claim gate's
extra agents-row admission and the role in client headers are deleted.
The two paragraphs below amend where the report's window lives and what
a gateway-forwarded User must carry under H. Built: the resolver and its
report (`boss_core::role_of_record`, `role_reader` and `role_service` in
`boss-policy-client`) and the registry rows, pinned by
`every_writing_automation_holds_a_registry_row_pg.rs`. Not built: row H,
whose item `ddf0773e` is open.

**The actor-role report is the third gate on that reader, and its
window is judged per process** (backlog `e0bdba74`, 2026-10-08; amends
point 4 of design `abf9eeae`, which called the tally "telemetry, not
audit-log events"; design-doc *Where the actor-role report window
lives: on the event log, or in a telemetry store beside it*,
`da2234b8`, David 2026-10-08, both questions accepted as proposed,
chose the log over a telemetry table with a second reader, and train
#1000 built it). Row H earns `enforce` on 72 hours with zero
would-deny for every registered actor and zero unregistered writers,
and that tally was process memory in a pod seven trains restarted in
twelve hours: `durable_window` was a constant `false`. It now states
`actor_role.*` facts by the same rule and through the same machinery
(`Gate::ActorRole`): a start and every mode move, one fact per SHAPE
`enforce` would answer differently at its first sighting in a tally,
one for the first overflow — never one per request, and none at all for
a request whose asserted and recorded answers agree. A shape is the
tally's own key: the actor id, the asserted role, the role of record,
the door's action and resource names, the lookup status and the two
answers; no header, credential, body or path. What bounds the facts is
what already bounded the tally — 512 shapes and 4096 bytes of caller
text a shape, per tally start, then one overflow that names no one and
is never clean. One rule (`gate_evidence::actor_role_would_refuse`)
says which shapes those are, for the producer and for the reader that
re-judges every live row: `would-deny`, `would-change-scope`,
`unregistered-writer`, and `unjudged` — a comparison that could not be
made is no pass. The window is read at
`/api/events/gate-window?gate=actor-role`, over every service the
launcher started, and each service's own report answers its own
window, `durable_window` true only when the log was read. The gateway
states no such facts: its one database pool is the audit sink of the
internet-facing process, so it mounts `role_service::unrecorded_tally`,
and the design's second answer left its wiring to a follow-up car,
the estate window reading not clean until then. Read live
2026-11-13T09:07Z, the window names it ("no `actor_role.recording_began`
from `gateway` on the log: it has never recorded here"), and the events
reader's read of the gateway's report answers 403. No item or car
owned that follow-up; it is item `fc884927`. Two things
it holds that a merged reading does not: every live process must
answer and match the newest start the log holds FOR THAT SERVICE, so
one service's clean restart never stands in for another's long-lived
process; and a shape first seen before the window by a process that
ended inside it is not read as unused, because a first sighting never
says when the last use was. A restart across a clean end does not
reset the clock, nor does the mode word moving between words that
record; `off`, a process that states no clean end, a gap longer than a
restart, a lost fact and any would-refuse do. It refuses nothing: the
car that makes `enforce` refuse declares `actor_role.refused` then.

**Under row H, a gateway-forwarded User is kept only on a signed
delegation from a named shared workload; none of it is built**
(design-doc *Verify shared-workload identity delegation before role
enforcement*, `ec695ecd`, opened by agent-codex, David 2026-10-06, Q1
accepted as proposed; it answers a gap in `ddf0773e`, the item that
owns row H). Design `abf9eeae` lets a gateway-forwarded request keep
the gateway's resolution, and nothing can tell such a request apart:
the gateway strips client `x-boss-*` headers and forwards the
session's full User in `x-boss-user` beside the estate machine token,
which every estate caller holds, so neither the header, the token nor
a loopback address proves who forwarded it. Decided: a
**shared-workload issuer**, named as such. Its signature proves that
the declared BOSS workload asserted the User, not that the gateway
process did, and every process that can read the signing key, plus
the administrators of that workload, is in its trust base. The key
family is asymmetric and broker-issued (a new issuer handler,
`resourceNames`-scoped placement, never placed by hand), with public
verification material mounted in each consumer. The envelope binds
the complete User, the session identity, issuer, audience service,
deployment, key generation and a lifetime of at most 60 s with at most
2 s of future skew. It travels only to literal-loopback upstreams in
the same Pod, with proxies and redirects disabled; any other hop
refuses until authenticated confidential transport is separately
approved. Method, path, query and body are not signed, there is no
nonce store, and a replay within the lifetime is allowed: each request
still passes its own policy, ownership, presence and idempotence
doors, and a delegation carries no approval. The verifier runs before
role resolution. An invalid assertion never establishes provenance,
and an absent one takes the ordinary machine path. The stronger
alternative, isolating the gateway's signing workload first (its own
ServiceAccount, session key and broker roots), was not chosen. The
decision approves a trust mapping only, and no grant, Secret,
deployment or H flip. Not built, measured on origin/main `2dbd868b`
2026-11-13: no `WorkloadIdentitySigner`, verifier or
`VerifiedWorkloadDelegation` exists; `role_headers.rs` still forwards
`x-boss-user` and the estate token; the broker has no issuer for the
key family; and row H cannot refuse (`role_reader.rs` maps the
`enforce` word to `report`, and `boss.yaml` sets `report`). The build
is item `8460533e`, and row H's enforce car on `ddf0773e`
waits on it.

**An automation reads at a tier only when its own row records one, and
reading a draft is a grant of its own** (design-doc *Qualify the
movement observer with explicit read-only authority*, `600c07d7`,
opened by agent-codex, David 2026-10-17, its one question accepted as
proposed; answers backlog `f21804d8`). The movement observer
(`automation:yard-mover`) must read the complete graph: job Read,
estate Read, workflow Read, the drafts, and the machinery reads, which
ask an Operator-or-Auditor tier (`trust::can_read`). The automation
registry recorded a role and no tier, and the one tier rule there was
infers Operator from an agent's role. Decided: a role is not a tier.
An automation row may record one tier, Auditor, the existing read-only
machinery tier, which admits the machinery reads and refuses their
writes and overrides no policy leg. It is resolved afresh for the exact
configured id: never a family member's through `signs_for`, a header's
claim, a borrowed reader or an inference from the role, and a missing,
dark, changed or revoked row refuses. Reading an unpublished draft,
which needed Create, Update or Publish on `workflow`, gains a read-only
leg, Read on `workflow-draft`, granted to no visitor, ordinary reader,
agent or auditor by default. It confers no authoring, and every owning
draft read asks the same four legs. The capture is never narrowed to
dodge a check: an observer without the draft grant stays unqualified.
Existing callers and author access are unchanged. The design approved
the mechanism only. Enrolling the actor, publishing its tier or grants,
credentials, producer activation and the native proof (full capture,
catalog, the 720 proof and the 42 historical UNKNOWNs) are separate
recorded operations.

Built (train #1242, 2026-10-18): the nullable `automation_actors.access_tier`
column, whose CHECK admits only `auditor`
(`20261018020000-an-automation-row-records-its-read-tier.sql`), and
`RecordedTier`, `recorded_reader` and `recorded_reader_again` with
their port forms in `boss-jobs/src/agents/automations.rs`, re-resolution
refusing a changed role or tier or a missing row. The platform seed
inserts a tier only with a fresh row, on its `automation.declared` fact,
and never stamps over or revokes a held one. The `READ_WORKFLOW_DRAFT`
control is the fourth leg of `http/kinds.rs::may_read_drafts`, read by
the kind and station draft reads. Its only default holder is
`platform-admin`, pinned by `only_platform_admin_reads_workflow_drafts_by_default`.
Not built, measured on origin/main `ebed6540` 2026-11-13: no bundle row
for `automation:yard-mover`, and no row records a tier. No door records
or revokes a tier on a held row, so no event yet states a tier change.
No production caller resolves through `recorded_reader`: the in-process
mover (`http/moves.rs::run_mover`) still builds its caller in code at
the User tier. A native producer adopting the resolvers before
discovery, capture, acceptance, replay and reconciliation has not landed.

**One audited door records or revokes an automation row's read tier, and
recording it is a passkey act; decided, not built** (design-doc *One
audited door records or revokes an automation row read tier*,
`308ccde1`, David 2026-11-14, its one question accepted as proposed;
answers backlog `fdc9901d`). Only the seed's insert writes
`access_tier`, so qualifying a held row such as the movement observer's
would be a hand SQL edit that states no event. Decided: a jobs-API route
on the registry that owns the table, `POST /api/automations/{id}/tier`
with `{"tier":"auditor"}` to record or `{"tier":null}` to revoke, behind
a CLI verb `boss automations tier <id> auditor|none --why <text>`. It
writes that one column on that one row, in one transaction with its
event and conditional on the prior tier, and reads the row back; another
field, an id the registry does not hold, a value `RecordedTier` cannot
name, or a no-op refuses. Two declared kinds, `automation.tier.recorded`
and `automation.tier.revoked`, carry the id, the old and new value, the
actor and the why. The seed stays insert-if-absent, a tree-wide pin
names any other writer of the column, and readers re-resolve through
`recorded_reader_again`, so a revoke takes effect at the next resolve
with no cache added. It is not an ops verb, because the jobs API is the
table's one writer and its own grant is the narrowest that does the act.
(Q1) **Record is a passkey act and revoke is not**: recording grants
read authority, so it is approved on the website with David's passkey
over a plan naming the row and the old and new tier; revoking narrows,
which never widens exposure, so it runs on the door alone and records
its event. Not built, measured on origin/main `b0aa242e2` 2026-11-17: no
`automation.tier.*` kind or door exists, and
`infra/platform/automations/README.md` still says there is none; item
`fdc9901d` is open.

**A judged gate observation is kept only as prose; retaining it whole,
as immutable chunk facts behind a verified manifest, is decided and
not built** (design-doc *Retain complete gate observations as bounded
immutable evidence*, `d4bb88fe`, opened by agent-codex, David
2026-10-06, Q1 accepted as proposed). A reader of the windows above
takes the joined window whole or not at all, up to
`gate_window_read::MAX_RESPONSE_BYTES` (64 MiB), and then writes a
sentence about it: `policy.check.refusals.alarm`,
`machine.gate.asked.alarm` and the self-issued rotation's revoke each
record their verdict in prose. The window's facts stay in `audit_log`;
its live tally reads, roster and probes do not, so a recovery cannot be
re-read later as the evidence it was judged on. Copying the observation
onto the alarm was tried on a branch that never landed (`8b536e67` to
`e188d625`, 2026-10-06): an 1800-fact window the reader accepts made
the recovery write answer 413, and one event is bounded below any
request by `MAX_STAGED_EVENT_BYTES` (983040, `boss-events/src/outbox.rs`),
which stays. Decided: the complete canonical observation is appended
as ordered chunk facts, each a whole event under that bound, then one
manifest fact binding the encoding, total bytes and SHA-256, every
chunk's id, length and hash, the original publication and the
observation's purpose and interval; the alarm or recovery carries only
a reference to the manifest. Chunks without a verified manifest are no
evidence and no recovery is claimed before it closes; an identical
retry writes nothing and a conflicting one refuses; reconstruction
reproduces the exact bytes or refuses a missing, reordered, duplicate,
foreign or truncated chunk. The design found today's content
attachments are not that contract (finalize trusts the supplied hash, a
detached object is collected after 30 days, a signed PUT can
overwrite), so the train diagnostics' chunked content transport
(`boss-cli/src/train/diagnostic_adapter/artifacts.rs`) is not the reuse.
It granted no writer, reader, event kind or endpoint; those are
established with the build. Not built, measured on origin/main
`2dbd868b` 2026-11-13: no chunk or manifest event kind, and no
reference on any alarm. The build is item `702c8851`; the runtime
listener inventory (item `3ff7b8eb`, design `2afd8e21`) needs it for
its capture retention.

**One actor, one identity: an agent's login resolves to a registered
id the way a human's does** (design `6fda05ae`, David 2026-09-12; all
three questions accepted as proposed; folds backlog `adf025df` and
`7dd9f28c` into one decision). Measured: the same CPU was
`claude@algedonic.dev` on `steps.assignee_id`/`completed_by` and
`claude:opus-5[1m]` on `agent_runs.actor_id`, so the question the
agent-runs module exists to answer — what did this actor build, at what
cost — could not be asked, and `ActorId` carried no address arm at all:
the address was live data the vocabulary did not describe. The human
side already had the answer — `boss-gateway/src/oidc.rs` resolves a
login to an `emp-*` id before any write is signed and fails closed on
no match — so an address surviving to a step meant that resolution
never happened for agents. Decided: (1) **the canonical id is an
emp-style opaque id in an agents registry** (`agent-<slug>`), never the
login address and never the model-qualified colon form — addresses are
logins (aliases), models are facts about a run; (2) **the model lives
on the run** (`agent_runs.model`), the agent row carrying its default
model and its caps, because one registered agent runs different models
over time and cost is priced per run; (3) **an unregistered agent login
is refused, failing closed**, exactly as a human login matching no
employee is — after a one-release migration window in which an alias
table maps the one live address to its agent and a lint counts writes
still arriving under an alias. Budgets (`AgentSpec` caps, a decision
recorded before a run starts) sequence after the run is a recorded
fact. Car 1 (registry + alias + resolution, window open) building
2026-09-15.

**Which model reviewed a car is the launcher's record, never the run's
word, and a FORMAL review is not yet bound to it** (design-doc *Bind
formal reviewer provenance to the executor launch*, `5155f530`, Q1
decided as proposed by David 2026-10-04, answering urgent `ad82090a`:
two reviewer runs declared `opus-5[1m]`, ran `gpt-6-sol` and wrote
RELEASE on cars a train then boarded; sequenced by design-doc *Refuse a
release when the reviewer model differs?*, `fb9ba887`, David
2026-10-09). The Workflow's preferred model, the configured launch and
the observed inference are three different facts, and a rate card
pricing a model observes nothing. Decided: a manual, externally spawned
reviewer may make a FORMAL release only on a trusted host/tool launch
receipt bound to its run, exact claim, configured model and execution
handle — never a parent's self-attestation, an environment variable or
caller metadata; until the launcher writes that receipt a mismatch is
recorded and not refused (`fb9ba887` D1), and once it does, only the
launcher's record counts (D2). Built: the step agent block's
`executor_provenance` word (`advisory` | `verified`), the immutable
`executor.source_claim` and `executor.binding` first records and one
predicate, `executor_attestation::executor_eligibility`, read by `boss
review`, `boss release` and train boarding alike (train #955,
2026-10-05); agent blocks naming the model that runs (car `acfd53d0`,
train #1036); and the launcher's pre-claim `launch.model` record —
written by `boss dispatch --launch`, and by `--from-hook` from the Agent
call — which a release now states as "declared, launched, observed"
(`car_release::reviewer_model`, train #1045, item `e6b05dd6`). Not
built, measured on origin/main 2026-11-13: the only production verifier
is `UnavailableLaunchReceiptVerifier`, which refuses every receipt;
nothing outside tests writes the source claim or the binding; no
authored workflow declares `verified`; so every FORMAL review is
advisory and the model is said at release, never judged. And a run
claimed before its Agent call, as `boss dispatch --next` claims it,
gets no `launch.model` at all and reads launched unknown. The refusal
D1 deferred to D2 is backlog `ff17de8f`.

**The source claim is captured in the claim's own commit, declared by
the protocol and never rewritten** (design-doc *Capture declared claim
evidence atomically under the admitted protocol*, `c0e07e34`, opened by
agent-codex and answered by David 2026-10-05, both questions as
proposed, answering `ad82090a`). It decides who writes the
`executor.source_claim` that the launch receipt above binds. A record
written after the claim could be missing for a claim that stands, and
one written before could exist for a claim that was refused. Decided:
(1) a versioned step row may declare `claim_records`, and only the
workflow publish authority declares them. Each record's values come
from a closed vocabulary of observations the server makes itself: the
admitted job, the step, the protocol kind and version, the resolved
claimant and the claim stamp. Source and configuration come from the
original admission envelope or an authenticated observation port, never
from mutable metadata. If a declared observation is missing, the claim
is refused before anything changes. The claim, its records and their
events commit together, or none of them do. Core claim code never names
the executor key; the executor adapter maps the generic record onto
it. (2) `conserve_original`: an equal retry returns the first receipt
and records no second event. A changed holder or binding is refused,
and a new execution is a new run. Not built, measured on origin/main
2026-11-13: `claim_step_displacing_at` takes no record, and no step row
declares `claim_records`. The build is item `a84ebe76`, the
source-claim half of `ff17de8f`. It extends the specific-step claim
door, which is live. The native selection commit that `4e61511c` binds
is not served until `1f04d9c6`.

**A run reported as a bare total is priced at a declared blend, and
says so** (design `91a9bfe7`, David 2026-09-20: "The blended rate for
the cost of runs sounds fine"; all three questions accepted as
proposed). Measured: 2 of 85 recorded runs carried any cost, because
only an input/output split was priced and the harness that runs a
dispatched builder reports one total — so both budget desks enforced
their caps against about a fiftieth of the spend. Decided: (1) **the
assumed input share is a per-model column on `agent_rate_card`**
(`blended_input_share_ppm`), declared rather than hardcoded, NULL
meaning undeclared and therefore unpriced; one row carries it,
`opus-5[1m]` at 875,000 ppm, the only model with measured splits behind
it; (2) **every figure names its basis** — `split` or `blended`,
derived from the run's own token shape rather than stored — carried
into every roll-up, where a bucket holding one blended run reports
itself blended, the same rule that already makes a bucket with an
unreported run answer `None`; a measured split always wins; (3) **the
83 runs recorded before it stay unpriced** — marking them blended would
record an assurance nobody has, so the series starts where the pricing
does.

**A run's spend is read per protocol and step, a helper is charged
once, and a 1-hour cache write gets its own price** (design-doc *Read
per-protocol spend with verified helper attribution and cache
lifetimes*, `091e8034`, both questions decided as proposed by David
2026-10-11; answers backlog `05bf580b`). It extends the metering of
the next paragraph, "A run is priced from what it consumed", and
reopens neither of its rules: an unknown price stays unknown, and a
budget stays a reading. Decided: (1) `agent_rate_card` gains a nullable
per-model 1-hour write rate, its provenance the provider's dated
official rate; new usage records split cache writes into 5-minute and
1-hour counts that must sum to the total; an unclassified lifetime or a
missing rate leaves the price unknown; a recorded price and its floor
are never rewritten, and publishing code changes no live rate. (2) A
helper a run spawns keeps its own usage record and native model, tied
to its parent by a verified execution receipt; direct and inclusive
readings are shown apart, an inclusive sum charges each usage record
once, and a missing edge, an ambiguous overlap or an incomplete set of
children makes the inclusive figure unknown and names why. The plan is
three cars, all on item `05bf580b`: a per-protocol read grouped by the
job's kind and the run's `detail.step`, with an explicit unattributed
bucket; the helper edges; then the rate. Measured on origin/main and
live 2026-11-13, none was built: `/api/agent-runs/cost` grouped by model
and branch only, and an unmatched `workflow_kind` answered the same 812
runs (444 unpriced) since 2026-11-06 as no filter. The read half of the
first car has landed since: the cost read answers `protocols` — the set
by `jobs.kind` and `detail.step`, with `no_job`, `unknown_job` and
`no_step` buckets — and `workflow_kind` scopes it, so an unmatched kind
answers zero; rendering it on the workflow page is still to build. Of
the other two, as measured: the card's 13 rows carry one write rate each,
and `cache_write_1h` is counted but priced at the 5-minute rate; the
meter reads exactly one transcript per run, so a helper's turns reach no
parent. The receipt a helper edge needs is not written either: every
launch record sets `parent_runtime_id` to null, and the launch-receipt
verifier in the formal-review paragraph above still refuses every
receipt.

**A run is priced from what it consumed, and a budget is a reading,
not a limit** (backlog `e6b2066f`, David 2026-09-23: "the goal is the
PLUMBING to understand costs … we do NOT want budgets to limit
building"). Measured: the bare total the blend above priced was the
harness's `subagent_tokens` — the run's FINAL context size, within 1%
of its last turn on 62 of 68 runs — while its summed per-turn tokens
were a median 48x larger, 96.8% cache reads; real spend was a median
4.9x the recorded figure. Decided: (1) `boss dispatch --report` meters
a run from its subagent transcript — the four counts (uncached input,
cache write, cache read, output) of every turn's `message.usage` — and
the typed count rides `detail` beside it; (2) `agent_rate_card` prices
cache reads and writes as data (0.1x and 1.25x input, the 5-minute
write rate, so a 1-hour write is a floor), basis `metered`; (3) the
`opus-5[1m]` blend is retired, so an unmetered total is unpriced rather
than about five times low; (4) an over-cap claim is admitted and
`agents.claim.over_budget` rides the log beside it, and an over-cap run
is recorded with its `budget` reading `deny` — neither refuses.

**A native runtime roster enters the record as an explicit, dated
snapshot, and an open run is not evidence that an executor is running**
(design-doc *Observed runtime roster on the BOSS Shop Floor*, `aad7c419`,
2026-10-02, recorded as engineering decisions with no open questions;
answers backlog `daa8f63f`). A capture is a `runtime-roster-capture`
packet filed by `boss roster record` from an operator-copied native
call/output pair: source, evidenced namespace, collection interval,
receipt time, the raw response and normalized rows ride a completed
`capture` step, so they are immutable. A malformed or incomplete read is
a `failed` capture, never an empty healthy roster; an equal capture-id
replay is idempotent and a conflicting one refuses. A native path is an
alias reused across tasks: it is never joined to a BOSS run or read from
completion prose, current work stays unknown until an assignment epoch
and an attributable start acknowledgment tie a native instance to one
claimed step, and no heartbeat, observed model or billing is made up
from coordinator interaction (the run meters above stay the only spend
record). Measured 2026-11-12: the protocol and the verb are built,
pinned by `boss-cli/tests/runtime_roster_capture.rs`; two captures exist,
the last on 2026-10-03. The presentation was built and then retired with
the Crew Board (item `740b3fb6`, train #1636, 2026-11-08), and no station
view draws a capture today. Not built, and held open on `daa8f63f`:
showing captures beside a station's Working rows, the current-task
mapping, and a continuous adapter — which needs a supported runtime
bridge and its own reviewed design before it adds a trust boundary.

**A destructive change is approved by the founder's passkey over a
rendered plan and executed by the machine** (design `17835005`, David
2026-09-21, all five questions accepted as proposed). The occasion was
commissioning the forge's new disk: a second NVMe renumbered the devices,
and the drive that looked new by number was carrying the system —
approving "format nvme0n1" would have approved destroying it. So **the
guard matters more than the approval**: bounded verb, machine-checked
preconditions, plan, passkey-bound approval, recorded execution, the
passkey last of the five. Decided: (1) **the signature binds a rendered
plan** — the target resolved by-id, preconditions evaluated live on the
host — never a verb call; (2) **a runner never trusts an approved
field**: the system of record issues a short-lived single-use
capability, host-side verification of the enrolled key being later
hardening, and the weakness stated plainly — this trusts the SoR;
(3) **eligible is a declared verb whose dangerous outcome machine-checked
preconditions exclude**, however destructive; a verb that needs someone
to eyeball which thing is meant is not eligible behind any approval —
the line moves from who acts to who decides, and stays sharp; (4) **an
approval is single-use, minutes long, consumed when the runner begins
and voided by drift**; (5) `commission-a-disk` is the first verb.
Landed: read-only planners `plan-a-disk-commission` and
`plan-a-tenant-merge`, which evaluate exactly the write path's
preconditions and refuse (exit 78) what cannot run; the mutating
`commission-a-disk` and `merge-tenant-main`, each declaring
`requires_approval` and re-checking immediately before writing; and
ops-request's `approve` step, the first presence-assured step in the
system — the plan rides in that step's metadata, so the WebAuthn
assertion over its shape hash signs the plan's bytes with no second hash
to keep in agreement, and an edit after the ceremony voids the stamp.
Then the runner half (backlog `fd7090cc`, 2026-09-24): `boss ops` files
an approval verb with `requires_approval` and without its hash; the
runner renders the verb's declared `plan_verb` on the host onto the
approve step, and closes the request refused, with the plan verb's
words, when that refuses; it runs the write only when the approve step
is completed with a presence stamp for every required role, bound to
the step's current shape hash (recomputed on the host and pinned equal
to the server's), signed within ten minutes; it claims `execute` active
before the argv runs and never runs an active one; and it hands the
write sha256 of the SIGNED plan as `plan_sha256`, so the write's own
re-render refuses drift. q2's weakness stands as stated: the runner
trusts the SoR's sign-off record. `commission-a-disk` stayed inert until
its script took that hash (backlog `b2d5b546`, 2026-09-26): its write now
re-renders the plan with the one command `--plan` uses and writes nothing
unless the bytes hash to the approved value. The car's security review (2026-09-24)
held it for seven further tightenings, all built: the stamp must be by
an employee the verb file names in `approvers` (03451237 q2's named
list, not a role); `step_shape_hash` JSON-encodes its keys, in Rust and
in the runner's jq, because raw keys let `{"zz":1,"zzz":2}` and
`{"zz:1,zzz":2}` share a canonical form; the runner signs the verb,
host and args with the plan and the hash of the bytes it rendered, and
compares the request to them before building an argv; it re-renders the
plan verb before claiming and refuses a signed plan that is not those
bytes; it re-reads the job immediately before a claim that is a
compare-and-set through the claim door, signed as a claimant unique to
the pass; an execute left `active` records "claimed, outcome unknown"
on the request and is never closed `refused`; and an arg carrying a
control character is refused, because jq's `$` matches before a
trailing newline. The re-review (2026-09-25) found the claim could
never succeed live: the dispatcher nominated every ops-request execute
to the agent executor ~50ms after ready, and the claim door admits a
ready step only unheld or to its holder. ops-request's execute is now
`claimable` (a role queue, never a nomination — not a fixed runner
assignee, because the door is idempotent for its holder and single-use
needs each pass a different claimant); a held execute is refused by
name, and "claimed, outcome unknown" is recorded only for a holder
shaped like one of the runner's own passes. The presence ceremony now
names the step content the surface rendered, and the gateway's
`assert_begin` refuses (412) one that does not hash to the step as it
stands. That guards an HONEST page from a swap between its render and
the key press; it does not prove what was on screen, because the page
supplies the content it names. The sign-off surface shows a
presence step's signed document (the `plan`) read-only and verbatim,
never as a text input that drops its newlines. The adversarial
re-review (2026-09-25) found a REJECTION ran the write: Reject runs the
same ceremony as Approve, so a rejected plan arrived as a completed
approve step with a valid bound presence stamp. The runner now requires
the approve step's `decision` (inside the signed shape) to be exactly
`approved`, and ops-request's execute is ready only on that decision;
any other decision, or none, readies the `refused` terminal. Still
open: the approve step's runner-written keys
(`plan`, `verb`, `host`, `args`, `rendered_plan_sha256`) are writable by
anyone who can write the step — the runner's re-render and the ceremony
binding refuse a swap, but do not prevent one.

**A tenant merge's passkey is asked only over an independent review of
the same plan** (design `9de0cd1c`, David 2026-10-04, its one question
accepted as proposed; answers backlog `84e7c3e4`). This narrows the
passkey route above for one verb. The approval proved who decided and
which bytes they signed, and nothing about whether anyone but the
branch's author had read them: the BOSS car reviewer resolves a
ship-a-change car on the BOSS forge, and a tenant builder ships none,
so the review was an operator's convention the next shift could not
read. Decided: a closed independent RELEASE of the exact rendered plan
is a machine-checked precondition of the approval becoming ready, and
is checked again before the claim; an absent, open, unreadable,
`changes`, self-authored or moved review refuses before any mutation,
and anything the plan names moving needs a fresh review and a fresh
approval. Single use, the ancestry check, the main lease and the
effect readback stand as they were, and the review grants no write,
publish, export or standing authority; tenant publication stays with
its own item (`69c8aee1`). Built (train #1026, car `e1f4fc2a`,
2026-10-09): `infra/ops/verbs/merge-tenant-main.json` declares
`requires_review`; ops-request carries a `review` step funded for the
`tenant-reviewer` profile (`infra/platform/documents/tenant-reviewer-rules.md`),
`approve` is ready only over its `release`, and `changes` closes the
request on `not-released`. The runner's `verify_review`
(`infra/ops/ops-runner.sh`) renders the approval's plan only for a
LANDED tenant-reviewer run dispatched for that step, which is not the
source run it names, whose source run's `tenant_receipt` sha the plan
lands, and whose `reviewed_plan_sha256` is the hash of the plan
rendered onto the step. The approval's plan is a fresh render that
must hash to the reviewed bytes, and the review run rides beside it as
`review_run` inside the shape the passkey signs, never inside the plan
it vouches for, so the digest is not circular; `review_holds_for`
checks both again before the claim. Pinned by
`boss-jobs/tests/a_tenant_merge_waits_for_an_independent_review.rs`
(readiness) and boss-testing's `ops_runner_approval_sh.rs` (the
runner). Live on 2026-11-13: ops-request v17 is active, and all eight
`merge-tenant-main` requests filed since 2026-10-09 carry
`requires_review` (request `9ea628e8`, 2026-11-06, was released by
review run `d7c8e75e`, signed `agent-codex`, over source run
`b3499c2a`, signed `claude-b`). The cross-packet check lives in the
runner rather than in `ready_when` because design `69c0ace7`'s
predicate-level verifier was never built; that design's fold records
this build as the reason. Two gaps stand, with no new item:
independence is per RUN, not per agent, because every agent signs as
a shared actor id, so two runs of one agent can author and review the
same merge; and nothing verifies which model reviewed, which backlog
`ff17de8f` owns.

**A key a human signs has one declared writer, and the server knows
who that writer is** (design `f623e425`, David 2026-09-25, Q1 answered
as option A; answers backlog `6c9183de`). The paragraph above leaves
the approval tamper-evident AFTER the ceremony and open BEFORE it, in
four measured ways: the merge door lets anyone with Update on the step
write `plan`, `verb`, `host`, `args`, `rendered_plan_sha256` and
`decision`; the `claimable` execute step can be claimed, returned to
ready or completed with invented `output` by any machine-door caller;
the sign-off surface rendered only `comment` and `decision` while the
passkey stamped the whole shape, so a planted plan or a planted
`decision = approved` was signed in one tap; and the runner's identity
is self-asserted, an `x-boss-user` id sent beside the one
`x-boss-machine-token` that every agent's `boss-api`, the conductor,
the dispatcher and the runner all hold, so a rule keyed on actor id is
satisfied by typing the id. Decided: (1) **the writer is declared on
the Workflow row, per field** — `writer = "runner:ops"` on the runner's
keys, `writer = "signer"` (a holder of one of the step's
`sign_offs_required` roles through the gateway session) on `decision`
and `comment`, and `executor = "runner:ops"` on execute for its status
and assignee transitions — so a new approval-carrying protocol gets the
rule by writing a row, not an ops-request branch in `steps.rs`;
(2) **one check at every door that can change the key** — the merge
door, the step PUT, the claim door and the transitions of a step that
declares `executor` — refusing 409 naming the key, its declared writer
and who asked, the shape `human_only` refusals already take; (3) **the
surface shows every key the passkey signs, or does not sign**: every
metadata key in `step_shape_hash`, the title, and the plan verbatim,
an unrendered key shown as raw JSON, with a test that the rendered keys
equal the hashed keys — this ships FIRST, alone, because it is needed
whatever Q1 answers and a human who can see a planted plan will not
approve it; (4) **the runner's refusals stay** as the second line — the
guard prevents, the runner detects, and the guard is only as strong as
the identity it reads; (5) ops-request's approve and execute steps
first, other presence-assured steps adopting it as rows when a need is
measured. Q1 — how the server tells the runner from anything claiming
to be it — is **a per-runner credential minted by the credential
broker** (a `rotate-a-credential` packet; its issuer is the jobs API's
own actor-credential table, so it is derivable and no human places
it): the jobs API resolves the presented credential to an actor and a
host and IGNORES the self-asserted id, `runner:ops` means "presented a
runner credential", and a runner for host h writes only requests whose
`host` is h. Rejected: runner-signed writes (B), because the planted
write still reaches the record and the page — detect, not prevent;
server-rendered plans (C), because the plan is observed on the host,
though the server computing `rendered_plan_sha256` from `plan` rides
with A as a detail; and writer rules enforced against the forgeable
header alone (D), because a guard that LOOKS like protection against
the adversary the review named, and is not, is a mostly-sure guard.
So (1)-(2) ship only together with the credential, never before it,
and until then the display and the runner's refusals are the controls.
Stamp voiding after a reverted edit is `87329a13`'s decision, not this
one; under (1) only a signer may write `decision`, which narrows that
attacker without closing it. The surface display rides its own
trust-boundary car (`fix/the-approval-surface-shows-every-key-the-passkey-signs`).
The writer rule is built at the merge door and the step PUT
(`StepField::writer`, `boss_jobs::field_writer`, 2026-09-26): a
declared writer is satisfied only by a `CredentialedCaller` request
extension, which no client can set, so until the resolve step inserts
one nothing satisfies it — which is why no live protocol declares a
writer yet; ops-request's row declares `runner:ops` in the same car
that delivers the credential. The resolve step is built
(`boss_jobs::runner_credential`, 2026-09-29): a runner presents its
credential as `x-boss-runner-credential` (a header file, never argv —
`ops-runner.sh` reads it from `/etc/boss/ops-runner.credential`), and
the jobs API compares it in constant time against the slots of a
mounted directory, `<host>.current|next|previous`, inserting
`CredentialedCaller { runner:ops, host }` for the one host that holds
it. It refuses nothing, and with no directory mounted every request
passes uncredentialed, as before. The issuer is that directory — the
mount of a Secret only the broker writes — rather than a table: the
broker's authority to mint is then the name-scoped Role admin already
grants, the value lives where every other brokered credential lives,
and the database never holds one. `runner:ops` stays out of
`RESOLVABLE_PRINCIPALS` until a credential is delivered, so the
declaration cannot land first and lock the runner out of the plan the
founder signs. The broker half is built for the forge (backlog
`1e50e66b`, 2026-09-30): `credential.rotate.ops-runner` mints 32 random
bytes into `<host>.next` of Secret `boss/ops-runner-credential`, which
the jobs API mounts whole at `/etc/boss/runner-credential` (optional, no
`subPath`), and verifies it through `GET /api/jobs/runner-credential`;
the forge's converge (`runner-credential-deposit.sh`) proves the staged
value through that door, installs it root:root 0600 and records
delivery; and the promotion writes `current`, blanks `next` and
`previous`, and confirms the old value resolves to nothing. `previous`
is never filled, because delivery is recorded before promotion. Not yet
built: delivery to boss-gcp (the remote deposit, `7336cb5f`), `runner:ops`
in `RESOLVABLE_PRINCIPALS` (which waits for both hosts), the `signer`
and `executor` rules, and the declaration on ops-request. The
write strip below never drops the declared writer's own re-send — its
write is the record (follow-up of car `f3365343`). What the
adversarial review of the writer rule required BEFORE that declaration
landed on 2026-09-28: the approve step declares all five runner keys as
required fields (`args` an array, so a zero-arg request's `[]`
completes); a re-pin refuses to move a live step's declared writer or
write a reserved key, and the publish lint refuses a default projected
into one; a packet whose steps declare a writer has its `host` fixed
at admission at both job doors; and the merge door drops an unchanged
re-send of a reserved key instead of applying it to a row the writer
may have moved since the read.

**Presence authorises a break-glass enrolment, and the bootstrap token
retires** (design `03451237`, David 2026-09-22, all four questions
accepted as proposed). Independence is a property of the ASSERT path —
hardware key plus PIN opens the door whatever else is down — while
enrolment is an administrative act done while things work: the founder's
software passkey may AUTHORISE administering the emergency credential and
may never BE it, and the refusal of synced and unattested authenticators
stays exactly as strict. Today `enroll_gate` admits a break-glass session
or the bootstrap token while nothing is enrolled, and that
zero-credentials condition, which exists only to bound a shared secret,
forces a repair (two keys bound to a retired relying-party id,
`1c4c100a`) to empty the store first and open a window with no emergency
door at all. Decided: (1) **one authorisation packet per key**, its step
naming label, relying-party id and deployment, stamped with
`assurance_required = "presence"`, the gateway re-computing the shape
hash against the step's current content before it enrols — the
mechanism `17835005` settled, pointed at a second subject; (2) **who may
authorise is a named list of employee ids on the gateway**, starting
with David, not a role — a role is registry data, and would make
enrolment depend on whoever can write a policy row; (3) **single-use by
record**: the gateway writes the credential id and instant onto the step
and refuses a step already carrying one, naming it; (4) **the token
retires**, and with it the zero-credentials window. Built for `1c4c100a`
(2026-09-28): the packet is the `break-glass-enrolment` protocol — the
filer's `label`, `rp_id` and `origin` bound into a presence-assured
`authorise` sign-off, the spend written onto its `enrol` step before the
record leaves the gateway — and the list is `BOSS_BREAK_GLASS_AUTHORISERS`
in `boss.yaml`. One tightening the design did not state: the key is
touched from the SIGNER'S OWN session, because a packet id is on the
board and an approved packet was otherwise spendable on anyone's key.
And the durable half of the incident: every record names the `rp_id`
it was enrolled under, the gateway never offers a key bound to another
party (it says which keys and which party instead), and the gateway
test `the_break_glass_records_answer_the_door_they_serve` holds each
committed record to `BOSS_PUBLIC_URL`, so the next cutover is refused at
its gate. The two keys enrolled under `playground.algedonic.dev` are
named there as awaiting re-enrolment, which is David's key touch.

**The break-glass key is the door for a lockout inside BOSS, not for a
dark edge; a second road on the same origin is decided and held**
(design `c5ce1aeb`, backlog `a15a1cd2`, David 2026-09-29, all three
questions accepted as proposed). Measured: the key reaches the gateway
by one road — Cloudflare's edge (the only certificate for
boss.algedonic.dev; the gateway's LAN address is plain http, and the
Caddy TLS front was deleted 2026-09-17), the Access one-time code (one
application over the whole host, `infra/cluster/dns/access.toml`), the
in-cluster tunnel, and the gateway, whose relying party accepts one
origin — and the key doc named none of the first three. An Access
bypass on `/break-glass` alone does not open a usable door (the
ceremony's API and the HttpOnly session it opens sit behind Access on
every other path), and a whole-host bypass exposes every gateway route
while keeping the edge and the tunnel on the road. Decided: (1) **the
scope is written down now**, as a docs car that refuses nothing and so
is not held by DR readiness `62dac114`: the key doc's "What this door
does not cover" names each hop and, for a dark edge, mailbox or
cluster, the road that owes Cloudflare nothing — the LAN ssh door, the
WireGuard hub, kube-apiserver rollback, the forge's emergency merge —
while the printed sheet (`125d405d`) derives its own needs line from
the files and does not restate the doc; (2) **the second road is a LAN
TLS listener on the SAME origin**, https://boss.algedonic.dev, reached
by a hosts line or a local-DNS override, because a new hostname is a
new rp_id and a re-enrolment — a second door, not a second road — and
its certificate comes from a **local CA whose root key is generated on
an air-gapped run of the recovery USB drive and kept encrypted there**
in the recovery kit (`c1bb822e`), never in the cluster or on the
forge; the cluster Secret holds only a one-year leaf for
boss.algedonic.dev, and a rehearsal packet reissues the leaf from the
kit and walks the road each time; (3) **the root is trusted only where
the ceremony is run** — David's workstation and the phone holding his
passkey, not the emergency delegate's phone, whose holder takes the
ordinary edge road — and is **name-constrained to
boss.algedonic.dev**, so a stolen root mints for no other name.
Rejected: ACME DNS-01 (it reverses the 2026-09-17 deletion to buy a
certificate whose renewal needs the provider this road avoids) and a
CLI assertion over plain http (the assertion and its session cross the
LAN in cleartext and can be relayed). Not yet built: the LAN road. It
adds a road into the break-glass door, so it rides as its own
trust-boundary car with an adversarial review, held behind `62dac114`,
and must settle `2710c8fc` (the LAN machine door trusts
`x-boss-user`) before it widens LAN reach to the gateway; placing the
root is David's act. Until it is walked on record, a lockout inside
BOSS during a dark edge or mailbox has no web road, and the repair
comes from the cluster side.

**One bootstrap, at the identity layer** (design `af6dfcdb`, David
2026-09-22: "a 1-time password for the admin's initial passkey
enrollment and then real security takes over from there"; all four
questions accepted as proposed). Retiring the break-glass token left a
fresh instance no way in; the answer moves the bootstrap down, from an
emergency credential to an identity, so the system has exactly one.
Measured: `infra/oss-quickstart/init.sh` sets the bootstrap admin's
password from `BOSS_BOOTSTRAP_ADMIN_PASSWORD`, defaulting to the literal
`change-me`, prints it and asks for a rotation nothing forces or
notices — a standing default credential on a public quickstart.
Decided: (1) **the window closes when the admin holds a passkey**, not
by a spent flag — no state to write, nothing left armed by a crash, and a
reader checks it with a question whose answer is obvious; (2) **the
password is generated and printed once**, the variable kept as an
override for scripted installs and the default removed; (3) **once the
passkey is enrolled, the bootstrap admin's local-auth row is removed** —
`credentials.toml` stays for deployments that never enrol a passkey —
with the counter recorded: an adopter whose only passkey dies is locked
out, which may argue for keeping the row until a second factor exists;
(4) **a fresh instance reaches break-glass enrolment only through the
admin passkey**, so a shared secret never authorises an emergency
credential, at the cost of a window where that passkey is the only door.
Not yet built: `init.sh` still defaults to `change-me`.

**A break-glass enrolment approval is to expire ten minutes after its
signature, and the estate loop is to read the break-glass store while
the gateway runs** (design-doc *A break-glass enrolment approval expires
ten minutes after its signature, and the estate observer reads the
break-glass store while the gateway runs*, `81b514be`, David 2026-09-28,
no question left open; answers backlog `80ab546b`). Measured on the
source car: `judge_authorisation` took no clock, so an approved, unspent
packet stayed spendable for as long as it stayed open, and nothing read
the store between boot and the door. (D1-D4) An approval is spendable
for 600 s from the `stamped_at` of the live presence stamp it matched,
the ops runner's `APPROVAL_TTL_S`, judged at both begin and finish
through a pure `judge_authorisation(.., now)` fed from `wall_now()`. A
stamp more than 60 s in the future, or with no date, refuses. An expired
packet is not voided and no clock rule is added, because it cannot spend
and the remedy is a new packet. The gateway's `ENROLMENT_APPROVAL_TTL`
is held equal to `APPROVAL_TTL_S` by a test (§9a). The same rule bounds
the elevated session below: authority a presence touch confers is
bounded by a named constant, measured from the touch's own recorded
time.
(D5-D7) Noticing while the gateway runs is the estate loop's: a
counts-only public `GET /break-glass/status` (`{door_rp_id, bound,
elsewhere, unknown}`, never `/health`, whose constant answer is a pinned
liveness contract), read by the observer best-effort exactly as it reads
the dispatcher's `readyz`; `estate.compare` raises
`break_glass_unbound:<rp_id>` when `bound == 0`, filed and closed by the
existing `estate.alarm` and `estate.recover`. One bound key but not two
is shown and not alarmed, since the door works. The boot alarm (backlog
`5eb583c2`) files under the same key, so a boot and the observer
describing one dead door make one packet. Built: only that boot half,
`break_glass_alarm.rs`. Not built, measured on origin/main `b0aa242e2`
2026-11-17: `judge_authorisation` takes no `now`, neither
`ENROLMENT_APPROVAL_TTL` nor `/break-glass/status` exists, and
`estate_compare` raises no such finding; item `80ab546b` is open.

**A passkey becomes operator tier through one ceremony vouched by a
break-glass key, and an elevation is to last one hour** (design-doc
*Promoting a passkey to operator tier: one ceremony, vouched by the
hardware key, with a short elevation*, `2cb6256f`, David 2026-09-28, its
three questions accepted as proposed; answers backlog `2a228d0c`).
Enrolment always stored a user-tier key, so nothing could promote one
and the elevation door answered "no operator-tier passkey enrolled",
while boss-people's credential POST took `access_tier: operator` from
any platform-admin body, so a copied cookie could plant an operator key
with no ceremony at all (`1d9970d1`). Decided: the machine files a
`passkey-promotion` packet, one per key, from a button on /me, with no
CLI verb, because filing grants nothing and a human act is a signature
on rendered bytes. David approves its presence-assured `authorise` step
over the key's rendered label, id, enrolment and last use. Then on /me
the key being promoted asserts, proving possession, and (Q1) **a
break-glass hardware key asserts as the vouch**, the one credential a
copied user-tier cookie cannot enrol. The gateway promotes only for the
roster's platform owner holding platform-admin, the owner's own key, a
live stamp re-judged against the step's current content, an unspent
packet and both assertions. **There is one writer of `operator`**:
enrolment no longer takes the field, and `POST
/api/people/{id}/webauthn-credentials/{credential_id}/promote` admits
only the actor `automation:gateway` (an actor id, because a browser
session carries the role), emitting `auth.passkey.promoted` through the
people outbox in the same transaction as the flip. A replay answers
"already operator" with no second event, and demotion is the existing
credential DELETE. (Q2) Any key David holds may be promoted, synced
ones included, with the residual risk stated: a compromised Apple or
Google account plus a copied session cookie reaches operator for at most
the elevation's lifetime. (Q3) **An elevation lasts one hour from its
assertion**, `elevated_at` + 3600 checked where the tier is read; the
session then stays signed in at user tier, and there is no revocation
list, because the short bound is the revocation. Built: the workflow
`infra/platform/workflows/passkey-promotion.toml`, the vouched finish in
`boss-gateway/src/promotion.rs`, and the people door with its event and
pins. Not built, measured on origin/main `b0aa242e2` 2026-11-17: the
hour. `Session::access_tier` answers operator whenever `elevated_at` is
set, and an elevation keeps the cookie's own expiry of up to 24 hours;
item `2a228d0c` is open.

**Presence tickets are signed with their own derived key** (design-doc
*Presence tickets get their own key*, `c22f36c6`, David 2026-09-26, both
questions accepted as proposed; answers backlog `19f00800`, closed
2026-11-16). The gateway signed session cookies, presence tickets and
the OIDC sign-in state with one HMAC key and no label, and the jobs API,
which verifies tickets, read that key, so the process that grants
presence also held the key that mints every gateway session. The jobs
API grants presence anyway, so a forged ticket gains it nothing; the one
power that matters is forging a session. (Q1) **A ticket-only key derived
one way, not Ed25519**: tickets are signed with `HKDF-SHA256(session key,
"boss presence-ticket v1")` and the OIDC state with `boss oidc-state v1`,
the HKDF written on the `hmac` crate boss-core already uses and pinned by
the RFC 5869 vector, the version carried in the label and nothing on the
wire. Session cookies stay on the root key, so nobody is signed out, and
the jobs API keeps only the derived key. Cross-decode tests hold that
neither format reads as the other, including a session payload that also
carries the ticket's fields. A rotation is one act, replace the Secret
and roll the `boss` Deployment, and a ticket that fails after the file
changed without a roll is still refused, with a warning naming the roll.
Asymmetric signing earns its cost only once something other than the
jobs API must check tickets without being trusted to grant presence.
(Q2) **The container is not split now**: every service runs as one uid
in the one `boss` container with the key file mounted, so a taken-over
jobs API can still read it, and both `presence.rs` files say so. The
derived key is what makes a later split effective; it is taken up when
the hosting provision protocol fixes what a hosted instance looks like.
Built: `crates/core/boss-core/src/presence.rs` carries both labels and
the stated dependency.

**Every control has a real person who holds it, and no write can take
the last one away** (design `1c4e42e1`, David 2026-09-28, all three
questions accepted as proposed; answers backlog `47aed706`: "make sure
that all required permissions to operate exist in the context of at
least one real user ... guards to make sure users, especially the
platform-admin can't create a policy or security configuration that
permanently locks any controls"). Measured at f04f7cf7 and live
2026-09-28: one real person; 60 policy pairs a door asks for (58 static
from 135 `Action::` sites, the rest `step-signoff:<role>` and
`authority_role` off 65 active workflows), all held by emp-david at
`all` through platform-admin; ZERO human holders of the operator tier,
because elevation needs an operator-tier key and no person had one; no
key bound to the break-glass door (`a9bfc232`); and nothing refusing on
coverage — platform-admin could set its own `policy-rule:update` to
scope none, because `judge_rule` skips its holds check for a write that
grants nothing, and `DELETE /api/policy/rules/{id}` was not judged at
all. **A control is anything a door asks for before it acts**, of four
kinds: policy pairs (static ones declared in code, dynamic ones read off
every active workflow version); the operator tier (a real person with
platform-admin and an operator-tier passkey, what `elevation.rs`
requires); the platform owner (a real person with platform-admin, what
`platform_owner` resolves); and presence (every step declaring
`assurance_required = "presence"` has a real person in its audience —
in the tree today break-glass-enrolment, ops-request approve,
publish-to-github approve and passkey-promotion). Decided: (1) **a real
person is an active employee with at least one bound passkey** — a fact
the record already holds, so system accounts (emp-audit, 0 keys), the
deploy superuser and `automation:*` actors never count, and removing a
person's last key stops them counting, the same answer the people
last-key guard gives; (2) **a pair is held at the widest scope any
active rule grants for it**, because authority rule 1 lets a person
re-grant only within their own scope, so a narrower holder cannot
restore it; (3) **the guard refuses only a write that orphans a control
covered now** — `orphaned(after) ⊄ orphaned(before)` — so a write that
leaves an existing gap as it was passes, or the operator-tier gap would
block its own repair; (4) **no role is exempt and there is no override
flag** (Q1): platform-admin is bound like everyone, because it is the
actor most able to lock itself out; the way past a refusal is to add a
second holder first, and the 409 names each control it would orphan and
the write that would make it safe — when that write is a human act, the
machine files and renders it and the human's act is a passkey signature
on the rendered bytes, never a typed command (`3df309bf`); break-glass
is exempt only when it restores a rule core ships exactly, the one
write it may make, and every other break-glass write is guarded like
anyone's; (5) **the operator tier counts as a control before any
operator key exists** (Q2): the coverage read reports it as a live gap
with 0 holders and the backstop files its alarm naming `2a228d0c` and
`1d9970d1` as the remedy; the guard is inert on it until the promotion
door lands, and from then the last operator-tier key and the last
platform-admin person cannot be removed; (6) **one real person is the
invariant** (Q3) — on this instance that is David on every control —
with break-glass as the recovery of last resort; break-glass binding is
the recovery path, watched by its own alarm (`break_glass_alarm.rs`),
never a human-held control, and the guard never depends on it;
(7) **one definition, called everywhere**: a pure
`coverage(controls, rules, overrides, roster, keys) -> Vec<Orphan>` in
`boss-policy-client`, each `Orphan` naming the control, its kind and
who held it before, so a refusal and an alarm read the same way;
policy writes are judged inside `port::Judge` (roster and keys read
through the people port before the transaction, rules and overrides
inside it — the read-before, decide-inside split authority already
takes), with the DELETE made a judged write; people writes (role,
status, delete, offboarding, the webauthn DELETE, the register tier,
the promotion door) at the boss-people repository port, asking
`POST /api/policy/coverage` before committing; workflow publish in
boss-jobs, refusing a version whose `sign_offs_required` role or
`authority_role` no real person can satisfy; (8) **the static pair list
is declared once and pinned now, collapsed later** (§9a): `CONTROLS` in
`boss-policy-client` with a test that reruns the triage's scan and
names every door site missing from it, until a later car makes every
door ask through a `Control` const and the compiler is the pin.
`GET /api/policy/coverage` lists every control, its holders and every
orphan, readable by anyone; a dispatcher clock rule reads it and files
one alarm per orphaned control, deduped on the control id, closing when
the next read shows it covered — which is what catches the routes no
write path sees (`tenant publish --force`, migrations, seeds, restores).
Limits, stated: a policy write reads the roster before its transaction
and a people write reads policy before its own, so two writes each safe
alone can together orphan a control — the backstop sees it on its next
tick, and one lock spanning two services is not worth building for a
roster this small; coverage says someone can act, not that two can;
controls data defines outside workflows (view and search resources,
tenant crates) and the readers of `/api/credentials` entries are not
enumerated yet — named as unmeasured, not covered. **The emergency
delegate** (DR readiness `62dac114` item 4: a phone passkey handed to a
named second person, scoped for emergency use only) is a real person by
(1) the moment their key is bound, and counts as a holder of exactly
what their standing grants hold — no more, because coverage reads rules,
not intentions. So an emergency-only delegate who holds nothing at rest
leaves David the sole holder and every refusal above binds exactly as
designed; a delegate given standing holdings is a second holder, shown
as one on the coverage read, and the guard then permits the orderings
that second holder makes safe. Either way the coverage read is where
"scoped for emergency use only" is audited, the delegate's road through
break-glass counts as recovery and never as coverage, and the platform
owner does not move: `platform_owner` resolves the EARLIEST platform-admin
hire, so an admin who joins later changes no filing. Who the delegate
is, what they hold and how it is revoked is that item's design, not
this one. **Sequencing** (`62dac114`, David 2026-09-28): no car that
adds a refusal on a human's path boards until the one-person DR
checklist is proven — two hardware keys touched at `/break-glass`
(closing `a9bfc232`), a second road to the emergency door, the USB
recovery drive, the emergency delegate, the printed re-entry sheet. So
the coverage core (function, `CONTROLS` and its pin, the read) and the
backstop ship now and only report; the policy write guard, the people
guard (after `1d9970d1`, landed 2026-09-28, so the register tier is read
from the key, not the body) and the workflow publish guard wait for
`62dac114`; the collapse to `Control` consts refuses no human and rides
whenever it is ready. The hold lifted on 2026-09-29 and the three guards
now release in the order the next paragraph fixes. Built: the coverage core, its read and the
backstop (car 1, 2026-09-29), and the collapse (car 2): every door asks
through a const in `boss_policy_client::controls`, whose one declaring
line lists it in `CONTROLS`, so `DOORS` is gone; a raw ask survives only
where the resource is data, pinned to a reasoned list. The collapse
found two pairs the hand table missed (Read on `event` and `subject`,
asked by global search), and a pair some door admits only at scope
`all` — the estate, a policy rule, the registry ladder, the commerce and
asset writes — is declared so and held only at `all` (car 1's release
review, LOW); the flag tells coverage and never widens a door. The
three guards have since landed: G3 in train #950 (2026-10-05), G1 in
#965 and G2 in #972 (both 2026-10-06).

**Seeding a fresh instance's policy is an installation act, not a write
the guard lets through blind** (design `b757cf08`, David 2026-10-17, its
one question accepted as proposed; answers backlog `44d78329`). Measured
at f2c551dc and still so at ebed6540e: when the guard's standing is
blind — the roster answers no active employee, or no bound passkey — a
rule with no row under its id passes unjudged, so a new wider rule on a
role no real person holds can raise a pair's widest-scope bar past the
narrower person who held it (review N2) and orphan a control the blind
path never sees. The exception has one legitimate caller,
`bootstrap::publish_policy_rules`, which posts a tenant's rules before
the roster and the founder's passkey exist; core defaults are a separate
path (`bootstrap_reconcile`, through the port at startup) and no
substitute for tenant seeds. Decided: (1) **an ordinary policy-rule write
under blind standing refuses, a new id included**, with the existing
503 naming the source and no mutation; no empty roster, fresh id, tenant
name, request body, force flag, `x-boss-user` claim or machine token
alone establishes a first installation; (2) **the pre-roster tenant
import moves to one bounded, local, service-owned operation** — not a
public HTTP door — that takes the exact seed artifact the already
authorised provisioning path selected for this instance, bound to an
initialisation identity that provisioning created (never inferred from
missing rows or an empty roster), validates the whole artifact first, and
commits the batch with an immutable receipt that consumes the identity in
one transaction: an identical retry returns the original receipt, a
changed digest is refused, and rollback, restart, an emptied table or a
lost key never reopens it; (3) **existing and restored stores default
closed**, an upgrade with no initialisation record included, and replay
never overwrites live policy or operator-tuned rows; (4) **no new
credential, principal, grant, role or remote bootstrap API**: if the
existing provisioning path cannot prove custody of the artifact and the
identity, the build stays blocked and names that missing dependency
rather than inventing authority; (5) complete-standing coverage, the
exact break-glass restoration, the service-read repair and the policy
service's availability under a dark People or Jobs API stay as they are,
and there is no blanket ban on wider rules — a wider rule is valid while
a real holder remains. The guard change and the import ship together (or
behind an inert preparatory car), held for a trust-boundary review,
because a guard alone strands first boot. Out of scope: cross-service
write races (`581d0146`) and the below-door writers — boot reconcile,
migrations, restores (`844aef24`). Not yet built: on main the blind arm
of `Standing::rule_write` (`crates/core/boss-policy/src/guard.rs`) still
answers a rule with no existing row `Ok`, and no import operation exists.

**The DR hold is lifted; refusing cars release one per train, in a
fixed order, each naming a way back that does not pass through what it
refuses** (design `b08725c2`, David 2026-09-29, all four questions
accepted as proposed; answers `62dac114`). Measured at 1aa025ef: both
hardware keys opened `/break-glass` (two `auth.login.succeeded
method=break-glass` events, 2026-09-29 01:36:51Z and 01:37:21Z, closing
`a9bfc232`); the door's scope and its Cloudflare-free roads are written
(`a15a1cd2`); the USB kit holds 11 of 11 items, read back from the stick;
the re-entry sheet is printed and names the "Apple Passkey Delegate"
role (`41c5ddaf`); and David walked the human-only release road once,
signing every command. What that does NOT prove is stated: no delegate
person is enrolled; the log cannot show the two sign-ins were two
different keys; the hub road from a fresh machine (`0697c8fe`) and the
LAN TLS road (`8c127e80`) are unwalked; no restore has run from the kit;
and no host holds the break-glass operator credential, so the
off-cluster arm observes but cannot act. Above all, **getting IN is not
getting PAST a refusal that misfires** — break-glass signs David in and
a misfiring refusal then refuses his signed-in writes — which is why
each refusal carries its own way back. Decided: (1) **`62dac114` is met**
(Q4): items 1, 2, 3 and 5 by artifacts, item 4 as a named role with
nobody enrolled; the item closes as delivered when the last guard (G3)
lands, and its open gaps ride on their own packets without re-imposing
the hold — but if any of them regresses to a road not working, no new
refusal boards until it is repaired; (2) **no refusal waits for an
enrolled delegate** (Q2): the delegate is continuity of a person, takes
the ordinary edge road and never break-glass, and every way back is
David's; instead the people guard must carry a test that enrolling and
later revoking a person who is the sole holder of nothing passes, and
the delegate, when enrolled, is emergency-scoped so they never become
the sole holder of a control, which would make revoking them refusable;
(3) **report-only work is never held** — the machine-token report mode
(`2710c8fc`) and its broker mint, the ops runner credential
(`1e50e66b`), and the break-glass narrowing (`4e1c33b4`) in Warn and
Audit start now; (4) **one refusal per train** (Q1), boarding only on a
train that departs in David's Pacific working hours so a misfire meets
a human, and the next only after the last has run 24 hours with no
unintended refusal in its own record (its misses tally, refusal events,
warn count); (5) **the order**: A, a release refuses the agent-run that
gated its head (`b7b02024` car 3; bypass: another actor releases);
B, the narrowing to Deny (rollback: delete the binding over the LAN
kube road); the break-glass credential delivered to boss-gcp
(`7336cb5f`); G1, the policy-write guard (revert car; it must not sit
on `bootstrap_reconcile`'s boot path); C, the machine token enforced
(`2710c8fc` car 4) after a 72-hour clean report window with overflow 0
(rollback: the mode word back to `report`, by the converge or, if the
converge is itself refused, the ConfigMap over the LAN kube road); D,
the signed `/api/policy/check` (`b8e75382` F7), which **ships behind its
own `off`/`report`/`enforce` mode key** because its misfire denies every
signed-in write and a revert car may not ride, after its warn count
reads zero for the same window; G2, the people guard; E, `runner:ops`
declared the writer of the ops-request approve keys (`6c9183de`) once
both runners resolve live and one approved ops-request has executed on
each host (rollback: a row publish without the writer, never in G3's
window); G3, the workflow-publish guard, last, because it guards the
publish E's rollback needs. C and D come after `7336cb5f` so the
off-cluster rollback exists before the two refusals whose misfire could
block the train; (6) **every refusing car names its rollback and the
road that applies it in its park prose**, and a first misfire is
handled by that named rollback, never by improvising. The must-fix lists
from each car's own review travel with the car and are not re-decided
here.

**The order in (5) was shortened: D and G2 went ahead of C, and C's
window became one daily cycle** (design-doc "Shorten the enforce
queue: D and G2 go next, C keeps its own lane, and its window is one
daily cycle", `09618594`, David 2026-10-06, all three questions
accepted as proposed). D and G2 did not wait for C, which boarded when
its own window was clean; the signer half of E went ahead of its
runner half. What ran, read off each car's record: D in car `d50717c0`
(train #969) and G2 in car `fdd13a69` (train #972), both on
2026-10-06, so G1, D and G2 landed within about nine hours, not 24
apart. David waived the 24-hour spacing of (4) for each pair, and each
car records the waiver: `david_waiver_2026-10-06_24h` for G1 to D,
`wait_waived` for D to G2. Nothing else was waived: each car still had
an adversarial RELEASE at its head and a Pacific-hours train, and D
also had its 72-hour window read at departure and David's rollback
dry run. The design proposed 30 clean hours for C, and David set 26
instead on 2026-10-07 (`row_c_bar_20261007` on `2710c8fc`, kept
2026-10-17). Under that bar every sender that runs daily or more often
must be seen once, and each slower sender is checked by name, because
no window shorter than a week sees a weekly caller. The 26 hours
replaced 72 for row C only. C enforced in car `10f1b77a` (train #1413,
2026-10-26), released by David on a machine-read clean 26-hour window.
Its release door now refuses a machine-gate enforce flip unless a
passing `gate-window-is-clean.sh machine-gate 26` receipt is bound to
the head (`boss_jobs::release_window`). David published the signer
half of E as ops-request v10 on 2026-10-07 (car `a4a6009c`). The
runner half followed later (see the runner-delivery section below).
It too went ahead of C (design-doc "Release order amendment: E moves
ahead of C", `ae1e2aff`, David 2026-10-10), in train #1067 on
2026-10-11, fifteen days before C. That day David also amended E's
precondition for boss-gcp, which had no approval verb with work to
do: the runner's own signed record replaced an executed approved
ops-request there (ops-request `17cd3612`, `reclaim-gcp-root`, closed
`nothing-to-do` and written by `automation:ops-runner`). The forge's
precondition was unchanged.
Items `47aed706`, `b8e75382`, `6c9183de` and `2710c8fc` closed
2026-10-07, 10-09, 10-12 and 10-26. Nothing of `09618594` is unbuilt.

**Kubernetes admission history is not a release input: the B-to-G1
window was read from B's own record** (design-doc "A native
admission-history reader reports its coverage before its verdict",
`25bdb2cc`, its one question approved by David 2026-10-03; it answered
backlog `90218205`, closed stale 2026-10-07). The 24 hours with no
unintended refusal in (4) are read from the refusing car's own record,
as written above, and not from the API server's admission log. David
decided on 2026-10-06 that B's own record stood (its daily break-glass
deposit runs since 2026-10-02, each exit 0, recorded on G1's car
`0df4f091`); G1 then landed, and no remaining step of `b08725c2` reads
admission history. Built, as read-only ops verbs under the existing
operator credentials that answer `unavailable`, never clean, when a fact
is missing: `discover-admission-source` (this design's approved
sanitized audit-source discovery, allowlisted output only),
`probe-admission-source-access` (design `f927de0c`) and
`probe-admission-reader-identity` (`ad504a45`). The access probe's live
answer, twice on 2026-10-05 (ops-requests `979f54b4` and `0bc6a951`):
history unavailable on all three control-plane nodes, reason
`retained_member_size_bound`, a retained audit file over the fixed
1 MiB bound. Design-doc "Stream retained admission files within a fixed
operator work budget" (`ad504a45`, its one question approved in session
on 2026-10-05, the quote kept on the packet) then made the probe's file
read a stream: each retained member is read whole, at most 32 MiB
encoded and 64 MiB decoded, 256 MiB encoded per pass, in exactly two
complete passes that must agree, under the unchanged 256 MiB memory and
850 s run, and anything over a bound answers `unavailable`, never
clipped and never retried (car `46384d3d`, train #957; the `STREAM_*`
constants in `infra/forge/admission-source.py`). Its prerequisite, the
installed reader's identity, is car `47a035e5` (train #966) and has
never been run live. The one access probe since the stream landed,
ops-request `afc02a99` on 2026-10-10, answered `retained_member_size_bound`
on all three nodes again: under the streamed bound, a member over
32 MiB, which the design expected, since its ceilings sit below the
configured 100 MiB rotation. No bound grows without a new decision.
Not built, and owed by nothing: the complete history
reader of the design's phases 2 and 3, which counts a declared interval
and judges coverage before outcome, and its native positive-and-denial
proof. Its contract stays on the packet; a later release that does need
admission evidence starts from the 2026-10-10 probe answer.

**Every service port requires the machine token, and the broker mints
and rotates it** (design-doc *Every service port requires the machine
token, and the broker mints and rotates it*, `6805c764`, on decisions
recorded by David and the operator on 2026-09-25, no question left open;
answers backlog `2710c8fc`, closed 2026-10-26). The LAN machine door
believed `x-boss-user` as sent, so anyone on the LAN or WireGuard could
act as platform-admin at any service port. Decided: one middleware,
`boss_core::machine_gate`, mounted on every binary that serves HTTP
(pinned by `every_service_mounts_the_machine_gate.rs`), in mode `off`,
`report` or `enforce`, read from a mounted ConfigMap key so that a
rollback to `report` needs neither a pod roll nor the jobs API. It
accepts any of three slots, `current`, `next` and `previous`, of one
estate token in a dedicated Secret, `boss-machine-token`, so the broker
that writes it cannot also write the database credential. Every caller
reads it from a mounted directory, never the environment: a mounted
Secret refreshes in place, while an env var forces the ordered restarts
that take a SoR dark. Reads join the gate with writes. OPTIONS and each
binary's declared health paths stay open, as exact paths and GET only,
the off-cluster watchdog's included, and a test pins them. The gateway
stamps the token on every request it forwards, sessionless included,
because its edge strip makes the forwarded identity its own resolution.
A gate in `enforce` with no readable token behaves as `report` and says
so loudly; it never refuses everything. A valid token still lets its
holder assert any `x-boss-user`: per-caller identity is `f623e425`'s
(above), and the role half is `abf9eeae`'s. The value is never placed by
hand. The broker's `credential.rotate.self-issued` handler mints 32
random bytes and rotates by staging `next`, proving every port accepts
it, promoting, and revoking `previous` only after a drain in which no
gate saw it, so no rotation leaves a caller holding a token some gate
refuses; the hand activation runbook is gone. A client that asserts
`x-boss-user` stamps the token through `boss_core::http_client::base`,
pinned by `every_x_boss_user_sender_stamps_the_machine_token.rs`. The
roll-out was callers first, a report window, then enforcement: the
window rule became `21946380`'s and the order `b08725c2`'s row C, which
enforced in car `10f1b77a` (above), and `boss.yaml` reads `mode:
enforce`. Car 4's road for the off-cluster hosts, `boss credential pull
machine-token`, was not taken: `20058482` (below) deposits and pushes
instead.

**A recorded probe reads through a door that holds a read-only
credential** (design-doc *A recorded probe reads through a door that
holds a reader credential, so the machine gate can enforce without
stranding every proof*, `b35c22b4`, David 2026-10-01, its three
questions accepted as proposed; answers backlog `d26515c5`, closed
2026-10-07). `boss-sor-read` sent no machine token, so under row C every
recorded probe would be refused and a true claim recorded NOT PROVEN,
and in `report` every probe read was a miss that kept C's window from
closing. Every option was judged against one constraint: the credential
sits on no path the probe's own text can read, because a probe's stdout
is on the record. (Q1) A dedicated **probe-reader credential**,
`boss-probe-reader`, self-issued and rotated by the existing handler
under its own rule, held root-only on the forge and served to the probe
by a Unix-socket door inside `boss prove` that exists only for that one
probe's run. `boss-sor-read` sends through the door when `BOSS_SOR_DOOR`
is set, and the pod's `boss prove --from-car` runs the same door code.
Rejected: the credential alone, because a file the probe's uid can read
is a file the probe can print; a door holding the estate token, which is
platform-admin power with GET-only resting on the door alone; and
exempting GETs, which reopens `2710c8fc`. (Q2) **The read-only scope
lives in the server**: on a reader-slot match every machine gate admits
GET and HEAD only, in every mode, overwrites `x-boss-user` with
`automation:run-car-probe-reader` at `audit-readonly`, and names the
slot `reader.<slot>` in its tally and the window's facts, so a door bug
yields reads and nothing more. (Q3) Probes stop running as david, as a
car of their own that did not gate C, built as `boss-probe` under
`c98c79aa` (below). Built: the reader slots in `machine_gate.rs`, the
rotation rules, `infra/forge/probe-reader-deposit.sh`, and the door in
`boss-cli`'s `probe_reader.rs`.

**Which in-cluster senders hold the machine token** (design-doc
`c8502e17`, David 2026-10-07; backlog `37742794`; under the standing
rule of `c395e62c` that a workload running branch or third-party code
never holds it). The rule is per CONTAINER, not per pod or manifest: a
container that sends to a port the machine door fronts mounts Secret
`boss-machine-token` read-only and optional and takes the reader from
the boss image (it runs that image, or an init container running it
copies the helpers in), or it is named with the reason it must not —
held by a parsed read of every manifest in
`every_in_cluster_sender_holds_the_machine_token_or_is_named.rs`. Three
applications: (1) the dev pod's `reclaim` sidecar holds it, at the
reader's default directory, and the `dev` container's mounts are pinned
whole — which adds a stamping sender and no new reader, since that pod's
doors already held the token and the sidecar runs the checkout the dev
container writes; (2) the recovery-sheet check holds it; (3) the
playground crawl's browser container never does: its packet writes moved
to a `record` container of the boss image in the same pod, and what the
crawl leaves that container is DATA — read through an open that follows
no link, bounded, cleaned, never executed. And (4) a CI job sends
nothing to the system of record at all: the `reclaim` job that asked the
forge to sweep its disk after each run is deleted rather than left as a
request whose designed answer is a refusal; the hourly host sweep is the
reclaim, and its own packet carries what it did.

**How the machine token reaches the two off-cluster hosts** (design-doc
"How two credentials reach the places that must present them",
`20058482`, questions forge-token and gcp-token, David 2026-10-06;
backlog `88379df3`, under `2710c8fc`). Design `6805c764` car 4's road,
`boss credential pull machine-token` under a name-scoped Role, is not
taken: boss-gcp holds no credential that may read a Secret, and a Role
for it would be a credential fetched to fetch a credential. Each host
instead holds all three slots of Secret `boss/boss-machine-token` in a
ROOT-ONLY `/etc/boss/machine-token` (root:root 0700, slots 0600), and
no new cluster grant was made. (1) **The forge** deposits it on every
converge tick through the admin kubeconfig it already holds
(`infra/forge/machine-token-deposit.sh`), and the units that ran as
david (the host observer, the disk-floor sweep, the deploy runner) run
as root. A group was measured out: a unit sharing david's uid writes its
header file where a process of that uid reads it. A recorded probe runs
as `boss-probe` and can read neither. (2) **boss-gcp** is pushed to from
the cluster by the `boss-machine-token-push` CronJob, every ten minutes,
on a deposit key of its own forced to the receiver's `machine-token`
purpose, so the break-glass kubeconfig keeps its cannot-read-secrets
proof. The key, cadence and enrollment are design `bdc60b65`'s
`gcp-push`, whose own fold is still open. Each host's converge then
verifies by effect, through the reader every caller uses, and records
`machine_token_effect`. Built: cars `ca0f16af` (forge, 2026-10-06) and
`fe480972` (boss-gcp, train #980, 2026-10-07). Live, read 2026-11-13:
the forge's converge of 07:03Z and boss-gcp's of 06:51Z each record
`matched current`, and the five newest of 5,229 push packets closed
completed. Not built: an alarm when boss-gcp's effect reads anything
but `matched current` (backlog `619e1850`, open, owed before row C of
`b08725c2`). The design's third answer, who fetches the broker stage's
signing keys, is folded under the broker stage decision below.

**Who holds the forge registry push credential** (design-doc
`c8502e17`, question image-build, David 2026-10-07; backlog `683d7fe9`,
two cars). The cluster image is built on the forge by the SYSTEM docker
daemon as root and pushed with a token the credential broker mints —
never the operator's rootless daemon and hand-run `docker login`. The
first car makes the credential exist: Subject
`forge-registry-push-token`, a `write:package` token of the forge user,
rotated by the handler `credential.rotate.forgejo-registry` — the
forgejo rotation with its proof changed, because a package token cannot
read a repository: "proven working" is a push the registry accepts (an
upload session opened and cancelled), and the proof belongs to the
handler NAME so no rule can choose one its token passes for the wrong
reason. Delivery is off-host: `infra/forge/registry-token-deposit.sh`
proves the value by a login before it writes it, as a docker config in
`/etc/boss/registry-push` (root:root, 0700 and 0600), and records
delivery so the old instance is revoked only once the host holds the
new one. **The order is the rule**: the units that build
(cluster-deploy-runner, disk-floor-sweep) move to root, the root-owned
tree and that directory only AFTER a rotation has been minted,
delivered and read `accepted` on the forge's converge packet
(`registry_token_effect`) — a unit pointed at a file no rotation has
filled cannot push the image every pod runs. The second car is that
move, held for its review and David's release until the token reads
accepted: both units run as root from `/var/lib/boss/tree/current`,
and the converge builds the generation root's tree holds (refreshed by
the converge itself, run from a hard-linked view so no prune can take it
mid-build), not a checkout; it refuses to build while the deposited
login is absent rather than failing an hour into a cold build; its
kubectl reads the kubeconfig David places (`/etc/boss-ops/kubeconfig`);
its stamps, request inbox and tenant checkouts live under
`/var/lib/boss/converge` (root, 0700), carried once from the old home by
its first root run; it reads a tenant from the forge's own repository on
disk, served as that repository's owner, with no credential; and the
operator's rootless daemon — 43 GB of images and 33 GB of build cache on
2026-10-07 — is used by nothing unattended and is his to free.

**Root on the forge executes only a tree it fetched itself, and a probe
runs only as `boss-probe`** (design-doc "The forge host: probes get
their own account, and root stops executing from a checkout its owner
can write", `c98c79aa`, three questions, David 2026-10-06; backlog
`703358ce` and `a604a35b`). (1) **fallback: stop, never fall back.** A
recorded probe runs as `boss-probe` (own group only, nologin, no sudoers
rule, not in `docker`, so neither daemon's socket opens for it) in the
root-owned read-only view
`/var/lib/boss/probe-view`, never as david
(`infra/forge/probe-account.sh`). Each runner tick writes its marker
only when the account verifies then; without it the unattended door
(`probe_account_refusal` in `prove.rs`) runs nothing, the car records
did-not-run, never NOT PROVEN, and is retried hourly, and the converge
is red with `probe_account: STOPPED: <why>`. The read verb
`probe-account-controls` prints one PASS or FAIL per control on the
host. (2) **root-tree: yes, in three stages, all built.** Root executes
`/var/lib/boss/tree/current` (`infra/forge/root-tree.sh`): root's own
repository, filled from Forgejo's on local disk with the serving side
run as its owner and every object checked, moved only by a whole
generation that fast-forwards `current` (a rewind is refused, exit 3,
until a person runs `refresh --accept-rewind <sha>`). Stage 1 is the
converge, through a launcher outside the tree that starts the newest
generation whose own converge completed (`good`); stage 2 is the ops
runner and every verb, which reach the checkout only as
`BOSS_OPS_CHECKOUT`, where git runs as its owner; stage 3 is the other
units, the cluster deploy runner and disk sweep last (backlog
`683d7fe9`). boss-gcp followed under `db018094` (below), and
`a604a35b` closed delivered on 2026-10-14 against the installed units on
both hosts. (3) **sudo-docker: superseded.** The forge sudo decision on
`a604a35b` (position B, David 2026-10-07) moved the three units that
used david's `sudo docker` to root on root's tree, so nothing unattended
uses it, and kept David's own interactive sudo as it is; a password on
it was offered and not taken. No per-unit sudoers wrapper is owed. Live,
read 2026-11-13: the forge converge of 08:14Z records `probe_account`
verified with no sudo, and root's tree and the probe view both at forge
main `2dbd868b`. Not built: the tree's own text still says otherwise in
four places, among them rows of `no_unattended_unit_runs_from_a_home.rs`
that call boss-gcp's stage owed (backlog `c440ab8d`).

**Row C, second round: who holds the machine token, and what runs branch
code instead** (design-doc "Row C, second round: who holds the machine
token on each host, and what the gate runner and the dev pod do
instead", `bdc60b65`, David 2026-10-06, all five questions as proposed).
Each answer is built and live, read 2026-11-13. (1) **probe-user**:
a recorded car probe on the forge runs as `boss-probe` (no sudo, no
docker, nothing of david's), made and verified by effect on every
converge (`infra/forge/probe-account.sh`), and a host where it does not
verify runs no probe (design `c98c79aa`). Cars `1386dd64` and
`55a99efa`, item `703358ce` closed 2026-10-09. The read verb
`probe-account-controls` reports what `sudo -l` says for the checkout's
owner. The proposed group for the three david units was not taken: they
run as root (see design `20058482`). (2) **gcp-push**: a dedicated
`machine-token-deposit-key`, prepared by the broker through its own
kind `prepare-the-machine-token-deposit-key`, not a second rule on
`prepare-a-deposit-key`, because a step's writer believes one actor.
The broker Role gained that one name. The sibling CronJob
`boss-machine-token-push` runs every ten minutes. Car `fe480972`
(train #980). Its one preparation packet closed with its human
enrollment and verify-by-effect steps completed, and the newest of 5,234
push packets closed at 08:00Z. (3) **observer**: the estate observer
mounts the token, runs the forge mirror's image by digest, and takes
`secret-header.sh` from the boss image through a `reader` init
container. Car `3b8d02f6`, item `066a7613`, held by
`infra/lint/a-manifest-sender-presents-the-machine-token.sh`.
(4) **gate-verdict**: a gate Job with `GATE_VERDICT_CARRIER=pod-log`
writes nothing to the jobs API. It prints its receipt and a trailer
(Job, verdict, bytes, sha256) to its own stdout, and the trailer again
to its termination message. The conductor records the verdict from the
frame that message names (`train/carried_verdict.rs`). It is not the
results volume, because every gate mounts `/gate-runs` read-write. The
edit-level read is handed in as `BOSS_EDIT_LEVEL_ANSWER`. Cars
`c57ad98d` and `033c5c2f`, item `934ccad1` closed 2026-10-09. The
conductor then took the record at gate end through a pod watch (car
`7d64488c`, item `301d6c3b`). Live: the newest gate-runs' receipt
steps are completed by `automation:train-conductor`. (5) **dev-pod**:
on a refused selector, `wt-cargo`, `wt-web`, `boss --built` and
`as-gate-uid.sh` refuse with exit 75 while `boss-api` answers. The
activation is not declared in `boss-dev.yaml` as proposed. Design
`a68fc42a` D6 put the store and its selector on the `/work` PVC, which
a roll keeps. Isolation is on (`b3071bf4` closed 2026-10-22). Builder
pre-flight runs through `dev-build exec-auto`. Nothing of `bdc60b65` is
left unowned. The boss-gcp effect alarm is `619e1850`, activation's gaps
are `a68fc42a`'s own fold, and the READ/USE matrix is `86d93e12`.

**Where root on boss-gcp fetches its own tree from** (design-doc
`db018094`, question q1, David 2026-10-10; backlog `ca356c79`). Root on
boss-gcp executes only from a root-owned tree (design `c98c79aa`), and
boss-gcp's tokenless senders move to root on that tree (design
`fe6d1124`, decision (a)). The forge's root tree needs no credential,
because Forgejo's repository is on its own disk; boss-gcp has none, and
its one checkout fetches as the operator. So root there fetches forge
main directly with a **read-only forge token the credential broker mints
and only root holds**: Subject `boss-gcp-root-tree-token`, a
`read:repository` token of the forge user, rotated by
`credential.rotate.forgejo` and proven by reading the repository —
the same shape as every other broker credential. Every other forge-token
rotation protects its instances in `spare`, including the dev pod's
mount-delivered rotation; a rule with no `spare` fails the declaration
test rather than being skipped. Delivery is
off-host, to `/etc/boss/root-tree/forge.token` (root:root, 0700 and
0600), and the old instance is revoked only once boss-gcp records the
new one. Declined: fetching from the operator's checkout and accepting
only a commit matching forge main read independently — no new
credential, but root would parse objects a non-root account wrote. The
first car declares the credential (its rules, its Secret grant, the
registry row the tree carries for the operator to publish); the
transport to boss-gcp, the tree that reads the file, and the four units'
move are later cars, in that order. The registry row says that its
consumer is pending; publishing the row and starting its first rotation
wait for the transport car, so no credential is minted without a reader.

**The forge host's checkout token is a broker credential delivered to a
file, and the old one is revoked by its last eight** (design-doc
*Rotating the forge-host checkout token: register it, deliver it through
the broker to a file, revoke by its last eight*, `1c90d183`, David
2026-09-26, both questions accepted as proposed; answers backlog
`c4cbc6b5`, closed 2026-09-27). The checkout's token was unregistered,
lived in the userinfo of the `forgejo` remote URL, and leaked into the
forge-converge journal through a git error that printed the URL. It is
now registry row `forge-host-checkout-token`, a `write:repository` token
(widened only by a recorded rule edit, if protection ever measures a
403), Secret `boss/forge-host-checkout-token`, minted by the existing
`credential.rotate.forgejo` through a rule and a `resourceNames`-scoped
Role, with no new handler. (D1, Q1) **The forge deposits to itself**:
`infra/forge/credential-deposit.sh` runs as root at the top of every
forge-converge pass and reads the Secret through the admin kubeconfig
the host already holds, the one read path that owes nothing to the token
it repairs. It writes the file atomically, proves it by `git ls-remote`,
and only then records delivery; the same script's remote target is the
forced-command road to boss-gcp. Because this consumer picks the value
up on its next pass rather than from a mount, the broker revokes the old
instance only after the `delivered` step, for any rule that declares
off-host delivery. (D2) The value lives in
`~<owner>/.config/boss/forge-checkout.token`, read by a host-scoped
credential helper in the owner's global git config, and the remote URL
carries no userinfo, stripped only after the helper was proven:
forge-converge, publish-to-GitHub's forge push and the tenant-source check
all read through it. (D3) The old token is named by its last eight
characters, computed on the host and written onto the item, and the broker
resolves the name itself, refusing zero matches, several, or its own
replacement. (Q2) A leaked token that matches no declared holder is
revoked even while other forge tokens have no known consumer, because an
unknown consumer that fails loudly gets its own registered credential; a
token declared to another consumer is not revoked here and gets its own
rotation first. Built: the deposit, the rules
`broker-rotates-the-forge-host-checkout-token` and
`broker-revokes-the-forge-host-checkout-token-on-delivery`, and
`old_token_last_eight` in the rotation handlers. Design `0af333ee`
(below) moves this token off user david.

**The GitHub verbs act with a token minted per act, never a standing
admin token** (design-doc *The credential the GitHub verbs act with on
algedonic-dev*, `76c46869`, David 2026-09-28; answers backlog
`1fce4d88`, closed 2026-09-28; follows design `76155676`, the App as the
root). The only App-minted token was the DR push token, narrowed to
`boss-dr`, which can neither create a repository nor write a ruleset.
(Q1) **Per act, with no clock refresh**, so between acts no unexpired
admin-scoped token exists anywhere. A standing org-admin token refreshed
every 15 minutes was simpler by about one rule file and was refused for
leaving that power on the forge host all day. (Q2) Two dispatcher rules
call the existing handler with `phase = refresh`: one when a GitHub
ops-request is filed, since its plan reads the token too, and one when
its approve step completes, minting again only if fewer than 40 minutes
remain. There is no new handler code and no rotate-a-credential packet.
`infra/forge/github-act.sh` renders its own slot through
`credential-render.sh` before reading it, so nothing waits for a
converge, and presents the token to `DELETE /installation/token` once
the write has read GitHub back. The exposure runs from filing to the end
of the act, capped by GitHub at 60 minutes a mint; the dispatcher keeps
the one App root, and the forge never holds the private key. The
installation-token pin admits such a rule only as a declared per-act
pair (`PER_ACT` in `github_app_installation_token_rules.rs`). (Q4) The
slot is named for the owner, not the use: Secret
`boss/github-app-algedonic-dev`, registry row `github-app-algedonic-dev`,
rendered to `/etc/boss-publish/github-app/algedonic-dev.token`, so a
hosting customer's organisation is another slot of the same shape. The
DR token stays separate and narrow, because its push runs on a clock.
(Q3, answered other than proposed) `dauld/boss-mirror` gets no
installation: dauld stops being a managed repository once the App works,
which `no-github-path-outside-the-org.sh` now holds. Built: the per-act
rules (`broker-mints-the-algedonic-dev-admin-token-when-a-github-request-is-filed`
and its siblings), the render and the verb's revoke.

**Forge automation tokens act as two non-admin accounts that reach only
`david/boss`** (design-doc *Forge tokens act as david, a Forgejo site
admin: move each consumer to a forge account that can reach only
david/boss*, `0af333ee`, David 2026-11-13, its three questions accepted
as proposed; answers backlog `70555ffd`). Every forge token the estate
minted was a token of user david, and david is a Forgejo site admin: the
broker's issuer root mints through `/api/v1/admin/users/{user}/tokens`,
which Forgejo serves only to one. Forgejo does not narrow a site admin's
token to its collaborations, so david's `read:repository` read every
repository on the instance and his `write:repository` wrote every one.
Scope cannot narrow that further, only the principal can, and a hosting
business will keep other people's repositories on this forge.
(accounts) **One account per access level**: `boss-reader`, a read
collaborator on `david/boss` only, holds the root-tree token;
`boss-writer`, a write collaborator there and never a site admin, holds
the dev pod and conductor token and the forge-host checkout token. The
registry push token (`write:package`) stays on david until the registry
has its own design. (create) The forge converge creates each account as
root, the way it makes `boss-probe`: created when absent, no password
login, not admin, its one collaborator grant, and a verify line on the
converge packet stating `is_admin` false and the one repository, red
otherwise. Tokens for an account are minted only by
`credential.rotate.forgejo` with `forge_user` naming it, so there is no
new handler and nothing placed by hand. The read token moves first,
having no delivered consumer to break, then the writers one at a time
through their ordinary rotations, each proving the new principal before
the old token is revoked. (attribution) Forge-side authorship of train
pushes and merges becomes `boss-writer`; commit authors are unchanged,
and BOSS keeps its own actor record. The write token named `boss-gcp`,
whose conductor consumer left boss-gcp on 2026-09-04, is a separate
item. Built, measured on origin/main `b0aa242e2` 2026-11-17:
`infra/forge/forge-account.sh`, run by forge-converge, with
`infra/forge/forge-accounts.json` declaring `boss-reader`, and the
root-tree rules minting for `boss-reader`. Not built: `boss-writer` is
not declared in `forge-accounts.json`, no writer rule names it, and item
`70555ffd` is open.

**Who may act on a step is a predicate over authoritative actor facts, and
eligibility never grants authority or implies presence.** Design
`347d14ee` (answers b4330f74; David decided that human is an actor
attribute alongside certifications and experience) records the shape. A
step declares `actor_requirements = { schema, predicate }` (for example
`actor.human == true`) in its metadata defaults, evaluated by boss-expr's
bounded evaluator over a small typed fact set validated at publication;
one module answers every caller (claim, assignment, completion, dispatch,
protected metadata writes and the needs-you view) with eligible,
ineligible or unknown and a named reason, and unknown never grants. Human
is true only for a verified active employee identity that resolves to no
agent or automation; an agent cannot become human through a writable
attribute, and qualifications are typed authoritative assertions with
provenance and validity, not caller strings. The admission to act is the
conjunction of the existing authentication, policy, assignment, station
capability and readiness checks plus the requirement: it can narrow a
grant, never create one. Presence stays the existing `assurance_required =
"presence"`, raised only where a passkey-bound approval is the protocol,
never merely because a human is required. Needs-you is a canonical server
projection over the same reader. Migration is additive: one compatibility
reader normalizes legacy `human_only`, new versions publish through the
workflow door, nothing is repinned without the conversion door, and
`human_only` is removed only when no source, live version, pinned packet or
consumer still needs it.

**A declared credential remedy is routed to a named human; routing never
delegates the authority.** Design `6e884db1` (answers backlog `08caa5e3`;
Q1 accepted as proposed, David 2026-10-17). The case: when boss-gcp does
not hold the break-glass deposit key, the daily deposit prints
`RED break-glass-deposit/authorize-key` with the line to place, and the
dispatcher hands it to `maintenance.chore.file_reds`, which files an
ordinary backlog-item whose triage is role-routed and agent-capable.
Owning a job does not assign a human step, so the one act the platform
owner owes never reached the owner's own queue. Decided: for that declared route
only, the authenticated dispatcher may create ONE linked human-only
remediation packet beside the ordinary diagnosis, carrying only a
validated, structured public proposal and assigned to the current
eligible platform owner. That is permission to route a proposal and
nothing more: it grants no right to enroll, approve or execute, to change
the receiver or its purpose, to read any further credential, or to clear
an alarm on a human's report. Genuine passkey identity, existing policy,
exact-content signing and independent machine verification all stay
mandatory; an assignee, a human-shaped header or a platform-owner lookup
never proves the caller is that human. The contract that follows:
(1) the mapping is registry data (source chore kind, finding route,
target workflow version, proposal schema, human step, holder selector,
verification relation) executed by one generic adapter, starting with
this single mapping, with no core switch on a route, a credential or a
person; (2) nothing is parsed out of RED prose: the producer exposes a
versioned typed proposal binding the exact public key, forced-command
line, receiver account, pinned host-key fingerprint and source, validated
whole against the existing receiver contract (its command, `restrict`, no
second key, nothing trailing), and a producer that cannot supply one
within its existing authority leaves the diagnosis at proposal
unavailable, never an agent's reconstruction or a wider reader; no
private key, token or Secret payload rides on the packet; (3) the holder
resolves through the `PlatformOwner` port and is checked as an active,
eligible employee, and a missing or unverifiable holder is a visible
unresolved-routing diagnosis with no actionable packet, never a fallback
to the caller, an automation id or a hardcoded `emp-david`; (4) the
assignment goes through the supported admission door recording the real
machine actor, never a machine writing the person's answer, and if the
nomination-only operation of `e8f9e47f` is needed it is a dependency,
not a bypass; (5) the human signs the exact bytes displayed, under the
existing presence rules, and a changed proposal is a new unsigned
proposal and a new decision with the old one retained; (6) dedup is on
source, route, receiver, purpose and exact proposal identity through a
persisted idempotent create-and-link, so a nightly repeat names one open
packet and a changed key stays separately visible; (7) the diagnosis
stays open until a later real maintenance run's receiver receipt, bound
to the proposed key and the human act, shows the effect and its negative
checks: a refusal, a supersession, a chat confirmation or an unrelated
successful deposit clears nothing, and an unreachable human is waiting,
not an idle agent task. Delivery is one credentials-marked car held for
adversarial review under an exact-head full gate; publication and native
acceptance follow their own protocols. No question was left open.
**Not built** when this was folded (2026-11-13): the rule
`file-backlog-items-on-break-glass-deposit-red` still hands the route to
`file_reds`, `break-glass-deposit.sh` still prints the line only as
prose, no remediation workflow exists under `infra/platform/workflows/`,
and `e8f9e47f` is open. On 2026-11-01 the consolidation pass closed
`08caa5e3` as a duplicate of the alarm design `97cb3a87`, whose car 2
carries deposit reds escalating to the platform owner; that kind is not
built either.

**A calendar feed token rests as its hash: the server never holds the
token** (design-doc *Calendar feed tokens: keep a hash, never the
token*, `3101c506`, David 2026-09-26, Q1 accepted as proposed; answers
backlog `4aaff4dc`, closed 2026-09-26). The token in a feed URL is the
whole of its authentication, and a rotate wrote it raw to
`tech_calendar_tokens`, to the payload of
`scheduling.calendar-token.rotated` in `audit_log`, and to the bus, so
the event tail handed every employee's feed to any global reader, the
guest login's `audit-readonly` included. Hashing at rest was chosen over
redacting in the readers, because redaction cannot reach the bus, the
backups or an export already taken, and would be a list of one kind kept
beside the log (§9a's failure shape). The table keeps `token_sha256`, an
unsalted SHA-256 of 32 random bytes, with the migration's SQL expression
pinned equal to the Rust hash; a new rotate's payload is `{employee_id,
token_sha256, rotated_at}`; rebuild hashes a legacy raw payload, so
replay gives the migrated table; the feed hashes the token it is given.
The tokens the log already held were revoked as events, one rotate per
row carrying the hash of a value nobody keeps, because a migration that
rewrote rows would be undone by the next replay. (Q1) The URL is shown
once, in the response to the employee's own rotate, as a forge personal
access token is, and a feed subscribed before the revoke is subscribed
again. An admin's rotate of another employee is a revocation, and it
answers 404 where no token row exists rather than minting an orphan.
**Any later secret-bearing event follows the same rule**: its payload
carries a hash, or a reference to where the secret lives, never the
secret. Built: `boss-jobs/src/scheduling/` (`feed_token.rs`), migration
`20260926003019-a-calendar-token-rests-as-its-hash.sql`, and the
`logged-raw` count and revoke routes in `scheduling/http.rs`.

**The playground's logged calendar tokens are to be revoked by a bounded
ops verb pair; decided, not built** (design-doc *A bounded ops verb
revokes the logged calendar tokens on one declared instance, first the
playground*, `d33dfbe5`, David 2026-09-26, both questions accepted as
proposed; answers backlog `77dc1579`). The revoke above was owed once on
every instance. The system of record read 0; the playground, where
anonymous guests read the audit log, was never measured, because the dev
pod's door reaches only the system of record. Decided: not an
`--instance` flag on `boss-api`, which would give the pod a general write
path to a public instance, but two ops-runner rows. `plan-a-logged-raw-revoke
<namespace>` is read-only: it counts and renders the plan.
`revoke-logged-calendar-tokens <namespace> <plan_sha256>` requires David's
passkey approval, refuses unless the plan still hashes to the signed
value, POSTs the revoke once and reads the count back as 0. The namespace
is the only parameter, accepted only when `infra/cluster/instances.toml`
declares it, with boss-dev refused by name. The script reaches the API by
`kubectl exec` into the instance's own `boss` container, on localhost.
(Q1) It signs with the playground's own machine token, used only inside
its own pod and never read out, copied or delivered. (Q2) It asserts
platform-admin as `automation:ops-runner` on that one call, so David's id
stays out of an audit log anonymous guests read (`abf0ce29`), while his
passkey approval stays on the ops-request. One act per verb pair until a
third such act shows whether a shared shape has earned its place. Not
built, measured on origin/main `b0aa242e2` 2026-11-17: no verb under
`infra/ops/verbs/` names the revoke or its plan, and item `77dc1579` is
open.

## Calendar

Release milestones are plans, separate from the act of cutting a release.
The settled practical test uses one `release-milestone` packet per date
on the schedule step assignee's calendar, initially emp-david. Moving a
date withdraws the old packet and files a new one, preserving both dates;
a reschedule mechanism is deferred until moves justify it. The agent may
schedule the accepted target under the recorded company-frame decision;
the fold supplies no new date. The historical 2026-09-30 target was not a
promise and is not renewed by this record. Decision history and the
original target remain on
[e1dba350](https://boss.algedonic.dev/ux/jobs/e1dba350-7556-49f4-a6f2-d5a73c59fea0).

Scheduling uses `duration_minutes`, the StepType and surface spelling;
the hook's old `duration_hours` spelling is retired. A workflow step's
capacity estimate is a different fact. The implementation obligation on
backlog `fe199985` requires an actual active scheduling step, its matching
calendar reservation and rendered calendar evidence. Recording the design
does not prove that obligation or create a milestone, reservation or tag.

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
is data (`workflows.toml` step graphs; `tenant.toml` rates, ramps,
anomalies, shocks, counterparties, periodic and batch cycles). The
workforce executor claims and completes **assigned** steps through
the public API as the role-matched employees, filling
required-at-done fields (bundle + step-authored) and collecting
sign-off stamps before completing — metadata first, stamps
attesting the final shape, then the status flip. Gates are
agent-executed by the dispatcher reading real stock — the
workforce never sees them. Brewery production is **demand-pull, not
open-loop**: each `morning-brew*` kind is a `deterministic` daily
review (one brew *reviewed* per working day — the rate is the
brewhouse's per-beer slot capacity, never a Poisson draw that could
silently emit nothing), and the gate is in-flight-aware, crediting the
pipeline (`effective_on_hand = real + open_sibling_jobs × batch_yield`)
before deciding brew|oversupply so the daily review doesn't double-brew
through the multi-day brew lag. Batch engines (payroll, taxes) are
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

**The simulated company may beat industry norms, but not with
impossible actors** (the one constraint folded from
`brewery-fidelity.md`, decided by David in packet `4a39c1df`,
2026-08-22; the rest of that program is parked with the brewery sim).
David: "we may end up with a brewery that looks significantly more
efficient than normal, and that is okay as long as the actors in our
simulator are modeled reasonably, like a person can't work more than 8
hours." Protocols are cheaper to change here than in any real company,
so a better-coordinated brewery is a legitimate result; **actor realism
is the honesty budget**. That splits a step's duration into two facts:
**labor-bound** work, bounded by people at no more than 8 labor-hours
per person per day, and **wall-clock-bound** work — fermentation,
conditioning, a settlement window — which no staffing compresses, so a
StepSpec is to carry both as `labor_hours` and `wall_clock_hours` (Q2).
The limit is **a configuration expectation, not an enforced invariant**
(Q3: "It does not have to be an invariant, but I wouldn't expect a
realistic configuration to include people working abnormal hours"): a
tenant's configuration is reviewed against it, and the conservation
sweep does not check it. Not built: steps still run their kind's
`typical_duration_hours`. Of the doc's other answers only Q4 landed —
graduated excise rates as registry data (`excise_rate_schedules`),
which the code cites as `brewery-fidelity Q4`.

## ML platform

`boss-ml` + `boss-ml-api` (gateway-proxied under `/api/ml/*`);
inference plugins live in `boss-ml-plugins` and register via
constructor wiring — no dynamic loading. Models bootstrap from
embedded TOML seeds; predictions store as JSONB; scheduling is
systemd cron. Next-action rules and risk scoring are declarative
rule models with plain string-template substitution — no embedded
scripting.

## Content, files, knowledge

### The public engineering explanation lives on the tenant website

The company website keeps its small-business lead. A separate engineering
page, visibly linked from the homepage, explains refusals, recorded
evidence and measured reliability. It belongs to the tenant's `site/`
content and publishing path, not a BOSS application route. Decision:
[59c0133a](https://boss.algedonic.dev/ux/jobs/59c0133a-8a8f-4a57-89b7-9b30e3e7adf4),
placement approved by emp-david on 2026-10-02.

The implementation is `site/engineering.html` and its homepage link in
`david/algedonic-llc`, signed merge `f51ed7f7`, tenant commit
`7434c8b5cd6def3c8f156c273530f78cae97e29e`. On 2026-10-15 the deployed
site reader returned both files with HTTP 200 and bodies byte-identical
to that commit; the complete bodies and hash receipts are attached to
implementation job
[f24df452](https://boss.algedonic.dev/ux/jobs/f24df452-ae24-46bb-b09b-c8bad52a17a8).
That is content-delivery evidence, not a change to public Access policy
or proof of an external authenticated browser journey.

The explanation leads with a real recorded refusal. Historical claims
need their original records, current measurements need dates and native
evidence, and detecting a fault is distinct from fixing it. A protocol's
existence proves neither completed backups nor guaranteed uptime.

**The blog is flat files the converge observes page by page, under the
founder's name.** Design `f89a241f`, both questions accepted on 2026-11-04.
A post is one flat file, `site/blog-<slug>.html`, with an optional
`site/blog-<slug>-evidence.json`; not a subdirectory (the converge would
not stage it) and not content rows rendered by the gateway, which buys
nothing until posts need search, drafts or images. `site/blog.html` lists
the posts newest first, a contended shared file accepted while posts are
serialized through the founder's approve step. The converge observes
**every** page it stages (`steps.run.site.pages: {path: sha256}`, read the
way a visitor's request reaches the gateway), and a
`publish-a-blog-post` packet's live step closes on its page's hash through
the same dispatcher rule the landing page uses, never by hand. A post's
numbers are copied, dated and sanitized into its evidence file, and a
second actor's scrub step refuses internal addresses, hostnames,
credential names and any number its source does not hold. The blog is
linked from the masthead as **Engineering notes**, `engineering.html` keeps
its URL as the first entry, and www still leads with small-business
benefits. Every post carries **David's byline as founder** (the approve
step is his signature on the words) and states that agents drafted it
under the `publish-a-blog-post` protocol.

### Content records and attachments

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

Legacy manual state may enter the log through one explicitly named
**present observed baseline**, not fabricated historical creation
events (design 2ec0c3e4, decisions by emp-david, 2026-10-03).
The observation preserves complete raw sections and history, including
serial identities, stored timestamps, editors, reasons, audiences,
publication state, hierarchy and sequence state. These historical
fields remain stored claims; the observation's own actor and time
describe the present act. A privileged complete inventory read and
observer write require explicit scope-all policy authority. Admission
compares a canonical inventory hash under the existing table/outbox
completeness locks and atomically records an idempotent observation.
Protected replay begins with that baseline, accounts for covered
genuine facts, and applies later genuine facts, conserving every
selected event row exactly once. Incomplete or changed evidence
refuses before a wipe. Source preparation and release neither execute
nor prove a live legacy migration; capture and its separate effect
evidence remain an operator-authorized bounded migration.

Manual authority is **named human authority**, not a role-wide default
(design bb3cc8d8, decisions by emp-david, 2026-10-11). The approved
continuing grants are exactly two user overrides for emp-david:
`read/manual-inventory/all` and `create/manual-baseline/all`, each with
`expires_at = null`. Only David's genuine authenticated platform-admin
session may activate them through the guarded policy door. Agents may
prepare exact requests; they receive no grant or authority to apply
them. Approval is not activation: a successful write must be followed
by exact override, coverage and audit readback. Partial success is
partial authority, and drift in an existing override requires a
separate explicit decision rather than silently replacing its scope,
denying intent or expiry. Neither this decision nor its documentation
clears an orphan-control alarm; fresh live coverage must name the real
holder.

Retirement uses the actual override id and remains subject to the
native last-real-holder guard. It may refuse removal of the sole
holder. A separately approved eligible real successor must actually
hold the exact scope-all control, confirmed by coverage, before that
authority can transfer. Without such a successor, retain the grant
and bring the refusal to the human decision. Direct database writes,
fabricated grants, expiry tricks, role or passkey changes are not a
revocation route.

The same decision permits the named human to obtain the complete
inventory only after both approved grants and current human identity
standing are evidenced, then prepare one bounded operation packet.
That packet binds the raw inventory, canonical hash, immutable
admission identity, observer and deployed artifact. David must
separately sign off that exact payload before any baseline POST; this
is no authorization for an unknown future hash, an agent observation
or a rebuild. The producer derives the present observer from the
authenticated identity; a caller nonce is not an event UUID. Preserve
the native response, producer fact and admission identities and
before/after evidence; an uncertain outcome is recovered with the
same witness, never by manufacturing another observation. Changed or
incomplete evidence refuses under the existing locks. Actual inventory
conservation, canonical hashes and native event/outbox/audit coverage
must be read back through supported readers. A missing reader holds
the operation at that boundary. Source presence and grant coverage
alone do not prove legacy event coverage, and no rebuild or wipe is
authorized by this decision.

## Search

One core endpoint (`boss-search`) queries `subjects`, `jobs` and
`audit_log` in a single round trip; each app contributes its own
scoped search for domain detail — the global box answers "what and
where", the app answers "which one". **Search reads its own
projections, rebuilt from the log** — not the live domain tables and
not the log directly — so a Subject absent from a domain projection is
still findable and the index reproduces rather than drifts. Results
group by kind in a hard order (Subjects → Jobs → Events, recency
within a kind); there is deliberately no cross-kind relevance score,
because unexplainable ranking is how search boxes lose trust. Policy
scoping is server-side in the same `PolicyClient` path as every other
read — a result the caller could not open must not appear, and
client-side filtering of a wider set is prohibited. The chrome
dropdown is the current app's scoped preview; the full cross-app
results page lives in Home. v1 shipped the unified claim whole (one
query returning a Subject with its Jobs with their events) rather
than name-lookup-first — the join on system-issued identity is the
point, not a later feature.

## Step UX & frontend

### Department tabs fold to the available width

Above the phone width, department tabs sit on the shared chrome bar
when they fit; only the remainder folds into More. Measure the available
width after the wordmark and right-hand controls, and each tab's rendered
width. Reserve room for More whenever anything folds, and omit More
when every department fits. A fixed department-count breakpoint would
encode one tenant's registry in CSS.

Home and the viewer's own department never fold and keep their places.
Other departments follow in registry order and fold from the end, so
the visible set is a prefix as width changes. Navigating at a fixed
width does not reshuffle that set. A folded active department still
names the More control, preserving orientation without pinning the
department being visited.

Simulator belongs inside IT, not among department tabs.
This is David's Q2 resolution, which replaces the proposal to pin
Simulator beside Home. The phone keeps its pinned tabs, More and
scrolling band; tab type and padding do not shrink to fit more tabs.
The sidebar, IT family strip and department-access policy are separate.

This is the settled contract from
[248e66c4](https://boss.algedonic.dev/ux/jobs/248e66c4-50b3-4e81-bee7-aa9d9b49f8a6),
answering feedback
[9fa458cf](https://boss.algedonic.dev/ux/jobs/9fa458cf-253a-4594-9c6a-751802fd5abd).
The fold records the decision; implementation and browser verification
remain owed. Verification uses the tenant's declared departments at
wide, intermediate and phone widths: all fit without More when room
permits, inline and folded tabs conserve the full list without
duplicates, narrower inline sets are prefixes, and navigation preserves
the set at a fixed width. The Simulator check must follow the resolution,
not the superseded proposal.

### Unmatched routes preserve the failed address

An unmatched path renders a `notFound` route at the requested URL,
including an unmatched path inside IT. It does not redirect to a landing
page or draw a healthy yard. The surface names the address as escaped
text and offers a route back to the relevant department or My Day.
Single-identity route tails exclude raw slashes; a legitimate slash in
an identifier must be encoded. Hierarchical manual section paths retain
their separate slug semantics. A real landing surface has an explicit
route rather than depending on the catch-all to remain reachable.

This is the settled routing contract from
[ee3a3a2f](https://boss.algedonic.dev/ux/jobs/ee3a3a2f-865d-4f7e-a9e0-4c5aa2b3e063).
The original `/ux` spellings are historical: subsequent routing work may
move a valid destination without restoring the misleading fallback.
The current router and `App.svelte` carry the not-found branch. Source
presence is not a fresh browser rendering receipt; implementation job
`c4f2ae24` retains its own acceptance record. This fold adds no telemetry
or inferred destination suggestions.

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

**The activity surface draws the network, not the route map.** A
workflow DAG is a route map, and stacking N kinds as N sections
could never show that two packet classes share the same operator —
so the **canvas is `/system/flow`'s hero**, per-kind decorated DAGs
demote to the route ghost and the Fleet inspector, and `/system/os-map`
retires as a page while its LAG-pairing SQL survives as the traffic
layer, re-keyed from department to station. **Rails are merged** into
one faint overlay of all declared routes with per-kind tint on
hover: stations serve many kinds by construction, and per-kind rails
would re-partition exactly what stations-as-queues unified.
**Packets move on the marker topics** — `step.ready`,
`step.assigned`, `step.done`, plus `jobs.job.closed` as the
departure — while `jobs.step.updated` metadata chatter stays
ticker-only; transport is hybrid per the SSE policy, dots riding the
existing push stream and depth piles and edge thickness riding the
polled aggregate. **Personal-queue stations materialize when
non-empty or recently active** (the os-map's nodes-from-edges rule
restated for queues, honest at the measured occupancy), the claim
hop renders in the one transfer grammar with the push-vs-self-claim
split as a tint rather than a second gesture, and the claim
compare-and-set was a hard prerequisite — without it the canvas
could animate two actors winning the same packet. **No company-level
canvas in v1**, but every layer endpoint takes a scope parameter
from its first version so the recursion is a query change rather
than a rewrite, pinned by a test that a department-filtered canvas
is self-consistent. Simulated traffic is always counted separately.

**The personal unit is a View, not a gadget.** A View is a saved
composition — a query plus a layout — holding no authoritative state
of its own; it is a pure function of the log, so it rebuilds, cannot
drift, and two people running the same View see the same numbers.
This is the deliberate inversion of the private-durable-state
micro-app (Cloudflare OS's gadget), which is the federation problem
returning one user at a time. Local state is allowed **while it stays
local**: inside a Step until the Step completes, scratch on a View
until it flows into a Job, Step or Event — the test is whether
anything outside depends on it, not whether it exists. Views are
declarative compositions (reviewable, diffable, deterministic);
agent-authored full-code apps are a later phase gated on
safe-user-code infrastructure. The promotion ladder: personal View →
shared (frictionless, the individual curates their own shareables) →
inclusion in a department's views (a submitted Job) — ceremony lands
only where something becomes the company's. **IT is the department
app and System Model lives inside it**: modeling the company is work
the IT department does, so dispatcher rules, step plugins and
experiments need no IT-vs-model line. Department Apps are the decided
workflows (registry-governed, the same for everyone in the role);
Home is where an individual explores what has not been decided yet.

**A page is a lens: a queue set, a presentation and a flow window,
declared as data** (folded from `views-as-queue-lenses.md`, all five
questions decided by David in review `6f33df26`, 2026-08-19). David
(`3f5f7f63`): "the Design Review page should really just be a custom
view onto a particular queue or set of queues. That is what many of our
pages fundamentally devolve into." Every bespoke page — My Day, the
departure board, TriageBoard, Fleet, Design Review, JobsList — is a
frozen instance of that triple, so rebuilding them is §9's
registries-over-code applied to the read side, the move StepPlugins
made for step UX. Every lens shows the packet with its protocol set as
`(kind, version)` chips, WIP where it sits (at a station, never as a
bare status column), and a flow strip. Decided: (1) a lens row carries
queue predicates in the same `boss-expr` surface admission evaluates,
a presentation naming a registered renderer, a flow window, a scope and
an owner role, append-only and versioned; (2) **presentations are
registered renderers in core** — board, cards, kanban, list — with the
StepPlugin bundle as the escape hatch, so a new idiom never needs a
core change; (3) **the flow strip** is depth now, arrivals and drains
over the window, oldest waiting and a small trend, from marker events
joined on `step_id`, served by one shared endpoint — the algedonic
surface, since a queue nobody drains shows itself on every page that
renders it; (4) **a converted page's bespoke fetch, filter and render
code is deleted in the same diff**; (5) the cars run registry,
renderer and strip first (proven by the departure board), then
TriageBoard, Design Review and Fleet, JobsList as the lens builder, and
My Day last. What landed took a narrower shape: **the lens rides the
station row** (`138-station-lens.sql`) — a station declares its page
header and panel keys, `GET /api/stations/{name}/queue` echoes them,
and `/it/design` renders the `design-review` station's queue from that
one call. Not built: a lens registry of its own, the renderer
vocabulary and the shared flow-strip endpoint.

**Aborting a job asks for the reason** (design `c6f9fb3e`, David
2026-09-15, all three questions accepted as proposed; from his feedback
`33324fe9`: "I need to be able to Abort a job in the UI, but make me
provide a reason"). Every workflow already declares its abort as a
terminal step with `outcome_kind: aborted` and a `reason` field, so an
abort is a step completion, evented, on the record; what did not exist
was one control that finds it. Decided: (1) the job page shows
**Abort…** whenever the job is open and its workflow declares a step
with `outcome_kind = aborted` — two such steps and the control lists
them and asks which; none and there is no control, because the
protocol says the job cannot be aborted and the *row* is what changes,
never the page; (2) the reason is **required, free text, at least a
sentence** — not a code from a list, because the list is what we would
maintain and the sentence is what the next reader needs — recorded on
the step as `reason` with the session's actor as `completed_by`, and
nothing else is asked; (3) **whoever the step's `authority_role`
admits may abort**, the ordinary claim rule and no separate permission;
a viewer without it sees the control disabled with the role named.
Built as `7a98040e`.

**Every page is audited as a packet, and the march is a protocol**
(design `0e07ce64`, David 2026-09-18, all four questions accepted as
proposed). Measured on #451: 45 routes in the nav catalog; the mocked
Playwright crawl rendered 26 of them and checked only that the shell
painted; behaviour was exercised on ~10; a 35-spec live suite under
`apps/web/tests/smoke` had been run by nothing since 2026-06-18 and
read as coverage; nothing asked whether a page shows what its
department needs. Decided: (1) a platform kind **`page-audit`**
(`infra/platform/workflows/page-audit.toml`), one packet per
catalogued route with `route` and `department` required on the packet,
walking **measure** (the controls inventory beside the department's
in / working / out needs, the gap as a list) → **file** (every gap a
backlog-item carrying route + department; 'none' with a reason) →
**test** (a mocked spec pinning every control — links land on
catalogued routes, buttons answer honestly, back returns, an empty
backend paints an honest empty state, a refused write shows the
refusal — and the reviewer's brief) → **review** (the founder reads
the gap list, the inventory and the page's words; specs and styling
are the agent's) → **styled**, deferred by design as a marker the
machine completes, because the visual redesign lands on pages whose
behaviour is already pinned → **audited**; (2) the opener is a Bun
script, `apps/web/scripts/open-page-audits.ts`, because the ONE
roster is `ROUTE_CATALOG` in `nav-catalog.ts` — TypeScript with no
JSON twin, so a Rust verb could read it only through a second list —
and it opens one packet per route idempotently (open or audited =
skip) in the decided order: the departments whose first protocols
land first (support, sales, product, finance, marketing, hosting),
then the rest, Home's surfaces carried as `it`, IT's own last; (3) a
generic gated interaction crawl over the catalog is the mechanical
half, and the dead live suite is deleted in favour of one nightly
chore crawling the playground whose reds are packets — each its own
car. Tooling stays mocked Playwright — hermetic, gated, already in
the image; "more thorough" is more assertions per page, not a
different tool.

**A department with no protocol is stood down on this instance, not
given pages** (design `36b79159`, David 2026-09-23, accepted as
proposed). The march kept meeting one fact. Maintenance (audit
`015c3935`) had 0 protocols, 0 sensors and 0 rules — its 26
`maintenance-*` rows are IT housekeeping that shares the word, and its
pages sit behind the `equipment` module that `1054c099` turns off for
Algedonic; People (audit `0c0265a3`) had 0 protocols and a roster of
the founder and a system account. Each had a weekly retro with nothing
to retrospect and pages drawing only the demo's shape, and neither is
among the six first protocols (`86f32b7d`). Decided: **such a
department is stood down on this instance** — no weekly retro and no
sidebar pages until a protocol exists (modules-off already covers
Maintenance); People gets a protocol only when a second person joins;
and each remaining audit records "no protocol: stood down" instead of
re-raising it. Not built: the `retro.open` handler behind
`department-retros-weekly` still opens one retro per row of the
`departments` table, and no stood-down marker exists for it or the
sidebar to read.

**Algedonic, LLC keeps its at-scale surfaces, lightly used, to dogfood
them** (design `922f37be`, 2026-09-23; the proposal to turn
`/watchlist` off here was declined). Measured by the `/watchlist` audit
(`08b0c4f8`): no sales protocol produces or reads a churn score, the
accounts directory holds one account (the shared Anonymous Sponsor),
and the nightly batch that scores had fed nothing since the second
stack retired (`9599babc`). David, recorded verbatim by the operator
because the review box was clearing his entry (`fec57f5f`): "I wanted
to keep watchlist and other 'at scale' needs in Algedonic, LLC even
when it is overkill as a means to dogfood our software development. I
want to scale really smoothly, so it is okay investing in protocols
that may be very lightly used until we have real scale. But the
playground instance and simulator are going to be where we really
pressure test modules and behaviors that are relevant for companies
with physical operations and more people than Algedonic, LLC." So
light use is not a reason to take a surface off the company's own
instance; the line is physical operations and headcount, which the
playground and the simulator carry. It differs in kind from the
stand-down above: a department with no protocol at all has nothing to
dogfood. Nothing to build; the batch fix landed on its own (#586).

**The Accounts directory is generic: an account's work is its packets,
and each optional panel waits on its own integration** (design-doc "Accounts shows department work and guards optional
module panels", `f8faef35`, 2026-10-04, answering backlog `4ddeb106`;
its panel half refined by design-doc "Accounts integration activation
and deployment evidence prerequisites", `8f28b94f`, 2026-10-12).
Measured: `apps/web/src/accounts` reads assets, open AR, invoices and
shipments for every tenant, hides a column whose loaded values are all
zero, searches "Account, doctor, city…" and picks a timeline icon by
the literal kinds `field-service` and `sale`. The f8faef35 review was
never signed; the decision on record is David's answer on the item's
review, not accepted as proposed: "Accounts should configured /
re-coded for our OSS business purposes, but we will also need a generic
module for the OSS and the brewery needs it too." Read with the
proposal: (1) the primary view is the account plus its packet work —
an Open work count that excludes closed and cancelled, a link to the
scoped packet list, a history with Workflow labels and one generic
icon, and no Workflow kind named in code; (2) a packet about something
else joins an account only through a Subject reference its admitted
Workflow version declares (kind, step, field), read from completed-step
evidence, never from names or arbitrary metadata, and no published row
or historical envelope is rewritten — the closed sponsorship `49cd9cc9`
records its sponsor only in `reconcile.sponsor_account_id`; (3) assets,
commerce and shipping each read only when that integration is Enabled,
a module bit is presentation (`finance=true` is not commerce), Unknown
says unknown, and an enabled read that fails is a named warning, never
a zero panel; (4) every paginated read compares rows to `total`. The
prose says "department"; in station-tree words that is the page's host
station (design `deba3000`), and the nav catalog still names the v1
owner `sales`, which is no station row. Built: (3)'s source contract,
car `a3038f63` (`boss-core/src/integration.rs`: the declaration, the
observation and the fail-closed Enabled/Disabled/Unknown derivation);
the open-AR server aggregate, car `3486058f`; the current gap pinned
in a mocked spec, car `befbb559`. Not built: the deployment observer,
the entitled gateway reader, the Accounts consumers and the fleet-join
removal, on item `93d83074` (policy trust boundary); (1) and (2), on
item `5901d9fc`. Activation, row publication and the historical
sponsorship link stay a human's.

**The IT department's pages consolidated behind a few rows, families as
tabs, and `/system` is gone** (folded from `it-consolidation.md`, all
four questions decided by David in packet `1f6d55e0`, 2026-08-31):
"Don't put it under /system, we put pages in the department they are
for", and "We do have too many IT pages though. We should
consolidate." Measured: 17 page components in 14 directories, four
surfaces routed under both `/it` and `/system`. Decided: (1) **`/system`
retires entirely** — every old path falls through to the catch-all,
with no redirects; (2) **17 pages become six surfaces with families as
tabs**: the landing at `/it`, `/it/operate` (Incidents, Audit Log,
Performance, Atlas, Bottlenecks), `/it/registry` (Workflows,
Dispatcher, Step plugins, Policy, Subjects), `/it/design` (Reviews,
Experiments, Feedback), `/it/estate` and `/it/kb`, with
`/it/auth-admin` an unlisted door and every tab strip in one
data-driven `ItTabs.svelte`; (3) the duplicate renderings of map, flow
and fleet die, their unique series fold into Atlas, and Fleet survives
as Bottlenecks; (4) **one car, no redirect ceremony** — "It is just me,
and I know all the changes." The sidebar's row count is pinned in
`nav-catalog.test.ts`, and a new row takes a decision of David's: it
went to ten with the Crew Board, the Codebase and the two receiving
yards, then back to **seven** when the Department Map took the yards
as stations on one map (design `e765b3fc`, 2026-09-25). Registry and
doc text still naming a `/system` path resolves to the catch-all until
touched, deliberately not swept.

**Every page belongs to the station that hosts it, and its URL is that
station's address plus the page's slug** (design-doc "Every page
belongs to the station that hosts it", `deba3000`, David 2026-11-10;
no open questions). David: "every station owns its position in the
tree ... that also makes maintenance of the pages the job of the
station that is hosting the pages." A page has one host station, named
by id beside its registration in `nav-catalog.ts`; company-level pages
are hosted at the root, `algedonic`. A page holds the dashboards,
context and reading its station's executors need for the steps they
claim, and a page no station's executors use is a deletion candidate,
judged by the `maintenance-surface-usage` reading rather than by
guesswork. Changing a page is a step at its host station, through that
station's queue. The URL is derived from the host's parent chain, so a
re-parented station moves its pages' URLs with it (/finance and
/finance/vendors, /engineering/gates, /operations/estate). Record views
that belong to no one station, such as a packet or a subject, stay at
the root under a short record path such as `/jobs/:id`. Both the `/ux`
and `/it` prefixes retire as their pages move. Moves are slow and made
car by car, usually when a car touches the page anyway, with no
redirect shims before 1.0.0. An unhosted-page count records the
migration's progress, and when it reaches zero the field becomes
required. This generalises the department rule in the next paragraph,
because a department is just the tier of stations under the root. It
supersedes two parts of that rule: the leaf-code URL, under which a
re-org changed no URL, and the `it` department code. The open
`/ux`-dissolves cars (3, 5, 6, 7 and 8 of `8c3e9599`) are to be re-read
against it, not built to their old targets. Built so far: Finance's
pages sit at /finance (backlog `c7ef4728`), and a station's upstream is
its parent's page rather than a hand-typed route (`a1c0a0ff`). Not
built, as of 2026-11-12 (origin/main `de93d1291`): the catalog has no
host field and still spells 18 `/ux` and 22 `/it` entries by hand. The
cars are backlog items: the host field, the unhosted report and the
root's pages (`03fb75c8`); a station's page listing what it hosts
(`e63139c6`); page-area items landing at the host station (`4eea33e2`);
and the URL derived from the host's address (`0bc2eb68`).

**Every page belongs to a department or to Home, at `/<code>/<slug>`;
`/ux` is deleted outright and its old links stop resolving** (design
`8c3e9599`, David 2026-09-25, the design approved and its one question
answered as proposed: "that roster looks right, go ahead with the
design"; while it was being measured he added: "let's not get trapped by forgetting that
something like Design might be a department and a function within a
department"). Measured on `d093f6e6`: the router's `/ux` block matched
50 patterns and already answered every one without the prefix, so the
prefix was decoration. Page ownership was written four times in four
vocabularies (the catalog's `app` and `department` fields,
`APP_SURFACES`, `IT_GROUPS`/`HOME_GROUPS`), `APP_SUBJECT_KINDS` made a
fifth, and web-kit spelled 15 `/ux` paths by hand. 630 of 19,884 stored
packets (112 open) and 23 page-audit Subjects name a `/ux` URL in the
immutable log. Decided: (1) **two axes, both data.** A *unit* is a row
in the `departments` Subject registry, and its code is its identity.
Nesting, when a roster first needs it, is a nullable `parent` on the
row, a Subject relation; the Class `parent_code` is not used, because
it nests values of one taxonomy. A *practice* is a kind of work
several units do: a Class on the department kind (`member_attribute =
practice`), with many-to-many membership in `departments.practices`
validated at the write door, and `parent_code` nesting sub-practices.
The single-valued `function` axis stays as it is, because a department
has one purpose and several practices. So Design is a department whose
row carries the `design` practice, IT, Product and Marketing carry it
too, and nothing special-cases it. Rejected: a flat page-to-department
map, a practice as a department that others link to, per-tenant
re-homing of surfaces (a second answer to "who owns this page"), and a
multi-valued `function`. (2) **Every catalog entry declares one
owner.** A unit-owned page mounts once at `/<code>/<slug>`. A
practice-owned page mounts under every unit that carries the practice,
scoped to that unit (`/api/jobs?department=<code>`). That one field
replaces `app`, `department`, `APP_SURFACES` and `APP_SUBJECT_KINDS`.
Every unit carries its in / working / out jobs view at `/<code>/jobs`.
Membership (`employees.department`, `agents.department`) decides
nothing about what a person sees, because one human runs every
department; visibility is policy, the module gate and a live registry
row. Membership does move onto department rows as the one vocabulary.
(3) **The URL is `/<code>`, `/<code>/<slug>` or
`/<code>/<slug>/<id>`, never `/<code>/<id>`.** It holds the leaf code
only, so a re-org changes no URL, and the code rather than the display
name. Packets move between departments, so job and step detail stay at
the root, and `/ux/sales/:id` and `/ux/service/:id` fold onto
`/jobs/:id`. **Home is not a department** and holds the root: My Day
`/`, `/inbox`, `/views`, `/schedule`, `/jobs`, `/search`, `/manual`,
and the guest `/system-model` outside the chrome. The registry write
door refuses a department code that equals a reserved root segment
(Home's paths, the gateway's prefixes, the SPA's static root entries),
and a test holds that list against the catalog and the gateway router.
`/<code>` lands on the entry flagged `landing`, else the department's
first entry, else its jobs view. The design's table maps all 50
patterns and deletes no page. A page whose owner is not a department on
this instance renders not-found with one door back to Home, never a
module-off notice. (4) **Old `/ux` paths stop resolving, deliberately,
before 1.0.0.** The design as approved kept every old path working
permanently, through a `legacy: [...]` field on each catalog entry,
alias pins, and a page-audit opener that canonicalised through them.
David superseded that the same day, ~14:30Z: "We can just retire the
UX pages. Remember, this is only a 1-person operation at the moment,
and I know the changes we are making." So `/ux` is deleted outright,
with no legacy field, no alias and no redirect. The stored `/ux` URLs
counted above, the page-audit routes and any bookmark stop resolving
and render not-found. That is a choice, not an oversight: one operator
knows every move made, so an alias would carry nothing he needs.
Keeping old addresses working is still the good habit, and it becomes
a requirement at 1.0.0. Car 1 (`163fdf7b`, the legacy field) was
declined (backlog `d61ba8a0`). The gateway is untouched, because it
already serves the SPA for every path. (5) **Navigation derives from
the registry.** Tabs are Home,
then Simulator when the sim module is on, then every live root
department in `sort_order`. A sidebar lists the department's own
entries, then its practices' surfaces, then its child units, then
Jobs. `entityHref` and GlobalSearch build their paths from catalog
entries that declare `detail: {subjectKind}`. A new department is a
registry row and needs no code. (6) **Algedonic, LLC's roster** (the
one question, an org-chart call): `it` (engineering and operations are
its two halves), `product`, `design` (top-level, owning the brand guide
and design language, `9230dfe4`), `marketing`, `sales`, `support`,
`hosting`, `finance` and `executive`. `people` stays a row so every
actor's name keeps a page, with its sidebar stood down (`36b79159`).
`production`, `warehouse`, `distribution`, `maintenance`, `service` and
`qa` are retired on this instance and kept for the Ales playground.
Not built: all of it. The plan was ten cars, filed as backlog items
that cite the design, and car 1 was declined. The departments write
door and the LLC roster come first and block nothing. Registry-derived
routing comes next, then the four department moves, then the links.
Deleting `/ux` comes last in the routing chain, and Home's pages move
to the root with that deletion, on car 8 (`924ace8b`), because car 1
had carried them. The practice axis rides with `9230dfe4` after the
write door.

**The look is one light theme, Transit, and the map's colours are the
first to route through it** (design `dea94998`, David 2026-09-23: "I
like where we are going with option 2, the route map / transit
option"; answers backlog `42f66fb3`). The direction set that day: one
LIGHT-only theme for the BOSS instance and as the company's overall
tone, flowing into www.algedonic.dev and the GitHub presence — themes
compared side by side first, specifics after — and every UI and
styling car waits on it while back-end work does not. The boards were
compared as a claude.ai Artifact linked from the packet, the gap
exhibits (§Design docs, `26a89f11`) exist to close. Landed (#586,
`42f66fb3`): the groundwork, not the look — the world map (`MapPage`,
`WorldMap`, `RegionMap`) reads only a `--map-*` block in
`apps/web/src/styles.css`, with no fallback and every value still
today's colour, pinned by `it/yard/map-palette.test.ts` so a retired
token fails the test rather than repainting in silence. Not built: the
palette itself (Enamel, below), and the rest of the SPA and www on it;
the SPA still paints the dark theme.

**Its parts are Enamel — Transit taken to station signage — with
comfortable rows** (design `a4df741a`, David 2026-09-24; board: "I
like Enamel. Let's start there."; density: "Let's do comfortable rows.
We are here all the time, so we want it easy on the eyes."). Round 2
drew the parts every page is built from — buttons, state pills, a
packet's progress line, a form with a validation error, the three
honest states (loading, empty, failed), a table at two densities, the
spacing scale, and a composed `/it/design/audits` — twice, from
identical HTML so that only the tokens differed: Transit as drawn
(pill buttons, tinted pills, one blue) and Enamel. Decided: (1)
**Enamel is where the look starts**, on the Transit palette: night ink
`#0E1B2E` on white, `#0F6E9F` for action and progress, Overpass and
Overpass Mono; corners squared to 4px (3px on fields and chips);
buttons uppercase and tracked in 2px frames; keyboard focus an amber
ring, the one amber outside a state; headers and table heads as enamel
bands, white on night ink; **states as solid plates that always carry
their word** — clear `#0B6B4F`, busy `#F2C230` under ink, troubled
`#C8283D`, pending dashed, ready solid blue, completed solid ink; and
**every department owns a line colour, run as a rail down the left of
its pages** — identity, never state, which is why no plate is read by
colour alone. A failed read is a red rail on the same device, so it
reads as "this line is down" and never as an empty list. (2)
**Comfortable rows are the default** (the boards' 12px cell padding
against compact's 5px). The proposal kept compact for working lists —
queues, the yard, the audit log — and the answer names no such
exception; read here as comfortable there too, since those are the
pages the operator is on all the time. The spacing scale is the app's
existing `--s1`–`--s7`, unchanged, with 72 for page gutters only.
"Start there" makes Enamel a starting point rather than a frozen spec:
the board named its own risks, loudness on dense pages and four
department lines plus three states to keep apart, and its line colours
(IT `#0F6E9F`, Brewing `#B98A00`, Finance `#0B6B4F`, People `#7A3E9D`)
and the BOSS mark are sketches, not finished identity — Finance's
sketch is exactly the clear plate's green. The boards were again a
linked claude.ai Artifact, so the values are written here rather than
left behind the link. Not built, any of it: `apps/web/src/styles.css`
still paints the dark Space Grotesk system; the item the doc named to
build these tokens (`42f66fb3`) closed on 2026-09-23 on the map's
`--map-*` groundwork alone, and no open item carries them; and every
page audit's `styled` step is still the marker the machine skips
(`infra/platform/workflows/page-audit.toml`), so applying the look per
route waits on a new version of that kind.

**The departure board: IT's landing shows its work moving, as a lens
over the delivery pipeline's own packets** (folded from
`departure-board.md`, all five questions decided by David in the in-app
decision tracker on 2026-08-14, no packet id; origin his feedback
`2e98b211`: "We are still missing good visuals for the IT department to
watch our information trains"). The pipeline was already four queues —
cars parked at the dock, trains boarded, trains departed, and the
arrivals — so the board adds **no projection and no state**: every row
derives from `ship-a-change` and `pr-train` packets, and every
transition it animates is a marker event already emitted. Decided: (1)
**the board is the IT app's landing and guests may view it**, so every
read it makes is safe for a read-only audience — jobs-API reads only,
the CI lamps reading the train's own `ci` step and never the forge API
a guest cannot reach; (2) **the cluster arrival enters the record** as
an event the train can read, shown until then as converging and marked
observed-not-recorded; (3) **the consist shows chips**, branch and
title, with the car's own page one click away — the board is for
watching, not working; (4) **history is the last five arrivals with
three stage timings each**, computed from step timestamps at read time,
with no new table; (5) **the lamps ship on the SSE markers plus a
poll, and the car that moves them onto `forge.check.completed` deletes
the poll**. Where it stands: `/it` is still the landing and still
guest-readable, now as the Department Map (design `e765b3fc`), whose
selection panel is drawn as a departure-board plate; the yard's
`DepartureBoard.svelte` still lists every wagon; and the train's `converged` step records the
cluster arrival (`infra/platform/workflows/pr-train.toml`).

**One map per floor: the region map draws its own slice of the yard,
and YardPage retires** (design `fe77a1d2`, David 2026-09-23, all three
questions accepted as proposed; answers backlog `c0565f48`, finishing
the consolidation design `d2154293` decided on 2026-09-19 — "let's
have this become THE surface"). Measured: `/it/yard/<region>` already
swapped to the region's own map (`ca37478f`), but for the six floor
regions `MapPage` still mounted `<YardPage focus={region} embedded>`
beneath it, and embedded YardPage still drew `YardMap` — the WHOLE
six-region floor — so `/it/yard/dock` showed the dock's wagons twice, a
plate and then a wagon, with five other regions underneath. Two facts
set the order: the region map's plates cannot be clicked, so every
selection that drives the entity panel and its verbs comes from
`YardMap` or the departure board; and `YardMap`'s drawing already
splits on region lines through the total `STATION_REGION` record in
`region-contents.ts`. The platform regions (receiving, marshalling,
shop floor) were already the end state — a region map, then that
region's working panels. Decided: (1) **each floor region draws only
its own slice**, so a wagon crossing from the dock to the track leaves
one map and appears on the next rather than sliding across one floor;
the crossing stays visible at the world level, where the borders carry
it, and the departure board still lists every wagon — a whole-floor
seventh view is the second page the consolidation removes; (2) **three
cars, and no detail leaves before its replacement is on screen**: split
`YardMap` into one pure layout function per region, keyed through
`STATION_REGION`, with no change on screen and a test that the six
slices' union draws what `YardMap` draws today; then `RegionMap` draws
its slice with `selected` and `onselect`, so every wagon, bay and
locomotive selects into the entity panel, and embedded YardPage stops
drawing `YardMap` in that same car; then the deck (alerts, departure
board, entity panel and verbs, production and signals) moves into a
`FloorDeck` that `MapPage` mounts, the source-reading tests repoint, and
`YardPage.svelte` is deleted, every route unchanged (`/it/yard` still
resolves to the track). The last two both edit `MapPage`, so they ride
one after the other; (3) **built now in the `--map-*` tokens**, not held
for the Transit reskin above, which chooses how the map looks rather
than what it shows and will then restyle one map per region instead of
two. Built, all three cars (2026-09-24): `floor-slices.ts` lays the
floor out one region at a time, `RegionMap` draws its slice through
`RegionFloor.svelte` and `YardMap.svelte` is deleted, and the deck is
`FloorDeck.svelte`, mounted by `MapPage` — `YardPage.svelte` is gone,
and with it the wagon plates' `interiorLayout` that nothing read once
the region map drew the floor.

**The map's meaning is fixed before its drawing: three state words, a
partition, the order a car walks, borders drawn** (design `62de32ae`,
"The IT map, round 3", David 2026-09-24, its one question accepted as
proposed; the build rode as parts of feedback `c3105b2a`). The later
[station alarm decision](#station-alarms-state-the-action-the-factory-needs)
refines its severity meanings and retains the four existing wire states.
A fresh-eyes
review that day, by an agent that had not seen the map's history and
rendered it against live reads at 1440 and 390 px, found the
foundations right — one shared world, flow-ordered territories, the
one server read `boss orient` prints too, unknown drawn as unknown, the
view swap at `/it/yard/<region>`, all kept — and the map failing the
five-second test: six of ten regions the same amber BUSY, the one red
that was ours buried in a 700-character sentence, the borders' best
content in hover titles, the dock drawn before the gates, the middle
third the thinnest part, and no phone view. Decided: (1) **one state
vocabulary, regions and borders alike — clear, attention, troubled** —
clear is flowing within declared bounds, busy-and-healthy included, so
a dock with a train due is clear; attention is a declared band crossed;
troubled is ours and not moving (the shed's rule, `3881f5c9`, on every
region) or a reading that could not be taken; every non-clear state
names the band that decided it ("oldest 7d > 3d band"). `busy` is gone
rather than aliased, so a paragraph above that says a region "stays
busy" now reads attention, and Enamel's busy plate is the attention
plate; (2) **hysteresis on the server, read from the record**: a band's
condition changes the state only after holding for the band's declared
period, measured from the instant the record says it began, so the
judgement is a pure function of the rows across restarts, replicas and
scopes ("troubled for 16m"); a remembered-state layer was rejected for
exactly that. As built it damps ONSET only — a condition clears at
once, because the record holds when a condition began and mostly not
when it last held — and one whose onset is unrecorded is stated at once
with no duration; (3) **the line is the order a car walks it** —
receiving → marshalling → shop floor → gates → dock → track → arrivals
→ shed, the garage a siding under gates..track and publish off
arrivals — with the rails redefined to match (a gate-run opened, parked
on green, boarded, judged red); the old line put the dock before the
gates and a car walked backwards through two regions; (4) **the
regions are a partition**: the original implementation keeps a packet in
receiving until a non-trigger step completes (superseded by the explicit
intake decision below, not yet implemented), and in marshalling only after; the
eight named regions claim first and those two take the remainder, one
server test (`no_job_id_is_counted_in_two_regions`) pins it, and
receiving reads past its 500-row page instead of flooring — the world's
"669 waiting at the borders" had counted about 260 backlog items twice;
(5) **every KPI states its unit and every interior draws its header's
count or names what it leaves out**, with a bound declared as
`capacity` or `threshold` (the dock's "6 / 1" was six cars against a
boarding threshold of one, not a space for one); (6) **borders are
drawn, not tooltipped**: the rails leave the territories and run along
a line above them with room to write the machine's name and lamp on the
rail and the rate beneath, rail width follows the logarithm of the rate
(`railWidth`, presentation only — the number is printed), and a click
opens the crossing inline under the map; (7) **a region owns its page**:
"IT · Dock" under a breadcrumb, the world's summary line replaced by the
region's own in and out rails, and the departure board and alerts strip
scoped to the region, with what they leave to other regions counted
rather than dropped (`region-page.ts`) — folded into `fe77a1d2`'s third
car rather than moving the deck whole; (8) **the shop floor draws agents
as actors** — an identity heading its sessions, a lamp per session with
its idle age, a lamp per run labelled with run, item and step, and a
run no session claims drawn on a "no open session" row, never dropped;
(9) **one KPI per region**, the server's sentence and unit, which the
map prints and never builds; (11) **machinery that serves every region
stands in none**: the host runners move to a plant strip along the
world's edge (`plant` on `/api/yard/regions`), and stations the server
cannot judge are counted in marshalling's header ("2 stations
unjudged") rather than drawn clear; (12) **a phone gets
a vertical strip map**, one row per region in flow order under the
server's thirds, each row the state, the KPI and the rail coming in,
and the SVG world is not shrunk. The question — the strip before 1.0.0
or after — was answered as proposed: after cars A–D, as car G.
Decision (10), a one-line HUD, was replaced by its own design, the HUD
frame (`00774ca8`, one row per third), which is folded on its own
packet. Built, 2026-09-24, every car: B the partition (#628), A the
states, bands, hysteresis and units (`region_states.rs`, #629), C the
order and the drawn borders (#630), F the HUD frame and the server's
`thirds` (#632), E the actors, the plant strip and the rest of (5)
(#634, with the region pages of (7)), and G the phone strip
(`PhoneStrip.svelte`, #637). The second half of receiving's KPI, the
share whose channel is unrecorded, waited as residue on `c3105b2a`
while the channel rule lived only in the client; backlog `1eea4554`
moved the rule to the server (`boss_jobs::channels::lane_of`: the
filer's `input_channel`, or `unclassified`), where the region measures
the share over what stands and the jobs list hands each row its lane on
`lane=true`, so the receiving board draws the server's reading and
keeps no vocabulary of its own.

**Receiving is an explicit phase of the pinned protocol.** (Engineering
decision `e37f5f51`, 2026-11-16; backlog `49391a67`.) An optional typed
`intake = { step = <slug> }` declaration names one existing non-trigger,
non-terminal step. No declaration means no receiving phase. Category,
kind names and completion of unrelated work do not imply intake. Only
the declared step's completion accepts the packet; ready, active and
skipped do not. Its recorded completion time is the crossing time; a
missing stamp stays unknown. This keeps a working page audit or an open
work-session from appearing to wait for triage throughout its life.

Read the declaration from each packet's pinned protocol version. An
unread or malformed specification leaves placement unavailable, never
an empty receiving queue. Keep specific station/equipment placement and
train carriage precedence, and derive receiving, marshalling, board
membership, counts, border crossings and moves from one placement
reading. A packet without intake remains visible at its declared work
station and in its actor's queue.

**Not built.** Inventory active and in-flight pinned specifications,
publish declarations for real intake decisions and use recorded packet
conversion only where its evidence checks permit. Historical versions
remain immutable; omission on an old version also means no receiving
phase. Record rollout reclassification under the placement model
generation, separately from operational movement: never invent a
triage completion or crossing time at deployment. The implementation
must prove pinned-version handling, unknown reads, exact-step acceptance,
disjoint membership and deployed positive controls. Station pagination
already uses `list_every` and is outside this repair. The design packet
holds the acceptance matrix and rollout constraints.

**A placement-model transition is recorded separately from work.**
Design `e5cfd464` (agent-codex, 2026-11-16; answers `e8eed4ac`, no open
questions) settles the generation boundary required before the intake
classification of `e37f5f51` activates. Extend the existing partition
and mover with one immutable placement-model value. Its canonical
manifest names the evaluator contract and retained artifacts, station
and routing declarations, and exact pinned protocol versions and
packet bindings used at the observation cut. Work facts, clock and
audit cursor are separate inputs. Retain the manifest, not only its
digest; source hashes alone do not identify mutable registry inputs.
Missing, partial, malformed or mixed-revision inputs leave the last
durable checkpoint intact and expose an unavailable observation,
never an empty registry or a successful model checkpoint.

Persist the audit cut, model identity, replayable work-state reference
and placement snapshot. For a batch changing work and model, evaluate
P(old_model, old_work), P(old_model, new_work), then
P(new_model, new_work) through the same partition. The first comparison
records ordinary crossings with their actual causal event IDs,
sequences and timestamps. The second records model reclassification
with old/new identities, cuts, provenance and before/after placements;
it creates no ordinary crossing, fake triage or intake completion time.
This work-then-model order is the declared accounting convention, not
a claim about wall-clock interleaving. Retain old packet bindings for
the first comparison and name admission or repin evidence in the
second. New packets retain ordinary admission semantics. Even a net-zero
final position may contain a real crossing followed by reclassification.

Commit movements, transition and next checkpoint atomically, with an
outbox fact, deterministic identities, uniqueness and a checkpoint
fence for competing movers. Restart loads the durable checkpoint;
an initial baseline states its missing prior coverage. A missing old
evaluator or pruned interval is an explicit unknown gap, never a silent
resync claimed to preserve real crossings. Retain both evaluators or
establish the old evaluator's complete cut before an incompatible
upgrade. Counts, queues and the mover share the accepted model, while
ordinary rates and dots consume only operational movements.

**Not built:** two implementation cars first add the model, complete
pinned resolver, pure fold and checkpoint/transition port, then wire
the existing mover, persistence/outbox, restart and API evidence.
Tests must conserve same-batch real crossings alongside registry-only
reclassification, including net-zero results, unread inputs, repins,
crashes, duplicate replay and competing movers. A deployed machine-run
probe must show a model-only transition with zero ordinary crossings
and a mixed batch preserving its real cause. Intake publication and
conversion remain on parent `49391a67`, blocked until this prerequisite
is implemented and proven. This fold supplies the contract only.

**The map moves only where the record says work is moving, so
stillness is the stall signal** (design `31bade8f`, "The IT map moves",
David 2026-09-24, both questions accepted as proposed; answers backlog
`d220022f` — "more ambitious with our real-time animations … to more
easily see the stalled areas"). It builds on the round-3 map design
`62de32ae`, which decides what the map means; this decides how it
moves, and changes no count and no state. Measured that afternoon: the
map redrew every 10 s and nothing moved between reads, its rails' CSS
dash in five `densityOf` bands was decoration rather than a rate
(`heavy` spanned 20 to 501 crossings a day), and every border already
reported `rate.current` and `rate.previous`, `waiting`, its `holds`,
`last_crossed` and its machine's silence — so motion needed almost no
new facts. Decided: (1) **motion means work and only work** — crossings
travel and running machines animate, nothing else moves, no ambient
shimmer or pulse; (2) **a token is a crossing replayed at the rail's
measured 24 h rate**, action blue, evenly spaced (a Poisson draw would
invent bursts the record does not hold), every rail at the SAME speed
so density alone carries the rate, prefilled to steady state on load,
aggregated above 5 a second with the rail labelled "●=k", and the time
compression always stated on screen — **×600 by default** (Q1: one
real hour is 6 s; pause, ×60 and ×3600 beside it) under "tokens replay
each rail's measured 24h rate; they are not individual events"; (3)
**waiting is a pile whose printed number is the server's `waiting`**,
never a count of squares, "+N" past the grid, each square shaded by its
hold's class — solid ink for a machine, hollow ink for a person or the
world, grey for cannot tell, red for stuck and ours, the red settling
as still sediment at the pile's base with "N stuck · held Xh"; (4) **a
stall is stillness judged on the server, in three kinds** — a rail
troubled, at rate 0 or `flowing: false` stops emitting, turns red and
shows "held 3h 12m"; a region past its band stops its machine glyphs
and names the band on its plate while its outbound rails keep their
true rate; and a rail at 0 against a nonzero previous window is drawn
still with "0 vs 4 /day" — and the client never decides a rail has
stalled; (5) **unknown is still and grey**, never animated; (6) **no
number the server did not report is animated**: a pile changes only on
a read, clocks tick locally from the server's own timestamps, and past
3 missed reads the whole map desaturates with "not read for 40 s"; (7)
**poll the aggregates, push the crossings** (`docs/design/sse-policy.md`)
— regions and borders stay on the 10 s poll, while a
`GET /api/yard/stream` emits one `crossing` frame per crossing,
classified by the code in `boss_jobs::borders` that computes the rate,
drawn as a real, ink-ringed token over the replay; (8) **the server
adds the only new facts** — per border `flowing`, `held_since` and
`holds_by_class`, printed by `boss orient` so the one server read stays
shared; (9) **reduced motion is honoured by default** with the page's
own toggle, the rate drawn as static dots at the moving spacing, and
every stall still carrying a word and a colour; (10) **one `<canvas>`
over the SVG world**, tokens in arrays not DOM nodes, at most 60 in
flight a rail, `requestAnimationFrame` stopped when hidden or still, a
2 ms frame budget at 500 tokens, and the arithmetic in pure functions a
unit test pins, never a pixel snapshot; (11) **region pages speak the
same grammar** at their zoom; (12) Enamel's colour roles hold in motion
— blue moving, ink waiting, red stalled, amber only the selected rail.
And **entering troubled gets one expanding ring, once** (Q2) — never a
loop, none under reduced motion. One engineering correction, recorded
on the packet while M1 was built: the still rule is **4 of the rail's
OWN mean gaps** (`STILL_AFTER_GAPS`, band `border-still` in
`region_states.rs`), never a machine's declared cadence, because a
cadence is how often a machine runs, not how often its rail is crossed
— `train-reconcile` fires every 10 minutes while track → garage is
crossed about four times a day; the machine's heartbeat stays judged as
`machine.silent`. Built (2026-09-24): the server half (M1, #636) —
`flowing`, `held_since`, `flowing_why` and `holds_by_class` on
`GET /api/yard/borders`, printed by `boss orient` — and the client
motion layer (M2, #639) — `MotionLayer.svelte` over `WorldMap`, its
arithmetic in `world-motion.ts` (`emitPerSec`, `pileOf`, `railStill`,
the ring), pinned by `world-motion.test.ts` and
`it-map-motion.mocked.spec.ts`. Not built: the crossing stream and its
real-event tokens (M3, decision 7), so the map runs the replay layer
alone and says so; and region pages adopting the vocabulary (M4).

**Agent B is not a page: it is the SENSORS station on the map, and its
panel shows each inbox's mail arriving and leaving** (design
`eb008249`, David 2026-09-28, no open questions; answers backlog
`486b0753`, his 2026-09-21 ask "have 'Agent B' as a page in the IT
Department where I can see the mail sensor and outbound email activity
flowing by department inbox"). Measured that day: nothing to render
yet — `GET /api/sensors` held three rows (`stripe-payouts`,
`stripe-sponsorships`, `www-visits`), none of source `mail`; no mail
adapter reads Bridge; `receive-a-message` answered 404 beside a 200 for
`backlog-item`; `MailTransport` has no SMTP arm; and the credentials
registry holds no Agent B row. Since the item was filed, the Department
Map (`e765b3fc`) turned pages into selection details under one map and
`16091dfb` made that map a transit monitor, so the question became which
station this is and what its panel shows. Decided: (1) **a station, not
a page** — it stands where outside traffic enters, selecting it opens the
panel below the map, and there is no new route and no sidebar row; (2)
**the station is SENSORS and mail is one source on it** — one panel row
per sensor row (id, source, selector — the inbox address for mail — the
protocol it opens, last polled), because the sensor registry is already
the one list of inboxes (`bffc0aba`), so a panel that only reads it
cannot keep a second list; it renders today with the three live rows,
and `support@` and `finance@` arrive later as rows with no page change;
(3) **arriving is the readings door that exists**,
`GET /api/sensors/{id}/readings?since=` over the station counts' 24 h
window — arrived, stamped, unstamped, packets opened — with no new API;
an unstamped reading older than a poll interval turns the station
attention, an open urgent `sensor_unreadable:<id>` turns it troubled, so
a logged-out Bridge shows where it happens; (4) **leaving is the
completed, presence-approved `sent` step, never a transport log** — each
counted under the sensor whose reading opened the packet and linked to
the packet rather than copied, a failed send shown held, and until the
SMTP transport and the reply car exist the column reads "no outbound
transport", never 0, because a zero there claims nothing was sent; (5)
**each department map starts at its own inbox** by sensor row →
protocol → department, the IT station showing every inbox; (6) **"Agent
B" is a label, not a key** — rows sharing a source credential group
under it by the `credential` column, with no brand word in code, so a
second mailbox groups itself; (7) **order** — (1)–(3) are one read-only
car buildable now behind the transit map's flight, (4) waits for the
support `receive-a-message` car (`86f32b7d`) and its send step, (5) for
the department-map car of `e765b3fc`, and the pixels for the redesign;
this design fixes structure and data only. Left undecided on purpose:
where the mail adapter runs (the support build under `bffc0aba` and
`0390f7d2`) and the station's colours and marks (Design's tokens,
`16091dfb` Q1, `e765b3fc` Q2). The first car owes a probe: the panel
lists exactly the rows `GET /api/sensors` returns, and each row's
arrived count equals that sensor's readings door over the last 24 h,
row by row. Not built, any of it (2026-09-29): no sensors station or
panel exists in `apps/web`.

**What the website says is checked against what the record holds;
whether it works is a reading with a threshold named first** (design
`59a776c5`, 2026-09-20). Correct and effective are two protocols with
two kinds of evidence. **Correct is a check:** a claim is marked in the
page source (`data-claim="<id>"`) and a registry
(`apps/web/src/marketing/claims.ts`) says which row answers it — David:
"Marked claims are fine to start, let's go with that." The limit is
accepted in the open: an UNMARKED claim is invisible to the check, so
the checker's report carries the marked count and a prose fingerprint —
the decidable stand-in for a coverage denominator nothing can know,
since words that moved while the claim set did not are the prompt to
mark what was added — and a claim whose row cannot be read is a
refusal, never a green. The scope is www only, a site being a row so a
second is data. The design decided a daily `check-the-claims` protocol
plus a run on every publish, a disagreement FILING rather than paging —
a price included, until one instance says a louder arm was wanted. The
caller that shipped first is a gate test instead
(`the-landing-page-claims.test.ts`, backlog `e1524f57`), where a
disagreement reds the gate: every source kind today is a fact of the
tree and can only drift through a commit, so the cadence arrives with
the first `registry` source kind, which reads live data and moves
without one. **Effective is `measure-the-page`**, already
decided by the department template (`3613f0af`): visits by page and by
week, and nothing else until that sensor has produced a decision, with
the step requiring the reader to write down what the number would have
to say to change anything BEFORE taking the reading — not yet built.

**A train is on the map at all times, drawn once, where the server says it
stands.** Design `1c28b67f` (feedback f31a46e5, David 2026-09-29), five
questions decided as proposed on 2026-10-22. The server is the one
definition of where an open train stands (`train_region`: boarding at the
dock, awaiting CI or merge at the gates, deploying or converging on the
track), so a region carries `trains: [{id, cars, phase}]` and the client
draws what it is handed rather than deriving it again. A standing train is
the same bar with a pip per car that a crossing train is, up to three per
station and then `+n`, laid out by a pure function pinned not to cover the
station's ring or words. A train with a dot in flight is not also drawn
standing, because a train in two places is a picture of something that did
not happen. A click selects the station (one tap, one target) and the
title names the train, its cars and its phase. Only trains stand; cars,
gate-runs and other packets keep their counts. This is current activity,
so it carries onto the station-tree floor (T7) when the transit map
retires.

**Every absolute time on the station tree reads local and UTC side by
side.** Design-doc "The station tree shows UTC beside local time"
(`b03ef2d4`, answering feedback 0b1fd59f, David 2026-11-08: "We need to
show UTC in addition to local time since the factory runs on UTC"),
decided with no open question. A time the station-tree floor or a station
page draws (`apps/web/src/it/tree/`) reads `17:42 PT · 01:42Z`, the UTC
half in secondary ink on the same line, because UTC is the stack's clock:
the record, the alarms and the handoffs are written in it. One formatter
does it, `formatLocalAndUtc` in the SPA's one date module
(`libs/web-kit/src/ui/date.ts`), drawn by `LocalUtc.svelte`, and no page
formats its own; relative ages ("12m ago") keep their form. Built by car
3a01f5e2 (train #1699, 2026-11-12) for the Working column's since and the
StationLine and StationMotion moves lists. Not built: the right rail's
Moves list (`StationRail.svelte`, train #1704) still prints `hh:mm:ssZ`
sliced from the ISO string, UTC alone, in its own code (backlog aa1b9ea6).

**The top board shows what David reads.** Design `b31eb68e` (feedback
d0d46f02, David 2026-09-30), five questions decided as proposed on
2026-10-22: the MACHINES row leaves the top board (a failed machine already
troubles its own region and stands on the alarms board; the row added only
an unjudged count), so the ALARMS column holds Outranks alone; Next up drops
the gate line and every dispatcher scheduled-rule row, deleting their reads
if nothing else uses them; Next up holds **three** events across timed and
untimed rows, soonest first, while a source that could not be read still
adds its own row, because an unread source left off would read as nothing
coming; and the cut lives in the server's one definition (`next_up.rs`),
so the board and `boss orient` print the same three. The map scrolling
with the page was recorded as already shipped (train #1144).

**A station acts on its packets only through doors that already exist.**
Design `ad3989ff` (feedback 2b713c5d, David 2026-10-09), five questions
decided as proposed on 2026-10-22: every packet a station panel lists links
to its packet page, where its steps' own doors are, and a row carries a
button only where the website already has a door (the dock's release and
the track's cancel), because a control is a link to the step's surface and
the approval door is a trust boundary a map must not route around. Garage
stranded and held greens anchor to their gate-run packet with the short
sha, and a row with no packet id says "no packet named" rather than
linking to nothing. A website door to free a dead run's step (`boss step
release`) is a trust-boundary verb of its own, filed separately. The exits'
removal and the dock's release view were recorded as shipped, not rebuilt;
the overview keeps its orientation panel.

**A station page answers in its header, and its body is the line the
packets move along.** Design-doc "The engineering page answers in five
seconds, and shows jobs moving through stations and their machines"
(`08d1530e`, reviewed by David 2026-11-12, its three questions decided as
proposed). Measured before it on `/it/tree/engineering`: the plate read
CLEAR at all times, the same stations were drawn three times (parts grid,
motion band, In/Working/Out), and the motion band opened on a protocol
with no packets. Decided: the header shows Working, Stuck and Needs you as
linked counts and the binding constraint. The body is the owned protocol
with the most open packets, drawn as a line of station boxes, and the
other protocols are count chips and thin strips. Each box shows its
admission queue, its seats or bays as cells holding a named packet and its
dwell, and every internal path, bundled above four in parallel. The boxes
sit on the line, not in a second view (Q1). A packet the record does not
place waits in a tray by step and pinned version, and is never placed by
guessing. A packet moves on screen only when a recorded move names it,
and nothing replays. The right rail holds the waits ladder, Stuck, Needs
you and the pools as one strip, with the caveat said once. A car's mark
comes from its claim's `executor_kind`. Engineering has its own line
colour, the IT blue it used to borrow (Q2). Amber means past the step's
budget and red means stuck: an idle machine with a queue (Q3). Built as of
2026-11-12 (origin/main `0c15d43bc`): the station line, with admission
columns, seats, gate bays read from `gate_bay` and `gate_reservation`,
internal paths, executor marks and the tray (`StationLine.svelte`, train
#1701, backlog `9d0b0944`); and the answer band, the right rail, In and
Out deleted, `--map-line-engineering`, and `/engineering` routed to the
station page (train #1704, backlog `950ad972`). The queue read's slim
`fields=` option was dropped, because the page no longer reads the queue.
Not built: the parts grid and the Working column still render beside the
line (`70857db1`); the thin strips (`e560f66e`); amber, which no read on
the page judges yet (`d512d3d6`); and the tray's link to the cohort
convert (`86cc4689`), after `boss job convert --cohort` lands (car
`d59e29b5`). Hops wait on three server cars, in order: taking a gate bay
becomes a claim, and steps carry `ready_at` and `admitted_at` (`0ef800c6`);
moves are keyed by tree station, step and seat, not by the retired regions
in `moves.rs` (`de0fddb5`); then the glide itself (`513670f8`). The build
station's robot seats fill only when claims stand at their step's tree
station (car `541c8c8d`, parked). David set the order the rest of the data
arrives in, one car at a time: steps placed at their stations, the cohort
convert, station admission queues (`424329a5`), claims at tree stations,
the three hop cars, then `1893ad4d` and `41ec2e4f`.

**The company-to-department map was built, then retired with the transit
map; its accuracy contracts live on in the station page.** Design-doc
"System Network Map: orient first, inspect deliberately"
(`d4dada70`, David 2026-10-02, both questions decided as proposed;
answers feedback `e847f969`) set a presentation order: a company map
with departments as regions, IT the one connected region and every
other department a stub reading "Not connected", then a simplified IT
transit overview, then a station or route overview, then the queue or
the packet. Each level was a presentation scope over existing
registries, never a new entity. Built the same day (train #895, car
`1d8ecfff`): `/map` as `CompanyMap.svelte`, and `/it?view=overview`.
Retired since, and not to be rebuilt: the transit map and its overview
went with R3 of backlog `23a4b4d7` (trains #1638 and #1639, 2026-11-09;
David, 2026-11-08: the visuals show real, current activity only);
`CompanyMap` and `/map` went with backlog `858e62aa` (train #1647,
2026-11-09); and IT as a region went with design `df0ecd8b` (§IT
retires as a department). The order survives in station-tree words:
`/it/tree` is the company index, `/it` draws Needs you, the alarms
board and the root station's page, a station page answers in its header
(`08d1530e`, above), and a station's packets link to their packet pages
(`ad3989ff`). What carries over is the design's accuracy contracts, and
the station page keeps them: a failed read is said, drawn as a red rail
and never as an empty list; a packet moves on screen only when a
recorded move names it; and a packet the record does not place is never
placed by guessing. The defect the design set aside, merged cars read
as stranded greens once they aged past a 400-row car read, was repaired
on its own (`d59c4a37`: the yard reads every scoped car), and runtime
roster ingress stays its own item (`daa8f63f`). Nothing of this design
remains to build.

### The transit map and its boards: decisions carried into the station tree

These eight decisions describe the earlier IT map and its boards. The
station tree supersedes that map: its geography, flights and retired
routes are history, not instructions to rebuild them. The station-page
contracts above govern current visuals; recording a decision here is
not evidence that its implementation or browser verification passed.

**The transit grammar** (`16091dfb`, David, 2026-09-24) replaced
territories with stations and schematic route lines, keeping route
colours separate from clear, attention and troubled state marks. David
approved provisional Design-owned delivery blue, publish purple, siding
grey and tenant amber on a light ground, and a David-first transit
flight over the existing reads, with the world map remaining default
until an observed promotion. Those colours and that flight describe the
old map; this fold neither promotes a flight nor restores rate-replay
animation or a planned tenant line without a source.

**The Department Map** (`e765b3fc`, David, 2026-09-25) required
routes derived from protocol edges, declared hand-offs and observed
moves, with undeclared observations visibly troubled. A dependency such
as reading main was not packet travel. One placement function supplied
counts and motion; each move cited its causing event, was idempotent on
event and packet, and preserved lineage across identity changes. Boot
and resync established a baseline without inventing moves. David let
motion cars proceed and chose a solid green full mark, a heavy ink train
line and the blue car line, with gates as interchange: full meant useful
capacity while work flowed, not stuck. The map collapsed pages into
selection details and retired IT aliases without redirects before 1.0.
Its geography and below-map panels are superseded; sourced movement,
honest missing readings and no synthetic replay remain accuracy
constraints. The old 40-dot, hidden-tab and reduced-motion rules were
presentation bounds, never permission to omit events from the record.

**Station exits** (`f0313eda`, David, 2026-09-26) chose E2, a dedicated
exits band: one fan per station, alphabetically stable terminal branches,
buffer stops on one rail and readable angled labels, with wide fans
jogging aside rather than crossing routes. The server supplied terminal
counts at move time; branch counts conserved exits. Unknown readings
used a hollow tube, undeclared exits dashed red and held sections solid
red. The partial observation window could not establish a standing
balance. The band is retired map geometry; its lesson is to distinguish
missing evidence from a finding and never force incomplete history to
balance.

**The top board** (`ea906603`, David, 2026-09-27) replaced flow
arithmetic with Outranks regular order, Needs you, Next up and Machines.
Needs you resolved the signed-in viewer through registry aliases and
policy roles in the shared assignments read. Next up came from registry
schedules and measured journeys, in the viewer's timezone; maintenance
times held only in configuration were omitted until registered. A dark
source remained unread, not an empty healthy row. David's Q5 removed the
three-thirds figures for now, overriding the proposed collapsed flow
disclosure; it did not authorize deleting the underlying evidence.
The former HUD layout is history; the viewer scope, priority ordering
and honest unread state still constrain the boards.

**An alarm's promised ETA** (`1ef7c771`, all five resolutions accepted)
belonged to the open packet with owner, plan_of_action, eta_utc and the
station link then named alarm_region, without another table or client
join. Attention items followed the same rule as alarms. Several plans
showed the earliest ETA and counted the others; an unreadable ETA stayed
visible, a passed promise rendered red, and neither a plan nor its ETA
cleared the station's judged state. This records an actor's promise,
not a system prediction. The retired region join is not a new station
implementation; packet provenance and the distinction between a plan
and recovery survive it.

**Alarms left, controls right** (`3dfdb8c4`, all three resolutions
accepted) was a layout change over the existing top-board reads:
Outranks and Machines on the left, Needs you on the right, Next up below,
stacking in that order on phones with each row's own error boundary.
Controls were links to the viewer's step surfaces, not a new global
button board. No map click bypassed the approval door, and machinery
verbs stayed with their station. The HUD's columns are superseded;
the step's own authority boundary is not.

**Ultra-wide layout** (`92f97670`, all four resolutions accepted)
was an optional browser-local choice, off by default, with refused
storage reads falling back to off and refused writes preserving the
tab's choice. At 1800 pixels and wider it spent width on the map and
selection panel side by side, capped the drawing to the viewport and
kept a full-width map without a selection. Narrower windows ignored the
stored choice. A second world geometry, server preference and company
map changes were excluded. This is the old map's layout history, not a
requirement to restore its panel or preference in the station tree.

**Individual packets in simultaneous traffic** (`b1c0acce`, all five
resolutions accepted) kept one dot per recorded move and ordered starts
on each route by sequence with a 1500 ms headway, longer than the
1000 ms arrival-burst window. Waiting dots were hidden until departure;
different routes did not delay one another. Only an already recorded
train with cars aboard was one batched mark. The 40-dot cap and explicit
overflow count remained, and labels kept record time despite display
spacing. This retired transit animation must not be revived as replay
in the station tree: real current movement and explicit recorded
batching survive, while a display delay never rewrites event time.

**Workflow UX as data** (flattened from `workflow-ux-as-data.md`,
review `b4cc0406`, David, 2026-08-19) extended the StepPlugin principle
to workflow views while keeping generic surfaces as an always available
fallback. The settled choice was one plugin registry, not a sibling
table per surface. The host contract included the step props, a provided
client with readJob, merge-safe patchStep, route(edge) and listAtNode,
plus optional DAG and schema components; drag-and-drop started with a
kit component but allowed per-view freedom. Publishing required policy
on (publish, ui-plugin), and plugin smoke coverage belonged in the
CI-gated mocked suite. Contracts version additively, destructive changes
in two steps. The protocol still enforces legal transitions beneath any
view; scratch state becomes authoritative only through a recorded
write. These are accepted contracts, not a claim that every host client,
workflow slot or smoke harness has shipped. Current metadata writes use
the step merge door, followed by a separate status update; the old
document's metadata-merge PUT wording is superseded.

## OSS posture & tier boundaries

Two install paths: single-VM bare metal (`infra/oss-quickstart/`)
and Docker compose. File-backed auth is for evaluation; HA
topologies return as opt-in blueprints under `infra/blueprints/`.
Crates split into **Tier 1 — core state-machine OS**
(`crates/core/`, 26 crates: the four primitives' services, policy,
gateway, dispatcher, clock, expression DSL, taxonomy registries,
calendar, content, docs, ML stack, testing, ports,
plus `*-client` crates) and **Tier 2 — company-modeling layer**
(`crates/modules/`, 16 crates: people, accounts, commerce,
inventory, shipping, ledger, products, messages, catalog, assets,
clients, ML plugins). A non-company tenant deploys Tier 1 alone.
**Orchestrators** (`crates/orchestrators/`: `boss-rebuild`,
`boss-cli`, `boss-sim`, `boss-ml-api`, `boss-simulator`) fan out
across tiers by design; **tenants** (`crates/tenants/`: brewery
engine) carry tenant binaries.
`infra/lint/tier-import-audit.sh` enforces
Tier-1-never-imports-Tier-2 for libraries. Seeds never write
emergent state — if a seed wants to `INSERT INTO invoices`, the
answer is a Workflow (`docs/design/seed-vs-emergent-state.md`,
enforced by `seed-bypass-smell.sh`); the canonical demo world is
**built live, not migrated**: the install starts the sim and it
generates 365 simulated days of events against the live API.

**Which tier a path is in is one file, read three ways** (design-doc
"Tiers are one registry, read three ways", `01c3cc3f`, David
2026-09-19, all three questions accepted as proposed). Two ideas of
2026-09-18 were both spelled "tier": arrivals split by tier (`ba429e7f`)
and hosting safety levels (`a479faf7`). Built alone, each would have
defined a tier for itself and the two would have drifted (CLAUDE.md
§9a). Decided: (1) **a file in the tree, not a registry row**,
`infra/platform/tiers.toml`, so the lint and the gate read it without an
instance; it becomes a row only if a tenant ever needs to redraw tiers.
(2) **Path prefixes with a rank**, because two of the three readers
classify the paths of a diff, not crates: core 1, infra 1, modules and
orchestrators 2, tenants (with `examples/`) and frontend (`apps/`,
`libs/web-kit/`) 3, and data 4 (`infra/platform/`,
`infra/dispatcher/rules/`, any `seeds/`, `docs/`). The most specific
prefix wins, a change's headline tier is its lowest rank, and a path no
row claims is in no tier rather than a guessed one. The Rust dependency
rule above is unchanged. (3) **Arrivals first, then hosting**: arrivals
is a measurement with no authority at stake. Built, all three readers:
`tier-import-audit.sh` reads its roots from the file through
`infra/lint/lib/tiers.sh`, a collapse rather than a pin; a car and a
train carry `software_tiers` and `software_tier`, and `boss channels
--tiers` prints the mix that protocol-retro's `collect` step quotes; and
a tenant's `[meta] edit_level` names a tier, enforced by one predicate
(`TierMap::first_above`) at both doors, `boss dispatch` before the claim
and the gate's `a-car-stays-under-the-edit-level` lint on the diff. An
instance that declares no level enforces nothing. The shell and Rust
readers are held equal by `tiers_sh.rs`, and `infra/platform/tiers.md`
beside the file says why each row is drawn where it is.

**Four languages: Rust for systems, Svelte for the frontend, Python for
scripting, containers for deployment — and a fence around the third**
(design `9b4d8ccd`, David 2026-09-20, all four questions accepted as
proposed). Measured that day: 234 shell files, 51,806 lines, against two
Python files — shell was already the scripting language, and `python3`
was already a declared forge requirement. Decided: (1) **new scripting
is Python where the work is structured data** — the lints first, where a
1,422-line bash lint is the argument and the startup objection measured
0.56 s across all 88; **never for the forge's shell twins**, which
retire by deletion as the `boss` CLI reaches the forge (§Consolidation)
rather than by translation; **rarely for glue** that drives systemd,
`talosctl` and docker, where an interpreter on the recovery path is a
cost when the system is unhealthy; (2) **the fence is four rules**: no
`.py` under `crates/`; **stdlib only**, the load-bearing one — no
`requirements.txt`, no venv, no `pip` in any image, because a script
that needs a package manager is a program; never imported, only
invoked; and never on the runtime execution path of a cluster service
— Python may do a job and exit, never become a component; (3) **no
migration**: existing shell stays until it is touched for another
reason; (4) **enforcement is a lint measured like any other** — the
directory and no-pip rules are near-free greps, the never-imported
rule needs a parse. No such lint exists yet, so today the fence is a
written rule and is unchecked, stated here rather than implied.

**A real instance carries only what its tenant declares plus what the
platform needs; example data reaches an instance only through its
tenant contract; a bounded verb evicts residue.** (Backlog `718ac982`;
design `e2580840` car 3, David 2026-09-17, folding `83a873e8`.)
01-registries.sql and 40-ledger.sql seeded the two worked examples'
reference rows on every instance — the used-device shop's 26 roles and
ten departments, the brewery's location kinds, account types,
equipment categories and two production sites, the brewery-shaped
starter chart (33 accounts) and a `companies` row each — and on
2026-09-17 the company's own instance answered "Brewery Taproom" from
`/api/locations`. Those rows are now the example tenants' seeds
(`examples/brewery/seeds/classes.json`, `locations.toml`, the new
`chart_of_accounts.toml`; and the device shop's classes seed until that
tenant was deleted on 2026-09-24),
published through the same contract every tenant uses. The migrations
are not edited — an applied file is history and `migrate.sh` refuses
a changed checksum — so the rows still land in every database the
converge creates, and they leave by two doors that read ONE
derivation, `infra/postgres/example-reference-rows.sh`, whose
candidate set is read from the example seeds and never typed — and,
for an example that has been retired while the migrations still seed
its rows, from `infra/postgres/retired-examples/<name>/` in the same
shape, a declared list held equal to the migration by a test (the
used-device shop's, backlog `a8991c86` car 6, 2026-09-24): on a
fresh instance's FIRST start `boss-init` evicts them before any
service starts when the declared tenant (`BOSS_TENANT_DIR`, the same
directory the launcher publishes) is not an example — an example
tenant keeps its own rows, an undeclared tenant keeps them too; and
on an instance that has already booted the forge verb
`retire-example-reference-rows <mode> <namespace>` evicts them with
the record on the ops-request packet, `--dry-run` the plan, `--for-real`
one transaction per table, refused for an image-sourced instance.
**Deletable only when unreferenced**: a row anything points at — an
employee wearing the role, a location wearing the kind, a journal line
on the account, a tax kind naming it, a job about the company — is kept
and named with the reason, so an instance that adopted an example code
keeps it. What the platform keeps is exactly what the platform names,
and a test derives that line rather than listing it: the roles
flagged `is_system_role` (`platform-admin`, `audit-readonly`), the
bootstrap admin's and the operator baseline's row (the `it`
department, `loc-hq`), the kinds the platform's three default
locations wear, the `unspecified` account type the column defaults
to, employment types and statuses, the module-tier vocabularies. (Until
backlog `7f163e58`, 2026-09-18, that line also carried the five accounts
the migration's tax kinds reference; see the tax decision below.)
The starter chart is therefore no longer an OSS default a company
adopts by collision: a fresh company instance has no chart until its
tenant declares one, and the rows below about a colliding code apply
only to an instance that ran the migration before the eviction.

**The instance is the truth; seeds bootstrap; the tenant repo is
bootstrap + export.** (Design `e187198f`, David 2026-09-18: "move away
from seeds mattering, except to help OSS users bootstrap and/or to
support playground; our actual BOSS instance should be flexible to use
data instead of seeds; going all the way back to a seed is its own
issue.") Measured that day against `40868c22`: `boss tenant publish`
runs at EVERY services-container start, and four of its doors
overwrote a live row on each run — business calendars wholesale
(`ON CONFLICT DO UPDATE` + a closed-day DELETE/reinsert), the company
label (`COALESCE(EXCLUDED.label, …)`), an employee's declared fields
(the 409→GET→PUT overlay of `09887242`) and an agent's whole row — so
an operator's edit to any of them lived until the next boot, while four
others (classes, sensors, policy, workflows) kept the live row and said
NOTHING when the file differed, so a repo edit that never landed was
dead text. Decided, all as proposed: every door is **insert-if-absent
by default**, each batch route carrying `?mode=insert-if-absent|take`
where its semantics live; **`boss tenant publish --take
<registry>[,…]`** is the only overwrite, sent per named registry
(`classes`, `calendars`, `company`, `policy`, `employees`, `agents`,
`workflows`) and printing every overwritten row field by field; the
contract's "declaration wins on declared fields" rule (`09887242`,
2026-09-17) is **dropped** — it covered a repo-edited location that had
not landed, the bootstrap case, and on a running instance the same
overlay was the collision; **every registry's publish line names its
kept-but-differing rows** in one shape, `kept: <id> differs on
<fields> (the instance is the truth; --take <registry> overwrites)`,
as the decision surface; the publish runs **once per database** (a
stamp, like `init.sh`'s first-start gates) and thereafter only by
`--take`; and **`boss tenant export`** writes the live registries back
into the contract's file shape by machine on a cadence, so the repo is
never hand-edited on a live instance. The playground publishes the
brewery every boot and, with insert-if-absent as the default, that
publish changes nothing on a running instance — which is the point.
Car 1 (the doors, the flag, the lines, this paragraph, the contract's
section — printed by `boss tenant contract` and pinned) landed first;
the stamp and the export verb follow. The rule is stated for tenants in
`docs/tenant-contract.md` ("The instance is the truth; `--take`
overwrites by decision").

**A merged tenant main is not a published one; publication is its own
recorded act.** Design-doc *A tenant delivery proves publication after
the merge* (`26f6bd62`, David 2026-10-03, its one question accepted as
proposed) was filed when a Marketing protocol merged onto the tenant's
main and stayed absent from the live registry: once the stamp above is
set, a services start publishes nothing, so a merge proves source only.
Decided: a bounded, passkey-approved publisher renders every declared
row as absent, equal or kept, inserts only the absent ones, refuses a
dark or drifted read, and reads each effect back; the item closes on
that live read-back, never on the merge or a restart. Built: the verb
pair `plan-a-tenant-publish` / `publish-tenant` (backlog `1610115d`, car
`0c562093`, 2026-10-21), and an insert-only workflow publication port
(car `cd077f2b`, 2026-10-03). Superseded: Q1's one approval binding the
main push and its manifest together. *A workflow publish that needs a
person is a packet on that person* (`b1f319bc`, David 2026-11-05) gives
each tenant merge that lands a workflow or rule row its own
`publish-a-protocol` packet and approval; that filing trigger is not
built (item `accbe126`), and a publication executing as an Operations
child is item `dcbeef5a`. Not built: the merge plan still says the
tenant seed publishes "at every services-container start"
(`infra/forge/merge-tenant-main.sh` and the `merge-tenant-main` verb's
`about`), which item `69c8aee1` owns. The Marketing row
(`measure-the-page`) went live on 2026-10-11 through the tenant seed,
not a recorded publish, and item `9f69a057` still waits on its
prove-delivery read-back.

**A module is on only when its tenant lists it `true`, and an instance
says how many at boot** (design `1054c099`, David 2026-09-22). Measured:
the company's own gateway answered `"modules":{}`, and under the contract
the brewery's `tenant.toml` states (a missing key is off, the same as
`false`) all ten module-gated surfaces had been off since the
declaration existed. It went unseen while the nav catalog and the route
switch disagreed about which module gated which route, and surfaced when
car `f9b43965` made them one fact: a missing declaration is not an
error, it is a confident empty answer. The declaration lives in the
tenant repo that `infra/cluster/instances.toml` names, so which modules
the company runs is a business statement, not a car. Decided: Algedonic
runs `finance`, `exec` and `support` (support "as that is our email
inbound for now"); `calendar` only once dated work is modelled as Jobs
with a `release_date`, since an empty surface is worse than a hidden
one; the six physical-operations modules are WRITTEN `false` with the
reason rather than left missing; prod's absent `edit_level` is
confirmed intended. The platform half: **an instance states its module
count at boot** — a tenant running only jobs, people and messages is
legitimate, so a refusal would be wrong, but silence let ten surfaces go
missing. Landed: the services launcher and the gateway each print one
line (`modules on: <n> of <m> declared`, naming both sides), and the live
manifest reads exec, finance and support on and every other declared
module off.

**An off module's page shows a notice at its URL, unless its catalog
entry says absent** (design "Vendors removal: preserve or gate the
coupled Finance detail routes", `45fad3bb`, David 2026-10-11, answered
as proposed). A direct visit to a page whose module is off renders the
module-off notice. A catalog entry declaring `whenDisabled: 'absent'`
renders not-found instead, and only once the manifest has been read; an
unread manifest decides nothing (`withManifest` in
`apps/web/src/shell/sections.ts`). Vendors is the one entry that
declares it, gated on `warehouse`, so the company's instance neither
links nor serves /finance/vendors while the brewery keeps it (backlog
`6d6f88bd`, landed 2026-10-11 in train #1096). The question was the
money-adjacent scope: purchase-order and vendor-invoice detail shared
the Vendors entry, so the gate would have removed them too. They do
not follow the register. Each is its own unlisted Finance entry with no
module, so both stay reachable and add no sidebar row.

### Tenant delivery and export

**Tenant work travels through the same record on its own delivery route.**
Design `fd8b5143` (David, 2026-09-24) gives tenant work a
`tenant-builder` in a worktree of the tenant repository. Its handback
copies the complete passing `boss tenant check` receipt; it ends
`delivered`, not `gated`. Landing goes through the tenant merge plan,
independent review and David's passkey approval, then a read-back of
tenant main. Publication and a probe of the live registry are separate
obligations: the services-start publication described in the original
plan was superseded by the publication decision above. Neither a source
merge nor a copied check proves that the live registry changed.
Q1 chose a second route on the same map: build, tenant check, awaiting
David's approval, tenant main, then the shared proof station. Approval
waiting must look different from building and troubled past its band.
That is a rendering requirement, not a claim that the old map implements
it; station-tree views carry the requirement forward.

**An export is a reviewed reconciliation, never an automatic overwrite
of tenant main.** Design `77e36e73` (David, 2026-11-14, Q1–Q3) refines
the export decision above. Registries with ownership keep their owner
filters. Unowned registries omit unchanged platform-baseline rows,
deriving the baseline from the migration and seed sources rather than a
typed list. A baseline row already declared by the tenant, or changed
from its seeded value, is a tenant edit and is exported. Example residue
is evicted through its own recorded door; export never hides residue
that the instance still holds.

The comparison is three-way against the tenant main sha recorded on the
last publish stamp and `tenant.published` event. Repo-only changes stay
in the repo and are listed as awaiting a publish take; instance-only
changes are exported. When both sides changed a row, the repo row stays,
the plan names the conflict field by field and an item is filed. Without
a base, the first export presents every difference as ambiguous for
review; it is not an unattended cadence run. Publish and export share
one row-identity normaliser.

The decided `export-tenant` verb reads the instance's declared repo and
ref, writes only an `export/` branch and uses the existing tenant
credential. It runs after publish read-back and daily; an empty diff
pushes and files nothing. Main still requires an independent
tenant-reviewer releasing the exact merge-plan bytes and David's passkey
over that plan. Accepting an export ops-request's branch effect as the
author widens provenance, not authority: the automaton cannot also be
its reviewer. The order is publish-sha provenance, comparison/filter,
export and review-source support, then cadence. Folding this decision
does not certify those implementation or publication effects.

### Instance declarations and external services

**The OSS holds the recipe and a neutral default; an instance declares
its own values.** Design `c6f08b60` (David, 2026-10-01, Q1–Q2) keeps
manifests, converge, identity-provider installation and Talos patch
shapes public and parameterised. Algedonic's addresses, domains, node
list and DNS declarations belong in its private company repository;
the OSS ships an example estate with reserved test values. Q2 did not
approve deferring that work until after 1.0.0: the release scope and
date were undecided, and the instruction was to proceed.

Fixtures use reserved addresses and names through one helper per
language; a pin on a real deployment reads its declaring file instead
of copying a literal. Guest identity is install-declared with neutral
default `guest`; the playground may retain its declared identity
without rewriting audit history. The guest-availability response
supplies UI identity. Refusal prose points at `BOSS_JOBS_URL` and its
declaration, and an install without a dev door gets an explicit absent
state. Applied migrations remain immutable. These are the accepted
boundaries, not a statement that the audit's whole sweep has landed.

**GitHub credentials derive from one App root and per-installation
registry data.** Design `76155676` (David, 2026-09-27, no open
questions) places the App key and ids once as root material. The
broker mints one-hour installation tokens into named consumer slots;
no new personal access token is the substitute. Installation ids,
repositories and permissions are rows: the company's organisation is
one installation, and a customer's organisation can be another.
The credentials page renders issuer, scopes, location, consumers and
rotation history, never secret values, and offers declare, rotate and
retire as packets. Repository creation, branch protection and named-ref
deletion are bounded plan/passkey verbs whose effects are read back.
The private organisation-owned disaster-recovery repository is created
through that door, not a fork or a hand-created shortcut. David
accepted the off-site-copy gap while that exercise was pending rather
than minting a stopgap token. The decision records that historical
trade-off, not the present backup state.

### Algedonic's public site and Marketing

**A public company map is a dated static snapshot, never an operational
feed.** Design `e21aba42` (David, 2026-10-22, Q1–Q4) permits region
names, a data-derived connected flag and the snapshot date only: no
packet counts, station names, health or actors. A pure renderer emits
a self-contained HTML/SVG fragment for the tenant's site tree, with no
script, API access or new gateway route. Publication waits for both
the first-protocol milestone and the public site's release from
Access, not a date. The old department-map component is historical;
this decision does not revive it in the station-tree reconstruction or
assert that its snapshot has been published.

**Marketing is on when its weekly, measurement and publish loops run.**
Design `ff0f4b9a` (David, 2026-11-13, Q1) keeps the brewery asset
catalog off for Algedonic. Its current assets are site files with
identity, versions and approval already in git and publish packets;
a second catalog would duplicate that record and enable unevented
writes. The weekly visits, arrivals and digest steps gain dispatchable
agent blocks and station audiences through tenant protocol publication;
existing weeks move by recorded conversion and run oldest first.
Missing retained readings are recorded as missing, never invented or
passed as evidence. David reads each digest; a read starts the page
measurement, whose proposed change is tied to a figure. Words and look
remain his approval on the existing publisher. No new workspace is
designed before a digest and measurement have run. A non-site asset
can reopen catalog activation as its own item only after its create
and retire writes emit facts. This fold records the operating decision,
not completion of those loops.

### The public tree and its publication

**One forge tree produces its public tree through one deterministic
filter.** Design `f7087b8c` (David, 2026-11-16, Q1) puts the internal
operating manual under the single top-level `internal/` directory.
`public_tree(source_sha)` drops that root entry without changing the
index or worktree. Snapshot creation, merge equality, drift, changed
paths and foreign-commit classification all use that same definition;
an internal-only source change creates no public drift. Measurements
state `internal_excluded`, including zero. The passkey still signs the
source sha, and secret scanning still covers the full source tree.

Public `AGENTS.md` remains a relative symlink to public `CLAUDE.md`.
Claude imports the worktree's operating manual; managed Codex and
Gemini global instruction symlinks read the main checkout's copy.
That freshness difference is accepted and the combined instruction
size remains pinned. Public preflight instructions and anything read
by a public test stay public, or their pin moves with the text into
`internal/lint/`. The full gate on a public snapshot explicitly
declares `BOSS_PUBLIC_TREE=1` and reports zero internal pins; an
unexpectedly absent internal tree on the forge refuses.

The canary comes before operating content: the next publish must
exclude the canary, preserve the expected tree identity and pass the
mirror's full gate. Only its recorded proof permits the manual and
estate material to move, with instruction-loading and no-rule-lost
proofs. This fold authorises no such move. Q1 leaves existing public
history intact: a communication boundary cannot recall clones or
indexes. A real exposed secret requires broker rotation, not a history
rewrite. Incident-specific operating material moves verbatim; public
stub headings preserve existing citations.

**An unsigned stale publish packet yields to the newest tree, and
approval follows a mirror-equivalent gate.** Design `62f27539` (David,
2026-11-16, Q1 accepted) supersedes an unapproved packet when the daily
reading names a newer source sha, recording both shas and the successor
edge. A signed packet runs to its own terminal. Measure and review have
duration bounds. Failing-check annotations retain check, path, line,
level and message, and the judge begins with the named failure. The
author's correction matters: Gate check names were already recorded;
the gaps were other checks' annotations and the judge not reading the
names, not universal absence of diagnostics.

Before approval, the exact filtered public tree is gated with the
mirror's base, public-tree declaration and applicable image. David
accepted one additional daily gate on the shared bays; a red files its
findings and waits for a newer tree. The order is diagnostics, unsigned
supersession, then the mirror gate after image-parity findings are
resolved. A draft workflow is not silently promoted: the design names
a pre-existing draft lineage hazard that publication must reconcile.
These rules do not bypass approval or claim that a forge green alone
proves the public snapshot.

**Publishing to GitHub has targets for 1.0.0, and the forge's history
carries its own changelog** (design-doc "Publishing to GitHub is smooth
by 1.0.0", `cb38d806`, David 2026-09-19). The targets: a publish PR on
every day there is drift, sized like a day of trains; zero unjudged
reds, every GitHub check result read back onto the publish packet with a
disposition before a merge; at most one human act per publish; a publish
region that shows the PR and looks troubled when it stalls; and a 1.0.0
tag that clones and installs. (Q2) The public repository moves from
dated snapshots to real history at the 1.0.0 cut, not before. Measured
that hour, the forge's history was not cluttered but mute: 610 linear
train commits, every body empty, the car titles living only on the train
packet and the forge PR. So the train's squash commit carries its
consist, one line per car with its branch, title and packet ids
(`squash_message` in `boss-cli/src/train/verdicts.rs`, backlog
`f252cb1c`), and `git log --grep <id>` finds a change. (Q1)
**Superseded:** the design kept the GitHub merge as David's hand act
until publish PRs had landed green without questions for a while. On
2026-09-30 (backlog `602fe95f`) he chose instead that nobody pushes or
merges main by hand, after a UI squash stamped a personal address on the
public main and every merge was found to be an admin bypass of a stale
branch rule. The `merge` step of `publish-to-github` is a machine step:
once every check main's ruleset requires is green, the App fast-forwards
main to exactly the tree his passkey approved, and the ruleset admits no
other writer. The one human act is that approval.

## Deployment, the forge, and the cluster

### A gate judges its tree; registry drift is estate work

The live protocol and dispatcher-rule comparisons run after convergence,
when the deployed image, authored tree and registry should agree. Their
live reads are excluded from the consist gate; their tree checks and
reader self-tests remain testable. A car cannot be failed because another
train changed a live registry while its frozen tree was being tested.
Decision history:
[d349e0ba](https://boss.algedonic.dev/ux/jobs/d349e0ba-dd36-4b45-82d3-fe174b32043b).

The converge records discrepancies and files deduplicated work naming
the affected protocol or rule and the evidence for which side is ahead.
A disagreement does not fail the deployment or silently overwrite a live
edit. The current `registry-drift.sh` files work and does not publish.
The approved successor may publish only a lineage-proven tree-ahead edit,
after the lineage guard required by `9235802a` exists, under a named actor
with the declared publication scope. A live-ahead row, unmatched lineage
or retirement remains filed work. That conditional approval is not proof
that automatic publication or the separate cadence observer has shipped.
Its direction changed on 2026-11-08 (design-doc *Five further collapses
of duplicated factory facts and packet handoffs*, `942dc0ac`, Q2): after
bootstrap the live row is the authority and a tree edit is a proposal,
so an automatic tree-ahead publish is no longer the target; see "Live
registry content is authoritative after bootstrap" under the network
substrate.

**A test never writes a Duration.** Design `e975816d` (2026-10-31, no
questions): `boss_testing::deadline(kind)` returns the bound for a named
kind of wait (`startup`, `io`, `reap`, `sql`, `witness`) as a base times a
load factor read once (load average over CPU quota, or
`BOSS_TEST_LOAD_FACTOR` set by the gate from its measured stall), and
`wait_until` fails naming the kind, base, factor and elapsed time.
`no-wallclock.sh` refuses `Duration::from_*`, `sleep` and numeric timeouts
in test code outside the helper. The gate runs suites under a per-test
timeout from the same factor, so a hung test fails by name inside the
bound instead of holding the gate. Fixtures start ready and say what they
replayed; a refusal under load is a failure, never a pass; a reaped child
is awaited on its exit; a witness waits on its writer's own signal.

### Type checks cover dependent fixtures

Test fixtures for `Job`, `Step` and `StepField` use constructor bases
instead of copying every field. Production literals stay exhaustive
where a new field needs an explicit semantic choice. `StepField::new`
requires the name and field type while supplying the existing defaults;
an empty-name `Default` would conceal a meaningless field. The
`test-fixtures-use-constructor-bases` lint refuses exhaustive literals
under crate `tests/` directories without a baseline allowance. In-source
test modules remain outside that textual lint's reliable scope.

Both the car gate and `--lint` preflight run workspace clippy with
`--all-features --all-targets`, including normal binaries and dependent
test targets. Build and test can retain their scoped gate selection;
there is no second workspace check duplicating clippy. A warm leaf edit
can reuse cached dependents; a cold cache or core type change pays the
actual dependent compilation cost. A preflight pass is still not a gate
or runtime proof. A changed parked head needs a new qualified receipt.
These are the engineering decisions recorded in
[caa2acc9](https://boss.algedonic.dev/ux/jobs/caa2acc9-4d2f-4224-9d2a-d96d281c3f30),
not a claim that every in-source literal has been migrated.

**Deployment is modeled on how networks patch** (decided 2026-08-12,
folded from `deployment-as-network.md`, deleted by fold batch F3 on
2026-11-17; its five questions were decided as proposed: Q1 a
generation store of three `releases/<sha>/` behind `current` and
`previous` symlinks, Q2 a confirm read at +2 and +8 minutes, Q3 an
auto-revert of binaries, web dist and step plugins only, Q4 the
evaluator a separate unit armed at the flip, Q5 scratch as wave 1.
Those mechanisms were built for the one bare-metal box and deleted with
it on 2026-09-18, so of that document only the split below and the
lessons the next three paragraphs keep are current),
which means distinguishing what kind of thing is changing. **Traffic** —
requests in flight, Jobs mid-step, the log — is never rolled back; a
delivered response is history. **Derived state** — binaries,
projections, the served SPA — is *reconverged*, not restored:
rebuilt from intent, freely replaceable, no snapshot nostalgia.
**Intent** — the repo at a commit plus the registries and config —
is the only versioned layer, and "rollback" exists there only as
rolling *forward* to a prior intent. Two prerequisites were already
policy, which is why the rest is plumbing: expand/contract
migrations are exactly the N-1 compatibility that lets a reverted
binary run on today's schema, and the SPA's content-hashed dist is
make-before-break natively.

**The converge reconverges the cluster on main, and a rollback names
its target.** The bare-metal deploy — release generations under
`releases/<sha>/` behind `current` and `previous` symlinks, a flip
confirmed at +2 and +8 minutes and auto-reverted at +10 by its own
`boss-deploy-confirm` unit, and scratch as the first wave — was
deleted by train #443 on 2026-09-18 (H7 under §Consolidation toward
1.0.0). What deploys now is `infra/forge/cluster-deploy-runner.sh` on
the forge host. For a new main head it builds the all-in-one image
(`infra/oss-quickstart/Dockerfile`, which carries the SPA), pushes it
to the forge registry, and proves it boots (`image_boots` in
`cluster-deploy-lib.sh`) before anything is applied, so a head that
fails there is quarantined with no dark window at all. It then applies
the tree's manifests and rolls `deploy/boss` with one patch, so every
revision in the history is a coherent template. A roll that does not
go Ready is rolled back to the **last converged build, by name**
(`roll_deployment`), never to "the previous revision", which on
2026-09-05 was the placeholder the apply had just created and could
not boot; the failed head stays quarantined until it is cleared, and
what the converge does after repeated init failures of one build is
design `b2e778ec`, below. The train hears the roll on the converge's own
packet (`report_rolled`, dispatcher rule
`complete-train-converged-on-runner-rolled`). The roll is not
make-before-break: `deploy/boss` runs `strategy: Recreate` because two
RWO claims cannot attach to two pods (`infra/cluster/manifests/boss.yaml`),
so a roll has a dark window that the readiness probe shortens and does
not remove.

**The dead-man is a watchdog that owes nothing to what it watches.**
The commit-confirmed flip's lesson outlived it: an evaluator that dies
with the process it guards reverts nothing. `infra/forge/cluster-watchdog.sh`
runs every five minutes from its own timer on the forge, opens no
packet through the API it watches, and reads `/api/jobs/health` from
outside the cluster; once the API has been dark for three consecutive
checks it rolls `deploy/boss` to the converge's stamped last-converged
build by name, and when it cannot read the cluster it says it is blind
and files one alert rather than closing ok. `rollback-to <sha>` is the
operator's verb for the same act, and it reads the image back before
it claims the roll. **Schema, registries, the log and the data are
never rolled back** — roll-forward only — so events written by a build
that is later rolled away stay in the log, and projections and
rebuilders must tolerate event kinds the running build does not know:
closure doing revert-safety work.

**There is no canary wave.** Scratch was wave 1 on the one box and went
with it. The converge rolls prod first and applies the other instances
(the Ales playground) the same way only after prod is stamped, so they
follow prod rather than shield it. Per-node waves and a true
drain-patch-undrain are not built.

**Git and CI come inside, on Forgejo.** Internalizing is what turns
the merge wall into policy — the ship-a-change `review` step gains a
required operator sign-off, and the conductor, on seeing a
signed-off review, calls the forge adapter's merge verb and stamps
the merged marker exactly as before: the observe-then-mark shape
survives, the observer becomes the executor. CI is **Forgejo
Actions**, chosen de facto once GitHub-Actions compatibility held
with two container-job deltas, with the CI image's Dockerfile in the
repo and the gate script pinned on both sides so a second gate
definition cannot drift into the workflow file. The GitHub mirror
becomes a **push-mirror on every main update** — superseding the
earlier daily cadence, because a disaster-recovery copy of the
system of record that is a day stale is a day of lost commits, and
the GitHub-native checks only audit what the mirror shows them.
Inbound stays deliberate-pull through GitHub PRs, keeping external
code off internal runners. **Forge events land on the outbox** via a
small ingress that validates the webhook secret and stages with
`record_event_in_tx` — never post-commit publish — with
`forge.push`, `forge.check.completed` and `forge.merge` born
declared in the event-kind registry. (Folded from `internal-forge.md`,
its seven questions decided 2026-08-12 and the file deleted by fold
batch F3 on 2026-11-17. The two this paragraph does not carry: Q1
placed Forgejo on the interim LAN host, its data directory in the
backup set, with cluster placement left to the topology work; it is
still the forge host today. Q7 sequenced the cutover: CI shadowed
GitHub's first, the forge's configuration came into the repo, the
Forgejo adapter landed behind the conductor's forge seam, and the
review cutover waited on a rehearsal. The mirror above is history:
since design `1f35a3e8` forge main goes to no public repository, only
to the private disaster-recovery copy, under "The off-site copy of
main is private" below.)

**The forge is also the cluster's image source, so its downtime is the
cluster's restart window** (design `ccce1191`, 2026-09-20). Thirteen
manifests pull from the forge's registry, the system of record's
postgres among them — pulled from the forge rather than docker.io
because a database that cannot re-pull its image cannot restart. While
the forge is off, running pods keep running and anything that must
restart waits in `ImagePullBackOff`. So a forge window is quiesced (no
trains, gates, converge or deploy) and never overlaps a cluster node
being down; with etcd quorum at two of three and Longhorn at three
replicas, nodes go one at a time. Where postgres sits during a
control-plane window was not reached, and waits for a window that wants
one. The
window it was written for took the forge alone (David: "Let's do
forge only then"), and the extra disk was left to a judgement at the
box, because the measurement that night was headroom, not a repair.

**Maintenance stops being invisible work.** The department's
recurring labor — backup, audit integrity, ledger replay checks,
views catchup, GC, purges — ran as systemd timers outside the Job
model, in a system whose thesis is that work is visible. Each chore
is now a **maintenance Workflow kind**: success completes the run
step, failure completes nothing and leaves the Job open and loud,
and recovery closes the standing Job, so a failed backup is an
algedonic signal instead of a quiet journal line. The spawner is
deliberately the **maintenance wrap on wall-clock time**
(`infra/boss-maintenance-wrap.sh`), not the dispatcher's schedule
runner — an amendment to the original proposal, ratified: sim-day
rules fire every couple of wall-minutes at warp, and maintenance is
wall-clock work. The wrap was a systemd timer's when this was decided;
the chores that moved into the cluster run the same wrap from a
CronJob (`infra/boss-chore.sh`). The cadence loop
retired the *train's* timers, not systemd itself; the remaining
timers are a rollout list, not a claim.

**The cluster reaches the hub over bare WireGuard.** The GCP box is
the hub — stable public IP, overlay `<overlay>/24` — and cluster
nodes are spokes that dial *out*, so no inbound hole is opened in
the home router and a keepalive holds the NAT mapping; node-to-node
traffic inside the cluster stays on its own mesh. Kanidm and the
log-copy migration both ride that wire. The cluster is a *client* of
identity and a consumer of intent, never the host of either: moving
the company is copying its log and its rules, and everything else
regenerates.

**Compute is one fabric at the BOSS layer, not at the Kubernetes
layer** (folded from `compute-fabric.md`, all five questions decided by
David in review `a96c4027`, 2026-08-18; worked up from `a59de54d` and
`6796ee5f`). David: "I think our compute should all be part of the same
fabric." Joining boss-gcp or the forge host to the Talos cluster as
kubeadm workers was rejected: it defines membership by process
supervisor and buys a permanent snowflake over a WAN link. Decided: (1)
**a machine is in the fabric when it is a Subject, a station can route
it work only it can do, and its operations are protocols that leave
events**; its supervisor — kubelet, systemd, launchd — is the machine's
own affair, replaceable without touching the fabric; (2) **machines are
seeded as data** under the `node` Subject kind with capabilities as
Class rows, and a per-node sweep checks each declaration against the
machine; (3) once cadence is protocol data the conductor's location
stops mattering, so moving it is a one-line choice later — it moved
into the cluster on 2026-09-04; (4) overriding the doc's proposal to
keep the demo instance separate, **no multi-tenancy**: "playground.
algedonic.dev is where we both run and develop BOSS together. Guest
access is our mechanism to prevent unauthorized use... The operator and
enterprise will be the same." The older second stack on boss-gcp was
retired on 2026-09-15 (ops-request `7912c9ae`); the instances since
are the company's own and the Ales playground ("The instance is the
truth", §OSS posture & tier boundaries); (5) **every IT protocol that ships
names the memory entries it supersedes**, the count of
superseded-but-still-cited entries being the measure — carried further
by §Agent memory and the record. The estate registry below is (2)
built.

**The estate is drawn from Subjects, and a view of it separates what
the system did from what a target is now** (folded from
`infrastructure-view.md`, all four questions decided in packet
`3ddf33ed`, David 2026-08-16). Measured: 204 distinct subjects on open
infrastructure packets, nearly all repo paths, so a page keyed on them
would have drawn the codebase rather than the estate — and one commit
reached two targets by two deploy paths while the train said only
"deployed". Decided: (1) **the `node` and `service-instance` Subject
kinds are created**, with this page as the forcing function — a
`service-instance` carrying its service, node and environment, its
port, its database, and whether it is **authoritative** for that data;
(2) **two views, rollout first** — the rollout (car, train, CI, merge,
the deploy paths, the targets) needed no new kinds, and the estate view
lands on real Subjects rather than a hand-drawn diagram; (3) **packets
for history, a thin per-target probe for current state**, and a target
that has not reported reads *unknown* rather than an inferred value — a
stale number presented as live is worse than a blank; (4) **the estate
is its own page, cross-linked to the station map**, because a station
and a node are different objects. Built: both kinds
(`144-estate-subjects.sql`, which cites this decision beside
`bossnet-physical-topology` Q1 and `dev-node-checkout` Q1), the estate
registry and its observations under `/api/estate/*`, and `/it/estate`.

**PVC intent is an explicit report-only assignment, independent of
bootstrap, live request and filesystem capacity** (design
`32e7cf87-7d8e-4764-94a3-3b0cfa8548c4`, Q1 resolved by David,
2026-10-02; answers child `10effd3f` of `53c8cb72`). Namespace and
claim identify each declaration in `infra/estate/estate.toml`;
desired bytes carry the assignment's packet, review step, question,
actor and time. Q1 explicitly assigned 30 GiB to
`boss/pgdata-postgres-0` for reporting; since 2026-10-07 the
declaration reads 40 GiB and its assignment is the `approve` step of
ops-request `b018a106`, the signed plan under which
`expand-instance-volume` grew the claim. Other claims without an
assignment remain unknown. The pure comparison retains declaration,
live request, provenance and match, drift or unknown in the existing
comparison event; the estate page renders that record. Missing,
malformed or duplicate input cannot match. Filesystem overhead is not
request drift; free-space floors remain independent. Neither this
declaration nor its verdict changes storage, bootstrap templates,
authority or the signed explicit-size approval boundary. This child
does not complete the parent's remaining automatic remedy work.

**A claim that cannot grow is proposed exactly one decisive replica
move, or refused** (design-doc `ada8f698`, "A tight claim proposes one
evidenced replica move"; its review answer chose neither option, and
David chose option A in chat on 2026-10-07; part of backlog `53c8cb72`).
When `plan-the-largest-instance-volume-expansion` finds no whole-GiB
growth that fits, `expand-instance-volume.sh --plan-largest` asks
`move-volume-replica.sh --plan-decisive` whether exactly one replica
move would free the volume. A replica is eligible only when the
existing move plan admits it and every remaining replica disk admits
the projected growth on the same ledger and live arithmetic. The one
eligible replica is proposed as `plan-a-volume-replica-move` with that
plan and its hash. Zero or several eligible replicas, a bound that is
only the ceiling, no short disk, or more replicas than the retirement
floor each refuse and name every candidate. Nothing ranks, and listing
order never chooses. The dispatcher files only that read-only plan
request, from the `no_growth_plan_verb` declared on the discovery's
`discovery_remedies` entry. A person files `move-volume-replica` and
David's passkey signs its plan. Option B, ranked partial-progress moves
across several short disks, was not chosen, so a claim with two short
disks stays a person's plan. Built: train #978 (2026-10-07) carried the
read, the selector and the dispatcher fallback, pinned by
`a_tight_claim_proposes_one_decisive_replica_move.rs`, and `53c8cb72`
closed that day. The verb contract is in `infra/ops/verbs/README.md`,
"When no growth fits".

**Retiring a unit will mean masking it, because disabling is not
retiring** (design `7b230ddf`, David 2026-09-23, accepted as proposed;
answers backlog `9599babc`). Measured through `boss ops boss-gcp
journal-tail` (ops-request `5acce6ec`): `boss-ml-api`, stopped and
disabled by `retire-second-stack` on 2026-09-15, had been running on
boss-gcp since 2026-09-20 as one process — the kept
`boss-ml-inference-batch.service` declared `Requires=boss-ml-api.service`,
and systemd starts a required unit whether or not it is enabled, so the
02:30 batch revived it and it served the batch nightly against the
retired stack's database while no prediction reached the system of
record. The class: any kept unit that `Requires=` or `Wants=` a retired
one brings it back. Landed (#586, `9599babc`): the batch reaches the
cluster's ML API through the machine door, its `Requires=` is gone, and
its packet carries the prediction count, a zero night failing loudly.
Decided, not built: after that car converges, `retire-second-stack` is
re-run for `boss-ml-api`, and the verb changes to **mask** what it
retires (`systemctl mask`) and to refuse to retire a unit a kept unit
still `Requires` — still a bounded verb, still David's to file.

**Development runs in a durable session inside the cluster; the laptop
is a thin terminal** (folded from `durable-session.md`, all five
questions decided in packet `9abd4ad5`, David 2026-08-16: "My laptop is
really meant to just be a thin terminal to interact with a durable
session somewhere in the BossInfra layer"). `boss-dev.yaml` had already
settled the shape — cluster hardware, **the CI image itself** so that
"works on the dev box" and "passes the gate" cannot drift, and a
Postgres sidecar at `127.0.0.1:5432` so a test cannot reach production
by construction — but nothing had applied it, and four environments
each claimed to be the target. Decided: (1) **measure the nodes' disk
before applying, and pin the pod** — a cold `cargo test --all-features`
wanted ~74 GB of target; (2) **the access credential is scoped to the
dev namespace**, since one that also reaches `boss` recreates the blast
radius the sidecar removed; (3) **the durable unit is a multiplexed
session inside the pod**, tmux, which a terminal attaches to and
detaches from — a pod that outlives the laptop is not enough if the
shell does not; (4) **the session holds a repo-scoped, push-only forge
token as a Secret**, and never the GitHub mirror credential; (5) **a
check fails when a manifest in the tree is not an object in the
cluster** — the tree is not the system. Where it stands: the pod
became the working session on 2026-08-21 (gates have since moved to
runner Jobs of their own), and an interactive login execs into its
tmux session; the forge token reaches
it through the credential broker (CLAUDE.md §Doors); the way in became
the Access SSH door below rather than a kubeconfig on boss-gcp; and the
forge's converge applies every manifest, with lints refusing a manifest
the converge would ignore (`a-manifest-the-converge-ignores-is-refused.sh`)
and reporting an object whose manifest was deleted
(`a-deleted-manifest-leaves-no-object.sh`).

**A workspace declares what it guarantees; that is the half that has
shipped.** Allocation was decided first (`2d43cbcb`, 2026-08-16): a dev
node is a **`service-instance` Subject**, the pool a StatefulSet whose
ordinals give stable identity and a per-replica PVC, **the checkout Job
the lease** — its terminal releases it — with a read-only credential
and a sweep for leaks; and "from the website" hands over a lease and
the facts to attach (pod, namespace, tmux session), not a terminal,
which stays the developer's own (David's override of Q3). Those five
answers were folded from `dev-node-checkout.md`, deleted by fold batch
F3 on 2026-11-17. Review `775f0b35` (2026-08-29) then settled
what a workspace *guarantees*, separately from who gets one: the
capability declaration belongs on the Subject, not on the checkout
packet (Postgres is there whether or not anyone has leased it; on the
lease it would be copied onto every checkout and drift per-lease);
**evidence before enforcement** — record which workspace the work
happened in before refusing work from one that lacked a capability,
because nothing yet says what a change *needs* and inferring it from
touched paths would refuse correct work; and **a laptop is a workspace
like any other, with its capabilities declared false** — a car that
passed 285 local tests failed the gate on a roster test that cannot run
without Postgres, 118 database-backed targets unrun on the workstation.
What the code does today is the last of these: the pre-flight
**opens** by reporting what the workspace cannot cover
(`workspace-declares-what-it-runs.sh`, pinned first in `gate.sh`),
which is that guarantee made legible at the point of work. The pool,
the lease, the `service-instance` kind and the `build` step's record
of its workspace are decided and **not built**: the dev session is one
Deployment on the build node, allocated by hand.

**The dev door is an Access SSH application** (design `5fc71f03`, David
2026-09-18, all three as proposed; backlog `e4cedb46`). That one
Deployment answered only on a MetalLB VIP on the LAN, so from anywhere
else the way in was a jump through the boss-gcp WireGuard bastion —
which `/it/estate` had to spell out in three forms, because `ssh://`
cannot carry a `ProxyJump` — authorised by one long-lived ed25519 key
an operator had loaded into a Secret by hand. It is now
`dev.algedonic.dev`: a CNAME to the tunnel, a route on the in-cluster
connector to the pod's own ssh Service, and a Cloudflare Access
application in front of it, all four declared in the same files every
other public name uses. What that buys is not convenience but the
credential: **Access issues a certificate for the session**, which the
pod's sshd accepts through `TrustedUserCAKeys`, so nothing long-lived
sits on either side of the door.

Two things fell out of the measurement. The pod ran **Dropbear rather
than OpenSSH for one capability** — sshd's preauth privsep child
chroots, Dropbear does not, and in 2026-08-30 the cheaper door won.
Dropbear cannot verify a certificate against a CA at all, so the swap
back costs exactly `SYS_CHROOT` and the history stays in the manifest
beside the capability list, which is the only place a reader would ask.
And **the Access declaration needed no new vocabulary**: `type` is a
value the handler carries from the declaration to the comparison to the
create body, never matched against a list of known kinds, so `type =
"ssh"` is declared, compared and applied by the same four fields
`self_hosted` uses — pinned now, so the next reach for an enum goes
red. The short-lived-certificate CA is not a field: Cloudflare generates
one per application, and reading its public key into the Secret the
manifest names is a root ceremony, done once.

**SSH stays the base connection; Mosh is optional, and only on a route
that already exists** (design `7ddd18ea`, David 2026-10-13, both
questions as proposed). The question was whether Mosh would stop the
dev door dropping. It cannot on the door as built: the Cloudflare
`ssh://` route streams SSH over WebSockets and publishes no UDP
service, so installing Mosh does nothing for the public name. tmux
already keeps the work alive across a drop; what Mosh adds is that the
terminal reconnects to it without a new login, and nothing else. So (1)
**a Mosh trial runs only on a UDP route David's client already has**,
and no VPN, relay or public UDP exposure is added for it. Without such
a route the work is repairing and measuring SSH on today's path, and a
private route is a separate design. (2) **Mosh is an optional command
first.** It becomes the preferred interactive connection only on a
proven route, after a day of twenty planned client-side interruptions
all recover to the same live tmux workload, and only on David's
acceptance. A disconnected session may live eight hours. SSH stays for
file transfer, automation and fallback either way. Neither transport
survives a pod replacement, so the comparison records pod restarts
beside every drop. Where it stands: **no Mosh is built and no trial
has run**. The decision authorised no deployment, and the bounded UDP
range the design sketched (four ports) is a proposal for that trial,
not a declaration. The SSH half has moved: the pod's sshd now sends
keepalives every 30 s (`ClientAliveInterval`, backlog `98c6514e`,
2026-11-02) and timestamps the line that says why each session ended.
The client-side keepalive is still owed on the reliability item
`d4c8e9bf`.

**The train is tested where the seed is** (design `128b5496`, David
2026-09-12, all three questions accepted as proposed; cars landed
2026-09-13). Measured off the forge's task list on 2026-09-12: CI's jobs
ran *serially* on the runner's one slot, about seventeen minutes a run,
of which `test` — `infra/gate.sh`, cold, in a fresh container — was
eleven to thirteen; and every train ran twice, once on the PR head and
again on the squash commit, for a result nobody read. The cluster gate
runs the *same script* warm on the reflink seed in three to five
minutes, and the forge's disk is ext4 (no reflink) and chronically
under its floor — so the seed does not move to the check; **the check
moves to the seed.** (1) **The launcher is the conductor**: when it
opens a train's PR it files a gate-run for the *train branch* — the
assembled tree — and creates its Job the way `boss gate` does, under a
`boss-conductor` ServiceAccount bound to the gates Role the dev pod
already uses, with kubectl and the runner manifest in the cluster
image; the gate-run carries `train_gate`/`train` marks and a `hold`, so
the stranded-green sweep reads it as HELD and auto-park leaves it alone
(`boss-cli/src/train_gate.rs`). A standing launcher that turns *any*
requested gate-run into a Job — a gate filed from the forge, a Mac, or
boss-gcp — is the general door, deferred. (2) **The verdict is both
halves read together**, off the system of record and never posted back
to the forge (the forge stays a one-way peer): a green CI does not merge
a train whose gate has not spoken; a red gate strikes the cars like a
red CI; a refused or lost gate is filed again up to three times and then
the train reads *aborted*, cars released unstruck. Diagnosed strike recovery
reads those same two halves: `boss release --diagnosis-file` accepts a
RED train gate, or a checked GREEN gate when the train's completed CI step
records both Forge and combined results as failing. The latter requires
the exact gate-run on both the train and CI step, the gate's reciprocal
train identity, a completed verdict and receipt for its frozen head with
passing named checks, and the car in immutable `boarded_jobs`. Missing,
malformed, unrelated, pending, aborted or successful evidence refuses.
The diagnosis names a full gate-run UUID; the supported merge door clears
the strikes and appends who looked, when, the verified gate and verbatim
diagnosis, keeping earlier releases. This is an operator action with
independent held review, not automated clearing or runtime activation
(David approved design `921c62af`, 2026-10-11; backlog `602fd374`).
A train that has lost its pointer to its gate is recovered from the
record, never by choosing a verdict (backlog `971c9006`). The conductor
reads a complete census of every gate-run filed for the train, closed ones
included, and binds the one original attempt at the train's exact head
without filing another. If it finds two or more genuine originals, it holds
the train, uncounted, with no new gate and no fallback to CI alone, because
it never picks the newest or the greenest (`launch_train_gate` and
`train_gate_census` in `boss-cli/src/train/conductor.rs`). Choosing among
several genuine attempts is a human trust decision, taken one train at a
time. Design `e312597f` (David, 2026-10-17, accepted as proposed) granted it
once, for train `be43bf8c` (PR 1213), as a guarded metadata repair. The
repair writes only the gate reference and an actor-attributed recovery
marker, and only after the exact head, the native Job, the launch intent,
the census and the provenance have been verified again. It leaves CI and
gate judgement, every hold and each duplicate's own outcome as they were.
That grant is not standing permission for selective adoption, automated
recovery or a new actor. Binding an earlier green never hides a later red
at the same head: the later red is reconciled under the merge policy, or
the train waits. While CI still ran
the Rust checks, a gate the conductor could not *file* fell back to CI
alone after three passes, stamped `train_gate_fallback`; car 3 sets
`BOSS_TRAIN_GATE_REQUIRED=1` in the same change that drops CI's `fast`
and `test`, and from then on a train waits. (3) **The per-train image
stays**, contrary to the proposal as filed: `locomotive.sh` reds when
the image's baked stamp disagrees with the tree, and a floating tag was
once served stale from the runner's cache (`6aa603ef`) — so
`build-image` keeps pushing `boss-ci:<sha>` (about a minute, layers
cached) and the disk cost it carries is the bigger forge disk's to
absorb. *Superseded 2026-10-18 (backlog `399ee6c3`):* 780 trains in
09-17..10-15 each pushed and pulled a 3.48 GB image while the forge's
disk floor stayed a named reason trains are rate-limited, and the tag
is now `boss-ci:<stamp>` — the recipe hash the
locomotive already checks, which no runner can hold stale — built only
when the registry does not already serve it under `:rust1.96` too. The double run went first (`fix/a-train-is-tested-once`,
2026-09-12: `pull_request` alone); the two hours of unconverged trains
on 2026-09-13 taught that a tag in the mirror *list* is a declaration
and the registry holding it is the fact, which the converge now closes
before it builds.

**A tree is proven once: a one-car train whose inputs equal its car's
gate carries the car's green.** Design `cdf626ac` ("Prove each tree once,
then stop recompiling it", David 2026-10-15, all three questions accepted
as proposed) measured that gate-shaped time on one disk, not agent time,
set the factory's pace, and that a one-car train usually re-ran its car's
gate on the same tree: in 2026-10-01..15, 48 of 216 comparable one-car
trains were tree-identical to the car's green, 48 of 48 green, 12.4% of
train-gate minutes. (1) **Telemetry and the gate disk come first, then
reuse.** The receipt now says where the runner spent its time
(`timeline`: runner start, each setup phase, `setup_s`; backlog
`d777b20a`) and samples io, cpu and memory pressure
(`infra/gate-runner/runtime-evidence.py`); the red's cause is recorded
as two facts beside the verdict, `equipment_fault` and `product_defect`
(`boss-jobs/src/failure_facts.rs`, item `c02fc3b3`), with `flake_of` on
a green re-gate at the same head; nothing yet tallies them across gates
(`1a75675a`). The gate disk went to the IT session as ops work (Q3), and
is the drive story of `121f44a1` and `3c475a19` below. (2)
**Reuse binds, never copies** (`boss-cli/src/train_gate_reuse.rs`,
backlogs `bbc13a4e` and `2647cf38`): the train's `train_gate_run` points
at the car's own gate-run when there is one car, its full or `--auto`
green (never a scoped one), an assembled head that is exactly one merge
of main at departure with that main the car's base, an equal tree, the
run.sh that tree carries, every container image equal by digest to the
one the car's pod resolved, and an equal launch request. An input the
record cannot supply refuses like an unequal one, and anything else files
the ordinary train gate. The conductor sets `BOSS_TRAIN_GATE_REUSE=exact`,
but `gate-runner.yaml` names its images by tag, so no train binds until
backlog `35a0de38` pins them. (3) **The queue-owned gate is the end
state, and it is not designed yet** (Q2): one full gate per car against
main plus the cars ahead, then the tested sha fast-forwarded, retiring
the separate train gate. Until it has its own design packet the train
gate stays, and a cold, unselected full run is to audit before test
selection is allowed on car gates. David chose `--auto` as the car-gate
default on 2026-11-02, full only for a trust-marked or judge-flagged car
(backlog `885873eb`), without that cold audit having run; the unscoped
full gate on main after each converge (backlog `77596188`) is the audit
that exists. `006f480f` below decides to widen reuse from whole trees to
single checks.

**A train gate runs every check its cars' greens did not already prove for
an input-identical tree.** Design `006f480f` (item 217e129c), both
questions accepted on 2026-11-02. A check runs unless some boarded car's
green proved it for an input-identical tree in an identical environment,
input-identical being computed from the check's derived input closure
against both the other cars' diffs and main's movement since the car's
base; every undeclared closure runs, and any global file moving runs
everything. The release guarantee becomes "every check in the scope either
ran on the assembled tree or is named as covered, with the receipt that
proved it": a covered check records `result: "pass"` with `covered_by`,
not a new result word. The conductor computes the plan and carries it into
the gate; the gate pod reads nothing from the system of record. Skipping is
licensed only after zero escapes over 30 consecutive trains or seven days
in shadow mode, and the full gate on main after each converge stays the
backstop. The always-run floor (the fixture, every lint, the tree-wide
pins, svelte-check) stays, because those have undeclared or live input
closures no receipt can prove for another tree; the pins' cost is attacked
separately.

**Trains depart on a declared rhythm, and a landed change's dependents are
proven on main after it converges** (design `c6d314a9`, "Deploying faster
without faster gates", David 2026-10-16, both questions decided). Measured
over October's trains: an unheld car spent a median 44.7 minutes from ready
to converged, 36% of it in the train gate and 11% in the 30-minute boarding
cooldown; replayed over the real arrivals, batching windows only added
latency and one car per train was ruinous (median 231 minutes). (1)
**Depart on a clear track** ("If the tracks are open, a new train can
depart after gating"): board rule v11 dropped the cooldown to zero, live
from 2026-10-17, after migration `20261016193000` let the cadence registry
accept a zero delay (backlog `613295dc`). The before/after reading the
question proposed was never written onto the packet. *Superseded
2026-11-02* (backlog `a74d06d3`): with depth 1 and no cooldown nearly every
train carried one car whose train gate re-tested its car's tree, so David
moved to hourly departures that skip an empty dock, and v12, depth 1 with
a 60-minute cooldown, is the active rule
(`infra/platform/cadence/train-board-on-dock-depth.toml`). (2) **Option
(a): an unscoped full gate on current main after each converge.** `--auto`
tests only the crates whose files changed; clippy compiles their
dependents but never runs their tests, and in 118 of 252 October trains
neither the car's gate nor the train's tested them, so a dependent broken
that way surfaced later as a red on an unrelated car. The conductor now
runs one full gate of main per main sha, coalesced, never while a merged
train is unconverged, last in line and only when a bay would still stand
free after it; a red files one urgent item naming the failing checks and
the trains merged since main's last green full gate
(`boss-jobs/src/full_gate_on_main.rs`, backlog `77596188`). It detects and
attributes; it never holds, strikes or cancels a train. Option (b),
testing every dependent inside the train gate, was not taken: the
dependent set is a median 38 of 51 crates, a nearly full gate on most
trains' critical path. The design's other finding, the byte-identical
train-gate skip, is `cdf626ac`'s reuse above; a dependency-graph "change
index" to skip disjoint train gates was judged not worth building at 6–8%
of train-gate minutes, and `006f480f` above later took up that question
check by check.

**The release image is built once, on the train, and the converge rolls
it by digest — decided, not built** (folded from `build-and-deploy-cost.md`,
its four questions decided 2026-08-14; restated for a container image by
design `a80f0608`, David 2026-11-13, both questions decided as proposed;
backlog `0d860145`). The 2026-08-13 measurement: the workspace CI had just
compiled was compiled again for the deploy, from scratch whenever a core
crate changed (30+ minutes on train 12, against 164 s for a leaf crate),
while across 14 trains the median wait was CI at 35 minutes board→merge and
the deploy at 3 minutes merge→deploy. The 2026-08-14 decision: (Q1) **one
build per commit, consumed by every environment** — option (a), CI builds
the release binaries and the deploy fetches them into the generation
store's `releases/<sha>/bin/`; (Q2) **the artifact is keyed by the commit
it was built from, a mismatch is refused, and what landed is recorded**,
the locomotive's equality posture; (Q3) CI's own cost does not move with
it, and is data to gather rather than a blocker; (Q4) worth doing now,
because "we want to be more active in general". **Q1(a) was never built,
and its mechanism is gone**: train #443 deleted the bare-metal deploy
(`deploy-services.sh`, `deploy-web.sh`, `deploy-confirm.sh`) on 2026-09-18,
and with it the only writer of `releases/<sha>/bin/` for services, so the
generation store of the 2026-08-12 deploy design (`deployment-as-network.md`
Q1) is retired; `infra/generation.sh` survives only
for its `gen_atomic_link`, sourced by `infra/estate/install-cli-from-image.sh:201`,
whose note says the store variables "are not used here". **What runs today
compiles after the merge**: `infra/forge/cluster-deploy-runner.sh:449`
runs `docker build -f infra/oss-quickstart/Dockerfile` at the merged head
on the forge host, whose `rust-build` stage runs `cargo build --release`
(`Dockerfile:175-184`); the image is pushed at `:488` and proves it boots
(`image_boots`) only at `:498`, when main already carries the tree. Nothing
before the merge builds it: neither `infra/gate.sh` nor
`infra/gate-runner/run.sh` runs `--release`, and forge CI, triggered on
`pull_request` alone (`.forgejo/workflows/ci.yml:38-40`), builds only the
`boss-ci` runner image. So one tree compiles three or four times — the
builder's worktree, the car gate, the train gate, the converge — and the
image that deploys is the first release build anyone made of it. `a80f0608`
restates the intent in image terms: (1) **forge CI builds the product image
once per train, before the merge** — build, `image_boots`, push by digest,
and record `{image_digest, image_tree}` on the train — with push rights on
the product image repo only (Q1), because the train is already the
assembled tree's test and forge CI runs beside the gate, not in a gate
bay; (2) **the converge pulls and rolls that digest when the merge commit's
tree equals the recorded tree, compiling nothing** — tree equality, not
commit equality, because the squash makes a new commit object that carries
the PR head's tree (`ci.yml:34`); (3) **on a tree mismatch it builds from
source as today and records `build_reason`** on its run summary, since a
refusal would leave a correct merge unconverged, and the fallback is
deleted if its count stays at zero for a month; (4) **`BOSS_BUILD_COMMIT`
stays the commit the image was built at**, and the train's `converged`
step accepts a running commit whose tree equals the merge commit's; (5)
**scope stops at the product image**: no release build moves into the gate
and the gate's tests stay debug-profile, so "the deployed binary is the
tested binary" holds up to the profile switch and no further. Built so far:
this fold. The CI image build and the converge-by-digest are two further
cars, after the gate-image digest pin of backlog `35a0de38` (Q2); until
they land, every converge still runs the `docker build` at `:449`.

**A car lands where its change goes live** (design `c6bd173e`, David
2026-09-15, all three questions accepted as proposed; from his feedback
`61366e5a` on the yard). Every car already carries a
`delivery_channel` — data | config | software | infra — stamped at gate
time from the paths it changed, the heaviest winning
(`boss-cli/src/channels.rs`); the yard read none of it, and "landed"
meant one thing, the image converged, whatever the car changed. Now:
(1) **the arrivals yard is four sidings**, one per channel, and a car's
wagon lands on its siding when *its channel's* live evidence exists —
data when the live-protocols / live-rules equality holds for its files
(read off the system of record), config when the converge packet
reports the manifest applied or the unit installed, software when the
train's `converged` step completes, infra when the host converge
packet reports — with the car's probe the only *proof* in all four.
(2) **Three terminal tracks and only three**: arrivals by channel, the
inspection shed, and a cancelled siding for a withdrawn car; struck and
left-behind are states of a car ON THE DOCK, drawn there with a badge,
never a track. (3) **The train has a channel too**, the max of its
cars: a data- or config-only train's `converged` is satisfied by the
registry read / manifest apply, not the image roll, so those trains
arrive in minutes. Built in three cars: the sidings by the stamped
channel first (`953aaf30`), the train's channel second, per-channel
landing evidence third.

**Migrated is a reading on the health answer, not an assumption**
(design `a5323701`, David 2026-09-24, its one question accepted as
proposed; backlog `7c298c34`, landed in train #643). `commit` on a
service's health says which build is RUNNING, never whether that
build's migrations have RUN, so a reader comparing it to a sha got YES
the moment the binary rolled — the false green in the header of
`boss-cli/src/running.rs`. In the cluster the `boss-init` initContainer
runs `migrate.sh` before a pod serves, so on our instance the two
usually agree; that is an argument, not a record, and it does not hold
for a database repointed or restored after the pod started, an
instance migrated `--without` a module, or any instance whose schema we
do not converge (the OSS quickstart, a hosted customer's). Decided and
built: (1) **`/api/jobs/health` carries `capabilities.schema = {head,
pending, first_pending}`** — `head` the full file name of the highest
applied migration (the legacy 2- and 3-digit prefixes are not unique,
and `schema_migrations.id` already holds the name); `pending` the
migrations in the running build's OWN tree the ledger lacks, so any
reader gets "migrated to my own build" without a checkout; and
`first_pending` naming the first of them. A bare count of applied rows
was rejected because it compares to nothing, and a `--without`
instance reading `pending > 0` is true, not a false alarm. The build's
list is generated by `boss-jobs/build.rs` from the one ordering,
boss-testing's `schema_order.rs`, which it `include!`s rather than
restates (§9a). (2) **Read on every request, never cached at startup**
— a startup read is a belief about the past, and a repointed database
is exactly what it would miss — bounded at two seconds
(`schema_level::READ_TIMEOUT`). The field has three states that must
not collapse: absent (this service does not report one), `null` (it
tried and could not read — never zero, never a pass) and an object.
(3) **The jobs API only** (`boss-jobs/src/schema_level.rs`: the pure
`judge`, the `SchemaLedger` port, the Postgres adapter); another
service adds the field when a reader needs it. (4) **The health read
stays behind authentication at the front door**: the gateway's public
`/health` stays a bare `ok`, because build sha and schema head are
version disclosure that helps an attacker aim at a known-vulnerable
build on a hosted instance. Our own reader, `cluster-watchdog.sh`,
reads the jobs port on the LAN; an off-LAN monitor authenticates like
any other reader, and a public read would be a new `PUBLIC_BY_DESIGN`
row with its own why. **Not built yet:** `boss running` judging a
MIGRATED layer by `schema.pending == 0` remains separate work.
Its DEPLOYED layer now reads the latest cluster converge's recorded
roll or successful unchanged-image check (backlog `004bdb2b`), naming
and dating that packet beside the live health build. An unread latest
record stays unknown; it never falls back to an older successful run.

**A builder is a pod, not a process in the operator's shell** (design
`90a14acc`, David 2026-09-15, all three questions accepted as
proposed). Measured 2026-09-14: two builder agents and the operator's
session in one 8-CPU / 16 GiB cgroup put memory at the ceiling and
throttled the pod 2,177 periods in an afternoon; builder throughput
was bought with operator latency, and a builder was invisible to the
record while it worked. Decided: `boss builder <packet>` renders a Job
the way `boss gate` does — the brief handed in, a cargo target
reflink-seeded from the gate seed volume on w-1 (the pin to w-1 is
accepted for now; a registry image of the target is the alternative),
the branch gated with the same park intent a human builder types —
and (1) **its forge credential is a short-lived, branch-scoped token
minted per run by the credential broker**, placed as a Secret the
launcher creates and deletes with the Job; never the operator's
identity, never one shared long-lived token; (2) **the pod ends when
its gate is launched**, with a 90-minute ceiling after which the
observer settles it as a dead runner; (3) the seed is the gate seed
volume by reflink. **Until the broker mints** (the maiden rotation,
`25dc5358`) builders stay in the dev pod: a bigger pod (`5ee0ff2a`,
held for a David-timed restart) and 4-wide niced cargo for `agent-*`
worktrees (`infra/dev/wt-cargo`), which took the throttling from
2,177 periods to ~350 over the next ten hours.

**Upgrading Claude Code is a bounded ops verb that David or the agent
may run** (design `773236fc`, David 2026-09-22, all three questions
accepted as proposed). Sometimes a new model is blocked on the upgrade,
so it is a precondition for working rather than housekeeping. Measured:
`/work/home/.local/bin/claude` is a symlink into self-contained binaries
under `versions/` on the persistent `/work` PVC — an upgrade is one
symlink flip, rollback is pointing it at a NAMED version already on disk
with no network, a running session keeps the binary it exec'd, and a
pod roll loses nothing. Decided: a verb `upgrade-claude-code` under
`infra/ops/verbs/` that resolves the target to a concrete version and
records it (never "latest", which names nothing), records the version it
leaves as the rollback target, defers while a `gate-run` is launching,
proves itself by `claude --version` or re-points to the recorded version
and reports failed, and prunes; (1) **the agent files and runs it** —
bounded, offline-reversible and unable to touch work in flight, so
gating it on David gates a model on his being at his desk; he keeps the
UI entry; (2) **the target is per request**, recorded on the packet,
never pinned in the tree, which would make every upgrade a car and a
converge; (3) **three versions are kept**. Upgrading at pod start is
rejected: it makes the pipeline's tool whatever the registry served at
boot, with no receipt and no named rollback. Not yet built; the dev pod's
manifest only installs the tool when it is absent.

**An agent reads the cluster through a scoped credential, never the
admin kubeconfig** (folded from `agent-cluster-read-path.md`, all four
questions decided in packet `f2f09077`, David 2026-08-16: "I think it
makes sense then to have kubectl on GCP as a backup?"). Measured then:
the Kubernetes API answered a clean 401 and no host an agent could
reach held a credential, so an investigation read the employee roster
from the wrong instance's database and published a root cause it had
to retract. Decided: (1) **a read-only, namespace-scoped
ServiceAccount**, because the host was internet-facing and an admin
kubeconfig would make its compromise the cluster's — accepting that
read-only cannot restart a wedged pod; (2) `pods/portforward` is
granted, decided rather than inherited, and named for what it is: not
a read verb; (3) the token is a file only the service user can read,
and **bounded, expiring tokens** turn rotation from a discipline into
a deadline; (4) `SECURITY.md` gains a paragraph saying the host holds
a scoped cluster credential and what its blast radius is. Overtaken
since, with the principle intact: agents read the cluster from inside
it (the dev pod, and the read-only ops verbs below); boss-gcp holds a
scoped break-glass ServiceAccount, read plus roll-to-last-known-good
(`infra/cluster/manifests/boss-break-glass-operator.yaml`), delivered
by the machine as the next paragraph but one decides; and any
credential admin can mint goes through the credential broker. (4) was
never done: `SECURITY.md` names no cluster credential today.

**Cluster management runs on an internal host; the workstation is a
terminal** (design `1bc4b4ed`, David 2026-09-12: "I would rather one of
the internalized systems be where commands run"). Measured: every
`talosctl` and admin `kubectl` ran from `~/talos-homelab/v2` on a
laptop — the cluster's root credential and machine configs on a host
that is not managed, converged, observed or reachable — and the gate
seed's kubelet mount took nine hours to diagnose because three of the
reads that settled it had to be typed by a human and pasted back.
Decided: (1) **the forge declares a `cluster-operator` role** (a Class
row on the node, `infra/estate/roles.toml` saying what the role
installs) — "start with forge; a GCP VM may make more sense later", so
w-2 off Talos and a GCP VM are recorded as future options for
separating the credential from git+registry; the role brings the
interactive door first (`talosctl`/`kubectl` on the forge, credentials
root-only under `/etc/boss-ops`, installed once by David — the
estate's converge never writes a credential — and the Talos configs
move there as the canonical copy), then (2) **mechanical reads as ops
verbs** — `node-status`, `pod-describe`, `pod-events`, `talos-ls`,
`talos-logs`, `talos-get`, literal argv, reads only — and (3) **one
bounded act**, `node-converge <node>`: `apply-config --dry-run` against
the per-node file declared in the repo (non-secret entries only; PKI
stays in `/etc/boss-ops`), the diff recorded on the packet, then apply.
**Reboot, upgrade, reset and etcd membership never become verbs.** The
question of *how the operator's own terminal should carry the
credentials* is not decided: David's answer asked what professionals
do and whether a small standalone management tool should ship for
terminals like the Mac, and that stays open on the packet.

**A credential the estate can mint is delivered by the machine, never
carried by a person; the line is derivability, not sensitivity**
(design `835c0c9c`, David 2026-09-20, all three questions accepted as
proposed). Root material — an admin kubeconfig, a
talosconfig, the credential broker's own root tokens — cannot be
minted from anything the estate holds, so placing it stays David's
act, as above. A scoped credential is minted FROM that material, so
its path is the credential broker
(`infra/cluster/manifests/boss-credential-broker.yaml`), which exists
because a hand-placement walkthrough leaked the dev pod's forge token
on 2026-09-02 — the ceremony was the vulnerability. The first case the
broker's Secret-and-mount pattern cannot reach is boss-gcp's scoped
break-glass kubeconfig (`boss-break-glass-operator.yaml`): an
off-cluster host cannot read a Secret without the credential being
delivered. Decided: (1) **push, don't pull** — a bounded ops verb on the
forge, which holds admin and already reaches boss-gcp, mints the
kubeconfig and deposits it over a forced-command key scoped to
`/etc/boss-ops/`, the shape `boss-backup` already uses for the nightly
dump, so the value never enters a packet, a transcript or a person;
(2) **verify by effect in both directions** — the credential CAN read
nodes and CANNOT read secrets — and refuse the deposit when the
negative fails, because a break-glass credential broader than its
declaration is worse than none; (3) **a host declares the credential
set it is expected to hold** (`BOSS_OPS_CREDENTIALS=kubeconfig` on
boss-gcp), since a talosconfig has no scoped form and a full one on the
public edge is the unbounded grant the scoping exists to avoid — so
the converge reports the truth about that host rather than a
permanently red absence. The delivery verb is not yet built. When it
is (`7336cb5f`, design `b08725c2` Q3, David 2026-09-29), the Job that
reads the token **runs as a measured non-root uid** of ONE image that
carries both kubectl and openssh-client, with fsGroup set and the
deposit key mounted 0440 to that group; a rehearsal ssh deposit proves
OpenSSH accepts the key rather than assuming it, and only if that probe
fails does root become the answer, as a stated decision with the
measured reason in the lint declaration, never an exemption entry. It
lands after the break-glass narrowing (`4e1c33b4`), because the token
can patch every boss Deployment and so read every credential those
templates carry, and its verify-the-negative gains one probe: patching
a Deployment's command must be refused. Decision
(3) was built as data rather than as that variable (2026-09-29, backlog
f371c749): the `[ops_credentials.<host>]` tables of
`infra/estate/estate.toml`, read by the host check and by
`estate.compare`, whose alarm names David only for root material and
names the broker delivery for a scoped credential.

**Agent B's mailbox: BOSS holds the operational pair, the founder holds
the account** (design `0ec5e1d2`, David 2026-09-21, all three questions
accepted as proposed). The keyvault already exists — the credentials
registry holds knowledge and never values, values live in k8s Secrets
read only to send them, the broker mints, verifies by effect and
revokes — so the question was what goes in it. Agent B has four secrets
and they are not one risk: the Proton account password (the whole
mailbox, including changing its recovery address), the MFA seed, the
Bridge IMAP password (one Bridge install, localhost only) and the SMTP
submission token (send as Agent B, nothing more). Decided: (1) **BOSS
holds the IMAP password and the submission token; David holds the
account password and the MFA seed**, and neither of his ever enters the
registry, the tree or a transcript — an agent holding the account
password can take the mailbox, which no protocol step needs; the same
scoped-not-root split as the break-glass kubeconfig above; (2) **both
BOSS-held secrets are registry-declared and broker-rotated**, the shape
of `stripe-restricted-read`, never hand-placed; (3) **MFA goes on now**,
its cost accepted knowingly: Bridge keeps its session in the host
keyring, so a session that dies at 03:00 stops the mail sensor —
`sensor_unreadable` makes that loud, and recovery needs David. Not yet
built: the credentials registry holds no Agent B row. Whether the
founder-held pair belongs on the one-stick recovery kit (`c1bb822e`) is
that kit's question.

**The way back in is a printed sheet rendered from the tree and signed
when printed; the paper carries locations and order, the kit carries
values** (design `125d405d`, answering backlog `fd6d6c08`, David
2026-09-28: "we are going to maintain the fidelity of those
instructions or generate a new print job. The print job can simply be
a PDF download to print"; all three questions accepted as proposed). Measured on
origin/main `b4c80a0d`: no printable re-entry sheet existed, and the
two prose runbooks playing its part each contradicted the tree — the
access-recovery runbook named Kanidm as the human door's recovery
while the gateway boots `LocalAuth`, and the dev-pod-access runbook
landed in tmux session `main` while `boss-dev.yaml` starts `dev` — the
retyped copy §9a exists to refuse (both runbooks were deleted on
2026-09-29 by the car that added `infra/recovery/re-entry.toml`).
Several facts a human needs under stress lived in no file at all; the
admin kubeconfig has two spellings (`fb444bbb`); and the break-glass
door has one road in, Cloudflare Access plus the in-cluster tunnel,
with nothing saying so (`a15a1cd2`). **The rule it serves: a human act
is a signature on rendered bytes, never a transcription.** Decided in
the design from the company frame: (1) **one source**,
`infra/recovery/re-entry.toml`, an ordered list of roads, each an
ordered list of lines that are exactly one of a **fact** reference
(`file` + `key`, resolved by the parser that file's real consumer uses,
so there is no second opinion of what a file says), **prose** that
states no host, port, URL, path or name (a pin refuses the literal),
or **`not_tree_held`**, printed as "NOT HELD BY THE TREE: <why>" rather
than going silent; the dev door's client steps, the one fact already
spelled twice (the `/it/estate` page and the paper), **collapse** into
one data file both read; (2) **the version is a sha256 over the sorted
fact set, never over PDF bytes**, which carry their own timestamps and
would call every day stale — the PDF's own sha is recorded beside it,
because the fact hash says what the paper says and the byte hash says
which bytes were signed; it is rendered from origin/main, the one sha
every reader can name; (3) **a road's dependencies are derived, not
written**, so the break-glass road says it needs Cloudflare's edge, the
Access one-time code and the in-cluster tunnel, and reprints by itself
when `a15a1cd2` changes that; each road prints the packet that last
proved it or **NOT EXERCISED ON RECORD** — no evidence is not a pass;
(4) **the printed version is the fact hash on the last completed
`print` step of a `reprint-recovery-sheet` packet**, and there is no
other record of what is on paper: a daily chore in the shape of the
playground crawl (the `boss-ci` image, which already carries Chromium,
so no browser enters the production image) compares the tree's hash
with it and files, or refreshes in place, ONE print job carrying the
PDF as a file_ref and the fact-level diff; its `print` step is a
presence-signed sign-off whose statement — "printed version X,
destroyed version Y" — the machine fills, so David types nothing; (5)
the two runbooks are **deleted** in the car that lands the sheet, so
there is never a window with two copies. David's three answers:
(1) **the paper holds LOCATIONS, ADDRESSES and ORDER only** — which
door first, which host, which file, which console, and the kit
(`c1bb822e`) by NAME with its restore order — never a secret, a PIN, a
recovery code or any value; every artifact that is itself a credential
or restores state lives in the kit. One-time recovery codes on the
paper are rejected, because a sheet holding a secret must be guarded
as one and then cannot be left where it will be found under stress;
(2) **reprint on every change, plus a 90-day existence check** — a
presence-signed step reading the version last signed and asking that
the copy be confirmed where it is kept, because a sheet nobody can
find fails exactly like a stale one; (3) **the location facts become
tree keys, published by the public mirror**: the bastion's GCP
project, zone and instance, the cluster API VIP, the bastion ssh user,
the domain registrar, the mail provider behind the Access one-time
code, and the kit's contents by name with its restore order; **which
private key opens the LAN door, and which device holds it, is never
declared** and prints NOT HELD BY THE TREE. The plan is five cars,
every one trust area `credentials`, gated held for an adversarial
review whose single question is whether any value, or any new
exposure, reaches the paper or the public mirror. Car 1 landed the
source, `boss recovery sheet` and the pin; car 2 the PDF. **Car 3 is
the reprint loop, and the 90-day existence check rides it**: `boss
recovery reprint`, run daily by the `boss-recovery-sheet` CronJob
(boss-ci's Chromium, a clone of main, the `boss` CLI copied from the
boss image), reads the version on paper off the newest `print` step
completed `approved` with a LIVE presence stamp, files or refreshes ONE
`reprint-recovery-sheet` print job with the PDF attached to that step
and the fact-level diff on the packet, and — when the paper is current
and the newer of the signature and the last `kept` confirmation is 90
days old — files one `confirm-recovery-sheet-kept` packet, whose
`missing` outcome makes the next run file a reprint of the same
version. Car 4 (the `/it/estate` line) and car 5 (the Q3 keys) remain.

**The knobs outside the tree become declared settings** (design
`16115a17`, David 2026-09-12; all four questions accepted as proposed).
Measured 2026-09-11: three of the five levers behind a 22-minute outage
of the system of record and a publish PR closing itself two minutes
after it opened were settings that live in no file here — a Forgejo
push mirror found only through its API, the Talos kubelet image-GC
thresholds, Longhorn's node-down pod-deletion policy — each an
imperative act with no packet, no declared value to converge from, and
no observer comparing it to anything; and retention had the same
absence, three reclaim scripts each carrying its own constant for the
lifetime of an image, a target dir, a `publish/<date>` branch. Decided:
(1) **scope** is the four that bit — Talos kubelet extraConfig, Longhorn
settings, Forgejo repository settings, and the retention rules — with
GitHub token/repo facts recorded but not compared (no read without the
token); (2) **one table, `declared_settings`, with a `kind` column**
(`setting` | `retention`) — retention *is* a declared setting of an
artifact kind, and one observer and one alarm path is the point (two
tables would drift the way the three scripts did); (3) **apply is an
ops verb per system where a runner can reach it** (Longhorn via the
conductor's kubectl, Forgejo's API from the forge) and **a named human
step for Talos** (`talosctl` is David's door), and **verify is always
the observer's next comparison, never the applier's exit code**; (4)
**drift raises through `estate.alarm`** as a hard finding carrying live
and declared values, never auto-applied, and a setting nobody declared
but the observer can see is reported **UNDECLARED** — the third state
beside declared-and-matching and drift. Not yet built; the Talos
entries the gate depends on are being declared first (`08430090`).

**A maintenance chore states its coverage, its numbers, and its
suppression** (design `14c135f5`, David 2026-09-11; all three
questions accepted as proposed). Nine maintenance mechanisms were found
failing in one session on 2026-09-10 and they were one shape, not nine
bugs: a chore is not obliged to report honestly about itself. Seven
daily sweeps dead nine days each behind its own undrained packet; the
mirror check dead nineteen days while the mirror drifted 238 commits;
a branch sweep reading the newest fifty trains and calling it total;
139 disk-floor packets recording `result: ok` and no number while
computing the very number that decided it; a `disk_tight` alarm that
could never fire for a cluster node because the observer records
capacity, never headroom. **Coverage** — what population was examined
and whether that was all of it; **numbers** — what was measured, not a
word for how it went (CLAUDE.md's rule about reducing a record before
storing it, applied to successes); **suppression** — "did not run
because a packet is open" distinguishable from "ran and found nothing".
Decided: (1) **the contract lives in the Workflow registry**: the
`measured`/`coverage` fields are REQUIRED on maintenance chore kinds via
`metadata_schema`, so the protocol refuses a chore packet that cannot
say what it examined — the packet is the durable record, and a lint
over scripts would catch the author but not the packet; (2) **a
suppressed firing refreshes one open record, never twins** — the estate
alarm's existing idiom (bounded dedup read, refresh not twin, a human
close suppresses for a week, self-clear stamped) — because a packet per
suppressed tick would file hundreds and no packet is the invisible
state that cost nineteen days; (3) **a chore that cannot state its
coverage FAILS**, not warns: the audit-integrity alarm's history is the
proof that permanently-red and green-with-warnings decay the same way.
First instance landed 2026-09-15: the protocol-drift chore records
`measured` (kinds compared, fields, head) on every run.

**The LAN machine door carries every read surface of the instance, not
only the jobs API** (design `28d2bed9`, David 2026-09-17; both
questions resolved as proposed). Measured: `boss-jobs-internal`
(10.20.0.34, MetalLB, LAN-only) exposed one port, and
`boss-sor-read` — the one way a car's recorded probe reads the system
of record — was `curl $BOSS_JOBS_URL$path`; so the batch-door events
car (`056f7bd8`) sat UNPROVEN because `/api/events/tail` lives on
boss-events-api and answered 404 on the jobs port, the locations door
(`1ec8312a`) was proved by tree shape instead, and every future car
about people, classes, locations or the audit tail had the same
blindness. Decided: (1) **the door widens, the trust class does not** —
the events, people, classes and locations ports join the same Service
on the same IP, header-trusted and LAN-only exactly as the jobs API
already is there WITH its writes (the conductor writes through it), so
a read service on that IP exposes nothing to anyone who could not
already write the record; the manifest's ports are boss-ports' prod
ports under boss-ports' names, pinned by one equality test
(`the_machine_door_carries_every_read_surface.rs`, CLAUDE.md §9a); (2)
**the reader routes by path** from a `name=port` table rendered from
boss-ports into the ops-runner's environment (`BOSS_SOR_PORTS`, read
by `boss prove --unattended` — until 9f00a805 car 2, run-car-probe.sh — from `infra/forge/sor-ports.env` in its own
checkout), defaulting to jobs, still one argument, still refusing a
URL and an unidentified read, and unchanged when no table is present;
(3) **the gateway service-token door is deferred** to the hardening
programme, as the machine door for OFF-LAN callers — the probe runner
is on the LAN and needs no public door, and a proof instrument that
depends on Cloudflare and a credential rotation is the wrong
dependency to add before hardening starts. Not chosen: the jobs API
proxying read-only paths for the probe tier, which duplicates the
gateway's routing inside a domain service.

**A probe is a declared object that is executed, not shell text that is
graded.** Design `0fe4c35e` (2026-10-31, filed with no questions because
"proven requires a machine-run probe" already states the intent) retires
the regex text judge in `boss-jobs/src/probe.rs`, which thirteen open
items each caught misjudging in one direction or the other. A car's probe
becomes `[probe]` data in its park file: `since` (a reference resolved
from the record, such as the car's recording start), `read` (one of four
kinds: a named gate check, a tree grep, a record read, a host read),
`expect` (a typed comparison: after, before, equals, count_gt, matches)
and `waits_on`, required when the read is a close the probe waits for.
`boss prove` resolves `since`, performs the read and evaluates `expect` on
typed values, so "does this probe compare the recording start" is answered
by running it. The park door and the prove door share one validator, so
the two cannot judge different sentences; a refusal names the key and its
value in full; a shell read declares the tools it runs as data, compared
with the forge's tool roster rather than its prose. The text judge, its
clauses, lints and spelling pin are deleted in the third of three cars.

**Whether a car landed is written once, by the train, at the merge.**
Design `98f35ac2` (2026-10-31, no questions: every input is already on
the record) ends eleven readers re-deriving "did this change land" from
git after the fact, each wrong in its own way. When the conductor merges a
train it records on the train and on each boarded car, in one evented
write, `landed = { train, squash_sha, car_head, patch_ids, tree_sha_after,
at }`; a car boarded as a twin of a landed change gets `retired = { as:
"twin", carried_by, landed, at }`, and the item that named the car receives
the fact through the car→item edge. `boss merged`, `boss car retire`,
`gate::landing`, the lost-gate relaunch, auto-park and the bury pass read
that record first. Git is consulted only for a branch no train recorded,
through one fallback (merging the branch into main yields main's own
tree), and that fallback answers UNKNOWN, never MERGED, on a
whitespace-only patch-id match or a failed `git cherry`. The vocabulary
gains the twin: a reviewer verdict `retire` with `carried_by`, retirement
across items, an arrival rule that closes an item on `landed` or on
`retired` as twin, and an auto-park that refuses a green whose change the
record says landed.

**One change, one packet: the car is the change from its first build
minute to its proof.** Design `689d1f7f` (David 2026-11-08, from his
question "are job packets and cars still distinct things?"; all three
questions accepted as proposed). Measured that evening, one change was
four packets — the ask (backlog-item), the builder (agent-run), the test
(gate-run) and the car (ship-a-change) — and 299 of the newest 300 cars
had been filed by the auto-park rule at gate GREEN, so the engineering
station's ship-a-change line showed nothing in build or in the gates
while four builders and four gates ran. The slips that day came from
the stitching between the four, not from the work: a re-gate on a
renamed branch parked a twin car, and `rerail --finish` existed only to
copy a receipt from one packet to another. Decided: **the ship-a-change
packet is the change**, admitted when the build starts and the one
packet every reader of "this change" reads — the station page, the
yard, orient, the train, the proof. (1) **The ask survives beside it**:
a backlog-item's intake, triage and disposition are a different thing
from a change and often yield several cars or none, so its `build` step
is fulfilled by its car(s) (factory-taxonomy-v2 R8, a step fulfilled by
packets one altitude down) and only the linking code goes. (2) **The
agent-run and the gate-run stay packets**, as children one altitude
down — the gate is an equipment station with bays and its own protocol,
and a run carries the claim, the worker and the spend — each carrying
one parent edge to the car step it fulfils instead of cross-links.
(3) **Five cars, one at a time through the trains**, because each
touches dispatch, the conductor or the auto-park handler the factory
runs on. Built: car 1 (train #1645, 2026-11-09, item `68a8726e`) — `boss
dispatch --started` admits the run's car at `build`, adopting the
branch's open car on a repair instead of filing a twin; a park-intent
`boss gate` completes `build` and stamps its gate-run on the car's
`gate` step; a verdict that is not green hands that step back to READY
with the verdict beside it; and (train #1648, item `366c9fb1`) a car
whose run ends `refused` or `died`, or whose item closes unbuilt, before
any gate ran ends through ship-a-change's `abandoned` terminal, fenced
against a gate launch on one gate-step key so exactly one of the two
lands. Car 2 (train #1690, 2026-11-11, item `a3a367e4`) — the gate-run
records the R8 edge, `fulfils = {job, step}` (`boss_jobs::fulfils`),
written only once the car reads back holding the launch's stamp, and
its verdict follows that edge: green completes the step, anything else
hands it back, every write judged on the stamp, and auto-park neither
files, refreshes nor adopts for an edged gate-run. The edge is not
`parent_step_id` on purpose: the subjob rule completes a parent on ANY
close, and a gate-run closes on a red too, so that spelling would have
boarded red code. Not yet built: car 3 retires auto-park's park and
refresh path, `rerail --finish` and the receipt copy keys
(`regate_receipt`, `regate_note`), so a re-gate is the gate step
fulfilled again by a new child at a new head; car 4 has the item's
`build` step fulfilled by its car and retires `--park-backlog-item` /
`--park-partial-item` for "which item this car fulfils", recorded once
at admission; car 5 retires the stranded-green sweep (a green with no
car cannot exist) and the yard's car/gate join. Until car 3 lands, a
gate-run with no edge — launched before car 2, or one whose edge write
was lost — still reaches its car by the auto-park scan, and the
re-gate repair CLAUDE.md §Doors describes (park flags, or `boss rerail
<car> --finish`) is still the live door.

**A publication executes once, as an Operations child fulfilling the
publication step.** Design-doc *Five further collapses of duplicated
factory facts and packet handoffs*
([942dc0ac](https://boss.algedonic.dev/ux/jobs/942dc0ac-f4ec-48fb-aa0b-078544cd8803),
David 2026-11-08, all four questions accepted as proposed) found one
publication split across two packets: a `publish-a-protocol` (or
`publish-to-github`) packet owns the approval and a nominal step, a
sibling `ops-request` executes it, and ten dispatcher rules plus prose
regexes copy its result back through the `for_protocol_publish` and
`for_publish` edges. Decided (Q1): R8 extends beyond software change to
publication. The publication packet keeps its person's approval; its
step is fulfilled by one Operations packet one altitude down, bound at
the child's admission; the child keeps host admission, the signed
approval, the named signer and the immutable execution result, and the
parent's completion is judged from that structured result. The bespoke
filing and result-copy rules retire as each consumer moves; verb output
stays as evidence, and a correct refusal (a failing check) still
refuses. Not yet built (item `dcbeef5a`). The same design's other
decisions sit at their topics: registry authority (after the drift
comparator, under the network substrate), robot model class (after the
data pass that put classes on the robots) and the gates reservation
(after gate admission, below). Its one collapse that needed no
question has landed: the ops verb files compile once, in `boss-jobs`,
and `boss-dispatcher-handlers` reads `boss_jobs::ops_verbs::verb_files()`
instead of its own generator and duplicate pin (item `418fd736`, train
#1637, 2026-11-09).

**Gate admission is a claim on a pool, not a hand-built line.** Design
`d996ce3d` (self-critique pass H1, David 2026-11-01: "we need simple
units"), its one question accepted on 2026-11-02, found gate admission
deciding each of its questions twice: train precedence by two mechanisms,
a place in the line held by four liveness rules with three TTLs (the
waiter heartbeat, the packet's presence, the train's bound-wait key, the
dock's kept claim), occupancy counted from both claims and a native Job
census, the bound in three definitions, and an oldest-first line that was
the one queue in the system not reading priority. Decided: the gate bays
are the **first billed protocol version on the claim door**, ahead of the
agent workspace slot, because bay capacity is the one capacity the factory
already measures daily. A gate-run's bay step claims one unit at the
`gates` station through the ordinary claim door; train gates, dock
re-gates and car gates are the same claim with a class. The station's
discipline `["class", "priority", "age"]` (class rank train, dock re-gate,
car gate) is the only order, the claim is the only record of occupancy
(the Job launched after it carries the claim id, and the census only
reports a Job without a claim), and the claim's lease is the only
liveness. The actor cap counts building runs only, so a run waiting for a
bay no longer holds a run slot. *Superseded by the station tree (design
`a3c5e58a`, 2026-11-08):* the per-actor cap is retired now that
`robots:<model>` pools carry concurrency (item 3e927996), so a robot is
bounded by one mechanism, its pool.

In the station tree's words (4b27a65a, reconciled 2026-11-06) the bound
is the `gates` station's capacity, 2 until the new drive is measured;
`gate_max_concurrent` is already gone from the delivery policy, and
`bay` is a retired word for new files.

**A train's place at the gates is one reservation, and a native Job is
an observation of it.** Design-doc *Five further collapses of duplicated
factory facts and packet handoffs* (`942dc0ac`, Q4, David 2026-11-08)
measured one place held in several systems: the train's
`train_gate_bound_wait` metadata, a separate count of it in the gate
line, and a native admission Job plus a runner Job joined by packet
identity, so a lost CREATE acknowledgement could count one train twice
(item `279a388e`) and a census-unavailable hold kept its place with no
time bound (item `05bc8fe5`). Decided: the target is one recorded
reservation at the `gates` station, on the gate-run's step and claim,
covering queued, admitted, running and released, with native Jobs bound
to it by immutable identity, so a lost acknowledgement is answered by
reading back the same identity rather than a second place. That target
waits on proof of crash recovery across the API/native boundary. Until
then the place readers are unified over the existing atomic native
reservation, classification is fixed, and admission is not relaxed: no
database count replaces the atomic CREATE, every real execution stays
in the census, and an unreadable census still refuses. The reservation
belongs to the gate-run, not to the train it fulfils under R8 (item
`b8d65e3c`, not yet built).

**A scoped gate admission owner is selected by a new value of the
existing `gate_queue_owner`, and the value proves nothing by itself.**
Design-doc *Fence the cutover from existing gate senders to scoped
admission* (`32b69969`, opened by agent-codex, its one question
accepted as proposed by David 2026-10-17) found that the gate-capacity
plan (designs `1e787fc2`, `640acd5c`, `f31d94ac`) named no signal that
would move gate sends to a scoped, fenced native admission owner, and
that a policy read alone cannot stop an older sender that has already
passed its check. Decided: no policy field is added and the single
total, train-first, is kept; the contract of the existing
`gate_queue_owner` gains one more value, distinct from `legacy` and
`resident`. Its proposed spelling is `scoped-native`, and the answer
keeps that spelling a proposal, not an approved phase. A client that
checks the policy refuses a value it does not know rather than falling
back to `legacy` or `resident`. In the new mode a client refuses a
direct train, car or legacy native CREATE and submits to the declared
station; the scoped owner, after selection and immediately before its
one conditional fixed-name runner CREATE, re-reads the same pinned
policy and the current native membership and bay binding. No failed
read, unknown attempt or absent object permits fallback, resend,
adoption or bay release. The value takes effect only after the
compatible API and schema, every managed launch entry point and the
exact executor artifact are installed and evidenced, and every earlier
sender is proved stopped and unable to restart, with its old and
uncertain children still counted against the total. It grants no
principal, role, native permission, enrollment, credential, publication
or activation; those stay separately authorized. If the field is not
extended, the record must name another authoritative signal and how
each old writer is fenced, and required metadata an old writer does
not know is not a fence. Not built on origin/main (2026-11-13):
`resident_gate_queue` in `crates/core/boss-jobs/src/delivery/types.rs`
and the CHECK in
`infra/postgres/schema/20261016050000-gate-queue-ownership-policy.sql`
accept only `legacy` and `resident`, and the policy bundle
(`infra/platform/delivery-policy/train-conductor.toml`, v12) declares
`resident`. Since this answer the total left the delivery policy
(v12) and is the `gates` station's capacity, 3.

**A gate whose first claim was rejected before its send permit may close
withdrawn, and only on proof.** Design-doc *Guarded withdrawal after a
proven pre-send claim rejection* (`5d2bc66d`, Q1 accepted as proposed by
David on 2026-10-17, for urgent item `870dbfe4`) supplements the native
attempt design `9dfd7b85`, whose rule stands: a caller that finds an
attempt still SENDING reconciles natively or stays UNKNOWN. The measured
case fails earlier: the server commits a first SENDING claim, the
original receipt names a different actor than the attempt (cf060e7d:
`codex@algedonic.dev` against canonical `agent-codex`), and the launcher
refuses before it constructs SendPermit. `boss gate --withdraw` rightly
refuses that state today, as a protected native attempt that a later
absence cannot prove harmless (`withdraw_refusal` in
`boss-cli/src/gate.rs`). Decided: one more proof class, first
claim actor mismatch with no predecessor attempt and nothing broader,
that permits a terminal **withdrawn** verdict and nothing else. It
requires the genuine original conditional receipt and its event pair,
every other confirmation predicate matching, independently retained
provenance binding the original invocation to a compatible launcher
whose source fails that confirmation before the only SendPermit
constructor, the existing writer and authority checks, and a complete
fresh native census that finds no execution. The operation records a
typed rejected-claim proof on the existing `gate_orphan_claim` contract
through the same conditional arbitration as the sender, reconciles
natively again, then writes the existing withdrawn verdict and reads back
the close. It preserves the original SENDING attempt, request and
receipts byte for byte, and creates no SendPermit, `no_child`, retry,
deletion or native action; later work is a new packet. A missing
artifact, conflicting evidence or any possibly effective execution stays
UNKNOWN, including cf060e7d if its launcher provenance cannot be
recovered, and 27112421, whose Job and Pod genuinely exist, is a refusal
control. Approving the mechanism settles no gate. Not yet built.

**A scoped executor passes the machine door on its own verified
credential, never on the estate token.** Design-doc *Admit the scoped
executor purpose at the machine door without an estate token*
(`ed279f2a`, opened by agent-codex, its one question accepted as
proposed by David 2026-10-17) is a trust-boundary amendment to the
gate-capacity plan (item `0f4224bc`) and to the scoped-admission cutover
of design-doc `32b69969`, and replaces neither that cutover's signal nor
its proof that old writers are stopped. It found that the jobs API
mounts the machine gate around every other door, so in a non-degraded
`enforce` only the estate token or the probe-reader credential passes,
and the reader is GET and HEAD only: a scoped executor had no write
transport, and nothing approved said what it should present. The
estate token would grant a broader identity than its purpose, and
exempting its URLs would admit unqualified callers. Decided: the jobs
service may hand its existing machine-door admission one explicitly
configured, server-owned purpose resolver, and only the configured
native executor's fully verified purpose credential passes through it,
in `enforce` too. Before resolving any identity the server verifies a
distinct issuer and audience, the signature and lifetime, the current
Pod, Job and ServiceAccount UIDs, a registered open run, the exact
installed executor artifact and its protected enrollment on the
published pinned workflow. An admitted credential reaches only the
operation its sealed context authorizes: each write still needs its
exact control, selection or bay scope and current policy, and an
identity read never supplies a write or a send. It is not an estate
token, a probe reader, an asserted actor or header, a policy Allow or a
broker-stage authority, and the raw credential is taken off the request
before any handler and never recorded. Missing or partial configuration
leaves admission exactly as it is; an invalid, expired, mixed,
duplicate, foreign or changed identity refuses, and acceptance and
refusal stay visible in machine-door reporting without token values.
The answer approves a server admission contract only. No principal,
catalog entry, credential placement, native permission, enrollment,
publication, cutover or activation is granted; those stay separately
authorized under `32b69969` and the parent. Not built on origin/main
(2026-11-13): `crates/core/boss-jobs/src/bin/boss_jobs_api.rs` mounts
`boss_core::machine_gate::mount_with_request_latency` with no resolver,
and the gate's `Presented` in `crates/core/boss-core/src/machine_gate.rs`
has no variant for a purpose credential.

**A delivered proof records the protocol scope it covers.** Design
`52b65a31` (agent-codex, 2026-11-16; answers `b141b8e4`, no open
questions) records the engineering contract for the readiness probes
part. A car records versioned scope bound to its frozen gate head:
repository, base and head, protocol kind and registry version or content
identity, owning station at that version, derivation and diff or
publication evidence. Derive workflow additions, deletions and renames
from their declarations; tenant changes carry their repository and
publication evidence. An item's department is a routing hint, not proof
of scope. Code-only changes need explicit evidenced scope through the
existing review. Missing, ambiguous or conflicting scope stays unknown;
an evidenced empty scope is a different fact. The server validates the
association, a changed head refreshes it through re-gating, and a
historical backfill is explicit and audited, never inferred silently
from today's owner.

Readiness joins that scope to deployed artifact evidence and actual
machine-run proof results on the server, preserving original rule
versions, verdicts and separate corrections or rule sweeps. A green
build gate alone proves no delivery. Return the observation cut,
population completeness, obligation counts and evidence IDs. Count each
car/protocol obligation once; keep retirement obligations with the
former owner and do not erase unpaid obligations on later publication.
The population is all retained delivered cars through the recorded cut;
unassigned unknown scope remains visible rather than disappearing from
the join. The probes part is true only with a complete population and
attribution, at least one applicable obligation and qualifying passes
for every obligation. Known owed or failed obligations make it false
when judgment is complete; unknown attribution, unread evidence,
incomplete sources or no applicable obligations make it null with a
reason. Known failures remain visible alongside a null result. This
does not replace the proof classes below or turn source-only evidence
into a runtime pass. Readiness and retros use the same server result.

**Not built:** first record scope through the car protocol, then add the
pure join and API evidence, then replace the readiness sentinel and
render it. Registry publication is append-only; conversions and legacy
backfills are recorded. Tests cover changed heads, tenant identity,
ownership changes, duplicate scope, retirement, failed or pending proof,
corrections, incomplete reads and zero obligations. Delivery requires a
machine-run API probe demonstrating both a valid association and that
unknown or failed evidence cannot pass. This fold supplies the contract,
not its implementation or live proof.

**A proof is a claim on an event, with a lease, and a car whose change
nothing in production can print closes on its landed-review.** Design
`ada8715e` (self-critique pass H3), its one question accepted on
2026-11-02, measured the proof loop as the factory's largest consumer of
ops-requests: 1,864 run-car-probe requests in nine days, 1,703 of them from
the hourly recheck, six cars accounting for 1,111 rechecks of probes
waiting on an event that had not fired; 62 landed cars held open at
`proven`, 44 of them carrying no probe at all and so unable to close by
rule. Decided: one record on the car, written at park, `proof = { class,
probe, expect, waits_on, attempts, lease_until }`. The **class** is
declared at park and pinned by a lint: `behaviour` (a probe that prints its
expectation on the deployed build, declaring the host it runs on, which
the park refuses if that host cannot run it), `absence` (a deletion),
`guard` (a refusal of something real), or `review` (a test, pin, doc or
verb-doc change, proven by the landed-review's byte-identity reading). A
rerun fires when the event in `waits_on` fires, never by a clock; the
hourly recheck goes. When `lease_until` (park time plus the car's
`duration_hours × k`) passes without a green, the car reaches a terminal
`unproven` with the last diagnosis and its item reopens. `attempts` is the
one record every path writes, the arrival run, an event rerun, a person,
the independent prover and the owed-proof payment alike. `boss prove`
stays the one judge.

**An attended browser observation is evidence, and it qualifies a claim
only through an independent typed judgment, never as a `boss prove`
receipt.** Design `ec6fc29a`, its one question accepted on 2026-10-15,
met a claim only a signed-in browser can observe: the deployed question
renderer, whose gateway reads need the person's HttpOnly session. A
browser can run checks, but the server cannot know from a submitted JSON
object that the JavaScript really ran, and the step API checks a proof
field's shape, not who executed it. Decided: a separate versioned
`browser-renderer-observation` protocol and StepPlugin, with no change to
the renderer, gateway authentication or roles. The person's browser
fetches the served asset over its own session, refuses a redirect, a
wrong path, an oversize body or a digest that differs from the pinned
contract, and runs the exact bytes against a fixed synthetic suite in an
opaque sandboxed frame that has no network and never writes to a live
packet. The person submits the whole record: renderer, suite and every
assertion, as attachments. The actor comes from the server's write
stamp, and no pass flag the client sends counts. A different executor
then runs `boss browser qualify`, which re-reads the originals, refuses
self-review and incomplete or mismatched evidence, and records an
`attended-browser-independent-review` qualification. `boss browser
accept` writes that qualification onto a car only after the car has been
explicitly converted to a `ship-a-change` version with the distinct
`attended-qualified` terminal. The car's `proven` step and its evidence
stay as they were. Had the question been rejected, the observation would
have stayed evidence only until a trusted browser executor existed;
exporting the human's session to an agent was never proposed. Built and
published (the observation row at v2, `ship-a-change` v16 carrying the
terminal; runbook
[docs/runbooks/browser-renderer-observation.md](runbooks/browser-renderer-observation.md)),
but no observation had been filed as of 2026-11-13, so no renderer has
yet been qualified this way.

**The dev pod's work volume grows in place, through a door of its own,
and never at the cost of the live session.** Design `9687bcaf`, its one
question accepted as proposed by David on 2026-10-15 (urgent parent
`31d76e37`). `/work` held about 5.1 GiB against a 12 GiB preflight floor
and an 18 GiB reclaim target; a reclaim pass had removed nothing, because
what filled it was locked, dirty or sole-copy work, and deduplication
could have freed under 1 GiB. Decided: grow the claim rather than delete,
move or archive sole originals. `boss-dev/boss-dev-work` gets a
fixed-purpose pair, the read-only `plan-dev-work-volume-expansion` and
the mutating `expand-dev-work-volume`, and the instances-only
`expand-instance-volume` keeps refusing `boss-dev`. The pair takes no
namespace, claim or command from the caller, only a whole-GiB size, and
it only grows, at most 2x per operation. The plan pins the claim's UID,
its bound volume, the old request and the live pod and mount, and checks
every replica disk against both Longhorn's physical space and its
scheduling ledger. A missing or partial read is "cannot answer", not a
plan. David approves that exact plan with his passkey through the
ops-request path, and approving the design never stands in for it. The
write is one PATCH to the claim's storage field, conditioned on its UID
and old request. No restart, replica move, recreation, new grant or
credential is allowed; if growth needs any of them, the door stops and
a separate maintenance proposal follows. Proof is the same mounted
filesystem showing more than 18 GiB free, with the pod and session
processes unchanged. A pending resize or a timeout stays unproven and is
never retried or answered by a shrink. The ceiling was 64Gi; correction
`[0]` on the review (2026-10-28) moved it to 65..128Gi, which design
`cab8f1cd` below settled. Built: both verb rows and
`infra/forge/dev-work-volume.sh` (2026-10-15 to 2026-10-28), and the
claim in `boss-dev.yaml` is declared a `floor`, so a converge never asks
for a shrink of a claim that has grown past its manifest. The parent
closed on 2026-10-18; whole-hash conservation of the records on the
volume is still unproven, and the verb's receipt says so.

**The dev-work ceiling follows what the estate can place, and the
worktrees that fill it are bounded.** Design `cab8f1cd`, three questions
decided as proposed on 2026-10-22, found `/work` under its 18 GB floor at
the 64Gi ceiling of the expansion door (9687bcaf), with 555 live worktrees,
220 of them harness locks left by builder runs. Decided, both in order:
the ceiling rises to **128Gi** (the 64Gi bound encoded an estate that
could not place larger replicas; the plan verb admits 65..128Gi on main);
locked worktrees are bounded at **40 per actor**, the oldest closed-run lock
released first; and a detached head no ref holds is reclaimed after **7
days**, its commits first saved under `refs/reclaimed/` so removing a
checkout never loses work. Measured by `/work` free space staying above
18 GB on each reclaim receipt for seven days.

**A volume claim is retired by a passkey-approved verb, never by hand.**
Design `b4b1c99d`, filed with no open questions; David decided it on
2026-10-18 on the design-review of backlog `f36eb7ce`: "manually running
any ops command is a gap in BOSS coverage." Until then
`delete-orphan-object` kept PersistentVolumeClaims out of its kind floor
as a named human step with the admin kubeconfig, because a claim is
bytes. That floor is unchanged; the claim gets a door of its own, shaped
like the dev-work expansion above. The read-only
`plan-a-volume-claim-retirement` renders one named claim (uid, size,
class, its volume and reclaim policy, the Longhorn volume) and refuses by
name a protected claim (`pgdata-postgres-0`, `boss-dev-work`, in any
namespace), a claim `undeclared-objects.sh --check` does not call
undeclared (an unowned namespace, a declared or controller-owned claim),
a claim being deleted, and a claim that any Pod, Job, CronJob or
workload controller in its namespace mounts or would stamp again. David
signs those plan bytes with his passkey (design `17835005`). The mutating
`retire-volume-claim` re-renders, acts only when the plan still hashes to
the signed value, attaches the PVC, PV and Longhorn YAML to the
ops-request and reads it back before deleting, deletes once with
`preconditions.uid`, and prints its effect only when the claim reads back
gone and the volume reads back gone (Delete) or kept (Retain). One claim
per request, and a second run of an applied plan refuses. Built on
2026-10-18 (both rows in `infra/ops/verbs/`,
`infra/forge/retire-volume-claim.sh`), and the registry refuses an
approval verb that names no `plan_verb` (`boss-jobs/src/ops_verbs.rs`).
`retire-access-object` is its sibling for ServiceAccounts and grants.
Not yet used: on 2026-11-13 no ops-request had named either verb, so the
first use the design named, the eight leaked `dev-build-*` worker claims
of item `2346fe79`, did not go through it.

**Agent evidence on `/work` is archived losslessly or kept, never
expired.** Design `195276f0` was filed with no open questions, and David
approved it on 2026-10-17 on the design-review step of backlog
`25022367`. On 2026-10-16 `/work` reached 96% with 1.7 GiB free. The
hourly reclaim correctly kept 486 dirty, locked or unreferenced
worktrees, and it had no lifecycle at all for the scratch, session and
transcript files agents leave behind. Decided: the lifecycle is the
existing reclaim sidecar's hourly pass reading a versioned data policy,
not a new service. There is no destructive expiry, and no transcript is
ever truncated. The one action is lossless compression of completed
evidence. The plain copy is released only after the decoded archive
matches the original's full digest and an fsynced, append-only manifest
records it. A reader that opens a literal path gets it back through a
verified restore. An unknown policy, owner, liveness, reader or reserve
retains, under a named reason. Completion is read from the record (a
closed, reported run that no open packet names), never from age, a
filename or a quiet process table, and a missing heartbeat is not a dead
writer. Session and transcript stores stay unsupported until their real
readers are measured. Activation is a separate act: exact-head review, an
adversarial review (the item is a `mutating-verb` trust boundary), and
native acceptance of one bounded pass.

Built, on origin/main 2026-11-13: the policy
`infra/cluster/artifact-lifecycle.toml` (version 2, four families,
owned by platform-admin) and `infra/cluster/dev-artifact-inventory.py`.
The hourly pass in `dev-scratch-reclaim.sh` runs it as a dry run that
accounts every file by family, unit and retaining reason, beside the
mount's reserve and shortfall. Also built: the agent-run completion join,
the archive actuator (each file revalidated under one exclusive
lifecycle lock, and an interrupted pass recovered by the next), and the `--restore PATH` door. Its
loader refuses an action on a reader other than `plain-path`, an action
without a join or a per-pass byte budget, and any action but `archive`.
Not active: no family carries `action = "archive"`, so the mechanism has
archived nothing, and its FILESTORE seam is unselected. Not built: the
exact deduplication the design permits, and the disk incident naming
what the inventory retains, because no alarm reads its report yet.
Backlog `25022367` stays open for the activation.

**The estate registry is the hardware inventory, and hardware operations
are protocols, not runbooks.** Design `60a8c155` (David, 2026-10-24: "I
don't think we have an accurate understanding of all the physical
hardware"), three questions decided as proposed the same day. The live
registry held seven machines with only their first disk, no CPU, memory,
GPU, NIC, firmware or location, and no alarm comparing declared with
measured. Decided: read-only inventory probes per machine through existing
read doors merge into the registry rows with a declared-vs-measured diff,
and the declared-vs-measured alarm files drift itself; what probes cannot
discover (workstation, NAS, UPS, switches and router, spares, cloud
accounts beyond boss-gcp) is David's to list, with where it is. Hardware
protocols are built as Workflow rows in this order: capacity response
(each capacity alarm names its door, and a missing door is the build),
snapshot hygiene, disk replace/add, node join/retire, firmware/OS upgrade,
failure response. The approval boundary: reads and bounded reclaims run
unattended; anything that resizes, moves, removes, reboots or upgrades
needs David's passkey on the website; physical acts (cabling, a disk swap)
stay his, with a checklist the protocol prints. In the station tree
(4b27a65a) the estate is the root station's pools.

**The offsite backup is one versioned bucket, and the old receiver is
gone.** Design `1511f7d4` (David, 2026-10-27, both questions yes): the
boss-gcp SSH dump receiver, unused since the GCS bucket `boss-gcs-backup`
became the only offsite copy on 2026-10-01, is retired (its receiving
path, `authorized_keys` line and Secret) through a passkey-gated ops verb
that reads back their absence; and the bucket gets object versioning with
a lifecycle rule deleting noncurrent versions after **30 days**, through a
passkey-gated verb that reads back the state, so a buggy prune or a
compromised writer key makes noncurrent versions instead of destroying
every offsite copy at once.

**At most one LIVE worker per run, with a bounded, recorded chain of
re-admissions.** Design `4e79c324` (backlog d26832f7), its one question
accepted on 2026-10-29, relaxes the isolation invariant of reviews
b831d89e and 7dd185c6 from "one worker per run's lifetime", under which a
single Kubernetes read timeout ended a run for good (five runs lost on
2026-10-29). A run binds a new worker only after its old Job's retirement
is acknowledged by UID (a 404 counts as confirmed), only when no live
binding for its tree remains, only for the same tree, only for a
re-admittable cause recorded as `cause_class` (`api-transient`,
`storage-placement`, `capacity`, `source-moved`; integrity refusals and an
absent source stay final), and at most **twice**. Each re-admission is an
append-only record (`run-worker-<run>.readmit-<n>.json`, written link-once
so two racing controllers collide rather than double-allocate) in the
controller store and on the agent-run as `worker_readmissions`, and stop,
ensure and ownership follow the chain to the newest session.

**PostgreSQL moves to its own node on power-loss-protected disks; the
cloud is the off-site copy, never the primary.** Design `f29798c0`
(answers ba0b7308), three questions decided by David on 2026-10-31. The
disk: David bought a Dell Precision 5820 with two Micron 7400 PRO 960 GB U.3
drives and PCIe adapters, to be the database node (the placement and
timing are in design 3c475a19 and the hardware plan on 121f44a1). The
durability tier is **D-a only**: WAL archived to the existing GCS bucket
through pgBackRest (RPO about 60 s, restore 15–60 min), accepting slower
recovery to save cost; the cloud standby (D-b) and an in-LAN streaming
standby (D-c) are not taken. The primary does **not** move to the cloud:
the measured 51–57 ms LAN-to-GCP round trip would add 0.2 s to a read and
up to a second to a write, and a WAN outage would darken the system of
record and its alarms.

**A node's install disk is declared by what it is, never by kernel name.**
Design `ae40927c` (backlog 25d682bd), its one question accepted on
2026-10-31: w-1's two NVMe disks are exactly the same size, so only the
model tells them apart, and a reinstall applying a config that names
`/dev/nvme0n1` after enumeration flips would install onto the other disk
and wipe the gate volume. node-converge may now write one more Talos key,
`machine.install.diskSelector`, as a read-and-write class of
`check-declared.sh` limited to the literal selector fields `model`, `type`,
`size` and `busPath` (never `disk`, `wipe`, `image` or `extraKernelArgs`),
so `w-1.yaml` declares its system disk by model and every apply remains a
passkey-signed plan. The selector is the exact model, not a glob, serial
or bus path, and an install that matches nothing refuses rather than
picking a disk. The design's other decisions, made from the frame: no
door reads machineconfig, redacted or otherwise; `talos-get` gains the
read-only resource `systemdisk`; and the upgrade verb (1695feb6) refuses
unless the systemdisk's model equals the declared one, so an upgrade
proves the disk it will touch before it touches it. The rule applies to
every node that gains a second disk.

**A gate's PostgreSQL data directory is memory, never the system disk.**
Design-doc `3a8a44c1` ("Using both of w-1's drives properly: the gate's
Postgres leaves the system disk, and three choices that are yours", David's
answers 2026-10-07) measured that a gate's test check barely wrote the gate
volume and wrote w-1's system disk instead, because each gate's PGDATA was
an emptyDir on EPHEMERAL beside the builders. Built on 2026-10-08 (backlog
9cd74fd5): the `pgdata` volume in `infra/gate-runner/gate-runner.yaml` is
`medium: Memory` with a size limit, which loses nothing because the
sidecar runs with fsync, synchronous_commit and full_page_writes off, and
the test `the_gate_postgres_data_lives_in_memory` pins it. The read-only
disk report its step 1 asked for exists as the `node-disk-usage` verb
(backlog ebcddea5). The rest of the doc was overtaken. Per-drive latency
is the on-demand `talos-read-diskstats` verb and is never an estate field
(f29798c0). The scratch-reclaim item b6d23b1b closed stale on 2026-10-19.
The Q new-drive "not yet" gave way to 121f44a1 and 3c475a19 below: the
gate volume has been on the Micron 9300 PRO since 2026-11-11 (window
ba305b66). The Q four-bays rule gave way to 121f44a1 D2, and the count is
`bays` in `infra/platform/stations/gates.toml`. The Q who-yields rule was
never built: the `gates` station's reservation of 12 vCPU of `host:w-1`
does that job.

**A gate ends on its verdict; the seed refresh runs after it, beside
the record and never in it.** Design `b4bf8bba` ("Separate judged gate
completion from supervised seed refresh", both questions approved as
proposed by David on 2026-10-16; backlog 3e2c8ddc, with 82ffaff3 folded
in) measured a green that waited 638 s for its own seed copy (gate-run
86dd2d48), because the conductor records a verdict only from a `gate`
container that has ended. Built and live: `infra/gate-runner/gate-runner.yaml`
labels the Job `boss.dev/gate-layout: judged-init` and runs `gate` as a
one-shot init container after the postgres sidecar, then a regular
`seed-refresh` container that the kubelet starts only after the gate
ended 0. That container runs main's `infra/gate-runner/seed-refresh.sh`
from the ConfigMap, holds no credential, and takes one line of a fixed
shape from the gate (`refresh <40 hex>` or `skip <word>`). It refreshes
under the exclusive non-blocking seed lock: a busy lock skips, nothing
queues, and the copy is staged and then renamed. Each phase is bounded
at 900 s plus a 30 s grace, the helper always exits 0, and the
conductor writes its record as `seed_maintenance` on the closed
gate-run (`recorded`, `not-run` or `unknown`, never guessed), never
into the receipt and never as a strike
(`boss-cli/src/train/carried_verdict.rs`). The recorder reads `gate`'s end
from the status list its pod spec declares it in, exactly once, and the
old layout stays readable. Two parts of Q2 are not as decided. The
bounds are env values in the manifest, not rows in the delivery-policy
registry. And the bays are unchanged: a Job holds its bay until it
finishes, helper included (`running_gates` in `boss-cli/src/gate.rs`). So
the verdict arrives earlier, but the next gate starts no sooner.
Admitting one while a refresh runs still waits on the node-headroom
proof Q2 asked for. Rollback deletes that label, `GATE_LAYOUT` and the
helper, and moves `gate` back under `containers:`.

**Emptying a gate's workspace will have one supervised owner of its
own; no gate waits for it.** Design `bb75d192` ("Give gate workspace
cleanup a separate supervised owner"). David approved its one question
as proposed on 2026-10-17. It measured a red gate whose receipt was
written 17 minutes before its container ended, because that container
was still deleting its workspace in uninterruptible XFS I/O. The gate's
workspace is a persistent per-Pod directory on a Retain volume, not an
emptyDir. The decision is one serialized owner limited to that
volume's runs directory. The container that holds the verdict keeps its
full evidence and receipt, leaves its workspace pending, and exits with
its real verdict. It starts no detached child. The owner deletes a
directory only with proof from the trusted recorder: the gate ended
natively, and its full evidence was durably retained. A file in the
branch-writable workspace cannot supply either proof. Before each batch
the owner checks the directory's identity and the original `.alive`
inode and takes that lock. It keeps anything live, young, replaced,
symlinked, ambiguous or missing proof, and an interrupted batch stays a
named, resumable, incomplete operation. No deadline can bound a syscall
already blocked in kernel I/O. The registry owns the batch, age and
headroom policy. Cleanup backlog, bytes and inodes, judging slots and
node headroom are counted separately, and judging concurrency is
unchanged. Any scoped credential comes from the broker. The approval
itself ran no cleanup and granted nothing. Activation still needs a
trust review of the exact head and native proof of conservation,
headroom and lifecycle. **Not built** as of 2026-11-13, and no open item
carries it, because item 82ffaff3 closed on 2026-10-17. Three deleters
remain in `infra/gate-runner/run.sh`. A red still empties its own
workspace in its EXIT trap (`empty_workspace`), whose
`find -mindepth 1 -delete` also removes the `.alive` it holds locked. A
green on the judged-init layout has `seed-refresh.sh maintain` empty it
under `timeout`, which cannot bound a stuck disk. Every gate's start
runs `sweep_dead_workspaces` on dead runs idle for an hour. The design
collapses that sweep into the new owner and keeps synchronous cleanup as
the named rollback.

**The gate's limit was one drive, and the fix is a drive fit for the
writes.** Design `121f44a1` (backlog 739d417d) measured on 2026-11-01 that
the three gates sharing w-1 were bound by the gate volume alone: a
DRAM-less Kingston NV3 100% busy at 60 MB/s with 854 ms mean writes,
taking about 3.4 TB a day against a 320 TBW rating, while CPU, memory, the
install disk and Longhorn were idle. David decided (Q new-drive, 2026-11-01)
not to pull drives from w-1 but to add a Micron 7400 PRO 960 GB in a PCIe
slot as the gate volume, with a Dell Precision 5820 coming to run the
database on its own node. As carried out: on 2026-11-07 the gate volume
moved to a Micron 7400 PRO on w-1 through the volume doors with no reboot,
and the Kingston was left blank. The gates stay at two seats until the new
drive is measured.

**Each drive carries what its endurance and durability fit, selected by
what it is.** Design `3c475a19` turns David's approved hardware shape
(recorded on 121f44a1, 2026-11-01) into a placement, filed with no
questions. On w-1: the Micron 9300 PRO 3.84 TB becomes the gate volume,
selected by size (> 2 TB); the Kingston NV3 becomes Longhorn `scratch`, for
single-copy dev-build worker volumes only, because it is rebuildable, not
because it is spare; the Samsung 980 PRO stays the system disk and a
`durable, postgres` Longhorn disk. The Dell joins as worker `w-3`
(`boss.dev/purpose=database`, never a build node or a control plane), its
first 7400 PRO the system of record's data volume and its second a durable
Longhorn disk. PostgreSQL runs on **two** replicas on the two best disks
(w-3 and w-1's 980 PRO), pinned by `diskSelector: [postgres]` and node
affinity, after a restore of the newest dump into a scratch database has
been compared with the live record. Durable volumes are placed by tag
**before** any scratch disk exists, Longhorn leaves the control planes by
eviction and scheduling is then closed there, and two new passkey doors
exist for it (a user-volume wipe and a Longhorn disk-tag write). The
registry, the Talos patch and the Longhorn classes say so before anything
is fitted. The order is five cars: gate volume, Kingston to scratch, the
Dell joins, PostgreSQL to the Dell, Longhorn off the control planes. The
interim of 2026-11-07 (the gate on a 7400 PRO, pending the 9300 PRO) is a
step on the way, not a change of placement.

**A node-local volume moves between two fitted disks on one protocol,
`move-a-node-volume`.** Design-doc `558ca6d7` ("Move a node-local volume
between two fitted disks as one protocol: move-a-node-volume", answers
backlog e581f615), its one question decided by David on 2026-11-11. Every
door such a move needs already exists: `boss gates hold` / `release`,
`retire-a-user-volume`, `wipe-a-partition`, `node-converge` (volume-only
patches since a93010a1) and `prove-gate-volume`. What was missing was the
order of the doors and the evidence each one owes. The 2026-11-07 move off
the Kingston was recorded as free-text keys on 88b35891, and only the
operator's session knew which door came next. The move is a Workflow row,
not code. It is also not a step of `node-maintenance-window`: that
protocol powers a node off for hands-on work, and this one powers nothing
off. Where a window fits a disk beside the old one, the window ends at
`declare-capacity` and a `move-a-node-volume` packet does the move. Its
steps are declare, hold, retire, wipe, converge, prove, resume and
verify-work. Each step completes with its ops-request id and the request's
effect line, copied rather than retyped. When a step completes, a
dispatcher rule files the next request, so David signs from the website.
**declare** refuses a target selector unless exactly one fitted,
non-system disk matches it: which disk Talos picks for a two-way match has
not been measured. When two fitted disks share a model, the selector names
the target's serial. There is no data-copy step, because the gate volume
rebuilds cold; a volume whose data cannot be rebuilt is out of scope for
this row. A failure after the wipe leaves the station held and the packet
open at the step that failed. Node, volume, station and proof verb are
packet metadata, but declare refuses a volume whose workload has no proof
verb, so today the row moves only the gate volume. **Three passkeys per
move (Q1):** retire, wipe and converge each keep a signature on their own
plan. The wipe plan and the converge dry-run read node state that exists
only after the step before them has run, so one upfront signature would
sign intent rather than bytes (design `17835005`). Not built as of
2026-11-12 (origin/main `50f9be981`): the row and its dispatcher rules are
one car, carried by e581f615's `build` step. The first move between
fitted disks after that car lands runs on the row, and its converge is the
first live run of node-converge's repaired volume-only patch.

**An ops-request records whether its verb ran apart from how it ended,
and nothing reruns it.** Design `b7bb1625` (answers backlog `50a1578f`),
its one question decided by David on 2026-11-17 as proposed. Ops-request
`02035ff0` executed a verb that failed a rebuild after 1,204 seconds with
`exit_code` 1, and closed on the `answered` terminal with `outcome_kind`
completed: `infra/ops/ops-runner.sh` writes `answered` for every verb it
invoked, whatever the exit, and the row forks on that disposition alone.
Relabelling such a run `refused` would falsify provenance, because the
refusal contract says nothing ran (§Policy & auth, "A destructive change
is approved by the founder's passkey"). Decided, as a new versioned
contract: **refused means nothing ran**, the runner having declined
before invocation, and a refusal keeps its reason and its `aborted`
outcome; **an executed nonzero exit closes `failed`** (`outcome_kind`
failed); **an invoked verb with no trustworthy exit closes
indeterminate** (`outcome_kind` failed, reason unknown execution
result), and missing execution evidence is never turned into a claim
that nothing ran; **neither failed nor indeterminate ever reruns the verb
automatically**; and **a zero exit closes `answered` and keeps its
separate effect qualifications** (`effect`, `effect_unproven`,
`effect_unread`), because answered is execution success, never proof of
the requested effect, and the surface shows the qualification beside it.
One pure classifier over the credentialed execution record serves the
writer and every reader, with no second stored copy of `exit_code`; an
unknown, malformed, signal-like or timeout exit keeps its raw value and
cause, and 124 or 128+N alone proves no timeout or signal without the
wrapper's evidence. **Readers land first**: job and outcome rendering,
ops wait, the failure judges, signals, teardown, move diagnosis and
recorded proofs agree on failed and unknown, legacy pinned requests
included, from a consumer inventory taken before the code. The runner's
classification follows, and then **a new ops-request version, published
held, activates only after an independent adversarial review and
David's existing protocol-publish approval**. Old pinned requests stay
valid and immutable: no automatic repin, no rewrite of a completed step,
a legacy request that answered on a nonzero exit renders failed from its
execution record, and any explanation of one goes through
`boss correct`. The change widens no runner authority, replaces no
approval, and repairs no volume. Decided, not built: on origin/main
`24d3c02d5` ops-request's terminals are still `answered`, `refused`,
`not-released` and `nothing-to-do`, and the runner still writes
`answered` for every invoked verb.

**Hardware is proposed by a weekly capacity review, and only when the
hardware we have is used well.** Design-doc `27c94d85` ("Capacity review: a
weekly protocol that proposes hardware only when the hardware we have is
used well"), filed with no open questions on 2026-11-10, after that day's
hardware was judged by hand in chat while the cheapest win of the day cost
nothing (worker volumes sized from 100Gi to 40Gi from a measured 17.9 GB
peak, b3ad4219, fitting 6 builders where 2 had fitted). David's four
decisions (2026-11-10): (1) **weekly** — a clock rule files one
`capacity-review` packet a week, because needs can accelerate suddenly;
(2) **David approves every purchase** — BOSS holds no spending mechanism,
and the available seed balance (1,000 USD put into the company Stripe
account that day) is a search constraint no proposal may exceed, read from
the ledger and never typed, so further seeding or sponsorship revenue
raises it without an edit; (3) **the purchase is a human step on David**,
with a step plugin where he views the approved item and records vendor,
order reference, price paid, expected arrival and serials; (4) **cloud is
evaluated, never provisioned** — the scan may price it as a comparison
only. The protocol runs measure (the needs report from the record, naming
ONE binding constraint with numbers), use-first (when software or
configuration on hardware we already have can fix it, the review files
backlog items and ends with no proposal), scan (candidates as structured data, judged against role
requirements held as registry rows), propose (ranked, or none, with the
expected change in the constraint and the total cost against the balance),
decide (David), purchase (David), place and bring up (the estate registry
first, then the existing doors, never by hand) and prove (re-measure the
constraint measure named). The price paid posts to the ledger as an
expense, so how much is left is a figure the next review reads. Not built,
as of 2026-11-12: its five cars — the Workflow row and its clock rule
(backlog f75cebe0), the role-requirements registry (024584bc), the
needs-report read (82ea2ecc), the purchase step plugin (eea00a8b), and the
ledger posting with the balance read (b77c52c1). Backlog 3c08f3a1, the
one-off acquisition protocol, closed as its duplicate.

**A failed roll holds the converge by itself; hold-converge stays David's.**
Design `b2e778ec` (item 78625ebd; fix car 1741c487 landed in #1509) is the
post-mortem of the two system-of-record outages of 2026-11-01, when a
`CREATE INDEX CONCURRENTLY` on the boot path was cancelled by its lock
timeout and the watchdog rolled back. Its one question was accepted on
2026-11-02: `hold-converge` keeps David's authorization (the same as
`rollback-to`); instead **the converge runner holds itself** after a failed
roll, because its precondition is already machine-readable (a converge
packet with `failed_stage=roll` on the head about to be re-rolled): after
N init failures of one build it rolls back to the named last-converged
build and refuses to roll that head again until the item it files is
closed. No new authority is needed, and release stays an operator act or
clears when a later head converges. The protocol gains, as cars: the
phase pin flipped so every `CONCURRENTLY` file is post-start by
construction and boot-wait is the two historical files only; a gate pin
that applies the real schema's boot phase while a long snapshot is open
and fails if boot overruns its budget; long transactions as a registry
row whose measured durations are the pin's budget; actor alarms that hold
or name a recorded dark window as their cause; and a boot that stops at
the first named failure of a no-transaction file and puts the cause on
the converge packet.

**The interactive foothold is separate from the build environment.**
Design `0491591b` (backlog 47601829), after the dev pod sat Pending for
about 80 minutes during the w-1 drive window of 2026-11-06/07 and David had
no shell inside the LAN while everything else was up. A tiny, always-present
`boss-dev-standby` Deployment (well under 1 CPU and 1Gi requested,
required off the control planes and off the dev pod's node, no `/work`
volume, a shallow clone from the forge, never building) carries sshd, the
`boss` shim, `boss-api`, kubectl and git, and its login banner prints the
dev pod's phase, node and last scheduler event before the prompt. It is a
door of its own (`dev-standby-ssh` in the estate doors, probed by the door
observer, offered on the login menu). David's two decisions on 2026-11-07:
the standby carries **the dev session's ServiceAccount and machine token
but NOT the forge write token** (it reads and files, it never pushes), and
it gets a **public hostname**, `standby.algedonic.dev`, behind the same
Cloudflare Access policy as the dev door, because the case it exists for is
being outside with the LAN door dark. A lint pins its anti-affinity and its
small request.

### Gates: what a gate runs, counts and keeps

This sub-section and the three after it fold twenty-three design-doc
packets that were decided and in force but absent from this record
(fold batch F3, backlog `9f92cf41`, design `25511015` Q5, 2026-11-17).
Each paragraph cites its packet and says what is built, measured on
origin/main `19371b9fa`; "not built" is a decision in force whose car
is still owed. A design "decided from the frame" asked David nothing,
and its review step carries no questions.

**A gate runs the run.sh of the tree it gates** (design `1d7a1669`,
David 2026-09-28, no questions; backlog `e3100bc8`). run.sh reaches the
gate pod as ConfigMap `gate-runner-script`, which the forge converge
writes from main, so a run.sh car was gated by main's old script and its
change went live only after the converge that followed its merge.
Decided, shape C: the ConfigMap carries a fixed bootstrap that resolves
the branch once to `GATE_SHA`, fetches that sha's
`infra/gate-runner/run.sh` into a directory outside the clone and execs
it; run.sh gates `GATE_SHA` rather than the moving head; the receipt
records `runner_source` and `runner_sha256`; and a failed fetch execs
main's copy as `runner_source=main-fallback` with its reason, never
silently. Rejected: a per-gate ConfigMap (RBAC, cleanup and the
undeclared-object sweep changed to move one file) and the script inline
in the Job spec (87 KB of `$`-laden text in a textually rendered
manifest). The trust boundary is the pod spec, not the script: branch
code already runs `gate.sh`, every build script and every test in the
same container with the same token mount, and already writes the
receipt, so running its run.sh adds no capability; the hardening that
would move the boundary is filed as `ba255e0e` (a token-free check
container) and `2710c8fc` (a verdict branch code cannot forge). Built:
car 1, `infra/gate-runner/bootstrap.sh`, both ConfigMap keys written by
`cluster-deploy-runner.sh` and `apply-script-configmap.sh`, and
`runner_source` on the receipt. Not built: car 2, which points the Job's
command at the bootstrap; `gate-runner.yaml` still runs `bash
/repo-script/run.sh`, so every gate still runs main's run.sh.

**The gate bound counts every gate, and a waiting train goes first in
line** (design `1e787fc2`, David 2026-09-28, no questions; backlog
`0f4224bc`). A train was admitted at the bound (backlog `48f7aba1`), so
with the bound raised to four the real ceiling was five, the measured
I/O cliff, and a 7-minute train gate took 24. Decided: the bound is the
total of running gate Jobs, trains included, and nothing is admitted at
it; a waiting train holds the first place, ahead of the dock's re-gates
and every builder place, so the bay that frees while it waits stays
free for it; no reserved train bay, which would idle three quarters of
the time, and no new policy field. Built: `admits()` in
`boss-cli/src/gate.rs` (`live + ahead < max` for a car, `live < max`
for a train, every car counting waiting trains ahead of itself). Two
things have moved since. The train's claim is read from
`train_gate_bound_wait`, not `train_gate_wait_reason`, which the
conductor writes on every launch error (backlogs `82071657`,
`812a6ed8`). And the bound is no longer `gate_max_concurrent = 4` but
the `gates` station's capacity, with train, dock re-gate and car gate
as classes of one claim (design `d996ce3d`, "Gate admission is a claim
on a pool" above); the four is the number David chose that day, not
today's.

**A gate's complete output is kept before anything reduces it**
(design `5a274e7c`, David 2026-11-13, Q1 accepted as proposed; backlog
`7c49bd38`). gate.log died with the workspace, the receipt kept at most
24,000 characters of excerpt, and the pod log that held the replay
lived one day, so retro W42 lost the causes of 33 gates. Decided: (1)
the runner frames the whole gate.log, gzipped and hashed, onto stdout
before the failure extractor runs (`runtime-evidence.py retain-log`,
bounded at 4 MiB of gzip, past which it says `state=partial` with the
whole original's size and hash) and binds the same facts into the
receipt as `log_retention.gate_log`; (2) the conductor, the only side
that may write, verifies the frames against that binding, attaches the
verified gzip and the outer pod log to the gate-run's verdict step in
the file store and records `log_retention` per stream, never at the
cost of the verdict; (3) excerpts stay as views naming the `file_id` of
their original; (4) one reader, `boss gate log`, verifies every hash
before it prints; (5) every verdict is kept, 30 days for green and 180
for the rest, expired through the store's own delete. Q1: a retained
log is readable by whoever may read the gate-run packet, with no
redaction pass; a secret found in one is a defect against the check
that printed it. Built: car 1 (`retain-log`, pinned by
`gate_log_retention.rs`) and the verification half of car 2
(`boss-cli/src/train/gate_log.rs`, recorded beside the verdict). Not
built: the attach to the file store, `boss gate log`, the excerpts'
`file_id`s and the expiry; the verified bytes still live only as long
as the pod log.

**A pre-flight leaves a receipt that outlives the context that ran it**
(design `141be047`, 2026-11-13, decided from the frame; backlog
`3ef5fe30`). A pre-flight already runs in the run's worker through
`dev-build exec-auto`, which writes an `.exec-` receipt with argv, the
trusted transport's exit and the full output, but only after the
command returns. Decided: (D1) a `started` record is written before the
transport starts, so a killed controller leaves an absence that can be
told from a loss; (D2) SIGTERM and SIGHUP write the terminal receipt as
`interrupted`, outcome unknown, and a worker-side exit is never
trusted; (D3) both records name the head the command ran on and keep
the admission sha as `admitted_head`; (D4) one reader, `dev-build
receipt --run`, reports each record as observed, interrupted, active or
lost, and `boss gate` records the newest pre-flight receipt at the
gated sha on the gate-run as `preflight_receipt`, never refusing on it,
because a pre-flight is not a gate. Not built: `infra/dev/dev-build.py`
writes no started record and installs no signal handler, and nothing
under `crates/` names `preflight_receipt`.

**A probe rule that tightens is swept by a verb, not by a rule off the
merge** (design `acadeac5`, 2026-11-14, decided from the frame; the
design half of backlog `a8f353fd`). A recorded proof stays pinned to
the predicate it was judged under: a changed rule is a fact about the
rule, and the record keeps the proof with any correction beside it.
`boss prove --sweep [--since]` reads every car carrying a recorded
probe, pages to `.total`, judges each against the current shape
predicates (the same functions, no second copy) and prints each refused
car that carries no correction naming the predicate; it writes nothing,
because a correction needs a fresh measurement. A car that changes a
probe-shape predicate runs the sweep before it parks and puts the count
and the cars in its park text. Not built: `prove.rs` has no `--sweep`;
the one sweep so far was by hand, for item `002f5f31` (1,855 cars, 14
found, three corrected through `boss correct` on 2026-11-14).

**A forge fake refuses a wrong credential** (design `ef6394c8`, David
2026-09-26, no questions; backlog `016f5a71`, the blind spot of
incident `a65ba21e`). Two cars in one night passed every test against
a local bare repo that accepted any request, then failed against the
real forge, one of them printing its token into the forge journal.
Decided: (D1) one shared fixture, `boss_testing::forge::AuthForge`,
serving smart HTTP through the gate image's `git-http-backend`,
answering 401 to no credential, a wrong token, a wrong user or a mode
the test did not enable, running the code under test with an empty git
config and no prompt, recording every request, and asserting the
secret absent from the output; it panics rather than skips when the
backend is missing; (D2) a ratchet lint, `forge-fakes-authenticate.sh`,
refusing any crate file that fakes the forge with a bare or `file://`
remote unless it uses the fixture or sits on an allowlist that can only
shrink; (D3) the fifteen existing sites migrate in three cars. Not
built: there is no `forge` module in boss-testing, no lint and no
self-test.

### The dock and the trains

**The dock refreshes between departures** (design `42279fb2`, David
2026-09-25, no questions; backlog `4890165b`). Re-gates launched only
inside `board`, once a window, against the main the departing train was
about to replace, while builder gates re-polled every 30 s and took
every bay that freed: 42 of 58 left-behind rows read "waiting for a
gate slot". Decided: (D1) a `refresh` verb on its own cadence rule,
`train-dock-refresh`, runs boarding's own per-car judgement and
launches the re-gates owed, only while the track is clear; (D2) a
departure waits, bounded by `regate_hold_minutes = 15`, for the round
it started; (D3) dock re-gates go ahead of builder places at the bound.
Rejected: boarding a touched car and letting the train gate judge it
(backlog `969a1092` reverted), narrowing "beside" to exact paths, and
raising the dock depth. Built: D1, the `refresh` verb in `cadence.rs`
and the rule, now version 3 at five minutes because at two a pass over
ten cars held the conductor lock almost continuously; and D3, now the
class order of the gate claim. D2 is retired: the board stopped waiting
on re-gates (backlog `96f02540`) and the boarding rule's v10 dropped
the field (backlog `d1d4275d`).

**A rule the tree declares active is republished, not left retired**
(design `e62240de`, agent-claude 2026-10-20, decided from the frame).
The live registry held `train-dock-refresh` v1 retired with no recorded
reason while the tree declared it active, and on 2026-10-20 four
gate-stale cars waited three hours for a bay. Decided: that is drift,
repaired toward the tree; the bundle's version went to 2 with nothing
else changed and was published through `boss cadence publish`, and the
gate's dock-claim accounting and the supervisor's `dock_waiting` read
stay as the guards against a dead rule. Built: v2 (backlog `f268e0a8`),
since replaced by the v3 above. The direction of repair later turned:
after bootstrap the live row is the authority and a tree edit a
proposal (design `942dc0ac`, under "A gate judges its tree" above).

**A change to a crate the car depends on stays the train gate's to
catch** (design `6af5acdf`, David 2026-09-26, no questions; backlog
`23fbe4e3`). Over 479 merged cars, widening the dock re-gate to
dependency crates adds 21 re-gates (4.4 points) to catch a class that
reddened 0 of 151 trains: a public rename compiles only if its own
commit edits every crate already using it, and the crate rule already
catches those. Decided (d): the dock rule is not widened, a pub-line
heuristic and a waited-too-long rule are rejected, and a released car
is judged like any other. Correctness is not traded either way, since
the train gate judges every assembled tree. Revisit when two train-gate
reds within 14 days fail `clippy` or `build` on a car file whose crate
main did not touch. In force as the absence of a change; the module
doc of `dock_regate.rs` ("What it does NOT see") still names the gap
without this measurement.

**A lint-only main change earns a delta receipt, not a full dock
re-gate** (design `7c4024d8`, David 2026-10-21, three questions
accepted as proposed; backlog `b245c9e7`). One changed lint staled all
25 cars at the dock, and eight comment lines in `gate.sh` staled 22; a
full re-gate is a median 42 minutes. Decided: when every gate input
main changed since a car's base is a lint under `infra/lint/` (not
`lib/`, not a consist-skip lint) and the car's diff is not scoped to
boss-cli or boss-testing, the changed lints run against the car's head
merged with main (Q2: one short Job on the gate runner image per
refresh pass, a gate-run of mode delta, ahead of builder gates and
behind a waiting train), and a green records `lint_delta` (head, main,
the gate change, each lint with its file's sha256 and exit), a key of
its own and never `regate_receipt`; any other gate input, or a red
delta, forces the full re-gate (Q1); and a `gate.sh` diff of blank and
whole-line comment lines outside any heredoc stales nothing (Q3).
`028ddab6` holds: every check main has ran over the boarding tree, the
changed ones by name. Built: the classification and the judgement in
`conductor.rs` `base_hold` and `dock_regate.rs`. Not built: the delta
runner, so a lint-only change is still re-gated in full with its lints
named.

**A car left behind on a conflict gets a machine-filed rerail**
(design `b35456ac`, David 2026-09-28, no questions; user feedback
`ff4ae3d2`). The conductor rebases before it records a conflict skip,
so every stamped skip is one `git rebase` could not resolve: the repair
is authorship, and on 2026-09-28 it waited for whoever read the stall
alarm. Decided: a `rerail-a-car` Workflow, one builder step with the
terminals `rerailed`, `refused` (naming the hunk, so a person decides)
and `overtaken`; a dispatcher rule spawning it when a car's `skips`
reads exactly 3 with a conflict reason and no repair is open on its
subject; `regate_patch` nulling `skips` on a re-park, so the count
means consecutive refusals since the last green; no new hold, because a
resolution carries everything any code change carries; and a close
rule ending the repair `overtaken` when its car closes. Built:
`infra/platform/workflows/rerail-a-car.toml`, the rules
`rerail-a-car-left-behind-on-a-conflict` and
`rerail-a-car-is-overtaken-when-its-car-closes`, and the `skips` reset
in `boss_jobs::car`.

**A release is a review verdict bound to the head it judged** (design
`7cedfa29`, David 2026-09-28, Q1 accepted as proposed; backlog
`b7b02024`, a trust-boundary design). A release was a bare null any
metadata write could set, bound to no head, and the diff judge ran only
at CLI doors. Decided: (D1) the conductor re-judges every car's diff
where it boards and writes a hold, failing closed, instead of skipping
it in silence; (D2) every hold carries `hold_sha`; (D3) a release is a
record (`sha` read from the forge, `review`, `verdict`, `by`, `at`,
`main_sha`), refused when the named review is missing, not a release,
at another head, or the builder's own; (D4) `boss review <car>
--verdict` writes the verdict onto the reviewer's own run; (D5) the
builder is the run whose gate vouches for the head, never an actor id,
because every agent signs as one actor; (D6) at boarding a release at
another sha is no release; (D7) the writer of those keys is declared as
data; (D8) `main_sha` carries the residue of `23fbe4e3`. Q1: the
builder refusal binds agent runs only; a human may release on an agent
review, or as his own review at that head accepted only through his
passkey session. Built: the conductor re-judge, `hold_sha`,
`release_record` and its refusals, the builder run among them
(`boss_jobs::car_release`), and `boss review`
(`boss-cli/src/review_verdict.rs`). D7's writer rule was generalized by
design `7fc4fb2e`, a registry of reserved keys refused on every
metadata door.

**Two changes that must land together ride as one car** (design
`2f75d67d`, David 2026-11-13, Q1 accepted as proposed; backlog
`6e6ab9ac`). On 2026-10-16 a pairing recorded as a note was released
alone, and every gate after its converge ran with a cold cache.
Decided: no co-landing edge, which would add a coupled state to seven
readers of a car; "safe only together" joins "green only together" as
one car. The door is `boss car fold <car> --into <carrier>`: it proves
by patch-id that the carrier's branch carries every commit, writes
`folded_into` with the proof, and takes the car off the dock, out of
boarding and out of the yard's release control; the carrier's merge
retires it `landed-twin` through a dispatcher rule, and an abandoned
carrier leaves it drawn as orphaned. Q1: a folded car shows no Release
control to anyone, David included; the road back is `boss car fold
--undo` with a reason, which returns it to the dock held. Not built on
origin/main: nothing but the fold step reads `folded_into`; the branch
`feat/a-car-folds-into-its-carrier` carries the door unlanded.

**A declared boarding edge holds its car when the predecessor cannot
be read** (design `87c7a624`, 2026-11-13, decided from the frame;
backlog `c303aa90`). `boards_after` failed open: an unreadable
predecessor answered `BoardUnjudged` and the car boarded. The reason
given, that holding would stop every train, was about a fallible
signature, not the verdict; a hold is per car, and every other car
boards. Decided: (D1) unreadable holds, with kind `unread`, and
`BoardUnjudged` is deleted while the signature stays infallible; (D2)
the dock and stuck regions count unread holds on a line of their own
and report a persistent one as a read failure; (D3) a malformed
predecessor id is Absent, which needs a person; (D4) only a landed
predecessor boards a car, and the edge promises "on main before me",
never "converged"; (D5) the unread hold carries the full cause; (D6) an
edge orders across merges while a fold (`2f75d67d`) makes two changes
atomic, so the two do not overlap. Not built: `EdgeOutcome::BoardUnjudged`
is still in `boss-jobs/src/car.rs` and its readers.

**A merge main lost ends its train, re-rides its car, and closes the
door it came through** (design `d812f1b7`, David 2026-09-25, three
questions accepted as proposed). On 2026-09-25 forge main moved off
train #687's squash within 33 seconds; the record said its car had
landed, and neither protocol had a true exit. Decided: (D1) pr-train
gains a `merge-lost` terminal, aborted with four required evidence
fields, reached by a conductor arm that needs two not-an-ancestor
readings of forge main on two ticks, unlands every boarded car and
files one pipeline-failure item, the terminal written first; (D2) `boss
car unland`, which re-reads the ancestry itself, corrects the car's
review note through the correction door and opens a linked successor
(`supersedes`, `superseded_by`) whose first open work is a gate on
current main, the item staying open until the successor lands, and Q1
gives ship-a-change an `unlanded` terminal rather than overloading
Abandoned; (D3) `boss publish` refuses `main`, `HEAD` and any `refs/`
name; (D4) a bounded, audited `forge-log` ops verb reads the Forgejo
container log over at most 15 minutes. Q2 protects forge main so that
only a PR merge writes it, declared in the tree and applied by the
forge converge; Q3 authorized one hand read of that log, the last.
Built, all of it: `merge-lost` in `pr-train.toml` and `MergeLost` in
`train/verdicts.rs`, `car_unland.rs` and `unlanded` in
`ship-a-change.toml`, the trunk refusal in `publish.rs`,
`infra/ops/verbs/forge-log.json`, and `infra/forge/protect-main.sh` run
by `forge-converge.sh`.

### Deploy and the forge

**Build the release image once, on the train, and converge that image
by digest** (design `a80f0608`, David 2026-11-13, both questions
accepted as proposed). The decision is told in full above, under "The
release image is built once"; what this fold adds is its measured state
and its place in the order. Built: car 1 only, that fold, which deleted
`build-and-deploy-cost.md`. Not built: car 2, forge CI building the
product image at the train's head, proving it boots and recording
`{image_digest, image_tree}` on the train (no job in
`.forgejo/workflows/ci.yml` builds `infra/oss-quickstart/Dockerfile`),
and car 3, the converge pulling that digest when the trees match and
recording `build_reason` when they do not (`cluster-deploy-runner.sh`
still runs `docker build` for every head, and no `image_tree` or
`build_reason` exists in the tree). Car 3 is a precondition of
reconstruction step 5 (design `25511015` §4, `roll-a-train`): the
train can hand its delivery off at the image only once the converge
deploys an image the train built, so step 5 waits on cars 2 and 3, not
on this fold.

**The off-site copy of main is private** (design `1f35a3e8`, David
2026-09-27, no questions; backlog `67931115`). The off-site push sent
`main` to `dauld/boss-mirror`, a fork of the public repository and so
public itself: every train was public within one tick, with no secrets
scan and no approval. Decided: forge main is not meant to be public;
`main` leaves the fork push at once; the disaster-recovery copy moves
to a private repository outside the fork network, its push credential
minted by the credential broker; and main's history is scanned once for
secrets. Accepted: no off-site copy of main between the two cars.
Built: `infra/forge/offsite-push.json` declares one target,
`algedonic-dev/boss-dr`, `main` only, credential `github-dr-push-token`
(design `76155676`), and the fork has none. The one-time scan was a
hand act the tree does not show.

**The forge's backup leaves the forge, encrypted** (design `5c81319e`,
David 2026-10-01, Q1 answered as proposed; backlog `be956c0e`). The
nightly forge archive sat only on the forge's own disk. Decided: one
off-site leg, to boss-gcp's data disk and never its root, which the
receiver refuses by mount source; no GCS leg, which would put a cloud
key on the forge for a fourth copy; encrypted before it leaves with
`age` to a public recipient committed in the tree, so neither host can
read it; carried by a deposit-only SSH key born on the forge, whose
public line David authorizes; the receiver stores only ciphertext,
names it, keeps 14 and prints what it stored; the sender records
`offsite=` only when the receiver's sha256 equals what it sent, and
otherwise `FAILED` with a non-zero exit, keeping the local copy either
way; a restore uses `forgejo-db.sql`, never the raw `gitea.db`. Q1: the
age identity is generated on David's workstation and kept in his
password manager and inside the recovery stick's envelope. Not built:
there is no `infra/gcp/forge-backup-recv.sh` or
`infra/forge/offsite-recipient.txt`, and `forge-backup.sh` still
records `offsite: none`.

### The estate and its operations

**w-1's second NVMe carries all of the gate's disk I/O** (design
`8457c07b`, David 2026-09-29, both questions accepted as proposed;
backlog `52ea56ac`). Decided: only fitting the disk needs w-1 down,
because a Talos user volume provisions live; w-1 goes down through a
graceful Talos shutdown, never the power button; the gate's whole I/O,
the warm seed and every run's workspace, moves to the new disk as one
xfs filesystem, because a reflink cannot cross two (a split by seed and
copy would restore the 184 s full copy); the install disk keeps
containerd, the kubelet, Longhorn and the builders' `/scratch`, and
EPHEMERAL is not moved, which would wipe w-1's replicas; the disk is
chosen by what it is, never by kernel name. Q1: `shutdown-node` runs
per use on David's passkey over a rendered plan naming the node and
what it will evict, and `cordon-node` and `uncordon-node` are bounded
verbs authorized once; Q2: if those were still held that morning, David
ran the cordon and shutdown by hand once, recorded on the packet.
Built: the verbs `cordon-node`, `uncordon-node`, `shutdown-node`,
`talos-get`, `node-status` and `node-converge`; the `gate`
UserVolumeConfig in `infra/cluster/talos/patches/w-1.yaml`, whose
kubelet bind was withdrawn by review `a79746c6`; one claim, `gate`, at
`/var/mnt/gate` with a `runs/<pod>` workspace per gate
(`gate-runner.yaml`, `gate-seed-local.yaml`); and per-node `disks` in
the estate registry (backlog `ba0b7308`).

**A host's ops-runner retires itself, and the converge witnesses it**
(design `a79a8067`, David 2026-10-01, both questions accepted as
proposed; backlog `98eb9349`). A verb that stops the runner would stop
what runs it and report through what it removes. Decided: the runner is
its own executor and stops only its timer, never the `.service` whose
pass must report, so the branch is cut behind the run; the verb reads
back `is-enabled` and `is-active` before its OK line and writes
`/var/lib/boss/ops-runner.retired` first; the host's converge, which
owes the runner nothing, reports `RETIRED` (or `DISABLED BY HAND` with
no marker) and later removes the unit files as `REMOVED`; undo is
declaring the role again. Bounds: the role already undeclared, read
live; no other open request for the host; never the estate's last
runner; one literal unit. Q1 admits it as a MUTATING verb on forge and
boss-gcp behind David's passkey over a rendered plan; Q2 built it at
once. Built: `infra/ops/retire-ops-runner.sh`, the verbs
`retire-ops-runner` and `plan-retire-ops-runner`, and the `RETIRED` and
`REMOVED` reports in `install-ops-runner.sh`.

**Longhorn is declared in the tree before it gets an upgrade verb**
(design `544c8e80`, 2026-11-14, decided from the frame; the Longhorn
half of backlog `cecf62b3`). The tree holds no Longhorn install, and
its v1.11.3 is known only from measurements. Decided, three cars: (1)
`read-longhorn-managers` adds the manager DaemonSet's
`app.kubernetes.io/managed-by` and `helm.sh/chart` labels and the names
of any Helm release Secrets, so the install method is read, not
remembered; (2) the install moves into `infra/cluster/longhorn/`,
version, a vendored source pinned by sha256 and the settings BOSS
depends on, by the method car 1 reads, with no re-install; (3)
`plan-a-longhorn-upgrade` and `upgrade-longhorn` follow the Forgejo
upgrade's shape, one minor step per plan, preconditions re-read before
each phase, live replica lists kept out of the signed bytes, engines
moved by Longhorn's own one-per-node setting, and the read-back
requiring every manager and engine on the target twice. Not built:
`read-longhorn-managers` reads none of those labels, and there is no
`infra/cluster/longhorn/` and no upgrade verb.

**A changed METHODS script refuses its old identity receipt where it
is read** (design `eaf0772b`, 2026-11-14, decided from the frame;
backlog `ff502049`). Decided: no dispatch rule or converge step opens a
re-read when a METHODS script changes; the consumer's own refusal,
`identity_method_changed` in `admission-reader-identity.py`, is the
mechanism, and the one change is that a refused caller's packet names
the method keys whose hashes moved, with a pin that every METHODS key
is a file. And the hand `rollout undo` line leaves
`infra/forge/OPERATIONS.md`, which names the `rollback-to` verb as the
one lever: collapse the copy rather than pin it, so the no-log pin
keeps its one spelling. Built: the refusal. Not built: the moved-key
list on the caller's packet and a dedicated pin (a fixture in
`admission-identity-test.py` copies every METHODS file and would fail
on a missing one), and the docs line, still at `OPERATIONS.md:454`.

**A stale ops plan is refused before the passkey, not after it**
(design `367777e5`, David 2026-11-16, Q1 accepted as proposed; backlog
`52567458`). The runner re-renders at execute and refuses a plan whose
sha moved, which is right but spends the signature first, and nothing
recorded when a plan was rendered. Decided: the runner records
`plan_rendered_at` beside `rendered_plan_sha256`, inside the signed
shape; the approval surface shows the plan's age; the jobs API refuses
a sign-off on a plan older than the `ops-request` row's
`plan_max_age` before the passkey ceremony starts, naming the door that
files a fresh render; the execute-time re-render stays as the last
line. Rejected: re-rendering on open, a mutating side effect of a read.
Q1: one hour, as data on the row. The row is held, so the change is
David's publish. Not built: neither key exists in the tree.

## Consolidation toward 1.0.0

**The debt is measured, the order to pay it is decided, and every
consolidation car leaves a measured line smaller** (design `42277636`,
David 2026-09-18; all four questions accepted as proposed). The
framing, David's: "We built up a lot of tech debt in our sprint to
design BOSS, and we need to spend this next period getting ourselves
simple, clean, functional, and adaptive … getting the open source
version into 1.0.0 readiness, but we are going to prove its utility by
running Algedonic, LLC on it." A read-only audit of origin/main
`3465acbb` (train #436) measured the debt — 406k Rust / 49k shell / 93k
TS; 82 lints, two of which could not fail; ~100 pinned pairs, 14 with a
concrete collapse; 154 migrations of which 81 were DML-only seeds; a
2,081-line bare-metal deploy script with ~310 live lines; the
system-of-record address spelled in 47 files — and the report stays on
the packet, not in a file. Decided: (1) **the order** — 1.0.0 blockers
first, being what a stranger hits before anything else (H5 instance data
in the platform schema, H6 `emp-david` literals, H10 the address
literals, H7 the bare-metal path), then what costs cars and diagnoses
weekly (H2 lints that cannot fail, H3 unguarded flake classes, H1 the
contended monoliths, H9 the lint exclusion set in five places, H4
registry rows whose only home is a migration, H8 no `boss` binary on the
forge, H11 nothing prunes the pod), then the MEDIUM and LOW findings as
filler cars when a builder is idle; (2) **the rule** — each item is one
car or a short series, and **every car must leave a measured line
smaller** (files, definitions, baselines): a consolidation car that
cannot name the number it reduced is not one; (3) **the bare-metal
deploy path is deleted, and the container launcher is the one way to
run BOSS** — it was 85% dead, drifted three ways from the container's
config generator, and every real instance is the container path; the
OSS quickstart documents docker/Kubernetes only, and the deleted code
stays in history; (4) **the `boss` CLI is installed on the forge from
the image** (as boss-gcp already is) and the ~3,000 lines of shell and
python twins each pinned by a test retire one verb at a time — one
implementation of each verb, in Rust, tested once, the forge running
the same binary the cluster does; (5) **the stranger lens** — the
by-hand and by-memory steps a fresh operator has no door for (the pod's
hand-installed tooling and door symlinks, the forge's hand-placed
Forgejo config, a printed runbook for the root ceremonies, the
hardening inventory) become items now, and they are the quickstart's
test: a fresh install on a clean machine, by the runbook, with nothing
from memory.

**What has landed, each with its measured line** (the item ids are the
backlog packets the design spawned):

- **H3 — the flake classes have a guard** (train #439, 2026-09-18,
  item `2c257761`). The 47 `… | grep -q` pipelines under `pipefail` in
  `infra/lint/` — a 1-in-3,000 SIGPIPE false red or green — became
  here-strings the converted-lint pin already covers, and the
  `tracing::set_default` shape that redded two trains (the #281 class)
  left `boss-core/src/publisher.rs` for one-test binaries that own their
  process, pinned by `a_log_capturing_test_owns_its_process.rs`.
  Measured: 47 flake sites → 0.
- **H2 — a lint that scans nothing is red** (train #441, 2026-09-18,
  item `cdf2d959`). `sim-boundary-audit.sh` scanned zero declarations
  under mawk's `\s` and exited clean; `seed-bypass-smell.sh` scanned a
  deleted directory. Every scanning lint now prints its scanned count
  through `infra/lint/lib/scanned.sh` and refuses on zero, a test of
  lints (`a_lint_that_scanned_nothing_is_red.rs`) runs each against the
  tree and asserts it, stale allowlist entries are refused by a shared
  helper, and `a-kind-bundle-does-not-tighten.sh` — the weaker twin of
  `steptype-bundle-ratchet` — was deleted. Measured: lints that cannot
  fail 2 → 0, and one duplicate lint gone.
- **H6 — one `platform_owner()`** (train #442, 2026-09-18, item
  `3c23662d`). Fourteen production sites named `emp-david` (one in
  Tier-1 `car.rs`) and `bootstrap.rs` named `emp-cto`; a second
  operator (Algedonic, LLC on BOSS, decided 2026-09-16) would have been
  fourteen edits or every alarm landing on David forever. The owner is
  now read once from the people registry (`boss-people-client::
  platform_owner`, the first hire holding `platform-admin`, with an
  explicit launcher override), the shell alerts read it off the same
  API, and `no-employee-id-literal.sh` refuses a new literal outside
  tests and example seeds. Measured: 15 literals → 0.
- **H7 — the bare-metal deploy path is gone** (train #443, 2026-09-18,
  item `e109bd71`; 102 files, +1,134/−5,508). `deploy-services.sh`
  (2,081 lines), `deploy-web.sh`, `bootstrap-local.sh` (which still
  started the deleted `boss-docs-api`), `bootstrap-vm.sh`,
  `dev-postgres.sh`, the four smoke scripts nothing ran,
  `check-service-drift.sh`, `check-service-write-roundtrip.sh`,
  `push-step-plugins.sh`, the host backup and restore, the
  deploy-confirm dead-man (unit, timer and script) and `files-root.sh`
  were deleted; the ~310 live lines — the `units` and `roster` modes
  boss-gcp's converge runs — became `infra/gcp/install-units.sh`,
  reading its roster from `infra/estate/roles.toml` and nothing else.
  The container launcher (`infra/oss-quickstart/services-launcher.sh`)
  is the one way BOSS runs, and its roster — the unpinned copy the
  whole time — is now the one held equal to `boss-ports`. The quickstart
  and the operator runbook document the container path only.
- **H10 — the address is spelled once** (train #444, 2026-09-18, item
  `5222163e`). The system-of-record address was in 47 files three ways
  and the forge IP in 62. Both now live in `infra/estate/estate.toml`:
  each managed host's converge renders `/etc/boss/sor.env` from it,
  every unit reads that file with `EnvironmentFile=`, every script
  through `infra/lib/sor.sh` (which refuses, naming the file, rather
  than fall back to a literal), the forge verbs source one
  `forge-defaults.sh`, and `the-estate-address-lives-once` refuses
  either address anywhere else, with a per-file allowance table whose
  stale entries are themselves refused. The `boss.algedonic.dev`
  cutover is now an edit to one file. Measured: 47 files → 1.
- **H5 — instance data leaves the platform schema** (train #445,
  2026-09-18, item `ee368d0c`). Nine migrations inserted this LAN's
  seven nodes and eight service instances, five node roles and eight
  credential ids into every fresh database — an adopter's first boot
  declared a cluster it does not run and credentials it does not hold,
  and one re-declaration under `ON CONFLICT DO NOTHING` had never
  landed anywhere. Credentials are now the instance's declaration
  (`seeds/credentials.toml` in the tenant contract, sent by `boss tenant
  publish` through an insert-if-absent batch door); the estate's nodes
  and roles are the tree's (`infra/estate/estate.toml`, published on
  every pod start through `POST /api/estate/nodes/batch`); and migration
  `20260918063829` deletes the seeded rows wherever nothing references
  them. Measured: the instance rows a fresh install boots with (seven
  nodes, eight service instances, their roles, eight credentials) → none.
  Explicit positive estate `disk_gb` is the narrow exception to
  `e187198f`'s insert-if-absent default: it refreshes a kept node from
  the tree; omitted capacity and other existing fields stay kept, and
  missing roles remain append-only (item `c426bc8a`).
- **H9 — the lint declares its own consist skip** (train #447,
  2026-09-18, item `6fa15484`). Which lints the consist check leaves
  out lived five times, and nothing held `gate.sh`'s copy equal to the
  one the conductor ran on — which had already produced one false
  refusal. Whether a lint needs more than a bare tree is a fact about
  the lint, so each such lint declares it on one header line
  (`# consist: skip — <why>`), `gate.sh --exclusions` derives the set,
  the conductor asks the assembled tree's `gate.sh`, and migration
  `20260918102236` drops the `consist_excluded_lints` column the
  delivery policy no longer needs. Measured: 5 definitions → 1;
  `delivery_policy.rs` −160 lines.
- **H7 residue — the hops that called the deleted path** (this car,
  item `ed64f852`, 2026-09-18). What #443 left: the conductor's own
  deploy hop in `train.rs` (pulled `/opt/boss`, ran `migrate.sh`,
  `build-release.sh`, `sudo deploy-services.sh prod`, `sudo
  deploy-web.sh`), reachable only through a `BOSS_TRAIN_DEPLOY_TREE`
  the one live conductor had set empty since the 2026-09-04 cutover;
  `boss status`/`restart`/`logs` and `boss deploy` (a hand-typed
  service roster, `systemctl` and `journalctl` on a host, a backup check
  reading a deleted timer); `check-binary-freshness.sh` (the
  `/usr/local/bin` fleet), `dev-refresh.sh` (an ssh to a host that no
  longer exists) and `deploy.env.example` (sourced by the deleted
  script). Every present-tense mention of `deploy-services.sh` became
  history or named its successor. Measured: 4 files and ~2,300 lines
  deleted, 4 CLI verbs gone; the `deployed` step of a train is now
  completed by one arm instead of two, with the same evidence it always
  carried on the cluster. Kept, each with its reason: `build-release.sh`
  (the migration rehearsal `restore-log-copy.sh` names it for the
  release binaries it verifies), `validate-brewery-sim.sh` and
  `reset-to-baseline.sh` (the maintainer correctness gate and the
  drop-and-reseed it calls, named by the README, the runbooks and the
  invariant register — still shaped for a host with systemd, which is a
  retarget, not a deletion). And one thing measured rather than done:
  the `maintenance-deploy-confirm` kind (0 packets ever) was retired on
  the live registry at 12:20Z and re-published by the next boot's
  platform seed at 12:42Z as v3, active — the seed reads "present" as
  "an active row exists", so a retired kind whose bundle file is still
  in the tree is re-inserted on every boot, and the file cannot leave
  the tree while the registry admits the kind. The seed must learn that
  a retired version is an operator decision it does not revert; then
  the retire sticks and the file goes.
- **H4 — registry rows whose only home was a migration** (trains
  #448, #450, #453 and this car, 2026-09-18, item `393d3234`; the
  decision paragraph is under §Stations above). Four registries, four
  cars, one shape each: `infra/platform/stations/`, `step-plugins/`,
  `cadence/`, `delivery-policy/` — one TOML per row carrying every
  column, published insert-if-missing by (name, version) by the seed
  every launcher already runs, through one decision table
  (`boss_jobs::bundle_seed`, written once in car 1 and reused three
  times); `infra/lint/migrations-declare-schema-only.sh` holds the
  cutover stamp `20260918112134` once with the four tables beside it;
  each bundle is held equal to the migrations' active rows by a
  `*_bundle_is_the_migrations_pg.rs` pin that also rebuilds an emptied
  table from the bundle alone. Measured: 25 migrations (7 + 7 + 9 + 2)
  stopped being the only home of 21 declared rows (5 stations, 12
  step-plugin kinds, 3 cadence rules, 1 delivery policy — the
  directories, counted); no migration newer than the cutover may
  insert one; the seed reads five bundle directories from one
  `--seed-path` and no launcher changed. Kept, with its reason: the
  conductor's compiled delivery-policy fallback (a dark registry must
  still board a train), pinned to the bundle rather than deleted.

**Pending, by item:** H11 the pod prunes worktrees whose branch is gone from the
forge (`1933db9e`); H12 the conservation-invariant sweep never ran
against the system of record (`236529aa`, found by the H7 measurement);
H1 splitting `train.rs` along its test-module seams, and H8 the `boss`
CLI on the forge with the shell twins retired verb by verb, are decided
above and not yet filed as items; the stranger-lens items likewise. The
MEDIUM and LOW findings ride as filler, by what a first outside user
would see.

**Reliability is crested by closing defect classes, not instances**
(design `3036296f`, *Cresting the reliability hill*, asked by David
2026-09-27 — "find and crest whatever hill of reliability so we start
doing fewer fixes and more features" — and both questions decided as
proposed). A day's reds and review holds fell into three classes:
silence that reads as success, a write that leaves no fact, and
host-mutating verbs with soft edges, every instance of the last caught
by adversarial review and none by the gate. A fourth finding was a
visibility gap: of the first 200 open backlog items 166 were untriaged,
and none recorded the car, review or run that produced it except in
prose. Decided: one mechanism per class, so the class cannot recur, in
the order D, A, B, C, interleaved with the urgent band, features
resuming as the fix share falls (q1). All four have landed. **D, an item
carries its origin as data**: `metadata.source`, checked against the
record before the POST (`boss-cli/src/item_source.rs`), receiving
grouped by source and area (`boss_jobs::origin`), and a slim list row.
**A, a write leaves a fact**: `every_write_stages_a_fact.rs` reads every
Postgres write under `crates/*/*/src`, and
`crates/core/boss-events/writes-without-a-fact.txt` is the one
allowlist, a reason on every entry (its registry half is design
`e5d814d3`, §Correctness). **B, a mutating verb fails closed and waits
for its review**: `an-ops-guard-fails-closed.sh` refuses a read whose
failure is swallowed into a value or recorded as a number, and a script
a MUTATING verb runs without `set -u` and `pipefail`; and the gate reads
the diff (q2) — a car that adds or changes a MUTATING verb, its script
or the runner is filed held until a RELEASE verdict is recorded on it
(`boss-cli/src/mutating_verb.rs`), the judged hold that option A of
backlog `bd331d38` kept. **C, adapters agree**:
`boss_testing::adapters_agree!` runs one behaviour suite against both
adapters of a port, starting from every `JobFilter` field and since
spread across the registries. Not built: the crest measure itself — the
daily share of new items from `review-finding`,
`discovery-while-working` and `pipeline-failure` against `roadmap` and
`user-feedback`, and review holds per held car, served beside
throughput. The hill is crested when new cars pass review with only low
findings and the fix share trends down for a week.

**The factory was drawn whole once, and the drawing ranks ten collapses.**
Design `e30c9b6b` (2026-10-31, origin/main `dc2d4c1db`) mapped the tree and
the live record with five independent read-only mappers and no
architecture documents. Measured: 113,603 packets, 71% maintenance or
observation ticks; 306 registry rows against 581k lines of production
code; tests at 152% of production; five separate things that tick; facts
with two writers (a train's `converged`, a gate's verdict); four records
around one agent step; and protocol leaked into code at both ends of the
pipeline (the conductor's 9.1k-line train machine, approval in
`ops-runner.sh`, thresholds in handlers). The findings F1–F10 and their
measurements stay on the packet. Decided by David on 2026-10-31:

- **The order.** F1 (heartbeats out of the record) and F2 (the step is the
  run: retire agent-run, agent-execution and the `agent_runs` row) first
  and in parallel; then F3, a verb action and a threshold trigger in the
  rule grammar, because F4 (one scheduler), F6 (the verb surface) and F7
  collapse onto it; F5 and F6 as continuous small cars; F8 as
  infrastructure cars; F7 last, behind a flight.
- **Unit ticks live outside the audit log.** A unit run is an event with a
  projection (telemetry lives outside the log, 2026-09-16), and a packet is
  filed only when a run fails. One `chore-run` kind with `metadata.chore`
  is acceptable only until that projection exists.
- **No fourth layer.** The review proposed naming "the movers" (rules,
  loops, runners) beside substrate, protocols and actors. David declined:
  movers, or coordinators, are actors that route or handle rather than
  work the job, which does not justify a layer. The station tree (design
  4b27a65a) since gives them their word: they are automata, the machines
  of the stations that contain them.
- **The conductor stays a service, and trains run by protocol.** Not the
  proposal's "conductor becomes protocol": a CI/CD service still exists,
  but it does not push a train step by step. Trains run through their
  protocol, and the conductor watches on a loop for problems that need a
  protocol triggered. The proposal's acceptance test still applies: each
  two-writer fact (`converged`, the gate verdict) ends with one writer.

**The debt is paid by reconstruction onto the station tree, with intake
rules beside it** (design `25511015`, *Paying off the debt by
reconstruction*, an analyst read of origin/main `7c6c6aa29` on
2026-11-17, decided by David the same day). Twenty-three areas were
mapped against the v2 primitives. The station tree is real in the
registry and the claim path — 25 station rows, the pools, an owner
station on every one of the 81 workflow rows, the three executor kinds,
the failure facts and claim placement, each pinned by a test — but not
yet in most of what runs or what the operator reads: the landing page,
motion, alarms and boards, the train's back half, 24 systemd timers,
departments as an entity, and work that arrives by nomination and pane
nudge. Reconstruction does not drain the backlog: 55 of 1,248 open items
(4.4%) close as superseded, 62 at most, and 95.6% are defects in
delivery machinery v2 keeps under new names. So it is done for what it
deletes — about 28,000 Rust and 13,700 web lines that draw the v1 map —
and for the binding constraint, idle executors: the factory stood idle
7.5 hours one night and nothing noticed. Decided: (Q1) **only an area
where v2 runs and a test pins it counts as solid, and the slice is not
solid until five proofs land** — the tree's two stale register entries
flipped, an observed `people:platform-admin` capacity, a claim-placement
probe by method, the station views catalogued with their motion off
`/api/yard/moves`, and the idle-factory alarm; (Q2) **executors pulling
at their stations is step 1, ahead of the visuals**, already in motion
(`f999363f`, `35f828cf`, `b513514c`, `5627a4e1`); (Q3) superseded by
design `01fa5bdf`, below; (Q4) **intake pauses, narrowly**: a new item
about the v1 map, regions, departments as an entity, peer nominations
and pane nudges, or systemd timers is refused, pointed at the step that
retires its area; the 55 open close as superseded when their step lands;
and two rules govern the kept machinery, where the other 95% sits —
**one item per defect class for review follow-ups, and an open alarm
updates its item rather than filing a new one**; (Q5) **fold before
reconstructing**: the 71 decided but unfolded designs ride in batches,
one record section per car so no two cars share a tail line, the
executors batch alongside step 1 and the rest before the visuals step,
with the six superseded `docs/design/` files deleted and the eleven that
point at the dead `/system/design` route given a disposition. After step
1 the order is: converge the visuals on the station views, departments
become the tier, station specs on steps (T6), `roll-a-train` split from
pr-train at the image, `keep-the-estate` replacing the timers, then
contract the vocabulary. Not built: neither intake rule refuses or
updates anything yet. The area map and the backlog classification stay
on the packet.

**The visual surfaces are the station tree** (design `01fa5bdf`, David
2026-11-17, superseding Q3 of `25511015`). One tree, one address per
station: the root is Algedonic (`algedonic`), its tier-one stations are
Engineering, Operations and Finance, the stations already under them are
kept, and a station's page lives at its address. **Executive dissolves**
(item `878fd1e7` is the vehicle): its personal parts — the watchlist,
the person's task queue, inbox and calendar — go to `/me`, every human
executor's own station, resolved from the signed-in user, and anything
company-wide goes to the root. Informational pages move into a station,
mostly Engineering or Operations, and Finance keeps `/finance/*`. **IT
disappears**: the `/it` landing and the old map go — `/it/operate`,
Atlas, Bottlenecks, the region boards, and the regions server code once
nothing reads it. **The `/ux` business pages are removed** — the brewery
and LLC surfaces: accounts, sales, warehouse, shipping, support,
catalog, products, parts, shop, marketing, people, service, qa, exec,
jobs and assets — and any of them is rebuilt later as a station's page
when a protocol actually runs there. **When unsure, remove**, because
rebuilding is cheap; that rule removes `/it/registry/policy`, while its
question (`dfe749f9`, should a policy change be a protocol packet) stays
open. The cars serialise on `apps/web/src/shell/nav-catalog.ts`: remove
(`dfad4c2b`); then address, a page's URL derived from its station's
address (`0bc2eb68`) and each catalogued page naming its host
(`03fb75c8`); then Executive dissolves and `/me` is the person's station
(`878fd1e7`, `5cb56bb3`); then relocate the kept pages (`92bfcb04`),
each move deleting its `/it` route; and last the regions server code,
once the alarms board and `boss orient` read station rows. Items filed
against a removed page close as superseded when the removal lands. None
has landed: `nav-catalog.ts` still lists `/ux/sales` and
`/it/operate/atlas`.

**`boss-jobs` splits before 1.0.0, after the station-tree retirement
cars, and BossMemory owns the correctness properties** (design
`089dc107`, crates and layers, David 2026-11-18; the analysis was
`docs/design/crates-and-layers.md`, written 2026-08-13 and re-filed as
the packet by backlog 924de432). The layer order BossMemory, BossNET,
BossProtocols, BossActors, BossApps already holds in the build graph,
and `infra/lint/layer-order-audit.sh` (`layer_of`) holds it as a
second axis inside tier 1 beside the tiers, never replacing them. One
crate straddles a seam: `boss-jobs` carries BossNET (stations, routes,
admission) and BossProtocols (the workflow and step registries) in one
package, about 210,000 lines on 2026-11-18 against 30,541 in August.
**Q1: split it along that seam before 1.0.0, but after the
station-tree retirement cars**, which would otherwise compete with it
for the same files; once split, BossNET cannot import BossProtocols,
so the substrate cannot learn what a workflow means. **Q2: `boss-events`
is BossMemory and owns provenance, conservation, closure, idempotence
and determinism**: each property's entry in `docs/invariants/` carries
`owner = "boss-events"`, which `infra/lint/invariant-register.sh`
holds to an existing crate, so a property no mechanism holds is that
layer's recorded debt. The author's two calls stand with it: no
package rename, because `boss-events` is not a wrong name for a log,
and `boss-brewery-engine` may depend on `boss-sim`. Q2 is built; the
split is decided, not built, and still owes its design and car plan.

## Design docs and the decision record

The markdown corpus stopped being the source of truth and kept the
title, and everything that broke around design review followed from
that. **The packet is the doc.** Decided in review `87f5bc84`
(2026-08-29), on evidence: 146 recorded answers had never reached a
file (11 queued, 135 in failed flush jobs); a terminal step named
*"Settled — carry it to a file"* completed twice and wrote nothing;
and a doc's status was a hand-written line nothing updated, so 20 docs
claimed live discussion while having no open questions. None of those
were independent bugs — they were the cost of keeping an authored file
and a projection of it in agreement when the decisions already lived
somewhere else.

**Generation runs from the packet outward.** Previously a human
authored `docs/design/x.md`, an indexer parsed it into rows, decisions
were recorded against those rows, and a flush job tried to write them
back — the file was the source and the database the projection. Now
the packet is authored and the file, where one exists, is a generated
artifact. This is the rule the rest of the system already lives by;
design docs were the one place it was inverted.

**A proposal whose substance is visual rides inside the packet as an
exhibit, not behind a link** (design `26a89f11`, David 2026-09-23, all
five questions accepted as proposed; from his question that day about
richer renderings "so we don't need to go to the claude.ai links"). A
palette, a layout or a diagram was rendered as a claude.ai Artifact and
linked, which put what was approved outside the record and mutable,
behind a second door, and out of `boss brief`'s sight. **HTML as the
doc's source was rejected**: packet prose is untrusted — any actor can
write step metadata — and the review surface
(`infra/step-plugins/review-design.js`) runs in the reviewer's
authenticated session, which is why it renders prose through web-kit's
escape-first `renderMarkdown`; and the record is text that agents,
search and this fold read. Decided: a design packet keeps its markdown
and questions and may also carry **`exhibits = [{anchor, title, html |
file_ref}]`**. (1) **The bytes** live inline in step metadata up to a
size bound (proposed 256 KB), frozen when the step completes, and in
`file_refs` above it, content-addressed and rebuilt from
`content.file.attached`; the build first measures whether `file_refs`
is on, and ships inline-only if it is not. (2) **An exhibit may run
script and nothing else**: `<iframe sandbox="allow-scripts">` without
`allow-same-origin`, loaded by `srcdoc`, under CSP `default-src 'none'`
with inline style and script only — no network, forms, top navigation
or fetched assets; one that needs live data is a page, not an exhibit.
(3) **Any actor who may author a design packet may attach one**,
agents included — the sandbox is the protection, not the author list —
with the attaching actor recorded. (4) **A question may bind exhibits**
by anchor and renders beside them; validation refuses an anchor the
packet does not carry. (5) **It is proven** when the visual redesign is
reviewed in `/it/design` with no claude.ai link. Authoring is `boss
design --exhibit anchor|title|path.html`, read with no shell between;
where an exhibit cannot render (a terminal, `boss brief`) it is listed
by anchor, title, size and hash, never dropped. **Both arms are
built** (backlog `73ef81fa`, 2026-09-24, two cars; `file_refs` measured
ON that day): the review step declares `exhibits = [{anchor, title}]`
with `item_one_of = ["html", "file_ref"]` and `item_value_max_bytes =
262144`, and `questions` declares `binds = "exhibits"` — three generic
`StepField` attributes, with an `anchor` repeated inside one field and
an element carrying two of its one-of keys refused beside them, all
judged at the step merge door and on a non-completing step PUT as the
write lands (only for the fields it touches) and again at done, where
an element carrying none of its one-of keys is refused too. `boss
design --exhibit` / `--bind` refuse the same before filing; a file over
256 KB is attached to the filed review step through the `boss attach`
path (read back and digest-checked) and recorded as `{anchor, title,
file_ref, sha256, size_bytes}`. `review-design.js` renders each exhibit
in the reading pane beside the decision rail, a bound question naming
it; a by-reference exhibit is fetched as the reviewer, refused unless
its size and sha256 match the record (said so when no digest can be
computed), and handed to the same sandboxed frame. Not yet proven:
both rounds of theme boards (`dea94998`, `a4df741a`) were decided from
a linked Artifact.

**The legacy corpus is translated once, not indexed forever.** All 52
markdown docs become packets and the directory stops being read;
leaving it as a parallel source recreates the two-sources problem the
change removes. The 49 legacy `design-doc-review` packets are archived
as they are, and their stranded decisions are **not** flushed — the
answers are already on the packets, which is where the fold reads
them, so writing them into files is work in the direction being
abandoned.

What this deletes, and the deletion is the point: the pending-decision
table and the whole flush pipeline (including a service that ran `git
commit` and `git push`), `boss docs flush-pending` and `boss docs
reindex`, the prose parser and its conventions (`**Status**:` in the
first thirty lines, `## Open questions`, `### Qn:`, `(resolved)`), the
corpus lint that enforced them, and the `stale-statuses` and
`rejections` reports — with them the concept of a "drifted" doc, since
a packet's status *is* its status.

**History: the design this replaced** (design-docs-as-data, eight
questions resolved 2026-08-16 from feedback `19614937`; folded from
`docs/design/design-docs-as-data.md`, deleted 2026-11-18, backlog
924de432). It moved the discussion into BOSS but kept git as the source
for settled prose: questions and their state became rows, decisions
were flushed back into a machine-owned region of each file, an indexer
re-read the files at every boot, and a release-time export produced
the settled residue. The pipeline that design specified is what the
2026-08-29 decision above and the two 2026-09-10 deletions below
removed. Two of its answers outlive it, and
`infra/platform/workflows/design-doc.toml` cites both: Q6, "file wins on
prose, data wins on questions", which is why a settled file is residue
written at the end; and Q4, that writing the file is a car's job ("the
docs car that carries the flush through a train"), which is why the
protocol's `publish` step writes nothing.

**The write-back half was deleted on 2026-09-10** (backlog `f5da586c`,
filed because this record had described the deletion in the present
tense for twelve days while the pipeline kept running). Gone: the flush
pipeline end to end — `design_flush_jobs`, the four `/api/design/
flush-jobs` routes, `boss docs flush-pending`, the markdown surgery it
applied, and the `git commit`/`git push` it ran; `POST`/`DELETE
/api/design/pending-decisions` and the step plugin's mirror into them;
the `stale-statuses` and `rejections` routes, their two SPA panels, and
`design_docs.pending_count`; and the two dispatcher rules that existed
only to serve that half (`design-decision-flush-queue`, and
`maintenance-sweep-doc-status-daily`, whose entire content was the
drifted-status report).

Part 1 stopped at the read half and left two findings, both now
settled. First, the pending-decision rows were **load-bearing for the
read half**: `upsert_doc` read them on every reindex and force-resolved
their anchors, because reindex rebuilt the question set from the parse
and an answer known only outside the file was otherwise erased on every
boot. Measured then: dropping both tables would have re-opened 25
questions across 6 docs and handed back six review packets for questions
already answered. So the table survived one day, renamed
`design_recorded_decisions` and backfilled from the stranded flush
payloads — a closed ledger with no writers. Second, `boss docs reindex`
and the corpus index stayed, because the daily
`design-review-level-sweep` read them and retiring a working rule was
not a call to make inside a deletion.

**The read half was deleted on 2026-09-10, the same day** — the
sequencing had to run that way round, because the ledger exists only to
stop a reindex from re-opening an answered question, and with no
reindexer there is nothing for it to stop. Gone: the whole `boss-docs`
crate and the `boss-docs-api` service (the parser, the reindexer, the
port and both adapters, the remaining `/api/design/docs` routes and the
gateway's proxy to them), the three read-cache tables
(`design_docs`, `design_questions`, `design_doc_rejections`) and the
ledger with them, `boss docs reindex`, the `docs.design.sweep` handler
and the two dispatcher rules it served (`design-review-spawn`,
`design-review-level-sweep`) plus the one-kind
`open_review_exists` helper the first of them used, the `corpus` panel
on `/it/design`, and the corpus lint (`docs_corpus_presents.rs`) that
enforced the file conventions. **No history is lost**: `audit_log` is
the system of record and every `docs.design.indexed` /
`docs.design.decision_recorded` event stays in it; these were
projections and read-caches.

Three things that deletion established, worth keeping:

- **The `### Qn:` / `## Open questions` / `**Status**:` conventions were
  FILE-WORLD ONLY**, and the world they described is gone. A
  `design-doc` packet's questions are registry-enforced structured
  metadata (`design-doc.toml`, `item_keys = ["anchor", "title",
  "proposal"]`); `review-design.js` returns before any docs-API call
  when the packet carries its own, and a mocked test routes
  `**/api/design/**` to 500 and asserts zero calls. So nothing anywhere
  parses a markdown heading for a question any more, and CLAUDE.md's
  §"Design docs" was corrected from "author your questions like this" to
  what is actually true.
- **Docs under `docs/design/` survive as human-read living references.**
  Nothing indexes them, no lint enforces their shape, and a docs-only
  change now implies no crate to compile (`infra/gate.sh`'s path map).
  The file is the residue of a settled discussion, not its venue.
- **`/it/design` became only what it claimed to be.** The page
  advertised itself as a lens over the `design-review` station's queue
  while rendering a table of files; its one use of the queue was a join
  on `subject.id` = doc path, which no `design-doc` packet can satisfy
  (its subject is the literal `boss-platform`), so the station's real
  packets rendered nowhere. It renders the queue now.

**A feedback or backlog item routed to `design` opens the executor's
`draft-design` task first, and the founder's `design-review` waits on
the design that answers it** (`ready_when = steps.draft-design.done`;
backlog `f90ca046`, 2026-09-18, measured on 54f0ab33 where the review
went ready with no design filed — an empty decision, 4f6019d7 again) —
`boss design --answers <packet>` files the design, writes the review's
question, and completes the draft with the `design_id`, and the publish
rule closes the review exactly as before.

**Design error gets two instruments: the brief carries prior art, and a
packet's proposed FIX is verified, not only its claim** (design
`0d02cda0`, David 2026-09-21, all three questions accepted as
proposed; answers backlog `3c779ca8`). Measured over 2026-09-20/21: the
protocol refused a mechanical error nine times, each refusal naming its
reason, while five design errors went through — one reached David (a
scoped credential designed as a hand-placement, the broker that retires
that ceremony never found) and four were caught only by the operator
searching first, a habit rather than a mechanism. Decided: (1) **`boss
brief` derives prior art** the way it derives invariants — what has
already been decided about the packet's area, matched on its `area` and
title terms — and is **informational first**, printing the match count
and the sources searched even when empty, because a fuzzy match needs a
precision number before it can earn a refusal; (2) **the corpus is**
closed design-doc resolutions, this document's headings,
`infra/dispatcher/rules/` names and CLAUDE.md §Doors — deliberately not
the open backlog, which would match nearly everything; the acceptance
test is that "credential" surfaces the broker; (3) **"verify the
proposed fix, not only the claim" goes in both** CLAUDE.md's startup
step 4 and `builder-rules.md`, since three packets that night were
right that something was wrong and wrong about the remedy, and a
builder reads a packet's fix shape as an instruction. Not yet built.
Design error in genuinely new territory, with no prior art and no
proposal to check, is out of reach of both by construction.

**A fold cites its source, so the next survey can check it by machine**
(packet `c98e5183`, the first `doc-flatten` run, 2026-09-27; answers
backlog `04c6e143`, "doc-flatten has never run"). The `doc-flatten`
protocol surveys `docs/design/`, folds what has settled into this
record, and deletes the file. Every folded entry names its source file
and the packet, review or tracker date that decided it, so a later
survey finds a doc's material here by grepping for the filename and
the deciding id rather than by re-reading prose. The first run folded
seventeen docs, each into the section its topic belongs to (the list is
on the packet's `fold` step); `dispatcher-station-boundary.md` keeps its
page as a living governance reference, and `brewery-fidelity.md`
contributed only its realistic-hours constraint. None of the August
deciding packets reads back from the system of record — `GET
/api/jobs/<id>` answered 404 for all sixteen ids tried, while the
control, `04c6e143`, answered on the same connection — so for those the
file's verbatim decision history was the evidence, and each id is cited
as the file recorded it. References elsewhere in the tree now point
here, except in applied migrations, whose checksums make their comments
history.

## Agent memory and the record

**A memory corpus is a second copy of the record that cannot be
audited.** Decided in design `a5368918` (2026-09-19), on the
measurement that opened it: the agent memory index loaded with
`MEMORY.md is 24.5KB (limit: 24.4KB) — 1 of 152 lines were cut off`,
and the line it dropped was David's world-map direction for `/it`, one
day old and load-bearing for three cars then on the dock. Nothing
failed loudly — the session read 151 lines and had no way to know the
152nd existed. A store that sheds content by growing is the one
forbidden failure mode, and the file has none of the properties the
correctness protocol exists to guarantee: unversioned, outside the
event log, mutable in place, no provenance per claim.

**The rule: agent memory shrinks from a corpus to a kernel of
USER-scoped facts, and the test for a survivor is "would a new repo
need it?"** Everything project-scoped moves to where it can be
audited, by class:

- **A door is never remembered.** Each doors-class entry becomes a
  CLAUDE.md line, a `boss brief` derivation from the file that decides
  it, or a refusal in the verb itself. A remembered door can drift
  from the door it describes, and unlike the §9a pairs no equality
  test is possible against it.
- **A decision lives in this document and nowhere else.** The memory
  line is a lossy second copy of a decision the record holds in full.
  If a decision is not discoverable from the record without a memory
  prompt, that is a defect in the record SURFACE, not a reason to keep
  the copy.
- **The session-state class is deleted outright.** `boss orient`
  computes trains, gates, the dock, stranded greens and MY WORK from
  the record; a hand-written READ FIRST snapshot is a stale duplicate
  of a live read. Each thing orient cannot yet compute is a car on
  orient, not a memory line.
- **Incident residue belongs to the packet that fixed it**, and to the
  comment in the code that cites it by date and packet id. The rule
  outlives the memory of it.

Two consequences carry forward as work rather than prose. **The budget
must FAIL rather than truncate** — we do not control the harness
loader, so the refusal has to be ours: a check that reads MEMORY.md
and refuses over budget naming the offending entries. Where that check
runs is the one sub-question left open, because the file lives outside
the repo at `/work/home/.claude/`. And **user-scoped data is a tenant
Subject row**: David is an `employee` Subject with a Class, so
timezone, company email and working hours are Subject and Class data
read through the same door as everything else — which makes the kernel
genuinely empty and deletes the concept rather than shrinking it.

What was done on the day is a pin, not the destination: 131 over-long
index hooks were trimmed (25,675 → 21,388 bytes, no entries deleted),
purely to stop the live truncation.

## Credential phase recovery keeps the original command and receipt

Settled by design packet `5840c3bf-85f8-49b0-96b6-5b3829a8d8bc`,
reviewed by David on 2026-10-04. A retry is the same observation,
not a new fact. The existing rotation route remains append-on-call for
legacy requests. A request carrying a durable, bounded `observation_id`
opts into an owner receipt keyed by credential, phase and observation.
The owner stamps the authenticated actor, instant and event identity;
the receipt retains the complete canonical evidence and original JSON.
An identical retry by that actor returns that receipt without another
event or rotation timestamp. A changed actor or any changed canonical
evidence conflicts. Ordinary callers cannot supply the reserved receipt.
Actor equality binds the identity accepted by the existing jobs door;
it is not host or executor attestation and does not repair its
machine-token completion proxy.

The credential row lock orders the PostgreSQL decision before the state
change. Receipt, state and outbox event commit together. The memory
adapter uses one critical section. Receipts live independently of outbox
retention, and their full envelope rides in the original event. The
credential owner's reconstruction path restores declarations and phase
receipts from complete audit history, preserving original actor, time,
identity and scalar spelling, refusing conflicting receipts, and emitting
nothing. Historical registry knowledge predating declaration events is
preserved; it is not invented from incomplete history.

A Secret effect and its value-free recovery witness commit in the same
object write, conditioned on the observed UID and resource version.
The server assigns the next version; the witness never predicts it.
The durable candidate exists before a completed Minted fact is recorded.
Original commands are namespaced by packet so a newer stage cannot erase
an unacknowledged packet's history. Private old and carried candidates
stay in separate Secret keys that the credential resolver ignores;
they never ride in a witness, receipt, event, packet or error. A retry
reads the actual candidate and witness, resends identical phase commands,
and requires the owner's original receipt before advancing a step.
Promotion preserves the first old/carried history on restart. Missing,
foreign or malformed witnesses, a changed UID/version, an unknown owner
receipt, or a superseded candidate hold rather than silently remint.
Witness storage remains bounded by the Secret object's enforced size:
there is no silent deletion of unfinished recovery history or guessed
reclamation policy in this change.
The stage and delivery rules remain distinct actors. Delivery requires
the source issue/install steps that followed their acknowledged owner
phases; it cannot replay those phases under the stage actor's name.
When delivery independently verifies the credential, that actual new
read has its own stable observation identity, which subsequent delivery
retries conserve.

These are two local atomic boundaries, not a distributed transaction.
A Secret witness does not prove the owner committed; an owner receipt
does not prove an unobserved Secret effect. The runner broker records
only the effects it verifies through the existing credential door.
This decision grants no credential, host delivery or writer authority.
Both host deliveries, DR rehearsal, authenticated executor evidence,
the remaining R4 proxy-forgery boundary, and declared writer activation
remain separate held obligations on their original packets.

## A runner's delivery acknowledgment is signed by its own host credential

Settled by design-doc "A runner delivery acknowledgment names its
authenticated host principal" (`104a93d5`), its one question
(`delivery-attester`) approved by David on 2026-10-05; it answered the
delivery half of backlog `1e50e66b` and the oldest, `6c9183de`. The host
that holds a broker-issued runner credential attests its own
installation with that credential: no new issuer, grant or
shared-machine actor assertion. Its trust limit is the one design
`f623e425` already accepted for execution: a compromised host, or a
stolen host credential, can lie about its own host, so the
acknowledgment is the host's report, not a proof of a file write.
Independent installation attestation would need a named external
attester and issuer first, and none exists.

**Built** (car `0451f958`, train #957, 2026-10-06).
`POST /api/credentials/{id}/delivery` (`boss_jobs::credentials::http`,
`credentials/runner_delivery.rs`) admits only a request whose presented
runner credential resolves, from the mounted Secret's own generation and
from whichever slot holds it (the staged `next` during a first
installation), to its host and minted-for packet. The caller must be
`automation:ops-runner` at operator tier, and the body may carry only
`job_id`, `attempt` and `secret_uid`, which must match that generation
and the owner's original installed receipt. The recorded actor is the
one the resolver proves, never a typed label: the `x-boss-user` ids the
host scripts still send (`automation:runner-credential-deposit`,
`automation:runner-credential-recv`) reach the record only on a request
the door did not resolve. The rotation-phase door refuses a request that
presents a runner credential or carries the delivery purpose, so
delivery cannot record or replay a stage phase, and a retry reads the
original receipt back at `GET /api/credentials/{id}/delivery/{attempt}`.
Both hosts acknowledge through one helper, `infra/lib/runner-delivery-ack.sh`:
the forge from `runner-credential-deposit.sh`, boss-gcp from
`runner-credential-recv.sh` (car `2532e4ae`).

**Live.** The forge's first runner rotation (`c896cb12`, 2026-10-09) and
boss-gcp's (`f194ab3e`, 2026-10-10) each completed `delivered` as
`automation:ops-runner`, with a `delivered_receipt` bound to its host,
rotation packet and attempt. Those two deliveries met row E's
precondition in design `b08725c2`: `runner:ops` entered
`RESOLVABLE_PRINCIPALS` (`boss_jobs::field_writer`, 2026-10-10) and the
ops-request approve keys declare it as their writer. The "Not yet built"
list under the declared-writer decision above is therefore out of date
on boss-gcp delivery, `RESOLVABLE_PRINCIPALS` and the ops-request
declaration. Who may issue and install is the stage half, decided by
design `6e28ed42` below. Nothing of `104a93d5` is left unbuilt, and both
parent items are closed.

## A runner credential's broker stage is believed from a verified workload token

Settled by design packet `6e28ed42-38af-4dae-87cb-3b208de27d41`, answered
by David on 2026-10-05 and confirmed in his own words on 2026-10-06: the
existing shared `boss` workload identity is permitted for runner
credential stages ONLY, with a distinct verified audience and a maximum
ten-minute token lifetime. The alternative — a dispatcher-exclusive
workload and ServiceAccount first — was not chosen. The decision
authorizes no TokenReview grant and no live activation.

**What is trusted, stated as its limit.** Every process that can use
the pod's ServiceAccount shares this authority, the jobs API included.
A verified token proves workload membership — not which process asked,
not that a particular dispatcher rule fired, not that the pod still
exists. Verification is offline, so a retired workload keeps the
authority until its token expires, ten minutes at most. Nothing asks the
cluster whether a token is still live.

**What is verified** (`boss_jobs::credentials::broker_stage`). The
caller presents a separately projected ServiceAccount token in
`x-boss-broker-stage-token`. The door reads RS256 only, a signing key
the trusted key set names, the signature, and only then the claims: the
configured issuer; exactly one audience and it is the configured one (a
token also good for the cluster's own API is refused, and an audience
equal to the issuer is refused as configuration); the namespace and
ServiceAccount, in `sub` and in the `kubernetes.io` claim, which must
agree; an expiry not yet reached; a lifetime of at most 600 seconds. An
unknown key, an unavailable key set and a key under 2048 bits each
refuse. A refusal is a 403 carrying one of a closed set of fixed
sentences; no token, claim or key byte reaches a log, an event or an
error. The header is taken off the request before any handler runs.

**What it authorizes.** Only `POST /api/credentials/{id}/rotation/
{phase}` for a registry row of kind `ops-runner-credential`, and only
when the command's `job_id` names the open `rotate-a-credential` packet
whose subject is that row. The phase is then recorded as
`automation:credential-broker-stage` — the actor is assigned from the
verified workload, and the rule label, operator tier and host the caller
typed confer nothing. Every other credential's phases are recorded
exactly as the phase-recovery decision above left them, and a token
neither helps nor hinders them. A host's runner credential cannot stage,
and a stage token is not consulted by the delivery door.

**Off until configured.** The door is on only when all five
`BOSS_BROKER_STAGE_*` variables are set (issuer, audience, namespace,
ServiceAccount, key set); the dispatcher presents a token only when
`BOSS_BROKER_STAGE_TOKEN_FILE` names one. Unconfigured, a runner row's
phases are recorded under the typed label as before. Partly configured
refuses runner stages by name; a token presented to an unconfigured door
is refused, never ignored; a dispatcher told to present a token it
cannot read mints nothing.

**Activated** by backlog `25fff647`: `infra/cluster/manifests/boss.yaml`
sets all six in the one `boss` container and projects the token there
(audience `boss-credential-broker-stage`, `expirationSeconds: 600`, at
`/etc/boss/broker-stage/token`). The namespace and ServiceAccount come
through the downward API, so each rendered instance expects its own
workload. From that roll a runner credential's phase with no verified
token is **refused** — a new refusal, and the one this decision exists
for. The pin `the_broker_stage_door_turns_on_whole.rs` holds the
projection, the variables and the door's terms equal in every instance.

**In flight.** A phase recorded before activation keeps its original
actor, event and instant. The verified workload's replay of it is a
conflict, never a rewrite, so an activation requires that no runner
rotation packet is open. None had ever been filed when this was
written (three rotation packets in the record, none for a runner). The
first, for the forge's runner credential, completed with the door off
at 2026-10-09 03:25Z and keeps its typed actors; none was open when the
activation was built.

**Where the keys come from** (David on design `20058482`, `stage-keys`).
The jobs API fetches the issuer's key set itself, on every verification,
from `https://kubernetes.default.svc/openid/v1/jwks`, presenting its own
pod ServiceAccount token — the discovery endpoints answer 401 to an
anonymous read and 200 to any ServiceAccount (the stock issuer-discovery
grant, held through `system:serviceaccounts`). The connection trusts
only the cluster CA the kubelet mounts; built-in roots, plain HTTP and
redirects are refused, so the pod's token reaches nothing the cluster CA
did not certify. A `BOSS_BROKER_STAGE_KEY_SET` that is a path still
reads a key-set file. The registered actor is the automations row
`automation:credential-broker-stage`, at the `platform-admin` the stage
command carries.

**What the activation read and did not change** (review F1, F2, F4,
recorded on the car). Who can mint a token that verifies: anyone able to
run code as the `boss` ServiceAccount or to request its token — in the
tree no Role grants `serviceaccounts/token` or `pods/exec` in `boss`,
so it is cluster-admin, the control plane, and whoever sets the boss
pod's image or template (the converge; the break-glass operator within
its image-only bound). The audience is not the API server's (its default
audience is the issuer, which the door refuses as an audience). The
token is a bearer value sent over plain HTTP to the jobs API on
loopback inside the pod; a reader of that hop is a process in the pod,
which already holds the same ServiceAccount — the shared-workload limit
this decision accepted. Still to decide: whether an UNCONFIGURED door
should refuse runner rows by default rather than record the typed label.

## A run survives silence only on progress the server can attribute

Settled by design-doc "Bind native-run survival to authenticated
substantive progress evidence" (`afddd915`), its one question
(`progress-authority`) approved by David on 2026-10-06; it answered
backlog `d4c0cd2d`. An executor's own checkpoint may postpone its run's
`died` only when the jobs API can bind the caller to the run's assignee
through a protected credential, and the checkpoint is an immutable,
server-stamped record of new work (a committed source hash, a check with
its exit status and log hash, a bound artifact). It proves an authorised
submission, never continuous execution. The four-hour bound does not
change. A metadata write, a summary, the `--started` worker receipt, a
session heartbeat, a live host or a typed `x-boss-user` renews nothing,
and until such a credential exists the checkpoint door refuses rather
than trust an asserted actor.

**Built** — the refusing half. `agent-run-dies-when-building-is-silent`
reads completed-step stamps and a linked gate alone: the gate's launch
counts (car `cd552f58`, 2026-10-18), only when the gate was born linked
(`aaf5cafa`, 2026-10-20), and a queued place counts too (`18ad59b9`,
2026-10-21). Its comment no longer says a metadata write keeps a run
alive. The silent-session watch and the overdue-receipt rule
(`a-declared-worker-receipt-is-observed-every-five-minutes`) file
alarms; neither postpones a death.

**Not built** — the checkpoint door. The only principals the server
resolves today are `runner:ops` and `signer`
(`boss_jobs::field_writer::RESOLVABLE_PRINCIPALS`); an agent signs with
`BOSS_ACTOR` behind the shared estate token, so the precondition is
unmet. Backlog `56cfa6b5` holds the car until an executor credential
exists.

## A bounded START prints the current contract and lists the history

Settled by design-doc "Bound agent START without losing original
evidence or required authority" (`e72dc7c9`), its one question (Q1)
approved as proposed by David on 2026-10-15; it answered backlog
`40f0436c`. The measurement: a dispatched START printed every job
metadata value and every completed step's, and asked the worker to read
all of it before its own `--started` receipt. On packet 266f06e8 that
was 1,003,270 bytes, 836,756 of them one old gate-window observation,
with the pinned protocol at byte 959,711 and the run's own identity at
1,000,374. Conservation was already solved, because an oversized brief
rides as a verified attachment. Attention was not, and neither another
attachment nor a shorter summary bounds it.

**The decision.** A Workflow row may opt into a versioned contract,
`execution-inputs/v1`, declared under its `metadata.execution_inputs`
and pinned with the packet's version, so publishing a row changes no
admitted run. Under it every recorded value is in one of three classes:
REQUIRED current input, printed verbatim in the START; a MANDATORY
original, a verified reference the worker still reads in full before
`--started`; or an AVAILABLE historical original, retrievable in full
and not claimed read. Available does not mean read, satisfied, current,
superseded or green.

- **Unclassified is REQUIRED.** Age, size, completion, a familiar key
  name, a prefix such as `coordinator_`, prose and model judgment never
  demote a value. A missing, malformed or conflicting declaration never
  makes content optional, and where sources disagree the strongest class
  wins.
- **Two things may demote, and nothing else.** One is the row's own
  selectors, by exact pointer, which are registry data that publish
  authority already guards. The other is an operator's classification
  of ONE FROZEN VALUE, binding the packet id, the JSON pointer and the
  value's sha256 with a purpose and a reason. It goes through an audited
  door that asks `publish` on `workflow`, so no new role is granted. A
  changed value, a new correction or a copy onto another packet returns
  the value to REQUIRED, and the old packet is never rewritten.
- **Some facts cannot be demoted at all:** target and scope, the pinned
  protocol and the step's specification, authority and assignee, the
  executor's rules and derived invariants, the own-run declaration,
  holds and prerequisites, decisions and their resolutions, corrections
  including withdrawals, the findings the assignment must resolve, and
  any evidence a current step, predicate or proof requires.
- **The bound refuses rather than clips.** The START is at most 128 KiB
  of UTF-8, rules, own-run and manifest included, and names at most 256
  originals; past that it names one verified inventory, which is itself
  mandatory. Required material that does not fit is an admission refusal
  that names each oversized source and its size. Nothing is summarised.
  Authority and the own-run identity come before the historical list.
- **Originals are conserved as bytes.** The raw packet snapshot is
  stored as the API answered it, before parsing, and so is the complete
  legacy brief, each through the verified attachment door. The manifest
  must round-trip through admission before the prompt is offered. The
  bounded receipt states that this exact START and every mandatory
  original were read, and binds the contract, the START digest and the
  manifest digest. A coordinator's receipt cannot stand in for the
  worker's. Retrieval verifies bytes and never certifies reading.
- **Legacy stays whole.** Legacy rows, legacy runs and any adapter that
  cannot enforce the contract keep the complete START and the obligation
  to read all of it. There is no automatic repin, no receipt rewrite and
  no silent fallback to truncation.

**Built.** Car `df6adff8` merged on 2026-10-26 (train #1414). The pure
evaluator is `boss_jobs::execution_inputs`, which holds the bounds and
`protected`, the list of facts no classification can demote. The
operator's door is `POST /api/jobs/{id}/input-classifications`, behind
`boss classify`, and it appends to the packet's reserved
`input_classifications` list. `boss dispatch` renders the bounded START
through the one brief renderer (`start_inputs.rs`) and admits it only
after its record round-trips. `boss dispatch <run> --original
<n|snapshot|legacy>` prints an original once it has been verified
against the manifest. The native acceptance, which replays the actual
retained packet, is an ignored test that FAILS without its private
input (`the_actual_native_packet_replays_bounded_and_conserved`).

**Not enabled.** No Workflow row opts in. On 2026-11-13 none of the 89
active rows in the live registry carried `execution_inputs`, and no row
under `infra/platform/workflows/` declares it. Every START today is
therefore the legacy complete one; this fold's own START was 82,526
bytes. The design put the backlog workflow's opt-in after tests and a
distinct review, and no packet yet records either that opt-in or an
authorized native acceptance run.

## Open findings — where two live decisions disagree

Flattening surfaced three places where a settled decision conflicts
with another that is also still in force. None is resolved here; a
fold is not the place to pick a winner. Each names what the code
does today, because that is what this record is obliged to state.

- **Group→role mapping for the IdP.** `docs/design/idm-kanidm.md`
  carries it as an invariant — Kanidm owns membership, the join to
  BOSS roles is registry data. The shipped OIDC runtime deliberately
  has no such registry and reads roles from the employee row,
  reasoning that a mapping table would be a second source of role
  truth. Both are defensible; only one can be the design. **Today
  the employee row wins**, and the doc's invariant is stale as
  written.
- **A queue: lens or node?** `queue-visibility.md` (deleted by fold
  F2, 2026-11-17; §The network substrate keeps its resolutions)
  states that a queue is a `WHERE` clause and *never* a reified
  structure, with "no queue tables" as an explicit non-goal. The
  station registry shipped as rows. The distinction it was reaching
  for survives one level down — the station row is real, its
  membership is a predicate — but the doc as written argues against
  the substrate's own nodes, and two of its five open questions have
  been answered by shipped code (the claim primitive, the
  assignments lens) while still presenting as live.
- **What `Self_` and `Team` compile to.** §Policy & auth records
  the owner-keyed scope predicates as load-bearing and deliberately
  not flattened; the packet model's stage 2 re-keys them onto
  queue-derived ownership. This one is a **scheduled** divergence
  rather than a disagreement — stage 2 is resolved and unscheduled —
  but until it lands, two sections of this document describe
  different futures for the same predicate, and that should be
  closed by a decision rather than by drift.

## Registered conductor diagnostics — technical plan f7e3aba0 (2026-10-12)

A held source car can need an actual assembled-candidate diagnostic before
an operator can judge its hold. That work is a diagnostic-request packet,
with its operation version, required checks, actor and capacity declared
in the pinned Workflow row. The existing conductor consumes that station
under its existing lock and Kubernetes identity. Packet data supplies
identities and evidence; it supplies no command or privileged bootstrap.

The first operation uses the existing isolated-consist controller. Its
request names the held car, full candidate and baseline SHAs, and the
complete source receipt hash. The conductor copies the original signed
creation fact, pinned protocol, current car, authoritative policy and
edit-level before execution. It refuses changed bindings or unavailable
authority. Normal assembly uses a separate tree and a private immutable
ref; it moves neither the source branch nor the conductor's main checkout.

One admitted request has one durable attempt identity. Immutable first
records hold intent, acknowledged native incarnation and complete receipt;
large originals use verified content attachments. A private ordered journal
retains command inputs, exact stdout/stderr bytes and exits before parsing.
An acknowledged create can be recovered from those originals. An unknown
create acknowledgement never causes another launch. Native Job/Pod UIDs,
tree, baseline and actual exits bind the result; missing checks, init
failure and observation refusal are distinct outcomes. Completing a
diagnostic changes no hold, review, boarding, PR or departure.

Compute capacity counts nonterminal diagnostic workers. Terminal Jobs and
private refs remain retained when existing authority cannot clean them up;
this grants no patch or delete permission. A private UID-owned journal with
nonreplaceable ancestry is a runtime prerequisite, never inferred from a
PVC declaration. Source checks establish neither that prerequisite nor an
actual candidate result. Publication, arrival and the real diagnostic
receipt remain separately observed obligations. Urgent ddcdf6eb still
requires an observed successful departure. The next operator admission
surface and presentation are separate implementation work under this plan.

### Pre-create refusal and unresolved historical requests — d66ee7fa (2026-10-14)

A pre-create refusal is a distinct result with no native Job or Pod UID.
It retains the intent, unique invocation, winning immutable first-record
disposition, phase and complete command originals with verified readback.
Only the invocation that receives Recorded201 may prepare the candidate;
Replayed200 reconciles originals without repeating an effect. Durable
invocation entry precedes preparation, and an entering-create mark precedes
creation. A fully observed failure before that mark can establish refusal.
Create entry, lost acknowledgement, transport ambiguity, incomplete journal
or failed record readback remains UNKNOWN. Current native absence cannot
establish historical absence. The new result contract is an append-only
protocol version; existing packets retain their pinned interpretation.

David accepted administrative cancellation of historical diagnostic
47648ecc-5ef9-4823-bd1c-5365fec2c09c as unresolved after preservation of its
complete intent, original journal and current native census. That packet
is cancelled under v1; cancellation skips unfinished steps and records no
native outcome. Its original execution remains UNKNOWN. The immutable
intent, all 1657 hash-bound journal observations and current census are
preserved in verified content attachments, with a reconstruction manifest
on the packet. This retirement proves no refusal, launch or candidate
acceptance, cancels no native compute, releases no spool hold and authorizes
no subsequent request. Future source behavior and actual runtime proof
remain separate obligations; urgent ddcdf6eb still requires its observed
departure.

### Retained preparation recovery assessment — 2751a5d3 (2026-10-15)

David authorized a separate versioned read-only assessment for the retained
v2 preparation on diagnostic688b4f42. The assessment is its own packet,
linked to that original request and the decided design. It preserves the
original protocol, execute state, intent and invocation. A recovered refusal
is evidence on the assessment, never a retrospective v2 execution result.

The published assessment contract pins the authority resolution and exact
original request. Only the registered conductor reads its retained journal
and fixed native namespace. It retains whole original files and command
streams before judging them, checks one winning preparation, an adjacent
successful fetch and valid differing baseline read, and positive Job and
Pod absence. Interrupted streams or staged writes, conflicting journals,
duplicate preparation, any create entry or possible unacknowledged create,
and missing native evidence remain UNKNOWN. Append-only first records and
readback bind the assessment; replay repeats no preparation or compute.

This source implementation is carried by SOURCE39286f51. Its gate, distinct
review, convergence, registry publication and actual native assessment are
separate obligations. A subsequent current-baseline diagnostic needs its own
ordinary admission. No assessment clears a hold, grants authority or proves
a required check or departure. Future operationv3 baseline refusals remain
a distinct contract. Urgent ddcdf6eb stays open for its actual departure.

The ordinary queue reader may consume a completed assessment only after
independently checking its immutable disposition, original intent and
invocation, decided authority, pinned assessment contract and full evidence
readback. It then stops reserving the old attempt's effect-admission position;
the original packet remains unchanged. UNKNOWN, incomplete or conflicting
assessments never remove that reservation. A separately admitted diagnostic
still passes all ordinary claim, current-baseline, authority and launch gates.
The assessment itself neither changes ordering nor admits another effect.

The full retained journal exceeds one content-store upload. Its lossless
transport uses bounded files and an ordered manifest of byte offsets,
lengths and hashes, with whole-body verification before interpretation.
Immutable part records and the packet's file census reconcile interrupted
uploads; missing bytes, changed hashes, repeated positions or incomplete
references refuse the assessment. Events and completed-step evidence carry
bounded references, and the same reader reconstructs intermediate originals and the final
assessment on replay. Inline and single-file historical records keep their
meaning. No upload or event limit is increased and no original is shortened.

## A launched agent runs as its own Job — decided, built dormant, parked

Design-doc "A launched agent runs outside the launching verb: its own
pod, and a bound the verb does not hold" (`21062f95`, answered by David
2026-10-10T23:31Z) settles where an agent started by `boss dispatch
--launch` executes. Q1: in a Kubernetes Job of its own in `boss-dev`, on
the dev worker's posture (uid 65534 / gid 1500, no privilege, read-only
root), not under a second Unix account on the dev pod. Q2: it holds no
credential of the dev pod — no ServiceAccount, machine or forge token —
only its harness login, read-only (the login cache is amended by
`6f3ef63c` below), and its own workspace; the launching verb writes the
run's receipts from what the process prints. Q3: the cluster holds the
bound: `activeDeadlineSeconds` is the verb's bound plus its grace, so a
suspended or killed verb no longer extends a launch, and deleting the
Job ends every process in the pod, whatever process group it joined.

**Built, dormant.** The Job contract and its property validator
(`launch_job.rs`), the controller lifecycle (`launch_controller.rs`),
the native transport (`launch_cluster.rs`, rows under
`infra/platform/agent-controllers/`) and the `launch = "cluster"` branch
in `dispatch_launch.rs`. Nothing selects it: the one controller row,
`agent-controllers/codex-exec.toml`, says `enabled = false`, and no
harness row declares `cluster` (`codex-exec` is `process`, both Claude
rows are `host-tool`).

**Not built, and parked.** The managed issuer that gives each Job its
login (backlog `68e53553`), a published witnessed image, enrollment,
activation and the RBAC grant. David parked the program on 2026-10-17
(design-doc "Agents launched as cluster Jobs: continue, narrow, or
park", `b922bd17`): parked, not closed, to return to option A, Codex in
cluster Jobs. Backlog `3869107c` and `68e53553` stay open and carry the
remaining cars.

**So today** (origin/main `5f3cd40d0`, 2026-11-13): a launched agent
runs in the dev pod as the session's Unix account. A `process` launch is
bounded by the verb's own timer and its process-group stop; a
`host-tool` agent — every Claude builder and reviewer — is the session's
own subagent, and none of the newest 200 of 6362 agent-run packets
carries a `launch` receipt. The uncredentialed worker below isolates the
code a builder runs, not the agent process itself.

## Managed Codex cache boundary

David completed design-doc `6f3ef63c-469e-47e9-a48c-0eb7cbcb3214`
on 2026-10-14 at 01:10:34 UTC, resolving `managed-cache-boundary`.
It amends only design21062 Q2 credential storage. The issuer input stays
read-only; a dedicated managed run may materialize its login in a private
writable cache. No coordination-session cache is read or copied.

The trusted supervisor acquires a durable exclusive lease before granting
the installed generation to any run. It retains the original generation
and binds the run, JobUID, PodUID and container incarnation. After the
run is terminal, a trusted private collector verifies the exact retained
cache, its supported account metadata and the same authorized principal.
Publication compares and swaps against the leased generation. Cache bytes
stay private; only generation, issuer, consumer and effect metadata enter
BOSS. The worker receives no broker write authority, machine or forge
credential, ServiceAccount token or writable issuer Secret mount.

Uncertain refresh, failed save, unexpected format, collection refusal,
principal mismatch or replaced incarnation makes the stream unavailable.
Loss after a possible provider exchange quarantines the generation; it
does not retry the old refresh token or expire the lease into availability.
Recovery reconciles the exact retained renewed cache or requires dedicated
re-enrollment, preserving the original outcome. Expiry or revocation blocks
new launches. Cluster deadlines, UID-fenced teardown and private artifact
retention remain independent requirements.

The first source checkpoint adds cache transitions to the existing trusted
controller through a private broker port and fake-only controls. Validated
collection is staged in an unavailable `Collected` state, conserving the
lease and next-generation identity even when a save commits before its
acknowledgement fails (review962f1165). The kernel never releases that state
to `Ready`; exact durable publication/recovery remains trusted adapter source
work. It has no durable broker/provider adapter and does not wire the real worker cache.
The native managed path refuses before compute until those prerequisites
exist. Dedicated authenticated private human enrollment UX, genuine consent,
issuer installation, private collection/recovery and native runtime acceptance
remain owed by3869107c. Neither this decision fold nor a source GREEN is
evidence of those effects or authority to activate isolation.

### The issuer input is an issuer-owned login — decided, unbuilt, parked

Design-doc "Declare the issuer-owned login binding for isolated launched
harnesses" (`85c991d1`, answered by David 2026-10-12T00:22Z) settles
which login the read-only issuer input above may be. A launch stays
fail-closed until a credential registry row names the issuer, principal
and scope, the read-only storage and its consumer, and the renewal
owner, with an installed-generation receipt; the recommended binding is
issuer-managed and dedicated to launched harnesses. Whatever the estate
can derive is issued, delivered, renewed and revoked by a broker handler
with effect evidence; only an authorization it cannot derive is David's
act. It is no licence to copy a session's login, create a sample Secret
or make the mount writable. David chose the account on 2026-10-13: the
coordination session's own.

**So today** (origin/main `f998bdbaa`, 2026-11-13): no such row exists —
`boss credential list` shows 19 credentials and no harness login — so
the cluster path refuses an absent login witness (`launch_cluster.rs`)
as well as being disabled. The managed issuer that would install it is
backlog `68e53553`, parked with the program (section "A launched agent
runs as its own Job" above).

### Private collector placement

David accepted design-doc `001fe8d0-d3d8-4ac2-bad3-87eb8395024c`
on 2026-10-14 at 13:45:53 UTC, resolving
`managed-cache-collector-placement`. It settles collector placement and
the additional broker authority while preserving the cache boundary above.
The broker remains the sole issuer and owner in `boss`. The existing
trusted controller creates one broker-owned collector Job in `boss-dev`
only after independently observing the original compute Job and Pod stopped.

The collector mounts only the original retained workspace PVC read-only
at a fixed path. Its publisher-verified immutable image must contain the
pinned supported CLI and collector helper; previous image publication does
not prove that helper exists. The broker binds the complete fixed intent,
image digest, Job/Pod/container incarnation and original/current PVC UID
to its own records. Caller paths, images, addresses, UIDs and terminal
claims supply no authority. Issuer login, coordination home, machine/forge
credentials, hostPath and broker state are excluded from the collector.

The dedicated `boss-dev/boss-managed-cache-collector` ServiceAccount has
automount disabled and no Kubernetes API RoleBinding. A short-lived,
Pod-bound projected token uses audience `boss-managed-cache-collection`,
separate from metadata-only IPC and Kubernetes API authentication. Bearer
bytes remain in a protected projection, never argv or execution output.
Private cache bytes travel directly to a fixed additional HTTPS listener
on the existing broker through an internal Service. Journaled Exec and
metadata-only IPC cannot carry them. The sole operation collects at most
256 KiB for an exact active lease/run/nonce. Before reading private bytes,
the broker verifies cluster signature, audience, expiry, dedicated SA UID
and Pod UID, then independently reobserves matching native objects and
original compute stop evidence.

Only the collector gains a route to that fixed TLS Service/port and DNS.
It remains denied to the SoR, Kubernetes API, private hosts, other managed
endpoints and public network; worker denies remain unchanged. Private
originals and full durability receipts stay in a descriptor-confined,
ownership-verified 0700 broker-only directory on the existing `boss-auth`
PVC. No cache bytes enter BOSS packets, attachments, access logs, execution
transcripts or KubeTransport journals. Diagnostics use fixed error codes;
only identities, generations, hashes, counts and fixed receipts leave
the private channel.

The accepted source authority additions bind only `boss/boss`:

| Namespace / Role | Resources | Verbs / names |
| --- | --- | --- |
| `boss-dev/managed-cache-native-observer` | batch Jobs; core Pods, PVCs | get |
| same | core ServiceAccounts | get; resourceNames `dev-session`, `boss-managed-cache-collector` |
| `boss/managed-cache-server-tls` | core Secrets | get, patch; resourceNames `boss-managed-cache-server-tls` |

Dynamic Job/Pod/PVC names make the read boundary whole native objects in
`boss-dev`, including specs. Software derives names only from trusted
original run records. There is no list/watch, create/update/delete,
pods/exec, pods/log, node, cluster-wide or namespace Secret-read grant.
The controller retains its existing authority and the collector gains
none. Native broker RBAC remains unverified after the controller's refused
403 censuses; reconcile actual installed grants before adding duplicates.

A fixed-purpose broker handler mints, renews and revokes server TLS key
and certificate into that one named Secret, declared empty through normal
converge. The broker cannot create Secrets. Its private key remains in a
read-only projection; only public trust material reaches the protected
collector launch plan. This derivable credential requires no human minting
or placement. Rotation uses bounded overlap and trust generations, with
old keys revoked only after effect verification. Issuer enrollment and
installation remain separate obligations.

Collection durably closes the exact lease operation; token validity is
not replay authority. Retirement uses original UID-conditioned deletion
and independently observed absence. Lost acknowledgement, replacement,
expiry or uncertain collection retains or quarantines the exact stream
and workspace. `Collected` remains unavailable until trusted publication
and recovery prove their effect and acknowledgement.

This decision records no installed grant, Secret, helper, image, Service,
network policy, provider action or native launch. Implementation still
owes independent native observation, adversarial privacy/UID/token/mount/
writer/body/durability/lost-acknowledgement controls, exact installer and
archive witnesses, distinct review and actual installed effect evidence.
Supported account/format verification, issuer installation and renewal,
publication/recovery, controller acknowledgement and complete native agent
acceptance remain separately owed. Urgent `3869107c-1ff5-462e-ae24-9b11dfc37d97`
stays open; approval and this fold authorize no activation or enrollment.

### Protected collector observation reads — source authority 916ea815

David accepted design-doc `916ea815-4fd2-4969-b006-4f48d325f097`
on 2026-10-16 at 03:28:44 UTC, resolving
`protected-collector-native-reads`. It amends only the accepted source
authority of `001fe8d0`; it installs no grant or observer binding.
The additional reads bind only `boss/boss`:

| Namespace | Resources | Verbs / names |
| --- | --- | --- |
| `boss` | core Pods | get |
| `boss-dev` | core ConfigMaps | get; resourceNames `boss-managed-cache-public-trust` |

The namespace Pod grant exposes whole dynamic Pod objects in `boss`,
including specs and inline environment values. Software requests only its
retained original broker Pod name and refuses a changed UID or container.
The trust reader compares the complete protected original ConfigMap,
generation and digest before and after the fixed TLS handshake. Existing
`001fe8d0` grants remain unchanged. No list/watch, mutation, exec/log,
namespace Secret, TokenRequest, TokenReview, cluster-wide or worker
authority is added. Actual effective broker RBAC remains UNKNOWN and must
be measured before any separately authorized grant convergence.

The next source work must join the protected producer and installer,
reusing the concrete current-trust reader rather than adding another
disconnected observer. Retain original empty Secret creation and broker
Pod/container receipts before publishing metadata; configure the fixed
listener, projection, Service and network route, plus a real registered
generation-closure producer. A later mutable read or caller assertion
cannot supply an original installation or prove closed issuance.

This decision left a projected-token compatibility debt open, and the
next subsection settles it. A kubelet-managed 600-second projection does
not give the broker every original TokenRequest request/response, and it
cannot enumerate every rotated token. The producer had to reconcile that
path with durable closure, including cancellation, restart and lost
acknowledgement. Do not fabricate replies, weaken expiry, grant collector
API access or add an issuer. Any extra TokenRequest/TokenReview authority
needs its own explicit decision.

This amendment proves no installed grant, producer, TLS endpoint,
credential/provider effect, enrollment or native READ/USE acceptance.
Urgent `3869107c-1ff5-462e-ae24-9b11dfc37d97` remains open, and the
existing isolation hold remains in force.

### Retirement proves accepted authority, not issuance — bb59ee3d

David accepted design-doc "Reconcile projected collector tokens with
protected generation retirement"
(`bb59ee3d-e0bc-4aa1-947b-637d3c06a5ba`) on 2026-10-16 at 14:34:24 UTC,
resolving `projected-token-retirement-proof` as proposed. It amends one
evidence form that `916ea815` kept. Retirement no longer has to list
every original kubelet TokenRequest request and response. That list
cannot be produced: the collector Job requests a projected token
(`managed_cache/collector.rs:82`), and the kubelet, not the broker,
requests and rotates it.

The approved kubelet projection and the sole issuer stay. Retirement
instead proves that the private collection endpoint can no longer
accept any token of the old generation. It needs all of these:

- **One protected generation record.** The broker owns it and serves it
  to the existing `RegisteredCensus` consumer. The record holds the
  launch intents, the original create and Pod responses, and the
  admission seal. An intent is persisted before it is sent. A lost
  create or delete acknowledgement stays unresolved until it is matched
  to the exact original identity. The controller registers a collector
  before launching it and cannot mint its own clearance. A sealed
  generation refuses new launches across restart and cancellation.
  Labels, an empty list or a caller's completeness flag cannot seal it.
- **Admission and the seal share one durable boundary.** A private
  request binds its authenticated collector, lease and nonce to the
  generation before the broker reads its body. A request already
  admitted finishes or is durably cancelled before closure. The seal
  denies every later admission, whatever the token's rotation, and a
  restart cannot reopen it. A valid offline JWT alone cannot pass this
  gate. No new metadata IPC, journaled Exec or shared-PVC transport is
  implied.
- **The original launch set is conserved.** Original Job, Pod,
  installation and current-trust inputs are kept privately, and the
  absence of each named Job and Pod is observed twice with exact
  NotFound. A changed UID or container, an unknown or missing original,
  an unresolved operation or a replacement refuses. A namespace query
  cannot rebuild the set.
- **The verifier's real policy is bound into the evidence.** The
  endpoint verifies offline, not through TokenReview
  (`managed_cache/ipc.rs` `authenticate` calls
  `boss_jobs::credentials::broker_stage::verify`). That verifier refuses
  a signed lifetime over `MAX_LIFETIME_SECS` (600), gives expiry no
  grace, and allows at most `NOT_BEFORE_LEEWAY_SECS` (30) on `iat` and
  `nbf` (`broker_stage.rs:85`, `:88`). The proof binds the identity of
  the verifier actually serving, and those limits keep one definition.
  It never reads them from the projection's requested duration. A
  long-lived token from the real issuer is refused before its body is
  read.
- **The bound is seal time plus 630 seconds.** T is the broker's own
  clock reading when the seal durably commits. Every token accepted
  before T expires by T+630 s. Retirement waits that long and rereads
  the closure unchanged. Clock reversal, missing clock provenance, a
  missing policy identity, a missing quiescence receipt or a changed
  closure refuses. The result is recorded as an accepted-authority
  expiry bound, never as the last issued token's expiry.
- **Presented originals stay private.** They are kept only in the
  broker-private, descriptor-confined store, never in packets,
  attachments, logs, Exec journals or argv. No current projected-token
  read stands in for an original kubelet response.

This proves that no old token can still be accepted at that endpoint.
It does not prove that every token Kubernetes issued has expired. It
authorises no TokenRequest, TokenReview, audit stream, collector API
grant or new issuer, and the controller gains no PVC-deletion or Secret
authority. The proposal's alternatives were not taken: keeping the
literal all-issued ledger would leave qualification unavailable, and
broker-minted TokenRequests would need their own authority design.

**So today** (origin/main `11e2e77c7`, 2026-11-13): nothing of the
amendment is built. No `GenerationAuthority` port or generation seal
exists. `RegisteredCensus` (`observer/evidence.rs:59`) has only the two
test implementations in `observer/tests.rs`. `observer/retirement.rs`
still requires the schema-1 closure, with original TokenRequest
requests and `expirationTimestamp` replies (`retirement.rs:67`, `:117`,
`:136`), so retirement refuses today. The producer and its consumers
are owed red-first under urgent `3869107c`, alongside the installer
obligations listed above. Grant convergence, installation, enrollment
and native acceptance stay under the existing holds.

### The broker originates compute creation — source authority 27787f53

David accepted design-doc "Choose the genuine managed compute original
producer" (`27787f53-ada2-4d99-9ddf-093577a3f1f8`) on 2026-10-17 at
00:03:57 UTC, resolving `compute-original-producer` with the recommended
answer. It amends `001fe8d0`, which placed compute creation with the
controller and gave the broker GET only. The reason: a per-run Record
needs genuine originals, and the controller is an interactive dev-session
workload whose executable can come from a writable worktree. A mutable
job field, a caller-carried CREATE reply, possession of a workload token
or a later GET is not an original.

So source may move ONLY the creation of the managed compute workspace PVC
and compute Job into the broker's fixed lease-acquisition driver. Before
any CREATE the broker verifies the controller workload, the registered
run, the installed issuer generation and the publisher-qualified image,
derives the fixed manifest itself, and fsyncs one append-only LaunchGroup
for the lease. A second group or a changed command for the same lease is
refused. Each CREATE is reserved durably before dispatch, and its complete
request and reply are retained before anything interprets them. A lost or
partial reply stays UNKNOWN: there is no retried POST, and no later GET
is adopted as the original. The Job controller creates the Pod, so Bind
takes only a Pod-name locator, which the broker reads back by exact name
before it publishes Record. The controller keeps submission, Pod
observation, UID-conditioned retirement and collector creation, and the
Collect handoff of `001fe8d0` is unchanged.

The added source authority binds the SHARED `boss/boss`:

| Namespace | Resources | Verbs |
| --- | --- | --- |
| `boss-dev` | batch Jobs | create |
| `boss-dev` | core PVCs | create |

That account runs the whole `boss` Pod (dispatcher, jobs, gateway and the
rest), so these rights are not broker-only. Kubernetes cannot name-scope
CREATE, so any holder can create a Job in `boss-dev` that names another
ServiceAccount or mounts a Secret there, or allocate PVC storage. David
accepted that trade. The declined alternative was a separately qualified
controller producer with its own immutable journal. No Pod create, Job or
PVC mutation, list/watch, exec/log, Secret read, TokenRequest or worker
grant is added.

**So today** (origin/main `f00acc632`, 2026-11-13): none of it is built.
The controller still creates both objects through its own transport
(`Effect::CreateWorkspace` and `Effect::CreateJob` in
`launch_controller.rs`). No source names a LaunchGroup. In `boss-dev` the
manifests grant `boss/boss` only `get` and `patch` on named Secrets
(`boss-credential-broker.yaml`), and neither Job nor PVC create. Even
the `001fe8d0` observer Role is not declared. The cluster path stays
disabled (`agent-controllers/codex-exec.toml`, `enabled = false`). The
decision authorizes no grant, installation, enrollment or activation.
Urgent `3869107c` stays open and carries the work, and the issuer stays
on `68e53553`.

### The installer deposits the original and hands off by append — de48ff01

David accepted design-doc "Preserve managed collector installation
originals through bootstrap and broker replacement"
(`de48ff01-9bdb-4e4b-8c55-5967785b7c2b`) on 2026-10-16 at 17:33:35 UTC,
resolving `installer-original-custody` as proposed. It settles a gap
the sections above left. Under `001fe8d0` the normal converge creates the
fixed TLS Secret empty and the broker cannot create Secrets; under
`916ea815` the broker must hold the original empty-Secret creation and
its own original Pod and container, and may not take them from a later
read. Nothing delivered that creation receipt to the broker. And an
ordinary rollout replaces the broker Pod while the Secret is already
populated, so neither rebinding nor a fresh bootstrap was lawful.

- **Bootstrap is a fixed receipt deposit.** The converge keeps creating
  the Secret. The forge captures the exact empty CREATE reply privately,
  then delivers only that metadata through one fixed-purpose installer
  operation on the existing broker's ordinary service, separate from
  collection. The caller is authenticated as the actual forge installer,
  not as a typed actor with the shared estate token. The existing forge
  host credential (`boss_jobs::runner_credential`, held root-only by the
  forge converge) gains this one further fixed purpose, named here; no
  new issuer is added. The transport's target, confidentiality and host
  identity are qualified before any runtime use.
- **The broker checks for itself.** It observes its own Pod and the
  unchanged empty Secret, validates the original UID and
  resourceVersion, the workload, container and projection, and the
  descriptor store's ownership, and retains every original before it
  mints or publishes anything. A fixed request UUID with an immutable
  intent and readback makes a lost acknowledgement replay the same
  binding. The operation accepts no caller path, selector, URL, key,
  cache or token value and no other install action; only fixed
  statuses, identities, hashes and counts leave it.
- **Replacement appends; it never re-originates.** The root installation
  and its CREATE receipt are kept forever. A separately authorized
  installer may append a linked successor-incarnation proof. It needs
  the old owner quiescent, the old incarnation's termination or absence
  independently evidenced, the same Secret UID, slot and trust custody,
  and the successor's workload, container and projection validated
  independently. A Pod replacement and a container restart each need
  their own native termination evidence; an HTTP timeout or an actor's
  assertion is not evidence. Anything missing or unknown holds the
  handoff.
- **Old generations keep what they ran under.** The old generation
  closure, its actual artifact and policy and its original clocks are
  preserved, and the successor cannot rewrite them. New generations bind
  the successor's actual artifact and policy. Unknown clock continuity
  or accepted-authority bound, an unresolved old reader, create or
  delete acknowledgement, a replaced Secret or a source or trust
  mismatch refuses. The listener and renewal are configured only after
  the handoff commits.

The alternatives were not taken. Keeping today's refusal is safe but
leaves `3869107c` unable to finish. Storing the original in the Secret
or an annotation, or rebuilding it from a GET, breaks the accepted
custody contract; an admin Exec or shared-PVC ferry, or a second
issuer, is a different trust boundary; broad Secret create or list is
unneeded. This is a source contract only. It adds no Kubernetes grant,
TokenRequest or TokenReview, credential read or mint, provider
operation, enrollment or activation, and any further authority the
implementation finds returns as its own decision.

**So today** (origin/main `11e2e77c7`, 2026-11-13): none of it is built.
`FileBoundStore::bind` (`managed_collector_tls/installation.rs:380`)
still expects an `Original` from a protected installer, and only its
tests call it. It refuses a broker container whose `restartCount` is
not 0 (`:168`) and a Secret whose UID or resourceVersion differs from
the original or that is no longer empty (`:397`), and it has no
successor path, so a broker rollout still strands the installation. No
manifest declares `boss-managed-cache-server-tls`, nothing outside the
module wires `managed_collector_tls`, and neither `runner_credential.rs`
nor `forge-converge.sh` names an installer purpose. The work stays on
urgent `3869107c` under the existing runtime and activation holds.

## Dev builds run in an uncredentialed worker

Design-doc "Dev builds cannot read or use trusted session credentials"
(`c395e62c-5658-4588-a3cb-6ccd9e78d360`, 2026-10-05, no open
questions) is the dev half of David's isolation decision on backlog
`2710c8fc`. The credential-bearing doors (`boss`, `boss-api`, the
machine and forge tokens, the dev-session ServiceAccount) stay in the
trusted dev session. All repository and dependency code runs in a
separate worker Pod that holds no credential: bun lifecycle scripts,
`build.rs`, proc macros, candidate binaries, tests and lints. The
trusted controller owns the Kubernetes connection and the exact worker
Pod UID, and sends argv over the existing authenticated exec. No path
runs from the worker to a door. There is no stamping proxy or socket,
no bearer session token, and no new issuer or principal. What the worker
returns is data, never a command or an actor. A failed launch or
transfer is a stop, never a fallback to the trusted container. The
in-cluster senders rule above cites the same design: a workload running
branch or third-party code never holds the machine token.

Built, under backlog `b3071bf4` (closed delivered 2026-10-22; isolation
ON since 2026-10-21):

- The worker, `worker_job` in `infra/dev/dev-build.py`. It mounts no
  ServiceAccount token and shares no host namespace. It runs as uid
  65534 / gid 1500 with every capability dropped and a read-only root,
  and `forge-read` is mounted only in the bootstrap init container.
- The controller doors, `dev-build start`, `exec-auto`, `put` and `get`.
  They are published as a dev-control generation that
  `/work/tools/dev-control/enabled` selects (`infra/dev/activate-control.py`).
  `boss`, `boss-api`, `wt-cargo` and `wt-web` answer from that copy.
  On a refused selector, `wt-cargo`, `wt-web`, `boss --built` and
  `as-gate-uid.sh` refuse with exit 75, while `boss-api` still answers.
- The source fixtures for the acceptance controls are
  `infra/dev/isolation-fixture.py` and `infra/dev/fixtures/` (a build.rs
  attack, a proc macro, read/use, and a stamping-proxy negative). The
  native fake receiver is `infra/forge/isolation-fixture-contract.md`
  (design `a21c491c`).

Not built: the deployed, fake-only READ/USE acceptance matrix, which is
the design's controls 1-5 run against a live worker. `b3071bf4` closed
on three reads (the doors refuse, the budget refuses, and a builder
built through a worker), not on that matrix. Its own
`acceptance_matrix_runtime_route_20261005` records every deployed row
as owed. Backlog `86d93e12` owns it. Left out on purpose and still out:
authenticated per-agent identity. A session's `BOSS_ACTOR` is still an
assertion the trusted session makes. Later refinements live elsewhere:
activation and its gaps in design `a68fc42a` (its own fold), the
re-admission chain in `4e79c324` above, and the committed-head binding
in `ac162341` below.

## The matrix's live target runs on the forge executor

Design-doc "Bound the native fake-only isolation fixture and its
lifecycle" (`a21c491c-6a72-4698-804d-65cc38e68f9c`, answering
`b3071bf4`) asked who may create the live network target that the
READ/USE matrix's worker negatives need. David accepted R1 on
2026-10-15. The target is one fixed synthetic receiver Job and one
internal ClusterIP Service in `boss-dev`. The existing forge ops
executor creates them, using the rights it already holds. The
dev-session account gets no Service authority, and nothing gets a new
grant, credential, broker, egress route or network policy. If the
forge executor cannot show the rights it needs, the operation refuses
and names what is missing. The approval grants no rights by itself.

The receiver mounts no token and no credential. Its effects are
counters in its own emptyDir, and it calls no real service. A request
carrying only an asserted user, role or machine header is refused. A
request carrying the receiver's own synthetic authority is accepted.
That authority never leaves the receiver, so the candidate worker
cannot use it. The request's id fixes every name, and the operation
binds to the native UIDs it created. Teardown reads all the evidence
back first, then deletes by UID precondition and observes that the
objects are gone. A lost acknowledgement or a changed UID leaves the
fixture UNKNOWN. Nothing re-creates it by name, and a TTL does not
count as teardown. The design activates nothing.

Built on 2026-10-15 as three ops verbs: `prepare-isolation-fixture`,
`observe-isolation-fixture` and `retire-isolation-fixture`
(`infra/ops/verbs/`, driven by `infra/forge/isolation-fixture.sh`).
The live inventory reads the fixture's own journal to declare its
objects. The contract is `infra/forge/isolation-fixture-contract.md`.
That is source. Running the verbs natively is part of the matrix that
`86d93e12` owns, and a source green is not a native receipt.

## A dev worker follows its worktree's committed head

David answered Q1 YES on design-doc
`ac162341-3767-489b-876f-49246faa8a14` on 2026-10-20 at 17:04:24 UTC,
answering backlog `ceea10b8-fcef-443d-9cd0-59d3af0a61a2`, a flip
gate for dev isolation `b3071bf4`. A dev-isolation worker is admitted on
one exact commit, fetched by its init container, and until this decision
every `exec-auto` refused once the bound worktree's `HEAD` moved off that
sha. Builder rule 4's own order (commit, then pre-flight) therefore
tripped it on every first commit, and the activation receipt's
`rehearsal-check-after-commit` could not go green, so isolation could
not be activated through its own door. The decided reading: the binding
follows the worktree's committed head through the put door. The
worker exists to run candidate code away from credentials, so its
content was never the trusted part; the put door already carries
builder-chosen bytes into it with no binding change. A re-sync writes
the same bytes through the same door, with no new credential, no new
network path from the worker and a receipt per file. What changes is
the meaning of the binding record: it attests the local commit whose
committed content the worker was synced to, with the fetched sha kept
beside it. The rules:

- `admitted_head` is the sha the init container fetched and never
  changes for the session's life. `synced_head` is the local commit the
  worker was last synced to; it starts equal to `admitted_head`, and
  `check_binding` compares the bound worktree's `HEAD` to `synced_head`.
- On a moved `HEAD`, `exec-auto` reads locally and metadata-only, as
  `source_head` does, `git diff --name-status --no-renames
  <synced_head>..HEAD`, and requires `synced_head` to be an ancestor of
  `HEAD`.
- It refuses before writing anything, naming stop and re-admit as the
  way on, when the head does not descend from `synced_head` (a rebase or
  a reset onto another line), or any changed path is a symlink, a
  submodule, a mode-only change, a path the put door's own rules forbid,
  or a file over the put door's 16 MiB bound.
- Added and modified files go through the existing put code path;
  deleted files go through a new `remove` worker operation with the same
  path rules and the same directory-fd walk as put. That operation is
  the only new worker-side code; the worker gains no credential, route
  or authority.
- Each path gets a re-sync receipt (the sha256 put already prints, or
  `removed`), and `synced_head` advances only after every path has one.
  The re-sync never commits in the worker, whose own `HEAD` stays the
  admitted sha.

Decided from the frame beside Q1, because each widens nothing. The
worker's clone carries the forge's main as `forge/main`: the init fetch
adds `+refs/heads/main:refs/remotes/forge/main`, same container, token
and forge, so `infra/lint/lib/trunk-ref.sh` (which tries `forge/main`
first) and the trunk-relative pins run in the worker as in the gate,
with no hand-set `BOSS_TRUNK_REF`. The brief's worker-execution
invariant states the lifecycle, derived from the controller rather than
retyped: admit only a head the forge has; a commit re-syncs on the next
`exec-auto`; a rebase or a non-file change needs stop and re-admit.
Builder rule 4's wording follows it. Refusing an unpushed head before a
pod is created stays with `457ac1e2`, which owns that refusal. This
answers the per-commit cost design `a68fc42a` named as its gap (e) and
left undecided; that design's own fold of D1-D3 is not made here.

The cars, in order: `forge/main` in the bootstrap, with a dev-build test
that the rendered init command carries the refspec; the `remove`
operation and the re-sync in `exec-auto`, test first, which flips the
`rehearsal-check-after-commit` pin in `preflight-control_test.py` from
"stale is named" to "the second build after a commit passes", with a
test for each refusal above and for receipt-before-advance; then the
brief's invariant lines and builder rule 4, pinned by the brief tests.
This fold builds none of them and activates nothing. Backlog `ceea10b8`
and `b3071bf4` stay open, and the existing isolation hold remains in
force until isolation is activated through its own receipt.

## Dev isolation activation follows main from the /work PVC

Design-doc "Dev isolation activation: state, gaps and plan (b3071bf4)"
(`a68fc42a-f057-4dbd-b619-d2d163ce1097`) is how the uncredentialed
worker above was switched on. David answered D1-D6 on 2026-10-10, each
on its recommended reading. Measured against origin/main on 2026-11-13:

- D1, built. A failed re-activation leaves the selector refused, not
  absent. `wt-cargo`, `wt-web`, `boss --built` and `as-gate-uid.sh`
  then exit 75 while `boss-api` still answers. The reclaim sidecar's
  hourly pass files `dev-control-selector-missing`, `dev-control-refused`
  or `dev-control-behind` (`infra/cluster/dev-scratch-reclaim.sh`).
- D2, built. Builder pre-flight, probe rehearsals and direct
  interpreters run through `dev-build exec-auto` (builder rules and the
  brief's worker-execution invariant). The first builder to build a car
  through a worker is on `b3071bf4`'s record, 2026-10-22.
- D3, built. `infra/dev/control-images.json` names the worker and
  postgres images by repository and tag, `forge_url` comes from
  `infra/estate/estate.toml`, and `infra/estate/install-cli-from-image.sh`
  resolves both digests into each generation's immutable `config.json`.
- D4, built. CiliumNetworkPolicy `boss-dev-build-egress`, applied by the
  cluster converge, selects `boss.dev/purpose: isolated-build` pods. It
  denies every pod but kube-dns, the host, the nodes, the API server
  and the private ranges, and allows DNS, the forge's port and public
  port 443. The `dev-build-fence-status` verb reads it back.
- D6, built. The store and selector are `/work/tools/dev-control` on the
  `/work` PVC, which a roll keeps, and the hourly install re-activates
  each verified generation (`--reactivate`). On 2026-11-13 the selector
  and `image-cli/current` named the same generation, `2dbd868b`.
- D5, half built. Workers start cold (`WT_SEED=/workspace/no-seed` in
  `infra/dev/dev-build.py`). The main-only seed is backlog `db949aee`,
  and the cold build's measured cost is `e3ec2d95`.

Gap (e), a cold worker per commit, was answered by `ac162341` above. The
USE row the fence exists for is still unproven: the deployed READ/USE
matrix is `86d93e12`.

## After the shop floor, the machine carries the work (393bdef6, 2026-10-16)

Design-doc "After the shop floor, the machine moves the work: release on
verdict, launch on ready, and a map that shows who moved it" (`393bdef6`,
David accepted all three questions as proposed on 2026-10-16) measured a
week of downstream work. Two acts still waited on a session or on David:
launching a reviewer (353 launches, about 145 h of delay) and releasing
a held car on its reviewer's RELEASE (156 releases, about 106 h). The
rule it set: **who judges does not change; the machine does the carrying
around the judgement.** A reviewer still reviews and an agent still
repairs.

- **Q1, release on verdict. Built.** The rule
  `release-a-held-car-on-its-reviewer-verdict` (backlog `0cfc57e6`)
  fires when an agent-run records a review verdict naming a car, and
  asks the one release door with that review, signed as the rule. The
  door's judge decides, for every hold class, and refuses a CHANGES
  verdict or a verdict for a head the hold no longer names. The rule
  itself leaves a car to a person when the verdict comes from a run the
  car's review step was not dispatched to, or was recorded before the
  hold now on the car. Each refusal is written once onto the car's
  review step as `auto_release`, and the yard shows it beside the hold.
  A sibling rule (`60993cda`) asks again when a review hold is placed
  after its verdict. If the event is lost, the car stays held.
  David's per-car override is `boss hold --releaser`. `6fa9c790`
  (below) has since made that mark reserve the release for the named
  actor alone, not only against automation. Which cars are held at all
  is option A's question (`bd331d38`, 2026-10-09), measured and kept
  on 2026-11-15 (`e2f42de3`, "Both review holds stay" below).
- **Q2, launch on ready. Decided, not built.** The machine would launch
  a run only for a step whose agent block opts in (`launch = auto`),
  starting with held-car review and landed-review. Budgets stay on the
  block and the agents row, and sessions keep rescue and every red. It
  rides behind the launched agent as its own Job (`21062f95`). That
  program is built dormant, and David parked it on 2026-10-17, not
  closed, to return to option A, Codex in cluster Jobs (`b922bd17`,
  section "A launched agent runs as its own Job" above). The agent
  block has no `launch` key (`AgentSpec` refuses unknown keys). A later
  decision answers the same delay another way: an executor seated at a
  station pulls its next step, and a held car's review queues at a
  review station (`506e0a1c` above, 2026-11-09). Nobody has decided
  Q2's trigger again since then.
- **Q3, where the garage line falls.** The machine relaunches a lost
  gate once, on the same sha, with its park intent; it retires a landed
  twin; and it cancels a train whose two verdict halves are red. Agents
  rework a red gate, write or correct a proof probe, and write a hold.
  What the machine tries once and fails goes back to an agent with what
  it saw. Built:
  - the conductor's lost-gate relaunch (`boss_jobs::lost_gate`, backlog
    `565131e1`). A second loss on the same sha files one
    pipeline-failure item naming both runs, and stops.
  - landed twins. A consist that adds nothing to main retires each car
    as a landed twin (`81408a81`), and auto-park refuses a green whose
    change the record says landed (`98f35ac2`).
  - red trains. The judged-red cancel predates this design
    (`a2d4d842`, 2026-09-24). It acts once both halves have spoken, one
    is red and the red names its check. A red named by the forge alone
    with one culprit car followed on 2026-10-18 (`b4a95f47`).
  - publish `measure`. Its lesser item is also built: the dispatcher
    completes it from the drift reading
    (`publish-measure-lands-the-drift-reading`, backlog `f4bafd6b`).

**The map's reading. Half built.** Each region downstream of the shop
floor carries a `machine-moved X% · N hand-moved/day` KPI: gates, dock,
track, shed, arrivals, garage and publish
(`boss_jobs::regions::automation`, backlog `9ca41c08`). It counts the
`completed_by` of the step completions that leave the region, by actor
class, and keeps unattributed completions apart. It has a direction and
no target ratio. A step's declared `executor_class` (machine, agent or
human) is a Workflow field. When a row omits it, the class is read off
the agent block, `human_only` and the step kind. No workflow row in the
tree declares it.
Nothing yet compares it with `completed_by`, so the two flags the design
asked for are not built: a step declared machine but moved by hand, and
a step declared for hands that never needed one. Hand acts that are not
step completions, such as a release, a launch or a relaunch, are still
not counted. `moves::Cause` carries no actor. The reading is still keyed
by the IT map's region names, not by the station tree's (`4b27a65a`).

## Reserved car releases (6fa9c790 Q1, 2026-10-26)

A car's review-step `releaser` mark reserves release for that exact actor
and unique aliases of the registry row whose ID is that reservation.
An agent listing a person's ID as its alias gains no authority over that
person's reservation; naming an alias does not delegate back to its row.
Deleting or changing an existing reservation on a held review is judged
against the current reservation before the write, through both the CLI
and server metadata door. Placing a new reservation remains open.
Every other person,
agent or automation is refused at the shared release judge, with no
metadata or event written. The guarded write compares the raw reservation
under the review row's lock, so a reservation placed or changed after
judgment refuses the old write. A malformed reservation fails closed. Exact
named identity needs no alias lookup; missing, unavailable or ambiguous
registry evidence cannot grant an alias release. An absent reservation
keeps the ordinary review-based release contract.

The yard judges release for its signed-in viewer and uses the same door
as the CLI. A person's own shell or the yard's Release button attributes
the act to that person; a command typed into an agent's shell signs as
the agent and gains no authority from who typed it. The intended release
of car `10f1b77a` stands. This supersedes the automation-only reservation
rule in `393bdef6` Q1; it adds no passkey requirement or other permission.

## Both review holds stay; the cost is the wait after a verdict (e2f42de3, 2026-11-15)

Two holds decide which green cars wait at the dock for an adversarial
review before they board. The **trust-boundary hold** reads the item's
mark, an area from `infra/platform/trust-areas.toml`. The **path hold**
(backlog `fdbb447e`) is the gate's judge reading the diff: a car that
touches an ops verb, what a verb runs, the runner, a harness file, the
hold's own code, or a file that claims a review, its renderer or a file
it prints prose from. In practice it sweeps in common files such as
boss-cli's `main.rs`, `brief.rs`, `dispatch.rs` and
`infra/platform/documents/builder-rules.md`. On 2026-11-15 about 40
green cars stood held, trains left one car each (about 31 a day) and the
backlog stood near 1,190 items. The conductor's hypothesis was that the
path hold is too wide, and that anything outside the trust areas could
move to the post-merge `landed-review`.

**The measurement says no.** Design-doc "Narrowing the dock path-based
review hold: the measurement says keep it" (`e2f42de3`, David answered
Q1 as proposed on 2026-11-15) read the 1,000 most recent `agent-run`
packets closed within 7 days (of 1,326), with the reads taken before
21:39Z on 2026-11-15. 174 were reviewer runs on a car's `review` step.
The verdict came from each run's own report (CHANGES or RELEASE in its
first 700 characters). The hold class came from each run's own brief,
which carries the hold text verbatim at review time. A first pass read
the hold off the car instead, and was biased: a released car loses its
hold note, so released cars dropped out of the held classes and CHANGES
rates read 89–100%.

| Hold class at review time | Verdicts | CHANGES | Rate | P1 among CHANGES |
|---|---|---|---|---|
| Path hold only | 101 | 43 | 43% | 9 |
| Trust boundary | 39 | 18 | 46% | 4 |
| Neither named in the brief | 8 | 0 | 0% | 0 |

Landed reviews of cars that boarded unheld, same window: 98 read
verdicts, 27 with findings (28%): 1 P1, 4 P2 and 22 unrated, mostly
nonblocking coverage findings. A further 24 landed-review runs were
refused for independence or never read a verdict. So the path hold
finds defects at the trust hold's rate, with more P1s in absolute
count. It is not a false-positive hold. Moving it after the merge would
put about 9 P1s a week on main, where the landed-review loop finds less
and fixes later. Path-hold CHANGES that would have landed include
`3cf75d5b` (a run left at briefed with building pending was aged by no
rule), `53ca42f4` (the shared-checkout guard passed a bare relative
write) and `f82e386f` (a car-folds-into-carrier race).

**Decision: keep both holds as they are, and cut the wait after a
verdict instead.** The measured cost was not the review but what
followed it. A CHANGES verdict had no route back to its builder (car
`1d5379b5` sat 85 minutes; thirteen cars carried CHANGES from
2026-11-14 for more than a day), ready reviews held by nobody raised no
alarm, and reviews pooled on one or two named robots. Three cars answer
that:

- `ff8d4bef`, landed in train #1777: a review nominated to its own
  builder, or unclaimed for 2 h, returns to its station.
- `c2b21676` (car `53afc07d`): a CHANGES verdict returns the item's
  build to its station as a repair. In flight at the fold.
- `f0e604dc` (car `63699224`): an hourly per-station alarm for unheld
  ready reviews past their declared bound. In flight at the fold.

Still open: cars built for part of an item (`partial_item`) have no
repair route (`390f9e55`), and `6178a9b0`'s remaining split items, a
red gate whose builder's session is gone (`bbea9094`) and the instant
`step.assigned` twin (`c9cbf382`).

**Revisit on a number, not a feeling.** One week after `c2b21676` and
`f0e604dc` converge, measure again by the same method, which the record
reproduces. Add dock wait (green to boarded) by hold class and the
CHANGES-to-repair interval beside the CHANGES rates. If the path hold's
CHANGES rate has fallen below the landed-review rate (28% here), reopen
the question. Whether to trade defect-catching for throughput anyway
stays David's call.
