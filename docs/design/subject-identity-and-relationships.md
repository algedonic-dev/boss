# Subject identity & relationships — giving the model a home

**Status:** approved 2026-07-15 (all seven questions resolved via the
in-app decision tracker). **Owner:** platform. **Provenance:** the
2026-07-14 subject-model audit (three-track: code map, seed/data map,
live DB) and the 2026-07-13 phantom-account incident it grew out of.

## Problem

The subject **vocabulary** is healthy: the SubjectKind registry (five
roots, thirteen concrete kinds, taxonomy via `parent_kind`) and the
Class registry match the architecture decisions faithfully. The
failures are one level down, and they are systematic rather than
scattered:

**1. Identity has no home.** `Subject` in code is an unbacked
`(kind, id)` string pair; there is no core table where a subject's
identity *lives*. Two registered kinds (`campaign`, `customer`) have
no table, no crate, no KB view — yet live jobs carry campaign
subjects and `marketing_assets.linked_campaign_ids` points at ids
that are structurally unenforceable. The inverse also exists: the
`/system/job-kinds` design workflow opens Jobs whose subject is a
JobKind registry row, a kind never registered. The decision record
says a Subject "can exist from its stable id alone" — identity-first
— but there is nowhere for the bare identity to exist.

**2. Relationships are implicit, enforcement is a patchwork.** Each
cross-kind edge gets whatever its author reached for: a real FK
(mostly intra-domain), a handler check (per-field, opt-in), an
audit-trigger rule, or nothing. The tiers do not correlate with
importance: `invoices.account_id` — the incident edge — is guarded
only by the audit trigger, and the handler-side guard documented on
commerce's `people_client` is never called. The jobs
subject-existence gate covers five kinds of sixteen-plus, fails open
on upstream blips, and re-checks nothing after creation. Shipments
have no linkage to invoices or accounts at all. Meanwhile the system
has grown **three partial relationship registries** — the
`audit_log_ref_checks` trigger rules (nine edges), the integrity
scan's `audit_invariant_rules.toml` soft-FK rules, and the FK
patchwork — none complete, none authoritative.

**3. Id conventions fork per writer.** Five account-id conventions
coexist (`acc-bigseed-NNNN` seed, `acc-prospect-NNN`,
`acc-direct-shop`, sim-born `account-NNNNN`, used-device-shop
`acc-NNNN`); vendors fork identically; the same 67 brewery vessels
carry three naming stories across seeds, JobKind prose, and UI
comments. Convention forks are how phantom references get
manufactured — the sim's `account-NNNNN` mints were the raw material
of the July incident.

The incident chain that motivated the audit ran through all three:
a stale in-memory roster (a convention fork's id), referencing a
subject with no identity anywhere (no home), caught by no write-time
check (patchwork), surfacing only in the nightly deep replay-check.

## Design intent being served

Nothing here replaces the standing decisions — each proposal makes
one of them literal:

- *"Subject is a trait, not an enum; `{subject_kind, id}` validated
  against the registry"* → the pair gets a table so validation can
  mean existence, not just vocabulary.
- *"Subject creation is identity-first — the only hard constraint is
  identity"* → an identity row IS that constraint's artifact.
- *"Registries over hardcoded paths"* → relationships become one
  registry instead of three partials plus code.

## Proposal R1 — a core `subjects` identity table

One thin table, deliberately minimal:

```sql
CREATE TABLE subjects (
    kind        TEXT NOT NULL REFERENCES subject_kinds(kind),
    id          TEXT NOT NULL,
    label       TEXT,            -- display convenience only
    created_at  TIMESTAMPTZ NOT NULL,
    retired_at  TIMESTAMPTZ,
    PRIMARY KEY (kind, id)
);
```

- **Identity only.** No attributes — those stay in the domain tables
  and KB views. This is not a unified entity table; it is the
  minimal durable fact "this subject exists" (information is
  simple; the identity row is the smallest possible fact).
- **Every mint upserts it.** Domain services insert the identity row
  in the same transaction as their domain row (via the outbox
  pattern's transaction, once the emitters migrate). A kind with no
  domain crate — campaign today — can mint identity rows directly:
  identity-first becomes literal, and the crate can arrive later.
- **It is a projection.** Rebuildable from `*.created` events like
  every other table, checked by the deep replay-check like every
  other projection.
- **The existence gate collapses to one query.** The five per-kind
  HTTP endpoints (and the eleven unchecked kinds, and the fail-open
  behavior) are replaced by one indexed lookup that works for every
  kind uniformly, including tenant-defined ones.

## Proposal R2 — one relationship registry

Promote the declared-edge idea to the single source of truth:

```sql
CREATE TABLE subject_edges (
    source_kind  TEXT NOT NULL,   -- event kind or subject kind
    field_path   TEXT NOT NULL,   -- payload/column path
    target_kind  TEXT NOT NULL REFERENCES subject_kinds(kind),
    on_missing   TEXT NOT NULL DEFAULT 'abort',  -- abort | warn
    PRIMARY KEY (source_kind, field_path)
);
```

- **Consolidates the three partials**: `audit_log_ref_checks`
  (trigger rules), `audit_invariant_rules.toml` (integrity-scan
  soft-FKs), and the intent currently encoded in scattered handler
  checks. One registry, read by every enforcement point.
- **Enforced where it can abort**: the outbox ref-check trigger —
  in the domain transaction, post-#118 — so a write referencing a
  missing subject fails *before* it becomes state. Resolution is
  against `subjects (kind, id)` uniformly, which is why R1 comes
  first.
- **Swept where it can drift**: a conservation-sweep invariant walks
  the registry nightly (the E-invariant generalized), catching edges
  that predate their rule and any `on_missing = 'warn'` legacy
  edges.
- **First edges to declare**: `job.subject → (declared kinds)`,
  `invoice.account_id → account`, asset holder (see Q5),
  `purchase_order.vendor → vendor`, the shipment linkage (once
  modeled), and the asset event payload refs that are unchecked
  today.

## Proposal R3 — one id-minting authority per kind

Subject ids are minted through one core path (a helper that upserts
the identity row and returns the id, or `POST /api/subjects`), with
one convention per kind. Seeds, the sim, and tenant engines all
route through it. The five-way account fork and the seed/sim vendor
fork end; the audit's "convention fork per writer" class becomes
structurally impossible rather than reviewed-for.

## Proposal R4 — home or retire the inert kinds

- **campaign**: identity rows via R1 now; a `boss-campaigns` module
  only when a real workstream needs attributes (Q4).
- **customer**: blocked on the standing `/shop` email-OTP product
  decision; until then, either mint identity rows at checkout or
  retire the kind from the registry rather than carrying dead
  vocabulary.
- **recipe / equipment** (brewery tenant): finish registry-validation
  Phase B/C or retire; the beer-style taxonomy currently parked
  under `subject_kind="campaign"` moves home when `recipe` goes
  live.
- **job-kind**: register it. The design workflow is real and good;
  the model should admit what the system already does.

## Appendix — defect worklist (own-PR-sized, independent)

Findings of record from the audit; none block R1–R4:

1. `assets.account_id` holds location ids on the brewery tenant
   (170/170) — see Q5 for the shape.
2. `purchase_orders` carries both FK'd `vendor_id` and non-FK
   `vendor` TEXT; the Rust type maps to the TEXT column, bypassing
   the FK.
3. `assets.phase`: closed DB CHECK duplicating the Class rows — the
   "extend via a Class row" promise is false at the DB layer.
4. `classes.subject_kind` has no FK to `subject_kinds`; drift exists
   in both directions today.
5. Doc-claimed-but-unimplemented validation: `products.product_kind`,
   invoice `revenue_category`.
6. Closed enums that should be Classes: `PoStatus`,
   `VendorInvoiceStatus`, `DocumentAudience`.
7. Deferred taxonomy lifts: account tier/type, vendor
   `payment_terms`, revenue categories, tax jurisdictions/rates in
   `rules.toml`.
8. `brewery-hire` subject shape (an existing employee as the hire
   target) — suppressed with `rate=0`; needs remodeling, likely
   subject = the requisition or the org.
9. Dead-by-construction seed ids: `loc-hq` in the sim pool with no
   locations row; `acc-prospect-*` seeded for `sale` but absent from
   the sim pool.

## Phasing

1. R1 table + rebuilder + backfill (from domain tables and the
   audit log) + the uniform existence gate behind a flag.
2. Gate swap: jobs (all kinds now checked), then the remaining
   reference sites as they migrate to the outbox
   (`record_event_in_tx` callers pick up edge enforcement for
   free).
3. R2 registry consolidation: migrate `audit_log_ref_checks` rows
   and the integrity-scan TOML into `subject_edges`; wire the
   outbox trigger and the sweep to read it.
4. R3 mint consolidation (no back-compat needed — regen wipes).
5. R4 kind-by-kind; appendix defects as independent PRs throughout.

**Acceptance:** the audit's dangling counts reach and hold zero; a
new conservation invariant ("every job subject resolves in
`subjects`; every declared edge resolves") runs green over a full
365-day regen and on the playground nightly.

## Open questions

All 7 open questions were resolved 2026-07-15 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

## Decisions

### Q1: Is `subjects` write-through, a projection, or both? (resolved)

Resolved 2026-07-15 — override.

Both sounds good

**Operationally:** domain services upsert the identity row in the
domain transaction AND the rebuilder reproduces it from `*.created`
events — the `financial_facts` dual contract; the deep replay-check
owns its correctness.

### Q2: Edge enforcement default — abort or warn? (resolved)

Resolved 2026-07-15 — override.

Abort by default. We can afford to fix anything that breaks.

**Operationally:** stronger than the doc's phased recommendation —
`on_missing = 'abort'` for every declared edge from day one, legacy
included. No prod data; a regen resets anything a newly-declared
edge breaks, and a loud abort is the point.

### Q3: Do the big soft edges also become composite FKs onto `subjects`? (resolved)

Resolved 2026-07-15 — override.

Sounds good

**Operationally:** the recommendation as written — trigger + sweep
via R2 now; revisit per-edge composite FKs once the outbox
migration settles write ordering.

### Q4: Campaign and customer — identity rows now, crates when? (resolved)

Resolved 2026-07-15 — override.

I am fine with both as crates now. This isn't that hard of a decison.

**Operationally:** skip the identity-rows-only interim — build
`boss-campaigns` and the customer module as real domain crates in
the R1 workstream (hexagonal shape: domain types + port + HTTP
surface + rebuilder), with identity rows landing via the same mints.

### Q5: What is the asset holder edge? (resolved)

Resolved 2026-07-15 — override.

Your recommendation is good.

**Operationally:** a typed holder pair (`holder_kind`, `holder_id`)
validated via R2, replacing the overloaded `assets.account_id`;
custody is subject-valued (the reservation-on-Subject precedent).

### Q6: What subject is org-level work about? (resolved)

Resolved 2026-07-15 — override.

Can the Company be the Subject?

**Operationally:** yes — that is the answer. A first-class `company`
subject kind with one row per tenant; the thirteen org-level
JobKinds (payroll, tax filings, AP runs, facility overhead) get
`subject = the company` instead of the brewhouse location. The
cybernetics framing made literal: the organization being modeled is
itself a Subject in its own event log.

### Q7: Do automation actors get identities? (resolved)

Resolved 2026-07-15 — override.

A job shouldn't be owned by an automation. Only a step should be
owned by an automation. Automation actors shouldn't need identities
I think.

**Operationally:** ownership semantics split by level — `jobs.owner`
is always a human (accountability lives with people); steps may be
executed/owned by automations; no automation-identity registry.
Implementation note: every flow that opens Jobs with
`automation:*`/`rule:*` owners (the sim, dispatcher spawn rules)
must name a responsible human owner instead — the role-appropriate
person for the JobKind, which the `company` subject's org structure
(Q6) can eventually resolve.
