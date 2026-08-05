# Transactional audit log — the event write path

**Status:** reopened — the contract below is settled and in force (the
arc that established it completed 2026-07-29; decision history folded
into [docs/architecture-decisions.md](../architecture-decisions.md)
§Correctness protocol & the audit log), but Q2 and Q6 under Open
questions are genuinely undecided, and `living` asserts a doc carries
none. Every new write path in BOSS follows this contract;
`infra/lint/outbox-migration-ratchet.sh` fails CI on any deviation.

Flip back to `living` when Q2 and Q6 are resolved — or mark them
`(resolved)` in their headings if they turn out to be settled and
merely un-annotated.

## The invariant

> Every state-changing operation's audit event becomes durable
> **atomically with** the state change it describes. A rebuild from
> `audit_log` alone reproduces every projection, structurally — not
> as an aspiration a nightly check occasionally falsifies.

This is the provenance + determinism half of the five-property
correctness protocol
([correctness-protocol.md](correctness-protocol.md)), held **by
construction**: the event and the row commit in one transaction, so
neither a crash, a trigger rejection, nor a bus drop can produce
state without provenance (the swallowed-write class behind the
2026-07-13 replay-divergence incident) or an event without a fact.

## The pipeline

Writers stage events on the **`event_outbox`** table inside the
domain transaction (`boss_events::outbox::record_event_in_tx` —
plain BIGSERIAL, no global lock, cheap). A single relay,
**`boss-event-relay`**, drains the outbox in id order and, per
event: INSERTs into `audit_log` (the chain-hash trigger runs
uncontended — single writer) and publishes to NATS, then stamps the
outbox row delivered. At-least-once end to end; `event_id`
uniqueness makes the audit INSERT idempotent; consumers are
idempotent (JetStream durable consumers + receive-dedup).

Consequences worth knowing:

- **`audit_log` and NATS are eventually consistent** behind the
  outbox by relay lag (ms-scale). Anything that must read a
  state/log pair coherently (the deep replay-check, e2e tests)
  drains the outbox first — tests use
  `outbox::drain_outbox_once(pool, bus, n)` and deliberately wire
  **no** direct audit writer, so they only pass through the real
  pipe.
- **Referential guarding aborts the write**: the ref-check rules run
  as an outbox trigger inside the domain tx, so a phantom-subject
  event aborts the operation instead of committing state the log
  rejects. The audit_log-side check stays as belt-and-braces;
  relay-time rejections dead-letter loudly.
- **The relay is deployment-critical.** It ships in
  `deploy-services.sh`'s DAEMONS list, both from-empty quickstart
  launchers (`services-launcher.sh`, `bootstrap-local.sh` — learned
  the hard way, 2026-07-28: without it the dispatcher's
  `step.done.<kind>` signals never reach NATS and no side effects
  fire), and runs as `boss-event-relay --config
  /etc/boss-jobs-api.toml` (its env fallbacks are
  `BOSS_RELAY_DATABASE_URL`/`DATABASE_URL`, not
  `BOSS_POSTGRES_URL`).
- **Epoch trim** TRUNCATEs `event_outbox` before the `audit_log`
  DELETE; the truncate queues behind any in-flight relay batch, so
  quiescence never waits on the relay.

## The three recipes

Which one a write path uses depends on where the event's payload
and context live. All three end at `record_event_in_tx` before
commit.

**1. Per-kind stamp (the default).** Port mutations take
`stamp: &EventStamp`; the Pg adapter builds the payload (it owns
the row) and records the kind it knows. Handlers resolve one stamp
per request — `publisher.stamp_with_actor_at(actor, now)` when a
publisher is wired (its sim probe settles `_simulated`), else
`EventStamp::new(source, actor, now)`. Most domain crates
(commerce, products, inventory, ledger, shipping, catalog,
messages, calendar, accounts, people, content) use this shape.

**2. Events ride the write (boss-jobs).** When handlers derive
marker events from transition context the adapter can't see (status
flips, terminal closes, `step.done.<kind>` dispatcher signals), the
port's `_at` mutations take `events: &[Event]`: the handler builds
the complete list — state event + markers — and the adapter records
all of them in the write transaction. `JobsRepository::record_events`
covers the rare standalone marker with no accompanying write (the
post-materialization `step.ready` pass).

**3. The `EventRecorder` port (row-less events).** For components
whose events *are* their rebuildable state but have no domain row
write to join (cybernetics telemetry), `boss_core::port::EventRecorder`
with `boss_events::outbox::PgOutboxRecorder` stages each event on
the outbox in a small transaction of its own — same delivery
guarantees, honest about having no fact to be atomic with.
`InMemoryEventRecorder` collects for tests.

## Idempotency guards double as event gates

Wherever a write is replay-tolerant, its guard gates the recording:
`ON CONFLICT DO NOTHING` creates, `deleted_at IS NULL` /
`cancelled_at IS NULL` flips, and `rows_affected > 0` checks all
record their event **only when the write actually happened**. A
redelivered or double-submitted operation is a full no-op — no
duplicate event, no marker for a fact that didn't occur. (Before
the arc, six crates published duplicate events on every idempotent
replay, and two paths emitted *before* writing — an event with no
fact on failure.)

## Testing the contract

- **In-memory adapters collect** what the Pg adapter would record
  (`recorded_events()`), so HTTP-tier emit/no-emit contract tests
  run without Postgres.
- **PG e2e suites run publisher-less** and drain the outbox
  explicitly — they cannot pass except through the real
  outbox → relay → audit_log pipe.
- **The flat-ban lint** (`infra/lint/outbox-migration-ratchet.sh`)
  fails CI on any call to the four retired post-commit publish APIs
  anywhere under `crates/*/src`.
- Acceptance at scale: two full-year from-empty regens (2026-07-27,
  2026-07-29) with zero undelivered outbox rows (484K and 1.84M
  events respectively), deterministic rebuild, and exact
  conservation.

## Open questions (recorded endgames — out of scope, kept visible)

### Q2: Chain maintenance — does the pipeline keep insert-time chaining forever?

The single relay preserves insert-time chaining with zero
contention. If audit volume ever needs a sharded relay, the chain
serializes again at the log — checkpoint-time chaining (a chain
computed by the integrity checkpointer over an unchained tail) is
the likely end-state, with today's shape as the bridge.

### Q6: Does the dispatcher eventually consume the log instead of NATS?

With the outbox → log pipeline ordered and durable, NATS is a
latency optimization, not a source of truth. A log-tailing consumer
(cursor per consumer group) would collapse the delivery stack and
make "the log is the queue" literal — Hickey would approve.
JetStream works; re-plumbing every consumer is not worth it until
something forces the issue.
