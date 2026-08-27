# Design: P, Q and M — naming the three domains of getting work to actors

**Status**: approved — every question answered in packet `73697169`; carried to a file 2026-08-17.

**Origin.** David, 2026-08-16, after observing that "we have a muddy
queuing and matchmaking process happening still":

> Q should have a registry of every required station, which are both
> concrete actor queues and constraint-based queues where any actor
> that meets constraints can act against it. We have lots of protocols
> with steps with constraints, so we should have lots of stations to
> show. Dispatcher — and I think I want to use another letter
> signifier to distinguish this domain clearly like we are doing with
> 'Q', and I like 'M' for matchmaker now — should be querying stations
> for load and performs matchmaking support both as a regularly
> scheduled motion and ad hoc as needed. However, protocols that
> specify specific hand-offs into new queues should be automatically
> handled by the Protocol/Policy/Publish service, which I think we
> just call 'P' now.

## The measurement that makes the case

| | |
|---|---|
| active workflows | 45 |
| steps across them | 302 |
| steps carrying a constraint (`authority_role`) | **178** |
| distinct roles named | **22** |
| distinct `(step-kind, role)` constraints | **51** |
| stations that exist | **3** |

Fifty-one distinct constraints, three stations. And the three are
hand-seeded rows: `design-review`, `loading-dock`, `repair` (holding
zero packets), plus the one actor station `my-watchlist`.

The consequence is already visible. There are **zero unassigned
ready steps** anywhere in the system: with no station to hold work,
the dispatcher assigns everything on `step.ready`, so pull never
happens and the "up for grabs" list the assignments endpoint already
returns is structurally empty. The station layer was designed for a
world with many stations and is being run with three.

**Stations are not a thing to author. They are a projection of the
protocols.** A protocol step that says "a `bookkeeper` does a
`bill-approval`" has already declared a queue; writing a station row
by hand to say it again is the fact living twice that CLAUDE.md §9a
is about. That is why there are three: hand-authoring 51 was never
going to happen.

## The three domains

Today one service does two jobs and a third has no owner. The
dispatcher's own rule set splits almost cleanly along the seam
already: **45 rules fire on `step.done`** — protocol side-effects,
what happens next — and **17 on `step.ready`** — getting work to an
actor. Those are P and M.

```
        protocol rows                    P — Protocol / Policy / Publish
     (Workflow, policy, plugins)         owns: what a protocol MEANS.
              │                          Publishes protocol-specified
              │  declares constraints    hand-offs into queues; the
              ▼                          authority on who MAY act.
        Q — the queue layer
        registry of every required       Q — owns: where work WAITS.
        station, derived from P's        Concrete actor queues, and
        constraints. Reports depth,      constraint queues any matching
        load, age.                       actor can act against.
              │
              │  load + eligibility
              ▼
        M — the matchmaker               M — owns: who acts NEXT.
        queries Q for load, matches      Scheduled sweeps AND ad hoc
        actors to packets. Scheduled     on demand. Holds no state; a
        and ad hoc.                      pure function of Q and actors.
              │
              ▼
        actors claim (the CAS gate)
```

The division that makes this worth naming: **P says what may happen,
Q says what is waiting, M says who goes next.** Today the dispatcher
answers the first and third, the station layer half-answers the
second, and the My Day client re-answers the third on its own.

## What each letter takes from today's code

- **P** — the `step.done` rules (45), the Workflow registry, policy,
  step plugins. The hand-off case David names is P's: a protocol that
  routes a packet into a named queue is stating protocol, not making
  a matchmaking decision, so it should not travel through M at all.
- **Q** — `stations`, but generated from P's constraints rather than
  seeded. Depth, load and age become readable, which is what M needs
  and what nothing currently exposes.
- **M** — the `step.ready` rules (17) and the assign strategy. The
  key change is that M becomes a QUERY over Q rather than a reflex on
  an event: today assignment happens because a step became ready, not
  because anyone asked who should do it.

## Open questions

## Decision history

Reviewed as packet `73697169` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Are stations DERIVED from protocol constraints, or authored?**

Derived, and the numbers settle it: 51 distinct (step-kind, role)
constraints exist across 45 active workflows, and 3 stations have been
authored. Hand-authoring 51 was never going to happen, and the gap is
not laziness — it is that the protocol ALREADY declares the queue. A
step saying 'a bookkeeper does a bill-approval' has stated a
constraint queue; a station row repeating it is CLAUDE.md §9a's fact
living twice, and the copy has already drifted to 3-of-51. So Q's
registry is a projection of P, regenerated when protocols change, with
authored rows reserved for stations that are NOT implied by a
constraint — my-watchlist is the worked example, since 'packets I
filed' is not a step constraint. The open edge: a derived station has
no natural place for wip_limit, discipline or terminal_window_days,
which are operational choices rather than protocol facts. Likely
answer is a derived skeleton plus an authored overlay keyed on the
same name — but that is two sources for one row, so it needs care.

**Q2 — What exactly moves out of the dispatcher, and what stays?**

The seam is already visible in the rule set: 45 rules fire on
`step.done` and 17 on `step.ready`. The first group is protocol side-
effects — what happens next — and belongs to P. The second is getting
work to an actor and belongs to M. That is close to a clean cut and
should be tested against every rule before being believed. The deeper
change is not which rules move but WHEN M runs. Today assignment is a
reflex on an event: a step became ready, so someone gets it. David's
framing makes M a QUERY over Q's load — scheduled sweeps plus ad hoc —
which is what allows leaving a packet unassigned for pull, and pull is
the half of the model that is currently unreachable. What stays in
neither: the claim CAS. That is the substrate's own gate and belongs
where it is.

**Q4 — Do protocol hand-offs bypass M entirely?**

Yes, and this is the sharpest line in David's framing. A protocol that
names its next queue has already made the routing decision; sending it
through a matchmaker would let M second-guess a statement of protocol,
which is the layer leak the three-layers reading exists to prevent. P
publishes it into the named queue and M never sees it. M's domain is
only the case where the protocol names a CONSTRAINT rather than a
destination — 'a bookkeeper does this' — where somebody has to choose
which bookkeeper, or leave it for one to pull. The consequence to
accept: a mis-specified hand-off is now unrecoverable by matchmaking.
If a protocol routes into a queue nobody can act on, the packet waits
forever and no load-balancing will save it. That argues for the
publish-time refusal the queue-visibility Q5 proposal already names —
refuse a route into a queue no actor matches, at publish, not at
runtime.

**Q5 — What happens to the three stations that exist?**

`design-review` and `repair` become derived and stop being authored —
both are constraint queues that a protocol already implies, and
`repair` currently holds zero packets, which suggests it describes
work no protocol routes to it. `loading-dock` is the train's dock and
is genuinely operational rather than protocol-derived, so it stays
authored. `my-watchlist` stays authored and is the proof that the
authored escape hatch is needed: 'packets I filed' is not a step
constraint and never will be. Worth doing as the FIRST slice of this
work rather than the last, because it is small, it exercises both
halves of the registry, and it answers Q1's open edge — whether a
derived skeleton plus an authored overlay is workable — on four real
rows instead of in the abstract.

**Q3 — Are P, Q and M services, crates, or vocabulary?**

Vocabulary first, crate boundaries second, separate services probably
never. The value David is reaching for is that a question has ONE
owner — 'why is this packet in front of this actor' currently has four
half-answers — and a named domain buys that without a network hop.
Against splitting into services: M must read Q's load constantly and
P's constraints on every regeneration, and three chatty services where
one process would do is the distributed monolith this repo has
otherwise avoided. The existing tier layout already supports domain
boundaries inside one deployment. Recommend: rename to the letters in
prose and diagrams NOW, since that is the part that changes how people
think and costs nothing; move code when a boundary is being violated,
which the derived-station work will make obvious.

