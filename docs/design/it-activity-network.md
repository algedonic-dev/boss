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

## Reevaluating the prior visualization docs

David, 2026-08-10: the earlier visualization attempts should be
reread against stations-as-queues rather than answered in their
original frame.

- **department-flow-dashboards.md** — its remaining questions are
  network questions now: per-hop latency becomes station service
  time, edge pulses became FlowMotion and feed the canvas, and the
  "node set" question dissolves (nodes are queues, settled). Answer
  its review WITH this doc open; expect most of it to fold in here
  and the source doc to head toward the architecture-decisions fold.
- **queue-visibility.md** — ALIGNED, not superseded: it was already
  the queue lens. Q1 (My Day onto the lens) is strengthened — `/me`
  is a personal-queue station at another zoom; its claim-CAS question
  becomes the integrity of the claim hop.
- **The FlowNetwork stacked-DAG sections** (`df8a694c`) and
  **os-map** (`e66fe50c`) carry their own items: the canvas absorbs
  both.

## Open questions

All 5 open questions were resolved 2026-08-12 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: How does the claim hop render, and do personal queues always show? (resolved)

Resolved 2026-08-12 — accept.

Personal queues materialize when non-empty or recently active — the os-map nodes-from-edges rule restated for queues, honest at the measured 14-of-411 occupancy. The claim hop renders in the one transfer grammar; the machine/human actor split (dispatcher push vs self-claim, already computed in liveFlow) is a tint/glyph modifier, not a second gesture. Hard dependency: queue-visibility Q2's claim compare-and-set lands before the claim hop renders, or the canvas can animate two actors winning the same packet.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q2: Are rails drawn per-kind or merged? (resolved)

Resolved 2026-08-12 — accept.

Merged — one faint overlay of all declared rails, per-kind tint on hover/selection. Stations are keyed by authority-role/assignee and serve many kinds by construction; per-kind rails would re-partition exactly what stations-as-queues unified. Retires FlowNetwork's hardcoded 'pr-train' rank and the adjacent-sections-only link bars.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q3: What moves — every event, or meaningful transitions? (resolved)

Resolved 2026-08-12 — accept.

Packets move on the marker topics — step.ready, step.assigned, step.done — plus jobs.job.closed as the departure. jobs.step.updated metadata chatter stays ticker-only. Hybrid transport per sse-policy: dots ride the existing /api/events/stream push; depth piles and edge thickness ride the polled aggregate. The stream's Operator/Auditor gate currently bounds the live audience; widening it is a deliberate access decision, not an accident.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q4: Does the network canvas replace /system/flow's DAG sections or sit beside them? (resolved)

Resolved 2026-08-12 — accept.

Replace. The canvas becomes /system/flow's hero; per-kind decorated DAGs demote to the route ghost and the Fleet inspector; /system/os-map retires (page, route, nav row) once the traffic layer renders in the canvas. The LAG-pairing SQL survives as the traffic-layer repo, re-keyed from department to station. Simulated traffic stays counted separately.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q5: What is the company-level recursion, and when? (resolved)

Resolved 2026-08-12 — accept.

No company canvas in v1. Every layer endpoint takes a scope parameter (department / owner-role set) from its first version, per the /api/views/flow?owner_roles= pattern. Acceptance check: the layer queries filtered to one department return a self-consistent sub-canvas, pinned by test. The traffic-layer repo keeps its grouping key parameterizable so the company zoom reuses os-map's proven department grouping. Four-layers-vs-one-composite endpoint stays open until the first client forces it.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.
