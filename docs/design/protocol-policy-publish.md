# Design: Protocol, Policy, Publish — the network's API

**Status:** draft — open questions tracked at `/system/design`
**Origin:** David, 2026-08-12 (verbatim, feedback `63bf89d1`): "whether
we should have a Protocol, Policy, and Publish service that is
essentially the API for the network. It can evaluate the payload
against the protocol, ensure policy is met, and then publish to the
proper queue. We should be able to shrink the need for the dispatcher
to have a bunch of automated rules, which are now encoded in the
protocol/workflow definition."
**Related**: [job-packet-network.md](./job-packet-network.md) — the
packet model this is the API for ·
[requirements-based-addressing.md](./requirements-based-addressing.md) —
Q2's evaluator is this doc's evaluator ·
[transactional-audit-log.md](./transactional-audit-log.md) — the
staging discipline every consequence must ride ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

## The claim

The network gets one admission edge. A write arrives carrying a packet
mutation; **Protocol** evaluates it against the packet's pinned
protocol set (may this transition happen, and what follows from it);
**Policy** checks the actor may perform it; **Publish** stages the
consequences — queue placement, notifications, spawns, obligations —
in the same transaction as the write. Reaction survives only where
admission cannot see: timers, external ingress, and cross-protocol
reactors.

This inverts the dispatcher's contract. Today the write lands, events
emit, and 38 data rules *react* — at-least-once, milliseconds-to-
seconds later, in a separate process whose rules are data but whose
relationship to the protocol is folklore. Under 3P the protocol
definition itself declares its consequences, and admission computes
them synchronously with the fact they follow from.

## Two-thirds of 3P already exists

The boss-jobs write path is the prototype, unnamed:

- **Policy** — every write already passes `policy_glue` through the
  `PolicyClient` port before touching a row. This half does not move;
  it gets named.
- **Protocol (the "may" half)** — `ready_when` is already evaluated
  at admission: the two-pass atomic materialization promotes steps to
  ready inside the write transaction and emits `step.ready.<kind>`
  markers transactionally. The alphabet check (StepType), the
  required-at-done field validation, and `workflow_lint`'s viability
  proof are all protocol conformance running at write or authoring
  time.
- **Publish** — the transactional outbox is the publish half: every
  consequence the write path computes already stages in the same
  transaction and reaches the log and the bus through the single
  relay.

The missing third is **protocol-declared consequences**: what today
lives in `infra/dispatcher/rules.toml` as reactions keyed on
`step.done.<kind>` topics. The precedent that they belong in the
protocol already shipped: `on_complete_create` is a consequence
declared in the WorkflowSpec — a cross-protocol spawn the definition
owns, not a rule the dispatcher owns.

## The rule census — what moves, what stays

38 event-triggered rules today (the schedule runner has zero):

- **Jobs-internal consequences (7)** — `jobs.spawn` (3),
  `jobs.complete_step`, `jobs.clear_waiting`, `jobs.subjob_resolve`,
  `docs.flush_queue` + `messages.notify` (3) alongside. Same service,
  same transaction: these move into the protocol definition whole,
  and come out *stronger* — exactly-once by construction instead of
  the JetStream consumer's at-least-once, and replay-deterministic
  because the consequence is in the log beside its cause.
- **Domain effects of protocol steps (~22)** — `inventory.*`,
  `ledger.*`, `products.*`, `commerce.*`, `people.*`, `shipping.*`,
  `packaging.*`, `gate.resolve`: "the brew step completing MEANS
  parts were consumed." These are protocol consequences too, but the
  execution is another service's transactional write — admission
  cannot reach into a second database. Split the decision from the
  delivery: admission stages an **obligation event** (the protocol
  says X must now happen), and the existing handler machinery becomes
  the executor draining obligations instead of interpreting rules.
  The obligation is exactly-once in the log; delivery stays
  at-least-once with the handler's existing idempotency guards.
- **External glue (9)** — `webhook.notify`: egress to systems that
  are not on the log. Stays reactive, alongside the two other
  survivors: wall-clock timers (the maintenance family's ExecStartPre
  wrapper, deliberately not dispatcher rules) and external ingress
  (webhook receivers, the future `forge.*` events).

End state: `rules.toml` shrinks to the reactive residue, and every
remaining rule carries a sentence naming why it cannot be a protocol
consequence — enforced by a ratchet in the outbox-migration-ratchet
mold, so the count only goes down.

## Constraints this must respect

- **The staging discipline is non-negotiable.** A consequence
  computed at admission ships via `record_event_in_tx` in the write's
  transaction — never a post-commit call. The 2026-07-13 incident
  class and its CI ban apply with more force here, not less: 3P
  concentrates the emit sites.
- **Policy stays in boss-policy.** 3P *consults* policy at admission;
  it must not become a second authorization surface. The same warning
  requirements-based-addressing gives address predicates applies to
  protocol consequences that touch other actors' work.
- **Pinning governs evaluation.** A packet is evaluated against its
  *pinned* protocol versions — a republish changes nothing for
  in-flight packets, and changing a live packet's behavior is a
  layering operation (job-packet-network Q3), visible on the
  envelope.
- **Protocol changes are routing changes.** The workflow registry's
  draft/publish/bootstrap writes are un-evented today. Under 3P a
  protocol edit *is* a network configuration change; landing registry
  writes on the outbox is a prerequisite, not a nicety.
- **One evaluator.** `ready_when`, address predicates
  (requirements-based-addressing Q2's set-valued extension), and
  consequence conditions should be one `boss-expr` surface. Two
  expression languages at one admission edge is the fact-lives-twice
  failure with a parser.

## Open questions

### Q1: New service, or boss-jobs promoted?

Proposed: promote in place. The write path already owns policy,
evaluation, and staging; a separate 3P binary would put a network hop
inside the one transaction that must stay atomic. "The network's API"
is a *named role* — the port and its contract — not necessarily a new
process. Extract a `boss-admission` crate boundary (hexagonal, like
every domain) so a future second writer (the sim, an agent gateway)
speaks the same contract, and revisit a standalone service only when
one exists.

### Q2: Where do consequences live in the protocol definition?

Proposed: an `on` block per step transition in the WorkflowSpec —
declarative consequence rows (`notify`, `spawn`, `assign`,
`obligation:<handler>`) with optional `when` conditions, versioned
with the workflow like everything else in it. Seed by migrating the
seven jobs-internal rules and the spawn rules verbatim;
`workflow_lint` grows a pass proving every declared consequence
resolvable (handler exists, target kind exists) at authoring time —
the viability proof extended from steps to consequences.

### Q3: Are consequences computed sync and delivered async?

Proposed: yes — decision and delivery split. Admission computes the
consequence set synchronously and stages it transactionally (the
decision is the fact); jobs-internal consequences apply in the same
transaction; obligation events for other domains deliver through the
existing handler machinery, now draining declared obligations instead
of interpreting rules. Latency for the packet's own state: zero. For
cross-domain effects: unchanged from today, minus the rule
indirection.

### Q4: What does the queue-placement half publish?

"Publish to the proper queue" — under the packet model a queue is a
predicate, so placement is not a row write. Proposed: admission
resolves the packet's next station (authority-role or address
predicate), records the resolved pool as an event when it is
requirement-addressed (requirements-based-addressing Q3's
determinism rule), and emits the ready/assigned markers it already
emits. The queue lens then *reads* what admission *published* — no
new placement state, one more reason the lens stays the one read
surface.

### Q5: What is the migration path — and its ratchet?

Proposed: three stages, each shippable. (1) Name the edge: extract
the admission crate, no behavior change. (2) Move the seven
jobs-internal rules into WorkflowSpec `on` blocks, one car each,
deleting each rule as its consequence lands — the ratchet counts
`rules.toml` entries and fails CI on growth. (3) Convert the domain
handlers to obligation-drainers kind by kind, starting with the
inventory chain that already has idempotency guards. External glue
and timers never migrate — the ratchet's allowlist names them with
their reasons.

### Q6: Does 3P admit non-Job writes?

The claim says "the API for the network," and the packet doc says all
work is packet writes — but messages, docs decisions, and people
changes write through their own services today. Proposed: scope 3P to
packet admission (jobs) in v1 and state plainly that other domains
keep their write paths until the "everything is a packet" question
(job-packet-network's own horizon) is decided. A network API that
quietly annexes every domain's front door is the second-gateway
anti-pattern this repo already rejected once.
