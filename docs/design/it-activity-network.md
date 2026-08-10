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

- **Station** — where work waits plus who acts: a role-queue and the
  executors that pull from it; the dispatcher's rule-machine as one
  station; NAMED external stations (GitHub/CI — the second half of
  `39d5bfde`) where packets leave the department and return changed.
- **Rails (declared edges)** — the workflow graphs and the
  `job_edges` registry: the SDN control plane, drawn faint.
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

### Q1: What exactly is a station on the IT canvas, v1?

Proposed: one station per role-queue that IT's workflows reference
(platform-admin dominates today), one per named human executor, ONE
for the dispatcher's whole rule-machine, one per named external
(github-ci). Deliberately not (role × step-kind) — that multiplies
stations back toward the DAG view. Executors group visually inside
their role-queue's station.

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
