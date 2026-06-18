# Scheduler-shaped sim engine

**Status:** design exploration for v1.0.6 / v1.0.7. Active task: #72.
Phase 1 (auto-tick) ships as #71 ahead of this.

## The problem

Today's brewery sim engine (the `boss_brewery_engine` lib, driven
by `boss-brewery-sim`) is a batch processor. For each sim day, it:

1. Rebases clock-api to that day's midnight UTC
   (`POST /api/clock/configure {"set_to": "2025-04-05T00:00:00Z"}`).
2. Walks every JobKind, fires the day's work in a single tight
   loop (Periodic + Counterparty + HumanWorker engines), and
   POSTs every emit through the live API.
3. Calls `end_of_day_rollup`, then loops to the next day.

The whole "what happens on day N" workload runs in milliseconds
of wall time. Every audit_log row from day N stamps at the same
exact instant clock-api was set to — midnight UTC. After 14
sim-days we see 30k rows clustered at exactly 14 timestamps
(`00:00:00Z` × 14 days). In the UI everything looks like it
happened at 17:00 PDT, every day. The data is correct (sim-aligned)
but visually fake.

The deeper issue is the model: the sim doesn't behave like a
human or agent executor would. A real executor:

- Wakes up at a known time (shift start, business hours).
- Picks up the next ready item from the queue.
- Spends real time on it (calls a vendor, talks to a customer,
  walks the warehouse).
- Marks the item done.
- Repeats until end of shift, then sleeps.

Time flows; work has duration; events naturally land at the
sim-clock instant when the executor finishes them. The batch
engine collapses this to "all of day N happens at midnight."

## The vision

Reshape the engine so the simulator plays the executor role
inside the same state machine real humans and agents would.
Concretely:

- **clock-api becomes the rate.** In sim mode, set
  `BOSS_SIM_WARP_FACTOR` (sim-seconds per wall-second) so the
  sim clock auto-advances continuously (e.g., `warp=18000`
  means 50ms wall buys 900s sim → 1 hour wall = 750 days sim —
  or pick the ratio the run needs).
- **Engine never rebases the clock after epoch setup.** It
  only reads `clock.now()` to know what sim-time it is. (The
  epoch is set via `POST /api/clock/configure`, which rebases
  `epoch_start`; the old `/advance` endpoint is retired.)
- **Engine is a tokio scheduler.** A `BinaryHeap` keyed on
  (sim-time, item) holds pending work. The scheduler peeks
  the earliest item, computes
  `wall_delay = max(0, (item.sim_time - clock.now()) / ratio)`,
  `tokio::time::sleep(wall_delay).await`, then dispatches.
- **Step durations source from StepType.** Every
  `StepType.typical_duration_hours` already declared in the
  registry becomes a real wait. Variance via the existing
  per-step RNG (so replay determinism stays intact for
  fixed-seed runs).
- **Natural anchors.** Brewery shift opens at 06:00 local;
  finance close runs at 17:00 local; vendor calls happen
  during business hours. The engine schedules each work item
  at its appropriate anchor sim-time rather than batching.
- **Counterparty + Periodic engines schedule into the
  same heap.** Vendor-pays-invoice in N sim-days becomes
  "insert (today + N, vendor.pay_invoice) into heap."
  PeriodicEngine's first-tick-of-day becomes "insert
  (next anchor, periodic-job-N) into heap."

## What changes architecturally

**Old shape:**
```rust
for day in start..end {
    advance_clock(day);
    periodic.tick(day);
    counterparty.tick(day);
    human_worker.tick(day);
    end_of_day(day);
}
```

**New shape:**
```rust
let mut heap = BinaryHeap::<(SimInstant, Item)>::new();
heap.push((start, Item::PeriodicStart));
loop {
    let Some((sim_time, item)) = heap.pop() else { break };
    let wall_delay = wall_for(sim_time - clock.now());
    tokio::time::sleep(wall_delay).await;
    let next_items = item.dispatch(...).await?;
    for n in next_items {
        heap.push(n);
    }
    if clock.now() >= end { break }
}
```

Each `Item::dispatch` returns the items it spawned: a step
dispatch returns `[step.completed(at: now + duration)]`, a
periodic-fire returns `[next periodic fire, JobOpened]`, etc.

## What needs to be true for this to work

1. **Wall-time pace must be tunable per deploy.** Playground
   = realistic (1× → 100× ratio). Regen = "as fast as the
   computer can go" (∞× ratio = the heap drains with no
   sleeps, falling back to today's batch behavior but with
   per-item sim-time stamps). The regen path needs sim-time
   stamps to vary even though it doesn't wait.

2. **Side-effect handlers must be async-friendly.** Today
   they're sync. The scheduler's `tokio::time::sleep` already
   needs an async runtime; side-effects can stay sync but the
   dispatch loop is async.

3. **clock-api auto-tick must be reliable across long sleeps.**
   The 100ms ClockClient cache TTL already handles short
   bursts. For sleeps of seconds-to-minutes, the cache will
   miss + re-fetch — that's correct.

4. **Determinism for tests/replay.** Three knobs:
   - Tests run the same unbounded-`warp_factor` zero-wait mode
     as regen (no `tokio::time::sleep`, but stamps emit at the
     heap-popped sim-time). Same sim-time stamps as the real path.
   - Per-Job seed for the RNG (already exists).
   - StepType duration variance bounded so the heap order
     stays stable across runs of the same seed.

5. **Hard-fail mode preserved.** Today `--hard-fail` aborts at
   the first non-2xx. The scheduler version aborts the dispatch
   loop on the first failure and propagates.

## Phase 1 (#71): auto-tick only

What ships first: run clock-api in sim mode with its formula
clock, so `/api/clock/now` advances continuously rather than
sitting at the last instant the engine set. The clock computes
`sim = epoch_start + (wall_now − wall_anchor) × warp_factor`,
where `warp_factor` (`BOSS_SIM_WARP_FACTOR`, sim-seconds per
wall-second) sets the rate. Engine stays the same batch shape.
Between the engine's per-sim-day clock rebases, the formula
walks forward on wall pace, so sim events land at varied
times-of-day instead of clustering at midnight UTC.

Trade-off: timestamps are wall-pace-dependent. A faster CPU
clusters events tighter. Not the semantic-grade fix, but
visually a big improvement immediately and tunable via
`warp_factor`.

Phase 1 is a config change + a regen run. No engine refactor.
Ship in v1.0.6.

## Phase 2 (#72): scheduler-shaped engine

The real refactor described above. Probably v1.0.7 unless
v1.0.6 stretches. Touches:

- `crates/orchestrators/boss-sim/src/engines/day_runner.rs`
  (the batch loop) → becomes the scheduler loop
- `crates/orchestrators/boss-sim/src/shape_driven/engine.rs`
  (work dispatch) → returns spawned items instead of emitting
  inline
- `crates/orchestrators/boss-sim/src/engines/periodic.rs` +
  `counterparty.rs` → emit into the heap rather than via
  per-day step
- `crates/orchestrators/boss-sim/src/event_routes.rs`
  (the per-step side-effect emitters) → handlers stay sync but
  the dispatch context exposes the current sim-instant
- New: scheduler crate or module owning the heap +
  wall-vs-sim translation

## Open questions

### Q1: What's the canonical wall-vs-sim ratio for the playground?

Brewery is a 12-month sim. At 1× (real-time), it'd take 12
months wall. At 1000× it'd take ~9 hours. At 10000× it'd be
~50 minutes. We want the playground to feel "live" while still
being watchable end-to-end in a sitting. 1000–5000× feels like
the sweet spot but worth confirming on the deployed playground.

### Q2: How do we communicate "wall-pace-dependent vs real-time" to operators?

Phase 1 stamps depend on how long the engine takes to process
each sim day. Operators reading audit_log timestamps will see
inconsistent spacing across days if the engine speeds up or
slows down. Two options: (a) document it as "Phase 1 limitation,
fixed in Phase 2"; (b) skip Phase 1 entirely and wait for the
proper scheduler. The current proposal is (a).

### Q3: Should StepType.typical_duration_hours grow a variance field?

Resolved: it did. `StepType` now carries `typical_duration_jitter`
alongside `typical_duration_hours` (`boss-jobs/src/step_registry.rs`).
A jitter of `0.3` means the duration lands in `[0.7, 1.3] ×` the
typical, drawn from the per-Job RNG — so a payroll-release step no
longer takes exactly 2 hours every time.

### Q4: How do we keep the regen fast?

Regen wants "as fast as possible while still producing
sim-time-stamped events." The scheduler at an unbounded warp
factor is "no sleeps, but stamp at heap-popped sim-time" —
which is exactly what we want. Treat a sentinel `warp_factor`
(e.g. infinity) as regen mode, and the scheduler loop becomes
a tight drain with no `tokio::time::sleep`.

### Q5: Does this affect agent / human executors of the same JobKind in production?

No. The scheduler shape only runs the sim engine. Production
executors (humans, agents) hit the same HTTP APIs the sim
hits today; nothing about their experience changes. The point
is that the sim now looks like them.

## Why this matters

The sim isn't infrastructure — it IS the demo. Operators (and
prospects) read the brewery's UI to understand what BOSS does.
If every event in audit_log shows the same time-of-day, the
demo reads as "computer-generated test data." If events land
at the times they would for a real brewery — morning shift,
midday production, end-of-day finance close — the demo reads
as "this is what running on BOSS looks like."

Phase 1 is the fast win that fixes the visible problem. Phase
2 is the architectural alignment with the founding idea: the
software describes a state machine, and the sim plays the
executor role inside that machine. That alignment is what
makes the BOSS vision legible.
