# Design: workflow UX as data — when vanilla is the exception

**Status**: in-review — open questions tracked at `/system/design`.
**Source**: David, 2026-08-09: "having vanilla UX for a workflow or
step is going to become the exception rather than the norm… my
department app will want to build custom views for the workflows we
engage with, to triage efficiently via drag and drop… and useful
Step UX for any real work that needs to occur during a step. That
way I can pull in and render all the useful contextual data riding
along with the job."
**Related**:
[architecture-decisions.md](../architecture-decisions.md) §Step UX &
frontend (which also holds the folded Views / department-apps
decisions) · [queue-visibility.md](./queue-visibility.md) ·
[public-api-mcp.md](./public-api-mcp.md)

---

## The claim, named

The StepPlugin decision already settled this question for *steps*:
surfaces ship as data (a registry row + a JS bundle), the SPA never
changes, the generic card is the fallback. David's claim extends it
one level up — *workflow-scoped* views (a department's triage board,
a drag-and-drop queue, a kind-specific pipeline) will outnumber the
vanilla surfaces too. The core surfaces that shipped this week
(TriageBoard, TriageFlow, Fleet) should be read as the **floor**:
always present, always correct, and increasingly not what a
department actually works in.

## What exists

- **StepPlugins** — the settled per-step mechanism: append-only
  versioned `step_plugins` rows; bundles served at `/plugins/<path>`;
  steps pin the plugin version at creation; framework-free mount
  (`window.__boss_register_step_plugin(kind, mount)`;
  `mount(container, {step, jobId, onUpdate, currentUser})`); the
  platform surface, then the generic fields/notes card, as fallback.
- **What the host does NOT provide**: an API client. Every plugin
  fetches for itself — including the step PUT whose top-level
  metadata replace wipes unmentioned keys. The merge-safe discipline
  is documented in TriageBoard's `patchStep` because it once made
  cards vanish; today each plugin must rediscover it.
- **Core generic surfaces** — TriageBoard (fork columns), TriageFlow
  (per-step queues on the DAG, routing by edge — parsed from
  `ready_when`), Fleet (per-step depth for any kind). All core code.
- **The kit fragments** — the rules that have already drifted once
  and now live exactly once: `fork.ts` (which step carries the
  decision), `position.ts` (which node a Job sits at — pinned to the
  server's grouping), `workflowToDag` (edges + routing conditions),
  `StepDag`. These are the pieces a custom view would otherwise
  reimplement wrong.
- **The Views decision** (2026-08-05): local scratch state is fine
  while it stays local; the moment it flows into a Job, Step, or
  Event it is subject to the same rules as everything else.
- **Server-side guardrails**: policy on every write, `ready_when`
  gating, required-at-done validation. A view cannot invent a
  transition — the server refuses moves the Workflow doesn't declare.

## The shape

Extend the StepPlugin principle to workflow-scoped surfaces: a
**workflow-view registry** — kind-keyed, append-only versioned rows
naming a surface slot (`queue` | `board` | `flow` | detail panels)
and a bundle — mounted by the same host pattern, with the core
generic surfaces as the permanent fallback. A department app then
*is* data all the way down: its nav entries (already registry-shaped),
its workflow views (rows + bundles), its step surfaces (StepPlugins),
its saved Views. Core ships instruments and the kit; departments
ship views.

## The considerations

1. **The state machine is what makes custom UX safe.** Drag-and-drop
   routing is presentation over the same legal transitions — dropping
   a card on "build" completes the fork step with
   `disposition = "build"` through the same PUT everything uses, and
   a drop the predicates don't allow is refused by the server, not by
   the view's good manners. Custom views can be arbitrarily wrong and
   the model stays right. This is the whole reason the norm *can*
   flip to custom: the alphabet is enforced below the UX.
2. **The host must hand plugins the sharp-edged primitives, once.**
   The metadata-merge PUT, fork reading, position grouping, the DAG
   with routing edges — every one has a drift incident behind it.
   A plugin contract that provides a small client (`readJob`,
   `patchStep` merge-safe, `route`, the kit components for those who
   want them) turns "every plugin rediscovers the trap" into "the
   trap is unreachable." This is the single highest-leverage decision
   in this doc.
3. **Contextual data is already riding along; the contract is the
   schema.** `metadata_schema` on the Workflow and the StepType
   `fields` registry describe exactly the typed data a view can
   render. A schema-driven renderer in the kit covers the long tail;
   bespoke rendering is for the cases that earn it.
4. **Published contracts are promises** (the MCP doc's Q5, arriving
   here early): plugin props and the provided client version like the
   schema layer — additive freely, destructive in two steps. Views
   pin versions the way steps pin plugin versions today.
5. **Vanilla as floor is a guarantee, not a default.** The generic
   surfaces must always render every kind — the auditor's view, the
   debugging view, the view that works when a bundle is broken.
   Custom views are additive; nothing may *depend* on one existing.
6. **CI has to crawl what it can't import.** The route-smoke crawl
   covers core surfaces; plugin bundles are opaque to it. Plugin
   surfaces need their own smoke contract (a mount-under-adversarial-
   mock harness, or a deferred-with-reason discipline) or the "norm"
   becomes a fleet of unguarded UX.

## Open questions

### Q2: What exactly does the host hand a view?

Proposal: the step-plugin props shape, plus a provided client —
`readJob`, merge-safe `patchStep`, `route(edge)`, `listAtNode` — and
optional kit components (StepDag, the schema renderer). Decides
consideration 2. Sub-question: does the existing step-plugin host
retrofit the same client, so the trap closes everywhere at once?

### Q3: Is drag-and-drop a kit interaction or per-view freedom?

One curated dnd-routing component (drop targets = the routing edges
of the current node, exactly TriageFlow's semantics) keeps the
decision-graph reading consistent everywhere and is testable once.
Per-view freedom lets a department invent interactions the kit
never imagined. These compose — kit component first, freedom
allowed — unless we decide consistency matters more.


Proposed: **kit component first, per-view freedom allowed** — the
compose option the paragraph above already names, chosen deliberately
rather than by default.

The reason to prefer it is that the curated component is the one that
can be made CORRECT once. Drop targets being the routing edges of the
current node is not a styling choice; it is a claim about the
protocol, and getting it wrong means an operator drags a packet down a
branch the workflow does not have. That belongs in one tested place.

The reason not to forbid the alternative is that we have no example of
a department wanting something else, and forbidding on speculation is
how a kit becomes a cage. When one appears it will say what it needs,
and a plugin bundle is already the escape hatch for exactly this.

Worth stating plainly: this is a taste call more than an evidence one,
and it is cheap to revisit — the kit component is a component, not a
protocol, so nothing pins to it.

### Q4: How do plugin surfaces get gated in CI?

Options: a plugin smoke harness (mount each registered bundle
against the adversarial mock, fail on pageerror — the route-smoke
contract extended to bundles); publish-time validation only
(schema-checked row, untested mount); or deferred-with-reason per
plugin. The step-plugin fleet is small today; the decision sets the
bar before the fleet grows.


Proposed: **the plugin smoke harness, in the CI-gated mocked suite** —
and the measurement says this is not a close call.

Nine bundles ship in `infra/step-plugins/`. CI mounts exactly ONE of
them: `tests/smoke/step-plugin.spec.ts` drives `sr-triage`, and it
lives in `tests/`, the live-backend suite that needs a seeded stack —
not `tests/mocked/`, the fast deterministic set that actually gates.
So eight bundles reach production having never been mounted by
anything automated.

The harness to fix it already exists and costs nothing to run.
`playwright.mocked.config.ts` needs no backend and no seeding, and a
spec renders any surface as any persona in about four seconds. The
route-smoke contract there — fail on `pageerror`, fail on a shell that
never paints — is exactly the bar a bundle needs, applied per
registered plugin rather than per route.

Two things from the last day argue it is worth doing now rather than
when the fleet grows. The `answer-question` bundle shipped and the
first confirmation it rendered at all was a human opening the page.
And `review-design` carried a silent-swallow bug — a failed
pending-decisions write logged to the console and continued — in a
file no automated mount ever loaded; a harness that fails on console
errors would have surfaced it.

Deferred-with-reason stays available per plugin, but it should be the
exception recorded on the row, not the default that arises from having
no harness.

### Q5: Who may publish a view, and as whom?

Same question as design-docs-as-data Q4 and MCP Q2, arriving at a
third door: department apps will be authored substantially by
agents. Policy rule on `(publish, ui-plugin)`, a `publish-a-view`
Workflow, or role-scoped authoring at `/system/step-plugins`'s
successor. Should resolve consistently with those two.



Proposed: **a policy rule on `(publish, ui-plugin)`, and nothing
else** — the same answer David gave to the same question one door
along.

He resolved design-docs-as-data Q4 as "the policy rule on
`(publish, design-doc)`, and nothing else — because the Job-mediated
version already exists and is not this." The reasoning transfers
without modification: a view's *thinking* is already reviewable work
if anyone wants it to be (a feedback packet, a design review, a car),
and a `publish-a-view` Workflow would be a second protocol over the
same act, indistinguishable from the first on the board.

Not a role Class either, for the reason roles are Classes of what a
person IS rather than of a verb they may perform — adding one per verb
is how a taxonomy inflates until nobody can say what a role means.

The consequence to accept out loud, because it is sharper here than
for docs: department apps will be authored substantially by agents, so
a policy rule alone means an agent publishes a view that operators
then look at, with only the registry's schema check between the two.
That is already the trust posture agents have for Workflow rows and
station rows, both of which move packets rather than pixels — so it is
consistent. But Q4 above is what makes it safe rather than merely
consistent: a published bundle that CI has mounted is a different
proposition from one nothing has ever loaded.

## Decisions

### Q1: One UI-plugin registry, or a sibling table per slot? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> `step_plugins` could generalize to a `ui_plugins` registry with a
> `slot` column (`step:<kind>` | `workflow:<kind>:<surface>`), or a
> sibling `workflow_views` table keeps the two lifecycles apart. One
> registry means one authoring surface, one version discipline, one
> serving path; the split means simpler rows. Precedent (the Class
> registry: one table, kind-keyed) leans single-registry.

One registry. `step_plugins` (03-jobs.sql) is the single table and carries nine active rows today. The sibling-table-per-slot alternative was never built and nothing has needed it since.

## Decision history

_None yet._
