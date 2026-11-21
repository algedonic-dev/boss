# Platform station bundle — stations as DATA, not migrations

One file per station: `<name>.toml` holds exactly one `[[station]]`
whose `name` is the file name, declaring the fields of `StationSpec`.
The loader refuses `upstream`: a station's walk upstream is its parent
station's page, derived from the tree (backlog a1c0a0ff). It also refuses
the `department` label, retired with the department floor (backlog
aa82eb11). `station-rows-name-served-routes.test.ts`
holds every route a row names to one the router serves. **Adding a station is
dropping a file in.** Changing one is bumping its `version`.

Retiring a declaration field also requires a new version. The live-effect
guard compares the authored row with the system of record before the new
reader runs, so omitting the department label publishes a new version
without it. Previous versions and their audit facts remain intact. The
stored compatibility column is retained for older builds.

Since 2026-09-18 (backlog 393d3234, consolidation H4) this directory
is where a platform station is DECLARED. Before it, seven migrations
were the only home — 116 seeded two rows, 118 and 124 one each, 130 /
133 / 20260910201453 bumped versions, 20260915023614 authored one over
its projection, and 119 / 138 / 139 / 202609101900 UPDATEd columns in
place. Those files stay exactly as they are (migrations are append-only
history); what changed is that `infra/lint/migrations-declare-schema-
only.sh` refuses `INSERT INTO stations` in any migration newer than its
cutover stamp, and `the_stations_bundle_is_the_migrations_pg.rs` holds
this directory equal, column for column, to the active rows those
migrations produce — and proves the seed can rebuild an emptied table
from this directory alone.

**Published by `boss-platform-workflow-seed`**, the same binary and
the same `--seed-path` every launcher already passes: the station
bundle is the `stations` directory beside the Workflow bundle
(`--stations-path` overrides). Insert-if-missing by (name, version),
through `boss_jobs::station_seed::seed_stations`:

| live registry                                   | outcome                 |
|-------------------------------------------------|-------------------------|
| no row of this name                             | insert, active          |
| active row at a LOWER version                   | publish; retire the old |
| row at (name, version) active and EQUAL         | present — untouched     |
| row at (name, version) active and DIFFERENT     | REFUSED, by field       |
| row at (name, version) exists but is not active | superseded — untouched  |
| no row at (name, version); active row is HIGHER | behind — untouched      |

The refusal is the point: a file edited without a version bump would
otherwise disagree silently with every deployment that already has
the row. Live authoring (`POST /api/stations/{name}/publish`) is
untouched — an operator's later version supersedes the file's, and the
seed reports it rather than rewriting it.

Parsed by `boss_jobs::seed_loader::load_stations`, which runs every row
through the same viability gate an API publish passes and refuses a
file named for a station it does not hold, a status other than
`active`, and a version below 1. `created_at` is not declared here: it
is stamped by the seed's clock when the row is written.

**`line`** names the Design token a station's card rail draws
(`line = "it"` draws `--map-line-it` in `apps/web/src/styles.css`). The
root and each department — a station directly under it — declare one;
every station below draws the nearest line up its parents, so a child
declares none. A station the company hosts at the root itself is no
department and declares none either: `me`, the person's own station
(design 01fa5bdf step 3, backlog 5cb56bb3), one row for every person
because the session decides whose it is. Its children `my-watchlist`
and `q.platform-admin.task` stood at the root between the executive
desk dissolving (design 2910ec3d car 1, backlog 878fd1e7) and `me`
existing to take them.
`every_top_level_station_names_a_line_the_stylesheet_declares`
(`crates/core/boss-jobs/tests/station_tree.rs`) names that station and
refuses any other top-level row with no line, or one naming a token the
stylesheet does not declare (design c4854fd4, backlog 85cb2103).

**`placement`** is where a station stands on its PARENT's floor:
`placement = { x = 590, y = 120, w = 230, h = 170 }`, a box in floor
units. Every floor is 1440 x 900 units (`FLOOR_WIDTH` and `FLOOR_HEIGHT`
in `crates/core/boss-jobs/src/stations.rs`), the size of the Plan
artboard David chose the station view on (backlog e3a927c3), and the
view scales it. The placement is optional: a station with none is laid
out on the view's fallback grid. The viability lint refuses an empty box,
a box past the floor's edge, and a box on a row with no parent; the seed
and the live publish door refuse a box that overlaps an active
sibling's (`validate_station_placements`). Moving a station is a new
`placement` and a version bump.
