# Subject identity & relationships — giving the model a home

**Status:** design review. **Owner:** platform. **Provenance:** the
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

## Decision history

*(tracker-managed; resolutions land here)*

### Q1: Is `subjects` write-through, a projection, or both?

Write-through (domain services upsert in-tx) gives read-your-write
existence checks; projection-only (rebuilt from `*.created` events)
keeps it honest but eventually-consistent behind the relay once
emitters move to the outbox. Recommended: both — write-through in
the domain transaction *and* reproducible by the rebuilder, the same
dual contract `financial_facts` already honors. The deep replay-check
then owns its correctness.

### Q2: Edge enforcement default — abort or warn?

`on_missing = 'abort'` is correct-by-construction and would have
stopped the phantom-invoice class at the source, but flipping every
legacy edge to abort on day one risks breaking flows whose data
predates their rule. Recommended: new edges abort; migrated-legacy
edges enter as warn + sweep, promoted to abort after one clean
365-day regen each.

### Q3: Do the big soft edges also become composite FKs onto `subjects`?

A `(subject_kind, subject_id)` composite FK from `jobs` (and
`invoices.account_id` → `subjects('account', id)`) would give the
DB-level guarantee, but couples every domain migration to the
identity table and forbids identity-before-projection orderings the
relay introduces. Recommended: trigger + sweep via R2 now; revisit
FKs per-edge once the outbox migration settles the write ordering.

### Q4: Campaign and customer — identity rows now, crates when?

R1 gives both kinds a home without a crate. The question is the bar
for promoting a kind to a domain module: campaign has live jobs and
marketing-asset links today (arguably already over the bar);
customer is gated on the `/shop` OTP decision. Needs a product call
on each.

### Q5: What is the asset holder edge?

`assets.account_id` means "customer account" on the device-shop
tenant and holds a location id on the brewery. Options: split
columns (`account_id` + `location_id`, one null), or a typed holder
pair (`holder_kind`, `holder_id`) validated via R2 — matching the
reservation-on-Subject precedent (`{kind, id}`, not a closed
per-kind column). Recommended: the holder pair; custody is a
subject-valued property, not an account-valued one.

### Q6: What subject is org-level work about?

Thirteen of twenty-five brewery JobKinds — payroll, all four tax
filings, AP runs, facility overhead — are "about"
`loc-brewery-brewhouse` because nothing better exists. Options: an
`organization` subject kind (one row per tenant), a `custom`
org subject, or keep location-as-convenience. This decides what the
event log *means* for company-level work; the cybernetics framing
(the org as the system being modeled) argues for a first-class
organization subject.

### Q7: Do automation actors get identities?

`jobs.owner_id` holds an ActorId union (`emp-*`,
`automation:<slug>`, `rule:<slug>`) with no registry for the
automation side and zero human owners in the demo. Options: register
automation actors as subjects (kind `automation`?), keep the union
informal, or split owner (accountable human/org) from operator
(executing actor). Touches the executor model — the "agents are
additional CPUs in the same machine" framing suggests they deserve
identities too. Park here until R1 lands, then decide.
