# Inventory value conservation (costing PR 6)

**Status:** shipped — implemented as PR 6a (#84, value-primary storage +
exact conservation gates) and PR 6b (#85, the consume owns COGS).
Decisions live in the Decision-history below; settled material folds
into docs/architecture-decisions.md at the next release.

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
the equality independently.

Year-scale measurement (2026-07-06 regen, below) sizes the classes
honestly: the raw side now reconciles **exactly** — the #51-era ~$421K
consume-side residual is gone under current main (GR-IR + the #57–#73
costing arc fixed it) — and the surviving truncation leak is the FG
side at ~$6.6K/year. The mechanisms below are still real (and the
opening-JE and two-COGS-writer ones are structural, not rounding), but
the argument for value-primary storage is closure of the class plus a
conservation gate, not a headline dollar figure.

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

From the 365-day from-empty regen of main@b094dcc0 on an ephemeral VM
(2026-07-06; all gates green — zero sim errors, rebuild determinism,
integrity, deep replay-check 74,461 facts / 74,423 entries with 0
divergences):

| Account | GL balance | Physical | Divergence |
|---|---|---|---|
| 1300 raw | $1,413,386.00 | $1,413,386.00 (162,348 units) | **$0.00 — exact** |
| 1310 WIP | $12,210.98 | (in-flight brews at cutoff) | legitimate standing WIP |
| 1320 FG | $2,970,541.40 | $2,977,139.17 (91,430 units) | **physical +$6,597.77** |

- **Raw conservation holds to the cent over a full year.** The
  consume-side residual this workstream was named for is already fixed
  on main; what PR 6 buys on the raw side is a *guarantee* (structure
  + gate), not a repair.
- **The FG +$6.6K is the integer-cent truncation class** (mechanisms
  1–2): real, bounded, and the piece the produce line-total change +
  value-primary rows close.
- **The consume path posted zero COGS all year** — 0
  `products.consumed` events, 0 `finance.cogs.recognized` facts from
  `products_consume` in 365 days. The load-bearing flow is
  commerce's invoice-issue, which **UPDATEs
  `finished_product_inventory` directly** (boss-commerce
  `postgres.rs` — a cross-module write into products' projection,
  not a call through the products port) at invoice-time
  `production_cost_cents`. That's why physical tracks GL within the
  truncation residual, and why the products consume path is dead
  code on the brewery. Q2 therefore also carries an architecture
  decision: whichever flow owns COGS, the FG decrement should go
  through the products surface, not another module's SQL.
- Context: full-year revenue $8,506,488, COGS $1,987,430 (GM 76.6%),
  net −$837,903 (under-absorption at ~65% of design volume — honest).

## Open questions

All 3 open questions were resolved 2026-07-07 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: Value-primary rows, or higher-precision stored averages? (resolved)

Resolved 2026-07-07 — override.

Value-primary is right


### Q2: Which flow owns the COGS recognition (the 1320 credit)? (resolved)

Resolved 2026-07-07 — override.

Consume should own


### Q3: One PR or two? (resolved)

Resolved 2026-07-07 — override.

Sounds good
