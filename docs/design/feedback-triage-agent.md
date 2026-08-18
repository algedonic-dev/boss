# Design: What kind of agent should triage feedback?

**Status**: approved — open — evidence gathering. No implementation work yet (2026-08-18).
**Related**: [human-powered-state-machine.md](./human-powered-state-machine.md) ·
[extending-boss.md](./extending-boss.md)

---

## Why this doc exists

The triage board at `/system/feedback` has an agent slot: "Hand to
agent" writes `agent_requested_at` on the triage step and the card
moves to **With an agent**. Nothing consumes that record yet. The
question is what should — a handful of deterministic rules, or a
language model.

That question is not answerable from an armchair, because the answer
depends entirely on what real feedback looks like, and we have no
corpus. So we are answering it the empirical way: **humans play the
agent, by hand, on a periodic cadence, and record what each item
actually required.** When the table below shows a clear split, the
design follows from it rather than from taste.

This is the same move the repo makes everywhere else — decide from the
log, not from intuition about the log.

## What a triage pass actually does

Derived by running the procedure rather than by imagining it. Every
pass answers four questions in order, and each one is either a lookup
or a judgment:

| # | Question | Kind |
|---|---|---|
| 1 | **Classify** — defect, capability request, question, or noise? | partly mechanical |
| 2 | **Locate** — which surface and which crate owns it? | mechanical |
| 3 | **Check prior art** — already fixed, already known, duplicate? | judgment |
| 4 | **Dispose** — no-op, needs a human, actionable now, or spawn work? | judgment |

Step 2 is worth calling out: it is already free. The Job carries
`metadata.route`, and `nav-catalog.ts` maps every route to its
department app. Routing a card to an owner is a join against data that
exists, not an inference — so whatever agent we build, routing should
not be the part we pay a model for.

Steps 3 and 4 are where every judgment has landed so far. Both need to
know what shipped recently, which is repo state rather than anything
in the feedback text.

## Evidence

One row per hand-processed item. `Rule?` asks the load-bearing
question: could a deterministic rule with access to the Job, the route
catalog, and the open-PR list have reached the same disposition?

| Item | Route | Class | Disposition | Rule? |
|---|---|---|---|---|
| `efc423f2` | `/system` | capability request | Satisfied by #190 + #191; no code change | **No** — needed to know what shipped |
| (unfiled) | `/system/feedback` | defect | Triage step used a kind it could never satisfy; fixed at the spec | **No** — needed the StepType registry |
| `50f70a1f` | `/system/feedback` | defect | `color: inherit` on a bar that sets no colour; 1.06:1 in light theme. Fixed + pinned | **No** — needed to read four components' CSS |
| `41afd152` | `/system/feedback` | capability request | Drag-and-drop triage; raises whether the board should be generic. Decision is the owner's | **Partly** — routing yes, the insight no |
| `811c5dc5` | `/system` | defect | `/api/*` miss falls through to the SPA as 200 HTML instead of a JSON 404 | **No** — needed gateway routing |
| `8c55d799` | step focus | defect | Full-page step route demanded a plugin; fell back to the platform surface | **No** — needed the surface dispatcher |
| `bd500848` | job page | defect | Same cause as above, older link shape | **Partly** — a dedupe rule could have paired them |
| `74cbe627` | `/system/workflows` | defect (agent-filed) | Version pin defeated by in-place reconcile; both halves fixed | **No** — needed reconcile + re-eval together |
| `f91831a6` | `/system/monitoring` | defect | IT tab's landing page rendered in Home chrome: the route→section ternary emitted camelCase ids that miss the kebab-case catalog keys; map extracted to a typed module + the section→catalog half pinned | **No** — the text says "still in home"; the cause is a vocabulary drift two files away |
| `15c6004e` | `/system/feedback` | defect | "Flashing periodically": the 15s polls added that morning call load(), which flips `loading` and re-renders the surface into its spinner every tick — the flash WAS the poll; background refreshes made silent in both triage surfaces | **No** — the report describes the symptom; the cause is a state flag in a poll added the same day, invisible to any rule |
| `39d5bfde` | `/system/flow` | capability | "Visualize job flow through IT's queues": the operator's message IS the design decision (dashboards Q2 node-set + Q4 absorb-into-Flow); built as composition of the shipped instruments (kind DAGs + job_edges links + fleet depth + stage durations) | **Partly** — routing was mechanical once read as a decision; the recognition that the message answers open design questions is the model-shaped part |
| `823fcb22` | `infra/docker` (agent-filed) | defect | CI smoke flake, invoices never seeded: investigated from code to TWO mechanisms — the rules runner one-shot-accepting an empty table as final (fixed: wait_for_rules), and the launcher starting the sim with no dispatcher-readiness gate (fixed: readyz gate); the always-captured diagnostics decide which one the next real flake wears | **No** — the report describes a timeout; both causes live in boot-order code the text never mentions |
| `af1586e1` | `crates/core/boss-events` (agent-filed) | capability | Event kinds are the only primitive without a registry (120 live vs ~19 declared): routed `design`; the doc's substance came from measuring the kind space's SHAPE (static kinds + dynamic families whose suffix domain is another registry) | **No** — the disposition was mechanical but the design content needed the live-log measurement and the compositional insight |
| `1e576baf` | `/it/dispatcher` (agent-filed) | defect | Authored rules silently inert until restart: diagnosing meant reading the runner's consumer model (one durable over coarse first-token wildcards) to learn a registry swap + rebind covers it; fixed as a fingerprint-poll supervision loop rebuilding both runners | **No** — the right fix shape depended on how subscriptions bind, which lives in runner code the report never mentions |
| `2f2565fb` | inbox (agent-filed) | defect | The FIRST live done-notification (rule from 106) announced itself as "Ready:" — the shared handler hardcoded the ready wording; verb now follows the triggering topic, pinned by a test reproducing the exact live message | **Partly** — a rule could flag "done topic, Ready subject" as inconsistent, but locating the fix needed the handler's template |
| `aa9980c8` | `crates/core/boss-jobs` (agent-filed) | defect | All four #219 passengers closed with Merged SKIPPED: the re-evaluator inferred "provably unsatisfiable" from step terminality for predicates that also reference job.metadata, and update_job never re-evaluated at all — the audit log pinpointed the 134ms boarding-to-close sequence; both halves fixed and pinned | **No** — the diagnosis ran from the log through reevaluate's skip inference to a missing reeval call site; nothing in "outcome skipped" points there |

### Notes per item

**`efc423f2`** — "a test to see whether we can effectively develop via
browser feedback." Names three capabilities: filing feedback from the
browser (shipped, #190), an IT-app Kanban triage page (shipped, #191),
and processing items with agent help (this loop). Nothing to build;
the item is its own acceptance test and it passed.

The pass was cheap but not mechanical. Mapping "I want to process
items with the help of agents" onto "that is the open PR you are
reading this from" required knowing the state of the tree. A rule
matching keywords would have routed it to IT correctly and then had
nothing useful to say about it.

Caveat on n=1: this is the least representative item the corpus will
ever contain — it is feedback about the feedback system, filed by the
person building it. It should carry almost no weight in the verdict.

**The unfiled defect** — trying to close the item above returned
`400 invalid step metadata: document_title: required field
'document_title' is missing`. The triage step had shipped as an
`acknowledgment`, a kind meaning "confirm receipt of a policy or
document", and metadata validators run at `completed` — so the Job
materialized cleanly, sat in the waiting column looking healthy, and
failed only when a human first tried to triage it. Fixed by moving
the step to `task`, which is what the work actually is and requires
no metadata; pinned by a spec test that reproduces the operator's
exact error at authoring time.

Worth noting for Q1: dispositioning this needed the StepType field
schema, the Workflow spec, and the rule that validators fire at
completion. None of that is in the feedback text, and no amount of
classifying the text would have reached it. But also note what
*found* it — an operator clicking a button, not an agent reading a
queue. An agent of either kind would likely have filed this under
"works as intended" until it tried the write itself.

Standing caveat: the rows are still mostly the feedback system
talking about itself. The verdict needs items about the rest of BOSS
before it means anything.

### The split that is starting to show

Five hand-processed items in, the "can a rule do it" answer is not
uniform — it separates cleanly by **class**, which is more useful
than a single verdict:

- **Defects need repo comprehension.** Every one so far was
  dispositioned by reading code the feedback text never mentions —
  a StepType field schema, four components' CSS, gateway route
  fallthrough. The reporter describes a *symptom*; the disposition
  lives in the cause, and nothing in the text points at it. No rule
  bridges that gap, and a model without repo access would not either.
- **Capability requests mostly need routing.** `41afd152` wants
  drag-and-drop. A rule could classify it, route it to IT, and stop —
  and that would be the *correct* handling, because the decision it
  needs is a human's, not an analyst's. Nothing is gained by having
  something clever read it first.

If that holds, the shape is not "simple vs LLM" but a triage on the
triage: classify cheaply, route mechanically, and spend
comprehension only on the items whose disposition is a claim about
the code. That would also make the expensive path auditable, since
every model invocation would be attached to a defect with a named
cause.

What would falsify it: a run of defect reports whose fix is obvious
from the text alone ("this button is the wrong colour" needs no
investigation if the reporter names the button), or feature requests
that turn out to need real analysis to route. Neither has appeared
yet, and five items is not enough to say.

### A repeated shape of incident is itself a signal

The strongest finding of the session is not in the table. Three
separate items — a step kind that could never complete, a board that
could not find its fork step, and two Jobs that would not close — were
dispositioned as three unrelated bugs. They were one: **a Job
materializes its steps once and keeps them, so anything that changes
the Workflow underneath it strands the Job.** The root cause was a
Workflow refresh rewriting a live version in place while Jobs pinned to
it kept resolving that version.

Each item on its own looked like a one-off, and triaging item-by-item
is what kept it looking that way. Nothing in the feedback text could
have revealed it; it only appeared by noticing that three dispositions
rhymed.

That is an argument about what a triage agent is FOR. Classifying and
routing one item at a time — the part a rule does well — is precisely
the part that cannot see this. Whatever we build should be able to ask
"have I seen this shape before", which means it needs the history of
dispositions, not just the item in front of it.

### What the loop keeps costing

Every pass so far has produced a paragraph of reasoning with nowhere
to go — see Q2. It has now bitten twice: the disposition for
`50f70a1f` includes the actual root cause, and the card shows only
the original complaint. Anyone else opening the board sees three
untouched-looking items and no sign that two are diagnosed and one is
fixed. This is the strongest evidence so far that Q2 is not
cosmetic.

## Open questions

All 2 open questions were resolved 2026-08-18 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---

## Decisions

### Q2: Where does an agent's finding go? — ANSWERED (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Today the board records that an agent was *asked* (`agent_requested_at`)
> but has nowhere to put what the agent *found*. Every hand pass so far
> has produced a paragraph of reasoning that lives only in this doc,
> which does not scale past the experiment and is invisible to the
> operator looking at the card.
>
> Answered by the passes rather than by choosing. Across eight
> hand-processed items a finding was consistently two things: **a root
> cause** — a claim about the code that the feedback text never mentions
> — and **what was done about it**, usually a commit. Never a structured
> verdict, never a proposed Job. So it is free text with provenance, and
> a schema would have been invented rather than observed.
>
> Built as an optional `finding` field declared on the triage step, so
> it is self-describing data rather than a board convention: the generic
> step surface renders it from the same contract, and no second place
> has to be taught about feedback. Optional because a finding is
> evidence, and triage can legitimately route something obvious without
> one.
>
> Two properties the build had to preserve. A finding is **not a
> decision** — writing one leaves the item in triage, because finding
> something and deciding what to do about it are different acts, and
> collapsing them was the original modelling error behind "With an
> agent" being a column. And it **outlives routing**, rendering on
> routed cards too; otherwise the reason a card went where it went
> disappears the moment it gets there.
>
> Provenance (`finding_by`) is recorded for the same reason the
> hand-off record is: an agent taking an automatic first pass writes the
> identical shape, and the surface should not care which wrote it.

Answered in this doc's own body — 'answered by the passes rather than by choosing' — and since shipped: both `triage` and `investigate` declare a `finding` field on the step, so a root cause and what was done about it live on the step that produced them rather than in prose here. The heading has said ANSWERED for some time; this is the tracker catching up.

### Q1: Does feedback triage need a language model, or do rules suffice? (resolved)

Resolved 2026-08-18 — override.

**The question was:**

> The evidence table decides this. The shape to watch for: if most items
> are dispositioned by *route + class + a duplicate check*, rules win and
> an LLM is expensive ceremony. If most need "is this already fixed, and
> does the described behaviour match what the code does" — that is repo
> comprehension, and rules cannot fake it.
>
> A likely third answer is a split: rules do classification, routing, and
> duplicate detection deterministically; a model is invoked only for the
> residual that a rule declines. That would keep the cheap path cheap and
> make the expensive path auditable, which is the same shape as the
> pushdown seam in `boss-views` — push down what is mechanical, evaluate
> the residual honestly.
>
>
> Proposed: **rules, for the one thing rules can do; no model for the
> rest, and the rest is nearly all of it.** The evidence table this
> question asked for now exists — 161 real (non-simulated) feedback
> packets, 135 of them triaged:
>
> | disposition | count |
> |---|---|
> | build | 62 |
> | design | 48 |
> | reproduce | 23 |
> | duplicate | 1 |
> | decline | 1 |
>
> The mechanical class is **2 of 135**. Everything else is a judgement
> about whether an item is a code change, a decision, or something that
> must be reproduced first — and that judgement turned on repo
> comprehension every time it was interesting. `a001c78a` was `build`
> rather than `design` only because reading the workflow's predicates
> showed the routing was already decided and wrong. `0b8ae875` was
> `build` because David had already answered its shape twice in another
> doc. `bedda461` needed the corpus measured — 11 of 23 docs stale —
> before anyone could say the report was real. No rule over route, class
> and text reaches any of those.
>
> So the hoped-for split in the paragraph above inverts: it is not
> "rules do the bulk, a model takes the residual". Here the residual IS
> the bulk, and a rule engine would earn its keep on 1.5% of traffic.
>
> **The caveat that keeps this honest, and it is a big one.** This
> corpus is mostly self-filed: 86 packets from the operator baseline, 48
> from agents, and only 6 from guests or anonymous visitors. Public
> feedback has a noise profile this sample does not contain — blank
> submissions, "it's broken", the same complaint five times — which is
> exactly the class rules serve. So the finding is *"rules cannot triage
> the traffic we have"*, not *"rules cannot triage feedback"*.
>
> What follows: build the deterministic **duplicate check** now, because
> it is cheap, auditable and the one mechanical thing here; do not build
> a model-backed triage agent for this traffic; and re-run this table
> when guest volume exists, since that is the population the question was
> really about.

We don't need to make a real choice here. This is matter of protocol and policy, which is expressed in data and so should not affect any designs.


### Q3: Should the agent be allowed to close an item? (resolved)

Resolved 2026-08-18 — override.

**The question was:**

> An agent looking is deliberately not a decision — the card stays in
> flight, and only a human completes the triage step. Whether that
> should stay true depends on Q1's answer. If rules handle a clean
> majority with high confidence, letting them auto-close noise (blank
> submissions, duplicates of an open item) is a real saving. If every
> disposition needs judgment, the human stays in the loop and the agent
> is an assistant that drafts.
>
> Note the policy angle: the triage step is gated by
> `authority_role: platform-admin`, which is what stopped the sim
> workforce from completing these Jobs the moment they went ready. Any
> auto-close path has to hold that gate, not route around it.
>
>
>
> Proposed: **no.** Not because closing is wrong in principle, but
> because the saving is not there. Auto-closing noise is worth having
> when noise is a large class; on the measured corpus above it is 2
> packets in 135. Buying 1.5% at the price of a wrong close — a real
> report dismissed by a rule, with the filer told it was a duplicate —
> is a bad trade, and the failure is silent in exactly the way this
> system keeps being bitten by.
>
> Keep the `authority_role: platform-admin` gate as-is. It is what
> stopped the simulated workforce completing these Jobs the moment they
> went ready, and any auto-close path has to hold it rather than route
> around it.
>
> Worth distinguishing from something that already ships and looks
> similar: `complete-feedback-branch-on-car-merged` DOES close a packet
> without a human, and that is correct — it closes on **evidence that
> the work was done** (a car naming the packet merged), not on a
> judgement that the item was worthless. Closing on evidence and closing
> on an opinion are different powers, and the protocol should keep
> granting only the first.
>
> Revisit alongside Q1 if guest volume ever makes the noise class real.

This is also outdated thinking. The protocol will allow what the protocol allows.


## Decision history

_None yet._
