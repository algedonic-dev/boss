# Projection rebuilders — making `audit_log` the source of truth

**Status:** shipped across every domain service in v1. Per-service
rebuild functions live in `crates/{core,modules}/boss-<domain>/src/rebuild.rs`;
the orchestrator is `boss-rebuild-all`.

## Why this exists

The system writes a projection row + emits an event in the same DB
transaction. Both are kept in sync by convention; if the projection
drifts, there is no canonical reconstruction path. The
"audit_log is the source of truth" framing in the correctness
protocol requires a deterministic rebuild, or it's just a slogan.

The shape: `audit_log` is the only authoritative store of state
changes. Every projection table is *derived* — a deterministic
function of the events that flowed through. A
`rebuild_<projection>(pool)` function per service drops its
projection and replays the relevant slice of `audit_log` to
reconstruct it.

This enables three things the live-write-only shape can't:

- **Event-log as the only seed.** BOSS Brewery's playground state
  isn't a hand-baked dataset — the simulator builds it live,
  emitting events through the public API into `audit_log`.
  Rebuilders rehydrate every projection deterministically from that
  log, so the schema can evolve without a baked seed drifting against it.
- **Replay-based testing.** Sim runs become reproducible — given
  the same audit_log, every projection comes out identical.
- **Fearless schema evolution.** Adding a column to a projection
  no longer needs a backfill migration; just rebuild.

## The pattern

`crates/modules/boss-messages/src/rebuild.rs` is the reference
implementation. Every domain rebuilder follows the same shape:

1. **Advisory lock** (`pg_advisory_xact_lock`) on a key unique to
   this projection — concurrent writes block briefly during a
   rebuild.
2. **Drop the projection table contents** (`DELETE FROM <table>`)
   inside a single transaction.
3. **Read every event** matching this projection's topic prefix
   (`messages.message.%`, `inventory.item.%`, …) ordered by
   `audit_log.id`.
4. **Replay** — for each event, mutate the projection per a
   per-event handler.
5. **Tolerate gaps** — events with old payload shapes (missing
   fields the rebuilder needs) are skipped with a `warn!` and
   counted in `RebuildReport.events_skipped` rather than aborting.
6. **Commit + return a report** with counters
   (`events_processed`, `rows_inserted`, etc.).

### Critical correctness rule: timestamps in events, not `NOW()`

Every projection write that previously used SQL `NOW()` has to
read the timestamp **once in the request handler** from the clock
service (`state.clock.now().await.now`, wrapped as
`req_now(&state.clock)` in each module), then pass it to both the
storage layer and the event payload. Otherwise, the DB stamps `T1`
and the event payload carries `T2 ≈ T1 + 2ms`, and a rebuild
reproduces a different value than the original projection — failing
exact equality. Reading from the clock service rather than
`Utc::now()` also keeps the timestamp on the sim clock when the
deploy is replaying simulated activity.

Two ergonomics enforce this across services:

**`DomainPublisher::emit_at(kind, payload, timestamp)`**
(`boss-core/src/publisher.rs`) — publishes an event with a
caller-supplied timestamp; it takes no defaulted-`Utc::now()`
variant, because a wallclock default leaks wall-time rows into a
sim regen instead of stamping the sim clock. The handler reads `now`
once from the clock service and passes it both to the projection
write AND to `emit_at`. Audit_log row's `timestamp` column then matches what the
projection's `created_at` / `updated_at` got bound to.

**Repository trait `_at` overloads** — every mutation method
gets a sibling that takes the timestamp explicitly:

```rust
async fn create_job(&self, job: &Job) -> Result<(), JobsError> {
    self.create_job_at(job, Utc::now()).await
}
async fn create_job_at(&self, job: &Job, now: DateTime<Utc>) -> Result<(), JobsError>;
```

The `_at` form is what the handler calls when emitting an event.
The convenience overload stays as a default impl so existing
non-event call sites (tests, scripts, internal cleanups) compile
unchanged. The Pg adapter binds `now` explicitly into the
`created_at` / `updated_at` columns.

Pattern in the handler:

```rust
let now = state.clock.now().await.now;
state.jobs.create_job_at(&job, now).await?;
publisher.emit_at(JOB_CREATED, serde_json::to_value(&job)?, now).await;
```

Rebuilders that don't follow this can't reproduce timestamps
exactly.

## Operator caveat: cross-service FK cascade on TRUNCATE

Per-service rebuilders use `TRUNCATE ... CASCADE` for the
`accounts` and `employees` parents. Without CASCADE, FKs from
not-yet-rebuilt-by-this-arc tables blocked the wipe.

The trade-off: CASCADE wipes those out-of-scope tables along with
the parent. For a fresh-bootstrap path (the primary use case)
every dependent table starts empty so the cascade is a no-op. For
a live-DB-rebuild path, the cascade is destructive of state in
tables that don't yet have their own rebuilder. Operators running
`boss-rebuild-all` against a populated live DB should expect those
tables to need re-seeding.

## Threat model — what rebuilders catch

| Failure | Without rebuilders | With rebuilders |
|---|---|---|
| Projection drift from event log | invisible — both can be wrong, no canonical | rebuild always reconciles to event truth |
| Schema column added | needs custom backfill migration | rebuild populates from event payload |
| Bug in projection write | wrong rows persist | re-run rebuild after fix → corrected |
| Audit_log restored from older snapshot | projections still on newer state, inconsistent | rebuild snaps everything to the snapshot moment |
| Service emitting to NATS but `nats_url` unset | events silently dropped; rebuild reproduces empty projection | dangling-FK lint catches projected gaps; `boss-audit-integrity-check` daily timer surfaces them |
| Cross-event reference dangling (invoice → missing account) | projection rebuilds with orphan ids; SPA deep links 404 | dangling-FK rule in `boss-events::integrity` flags every offending audit_log id |

### Soft FK lint

`boss-events::integrity::foreign_ref_rules()` declares the
cross-event invariants that a clean log must satisfy. The rules are
data, not a Rust constant: it `include_str!`s
`seeds/audit_invariant_rules.toml` and caches the parse in a
`OnceLock`. Each rule is a child-event-field → parent-event
reference (e.g. `commerce.invoice.created.account_id` →
`accounts.account.created`); the daily `boss-audit-integrity-check`
timer runs the check alongside the chain-hash verifier and the
gap/regression scan. Add new rules by appending a `[[rule]]` block
to the TOML and restarting the timer — no Rust edit. The bar is
"this ref has burned us once in a real rebuild, or a missing parent
would silently 404 the projection".

## Operational cadence

Rebuilders are idempotent and safe to run on a live system —
the advisory lock briefly blocks concurrent writes, then state
catches up via the event handlers (which keep re-applying the
events that arrive during the rebuild window). Rebuild is a
one-line ops command per service:

```sh
boss-messages-rebuild
boss-inventory-rebuild
# …
```

Or top-level via `boss-rebuild-all`, which walks every service in
dependency order and runs them sequentially.
