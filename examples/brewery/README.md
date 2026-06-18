# Algedonic Ales (example tenant)

Public OSS demo tenant. BOSS expands to **Beer Open Source Software
— for System Modeling**; this brewery is its namesake.
**Algedonic** is Stafford Beer's term for the pleasure/pain
feedback signal in cybernetic systems — fitting for the demo
brewery running on a state-machine OS, and a nod to Algedonic, LLC
(the company behind BOSS).

The tenant exercises BOSS core through a domain anyone can reason
about — production batches, kegs, fermenters, distributor routes
— rather than the heavier device-refurb-and-service specifics that
the in-tree used-device-shop example uses.

Algedonic Ales is industrial-scale: a 30+ BBL brewhouse, multiple
fermenters in parallel, daily kegging, a working taproom, regional
distribution. Not a microbrewery.

See [`DOMAIN.md`](DOMAIN.md) for the narrative + how each brewery
concept maps onto BOSS primitives.

Seed data lives under [`seeds/`](seeds/). The brewery **prepare**
step — `boss-brewery-sim prepare`, the shared `prepare_model` lib
fn — reads these TOML/JSON files and POSTs each row through the
public API, so every seed lands as an event in `audit_log` rather
than as a hand-written table row. The Class registry (roles,
departments, vendor categories) loads the same way — as data, via
`POST /api/classes/batch` from [`seeds/classes.json`](seeds/classes.json),
not a direct SQL load — since each employee's role/department is
validated against it, so classes seed before the roster.

The SPA "first impression" surfaces are built live by the brewery
sim: a fresh install seeds the tenant reference data, then the sim
ticks forward from day 0 and the projections fill in as it runs.
`validate-brewery-sim.sh` is the maintainer correctness gate — it
runs the sim-year and asserts a clean rebuild.

Algedonic Ales is the **active decision tenant** for BOSS-core
simplification work. When a design choice has to land on either a
vertical-flavored shape or a generic shape, the brewery is the test
that forces the latter. Anywhere the brewery has to fork core is a
gap worth surfacing — see the `# Gaps` block in
[`seeds/tenant.toml`](seeds/tenant.toml).
