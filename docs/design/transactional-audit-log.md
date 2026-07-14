# Transactional audit log — closing the provenance gap

**Status:** design review. **Owner:** platform. **Motivating
incident:** 2026-07-13 replay-divergence investigation.

## Problem

The audit log is BOSS's system of record — "the company *is* its
event log" — but it is written **best-effort, post-commit, on a
separate connection** while the derived state it describes commits
transactionally. That inverts log-primacy: the projection is durable
and the truth is optional.

The 2026-07-13 incident showed the full failure chain live. A
committed domain transaction (invoice + `financial_facts` row +
journal entry) was followed by an audit INSERT that the
`audit_log_check_refs` trigger rejected (the invoice referenced an
account the epoch reset had wiped); the publisher discarded the
rejection. Result: 260+ state changes with **no event in the log**,
each permanently unreproducible by rebuild-from-log, caught only
because the nightly deep replay-check reprojects `financial_facts`
from `audit_log` and diffs. PR #115 made these failures loud
(ERROR-level logs); this document is about making them
**impossible**.

Two independent best-effort halves fail separately today:

- **audit half** — `PgAuditWriter` INSERT can fail (trigger
  rejection, pool exhaustion, transient PG error) → state without
  provenance.
- **bus half** — core-NATS `client.publish` is client-buffered with
  no JetStream ack → a reconnect can drop the notification → the
  dispatcher never runs the event's side-effects, and nothing can
  redeliver what was never published.

## The invariant to establish

> Every state-changing operation's audit event becomes durable
> **atomically with** the state change it describes. A rebuild from
> `audit_log` alone reproduces every projection, structurally —
> not as an aspiration the deep check occasionally falsifies.

This is the provenance + determinism half of the five-property
correctness protocol, promoted from "monitored" to "by
construction."

## Today's mechanics (what any design must reckon with)

1. **`DomainPublisher.publish()`** (`boss-core/src/publisher.rs`):
   NATS publish then audit INSERT, both post-commit, failures
   logged (since #115) but not recovered. Return value is bus-only
   and almost every call site ignores it.
2. **The chain-hash trigger**
   (`infra/postgres/schema/02-events.sql`,
   `audit_log_compute_row_hash`): BEFORE INSERT, takes
   `pg_advisory_xact_lock(hashtext('audit_log'))` — a **global,
   transaction-scoped lock** — then allocates `id` + `created_at`
   and chains `row_hash = sha256(prev || canonical)` off the
   current head. Works today precisely because every audit INSERT
   is a microseconds-long autocommit transaction.
3. **The ref-check trigger** (`audit_log_check_refs`, rules in
   `audit_log_ref_checks`): rejects events whose payload references
   a missing projection row. Post-commit, this *creates*
   state-without-provenance; in-tx, it would become a correct
   referential guard that aborts the bad write.
4. **The bundle-import escape hatch**: `SET audit_log.ref_check =
   'off'` for restore sessions (audit first, rebuild second).
5. **Consumers**: the dispatcher runs side-effects off JetStream
   durable consumers with NAK-redelivery; handlers are idempotent
   (the receive-dedup work). The deep replay-check and every
   rebuilder treat `audit_log` as the only source.

## Why "just move the INSERT into the domain tx" fails

The chain trigger's advisory lock is **xact-scoped**. Inside a
domain transaction it would be held for the transaction's full
lifetime — every state-changing transaction in the system
serializes behind one global lock, and any domain tx that takes row
locks before the audit INSERT can deadlock against another doing
the reverse. At demo warp (~66 audit rows/sec sustained, bursty)
this is a throughput wall. The naive fix trades a correctness hole
for a concurrency collapse.

So the design space is really: **where does atomicity live, and
who maintains the chain?**

## Options

### Option A — in-tx audit INSERT, chain moved off the write path

Writers INSERT the audit row inside the domain tx **without**
chaining (plain sequence id, no global lock). The hash chain is
computed asynchronously by the integrity checkpointer (which
already walks the log and signs checkpoints); `row_hash` becomes
nullable-until-checkpointed or moves to a sibling table.

- Atomicity: direct — one transaction, trigger rejections abort
  the domain write. Strongest possible guarantee, fewest moving
  parts.
- Costs: id-order and `created_at`-order are no longer
  commit-monotonic (ids pre-allocate under concurrency), so the
  verifier's id-walk, the trim machinery, and every `ORDER BY id`
  replay assumption need re-audit. Tamper-evidence gains an
  unchained tail window between checkpointer runs. The chain
  becomes a projection of the log rather than a property of the
  write.

### Option B — transactional outbox + single relay (recommended)

Writers INSERT into a per-database **outbox** table inside the
domain tx (plain BIGSERIAL, no global lock, cheap). A single
**relay** drains the outbox in order and, per event: INSERTs into
`audit_log` (the existing chain trigger runs exactly as today —
single writer, zero contention) and publishes to NATS, then marks
the outbox row done. At-least-once end to end; `event_id`
uniqueness makes the audit INSERT idempotent; consumers are already
idempotent.

- Atomicity: the durable hand-off is the outbox row, atomic with
  the domain write. Loss becomes impossible-by-construction;
  `audit_log`'s threat model, chain semantics, id-monotonicity and
  trim machinery are **unchanged**.
- The bus half is fixed by the same mechanism: NATS publish moves
  to the relay with retry — a dropped notification becomes
  redeliverable because the outbox (then the log) holds it.
- Referential guarding moves **up** to the outbox INSERT (the
  ref-check rules run as an outbox trigger, in the domain tx, so a
  phantom-subject write aborts the operation — this would have
  caught the epoch-rewind sim bug on day one). The audit_log-side
  check stays as belt-and-braces; relay-time rejections dead-letter
  loudly instead of vanishing.
- Costs: one new moving part (the relay — dispatcher-shaped,
  systemd unit, ~small); `audit_log` becomes eventually-consistent
  behind the outbox by relay lag (ms-scale); the deep replay-check
  needs an outbox-drained barrier or a relay-cursor watermark so it
  never reads a state/log pair mid-hand-off.

### Option C — in-tx INSERT, keep the global lock, measure

Accept serialization; measure whether real domain txs are short
enough. Rejected as the plan of record: warp-scale bursts already
sustain ~66 writes/sec with multi-statement domain txs, and
deadlock exposure is structural, not tunable. Kept here as the
control against which A/B are judged.

## Recommendation

**Option B**, phased:

1. **(done)** #115 — failures loud.
2. **(done, phase-1 PR)** Foundation, inert until an emitter
   migrates: `event_outbox` table + ref-check trigger on it (same
   rules table + function as the audit-side trigger, which stays as
   belt-and-braces) + `outbox::record_event_in_tx` +
   `outbox::drain_outbox_once` + the `boss-event-relay` daemon
   (deployed via the DAEMONS list) + the epoch-trim TRUNCATE
   ordering (see Q4's resolution).
3. Migrate the fact-adjacent emitters first (commerce, ledger,
   inventory, products — the paths the correctness protocol
   guards), each service a reviewable PR; add the deep-check
   watermark when the first emitter lands (the check must not read
   a state/log pair mid-hand-off).
4. Migrate the remaining rebuild-consumed emitters + the
   kind-classification lint (Q3's rule); `DomainPublisher` keeps
   its post-commit path only for observational kinds.

Each phase lands behind the usual gates and a from-empty regen; the
deep replay-check is the acceptance test throughout — its
divergence count is the definition of done (zero, sustained, on the
playground's nightly timer).

## Open questions

### Q2: Chain maintenance — does Option B keep insert-time chaining forever?

B preserves the status quo (single relay writer → the global
advisory lock never contends). But if audit write volume ever needs
a sharded relay, the chain serializes again at the log. Is
checkpoint-time chaining (Option A's tail-window model) the
eventual end-state regardless, with B as the bridge?

### Q6: Does the dispatcher eventually consume the log instead of NATS?

Once the outbox → log pipeline is ordered and durable, NATS is a
latency optimization, not a source of truth. A log-tailing consumer
(cursor per consumer group) would collapse the delivery stack and
make "the log is the queue" literal — Hickey would approve — but it
re-plumbs every consumer and JetStream already works. Out of scope
for this arc; recorded so the endgame stays visible.

## Decision history

*(tracker-managed; resolutions land here)*

- **Q1 — relay shape (resolved 2026-07-14, phase-1 PR):** separate
  binary, `boss-event-relay` (boss-events `bin` feature), wired into
  `deploy-services.sh`'s DAEMONS list from day one so it never joins
  the service-outside-the-deploy-list rot class. Folding into
  boss-dispatcher stays open as a later simplification if a fourth
  daemon proves annoying on small installs.
- **Q3 — transactional scope (resolved 2026-07-13, David):** the
  rebuild-consumed rule — an event kind goes through the outbox iff
  a projection rebuild consumes it; observational kinds (telemetry,
  health, docs-flush, cockpit counters) stay post-commit
  fire-and-forget. The kind-by-kind classification + the lint that
  keeps it honest land with the phase-2 emitter migrations.
- **Q4 — epoch trim × outbox (resolved 2026-07-14, phase-1 PR):**
  the trim transaction TRUNCATEs `event_outbox` *before* the
  audit_log DELETE. At restart time every outbox row is the finished
  epoch's by definition; the truncate queues behind any in-flight
  relay batch (ACCESS EXCLUSIVE vs the batch's row locks), and audit
  rows that batch committed carry ids ≤ the trim point, so the
  DELETE removes them. Quiescence does not need to wait for the
  relay at all.
- **Q5 — publisher API shape (resolved 2026-07-13, David):**
  explicit transaction threading — `record_event_in_tx(&mut tx, …)`
  at each migrated call site, mirroring `record_fact_in_tx`. Honest
  and grep-able; handlers already carry the tx for facts.
