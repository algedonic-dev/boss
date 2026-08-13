# Design: department flow dashboards — drawing the network's traffic

**Status**: in-review — open questions tracked at `/system/design`.
**Source**: David, 2026-08-09, under the information-network framing:
"I really want to get the dynamic flow of work better visualized. I
am trying to do it for the IT department with the release trains and
such, but it isn't coming together yet."
**Related**: [queue-visibility.md](./queue-visibility.md) ·
[workflow-ux-as-data.md](./workflow-ux-as-data.md) ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

---

## Why it isn't coming together — the diagnosis

Every instrument that exists is **intra-workflow**: Fleet draws one
kind's DAG with depth; TriageFlow draws one kind's queues and
routing; Flow lists one team's Jobs. But a department's actual flow
is **inter-workflow** — IT's change packet hops *between* kinds:
a backlog item spawns a ship-a-change Job, whose branch boards a
pr-train Job, whose merge deploys it. The dashboard David is
reaching for is the traffic view of that multi-kind topology, and no
surface can draw it because the links between Jobs are not data the
system knows about.

**Measured on the live playground (2026-08-09):** the links exist —
as six ad-hoc metadata keys nothing declares: `backlog_item` (13
Jobs), `train` (14), `boarded_jobs` (5 trains), `spec` (6), plus
`branch` and `merge_ref` pointing outside the Job graph entirely.
Thirty-eight job-to-job link instances, every one invisible to every
instrument, each drawable only by hardcoding the key name it happens
to use. In network terms: the packets reference each other, but the
topology is folklore. The semantic layer — the thing that makes this
network aggregable — stops one hop short of job→job.

## What exists to build on

- **The dynamic idiom is already proven**: the OS map's
  thickness = history, pulse = the moment, over actor hand-offs.
  Nobody has pointed it at a department's workflow topology.
- **Fleet** gives per-node depth for any kind (item lists parked);
  **TriageFlow** gives queues + routing edges; **Flow** gives
  wall-clock team throughput and owns the sim-time-vs-wall lesson.
- **Per-hop latency is measurable today**: the train pipeline was
  measured by hand this morning (board instant; CI 15–40m;
  merge-wait human 4m–3h15m; deploy 8–33m) with a 15-line wall-clock
  query — backlog `a5096c8f` wants exactly this, productized.
- **The registry precedent**: `subject_edges` declares payload→
  subject references once, uniformly, with enforcement mounted on
  the write path. Job→job links need the same move.

## The shape

1. **Declare the links** — a `job_edges` registry (sibling of
   `subject_edges`): which metadata fields of which Job kinds
   reference other Jobs. The six folklore keys become six rows.
   Everything downstream — topology drawing, cross-workflow
   tracing ("show me this change's whole journey"), link integrity
   — falls out of the declaration.
2. **The department network view**: nodes = the Workflow kinds a
   department engages (Flow's `owner_role` registry rule as the
   default set), each collapsible to its step-DAG; edges = routing
   edges within a kind + declared job-links between kinds.
3. **Traffic on it, the OS-map way**: per-edge flux from a trailing
   wall-clock window of transitions (thickness = volume, pulse =
   now); per-node depth from the fleet aggregate; per-hop latency
   as edge labels (the a5096c8f query, served properly). Little's
   law on any node: depth ÷ drain = expected wait, drawn where the
   wait actually is.
4. **IT proves it**: backlog → ship-a-change → train → deploy is the
   richest multi-kind pipeline in the system and the operator
   watching it is the one asking. The brewery's order-to-cash chain
   is the second tenant the moment the registry exists.

## Open questions

### Q1: How do job links become data? (resolved)

A `job_edges` registry declaring `(source_kind, field_path)` →
target Job (subject_edges' shape, minus the kind resolution — a Job
id is a Job id), or standardizing an `entity_ref`-style structured
key on Job metadata. The registry reads as more BOSS: declared once,
enforceable on the write path (a link to a nonexistent Job is the
same disease as a phantom subject), and instruments derive rather
than parse.

### Q2: What defines a department's node-set?

`owner_role` on Workflows (Flow's rule — zero config, follows the
registry) versus an explicit dashboard config (a View row naming
kinds — departments watch kinds they don't own). Probably
owner-role default with View-level override; deciding where that
config lives decides who can author dashboards.

### Q3: What are the dynamics' window and transport?

Pulses need a trailing window (wall clock, `created_at` — Flow's
doctrine) and a refresh transport. The SSE policy puts single-event
state flips in push and aggregates in poll; this surface is an
aggregate built FROM single events. Proposal: poll the aggregate at
10–15s like Fleet, with the pulse animation interpolating between
polls — push only if the demo's "watch it react" property demands
it.

### Q4: Does this absorb Flow and a5096c8f?

The stage-duration ask (a5096c8f) is this doc's edge labels; Flow's
throughput list is arguably one panel of the department view. Absorb
(one department surface, fewer instruments) or federate (Flow stays
the team-throughput list, the network view links to it)?

### Q5: Is the department dashboard the first workflow-ux-as-data consumer?

The sibling doc proposes custom views as registry data. A department
flow dashboard is naturally per-department custom — IT wants trains
drawn specially. Build the first version as core (the floor, like
Fleet) and let departments skin it via the plugin registry when that
lands, or hold the core version minimal and let the plugin
architecture carry the weight from day one?

## Decision history

**Q1 — the registry (decided by David in-session, 2026-08-09).**
`job_edges` ships in migration 104: the three real job→job edges
seeded (`backlog_item`, `train`, `boarded_jobs`; `spec`/`branch`/
`merge_ref` are external references, out of scope), a write-path
guard with subject_edges' `on_missing` dial, and — encoding the
measured folklore — prefix-aware resolution (unambiguous ≥8-char id
prefixes resolve; 14 of ~15 live links were prefixes) with `warn` as
the default until the values are cleaned. The abort dial is a
one-row update, pinned by test. An HTTP read surface for the
registry lands with the first instrument that consumes it.
