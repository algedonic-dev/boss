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
merely un-annotated. Measured grounding for both questions added
2026-08-08 (§What the pipeline measures today); each now carries a
proposed resolution awaiting decision at `/system/design`.

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
  outbox by relay lag (measured 2026-08-08: p50 ~200 ms steady —
  the floor is the relay's 250 ms idle poll, not chain cost —
  p99 5.4 s under the worst burst on record). Anything that must
  read a
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

## What the pipeline measures today (playground, 2026-08-08)

The numbers behind Q2 and Q6 — from the live system, not
estimates. Box: the public playground, PostgreSQL 16.14.

**Demand.**

- Steady state: 88K events/24 h (~1/sec sustained; busiest minute
  4.7K).
- Worst burst on record: the epoch bootstrap replay — 224K events
  in 90 minutes, peaking at 11.8K/min (~197/sec). Relay lag under
  that burst: p50 0.4 s, p99 5.4 s, max 7.6 s, zero pending rows
  once it passed.
- Whole-log scale: 300K rows / 275 MB live (~960 B/row); the two
  full-year regens landed 484K and 1.84M events.

**Relay + chain cost** (pgbench on this box, the identical trigger
on a clone table, realistic ~730-char payloads).

- Steady relay lag p50/p90/p99 = 203 ms / 630 ms / 1.8 s — the p50
  is the relay's 250 ms idle-poll sleep, not chain cost.
- The chain trigger, single writer: 611 rows/sec chained vs 691
  plain in row-per-tx shape (+0.2 ms/row). In the relay's actual
  shape — 100-row batches, the advisory lock taken once per batch —
  **14.6K rows/sec** (0.07 ms/row).
- Four *concurrent* chained writers: 789 rows/sec vs 1,927 plain —
  the advisory lock caps multi-writer scaling at 1.3× (vs 2.8×
  unchained). This is the measured reason the relay stays
  single-writer.
- The nightly integrity checker already recomputes the entire chain
  in SQL at ~72K rows/sec (1.32M rows in 18.3 s) and logs a
  chain-head checkpoint (id + hash + row count) on every run.

**Consumers.**

- Durable consumers on the BOSS_EVENTS stream: **exactly two** —
  `dispatcher-steps` and `dispatcher-rules`, both in
  boss-dispatcher, both at 0 pending. Their transport coupling is
  ~35 lines across two functions; both handlers are already
  transport-agnostic, consuming `(subject, event_id, payload)`.
- The JetStream stream is a second durable copy of the log: 219K
  messages / 210 MB on file storage, 3-day / 4 GiB limits, purged
  at every epoch restart alongside the outbox TRUNCATE.
- Ephemeral core-NATS subscribers: five. Two are display-only SSE
  fan-outs (assets, observability — at-most-once is correct there);
  one is the cybernetics inter-agent message plane (not event-log
  traffic); and two are **load-bearing over at-most-once
  delivery** — the assets ingress (`asset.>` appends into the
  assets repository; the subject isn't even in the stream) and the
  jobs escalation notifier (`jobs.job.created`). Backlog items
  filed; both need a durable leg whichever way Q6 resolves.

## Open questions

All 2 open questions were resolved 2026-08-08 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q2: Chain maintenance — does the pipeline keep insert-time chaining forever? (resolved)

Resolved 2026-08-08 — override.

That sounds good


### Q6: Does the dispatcher eventually consume the log instead of NATS? (resolved)

Resolved 2026-08-08 — override.

This is an interesting architecture question. Every actor needs visibility into their personal queue and there are lots of abstract groups, like anyone with a certain skill, that we will have steps queued up for, and of course agents are actors that will want queues. Do they each just have a lens onto the one giant queue? What happens when it inevitably gets too large? Everyone still just has an API onto the queue and underneath the engineering team makes it work? Let's discuss these before settling. It might open more questions.
