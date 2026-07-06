# Inventory value conservation (costing PR 6)

**Status:** open — design for the WIP-reconciliation item in TODO.md's
near-term queue. Decision questions at the bottom feed the tracker.

## The invariant

For every inventory-bearing account, the GL balance and the physical
valuation are the same number, to the cent, at all times, live and
rebuilt:

```
balance(1300) == Σ inventory_items.value          (raw)
balance(1320) == Σ finished_product_inventory.value   (FG)
```

This is the conservation property of the correctness protocol applied
to stock value. Today neither side stores a value — both store a
per-unit average cost and multiply — and four mechanisms each break
the equality independently. The #51-era regen measured the raw-side
residual at ~$421K over a year; the playground shows the FG side
drifting by ~$190K in a quarter. These are not exotic bugs; they are
the arithmetic of integer-cent averages at industrial quantities.

## What breaks it today

1. **Integer-cent moving averages.** `avg_cost_cents` (raw:
   `boss-inventory/src/postgres.rs`, receive) and
   `production_cost_cents` (FG: `boss-products/src/postgres.rs`,
   produce) update via SQL integer division, truncating toward zero.
   With 10,000–20,000 units on hand, one truncated cent is a $100–$200
   step in `on_hand × avg`, and it happens on ~every receive/produce.
   The GL, which accumulated the exact amounts, no longer equals the
   physical valuation, and consumes at the truncated average strand
   the difference permanently. Scale, not precision, is the point:
   raising precision (milli-cents) shrinks the leak per event but the
   class remains.

2. **Per-line rounding at produce.** The produce handler derives an
   integer `unit_cost` per format line via `.round()`
   (`boss-dispatcher-handlers/src/handlers/products_produce.rs`), so
   the posted drain (`qty × unit_cost`) differs from the line's exact
   largest-remainder share by up to ~qty/2 cents — defeating the
   exact mash allocation one step downstream of it. #73 documented
   this at the derivation site; the fix needs the produce endpoint to
   accept a line **total**, not a unit cost.

3. **Opening-JE idempotency is per SKU, forever.** `put_inventory`
   posts the FG opening JE with `source_id = opening-fg-{sku}` — no
   location component, no adjustment generation. Any second PUT on the
   same SKU (a second location, a re-seed, a manual set) moves
   physical state and posts nothing. Latent today (single-location
   brewery) but a designed-in leak.

4. **Two independent COGS writers on 1320.** The invoice-issue
   projection rule posts `DR 5100 / CR 1320` sized from the invoice's
   `cost_basis_cents` (captured at order time), AND the products
   consume path posts `finance.cogs.recognized` at the ship-time
   moving average. Two writers, two prices, one economic occurrence.
   On the live playground the consume path has fired **zero** times
   (no `products.consumed` events at all — physical FG never
   decrements) while 2,146 invoice entries carried the 1320 credit:
   the books recognize COGS against stock the projection still holds.

## Fix shape: value is primary, average is display

Store the conserved quantity. Each inventory row carries
`value_cents` (BIGINT); `avg_cost_cents` / `production_cost_cents`
become derived (`value / on_hand`, display only, never an input to a
GL amount).

- **Receive / produce add exact totals.** The endpoints accept the
  line's `total_cost_cents` (the exact largest-remainder share the
  caller already computed); `value += total`; the GL leg posts the
  same number. No per-unit rounding exists anywhere on the add side.
- **Consume / COGS drain proportional value.** A consume of `qty`
  from a row holding (`on_hand`, `value`) drains
  `round(value × qty / on_hand)` with the final unit taking the
  remainder — `on_hand → 0` forces `value → 0`, so nothing strands.
  The GL credit posts the same number.
- **Conservation holds by construction.** Every mutation's GL amount
  IS the row's value delta, so `balance == Σ value` cannot drift —
  the property stops depending on rounding behavior entirely.
- Facts already carry `total_cost_cents`; payload `unit_cost_cents`
  becomes memo-grade. Projection rules and the deep replay check
  (#72/#74) are unaffected in shape.

Schema change → no back-compat per repo policy: drop the stored
average columns, add `value_cents`, blow away and regen.

## Gate, not post-hoc discovery

PR 6 adds the equality itself as a check: a per-account
GL-vs-physical reconciliation (1300, 1320) in
`validate-brewery-sim.sh` after the rebuild step, and in the nightly
integrity timer set. The #51-era residual was found by hand; this
class should never be findable by hand again.

## Year-scale evidence

_Pending: the 365-day ephemeral-VM regen of main@b094dcc0 (running
2026-07-06) freezes a full year; its end-state GL-vs-physical numbers
land here before this doc merges._

## Open questions

### Q1: Value-primary rows, or higher-precision stored averages?

Value-primary (recommended): eliminates the truncation class outright
and makes conservation structural. The alternative — storing averages
at milli-cent precision — shrinks each leak but keeps the class, and
still needs the drain-side rounding rules defined. Is there a reason
to prefer keeping a stored average (e.g. tenant-visible standard
costing) that outweighs structural conservation?

### Q2: Which flow owns the COGS recognition (the 1320 credit)?

Today both the invoice-issue rule (at `cost_basis_cents`, order-time)
and the products consume path (at ship-time average) post it, and on
the playground only the invoice side ever fires — FG physical never
decrements. Proposed: the **consume** (the physical event) owns
DR 5100 / CR 1320, priced by the proportional-value drain; the
invoice JE keeps revenue/AR/tax legs and drops its COGS leg; every
sale flow (wholesale shipment steps AND taproom close) must then
drive a consume. Alternative: invoice owns COGS and the consume posts
no GL (but then physical and GL price the same drain differently and
reconciliation needs a bridging account). Which way?

### Q3: One PR or two?

Proposed split: **6a** = value-primary storage + line-total
produce/receive/consume + the reconciliation gate (mechanical
conservation, no modeling change); **6b** = COGS ownership per Q2
(touches the invoice rule, shipment/taproom step wiring, and the sim
drive). 6a is safe to land alone and makes 6b's effect measurable.
Agree with the split?
