# Brewery model completeness — executors, fulfillment, and release valves

**Status:** active design. Drives the v1.1.0 brewery JobKind rework.
Supersedes the warp-tuning framing — raising the regen warp was
treating a symptom (see Frame). Settled decisions fold into
[../architecture-decisions.md](../architecture-decisions.md); open
questions are tracked at the bottom.

## Frame

The brewery sim exists to prove out the *rough accuracy* of the model
before release. Run long enough, it surfaced three symptoms that looked
unrelated:

- **Open Jobs grow without bound** — in a 14-day regen, 820 of ~1071 open
  `wholesale-keg-order` Jobs sit at their *first* working step,
  `order-intake`.
- **Regen throughput is warp-sensitive** — completions/sim-day *fall* as
  warp rises (≈58/sim-day at warp 2000, ≈40 at 8640), and neither workforce
  concurrency (6→16) nor check-in cadence (200→50 ms) moves it.
- **Conservation P fails** — finished-goods GL (1320) diverges from
  physical inventory-at-cost by ≈$24K (≈4%).

Chasing these as a *tuning* problem moved nothing, because they are one
modeling problem wearing three masks. Warp speeds up time uniformly for
everyone; if throughput *falls* when warp rises, the model contains work
that cannot keep pace at any clock — it generates more human labor than a
real brewery does. A real brewery doesn't accumulate unbounded open
orders: people check stock, skip, backorder, and automate the routine. The
model is missing those release valves and the steps they hang on.

This doc catalogs what's missing and the model that closes the gap. It
builds on:

- [human-powered-state-machine.md](human-powered-state-machine.md) —
  executors are humans **and** agents ("agents are additional CPUs in the
  same machine").
- [../architecture-decisions.md](../architecture-decisions.md)
  §Dispatcher — the event router (the rule engine, today reacting
  only to `step.done.*`) + §Jobs, JobKinds, Steps (per-step
  `ready_when` predicates; the natural home for a `skip_when`) +
  §Finance & ledger (FG cost basis, which conservation P is
  closure over).
- [scheduler-shaped-sim-engine.md](scheduler-shaped-sim-engine.md) — Q5
  ("does this affect agent / human executors of the same JobKind?") is
  answered here.

## What the audit found

24 JobKinds, 43 StepTypes, 30 dispatcher rules. Three gap classes, in
priority order.

### Gap 1 — computer-speed work modeled as human labor (dominant)

Every working step is one of two things, but the model treats them all as
human:

- **Agent / computer-speed** (a decision or an automated action):
  `order-intake`, `acknowledgment`, `demand-gate`, `billing`, `scheduling`,
  and the data-ops `task`s (pull-aging-report, record-provenance,
  prepare-filing).
- **Human** (physical labor or judgment): brewing (`production-consume`,
  `production-produce`, the lauter/boil/whirlpool/ferment tasks),
  `checklist` QC, `inspection`/`repair`/`sign-off`, `approval`, `handoff`
  (physical pick), the sales funnel (`outreach`/`qualification`/`quote`),
  HR.

The agent-class steps are the *primary* steps of the highest-volume
JobKinds. `wholesale-keg-order` (48/day) is
`order-intake → acknowledgment → ship → billing` — three of four working
steps are computer-speed but modeled as human, ≈144 human completions/day
the workforce should never touch, and exactly where the 820 pile up.
`direct-shop-order`, `taproom-shift-close` (a register close — pure
software), and the five `morning-brew*` demand-gates have the same shape.

### Gap 2 — missing fulfillment steps

`wholesale-keg-order` goes acknowledgment → ship with **no inventory check
and no pick/stage**, and draws finished goods down at *billing, after* the
kegs have shipped. The correct shape already exists one JobKind over:
`direct-shop-order` has `handoff (pull-from-cooler)` — the physical pick.

### Gap 3 — release valves that require labor

`demand-gate` carries the data to decide oversupply
(`expected_daily_demand`, `oversupply_multiplier`) but has no logic in
core — it waits on a `head-brewer` to work it, so at warp ≈48 brews sit
*at the gate* instead of skipping. A release valve that costs labor isn't a
release valve.

*(Consistency: the `morning-brew` variants drop the `qc-checkpoint` that
base `morning-brew` carries.)*

## The model the brewery is missing

### Every step has an executor kind: human or agent

A step is **agent-executed** when it is a decision or an automated action —
it should complete at computer speed, the moment it is ready, with no
person in the loop. A step is **human-executed** when it is physical labor
or human judgment. This is not new vocabulary — it is the
human-powered-state-machine executor model taken literally. The brewery
mis-files routine automation as labor, which is why the workforce can't
keep pace and why warp isn't throughput-neutral: an agent step costs no
labor at any clock speed.

### The dispatcher is the agent

The dispatcher already assigns ready steps to role-matched employees and
runs side-effect rules on `step.done.*`. The minimal extension: for an
agent-executed step, on `step.ready.<kind>` the dispatcher *executes* it —
evaluates a predicate or calls a service, then `PUT`s the step
`done`/`skipped` — instead of assigning it to a human. The dispatcher
becomes the automated CPU; the logic stays data in the rule registry. This
keeps the human workforce holding only genuine labor, which it *can* drain
at the generation rate.

The evidence that this is the right seam: the `demand-gate` decision
*already exists* — `decide_demand_outcome(metadata, skus, on_hand)` in
`boss-sim/src/workforce.rs` — but it lives **inside the sim**. The
workforce reads real finished-goods stock at completion to stamp
brew/oversupply. That placement is the bug twice over: it forces the
decision through a workforce slot (assign → claim → complete), so gates
queue behind human labor at warp; and it puts a *system* decision (a
real-stock-driven outcome) inside the sim, which
[`feedback_sim_separate_from_system`](seed-vs-emergent-state.md) says
should only drive the human workforce through the public API. Moving the
pure decision into the dispatcher fixes the throughput throttle, the
warp-sensitivity, and the sim-separation smell in one change — the sim goes
back to driving only labor.

### Release valves are auto-evaluated gates

A gate (`demand-gate`, a new inventory-availability gate) is an
agent-executed step whose outcome is `done` (proceed) or `skipped`
(oversupply / can't fulfill → terminal or backorder). Evaluated by the
model, not pulled by a brewer — so an unwanted brew or an unfulfillable
order resolves to a terminal state instead of lingering as open WIP. This
is what bounds WIP the way human judgment bounds it in the real brewery.

### Wholesale fulfillment, modeled honestly (item #1)

Redesigned `wholesale-keg-order`:

```
trigger
  → order-intake        (agent)  confirm the order from the feed
  → stock-check          (agent)  inventory gate: enough FG? → proceed | skip→backorder
  → pick-stage           (human)  handoff: pull kegs from the cooler  ← FG drawdown here
  → acknowledgment       (agent)  notify the customer
  → ship                 (human)  driver delivers (transit duration; courier scans)
  → billing              (agent)  issue the invoice (revenue)
  → outcome
```

The FG drawdown moves from `billing` to `pick-stage`, where it physically
belongs — inventory is relieved when the kegs leave the cooler, against the
kegs actually picked. Billing then issues revenue only. This is the
right-ordering of physical vs financial effects and the structural fix for
conservation P (closure between 1320 and on-hand-at-cost); whether it fully
closes the ≈$24K drift, or whether the moving-average cost basis also needs
reconciling, is **Q5**.

## Plan

1. **Wholesale fulfillment** (this doc's item #1): the dispatcher-as-agent
   mechanism (Q1/Q2) + the redesigned `wholesale-keg-order` graph + the
   FG-drawdown move. TDD: dispatcher rule tests + a JobKind viability/lint
   pass + the regen.
2. **`demand-gate` auto-evaluate** — reuse the mechanism; `morning-brew`
   gate stops lingering.
3. **`order-intake` / `acknowledgment` / `billing` → agent** across
   wholesale / direct-shop / taproom.
4. **Re-regen at warp 2000** — open WIP should plateau and P should clear;
   then confirm warp is throughput-neutral by re-running higher.
5. Consistency (variant QC), then the v1.1.0 tag.

## Open questions

**Status (v1.1.0):** Q1–Q3 are resolved as leaned and shipped — the
`StepType.executor` flag (Q2), the dispatcher-as-agent `GateResolve` +
`JobsCompleteStep` handlers (Q1), and the skip-to-`backorder` outcome (Q3).
Q6 (production-pull: deterministic daily review + in-flight-aware gate) is
resolved as built, pending its validating regen. Q4 (FG-drawdown cost
parity) and Q5 (conservation P drift) are **deferred to the post-release
model-polish pass**. The resolved decisions extract to ADRs when Q4/Q5
close and this doc flattens.

### Q1: Agent-step mechanism — dispatcher rule, jobs-api predicate, or both?

A dispatcher rule on `step.ready.<kind>` can call services (query stock,
issue an invoice) and is data in the `dispatcher_rules` registry (seeded
from `rules.toml`); a jobs-api
`complete_when`/`skip_when` predicate (sibling to `ready_when`) is in-process
and needs no round-trip but can't reach cross-service data. Leaning **both,
each where it fits**: jobs-api predicate for pure in-Job decisions, the
dispatcher for gates/actions needing external data or a side-effect (the
`stock-check` gate needs inventory, so it's dispatcher-side).

### Q2: How is a step marked agent-executed?

So the dispatcher executes it instead of assigning it to a human. Options:
a StepType-level `executor = "agent"` flag (data, registry-driven); the
mere existence of a `step.ready` rule for that kind; or empty
`required_roles`. Leaning a StepType flag so assignment and execution read
the same source of truth and the human run never tries to claim an agent
step.

### Q3: When FG is short — skip, backorder, or partial-fill?

The `stock-check` gate's failure branch. A real brewery backorders or
partial-fills; the simplest honest model is skip-to-`backorder` outcome
(terminal, distinct from `delivered-invoiced`), with a follow-up backorder
Job as a later enhancement. Partial-fill needs line-item splitting.

### Q4: Does the FG drawdown at `pick-stage` use the same cost the GL credits?

For closure, the on-hand decrement and the 1320 credit must use one cost.
The drawdown reads `finished_product_inventory.production_cost_cents`; the
COGS posting must credit 1320 at that same number, in the same logical
operation.

### Q5: Conservation P — what's the exact drift mechanism?

`production_cost_cents` is an integer-rounded moving weighted average
updated per produce, while the GL tracks exact transaction costs;
separately, any FG on-hand increase without a paired 1320 debit (a no-cost
produce, or a `manual_inventory_transferred`) would push physical above GL,
which matches the observed sign. Needs a per-SKU `GL − (on_hand × cost)`
diff on a fresh regen to attribute. Determines whether FG-at-pick closes P
or a cost-basis reconciliation is also required.

### Q6: Production pull — closed-loop daily review + in-flight-aware gate

**Status (post-v1.1.0):** resolved as built; the regen that exercises it
is pending. This closes the half of the gate the original
`demand-gate` left open.

The `demand-gate` as shipped throttles production in one direction only.
It reads real finished-goods on-hand and, on oversupply, skips the brew —
it can push production *down*. It has no matching way to push production
*up*, and the open-loop creation it sat behind couldn't either: a
`morning-brew*` JobKind was opened at a Poisson rate, and a Poisson(λ)
draw is zero with probability `e^−λ`. At a low rate a brew could go many
working days without a single Job. The 365-day regen made the failure
concrete: the sampler opened no `morning-brew-stout` Job for the first
~28 sim-days, Stout finished-goods drew down to zero, and the run aborted
~day 58. A release valve that only opens downward is half a valve; an
open-loop sampler that can silently emit nothing is the upstream half of
the same gap.

The model is missing the *pull*. Closing it takes two changes that fit the
existing seams without new vocabulary:

- **Creation becomes a deterministic daily review, not a Poisson rate.**
  Each of the five `morning-brew*` kinds gets `deterministic = true` in
  `[job_rates.*]`. The sampler then opens the rounded effective-rate count
  once per sim-day instead of sampling it — exactly one brew is *reviewed*
  every working day, never zero by chance. The effective rate already
  folds `weekday_multiplier`, `weekend_multiplier`, and the
  US-federal-holiday rule, so a `rate = 1.0`, `weekend_multiplier = 0.0`
  brew reviews Mon–Fri and rests on weekends and holidays with no per-rate
  holiday list. The count is day-anchored on the first tick of the sim-day,
  so sub-day tick granularity doesn't multiply it.

  The `rate` is the brewhouse's per-beer brew-slot capacity, not a fixed
  one-per-day: one brew Job is one batch and the gate fills one slot per
  review, so a beer whose working-day pull exceeds a single batch must run
  more than one slot. Pale runs 2 slots; the flagship ipa runs 4 (its
  measured demand is 8× any other beer); the low-volume kinds (stout,
  lager, hazy) run 1. The gate matches demand
  down within that ceiling, so the rate is a capacity that need only clear
  peak — not a forecast the way the old Poisson rate was.

- **The gate becomes in-flight-aware, so the daily review doesn't
  double-brew through the brew lag.** A brew takes several sim-days to
  package; without accounting for brews already in the pipeline, a daily
  review would re-brew every working day until the first batch finally
  landed, overshooting hard. The `GateResolve` demand branch now credits
  the pipeline:
  `effective_on_hand[sku] = real_on_hand[sku] + in_flight × batch_yield[sku]`,
  then runs the same pure `decide_demand_outcome` on the effective map.
  `in_flight` is the count of open Jobs of this JobKind minus this one (its
  own gate is open at decision time; oversupply siblings are already
  terminal, brewing siblings are still open). The JobKind is read off
  `GET /api/jobs/{job_id}` — the `step.ready` payload carries the step kind,
  not the JobKind — and the count off the jobs-api list `?kind&status=open`
  total. `batch_yield` is a new demand-gate metadata map, sibling to
  `expected_daily_demand`, carrying the per-brew finished-unit yield of each
  target SKU (e.g. `FP-STOUT-1-6-BBL = 360`).

Together the two restore the feedback loop the gate was supposed to be:
production is pulled up to fill the day's brew slots — one per working day
for the low-volume beers, two for the high-volume ones — and throttled back
down once real-plus-in-flight stock clears the demand-window threshold.
The decision stays a pure function of stock; only the inputs to it grew an
honest accounting of what's already on the way. The IO (kind lookup,
in-flight count, on-hand fetch) lives in `outcome_for`; `decide_demand_outcome`
stays pure and unit-tested, including the exact day-58 regression — same
on-hand, zero in-flight brews → it brews; same on-hand, enough in-flight
yield → it skips. The open question that remains is empirical: does the
365-day regen hold every SKU's finished-goods above zero across the full
seasonal-demand curve? The per-beer slot capacities are set to clear peak
(ipa 4, pale 2, stout/lager/hazy 1) and the in-flight gate regulates
below that, so the model should hold — but only the regen proves the
seasonal curve, not review.
