# Design: SSE policy — when SPA views push vs poll

**Status**: living pattern doc.

Every SPA page that shows backend-derived state picks one of three
modes. This doc is the **policy**, not a per-route audit — a
route-by-route table drifts on every new page and service rename,
so the durable content is the decision criteria below.

## The 3-bucket policy

### (a) SSE-push

A single backend event flips visible state, and the user sees the
change at the speed it happens (sub-second). Wire via an
`EventSource('/api/<svc>/stream')` plus a server-side
`axum::response::sse::Sse` handler that pushes JSON frames as the
underlying state changes.

**Use when**: the view's primary signal is state-machine-shaped
(Job step transitions, sim_clock ticks, agent status, audit-log
tail). The cost of a 5-30s lag would degrade the demo's "watch
the system react" property.

### (b) Periodic poll

Re-fetch on a `setInterval` cadence (5-30s typically). Simple,
robust, no extra wiring.

**Use when**: the view's signal is an *aggregate* (revenue
rollups, in-flight Job counts, top-N lists) where a single event
doesn't unambiguously update the visible number. Or when the SSE
wiring isn't worth the complexity for the cadence the operator
actually needs.

### (c) On-mount only

Fetch once when the route mounts; don't re-fetch.

**Use when**: the data is *configuration* that doesn't change
during a session — JobKind catalog, ADR catalog, architecture
diagrams, design-doc text.

## Decision criteria

Choose by answering, in order:

1. **Does the view show state-machine state where a single event
   flips the visible value?** → SSE-push. (The four wired today:
   `sim_clock.current_sim_date` via `/api/jobs/sim-clock/stream`,
   a Job's step-status changes via `/api/jobs/{id}/stream`, the
   audit-log tail via `/api/events/stream`, and Ops telemetry via
   `/api/events`.)
2. **Does the view show data that doesn't change during a
   session?** → On-mount only. (Examples: architecture diagrams,
   ADR catalog, JobKind step graphs.)
3. **Otherwise** → Periodic poll, with a cadence based on the
   underlying signal:
   - sub-second cadence underlying → poll at 2-5s
   - tick-rate cadence (every few seconds) → poll at 5-10s
   - operator-driven cadence (manual changes) → poll at 20-30s
   - rare/aggregate cadence → poll at 60s+

## Anti-pattern: don't push everything

SSE wiring has real cost — backpressure, reconnects, server-side
fanout. Periodic polling at 20-30s is the right answer for ~70%
of views; reserve SSE for the views where the demo's "system
feels alive" property depends on it. Default to poll; promote to
SSE only when criterion 1 above applies.
