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

## Decision history

**Q1 — What is a node? Resolved 2026-08-13 (David): a node is a
station.** Verbatim: "Stations are the abstract priority queues we
use to either route or hold job packet traffic until we have
bandwidth or capability to handle the job packet in question. Actors
will certainly have individual priority queues, but there are plenty
of 'stations' that will be handled by groups of actors or be defined
by constraints like having certain skills and authority. For example,
at least each department needs a station queue, and we have the
'stations' associated with the SDLC process that are queues where
jobs can bundle up for periodic, higher-bandwidth or batch handling.
These are all defined in data." Registered agents have stations too.
Visual rollup of actor stations into team/department groupings is a
view-level aggregation for clutter control — showing everyone is
acceptable for now. Full definition: [stations.md](./stations.md).

**Q2 — What is an edge? Resolved 2026-08-13 (David, by the same
call): an edge is a route — packet motion between stations.**
Queuing is separated from dispatching: stations hold, the dispatcher
routes. Step handoffs are the default edge (that is the substrate's
own motion); messages and dispatch firings render as distinct
overlays, never silently blended into one graph.

**Q5 — Does the sim belong on the map? Resolved 2026-08-13
(David): real vs simulated is fixed on the packet at creation.**
Verbatim: "jobs are the new packets, and we said a job is created as
real or simulated, and all event/data/state information happens
within a job, so we should have a much easier time discriminating
between sim and real packets." Discrimination is a packet-attribute
read, not an event-payload heuristic; surfaces show sim traffic
visibly marked (the packet-card dashed/SIM grammar), never blended.
Core follow-up: `simulated` becomes a first-class job field stamped
at admission, inherited by the job's events — replacing tag-sniffing
and `_simulated` payload inspection.
