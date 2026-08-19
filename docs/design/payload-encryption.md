# Design: payload encryption — confidentiality in a log that proves plaintext

**Status**: decided — all questions answered by David in review `07c40d86`, 2026-08-19.
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

None — every question was answered in review `07c40d86` on 2026-08-19; see Decision history.

## Decision history

**Q1 — Which layer is the target, and which is the first stage (decided by David in review `07c40d86`, 2026-08-19).**
stage 2 (read-path redaction) ships first — it delivers the operator-visible half of the ask with zero crypto risk and forces the classification work that every deeper layer needs anyway. Layer 3 follows for the fields whose classification proves stable. Layer 4 only if a tenant's threat model demands it.

**Q2 — Where does field sensitivity live in the ontology (decided by David in review `07c40d86`, 2026-08-19).**
**inline, as a `sensitivity` key on `event_kinds.payload_fields` entries — because the column already exists and this question is the reason it was created.** Migration 108's own comment on it reads "flat field inventory `[{name, type, note}]`; starts empty, filled as consumers (encryption classification, rule authoring) need it". The standalone registry was considered and built against, twice over, by someone who then chose the inline shape.

**Q3 — What does the chain prove once fields seal (decided by David in review `07c40d86`, 2026-08-19).**
**the integrity checker learns nothing, and replay carries a rebuild principal.** The two halves of this question have very different answers and the doc treats them as one.

**Q4 — What happens to the existing plaintext history (decided by David in review `07c40d86`, 2026-08-19).**
Agreed with the accept part of the proposal, but let's not worry about the epoch reset or baseline time. That isn't needed anymore that the network is 'live'. There is only real-time / wall clock now.

**Q5 — How do sealed fields read in the UI (decided by David in review `07c40d86`, 2026-08-19).**
**sealed-and-marked, and the question the doc has not asked is what the marker says.** The choice itself is not close and the doc argues it correctly: absence lies. Two things worth adding before this is buildable.

