# Design: the IT activity network — stations, packets, routes

**Status:** draft — open questions tracked at `/system/design`
**Origin:** David, 2026-08-10 (`cb7e067b`): "Rather than visualize
activity as workflows, we need to think of the whole system as a
network to ground our visuals. Workflows are just routes through
that network for particular 'job packets', but the grounded visual
is the nodes in the network moving information around."
**Related**: [department-flow-dashboards.md](./department-flow-dashboards.md) ·
[queue-visibility.md](./queue-visibility.md) ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

## The inversion

Every activity surface built so far draws workflow DAGs and
decorates them with activity. A workflow DAG is a **route map**, and
we have been using route maps as terrain. The terrain is the
**network**: the stations that hold and process information. A
workflow is the route a packet class takes across it; a Job is one
packet in flight.

The measured motivation (`df8a694c`): /system/flow stacks N kinds as
N sections, but pr-train packets and feedback packets share David,
the agent, and the dispatcher in reality — a page whose unit is the
route cannot show the shared stations.

## The vocabulary, grounded in what exists

- **Station = a queue where work can sit** (David's clarification,
  2026-08-10). Two kinds: shared role-queues, and each actor's
  personal queue. Actors are the PROCESSORS attached to queues —
  they drain them and make hand-offs — not nodes themselves. The
  data model already speaks this exactly: `list_assignments` takes
  `assignee_id` (the personal queue) and `roles` (the claimable
  shared queues). The dispatcher needs no special case: its personal
  queue is the JetStream durable consumer. External stations
  (GitHub/CI — the second half of `39d5bfde`) are external queues
  where packets leave the department and return changed. `/me` and
  the canvas's personal-queue station are the same object at two
  zoom levels.
- **Claiming is motion**: a packet claimed from a role-queue HOPS to
  the claimant's personal queue — the assignment moment, invisible
  bookkeeping today, renders as a transfer.
- **Rails (declared routes + routing rules)** — the workflow graphs
  and the `job_edges` registry, with `ready_when` predicates as the
  routing rules that move a packet between queues: the SDN control
  plane, drawn faint.
- **Traffic (observed edges)** — actual handoffs. Already built:
  `os_map.rs` pairs consecutive step completions into actor→actor
  handoffs (`e66fe50c`: the os-map is this design's traffic layer,
  hung on the wrong wall).
- **Packet position** — a Job sits AT the station of its currently
  actionable step (the queue-visibility lens). Depth is dots piling
  at a station, not a table column.
- **Motion** — FlowMotion's SSE plumbing moves the dots; the pulse
  becomes a departure/arrival.
- **Route ghost** — selecting a packet (or a kind) highlights its
  workflow route ACROSS the canvas; the DAG demotes from canvas to
  overlay.

The Beer test: the same picture must work one recursion up (the
company: departments as stations) and one down (inside a workflow:
steps as stations). If the IT canvas cannot re-scale, the design is
wrong.

## Open questions

### Q1: How does the claim hop render, and do personal queues always show?

Stations are settled (operator, 2026-08-10): queues only — shared
role-queues + personal queues, actors as attached processors. What
remains: does every actor's personal queue draw permanently (N
stations for N staff — noisy at company scale) or materialize when
occupied? And does the role→personal claim hop animate like any
other transfer, or as a distinct "pull" gesture? Proposed: personal
queues render when non-empty or recently active; the claim hop is
the same motion grammar as every transfer — one grammar, no special
cases.

### Q2: Are rails drawn per-kind or merged?

Proposed: merged with per-kind tint on hover/selection — the canvas
stays one picture; a kind's route appears when asked for (the route
ghost), not by default.

### Q3: What moves — every event, or meaningful transitions?

Proposed: packet dots move on step.done/step.ready/step.assigned
(FlowMotion's set); the ticker keeps the full feed. Sub-step chatter
(metadata writes) does not move packets.

### Q4: Does the network canvas replace /system/flow's DAG sections or sit beside them?

Proposed: replace the stacked sections as the page's hero; the
per-kind DAGs remain reachable as route inspectors (click a kind).
The os-map page retires into the traffic layer (`e66fe50c`).

### Q5: What is the company-level recursion, and when?

Proposed: not in v1 — but v1's data contract (stations, rails,
traffic, packets as four queryable layers) must already answer
"give me this per department" so the company canvas is a filter
change, not a rebuild.
