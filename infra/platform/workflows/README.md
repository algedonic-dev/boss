# Platform Workflow bundle — protocols as DATA, not Rust literals

One file per protocol: `<kind>.toml` holds exactly one `[[workflow]]`
whose `kind` is the file name. **Adding a protocol is dropping a file
in.** Nothing is appended anywhere; two cars adding kinds touch no
shared line. Pin a kind's decided shape in its own test file,
`crates/core/boss-jobs/tests/platform_bundle_<kind>.rs`, for the same
reason.

This directory was one file (`workflows.toml`) until 2026-09-08, when
two protocol cars parked in one day and the second was left behind on
the tail conflict — the exact shape CLAUDE.md §9a records for
`manifest.txt`, and collapsed the same way `infra/postgres/schema/`
was: the listing is the definition, every reader derives it
independently (`seed_loader::load_workflows` reads the directory in
file-name order; `timers-leave-a-packet.sh` greps `*.toml`; `boss
workflow publish <kind> infra/platform/workflows` or
`.../<kind>.toml` names the same row). Load order carries no meaning:
the seed is insert-if-missing per kind and the viability lint is
per-spec. The loader refuses a file named for a kind it does not
hold, a file holding two, and an empty directory.

David, 2026-08-15: "get those protocols prioritized to be fully moved
into data and registry configurations. That is hunting leakage between
the layers too" — CLAUDE.md's own test: "a protocol that cannot be
replaced without a deploy has leaked into the substrate".

Three of the four registries carrying the operating model now seed as
data the same way this one does: dispatcher_rules from
`infra/dispatcher/rules/` (2026-09-11), stations from `../stations/`
and step_plugins from `../step-plugins/` (both 2026-09-18, backlog
393d3234 — until that day each of those two was declared only by
migration inserts, which is a seed nothing can re-run or drift-check;
the JS under `infra/step-plugins/` was data, the row naming it was
not). `workflows` was the last out: its kinds were Rust literals in
`platform_workflows()`, and an API publish of one is REVERTED on the
next boot because bootstrap_reconcile republishes the code default
(68331085).

**`platform_workflows()` is EMPTY as of 2026-09-11**, so this directory
is the whole platform protocol set. The last four out were
`design-doc-review` and the three `maintenance_spec` chores (backup,
audit-integrity, ledger-replay), and the roster is now pinned empty
from two sides: `platform_workflows_carries_the_shipped_meta_kinds` in
registry.rs, and the lint below, which requires the function body to
read exactly `vec![]`.

A kind that leaves the roster for this directory does NOT change the
row a live deployment already has. `bootstrap_reconcile` only iterates
the roster, so the kind simply stops being reconciled; the seed is
insert-if-missing and an active row is present, so it is untouched. The
four rows kept their versions — `design-doc-review` v1, the three
chores v3 — no version was retired, and no in-flight packet's pin
moved. What changes is ownership: nothing rewrites those rows any more,
so an operator's edit survives a pod roll, and a correction the tree
makes no longer reaches the row on its own. `design-doc-review` is the
worked case: its live v1 `description` still names a surface deleted on
2026-09-10, because reconcile's drift test excludes `description` as
cosmetic, and the file here carries the corrected text. Publishing a
new version from a bundle file is `boss workflow publish <kind>`.

Parsed by `boss_jobs::seed_loader::load_workflows`, the same reader the
brewery's 25-row bundle uses. Authored rather than generated on
purpose: `WorkflowSpec` does NOT serialize to TOML — TOML has no null,
so every `None` field fails with UnsupportedType(unit). The fidelity
guarantee therefore runs the other way, and it is not optional:
`the_platform_bundle_matches_the_specs_it_replaced` (registry.rs)
parses this directory and asserts each converted row equals the spec
it replaces, field for field; `platform_bundle.rs` runs the viability
lint over every file and pins the one-kind-per-file rule.

That test is David's answer to protocols-as-data Q4 made mechanical:
"moving user-feedback v10 from code to bundle must produce a row
identical to the live v10 — not v11. If the loader publishes a new
version instead of recognising the existing one, every in-flight
packet keeps its old spec and the board grows a second lineage."

Seeding is insert-if-missing (`boss-platform-workflow-seed`): a fresh
database gets every kind here at v1; a live one keeps whatever it has,
and a new version goes live through `boss workflow publish`, which
reads the same file — one definition.

## A kind the registry admits and this directory does not is a defect

`infra/lint/the-live-protocols-are-the-authored-protocols.sh` compares
the live registry to this tree and FAILS naming any admitted kind the
tree does not author — the direct analogue of
`the-live-rules-are-the-authored-rules.sh` one registry down. Measured
2026-09-10: 84 admitted kinds, twelve of them with no authored source
anywhere — `gate-run` (the protocol every car's gate packet runs on),
`publish-request` (how a credential-less workspace reaches the forge),
`maintenance-sweep` (the chore family seven clock rules spawn),
`approval`, `doc-flatten`, `incident`, `join-a-node`, `learning`,
`park-a-job`, `protocol-retro`, and the brewery's `keg-return` and
`tasting-panel`. All twelve were backfilled in the same change that
added the lint, so it landed with an EMPTY exemption list and stays
that way: publishing a new version live is still legal, leaving the
tree unable to describe it is not.

The lint counts four authoring homes, because the question it asks is
"can a reader find out what this protocol is": this directory, a
tenant's `examples/<tenant>/seeds/workflows.toml`, a Rust literal in
`platform_workflows()`, and an insert in `infra/postgres/schema/`
(how `repair-a-train` arrived). The last two are duplication this tree
is moving away from, not endorsements — and since 2026-09-11 the lint
says so about the third directly: a kind in `platform_workflows()` is a
FAILURE on its own, not merely a tolerated home, because a protocol
there cannot be changed without shipping a binary. The reverse
direction — a file with no live row — is REPORTED, never failed on: it
is the window between a protocol car's merge and the converge behind
it, and the permanent state of whichever tenant a deployment does not
run.

## A live row that no longer says what its file says is a defect too

The check above is satisfied by a file that exists and is WRONG, and one
was: `design-doc-review`'s live v1 `description` named `boss-docs-api`,
`/api/design/pending-decisions` and the flush jobs — all deleted on
2026-09-10 — while the file here carried the corrected text (backlog
e882b74c). Two mechanisms each declined to close it: the seed is
insert-if-missing, so a present row is skipped whole, and
`bootstrap_reconcile`'s `kind_body_matches` excludes `description` as
cosmetic. The same lint now compares **`description`, `label` and
`category`** — the scalar strings an operator reads — between each file
here and the live ACTIVE row for that kind, and names every
disagreement with the kind, the field, the live version and an excerpt
either side of the first differing character.

`owning_team` is deliberately NOT compared: the loader overrides the
file's key (`spec.owning_team = default_owner`), so a disagreement there
is a file claiming something no publish could ever make true.

Every other key is compared as well, since 2026-09-28: the step facets
(count, titles, required and optional fields, labels, agent blocks),
every other step key (`ready_when`, `kind`, `metadata_defaults`, …) and
every other workflow key (`metadata`, `subject_kinds`,
`metadata_schema`, `entitlements`). Structural keys were out on the
argument that they need the publish path's normalisation first;
measured over all 56 live kinds, only a step's `authority_role` did (the
publish fills it from the owner role, so it is compared only when the
file names one), and leaving the rest out hid two files whose revisions
the registry never took — `incident`'s surface and `publish-request`'s
owner role (backlog 462cdfe3). A file declares no version, so a finding
names the LIVE version it was measured against.

A drift is **REPORTED, never failed on** under a bare invocation, for
the same reason the existence check tolerates its own direction: a car
edits a description, merges, and the row does not move until an operator
runs `boss workflow publish <kind> infra/platform/workflows/<kind>.toml`.
Redding every car in between would be the churn argument that excluded
the field from reconcile, arriving again as a red gate. Run the lint
with `--require-live` to get a verdict instead: drift exits 2, and an
unreachable registry exits **75** rather than skipping to 0, because a
check that passes when it could not read is worse than no check. The
comparison also refuses (exit 1) rather than report a vacuous clean
bill if it compared fewer than 20 kinds — an empty bundle, a broken
reader, or a registry answering about a different world all find no
drift, which reads exactly like agreement.

**Render a backfill from the live row; do not retype it.** A file that
disagrees with the live row replaces one problem with a worse one. Four
columns have no TOML key and so cannot round-trip — `version`,
`status`, `created_at` and `owning_team`. The first three are correct
to omit: the seed is insert-if-missing, so a fresh database publishes
this body as its own v1 while a live lineage keeps the version it
reached. `owning_team` is written in the files that have one only to
record the live value; the loader stamps the bundle's default and
ignores the key.

## Every protocol names its owner station

Each row's `metadata.owner` is the stable `id` of the standing station
that owns the protocol (docs/design/station-tree.md §2 and §4; T6 of
design 4b27a65a, backlog baac0b65): the company's root, `engineering`,
`operations` or `finance` today, as design c0ae096f §1 decided on
2026-11-10 (twelve rows moved from the first assignment in car 2's
first sweep; held `publish-a-protocol` moves at the platform owner's
own publish). Three of the twelve, `break-glass-enrolment`,
`enroll-a-managed-agent` and `workflow-design`, are passkey-protected
on the server, so each carries a file under
`infra/platform/workflow-holds/` and goes live at the owner's signed
publish; the other nine an agent publishes (review 6254feec). It names
an id, never a name or an
address, so re-parenting or renaming a station moves no protocol, and
moving a protocol is changing this one value (§8 rule 3).
`station_tree::validate_protocol_owners` judges it, and
`crates/core/boss-jobs/tests/station_tree.rs` pins every row. The steps
a protocol places are checked against its owner's subtree (next
section; docs/invariants/a-protocol-names-only-its-owners-subtree.toml).
`metadata.department` stays beside it until the department join reads
the owner: `boss_jobs::department::kinds_declaring`, behind
`GET /api/jobs?department=<code>`, which the department pages
(`apps/web/src/departments/department.ts`) read — and `it` is still a
live department there until its retirement (design df0ecd8b).

## A step stands at a station, or hands off

A step names WHERE it runs as `metadata_defaults.at`, the stable `id`
of a station inside its protocol's owner's subtree, or names a
hand-off as `metadata_defaults.fulfilled_by`, the kind of the protocol
that fulfils it under that kind's own owner; never both
(docs/design/station-tree.md §1 and §4; T6, backlog baac0b65).
`station_tree::validate_protocol_subtree` judges every row and the
station-tree pins hold the bundle to it, so moving a protocol to
another owner names every step that would fall outside its new subtree
(§8 rule 3). `at` is placement only: who may claim the step is still
its `audience` and `executor`.

Engineering's protocols place every work step (sweep 2 of design
c0ae096f, backlog baac0b65): a robot's work at `build`, a car's
`review` step (held for its review, or riding its train) at
engineering's `review` stage, a boarding train at `loading-dock`, a
train's PR and merge on the `forge` and its CI at `forge-ci`, a landed
car's two debts at `probe-shed` and `landed-review`, a design's
questions at `design-review` and its fold at `design`. A step that
waits on another packet hands off to it: a car's `gate` and a run's
`verdict` are `fulfilled_by = "gate-run"`. An `at` never names a
station whose own predicate refuses the packets standing at that step:
a queue is its predicate OR its tree address, so the car's `review`
placed at `loading-dock` would add held and train-borne cars to the
dock queue the dock's predicate keeps them out of; a boardable car
still stands at the dock through that predicate. The pin does not
require a marker (a trigger, an outcome) to name a station. The steps with
no honest station are named, each with its reason, in
`ENGINEERING_UNPLACED` in `crates/core/boss-jobs/tests/station_tree.rs`,
and `every_engineering_work_step_names_where_it_runs_or_why_not` refuses
a new engineering step that is neither.
