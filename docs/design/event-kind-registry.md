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

All 4 open questions were resolved 2026-08-10 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: Registry table or generated manifest? (resolved)

Resolved 2026-08-10 — override.

Table is good


### Q4: Who seeds the 120? (resolved)

Resolved 2026-08-10 — override.

Sounds good


### Q2: What does a declaration carry? (resolved)

Resolved 2026-08-10 — override.

flat field works now


### Q3: Enforcement posture at emit time? (resolved)

Resolved 2026-08-10 — override.

Agreed
