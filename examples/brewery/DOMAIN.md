# Algedonic Ales — example tenant

Algedonic Ales is an industrial-scale brewing operation used as the
public demo tenant that ships with BOSS — clone the repo, run
`docker compose up`, and the brewery builds itself live in your
browser: the simulator ticks forward from day 0 and fills in a year
of operations as it runs (the SPA is sparse on first load). BOSS expands to **Beer Open Source
Software — for System Modeling**; *algedonic* is Stafford Beer's
term for the pleasure/pain feedback signal in cybernetic systems
— a fitting brand for the demo brewery, and the company name
(Algedonic, LLC) behind the project. It's the second example
tenant in the repo — the other, `examples/used-device-shop/`, is a
used-device refurbisher — and exists to exercise BOSS core through a
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
| Brewers, cellar techs, bartenders, drivers | `employee` (a `person`) | `seeds/employees.json`; each one's role + department is a Class row from `seeds/classes.toml`. |
| Distributors, bar/restaurant accounts, chain retail | `account` (a `person`) | The account's flavor is its `account_type` — `wholesale-distributor`, `bar-restaurant`, `chain-retail`, or `corporate-event` — validated against the Class registry. |
| Grain, hops, yeast, and packaging suppliers | `vendor` (a `person`) | 40 suppliers in `seeds/vendors.toml`. |
| Tap launches and seasonal promotions | `campaign` (an `intangible`) | The Subject a marketing-push Job is about. |
| Recipes — West Coast IPA, Czech Pilsner, the barrel-aged sour | `recipe` (brewery kind) | Identity + version history, grain bills, hop schedules, required vessels. A `morning-brew` step names one via `recipe_id`. |
| Equipment — mash tuns, fermenters, glycol chillers, the kegging line | `equipment` (brewery kind) | Per-unit identity + service history; the Subject an `equipment-preventive-maintenance` Job runs against, e.g. `Subject { subject_kind = "equipment", id = "fermenter-04" }`. |

The two brewery kinds (`recipe`, `equipment`) are rows in
`seeds/subject_kinds.toml` — adding a tenant Subject kind is registry
data, never a core code change.

## What's in this directory

```
examples/brewery/
  DOMAIN.md                 ← you are here
  README.md                 ← short pointer for repo readers
  scripts/gen_employees.py  ← deterministic roster generator → employees.json
  data/                     ← supporting data the generators draw from
  seeds/
    classes.toml            ← Roles, Departments, Beer Styles,
                              Vendor Categories — brewery taxonomies
                              as Class registry rows
    classes.json            ← the Class rows seeded via
                              POST /api/classes/batch — the form the
                              install / reset / CI paths load
    locations.toml          ← Location rows for the demo brewery
                              (one taproom, one brewhouse, three
                              distribution routes)
    job_kinds.toml          ← 25 JobKinds — e.g. morning-brew,
                              wholesale-keg-order, seasonal-release,
                              ingredient-restock, equipment-preventive-maintenance,
                              tap-launch, brewery-hire, payroll-run,
                              ap-payment-run, sales-tax-filing
    tenant.toml             ← Sim rates per JobKind + per Subject
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
create each JobKind, what Subject kinds to draw from, and how
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
- **Sim drives both tenants.** The shape-driven sim runs against
  either tenant's seed files without modification — both the brewery
  and the used-device shop prove the registries are load-bearing.
