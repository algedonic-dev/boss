# Design: payload encryption — confidentiality in a log that proves plaintext

**Status**: in-review — open questions tracked at `/system/design`.
**Source**: feedback `a32ea8c0` (David, 2026-08-09): "We should think
about encrypting job payloads given they are the atomic unit of
information in the system. We will need a sophisticated system to
allow actors to decrypt and view only the policy-approved elements
of the payload."
**Related**:
[transactional-audit-log.md](./transactional-audit-log.md) (the chain
that proves plaintext) · [correctness-protocol.md](./correctness-protocol.md)
· [human-powered-state-machine.md](./human-powered-state-machine.md)
(the semantic layer this classifies)

---

## Why this is hard here specifically, measured

The exhibit: `people.employee.created` payloads carry
`annual_salary_cents` and `email` — plaintext, in a log that is
append-only, hash-chained, replicated into projections, and by
design **never redactable** (the immutability triggers reject UPDATE
at the SQL level; that is the feature). 120 distinct event kinds
flow through the same log; nothing today distinguishes a salary
from a step id.

And plaintext is load-bearing, mechanically. Inventory of consumers
that read payload fields as data:

- **The chain itself**: canonical bytes are
  `event_id|timestamp|source|kind|payload::text` — the hash chain
  *proves the plaintext*. Encrypt a payload after the fact and every
  verification fails; encrypt before insert and the chain proves
  ciphertext — replay/rebuild then needs decryption to reproduce
  projections, and determinism must survive key rotation.
- **Referential guards**: `audit_log_ref_checks` and
  `subject_edges` resolve `payload->>field` inside the write
  transaction.
- **The dispatcher**: `when` expressions bind payload fields;
  handlers read them.
- **Views**: ten payload reads in boss-views alone (OS map, flow,
  fleet, stage durations); `event_facts` lifts payload wholesale;
  search indexes it.

Any design that encrypts fields these consumers read either breaks
them, moves them behind decryption, or classifies those fields as
never-encrypted. There is no free layer.

## The shape of an answer

**Classification lives in the semantic layer, not in crypto config.**
The ontology David is converging is exactly where "this field is
salary-grade, that one is routing-grade" belongs: `metadata_schema`
and the event-kind registry gain a sensitivity class per field.
Policy then maps `(actor, sensitivity class) → disclosure` — the
same row-level policy engine, extended one level down, into fields.
That is the "sophisticated system" in his sentence: not key
ceremony, but **policy-scoped field disclosure as a first-class read
path** — the API returns the payload an actor is entitled to see,
with sealed fields marked as sealed rather than silently absent
(absence lies; sealing is honest).

**Layers, from cheap to deep** (not mutually exclusive):

1. **At-rest disk encryption** — protects stolen disks, nothing
   else; every DB session sees plaintext. Table stakes, not the ask.
2. **Read-path redaction** (no crypto): policy-scoped field
   disclosure at the API, plaintext in the DB. Delivers the visible
   half of the ask immediately; DB access still sees everything.
   Honest as a stage, dishonest as an endpoint.
3. **Field-level encryption at write, policy-mediated decryption at
   read**: sensitive fields sealed before the outbox insert (the
   chain proves ciphertext + everything else stays plaintext and
   functional if classification keeps routing-grade fields clear).
   Rebuilders decrypt sealed fields to rebuild projections that need
   them — projections themselves become classified surfaces.
4. **Per-actor / envelope encryption (E2E)** — keys held per
   principal, the server cannot read sealed fields at all. Breaks
   rebuild-from-log for those fields unless rebuilders hold a
   rebuild principal; the deepest honesty and the deepest cost.

## What this deliberately is not

- Not redaction of history: the log stays immutable. Classification
  applies to *reads* and to *future writes*; the existing plaintext
  history is a migration question (Q4) with no pretty answer, named
  rather than hidden.
- Not a new policy engine: the existing `(action, resource, scope)`
  model extends to field classes; a second authorization system
  would drift from the first.

## Open questions

### Q1: Which layer is the target, and which is the first stage?

Proposal: stage 2 (read-path redaction) ships first — it delivers
the operator-visible half of the ask with zero crypto risk and
forces the classification work that every deeper layer needs
anyway. Layer 3 follows for the fields whose classification proves
stable. Layer 4 only if a tenant's threat model demands it.

### Q2: Where does field sensitivity live in the ontology?

A `sensitivity` attribute on `metadata_schema` fields and on the
event-kind registry's payload schemas — or a standalone
classification registry keyed `(kind, field_path)` like
`job_edges`. The registry shape keeps classification independent of
the many schema owners; the inline shape keeps one source of truth
per field. Both must answer: who may *author* a classification
change (it is a disclosure decision).

Proposed: **inline, as a `sensitivity` key on
`event_kinds.payload_fields` entries — because the column already
exists and this question is the reason it was created.** Migration
108's own comment on it reads "flat field inventory `[{name, type,
note}]`; starts empty, filled as consumers (encryption
classification, rule authoring) need it". The standalone registry
was considered and built against, twice over, by someone who then
chose the inline shape.

The measurement that decides it, though, is not the comment — it is
the discipline the column already carries. 135 event kinds are
seeded and **exactly one** declares a roster: `jobs.job.closed`, in
migration 137, added yesterday for rule authoring after eight WARN
redeliveries. That migration states the rule that makes the shape
work, and it generalises to sensitivity unchanged: *a ratchet, not
an inventory*. `payload_contract` skips a kind whose roster is
empty, so declaring one topic gates that topic without a complete
census first — "a check that has to be total before it is useful
never lands." Classification has the same problem in a worse form,
because 135 kinds is a lot of disclosure decisions and the salary
one is urgent.

A standalone `(kind, field_path)` registry cannot borrow that. It
starts as a second place a field is described, and the first thing
it needs is a rule about what an undeclared field means — which is
the question `payload_fields` has already answered.

The honest cost, stated because it is real: `payload_fields` is
**flat on purpose**, since only the root segment of an identifier
is a payload key for rule binding. Sensitivity does not respect
that boundary — `metadata.annual_salary_cents` is exactly the shape
a salary hides in. So either sensitivity classifies the root key
(sealing `metadata` wholesale, which is too coarse to be useful) or
`payload_fields` gains dotted paths for this consumer and stays
flat for the other. Recommend the latter, declared as such: the
rule binder keeps reading root segments only, and a dotted entry is
simply invisible to it.

Job metadata is the second surface and takes the same treatment on
`metadata_schema`, which already has per-field entries. Two
surfaces, one vocabulary.

On who may author a classification change: a policy rule on
`(classify, event-kind)`, consistent with David's answer to
design-docs-as-data Q4 — a verb is a policy rule, not a role
Class. Unlike a doc publish, though, this one should NOT be agent-
reachable by default. Lowering a field's sensitivity is a
disclosure, and it is the one write in this design whose blast
radius is other people's salaries.

### Q3: What does the chain prove once fields seal?

Sealed-before-insert means the chain honestly proves what was
written (ciphertext + clear fields). Verification stays mechanical.
But the *determinism* property — rebuild reproduces projections —
now requires rebuild-time decryption for sealed fields. Does the
integrity checker learn to verify sealed payloads structurally, and
does replay carry a decryption principal?

Proposed: **the integrity checker learns nothing, and replay carries
a rebuild principal.** The two halves of this question have very
different answers and the doc treats them as one.

The checker half is already settled by how it is built, and the
answer is better than the question assumes. `integrity.rs`
recomputes the chain in SQL — `digest(LAG(row_hash) ||
convert_to(event_id || '|' || timestamp || '|' || source || '|' ||
kind || '|' || payload::text, 'UTF8'), 'sha256')` — and it does
that deliberately, so the verifier and the trigger canonicalise
from the same `payload::text` and no cross-language
canonicalization bug is possible. That code hashes BYTES and never
reads a field. A payload whose `annual_salary_cents` holds a
base64 envelope instead of an integer verifies identically, with
no change to the checker at all. "Does the integrity checker learn
to verify sealed payloads structurally" should be answered no —
teaching it to look inside is how it acquires the ability to be
wrong about what it is checking.

The determinism half is where the real cost is, and it is a
principal, not a mechanism. `boss-rebuild` calls every domain
rebuilder; projections built from sealed fields (the people
projection reads `annual_salary_cents`) cannot be reproduced
without decryption. So replay carries a rebuild principal with
disclosure over every sealed class — which is to say: **the
rebuilder can read everything, forever, and that is now written
down as a property of the design rather than discovered during an
incident.** That single principal is the ceiling on what layer 3
can promise, and it is exactly why layer 4 is a different design
and not a deeper setting.

One consequence to accept with it: a projection built from a sealed
field is itself a sealed surface, and nothing in the current
projection layer knows that. `employees.annual_salary_cents` in a
projection table is as readable as it was in the log. Layer 3 is
not finished at the log — it is finished when the projections it
feeds carry the same classification, and the honest scope of
"field-level encryption at write" includes that work.

### Q4: What happens to the existing plaintext history?

The log cannot be rewritten. Options: accept plaintext history with
a classification cutover date (honest, cheap, leaves salaries
readable in old rows); epoch-reset the demo tenant post-cutover
(viable here, not for a real tenant); or a sealed re-log migration
(new epoch whose baseline is a sealed transform of the old log —
heavy machinery, the only full answer). A real tenant decides this
before their first sensitive write, which is the strongest argument
for deciding classification early.

Proposed: **accept plaintext history with a classification cutover
date, and say the date out loud in the doc.** Of the three options
this is the only one that is honest about what actually happened,
and the machinery for the other two argues against them rather
than for them.

The epoch-reset option is mechanically available today — `sim_clock`
carries `epoch_baseline_audit_id` and every restart-epoch trims the
log back to it — which makes it tempting and makes it worse. It
would work on this tenant, produce a log with no salaries in it,
and teach exactly the wrong lesson: that the immutable log can be
made to have not contained something. The demo tenant is the worked
example other people read. A cutover date models what a real tenant
will actually live with; an epoch reset models a capability real
tenants do not have.

The sealed re-log migration is the only complete answer and should
stay named and unbuilt. It is a new epoch whose baseline is a
sealed transform of the old log, which means writing a program that
rewrites history correctly — and the property the whole system
rests on is that no such program exists. Building one to fix
disclosure would be trading the strongest guarantee in BOSS for
plaintext that has already been read by everyone who has read it.

So: a `classified_from` date on the sensitivity vocabulary, rows
before it plaintext and known to be plaintext, and the operator
guidance stated once — the way to have no plaintext salaries in a
log is to classify before the first hire, not after. That guidance
is worth more to a real tenant than any migration this repo could
ship, and it is the reason to answer Q2 before Q1's stage 3.

### Q5: How do sealed fields read in the UI?

Sealed-and-marked (the field exists, shows as sealed, requests
elevation) versus policy-filtered (absent for the unauthorized).
Sealed-and-marked is the honest surface and matches the
"decrypt and view only the policy-approved elements" framing —
you can see that there is something you cannot see.

Proposed: **sealed-and-marked, and the question the doc has not
asked is what the marker says.** The choice itself is not close and
the doc argues it correctly: absence lies. Two things worth adding
before this is buildable.

The marker must distinguish *sealed* from *empty*. A salary field
that reads "sealed" and a salary field that was never set are
different facts about an employee, and a single treatment for both
reintroduces the lie one level in. That means the read path returns
three states per classified field — a value, sealed, or genuinely
absent — and the UI has three renderings, not two.

And the elevation request should be a Job. "Requests elevation" is
written in the question as a UI affordance, but a disclosure
request is bounded work with an owner, a subject, an approver and
an audit trail, which is the definition of a Job in this system.
Making it a `request-disclosure` Workflow costs a registry row and
gives the whole thing for free — including the property that
matters most here, that every disclosure ever granted is in the
log with who approved it. A modal that calls an endpoint gives
none of that.

No new surface work is needed to try this: nothing in `apps/web`
renders a redacted or sealed field today, so the pattern is
unowned, and sealed-and-marked should be built once in the kit
rather than per page — the same argument as workflow-ux-as-data Q3,
for the same reason.

## Decision history

_None yet._
