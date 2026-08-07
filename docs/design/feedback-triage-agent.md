# Design: What kind of agent should triage feedback?

**Status**: open — evidence gathering. No implementation work yet.
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

### Q1: Does feedback triage need a language model, or do rules suffice?

The evidence table decides this. The shape to watch for: if most items
are dispositioned by *route + class + a duplicate check*, rules win and
an LLM is expensive ceremony. If most need "is this already fixed, and
does the described behaviour match what the code does" — that is repo
comprehension, and rules cannot fake it.

A likely third answer is a split: rules do classification, routing, and
duplicate detection deterministically; a model is invoked only for the
residual that a rule declines. That would keep the cheap path cheap and
make the expensive path auditable, which is the same shape as the
pushdown seam in `boss-views` — push down what is mechanical, evaluate
the residual honestly.

### Q2: Where does an agent's finding go? — ANSWERED

Today the board records that an agent was *asked* (`agent_requested_at`)
but has nowhere to put what the agent *found*. Every hand pass so far
has produced a paragraph of reasoning that lives only in this doc,
which does not scale past the experiment and is invisible to the
operator looking at the card.

Answered by the passes rather than by choosing. Across eight
hand-processed items a finding was consistently two things: **a root
cause** — a claim about the code that the feedback text never mentions
— and **what was done about it**, usually a commit. Never a structured
verdict, never a proposed Job. So it is free text with provenance, and
a schema would have been invented rather than observed.

Built as an optional `finding` field declared on the triage step, so
it is self-describing data rather than a board convention: the generic
step surface renders it from the same contract, and no second place
has to be taught about feedback. Optional because a finding is
evidence, and triage can legitimately route something obvious without
one.

Two properties the build had to preserve. A finding is **not a
decision** — writing one leaves the item in triage, because finding
something and deciding what to do about it are different acts, and
collapsing them was the original modelling error behind "With an
agent" being a column. And it **outlives routing**, rendering on
routed cards too; otherwise the reason a card went where it went
disappears the moment it gets there.

Provenance (`finding_by`) is recorded for the same reason the
hand-off record is: an agent taking an automatic first pass writes the
identical shape, and the surface should not care which wrote it.

### Q3: Should the agent be allowed to close an item?

An agent looking is deliberately not a decision — the card stays in
flight, and only a human completes the triage step. Whether that
should stay true depends on Q1's answer. If rules handle a clean
majority with high confidence, letting them auto-close noise (blank
submissions, duplicates of an open item) is a real saving. If every
disposition needs judgment, the human stays in the loop and the agent
is an assistant that drafts.

Note the policy angle: the triage step is gated by
`authority_role: platform-admin`, which is what stopped the sim
workforce from completing these Jobs the moment they went ready. Any
auto-close path has to hold that gate, not route around it.

## Decision history

_None yet._
