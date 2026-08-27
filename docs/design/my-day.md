# Design: what My Day owes an operator

**Status**: approved — every question answered in packet `e4270060`; carried to a file 2026-08-17.

**Origin.** David, 2026-08-16, after noting his day reads as "a
mishmash of things that are really on me and just jobs in the wrong
queue or not organized well enough yet":

> It makes sense for 'My Day' to be showing both jobs in my personal
> queue or jobs that I am eligible / matched for based on role or
> other policy/protocol requirements. We just need to do a good job
> with the UI to ensure users understand what is presented to them.
> I think it probably makes sense to have a special separation between
> jobs that are in a queue with a human-only policy with jobs that
> agents are also eligible for as a practical consideration.

## Three measurements, and each one moves the design

**1. Nothing in his day is gated on a person.** All 48 items in
`/api/jobs/assignments` for `emp-david` carry `authority_role: null`.
Zero. So the surface has no signal to sort on: an item is in his day
because a step got assigned to him, and nothing distinguishes "David
must judge this" from "somebody has to do this".

**2. There is no human-only concept in the system at all.** No
`human_only`, `requires_human`, `agent_eligible` — nothing, anywhere
in the tree. And the claim does not check actor KIND: it resolves an
`ActorId::Human` or `ActorId::Automation` for provenance, then gates
on role. Agents carry roles exactly as people do — the header this
investigation ran under is `platform-admin`. So `authority_role:
"platform-admin"` does not mean "a person"; it includes every agent
holding that role.

**3. The "eligible for" half is structurally empty.** Not broken —
empty. There are ZERO unassigned ready-or-active steps across every
open job in the system. The endpoint already returns a role-matched
up-for-grabs list and `assignments.ts` already splits on it; it has
nothing to put there, because the dispatcher assigns everything on
`step.ready`. Pull never happens because nothing is ever left to pull.

## What that means for the ask

The three things asked for are at different depths, and calling them
one task would hide that.

- *Show the personal queue* — already works.
- *Show what I am eligible for* — the surface exists and is empty.
  Filling it is not a UI change; it is queue-visibility Q5's `assign`
  vs `leave` decision, because a step nobody is assigned is the only
  kind that can be pulled.
- *Separate human-only from agent-eligible* — needs a concept that
  does not exist, and the concept is the interesting part.

## The tension worth naming

`the-three-layers.md` is deliberate that **actors are alike**: "Humans
and registered agents are the CPUs — nothing moves without an actor
claiming a step... Actors are not users of the system; they are the
part of it that executes." Capability is enforced at the claim, by
role, on purpose.

A human-only marker cuts across that. It might be a refinement — some
work requires judgement, accountability or legal signature that an
agent cannot supply, and that is a property of the WORK. Or it might
be a leak of a temporary fact — "we do not trust agents with this
yet" — into a permanent model, which would age badly and quietly.
Those two readings want different mechanisms, which is why this is a
doc rather than a field.

## Open questions

## Decision history

Reviewed as packet `e4270060` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Does ActorId stop being a closed Rust enum?**

Yes. It is the exact pattern CLAUDE.md §9 says to replace: a closed
enum modelling an extensible taxonomy. Roles already took that advice
and are Classes; actor category did not. Making category a Class means
policy and protocol predicate over it with the vocabulary they already
use for role, which is what David's reframing asks for — one
mechanism, not a special case for humans. The honest cost: ActorId is
in boss-core and stamped on every event in the audit log, so the
variants are not just a type — they are the shape of history. A change
here is a migration over provenance, and the log is immutable. Likely
answer is that the ENUM survives as the wire/log form while the
CATEGORY becomes a Class the enum maps onto, rather than a rewrite.

**Q2 — Do agents and automations become registered Subjects?**

Yes, and this is the load-bearing one. A human is an `employees.id`
foreign key — a Subject with Classes, carrying role. An automation is
a free-form slug and an agent is an unvalidated mode/model pair; the
enum's own comment concedes a registry is a separate design and defers
it. So policy CAN predicate over a human's attributes and CANNOT over
an agent's, which is why 'human-only' looks like a new concept instead
of an ordinary predicate. Register them and the asymmetry goes away:
`actor_kind = human` is then the same shape of clause as `role =
platform-admin`, and an agent can carry capabilities, a trust tier, an
owner — the things one actually wants to route on. Against: a registry
is a real surface with authoring, and free-form slugs have cost
nothing so far. The counter is that they have cost exactly this.

**Q3 — How does a step state its requirement — extend `authority_role`, or replace it?**

Extend rather than replace, and resist a second field.
`authority_role` already means 'this step waits for an actor matching
X'; today X can only be a role. Widening X to a small predicate over
actor attributes — `role = platform-admin AND kind = human` — keeps
one concept and one place to look. A sibling `human_only: bool` is the
tempting alternative and it is the wrong shape: it answers one
question with a boolean and cannot answer the next one (agent-only, a
trust tier, a named actor), so it would be joined by a second boolean
within a quarter. The station predicate already demonstrates the shape
that generalises — a small documented clause set, not a general
expression language.

**Q4 — What are My Day's sections, and what does each promise?**

Three, ordered by what they demand. **Yours to decide** — assigned to
you and requiring an actor only you satisfy; nobody else can move it.
**Yours** — assigned to you but an agent is eligible; shown so you can
hand it off rather than do it. **Open to you** — unassigned and you
match; a pull queue, not an obligation. Naming is most of the work.
'My Day' today implies everything shown is owed by the reader, and 48
items each implying that is exactly why it reads as a mishmash. A
section that says 'an agent could do this' does more than a filter,
because it tells the operator what to do about it. Only the first
section may ever be bold or badged; if three sections shout, none do.
Sequencing: section three stays empty until `assign` vs `leave` lands,
so it should ship labelled as such rather than looking broken.

**Q5 — What stops human-required from spreading?**

human-required should just be data in a protocol fundamentally, so we
don't need to worry about it spreading. We can always query active
protocols, analyze whether it is spreading, and then make changes to
protocols. Most of this bubbles up naturally from bottlenecks occuring
anyways.

