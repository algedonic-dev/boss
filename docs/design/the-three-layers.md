# The three layers

**Status**: living — the reading frame everything else is measured
against. Both opening questions were settled by David on the day it
was written; see Decision history.
**Origin**: David, 2026-08-13 (verbatim): *"The network is the
substrate, the fat protocols dictate the current operating model, the
actors run it."*
**Supersedes as the primary frame**:
[human-powered-state-machine.md](./human-powered-state-machine.md),
which is now a **lens** over this rather than the foundation.

---

## The statement

**The network is the substrate. The fat protocols dictate the current
operating model. The actors run it.**

Three layers, each replaceable without disturbing the others. That
separation is the whole point: it is what lets a company change how
it works without rebuilding what it works *on*.

## 1. The network is the substrate

The bottom layer knows nothing about breweries, invoices, or trains.
It knows:

- **Packets** — jobs. An immutable envelope, a fixed compatible
  protocol set at creation, and a payload. Packets do not mutate into
  other packets; crossing fabrics is **translation**, a new packet
  admitted with a `translated_from` edge.
- **Stations** — data-defined priority queues that route or hold
  packets until there is bandwidth or capability to handle them. The
  network's nodes. Actor, group, constraint, and batch stations are
  all the same row shape.
- **Routes** — motion between stations. Edges in the map.
- **The log** — every state change, immutably. Projections are pure
  functions of it; the substrate's memory *is* the log.
- **Admission** — the one edge where a packet enters and its protocol
  set is fixed.

This layer is physics. It has no opinion about what the work means.

## 2. The fat protocols dictate the current operating model

Workflows are protocols, and they are **fat**: the meaning lives in
the protocol, not in the endpoints. A station does not know what a
`ship-a-change` is. Neither does the code. The protocol row does — it
carries the steps, the predicates that order them, the evidence each
requires, the terminals that end them, and the obligations their
arrival creates elsewhere.

That is why protocols are **registry data**: append-only, versioned,
with in-flight packets pinned to the version they were admitted
under. Changing how the company works is publishing a row, not
shipping code. It is also why they can be **experimented on** —
two versions are two arms, cohorts are fixed at admission, and the
verdict is a query over the log.

"The **current** operating model" is the load-bearing word. The
protocols are not the system; they are what the system is *doing this
month*. A protocol that cannot be replaced without a deploy has
leaked into the substrate, and that leak is the defect to hunt.

## 3. The actors run it

Humans and registered agents are the CPUs. Nothing moves without an
actor claiming a step and doing it. Actors have:

- **Stations** — a personal queue, the same row shape as every other.
- **Capability** — skills and authority, enforced at the claim.
- **Bandwidth** — finite, which is why queues hold rather than drop.
- **Identity that includes what they are** — `[agent-mode]:[model]`
  for agents, because a different model is a different CPU.

Actors are not users of the system. They are the part of it that
executes. The software describes the machine and instruments it; the
actors *are* the machine running.

## What the framing buys

- Change the operating model without touching the substrate: publish
  a protocol version.
- Add actors without changing protocols: they arrive at stations.
- Measure everything uniformly: it is all packets, stations, and
  motion, so cycle time, queue depth, and loss are one vocabulary
  rather than per-feature metrics.
- Locate a defect by layer: a thing that should be data living in
  code is a protocol leak; a packet no station can present is a
  substrate failure; work that never gets claimed is an actor
  problem.

## Where the OS reading went

The "human-powered state machine OS" framing is not wrong and is not
discarded — it is **a lens**, and a good one for reasoning about
execution: the log as memory, the StepType registry as the
instruction alphabet, a step's status as a program counter, policy as
the privilege model. It answers "how does a single packet get
executed". It is simply not the substrate. The network is.

The practical consequence, and the reason this doc exists: when the
two framings disagree in a design argument, **the network framing
wins**, because it is the one that survives changing the operating
model.

## Decision history

**Q1 — the older frame documents: keep, but curation is a standing
obligation. Resolved 2026-08-13 (David):** *"Older docs can be kept
for now, but we need to remember that keeping the repo documentation
well-scoped and aligned will increase the likelihood it is read.
Everyone claims to love history, but we need it edited and curated
when it gets too big."*

So `human-powered-state-machine.md` and its peers stay, retitled as
lenses rather than foundations. The principle behind the ruling is
the load-bearing part and applies to every doc in this repo: **the
corpus is a working surface, not an archive.** Documentation is read
in proportion to how well-scoped and aligned it is, which makes
sprawl a correctness problem rather than an aesthetic one — an
unread invariant governs nothing. History is kept, but edited: when
the corpus grows past the point where a newcomer can find the
current truth, curating it is the work, not a distraction from it.

**Q2 — policy is part of the protocol. Resolved 2026-08-13
(David):** *"policy is definitely part of the protocol. I only keep
policy separate because actor governance is very top of mind for
people, but I think policy is certainly encoded as data, and I don't
think it needs to be treated differently than protocol in general
except we will definitely need tools for managing the policy aspects
of protocol specifically."*

Policy is **not a fourth layer**. Who may complete a step is part of
what the protocol *means*, exactly as much as what evidence the step
requires — and it is already data (`entitlements` on the
WorkflowSpec, rows in the policy registry). Its apparent separateness
is a fact about human attention, not about the architecture: actor
governance is what people ask about first, so it gets its own
vocabulary and its own page.

The consequence is a **tooling** obligation rather than a structural
one: policy needs first-class surfaces for authoring, reviewing and
auditing the policy *aspect* of a protocol — who can act, on what,
under which scope — without that aspect being modelled as a separate
kind of thing underneath. Enforcement stays where it is; the
authoring story is what has to converge.

## Open questions

_None — Q1 and Q2 resolved above; further questions arise from the convergence pass and the policy tooling work._
