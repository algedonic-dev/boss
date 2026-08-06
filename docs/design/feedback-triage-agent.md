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
schema, the JobKind spec, and the rule that validators fire at
completion. None of that is in the feedback text, and no amount of
classifying the text would have reached it. But also note what
*found* it — an operator clicking a button, not an agent reading a
queue. An agent of either kind would likely have filed this under
"works as intended" until it tried the write itself.

Standing caveat: both rows are still the feedback system talking
about itself. The verdict needs items about the rest of BOSS before
it means anything.

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

### Q2: Where does an agent's finding go?

Today the board records that an agent was *asked* (`agent_requested_at`)
but has nowhere to put what the agent *found*. Every hand pass so far
has produced a paragraph of reasoning that lives only in this doc,
which does not scale past the experiment and is invisible to the
operator looking at the card.

The obvious shape is a note on the triage step plus a rendered card
section, since the step already carries the hand-off record. Deferred
until a few more passes show what a finding needs to contain — a
free-text note, a structured disposition, a proposed Job, or all
three. Building the field before knowing its shape is how it ends up
holding the wrong thing.

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
