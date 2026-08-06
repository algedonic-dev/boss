# Design: the Operating System view

**Status**: in-review — design only, nothing built.
**Related**: [human-powered-state-machine.md](./human-powered-state-machine.md) ·
[../architecture-diagram.md](../architecture-diagram.md)

---

## The idea

> Place all of the actors in our brewery system — Subjects that can
> perform CRU operations on Jobs — on a virtual map and show how Jobs
> get sent around as structured messages through a network.

This is the reading frame made literal. BOSS already claims to be *the
software layer of a state machine whose executors are humans and
agents*, with a CPU being "a human (primarily) or an agent". Every
other surface renders the *work* — a Job, a Step, a queue. None
renders the *machine*: who the processors are, and what moves between
them.

The claim is testable, which is what makes it worth building rather
than drawing. If the company really is a network of executors passing
structured messages, the audit log already contains that network and
we can derive it rather than author it. If the derived picture is
unreadable or uninteresting, the framing is decorative and we have
learned something more valuable than a diagram.

## What the log already knows

Measured on the playground, 2026-08-06. These numbers are the design
constraint, not colour:

| | |
|---|---|
| Automation actors | **27**, producing **780,135** events |
| Employee actors | **176**, producing **115,337** events |
| Active employees / distinct roles | **411** / **54** |
| Step handoffs between different actors | **58,650** |
| Distinct actor→actor edges | **3,838** |

Every event carries `payload->>'_actor'`, shaped either
`automation:rule:<name>` or `emp-<id>`, so both node identity and edge
direction are derivable today without a new projection. `_simulated`
distinguishes sim traffic from real.

Two facts should shape everything downstream:

**Automation is 87% of the traffic.** A map weighting nodes by volume
is a map of the dispatcher, with the humans as a rounding error. The
top two edges are a pair of rules passing work to each other 14,907
and 14,420 times; the busiest human edge is 399.

**Individual actors do not aggregate into a picture.** 3,838 edges
across ~200 nodes is not a diagram, it is a hairball. But 176
employees collapse to **54 roles**, and roles are already a Class
registry vocabulary — so the aggregation is data we have, not a
heuristic.

## What exists to build on

- `@xyflow/svelte` is already a dependency, and `/it/dispatcher`
  already renders a rule cascade from `cascadeToGraph.ts`. The
  automation half of this map is partly drawn.
- `event_facts` links events to Subjects, so a node can be clicked
  through to real history rather than being a dead shape.
- Roles are Classes of `employee` Subjects; departments already group
  them.

## Open questions

### Q1: What is a node — an actor, a role, or a department?

The measurement argues against individual actors: 200 nodes and 3,838
edges is a hairball, and a named employee is rarely the interesting
unit ("who does QA sign-off" beats "emp-aa-227 does"). Roles give 54
human nodes plus 27 automation nodes, which is a readable graph and
maps onto vocabulary the registry already owns.

The cost is that roles hide load imbalance between people holding the
same role, which is exactly the kind of thing an operator would want a
map for. A defensible answer is role-level by default with expansion
into individuals, but that is a real build rather than a default, so
it should be chosen deliberately.

### Q2: What is an edge — a handoff, a message, or both?

Three candidates, and they are not the same network:

- **Step handoff** — consecutive step completions on one Job by
  different actors. This is the "Job moves through the org" reading,
  and it is what the 58,650 number counts.
- **Message** — `messages.message.sent` (54,543 events) is literally
  actor-to-actor and needs no derivation.
- **Dispatch** — a rule firing in response to another actor's event.
  Already visualised at `/it/dispatcher` for rules alone.

Drawing all three on one canvas without distinguishing them would make
a picture that cannot be read. Drawing only one risks a map that
contradicts what an operator sees elsewhere.

### Q3: Is this a live instrument or a historical map?

A live view answers "what is my company doing right now" and makes the
algedonic framing visceral — traffic lighting up as it flows. A
historical view answers "how does work actually move", which is the
question you would redesign a process from, and can aggregate over a
window rather than sampling a moment.

They imply different infrastructure: live wants the SSE tail that
already exists; historical wants an aggregation over `audit_log` that
does not, and at 780k automation events the aggregation cannot be done
in the browser.

### Q4: Does this replace `/it/dispatcher` or sit beside it?

The dispatcher cascade already draws automation actors and their
triggering relationships. If this view covers automation too, there
are two graphs of overlapping data with different layouts, which is
how a UI starts lying. Either this generalises the cascade — humans
become nodes in the existing graph — or the cascade becomes the
drill-down for a single automation node on this map. Deciding late
means building the second one twice.

### Q5: Does the sim belong on the map?

87% of the traffic is `_simulated`. On the public demo that IS the
company and the map should show it. On a real deployment it would be
noise, and worse, a map that silently blends simulated and real
executors would misrepresent the organisation to someone making
staffing decisions from it. A filter is the obvious answer; whether
the default is "show" or "hide" is a judgement about who the surface
is for.

## Decision history

_None yet._
