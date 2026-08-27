# Design: holding the implementation to the design

**Status**: approved — in-review (2026-08-16).
**Origin**: David, 2026-08-13: *"Our biggest risk is that we aren't
holding our actual software implementation to the ideals of our
design."*
**Related**: [the-three-layers.md](./the-three-layers.md) ·
[correctness-protocol.md](./correctness-protocol.md) ·
[packet-loss.md](./packet-loss.md)

---

## The risk is measured, not hypothetical

One day's audit and one flatten run found these, all in docs that
read as authoritative:

| claim | reality |
|---|---|
| "every executor has an actor station" (`stations.md`) | two seeded `batch` rows; no actor, group or constraint rows; no authoring API |
| "a queue is a lens … **never a reified structure**" (`queue-visibility.md`) | the station registry ships as rows |
| "group→role mapping is a registry, not gateway code" (`idm-kanidm.md`) | `oidc.rs` deliberately has no such registry — roles come off the employee row |
| "systemd demoted to supervision" (implied by cadence work) | the *train's* timers retired; **12 systemd timers remain** |
| crate roster: 27 / 16 / 5 (`CLAUDE.md`, twice) | 29 / 18 / 6 |

The flatten agent stopped trusting the corpus and verified every
folded decision against schema and code. That is the correct
instinct and also the diagnosis: **prose asserts, and nothing
checks.**

## What already works

This repo has been converting ideals into enforcement for a while,
just not systematically. Each of these is a design claim with teeth:

- `tier-import-audit.sh` — "Tier 1 must not depend on Tier 2."
- `no-step-kind-match.sh` — "new work types are rows, not `match`
  arms."
- `dispatcher-rules-ratchet.sh` — "the reactive rule count only
  shrinks."
- `no-wallclock.sh` — "nothing reads the wall clock but the clock."
- `no-secrets.sh` — "credentials never enter the tree."
- `schema-converge.sh` — "every deploy entry point converges the
  schema."
- the workflow viability lint — "a protocol must be able to finish."
- `boss-testing/build.rs` — "the schema list has one definition."

The pattern is proven. What is missing is the **register**: a claim
with no enforcement is indistinguishable, at a glance, from one with
enforcement.

## The mechanism

**Every load-bearing invariant declares how it is held.** Three
honest answers, in descending order of strength:

1. **Enforced** — a lint, a test, or a type makes violation
   impossible or loud. Name it.
2. **Checked** — verified periodically by inspection, by a protocol
   that produces findings (the `design-conformance` run below).
   Name when it was last verified.
3. **Unenforced** — nothing holds it. This is not a sin; it is
   *debt*, and writing it down is what makes it visible instead of
   ambient.

An invariant with no declared enforcement is the defect this doc
exists to eliminate — not because the invariant is wrong, but
because nobody can tell whether it is still true.

## The conformance run

A periodic protocol, in the shape the retro and flatten protocols
already use: **survey** the register → **check** each `checked`
invariant against the code → **record** drift as findings →
**escalate** the ones that need a judgement call (is the doc wrong,
or the code?) → review.

Crucially, drift has two possible repairs and an agent must not
choose silently: either the implementation is behind the design and
owes a car, or the design is aspirational and the doc owes an
edit. Naming which is a judgement call — so conformance findings
escalate rather than self-resolve.

## Open questions

All 3 open questions were resolved 2026-08-16 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: Where does the register live? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Candidates: a table in this doc; a machine-readable file
> (`docs/invariants/`) that a lint can read; or rows in a registry
> on the cluster, like every other piece of protocol data.
>
> Proposed: **a file first, a registry later.** A file can be linted
> in CI today, diffed in review, and edited in the same car that
> changes the invariant. Moving it into the cluster is the natural
> end state — invariants are protocol data — but that only pays once
> the file exists and has proven its shape.

as proposed


### Q3: Does the conformance run gate anything? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Proposed: **no.** It produces findings and escalations, exactly like
> the packet census produces loss numbers, and neither blocks a train.
> Gating on conformance would make it something to route around; the
> value is the visible number, and the number needs to be trustworthy
> more than it needs to be feared.

Agree with proposal.


### Q2: What is the enforcement standard for a NEW invariant? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Proposed: a design doc may not resolve a question into a
> load-bearing invariant without declaring its enforcement class. The
> lint enforces the *declaration*, not the strength — an author may
> write `unenforced`, and that honesty is the point. Anything else
> turns the rule into pressure to fake enforcement.

as proposed
