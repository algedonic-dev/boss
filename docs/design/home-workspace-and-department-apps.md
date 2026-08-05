# Design: Home as a personal workspace, and the Department App split

**Status:** approved — the four open questions were resolved through
the in-app review (`/system/design` → design-doc-review Job,
2026-08-05) and are recorded under Decision history. Flips to
`shipped` when the work lands.

The app-tab rearchitecture ([extending-boss.md](extending-boss.md),
shipped in #178) split the SPA into eight apps. It left two things
unfinished, and Cloudflare's release of
[Cloudflare OS](https://blog.cloudflare.com/cloudflare-os/) on
2026-08-05 sharpens both:

1. **Home is a fixed page.** `MePage` renders "My Jobs" and "At a
   glance" — the same two panels for everyone, authored in Svelte,
   changeable only by a frontend commit.
2. **There is no IT app.** The department that builds and operates
   BOSS has no surface of its own; its work is scattered through
   System Model.

This doc argues those are the same problem seen from two ends, and
proposes the dividing line between them.

## What Cloudflare OS proposes

Its unit is the **gadget**: a privately instantiated micro-application
created on demand by an AI coding agent. "When you create a slide deck
in Cloudflare OS, you are not calling out to some SaaS software running
in the cloud. The system creates a private instance of the slide deck
software just for you." Each gadget is full-stack — client code, server
code, an API, and **its own durable state**. If it is missing a
feature, you ask your agent to add it rather than filing a request.

Around that sit three ideas:

- **Blueprints** — a shareable template. Others instantiate their own
  copy rather than joining a shared instance.
- **Capability-based access** — "each agent, and each Gadget, by
  default has access to nothing." You explicitly *introduce* an agent
  to each resource it may touch.
- **Gatekeepers** — mediating services that authorize, log, and allow
  a human to review an agent's actions asynchronously rather than
  blocking on approval.

## The one thing BOSS must not copy

**A gadget's private durable state.**

That is the load-bearing detail of the Cloudflare design and it is
directly opposed to BOSS's thesis. The event log is the system of
record; projections are pure functions of it; rebuilders reproduce
truth from it. A personal app holding authoritative state that no
projection can rebuild and no other surface can see is a silo — the
federation problem the [global-search](global-search.md) design
rejected, reintroduced one user at a time. Ten operators with ten
private gadgets holding ten private numbers is precisely the
condition BOSS exists to end, and it would arrive wearing the
clothes of personal flexibility.

The correctness protocol is not a quality bar to trade against
convenience here. It is the product.

## The inversion: a View, not a gadget

So invert it. In BOSS the personal unit is a **View**: a saved
composition over the Information API that holds **no authoritative
state of its own**. Its content is a query plus a layout. It is a
pure function of the log, like every other projection — which means
it rebuilds, it cannot drift, and two people running the same View
see the same numbers because there is only one set of numbers.

Everything else in the Cloudflare model survives the inversion, and
several pieces land on machinery BOSS already has:

| Cloudflare OS | BOSS | Already built? |
|---|---|---|
| Gadget (private state) | **View** (query + layout, no state) | no |
| Blueprint (shareable template) | View promoted to a registry row | registry pattern, yes |
| Capability grants | `boss-policy` row-level rules | yes |
| Gatekeeper approval | a Job with a sign-off Step | yes |
| Observation log | `audit_log` | yes |

The async human-in-the-loop approval Cloudflare builds as a new
primitive is, in BOSS, just a Job. That is a good sign for the
framing: the ideas that transfer land on things already here.

### The missing rung on the ladder

[extending-boss.md](extending-boss.md) has an extensibility ladder
whose bottom rung is "run an existing workflow against a new Subject
pool — just open a Job." Below that there is nothing. An operator who
wants to *look at* the information a different way has no rung at all;
their only options are to ask for a frontend change or to keep it in a
spreadsheet, which is the silo arriving by another door.

Views add the missing bottom rung, and a promotion path up it:

```
personal View  →  shared Blueprint  →  Department App surface
 (mine, private)   (my team's)          (registry row, governed)
```

A View that proves itself gets promoted rather than rebuilt. That
path is the answer to "how does structured UX stay in touch with what
people actually do" — the structured surfaces are fed by the personal
ones instead of guessed at.

## The dividing line

> **Department Apps** are the workflows the company has decided on:
> pre-defined, governed, the same for everyone in the role.
> **Home** is where an individual explores and drafts what has not
> been decided yet.

Structure where the company has committed; flexibility where it has
not. A Department App surface is a claim about how the company works,
so it lives in a registry and changes deliberately. A View is a claim
about how one person wants to look at it today, so it should be
cheap, private, and disposable.

## The IT app

IT is missing, and its absence is why System Model currently holds
fourteen surfaces doing two unrelated jobs:

- **Modeling the company** — Job kinds, Subjects & Classes, Policy,
  Workflows, Design review, Knowledge Base, the System Model hub.
- **Operating the software** — Monitoring, Auth admin, Step plugins,
  Dispatcher rules authoring, Experiments.

The test: **does this surface describe what the company does, or does
it keep BOSS running?** The first is the model. The second is IT's
job, and IT is a department like Finance or People — the department
that builds BOSS to power Algedonic Ales.

This is partly a restoration. An `/it` hub existed before the surfaces
were folded into System Model; the fold lost a distinction worth
having.

**The review resolved this differently — see Q2 below.** The two-app
split argued for here was rejected in favour of a single IT department
app that *contains* System Model. The section is kept because the
inventory of what those fourteen surfaces do is still the useful part;
the proposed dividing line is not.

Two things this is not. It is not a permissions tier — engineers are
operators like anyone else, and the split is about *what the surface
is for*, not who may see it. And it is not tenant-specific: every BOSS
deployment has someone operating BOSS, so the IT app belongs in the
core catalog even though "IT" is also a department Class of `employee`
Subjects in a given tenant.

## Decision history

Resolved 2026-08-05 through the in-app review flow — a
`design-doc-review` Job, answered in the `review-design` step surface,
flushed here by the queued flush job.

**Q1 — local state is allowed while it stays local.** State may live
temporarily inside a Step until that Step completes, and personal
annotations and scratch work get the same treatment: if it does not
affect the rest of the system, it can be held locally. This is
narrower than the gadget's private durable state and wider than the
strict reading of "a View holds nothing" — the test is not *whether*
state exists but whether anything outside depends on it. A View's
scratch is invisible to the rest of the system until it flows into a
Job, a Step, or an Event, and at that moment it is subject to the same
rules as everything else.

**Q2 — IT is the Department App; System Model is inside it.** Rather
than splitting fourteen surfaces across two tabs, IT becomes the
department and System Model and platform functionality become part of
IT's domain. This dissolves the contested cases in the original
question — dispatcher rules, step plugins and experiments no longer
have to be assigned to one side of a line, because there is no line.
IT is a department like Finance or People, and modeling the company is
work that department does.

**Q3 — agent-authored apps, but a later phase.** Users should be able
to lean on agents to build their own apps, with IT controlling read
and write policy and the scope that provides the guardrails. That is
the eventual target, not this phase: it needs infrastructure for
running user code safely, and that infrastructure does not exist yet.
The declarative composition is what ships first, and it is a first
phase rather than a permanent answer.

**Q4 — sharing is free; inclusion is submitted.** A View should be
shareable without a promotion Job. What needs a process is *inclusion
in a department's views* — a submission, not a gate on sharing. The
individual curates their own list of shareable assets from Home. This
splits the original question in two: personal → team is
frictionless, team → company is submitted, and the ceremony lands
only where something becomes the company's.

## Open questions (all resolved — see Decision history)

### Q1: May a View store anything of its own? (resolved)

The proposal says a View is a query plus a layout and holds no
records. But the moment someone wants a personal annotation on a
result, a pinned threshold, or a note against an account, that is
state, and it has to live somewhere.

Three shapes. Store it on the View and accept a small silo. Refuse it
outright and force every durable mark through a Job or a Subject
field, which is coherent but may make Views too thin to be worth
using. Or treat annotations as first-class events on the log — which
preserves the protocol but means a personal scribble becomes a
permanent public fact, which may be exactly wrong for a scratchpad.

### Q2: Where does the IT / System Model line actually fall? (resolved)

Monitoring and Auth admin are clearly IT. Job kinds and Subjects &
Classes are clearly the model. The contested ones:

**Dispatcher rules** are business rules expressed as data (model) but
are authored and debugged like code and fail like infrastructure
(IT). **Step plugins** are JS bundles — engineering work — that
implement a step's UX, which is model. **Experiments** are described
as sandboxed model modifications, which is model, but the sandbox is
platform work.

Splitting each contested surface between two apps (health in IT,
authoring in System Model) is possible but doubles the number of
places to look.

### Q3: Does a View get an agent that writes code, or a declarative composition? (resolved)

Cloudflare's answer is unambiguous: an agent writes real full-stack
code and the user asks it for changes in prose. That is far more
expressive than anything declarative, and it is the reason their
gadgets can be anything.

The cost is a code-execution sandbox BOSS does not have, non-determinism
in what gets generated, and a review problem — a promoted View becomes
a Department App surface, and "an agent wrote it and it seemed to
work" is a poor provenance for something the whole company then uses.
A declarative composition (pick a source, filter, group, choose a
layout) is reviewable, diffable, deterministic, and rebuildable, and
is strictly less powerful.

### Q4: Is promotion a Job? (resolved)

Making promotion a Job with a sign-off Step is the obvious BOSS
answer: it gets provenance, an owner, an audit trail, and a
reviewable diff for free, and it models the company's own governance
in the system that governs it.

The risk is that this is the ceremony Views exist to escape. If
sharing a useful View with two colleagues requires opening a Job and
waiting for sign-off, people will screenshot it instead — and the
promotion path fails not by being wrong but by being slower than the
workaround. Whether personal → team sharing needs the same gate as
team → company is the sub-question.

## Sources

[Cloudflare OS announcement](https://blog.cloudflare.com/cloudflare-os/) ·
[cloudflare/cloudflare-os](https://github.com/cloudflare/cloudflare-os) ·
[How we're rethinking work at Cloudflare with Cloudflare OS](https://blog.cloudflare.com/how-we-use-ai-with-cloudflare-os/)
