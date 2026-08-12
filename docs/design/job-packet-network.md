# Design: the job packet — envelopes, protocols, and queues all the way down

**Status:** draft — open questions tracked at `/system/design`
**Origin:** four feedback items, David, 2026-08-11/12 (verbatim):
"Jobs don't have owners or status anymore. They sit in queues. The
nature of that queue will explain the responsibility for the work and
its status. Jobs are packets of information, immutably built up
through writes, that follow one or more protocols" (`b32843df`).
"A job is a packet and the audit log captures the creation plus any
changes to the packet… the job envelope with our data-defined
protocols, probably as headers… priority queues being data-definable"
(`cdf6f9b5`). "Each job/workflow as a protocol of protocols. We can
add/subtract protocols from the job envelope over its 'route' though
each payload will be encoded with all the protocols and versions it
is supporting at that moment" (`75a66b01`). "Our architecture and
system diagram snap to networking primitives better now that we
understand where this is going" (`955ba4e5`).
**Related**: [requirements-based-addressing.md](./requirements-based-addressing.md) —
the addressing half of this model: queues as predicates, pools as
their actor face ·
[it-activity-network.md](./it-activity-network.md) — the rendering:
stations/rails/traffic/packets ·
[queue-visibility.md](./queue-visibility.md) ·
[transactional-audit-log.md](./transactional-audit-log.md) ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

## The claim

A Job is a **packet**: an envelope of headers plus an immutable
accretion of writes, moving between **queues** under one or more
**protocols**. The queue a packet sits in — not fields on the packet —
answers "whose responsibility is this" and "what state is it in". The
audit log is the packet capture: creation plus every change, in order,
with an actor on every write.

This is not a new system. It is the network framing finally applied to
the primitives themselves, and most of it is *recognition* rather than
construction:

- **Immutable accretion already holds at the log.** Every jobs/steps
  row mutation goes through exactly five SQL statements, all taking
  events recorded in the same transaction (boss-jobs postgres.rs; the
  outbox contract in transactional-audit-log.md). The projection rows
  mutate; the log accretes. "The packet" is the log's view of a job_id.
- **Status is already ~80% derived.** `compute_job_status(steps)` is a
  pure function; `blocked` has no writer anywhere; the feedback board
  computes its columns from step position and ignores `Job.status`;
  fleet computes station depth from steps. The stored column survives
  mostly as a query predicate (`status=open` in the lens, My Day,
  fleet, the conductor).
- **Queues already carry the responsibility.** The assignments lens is
  one WHERE clause: personal branch (`assignee_id = me`) OR group
  branch (`metadata.authority_role ∈ my roles`, unclaimed). Workflow
  steps have **no assignee field** — assignment is runtime state, not
  protocol data. The dispatcher deliberately leaves role-less steps
  unassigned for pull.
- **Protocol pinning already exists.** Every Job records
  `workflow_version` at open; in-flight Jobs stay pinned across
  publishes; steps pin `step_plugin_version`. The `jobs_kind_version`
  index exists and nothing reads it yet — protocol-usage measurement
  is an index waiting for a view.
- **Headers already exist as folklore.** `job.metadata` carries the
  real routing/coordination vocabulary today — `waiting_on`, `branch`,
  `train`, `merged`, `backlog_item`, `authority_role` on steps — and
  `job_edges` (104, 110) is a *declared registry over metadata keys*
  with prefix resolution and a warn/abort dial. The envelope's header
  registry is already half-built; `tags` is an entirely unused slot.

## The envelope

The packet's parts, named against what exists:

- **Envelope** — the Job row: identity, subject, headers, protocol
  set. Cheap to read without opening the payload.
- **Headers** — declared, registry-described metadata keys
  (`job_edges` generalized): cross-packet references, routing
  requirements, coordination markers. A header is data a *protocol*
  reads; free-form metadata stays for everything else.
- **Payload** — the accreted writes: step completions, field values,
  sign-offs — the log's record for this packet.
- **Protocol set** — the workflows this packet is following, each as
  `(kind, version)`. Today: exactly one, fixed at open. Proposed end
  state: a list, where layered protocols conjoin requirements and add
  obligations (requirements-based-addressing Q6: narrowing only in
  v1). "A protocol of protocols" is a workflow whose spec declares
  companion protocols the way `on_complete_create` already declares
  cross-protocol spawns.

## What the queue explains

**Responsibility.** A packet at a station is the responsibility of the
station's pool (the requirement's actor face). Claiming moves it to a
personal queue — the claim hop the network canvas renders. Standing
`owner_id` becomes a *derived accountability lens* — "who owns the
queue this sits in" plus "who has written to this packet" — not a
column every packet carries from birth to close.

**Status.** A packet's state is where it sits: at a station (some step
ready/active), between stations (all gated), departed (terminal
reached). `status=open` becomes "has a live station", which fleet
already computes. Two imperative states resist derivation and must
become explicit facts rather than column values: `draft` (a packet not
yet released to its protocol) and `cancelled` (an operator verdict —
today the close path force-skips steps *from* status, deriving queue
position from status, the exact inverse of this model).

## Constraints this must respect

- **Policy is keyed on owners today.** `Scope::Self_` dereferences
  `job.owner_id == user.id`; `Team` = owner ∈ direct reports; list
  scoping compiles to owner predicates. Re-keying Self/Team onto
  queue-derived accountability is the sharpest single migration in
  this reframe, and address predicates must not become a second,
  weaker authorization path (requirements-based-addressing,
  Constraints).
- **The owner-resolution decision is one release old.** Q7 (approved
  2026-07-15) made the create handler *reject* a Job with no
  resolvable human owner, because 18k automation-owned jobs had made
  accountability vacuous. This doc does not repeal accountability; it
  relocates it. The named-CPU invariant (human-powered-state-machine
  I-2) is untouched — every write still carries an actor.
- **Measurement preconditions.** The step-id correlation defect
  (`jobs.step.created` says `id`, the markers say `step_id`; their
  intersection over the whole log is empty — ship job `784d26c9`)
  makes every log-derived drain/traffic metric silently read zero
  until fixed. And workflow-registry writes (draft-save, publish,
  bootstrap-reconcile) are **not evented** — protocols the log cannot
  witness cannot be "protocols as data" yet.
- **Determinism.** Push-assignment is deterministic by construction
  (FNV-1a over a sorted roster). Pull needs the claim CAS
  (queue-visibility Q2 — still open, now load-bearing) and resolved
  pools recorded as events (requirements-based-addressing Q3), or
  replay cannot reproduce who could have taken the work. The sim
  workforce executes only assigned work today; a pull-native network
  needs it to learn to claim, under the same CAS.
- **Priority is a CHECK constraint + closed Rust enum + TS union.**
  "Data-definable priority queues" requires the §9 move first:
  priority becomes Classes of `job`-kind Subjects, and only then can a
  queue's discipline reference it. Today priority orders nothing
  anywhere — the lens orders by `(opened_on, sort_order)`.

## The diagram

The system diagram redraws on the network stack (`955ba4e5`): the
audit log is the wire; projections are interface counters; services
are hosts; the dispatcher is a router running data-defined rules;
workflows are the protocol suite; queues are ports; policy is the
firewall; the canvas (it-activity-network) is the NOC wall. One
picture, one vocabulary, from architecture doc to UI labels — the
presentation payoff item `75a66b01` asks for: a protocol has a name, a
version, a page, and usage stats, the way a workflow today has only a
DAG.

## Open questions

### Q1: What happens to `owner_id`?

Proposed: two stages. Stage 1 (cheap, honest): `owner_id` stays as
"accountable human of record" but stops pretending to be routing —
surfaces label it accountability, and queue lenses take over "whose
work is this now". Stage 2 (the real move): Self/Team policy scopes
re-key onto queue/requirement ownership + packet authorship, and
`owner_id` becomes a derived lens over the log. Q7's rejection rule
survives as "every packet's *protocol* must name an accountable
requirement owner".

### Q2: Does `Job.status` remain a stored column?

Proposed: keep the column as a materialized cache of the derived
value (the indexes and every `status=open` predicate keep working),
but the writers invert: step writes recompute it always; the manual
status PUT dies except for the two imperative facts, which become
explicit packet writes — `released` (draft → open) and a `cancelled`
verdict step — so even they are payload, not out-of-band column
flips.

### Q3: What is the v1 protocol set on the envelope?

Proposed: keep the single `(kind, workflow_version)` pin as
`protocols[0]`, and add **layers** as the only v1 mutation: a layered
protocol conjoins requirements (narrowing only, per
requirements-based-addressing Q6) and appends obligations as steps in
its own lane. Every layering is itself a packet write, so the
envelope's protocol set at any log position is reconstructible.
`workflow_lint`'s viability proof runs per layer at layering time.

### Q4: Which metadata keys become declared headers?

Proposed: generalize `job_edges` into a header registry: a header row
declares name, value shape, whether it is an edge (resolution +
on_missing), and **which protocol reads it**. Seed it from the
folklore set (`waiting_on`, the conductor's train contract, and
`authority_role` — whose triple duty as routing key, board heuristic,
and sim-defense gets named and split). Undeclared metadata stays
free-form payload.

### Q5: What makes a priority queue?

Proposed: priority moves to the Class registry (retiring the CHECK +
enum + union), and a queue definition (requirements-based-addressing
Q1's registry, when it earns itself) may declare a discipline:
`fifo` (today's `(opened_on, sort_order)`) or `by_class(priority)`.
Escalation stays a hop, not a discipline — the escalation router is
already the only consumer of priority and it routes rather than
reorders.

### Q6: What does protocol presentation look like?

The `75a66b01` question: protocols need semantics people can talk
about — name, version, purpose ("financial transaction compliance"),
what it demands of a packet (headers read, steps added, requirements
conjoined), and usage (packets in flight per version — the unread
`jobs_kind_version` index; per-protocol latency once `784d26c9`
lands). Proposed: a protocol page per workflow kind at
`/system/workflows/<kind>` grows the network vocabulary, and the
canvas's route-ghost layer becomes per-protocol tint — but the
naming/semantics conventions deserve their own sitting once this
doc's model is agreed.

### Q7: Where does the system diagram live?

Proposed: `docs/architecture-diagram.md` redraws on the network
vocabulary above and becomes the one diagram both the README and the
canvas legend cite — one picture, kept honest by naming the same
layers the canvas renders live.
