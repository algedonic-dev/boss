# Event kinds: the last unregistered vocabulary

**Status:** draft — open questions tracked at `/system/design`
**Origin:** feedback item `af1586e1` (routed `design` 2026-08-09), filed
while working the ontology-convergence framing: before real people
join, the semantic layer must be declared, not folklore.

## The gap, measured

Every load-bearing vocabulary in BOSS is registry-shaped — except the
one the whole system stands on:

| vocabulary | registry | queryable? |
|---|---|---|
| subject kinds | `boss-subject-kinds` | yes |
| taxonomies | `classes` | yes |
| work types | `workflows` | yes |
| step kinds | StepType registry | yes |
| step UX | `step_plugins` | yes |
| job links | `job_edges` | yes |
| **event kinds** | **nothing** | **no** |

Measured on the live box (2026-08-09): the log carries **120 distinct
kinds across 15 sources**, while the repo declares roughly **19 as
named constants** (`boss-jobs/src/events.rs` and per-crate siblings).
The other ~100 exist only at their emission sites. Nothing states what
a kind means, what its payload carries, or who consumes it.

Four consumers already key on kind strings that no registry validates:

- the ref-check machinery (`job_edges` resolves per-kind field paths)
- `subject_edges` per-kind extraction
- `dispatcher_rules.on_event` patterns — a typo'd topic is the
  silent-zero-deliveries class we have already been burned by
- the payload-encryption design (`payload-encryption.md` Q2) needs a
  per-field sensitivity home, and today there is no table for a field
  to be a row in

## The shape of the kind space

The 120 kinds are not a flat list; they come in two shapes, and the
second is why a naive `event_kinds(kind PRIMARY KEY)` table is wrong:

1. **Static kinds** (~45): `jobs.job.created`,
   `people.employee.created`, `ledger.entry.posted` — one emitter, one
   payload shape, enumerable as plain rows.
2. **Dynamic families** (~75 live values from a handful of patterns):
   `step.done.<step-kind>`, `step.assigned.<step-kind>`,
   `step.ready.<step-kind>`. The suffix domain is not free text — it is
   exactly the StepType registry. The family is the declarable unit;
   its legal suffixes are a **reference to the registry that already
   owns them**.

So the registry is compositional: a family row says "kinds matching
`step.done.*` exist, the suffix ranges over `step_types`, the payload
shape is the step-done envelope." Declaring the family once covers
every current and future step kind without a migration per step type.

## What this unblocks

- `payload-encryption.md` Q2 gets its home: sensitivity classification
  becomes columns/rows hanging off `event_kinds` fields.
- Dispatcher rule authoring can validate `on_event` against declared
  kinds at draft time — the typo'd-topic class dies at authoring
  instead of silently delivering zero.
- The ontology David asked for ("a good initial central ontology of
  meaningful semantics") gets its system-of-record surface: 120 kinds
  with descriptions, browsable in the UI like every other registry.

## Open questions

### Q1: Registry table or generated manifest?

A `event_kinds` table (authored like `job_edges`, seeded by migration)
versus a repo-generated manifest (harvested from emission sites at
build time, pinned by a §9a drift test). The table is queryable by the
UI and by other registries (encryption classification wants to hang
rows off it); the manifest cannot drift from code but cannot be
referenced by data either. Proposed: **table**, because two consumers
(encryption, dispatcher authoring validation) need to join against it,
and a §9a pin test closes the drift risk the manifest would have
solved — live-log kinds vs registry rows, failing with the undeclared
kind named.

### Q2: What does a declaration carry?

Minimum useful row: `(kind_pattern, source, description,
suffix_domain NULLABLE → registry name, payload_fields JSONB)`.
The open call is `payload_fields`: full JSON Schema per kind is
heavyweight and will rot; a flat field inventory (name, type, note) is
enough for the encryption doc's classification to reference and for
the dispatcher authoring UI to offer `when`-expression completions.
Proposed: **flat field inventory now**; JSON Schema only if a consumer
actually validates payloads later.

### Q3: Enforcement posture at emit time?

`job_edges` established the dial: `on_missing = warn | abort`, honoring
`audit_log.ref_check`. An undeclared kind at emit time could warn
(count it, keep writing) or abort the transaction. Emit-time abort
turns a missing registry row into a production outage; warn turns it
into a queue item. Proposed: **warn + the §9a drift test failing CI**
— the log stays available under drift, the build does not.

### Q4: Who seeds the 120?

Harvest: static kinds from the per-crate constants plus a live-log
sweep for anything the constants miss; families from the three `step.*`
patterns. One migration, reviewed against the measured inventory. The
per-crate constants stay (they are the emit-site spelling); the §9a
test pins constants ↔ registry rows so neither drifts.
