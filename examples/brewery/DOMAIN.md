# Algedonic Ales — example tenant

Algedonic Ales is an industrial-scale brewing operation used as the
public demo tenant that ships with BOSS — clone the repo, run
`docker compose up`, and the brewery builds itself live in your
browser: the simulator ticks forward from day 0 and fills in a year
of operations as it runs (the SPA is sparse on first load). BOSS expands to **Beer Open Source
Software — for System Modeling**; *algedonic* is Stafford Beer's
term for the pleasure/pain feedback signal in cybernetic systems
— a fitting brand for the demo brewery, and the company name
(Algedonic, LLC) behind the project. It is the one shipped example
tenant and exercises BOSS core through a
domain anyone can reason about without prior context.

The brewery is **not a microbrewery**. Think regional production-scale:
30-40 BBL brewhouse, multiple fermenters running in parallel, kegs
moving daily to distributors, a working taproom, and a recurring
cadence of seasonal releases and collab launches.

## What the brewery does

Three product lines:

1. **Taproom pours** — house lineup of lagers, ales, IPAs, and
   stouts poured at the on-site taproom. Volume-driven,
   recipe-stable, runs on a fixed daily production cycle (mash →
   lauter → boil → whirlpool → fermentation start). Some of the
   storefront-equivalent volume comes off the bright tanks straight
   into kegs that feed the bar.
2. **Wholesale keg orders** — recurring orders from regional
   distributors, bar/restaurant accounts, and chain retail. Each
   order is a Job; one or more brew batches roll up to a kegging
   run and a delivery.
3. **Seasonal releases** — limited-run beers, collaborations with
   other breweries, firkin launches, barrel-aged programs.
   Long-cycle, design-heavy Jobs that move from recipe design →
   test batch → full batch → packaging → marketing push.

The brewery operates from a **brewhouse** (production), a
**taproom** (retail + customer-facing), and a handful of
**distribution routes** (wholesale + festival drop-offs).

## How it maps onto BOSS primitives

Every identity-bearing thing in BOSS is a **Subject**, and each Subject
carries a `subject_kind` from the `subject_kinds` registry. Boss core
seeds five root kinds — **person**, **location**, **object**,
**intangible**, and **calendar** — each with named specializations
(`person` covers `employee`, `account`, and `vendor`; `intangible`
covers `campaign`, `invoice`, and the rest). A tenant adds its own kinds
as registry rows instead of forking core; the brewery adds two, `recipe`
and `equipment`. Here is how the brewery's world lands on that registry:

| Brewery thing | subject_kind | How it's modeled |
|---|---|---|
| Brewhouse, taproom, the three delivery routes | `location` | `seeds/locations.toml` — one brewhouse, one taproom, three distribution routes. |
| Brewers, cellar techs, bartenders, drivers | `employee` (a `person`) | `seeds/employees.json`; each one's role + department is a Class row seeded platform-side in `infra/postgres/schema/01-registries.sql`. |
| Distributors, bar/restaurant accounts, chain retail | `account` (a `person`) | The account's flavor is its `account_type` — `wholesale-distributor`, `bar-restaurant`, `chain-retail`, or `corporate-event` — validated against the Class registry. |
| Grain, hops, yeast, and packaging suppliers | `vendor` (a `person`) | 40 suppliers in `seeds/vendors.toml`. |
| Tap launches and seasonal promotions | `campaign` (an `intangible`) | The Subject a marketing-push Job is about. |
| Recipes — West Coast IPA, Czech Pilsner, the barrel-aged sour | `recipe` (brewery kind) | Identity + version history, grain bills, hop schedules, required vessels. A `morning-brew` step names one via `recipe_id`. |
| Equipment — mash tuns, fermenters, glycol chillers, the kegging line | `equipment` (brewery kind) | Per-unit identity + service history; the Subject an `equipment-preventive-maintenance` Job runs against, e.g. `Subject { subject_kind = "equipment", id = "fermenter-04" }`. |

The two brewery kinds (`recipe`, `equipment`) are rows in
`seeds/subject_kinds.toml` — adding a tenant Subject kind is registry
data, never a core code change.

## How the work gets done: one keg order, start to finish

The table above says what the brewery's work is *about*. This section
says how the work gets *done*. The brewery has only so much of
everything — fermenters, trucks, kegs in the cooler, hours of people's
time — and a workforce to use it. BOSS breaks each complex job into
simple steps, and does each step at a **station**: a place where, for as
long as the step takes, the right resources and the right executor are
reserved together to do that step the way the protocol says.

Eight primitives make up that model: resource pool, job packet, job
step, station, executor, machine, protocol and audit record
([factory-taxonomy-v2.md](../../docs/design/factory-taxonomy-v2.md)
defines them). Here is one wholesale keg order walked through all eight.

**The job packet.** A bar on the Mission route orders kegs: two
half-barrels and three sixtels of IPA, plus pale ale, lager, stout and
hazy. That order becomes one **job packet**, a `wholesale-keg-order` Job
about the bar's account. The packet is created under one version of the
protocol and stays on that version for its whole life, so changing how
orders work tomorrow never rewrites an order already on the floor.

**The protocol and its steps.** The **protocol** is the
`wholesale-keg-order` Workflow row in `seeds/workflows.toml`. It names
each **step**, the condition that makes it ready, and the test that says
it is done:

| Step | Ready when | Done when |
|---|---|---|
| confirm-order | the order arrives | the order is confirmed (no field is required: the lines default from the protocol row, and the delivery window may be left empty) |
| stock-check | the order is confirmed | it has recorded `fulfill` or `backorder` against the kegs actually in the cooler |
| pull-and-stage | stock-check said `fulfill` | the ordered kegs are off the shelves and staged at the dock |
| deliver | the kegs are staged | the driver has confirmed the delivery |
| invoice | the kegs are delivered | an invoice for the delivered kegs is issued |

If stock-check says `backorder`, the packet goes straight to its
`backordered` ending: nothing is pulled, loaded or billed against kegs
that are not there. No step starts until the one before it has passed
its test, so no step depends on anyone remembering what the last one
did.

**The resource pools.** A **pool** is a constraint, something the
brewery has a limited amount of. A pool takes one of three shapes:

- **Capacity** is held while a step runs and handed back when it ends:
  the brewhouse's 40 fermenters and 20 brite tanks, the trucks, the
  forklifts, and the people, counted by role from the roster (24
  distribution drivers, 6 warehouse leads) rather than set by hand.
- **Stock** is used up and comes back only through another protocol:
  finished kegs in the cooler (refilled by `morning-brew`), grain and
  hops (refilled by `ingredient-restock`), and empty keg shells (brought
  home by `keg-return`).
- **Rate** is spent and never handed back, like a monthly fuel or
  overtime budget.

**The stations.** A **station** is defined by what it holds: the pools
its parent station hands it and the executors it can seat. Stations form
one tree, and the company at the root holds every constraint there is:

```
Algedonic Ales           the company: every pool starts here
  production             brewhouse, fermenters, brite tanks, brewers, cellar techs
  packaging              the kegging line, packaging techs
  warehouse              the cooler and its keg stock, forklifts, warehouse leads
    cooler
    loading dock
  distribution           trucks, drivers, the three routes
    a delivery run       made for one truckload, released when the truck is back
  finance                billing
```

Some stations are **standing**, set up once and kept, like the cooler
and the loading dock. Others are made for one piece of work and released
after it, like a delivery run. The model's rule is that a station is
never booked past what it holds: when nothing is free (no truck, or no
driver) the step waits, and the record names what it is waiting for.
The brewery's seeds do not enforce that rule yet, because they declare
no station or pool (see *What the seeds hold today* below).

**The executors.** An **executor** is whatever does a step, and there
are exactly three kinds:

- **Humans** do the steps that need hands: the warehouse lead pulls and
  stages the kegs, and the driver delivers them.
- **Automata** are deterministic software. Confirm-order, stock-check
  and invoice are done this way, at computer speed with no person in the
  loop: stock-check is a rule reading real stock, not a judgment call.
  The step-type registry still marks these steps `completion = "agent"`,
  the older word; v2 calls them automaton steps because the dispatcher
  does them itself.
- **Robots** are AI agents, for steps that need judgment but no hands.
  The keg order has none today. If bars emailed their orders as free
  text, a robot could read each email and fill in the order lines before
  confirm-order runs, and adding that step would be a change to the
  protocol row, not new code.

**The machines.** A **machine** is software placed in a station to serve
the work done there. BOSS's dispatcher is the company station's machine:
it watches the steps that finish and reacts to them, and when this order
closes as delivered it opens a `keg-return` packet so the empty kegs come
home. For stock-check the same dispatcher is the *automaton*, because
there the step is its own to do: the step decides which role a piece of
software plays. Tanks, trucks and forklifts are not machines in this
sense. They are pool capacity, which a step reserves.

**The walk.** Put together, the order moves like this:

| Step | Station | Executor | Reserves while it runs | Uses up |
|---|---|---|---|---|
| confirm-order | sales | automaton | nothing | nothing |
| stock-check | cooler | automaton | nothing (it reads the keg stock) | nothing |
| pull-and-stage | cooler, then loading dock | human: a warehouse lead | the lead, a forklift, a dock door | nothing on the record (the kegs leave the shelf here) |
| deliver | a delivery run on the Mission route | human: a driver | the driver, a truck | nothing |
| invoice | finance | automaton | nothing | the ordered kegs, drawn from finished stock as the invoice issues |

The kegs physically leave the shelf at the pick, but the stock record
drops at the invoice: the dispatcher rule
`products-consume-on-invoice-created` in `seeds/rules.toml` drains
finished goods, and books their cost, when the invoice is issued.

Each reservation lasts only as long as its step. When the driver
confirms the delivery, the driver and the truck go back to their pools
and the next order can have them.

The kegs on the shelf came from an earlier packet, a `morning-brew`. It
holds a fermenter for about a week and then time on the kegging line,
and it turns grain and hops into finished kegs. The two
packets never refer to each other: they meet only in the cooler's stock,
which is what lets production and distribution each change how they work
without asking the other.

**The audit record.** Every change is written down once and never
edited: the packet opening, each step claimed and completed and by whom,
the invoice and, with it, the kegs leaving finished stock, the return
packet opening.
Every screen in BOSS — the order's page, the cooler's stock, the books —
is a reading of that record. "Did this order really ship, and who handled
it?" is answered by the record, not by anyone's memory.

**What the seeds hold today.** The protocol, its steps, the roles, the
stock and the dispatcher's rules are live in this directory's seeds. The
stations and pools are not rows here yet: `seeds/` declares no station
tree and no pool, so a step is assigned by role rather than to a named
truck or fermenter, and the fermenter count recorded on the brewhouse
Location does not yet hold back a brew. This section is the picture
those rows will encode.

## What's in this directory

```
examples/brewery/
  DOMAIN.md                 ← you are here
  README.md                 ← short pointer for repo readers
  scripts/gen_employees.py  ← deterministic roster generator → employees.json
  data/                     ← supporting data the generators draw from
  seeds/
    classes.json            ← brewery taxonomies as Class registry
                              rows (vendor categories, revenue
                              categories, product kinds + package
                              units, asset categories), seeded via
                              POST /api/classes/batch — the only form
                              the install / reset / CI paths load
    locations.toml          ← Location rows for the demo brewery
                              (one taproom, one brewhouse, three
                              distribution routes)
    workflows.toml          ← 25 Workflows — e.g. morning-brew,
                              wholesale-keg-order, seasonal-release,
                              ingredient-restock, equipment-preventive-maintenance,
                              tap-launch, brewery-hire, payroll-run,
                              ap-payment-run, sales-tax-filing
    tenant.toml             ← Sim rates per Workflow + per Subject
                              kind, plus anomaly probabilities.
                              Read by the shape-driven sim
                              that generates the seed.
    employees.json          ← Brewery roster (405 people across
                              executive / production / qa / packaging /
                              warehouse / maintenance / distribution /
                              sales / marketing / taproom / finance /
                              people / it / admin). Generated
                              deterministically (fixed RNG seed) from
                              scripts/gen_employees.py.
    accounts.toml           ← distributor / bar / retail account fixtures
    vendors.toml            ← 40 supplier fixtures (grain, hops, yeast, packaging)
    products.toml           ← finished-product (beer) catalog
    subject_kinds.toml      ← Tenant-extension Subject kinds
                              (recipe, equipment)
    parts.toml              ← Brewery ingredient + packaging SKUs
                              with bin / on_hand / reorder_point
                              for inventory_items seeding.
    policy_rules.toml       ← tenant policy seed (org-chart access matrix)
    operator_hires.toml     ← the C-suite personas the tenant runs as
    messages.toml           ← seeded operator-to-operator inbox messages
    bulletins.toml          ← bulletins seeded into the /content surface
```

The SPA's "first impression" surfaces are built live by the brewery
sim: the install seeds the tenant reference data, then the sim ticks
forward from day 0 and the projections fill in as it runs.

## How seeds become live data

The brewery seeds in this directory are **data-first** — TOMLs and a
JSON file rather than Rust. They're authored to be readable and
diffable, and they drive the live brewery playground today. The
shape-driven simulator reads `tenant.toml` to learn how often to
create each Workflow, what Subject kinds to draw from, and how
aggressive to be with anomalies. A fresh deploy seeds this reference
data, then runs the sim against the live API — every order, invoice,
and ledger entry lands as an `audit_log` event in real time. The demo
is the running sim.

## Why the brewery exists in this repo

- **Forcing function for OSS-readiness.** A brewery is universally
  legible — anyone can reason about it from cold — and it names the
  project (Beer Open Source Software — for System Modeling).
- **Forcing function for BOSS-core genericity.** Every place the
  brewery has to fork core (or stretch a primitive past breaking)
  is a signal that BOSS core is leaking example-tenant-flavored
  assumptions. The `# Gaps` block in `tenant.toml` is the explicit
  list.
- **Sim drives the example.** The shape-driven simulator reads the
  brewery's registry data and builds its operations from events.
