# Design: Global search

**Status:** approved — the four open questions were resolved through
the in-app review (`/system/design` → design-doc-review Job, 2026-08-04)
and are recorded under Decision history. Flips to `shipped` when the
work lands.

Global search is the last piece of the app-tab rearchitecture (the
chrome bar's third element, alongside the app tabs and the sign-in
control). It is also the smallest surface on which BOSS either
demonstrates its central claim or fails to.

## Why this is not a search box

Every enterprise suite has a search box. In a suite assembled from
separate systems, that box is a federation problem: query the CRM,
query the ERP, query the ticketing system, merge, rank, hope the
identifiers line up. The results are three lists that happen to
mention the same customer.

BOSS is not assembled that way. An account is a **Subject**; the work
about it is **Jobs**; what happened to it is the **audit log**. Those
are not three systems that agree by convention — they are three
projections of one log, joined on identity that the system itself
issued.

So the design target is not "search the apps." It is:

> Type a customer's name. Get the Subject, the Jobs about it, and the
> events behind those Jobs — as one result set, because underneath
> they are one thing.

If global search reads as three federated lists, the app split
described in [extending-boss.md](extending-boss.md) has cosmetically
reproduced the thing BOSS exists to replace. This surface is the test.

## What is searchable

Three result kinds, in the order they answer "what am I looking for":

1. **Subjects** — accounts, vendors, employees, assets, products,
   parts. The identity-bearing things. Matched on id, name, and the
   kind-specific descriptors each Subject carries.
2. **Jobs** — matched on title, kind, subject, owner. A Job is the
   unit of work an operator actually navigates to.
3. **Events** — the audit log. Matched on kind and payload. This is
   the one no federated search can offer, because in most suites the
   audit trail is per-system exhaust rather than the system of record.

Deliberately **not** searchable in v1: free-text document bodies (the
KB and design docs have their own surfaces), and message contents
(Inbox has its own filter). Both can land later behind the same
result-kind mechanism; neither is what "find the thing I am working
on" means.

## Ranking

Subjects first, then Jobs, then Events, with recency breaking ties
within a kind. This is a deliberate refusal to score across kinds: a
relevance model that ranks an event above the account it happened to
is impossible to explain to an operator, and unexplainable ranking is
how search boxes lose trust. Grouped-by-kind with a hard order is
legible, and legibility is worth more here than cleverness.

## Scope and policy

Search results are subject to the same policy the surfaces are. A
result the caller could not open must not appear — a search box is an
excellent way to leak the existence of records a role cannot read.
Filtering happens server-side, in the same `PolicyClient` path every
other read uses; never client-side on a wider result set.

## Decision history

Resolved 2026-08-04 through the in-app review flow — a
`design-doc-review` Job, answered in the `review-design` step surface,
flushed here by the queued flush job. The system modelling its own
development, which is the point of the surface existing.

**Q1 — endpoint vs fan-out: the third option.** Core identity
(`subjects`, `jobs`, `audit_log`) is served by one endpoint that can
rank coherently; each app contributes its own scoped search for its
domain detail. The global box answers "what and where", the app
answers "which one". This keeps the cross-domain join in the database
where it belongs without pretending a Tier-1 crate can see Tier-2
descriptive fields.

**Q2 — search reads its own projections.** Not the live domain tables,
and not the log directly: search builds indexes rebuilt from the log
like any other projection. More machinery than querying `accounts` and
`jobs` in place, and it is the answer the correctness protocol gives —
a Subject absent from a domain projection is still findable, and the
index reproduces from the log rather than drifting from it.

**Q3 — v1 proves the claim immediately.** No name-lookup-first
release. A v1 that returns only Subjects ships sooner and demonstrates
nothing a conventional search box does not; worse, it sets the
expectation that search *is* a name lookup, leaving the interesting
version to read as a later feature rather than the point.

**Q4 — dropdown in the chrome, results page in Home.** The dropdown
prioritises results from the app you are currently in, with a
fully-baked results option as the final link, opening the cross-app
results surface in Home. This resolves the chrome-vs-Home framing
rather than picking a side: the dropdown is the app's scoped search,
the Home page is the cross-cutting one — which is also what makes Q1's
third option land naturally.

## Open questions (all resolved — see Decision history)

### Q1: One `/api/search` endpoint, or a fan-out across domain APIs? (resolved)

A dedicated endpoint in a core crate can query `subjects`, `jobs` and
`audit_log` in one round trip and rank coherently, but it reaches
across domains that otherwise talk over ports — and the tier rules put
`subjects`/`jobs`/`audit_log` in core while the descriptive fields
worth matching on (an account's name, a vendor's category) live in
Tier-2 module tables.

A gateway-level fan-out to existing domain APIs respects the
boundaries but multiplies latency by the number of domains, and moves
ranking to a place that has to re-join results the database could have
joined.

A third option: search core identity (`subjects`, `jobs`,
`audit_log`) in one endpoint, and let each app contribute its own
scoped search for its domain detail — the global box answers "what and
where", the app answers "which one".

### Q2: Does search read projections, or the log? (resolved)

Projections (`accounts`, `jobs`, `subjects`) are indexed, current, and
cheap to query. The audit log is the system of record but is
append-only and large — 450k rows in three sim-months on the
playground, millions across a lap.

Searching projections for Subjects/Jobs and the log only for Events is
the obvious split, but it means a Subject deleted from a projection
and present in the log is invisible to search — arguably wrong for an
audit-first system. The alternative is a search projection rebuilt
from the log like any other, which is more machinery but is the
answer the correctness protocol would give.

### Q3: What is the smallest thing that proves the claim? (resolved)

The unified-layer claim is demonstrated by a single query returning a
Subject *with* its Jobs *with* their events. It could also be
demonstrated by a much smaller v1 — Subjects only, with the Jobs and
Events lists deferred — which ships sooner but shows nothing a
conventional search box does not.

Shipping the narrow version first risks setting the expectation that
search is a name-lookup, and the interesting version then reads as a
later feature rather than the point.

### Q4: Does global search belong to the chrome, or to Home? (resolved)

The chrome bar is the only furniture every app shares, which argues
for search living there — it is the one control that should behave
identically in CRM and in System Model.

But a full result set is a page, not a dropdown, and Home already
exists as the cross-app surface. Search-in-chrome with a
results-page-in-Home is a third shape, and it is unclear whether the
dropdown should be a preview of the page or a different affordance
entirely.
