# Design: framing convergence — bringing the corpus onto the three layers

**Status**: in-review — open questions tracked at `/system/design`
**Origin**: David, 2026-08-13 (verbatim): *"The network is the
substrate, the fat protocols dictate the current operating model, the
actors run it."*
**Purpose**: this is an **audit plan**, not a framing doc. The framing
is [the-three-layers.md](./the-three-layers.md); this document says
which files in the corpus still speak the old frame, quotes the
offending sentence, and proposes the minimal edit. It exists to be
worked through and then deleted.
**Incorporates** the-three-layers.md's two resolutions (David,
2026-08-13): **Q1** — older frame docs are kept, retitled as lenses,
but *"the corpus is a working surface, not an archive"*, so sprawl is
a correctness finding (§Scope and sprawl below). **Q2** — **policy is
part of the protocol, not a fourth layer**, which makes every
"Policy is a supporting concept alongside the primitives" statement
stale framing (§Policy as a peer below).
**Related**: [the-three-layers.md](./the-three-layers.md) ·
[human-powered-state-machine.md](./human-powered-state-machine.md) —
now the execution lens ·
[stations.md](./stations.md) · [job-packet-network.md](./job-packet-network.md)

---

## The bar

A file is measured against the three layers:

- **converged** — already speaks substrate / fat protocols / actors.
- **compatible** — a mechanism or contract doc that never states a
  frame. Nothing to do.
- **stale** — states the old framing as primary, points at
  `human-powered-state-machine.md` as *the* frame, or (after Q2)
  presents Policy as a peer subsystem alongside protocols.
- **contradictory** — would actively mislead someone reasoning from
  the three layers (denies a layer, or describes something the new
  frame makes first-class as a code path or a UI concern).

## The count

| Class | Files | Edits needed |
|---|---|---|
| Converged | 10 | 0 |
| Compatible | 26 | 0 |
| Stale | 8 | 8 (all small; 1 done) |
| Contradictory | 5 | 5 (2 done in this car) |
| **In scope** | **49** | **13** |

Q2 (policy is part of the protocol) moved one file —
`protocol-policy-publish.md` — from **converged** to **stale**, and
added a second, independent stale statement to four files already
classified (`CLAUDE.md`, `docs/architecture-decisions.md`,
`docs/architecture-diagram.md`, `human-powered-state-machine.md`).
See §Policy as a peer.

Scope is `docs/**/*.md` (42 — 34 under `docs/design/`), `README.md`,
`CLAUDE.md`, and the five root governance files (`CONTRIBUTING`,
`TODO`, `CHANGELOG`, `SECURITY`, `CODE_OF_CONDUCT`) — plus the Rust
`//!` module headers that state framing, called out separately below.
**36 of 49 files need no framing edit at all.** The old frame is
concentrated in a handful of high-traffic front doors, not spread
through the corpus.

Of the 34 design docs: 10 converged, 16 compatible, 5 stale, 3
contradictory. Every doc authored since 2026-08-09 is already
converged on the *network* axis — the drift is entirely in the
pre-network corpus and in the three front doors (`CLAUDE.md`,
`README.md`, `docs/index.md`). The *policy* axis is the opposite
shape: the newest docs are the ones that name Policy as a peer,
because that is the vocabulary the system was built with.

---

## Contradictory — fix first

### 1. `docs/design/queue-visibility.md` — denies the substrate's nodes ✅ FIXED 2026-08-13

The sharpest conflict in the corpus, and it is also the most
factually stale doc (see §Factual errors).

Line 121: quote — *"**A queue is a lens — a WHERE clause over the one
steps projection — never a reified structure.**"*
Line 149: quote — *"**Not per-actor queue storage.** No queue tables,
no broker state per actor."*

Under the three layers a **station** *is* a reified registry row —
`infra/postgres/schema/116-stations.sql` shipped, `boss-jobs` owns it,
`stations.md` Q1–Q4 ratified. The doc's "never a reified structure"
now reads as an argument against the substrate's own nodes.

Proposed minimal edit — the distinction it was reaching for survives
intact, one level down:

> **A station is registry data; its *membership* is a predicate, not
> a stored roster.** The station row is real (name, discipline,
> capability gate, `wip_limit`); what is derived is who is in its
> queue right now — evaluated over packet state at read time, so
> there is no second source of truth to drift from `steps` and
> nothing to rebuild. A *view* over a station is a lens; the station
> itself is a node in the network.

And line 149's bullet becomes **"Not per-actor queue *storage* — a
station row declares the queue; membership is still derived."**

### 2. `CLAUDE.md` §Project Overview — names the wrong primary frame

Lines 5–17 open the highest-traffic file in the repo with the old
frame. Quote (lines 16–17):

> *"Executors are humans and agents — the "human-powered state machine
> OS" framing is the executor model on top of the abstraction."*

and line 11: *"an event-sourced, state-machine-shaped OS for
describing real-world organizations directly."*

The §Reading frame section below it was converged in this car, so the
file now contradicts itself across two screens. Proposed: line 11
becomes *"an event-sourced network for describing real-world
organizations directly — see §Reading frame"*, and lines 16–17
become:

> Actors — humans and registered agents — are the CPUs that run it;
> the "human-powered state machine" reading is the **execution lens**
> over the three layers, not the foundation (§Reading frame).

Also line 52 (*"a generic state-machine modeling toolkit that happens
to be tuned for human + agent executors"*) → *"a generic network for
modeling work, tuned for human and agent actors."*

### 3. `docs/design/human-powered-state-machine.md` — **DONE in this car**

Was the declared reading frame. Header now states it is a lens over
the three layers and that the network framing wins on disagreement.
Body deliberately untouched.

**Residual proposal** (not done): §"Invariants this framing gives us"
I-2 cites `owner_id` on Jobs as a named-CPU carrier. Under the new
frame accountability relocates to the station (job-packet-network Q1,
resolved). Propose a one-clause addition to I-2, not a rewrite.

### 4. `docs/architecture-diagram.md` §0 — an executed decision that never executed

Line 39: `## 0. The framing — State · Surfaces · Work`
Line 59: *"This is MVC stretched to company scale…"*

`job-packet-network.md` **Q7 was resolved 2026-08-12 — accept**:
*"docs/architecture-diagram.md redraws on the network vocabulary and
becomes the one diagram the README and the canvas legend both cite."*
That decision has not been executed. Diagram 0 still presents
State/Surfaces/Work as *the framing*.

Proposed: retitle §0 to `## 0. The framing — the three layers`,
lead with the substrate/protocols/actors statement, and demote
State · Surfaces · Work to a **rendering split** (which it is — it is
how the SPA is organised, not what the system is). The `.mmd` source
`docs/architecture/00-state-surfaces-work.mmd` gets redrawn in the
same change; this is the largest single item in the plan and deserves
its own car.

### 5. `docs/design/extending-boss.md` — teaches the lens as the foundation

Status `stable`, and `docs/index.md` sends every newcomer here for
"see the four primitives". Line 22:

> *"BOSS models a company as a state machine. The four foundational
> primitives are Subject, Job, Step, and Event…"*

Proposed replacement:

> BOSS models a company as a network: packets move between stations
> under fat protocols, and actors run it. The four foundational
> primitives name the pieces — Subject, Job (the packet), Step, and
> Event — and this doc focuses on the **registry layer** that carries
> the protocol.

The rest of the doc (the extensibility ladder) is already
frame-neutral and needs no change. One further line: the code block
at line 34 describes Workflow as *"the program written in the
StepType alphabet"* — propose *"the protocol: the steps, their
ordering predicates, the evidence each requires"*, with the
program/alphabet reading kept as a parenthetical.

---

## Stale — small edits

| File | Line | Quote | Proposed |
|---|---|---|---|
| `README.md` | 33 | *"Event-sourced, state-machine-shaped…"* | **DONE** — now states the three layers, and the "Underneath" paragraph names each layer. |
| `README.md` | 143 | *"**Core state-machine OS** — `crates/core/`"* | *"**Core network substrate** — `crates/core/`. Packets, stations, routes, the log, admission, the protocol registries."* |
| `docs/index.md` | 8 | *"Event-sourced software for modeling systems as state machines."* | *"Event-sourced software for modeling an organization as a network: the network is the substrate, the fat protocols dictate the current operating model, the actors run it."* |
| `docs/index.md` | 32 | *"the human-powered state-machine frame"* | *"the three-layer frame (plus the human-powered state-machine execution lens)"* — and add a `the-three-layers.md` row to the "Start here" table, above the CLAUDE.md row. |
| `docs/architecture-decisions.md` | 28 | *"model the operating system of a company directly as a state machine"* | *"model the operating system of a company directly as a network of packets, stations, and fat protocols"* |
| `docs/architecture-decisions.md` | 60 | *"both instantiations of company-management on the same state-machine abstraction"* | *"…on the same substrate"* |
| `docs/design/platform-vs-tenant-jobkinds.md` | 63–64 | *"The framing in `CLAUDE.md` is 'BOSS is software for modeling systems as state machines.'"* | Quotes a CLAUDE.md line this plan changes — re-quote the three-layer statement. The argument that follows (alphabet vs programs) survives as *substrate vs protocols*, which makes it **stronger**: platform ships the substrate, tenants author protocols. |
| `docs/design/correctness-protocol.md` | 205 | *"human-powered-state-machine.md — the framing this all sits inside."* | *"the-three-layers.md — the framing this all sits inside"*, keeping the h-p-s-m link as *"the execution lens"*. |
| `docs/design/seed-vs-emergent-state.md` | 124 | *"human-powered-state-machine.md — the framing this principle falls out of: Jobs are the program…"* | Same swap. Add: *"a seed that writes a projection row is a packet that never crossed admission"* — the seed rule states more cleanly in the new frame than the old. |
| `docs/design/workflow-ux-as-data.md` | 74 | *"**The state machine is what makes custom UX safe.**"* | *"**The protocol is what makes custom UX safe.**"* — the argument (the server refuses a transition the predicates disallow) is unchanged and is a protocol-layer argument, not a state-machine one. |

`docs/design/sse-policy.md` line 20 (*"the view's primary signal is
state-machine-shaped"*) is borderline. Propose leaving it — it
describes a wire-level cadence choice, and "state-machine-shaped" is
doing real work there that "network-shaped" would not.

---

## Policy as a peer — newly stale after Q2

**Q2 resolved: policy is part of the protocol, not a fourth layer.**
Who may complete a step is part of what a protocol *means*, exactly
as much as what evidence the step requires — and it is already data
(`entitlements` on the WorkflowSpec, rows in the policy registry).
Its apparent separateness is a fact about human attention, not about
the architecture.

That makes every "Policy is a supporting concept *alongside* the
primitives" sentence stale. Five places say it:

| File | Line | Quote | Proposed |
|---|---|---|---|
| `CLAUDE.md` | 59–61 | *"The Class registry …, StepPlugins …, and Policy (the privilege model) are supporting concepts that hang off the four."* | Move Policy out of the supporting-concepts list: *"…and Policy — the actor-governance **aspect of a protocol**, carried as `entitlements` on the WorkflowSpec plus registry rows. It is not a separate kind of thing; it gets its own vocabulary because governance is what people ask about first."* |
| `CLAUDE.md` | ~255–261 | §Supporting concepts: *"These three hang off the four primitives."* + the Policy bullet | Same move: two supporting concepts (Class registry, StepPlugins) plus a Policy subsection under the protocol heading. |
| `docs/architecture-decisions.md` | 69 | *"Three supporting concepts hang off them: … **StepPlugins** …, and **Policy** (row-level privilege rules)."* | Same. §Policy & auth (line 461) stays exactly as-is — it documents enforcement, which is unchanged. |
| `docs/architecture-diagram.md` | 88, 101 | *"The Class registry, StepPlugins, and Policy are supporting concepts on top."* / *"**Cross-cutting rails.** Policy gates every write."* | Line 88 same move. Line 101 is **fine** — "gates every write" is an enforcement fact, not a framing claim. |
| `docs/design/human-powered-state-machine.md` | ~138 | *"**Policy** — privilege model on CPUs. Every write passes through."* | Leave. Inside an explicitly-labelled lens, "privilege model on CPUs" is the lens's own correct vocabulary. The header edit already scopes it. |
| `docs/design/protocol-policy-publish.md` | title + line 21–24 | *"**Protocol** evaluates it against the packet's pinned protocol set …; **Policy** checks the actor may perform it; **Publish** stages the consequences"* | The three-stage admission pipeline is right; the *naming* now implies Policy is Protocol's peer. Propose reframing the middle stage as **the protocol's actor-governance aspect** evaluated at the same edge — the doc keeps its title (it is a good name for the service) but says once, up front, that Policy is a facet of Protocol rather than a second authority. This doc is otherwise fully converged. |

**The tooling gap this creates.** Q2's consequence is explicitly a
tooling obligation, not a structural one: *"we will definitely need
tools for managing the policy aspects of protocol specifically."*
Today there is a workflow-authoring surface (`/system/workflows`) and
a policy subsystem with its own rules and `policy_rule_audit`, and
**nothing that lets an author see, review, or audit the governance
aspect of one protocol in one place** — who may act, on what, in
which scope, for this workflow version. Enforcement is fine; the
authoring story is what has to converge.

That is a design doc worth writing and is deliberately **not**
written here. It should answer at minimum: does the workflow
authoring surface grow a governance tab reading through to the policy
registry, or does policy authoring grow a per-protocol lens? Does
publishing a protocol version pin its entitlements the way it pins
its steps? And does the policy aspect become part of the protocol
diff an experiment compares (protocol-experiments.md)? **Do not
restructure `boss-policy` or its docs before that doc exists** — the
resolution changes how policy is *presented and authored*, not how it
is enforced.

## Scope and sprawl — the curation findings

Q1's ruling is that older frame docs stay, but with a standing
obligation: *"keeping the repo documentation well-scoped and aligned
will increase the likelihood it is read … we need it edited and
curated when it gets too big."* **An unread invariant governs
nothing**, so these are correctness findings, not tidiness. Ordered
by how much they cost a newcomer.

### S1. Seven places tell you what BOSS *is*

To learn the frame, a newcomer can land on any of:
`the-three-layers.md`, `human-powered-state-machine.md`,
`CLAUDE.md` §Project Overview, `CLAUDE.md` §Reading frame,
`README.md` opening, `docs/index.md`, `docs/architecture-decisions.md`
§Thesis & positioning, and `docs/architecture-diagram.md` §0. Eight,
counting both CLAUDE.md sections. Before this car they disagreed;
after the full plan they would agree, which is *worse in one
specific way* — eight maintained copies of one paragraph is exactly
the drift shape CLAUDE.md §9a legislates against.

**Recommendation:** one canonical statement
(`the-three-layers.md`), and everywhere else carries a **one-line
quote plus a link**, not a restatement. The three-sentence framing is
short enough to quote verbatim, which is what makes this viable.

### S2. Eight docs cover queues, stations, and lenses

`queue-visibility.md` (211 lines), `stations.md` (109),
`requirements-based-addressing.md` (195), `views-as-queue-lenses.md`
(116), `job-packet-network.md` (230), `it-activity-network.md` (141),
`departure-board.md` (129), `department-flow-dashboards.md` (132) —
**1,263 lines written across five days**, each partly superseding the
one before, none retired. The doc that describes what actually
shipped (`stations.md`) is the *shortest*; the doc that contradicts
it (`queue-visibility.md`) is the longest and the oldest and is what
`docs/index.md`-shaped navigation surfaces first.

**Recommendation:** `stations.md` becomes the one current-truth
station doc and gets promoted to `living` once its status is
corrected. `queue-visibility.md` keeps only its measurement section
(Q1 below). `views-as-queue-lenses.md`, `departure-board.md`, and
`department-flow-dashboards.md` are rendering docs and should say so
in their first line so nobody reads three of them looking for the
data model.

### S3. The flatten process has not run, so "current truth" is not current

`docs/architecture-decisions.md` declares itself *"the one
current-truth decision record"* and CLAUDE.md §Design docs promises
that each release settled material folds in and the source doc is
deleted. Meanwhile **11 resolved decisions sit un-flattened** —
`job-packet-network.md` carries 7 (Q1–Q7, all resolved 2026-08-12)
and `stations.md` carries 4 (Q1–Q4, resolved 2026-08-13). The
owner/status decision, the packet-translation decision, and the
station-registry decision are all load-bearing and none of them are
in the record that claims to be the record.

**Recommendation:** run the flatten before writing anything new. This
is the single highest-value curation act available, because it is
already the declared process — it just has not executed.

### S4. `CLAUDE.md` states its crate roster twice, and both copies are wrong

§10 (lines 205–222) and §Rust Conventions (lines 286–334) both
enumerate the tiers. Both say 27/16/5 where the truth is 29/18/6,
both omit `boss-dispatcher-handlers`, and the "0 violations across 27
core crates" line makes a third copy. **CLAUDE.md §9a's own rule
condemns this**: *"a fact that lives twice gets an equality test …
Prefer collapsing."* The roster is generatable from
`Cargo.toml` workspace members.

**Recommendation:** collapse to one roster, and pin the counts with a
test that reads `Cargo.toml` — the repo already has the mechanism
(`boss-testing/build.rs` generates `SCHEMA_FILES` from
`manifest.txt`, the worked example §9a cites).

### S5. `queue-visibility.md`'s "measured today" is eight days stale and reads as present tense

Its §"What exists today, measured (playground, 2026-08-08)" states
four things that shipped since (§Factual errors 1). A dated heading
does not protect a reader who greps for "claim primitive" and finds
*"No claim primitive."*

**Recommendation:** measured sections get a **freshness contract** —
either a re-measure date or a pointer to the query that regenerates
them. A number in prose with no way to refresh it is a claim with a
short half-life presented as a fact.

## Converged — no edits

`stations.md`, `job-packet-network.md`, `it-activity-network.md`,
`requirements-based-addressing.md`, `views-as-queue-lenses.md`,
`departure-board.md`, `department-flow-dashboards.md`,
`protocol-cadence.md`, `protocol-experiments.md`,
`deployment-as-network.md`.

(`protocol-policy-publish.md` was here and moved to **stale** on Q2 —
converged on the network axis, stale on the policy axis. It is the
only file whose classification Q2 changed outright.)

Several of these carry a `human-powered-state-machine.md` link in
their **Related** line. That link is now correct as written (it is a
real related lens) and needs no edit — only the docs that call it
*the framing* do.

## Compatible — no edits

`class-registry.md`, `schema-migrations.md`, `projection-rebuilders.md`,
`transactional-audit-log.md`, `subject-identity-and-relationships.md`,
`testing-strategy.md`, `sse-policy.md`, `event-kind-registry.md`,
`design-docs-as-data.md`, `gateway-audit-events.md`, `idm-kanidm.md`,
`payload-encryption.md`, `public-api-mcp.md`, `feedback-triage-agent.md`,
`dev-cluster.md`, `internal-forge.md`, `docs/formal/README.md`, the four
runbooks, `CONTRIBUTING.md`, `TODO.md`, `CHANGELOG.md`, `SECURITY.md`,
`CODE_OF_CONDUCT.md`.

## Rust module headers

Only one `//!` header states the frame rather than the crate's job:

`crates/core/boss-core/src/actor.rs` — *"Boss is a human-powered state
machine (see `docs/design/human-powered-state-machine.md`). Invariant
**I-2** says every transition has a named CPU."*

Proposed (one line, no code change): *"Actors are layer 3 of the three
layers (`docs/design/the-three-layers.md`) — the CPUs that run the
network. The execution lens's invariant I-2 says every transition has
a named one."* The I-2 citation and everything below it stay.

`crates/core/boss-jobs/src/stations.rs`,
`crates/core/boss-jobs/src/station_queue.rs`, and
`crates/orchestrators/boss-cli/src/cadence.rs` already speak the
network frame — they are the model to copy, not to change.
`crates/core/boss-core/src/lib.rs` and
`crates/core/boss-events/src/lib.rs` have **no `//!` header at all**
— the two most foundational crates are the two with no framing. A
short header on each is the cheapest place to state the substrate.

---

## Factual errors found (independent of framing)

These are worth more than the framing work and should be fixed
regardless of what happens to the frame.

### Contradicted by shipped code

1. **`docs/design/queue-visibility.md` is four claims out of date.**
   - Line 68: *"**My Day does not call it.** The page pulls
     `/api/jobs?status=open&limit=200`…"* — **false.**
     `apps/web/src/me/assignments.ts:91` calls
     `/api/jobs/assignments?assignee_id=…&roles=…`.
   - Line 78: *"**Nothing anywhere passes `roles=`.** The group lens
     … has zero consumers today."* — **false**, same line.
   - Line 90: *"**No claim primitive.** Ready→Active is a plain PUT."*
     — **false.** `POST /api/jobs/{id}/steps/{step_id}/claim` ships
     (`crates/core/boss-jobs/src/http/steps.rs:770-787`) with the CAS
     and the station capability gate.
   - Consequently **open questions Q1 and Q2 are both shipped and
     answered** but still listed as open, so the tracker is showing
     two live questions the code already decided.

2. **`CLAUDE.md` crate counts are wrong on three of four tiers.**
   Claims 27 core / 16 modules / 5 orchestrators (lines 206, 286,
   301, 314) and repeats "27 core crates" at line 338. Actual:
   **29 / 18 / 6** (2 tenants is correct); 55 workspace members.
   The orchestrators list omits `boss-dispatcher-handlers` entirely;
   the modules list omits `boss-campaigns` and `boss-customers`; the
   core list omits `boss-search` and `boss-views`.
   `docs/architecture-diagram.md` lines 149/159/167/180 repeat the
   same stale numbers.

3. **`README.md` test count is understated by ~40%.** Lines 257 and
   273 claim *"~1,640 Rust `#[test]` cases"*. Actual: **1,190
   `#[test]` + 1,160 `#[tokio::test]` = ~2,350**.

4. **`CLAUDE.md` §TypeScript Structure omits `libs/web-kit`.** The
   block lists only `apps/web` and `apps/simulator`, and the prose
   asserts *"There is no `libs/shared-types/`"* — but `libs/web-kit`
   exists and is imported as `@boss/web-kit` by both apps (it owns
   `PacketCard`, the card grammar the network UI is built on). The
   "no shared types lib" rule is still true for *types*; the
   structure block needs the third entry.

### Dangling references

5. **`docs/design/protocol-cadence.md:13`** cites
   `[clock-as-service (docs/design/clock-as-service.md)]`. **That file
   does not exist** anywhere in the tree. It is also malformed as a
   markdown link, so no link check would catch it.

6. **`docs/architecture-diagram.md:200`** cites
   *"designed under `external-financial-actors.md`"*. **No such file.**

7. **`infra/blueprints/ssh-ca/`** is cited twice as somewhere an
   operator should look —
   `docs/runbooks/dev-environment-bootstrap.md:279` (*"see
   `infra/blueprints/ssh-ca/README.md`"*) and
   `docs/runbooks/operator.md:315` (*"restore the blueprint at
   `infra/blueprints/ssh-ca/`"*). **`infra/blueprints/` does not
   exist.** These are break-glass procedures pointing at nothing.

8. **`docs/runbooks/operator.md:402`** instructs *"Write down what
   happened in `docs/runbooks/incidents/`"*. Directory does not exist.

9. **`docs/design/dev-cluster.md:74`** states *"**Machine configs in
   the repo** (`infra/cluster/talos/`)"* as present fact.
   `infra/cluster/` exists (`builder`, `manifests`, `wireguard`) but
   has **no `talos/`**.

10. **`infra/codebase-stats.sh:55`** iterates a `crates/bridges` tier
    that does not exist — it silently counts zero, so the stats it
    prints are quietly incomplete rather than loud.

### Status drift

11. **`docs/design/stations.md` says `**Status**: in-review`** while
    `infra/postgres/schema/116-stations.sql` and
    `crates/core/boss-jobs/src/stations.rs` both record *"Q1–Q4
    ratified 2026-08-13"* and the registry, the queue evaluator, the
    two HTTP routes, and the `/system/map` page have all shipped. It
    should read `shipped` (its Open questions section is already
    empty, so the lint permits the flip).

12. **Stations ship read-only and barely seeded.** Only two `batch`
    rows exist on the platform (`loading-dock`, `design-review`);
    there are **no actor, group, or constraint rows anywhere in the
    tree**, and no authoring API — `GET /api/stations` and
    `GET /api/stations/{name}/queue` are the whole surface. Any doc
    implying a live station-per-actor network is ahead of the code.
    `stations.md`'s *"every executor has one"* is the design, not
    today.

13. **`simulated` is a first-class column on `jobs` only.** On the
    event side it is a `_simulated` marker **inside the payload
    JSONB**, not a column — `audit_log` has exactly
    `id, event_id, timestamp, source, kind, payload, created_at,
    prev_hash, row_hash`. A doc calling it an audit-log column would
    be wrong; none currently does, and none should start.

14. **The cadence loop retired the *train's* timers, not systemd.**
    `boss train cadence` (`crates/orchestrators/boss-cli/src/cadence.rs`)
    now drives the conductor off `cadence_rules` rows, and
    `infra/train/` has no `.timer` files. But **12 systemd timers
    remain** elsewhere (backup, audit-integrity, ledger recognize and
    replay-check, files-gc, messages purge, search reindex, views
    catchup, deploy-confirm, conservation-invariants, ML batch,
    cluster-deploy-runner). "Every protocol internalized" is the
    direction, not the state; no doc should claim systemd is gone.

15. **"Jobs don't have owners or status anymore"**
    (`job-packet-network.md:5`) is a verbatim quote of the aspiration,
    and the doc is honest about it — its own resolved Q1/Q2 say
    `owner_id` and `status` both stay. Flagged only so no *other*
    doc lifts that sentence as present-tense fact:
    `jobs.owner_id` and `jobs.status` are both `NOT NULL` columns
    (`infra/postgres/schema/03-jobs.sql:16-19`) with indexes.

---

## Open questions

### Q1: Does `queue-visibility.md` get rewritten or flattened into `stations.md`?

Its measurement work (Little's-law WIP bound, the 0.15%-in-flight
number, the two-different-"too large"s split) is genuinely good and
worth keeping. Its architectural conclusion — queues are lenses,
never structures — is what `stations.md` overturned, and three of its
"what exists today" claims and both of its live open questions are
now shipped. Options: (a) rewrite in place onto stations, keeping the
measurements; (b) fold the measurements into `stations.md` and delete
it; (c) mark it `superseded` and leave it as a record.

Proposed: **(a) rewrite in place.** The doc's real subject is *queue
economics*, which no other doc covers and which the station registry
does not answer. Deleting it loses the only measured argument in the
corpus for why queues stay cheap.

### Q2: Does "core state-machine OS" become "core network substrate" in the tier names?

The tier vocabulary appears in `CLAUDE.md` (§10, §Rust Conventions),
`README.md` §What's in the box, `docs/architecture-diagram.md`
§2, and `docs/architecture-decisions.md` §628 — four files, one
phrase, and it is also the name people say out loud. Renaming it is
either the single highest-leverage convergence edit or gratuitous
churn on a phrase everyone already parses correctly.

Proposed: **rename**, in one car, all four files together. "Core
state-machine OS" names the lens where the tier's actual contents —
packets, stations, routes, the log, admission, the protocol
registries — are the substrate. Leaving it means the tier that *is*
layer 1 is named after layer 2's lens.

### Q3: Does the policy-aspect-of-protocol tooling doc get written before or after the convergence edits?

Q2 on the-three-layers.md resolves that policy is part of the
protocol and names a **tooling** consequence: surfaces for authoring,
reviewing, and auditing the governance aspect of a protocol. Nothing
in this plan writes that doc, and the prose edits proposed in §Policy
as a peer describe an authoring story that does not exist yet — they
tell a reader that policy is a facet of protocol while every surface
in the app still presents it as a separate subsystem.

Proposed: **write the tooling doc first**, or at least concurrently.
Converging the prose ahead of the surfaces makes the docs describe
an intent rather than the system, which is the specific failure the
Orwell lineage in §Founding ideas exists to prevent. The prose edits
are cheap and can wait a car; the doc is where the real thinking is.
Enforcement stays untouched either way — `boss-policy` and its
docs are explicitly out of scope until that doc exists.
