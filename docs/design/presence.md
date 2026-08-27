# Design: presence — passkey authorization as actor-auth on steps

**Status**: approved — every question answered in packet `5158ab84`; carried to a file 2026-08-17.

**Origin**: David, 2026-08-16: *"Passkey authorization as actor-auth
feature for job packets is broadly useful. Let's make sure we design
and build it that way."* Revising an earlier draft of this doc that
proposed a bespoke elevation broker — the wrong shape, and it asked a
question the codebase had already answered.

**Related**: [idm-kanidm.md](./idm-kanidm.md) (Q3 resolved: agents get
Kanidm service accounts, phase 2) · `architecture-decisions.md`
§Step types are property bundles

---

## The reframe

The first draft asked "how does an agent get a token". That framing
produces a special subsystem sitting beside the job model, with its
own request type, its own approval, its own audit trail.

The right question is narrower and more useful: **what does it take to
prove a human was present when a step was completed?** Elevation is
then one protocol among many that demands it — not a mechanism of its
own. Payment release, a deploy sign-off, an incident's closure, and
"mint me a token" all want the same property, and none of them should
each invent it.

## Most of this already exists

`SignOffStamp` is already an attestation with content binding:

```rust
pub struct SignOffStamp {
    pub authority_id: String,   // who
    pub role: String,           // which requirement this satisfies
    pub stamped_at: DateTime<Utc>,
    pub shape_hash: String,     // WHAT was attested
}
```

and `step_shape_hash(title, metadata)` canonicalises a step's
completion-relevant content — sorted keys, insertion-order
independent, deliberately excluding status/assignee/sort-order because
those do not change what is being agreed to.

So the earlier draft's Q4 — *what binds an approval to the request it
approved* — was answered before it was asked. A stamp already cannot
survive its step's content changing. The stamp endpoint already
refuses a stale shape and already re-stamps idempotently.

**What a stamp does not carry is how hard it was to produce.** Today
it proves an authenticated session said yes. It cannot distinguish
"David clicked approve" from "David logged in this morning and
something clicked approve".

## The shape

One field, one verification, and the rest is protocol data:

- A step declares the assurance it requires to be stamped.
- A stamp records the assurance it was produced with.
- The stamp endpoint refuses a stamp weaker than the step demands.

```
step.sign_offs_required = ["platform-admin"]
step.assurance          = presence        ← new, declared in the Workflow

POST .../sign-offs
  → gateway: is there a fresh WebAuthn assertion for this actor,
             over a challenge equal to this step's shape_hash?
  → Kanidm verifies the passkey
  → SignOffStamp { …, assurance: presence }
```

**The challenge is the shape hash.** This is the part worth building
carefully. WebAuthn signs over a server-supplied challenge; if that
challenge *is* `step_shape_hash(title, metadata)`, then the passkey
signature is itself the binding — cryptographic proof that this
authenticator approved *this content*, not merely that someone
authenticated near it in time. Replay against a different step fails
because the challenge differs. Replay against the same step after an
edit fails because the hash moved. We get Q4's property for free, from
a mechanism that was going to be there anyway.

## What elevation becomes

A protocol, not a subsystem:

```
elevation-request:  raised → verb+args → approval[assurance=presence] → executed
```

The approval step demands presence. A broker (an `Automation` actor
holding the credential) refuses to act without a presence-assured
stamp whose `shape_hash` matches the verb and arguments it is about to
run. No new auth machinery — it reuses stamps, shape hashes, policy,
and the event log.

## What this is not

Not a replacement for the permission classifier, which is a local
guard on what an agent may attempt. This is how a refused-but-
legitimate action gets done with a human in it.

Not a second identity system. Kanidm is deployed, passkey-first, and
the gateway is already an OIDC client of it.

## Open questions

### Q1: Where is presence declared — the StepType, or the step? (resolved)

Resolved 2026-08-16 — accept.

On the step spec in the Workflow row, beside sign_offs_required and
authority_role. StepType::completion already says which KIND of actor
completes a kind of step (human/agent/child-job) — that is registry
data about the alphabet. Assurance is a different axis: two sign-off
steps can want different strengths depending on what they gate. Let
the StepType carry a floor (a kind may declare it always needs
presence) and let a Workflow raise it but never lower it. That keeps
'how hard is this to approve' as protocol data, editable without a
deploy, which is the test the three-layers reading applies to
everything else.


`StepType::completion` already says *who kind of actor* completes a
kind of step (`human`, `agent`, `child-job`, …), which is registry
data about the alphabet. Assurance is a different axis: two
`sign-off` steps can want different strengths depending on what they
gate.

Proposed: **on the step spec, in the Workflow row** — the same place
`sign_offs_required` and `authority_role` live. The StepType can carry
a floor (a kind may declare it always needs presence), and a Workflow
may raise but never lower it. This keeps "how hard is this to
approve" as protocol data, editable without a deploy, which is the
test the three-layers reading applies to everything else.

### Q2: Is the challenge the shape hash, or is the shape hash carried alongside? (resolved)

Resolved 2026-08-16 — accept.

The challenge IS the shape hash. The alternative — sign an arbitrary
nonce and record the shape hash next to it — leaves the binding as
bookkeeping every code path has to get right; making them one object
means a stamp that verifies is a stamp that matched. Caveat to settle
before building: a shape hash is deterministic, so identical content
yields identical challenges, which is fine for binding and wrong for
replay. Probably sha256(shape_hash || server_nonce) with the nonce
recorded on the stamp — keeps the cryptographic binding, restores
single-use. Worth checking against the WebAuthn spec's challenge
requirements first.


Proposed: **the challenge IS the shape hash.** The alternative —
sign an arbitrary nonce and record the shape hash next to it — leaves
the binding as bookkeeping we have to get right on every path. Making
them the same object means a stamp that verifies is a stamp that
matched, and there is no code path where the two can disagree.

Worth checking against the WebAuthn spec's challenge requirements
(length, entropy, single-use) before committing: a shape hash is
deterministic, so two stamps of identical content produce identical
challenges. That is fine for binding and needs thought for replay —
probably `sha256(shape_hash || server_nonce)` with the nonce recorded
on the stamp, which keeps the binding and restores single-use.

### Q3: What does a presence failure do to the queue? (resolved)

Resolved 2026-08-16 — accept.

Nothing special — it waits, and the queue shows why. A presence-
required step cannot be cleared when you are not at a passkey; that is
the point, but it means a protocol can stall on human availability in
a way today's steps cannot. Surface presence-required steps as their
own group in My Day, beside the human/automation split already built.
The failure mode to avoid is a fallback path ('approve without
presence if urgent') — an assurance level with a bypass is a comment,
not a control.


If David is not at a passkey, a presence-required step cannot be
cleared. That is the point, but it means a protocol can stall on
human availability in a way today's steps cannot.

Proposed: **nothing special — it waits, and the queue shows why.**
Presence-required steps surface as their own group in My Day, beside
the human/automation split already built. The failure mode we must
avoid is a fallback path ("approve without presence if urgent"),
because an assurance level with a bypass is a comment, not a control.

### Q4: Does the agent ever hold a credential? (resolved)

Resolved 2026-08-16 — accept.

No — a broker acts and the agent holds only results. Narrowed from the
first draft now that the broker is the smaller piece rather than the
centrepiece. A short-lived token still lands in the agent's transcript
and logs, and a TTL bounds that exposure without removing it. The cost
is that each elevatable action needs a broker verb, so capability
grows by explicit reviewable addition rather than by handing over a
key.


Narrowed from the first draft now that the broker is the smaller
piece. Proposed: **no — a broker acts, the agent holds only results.**
A short-lived token still lands in the agent's transcript and logs,
and a TTL bounds that exposure without removing it. Cost is that each
elevatable action needs a broker verb; that cost is the feature,
because capability then grows by explicit reviewable addition.

### Q5: Where should Kanidm run, given it becomes the gate? (resolved)

Resolved 2026-08-16 — accept.

Honour the invariant and move it to the GCP box before anything
depends on it. idm-kanidm.md states 'the cluster is a client of
identity, never its host: rebuilding the cluster must not lose the
company's logins', and Kanidm is deployed on cp-1 inside that cluster
(correction 4c8259ea). Once elevation depends on it, a cluster rebuild
removes the ability to authorise repairing the cluster. The
alternative is to consciously retire the invariant and design a break-
glass path that does not need Kanidm — defensible, but it must be a
choice. The present state, where the invariant is written down and
false, is not.


Unchanged from the first draft and still the blocker.
`idm-kanidm.md` states "the cluster is a client of identity, never its
host", and Kanidm is deployed on cp-1 inside that cluster (correction
`4c8259ea`). Once elevation depends on it, **a cluster rebuild removes
the ability to authorise repairing the cluster.**

Proposed: honour the invariant and move it to the GCP box before
anything depends on it — or consciously retire the invariant and
design a break-glass path. Either is defensible; the present state,
where the invariant is written down and false, is not.

### Q6: Should a design-doc review's resolutions be bound to the markdown they reviewed? (resolved)

Resolved 2026-08-16 — accept.

Yes, and it is the same fix one level up. Discovered while revising
this doc: PUT /api/jobs/{id} accepts a full body and rewrites
metadata.markdown, so a design packet's prose can change after review
and the resolutions are not bound to what the reviewer read. This very
packet was rewritten in place after you saw the first version. Record
the hash of the markdown a resolution answered, so a doc edited
afterwards shows its resolutions as stale rather than silently
carrying them. Lower urgency than Q1-Q5 and cheap once the shape-hash
machinery exists — but worth recording now, because 'the reviewed
artefact can change after review' is obvious in hindsight and
invisible in practice.


Discovered while revising this doc: `PUT /api/jobs/{id}` accepts a
full body and rewrites `metadata.markdown`. **A design packet's prose
can change after it has been reviewed, and the resolutions are not
bound to what the reviewer actually read.** This very doc was rewritten
in place after you saw the first version.

That is the same defect class the shape hash solves one level down —
and the fix is the same shape.

Proposed: **stamp design-doc reviews with the shape hash too.** A
resolution records the hash of the markdown it answered; a doc edited
afterwards shows its resolutions as stale rather than silently
carrying them. Lower urgency than Q1–Q5 and cheap once the machinery
exists, but it should be recorded now, because "the reviewed artefact
can change after review" is exactly the kind of thing that is obvious
in hindsight and invisible in practice.

## Decision history

Reviewed as packet `5158ab84` on 2026-08-16; the packet carried the
prose and the questions, so this file is the residue rather than the
precondition. Answers verbatim:

**Q1 — Where is presence declared — the StepType, or the step?**

On the step spec in the Workflow row, beside sign_offs_required and
authority_role. StepType::completion already says which KIND of actor
completes a kind of step (human/agent/child-job) — that is registry
data about the alphabet. Assurance is a different axis: two sign-off
steps can want different strengths depending on what they gate. Let
the StepType carry a floor (a kind may declare it always needs
presence) and let a Workflow raise it but never lower it. That keeps
'how hard is this to approve' as protocol data, editable without a
deploy, which is the test the three-layers reading applies to
everything else.

**Q2 — Is the WebAuthn challenge the shape hash, or carried alongside it?**

The challenge IS the shape hash. The alternative — sign an arbitrary
nonce and record the shape hash next to it — leaves the binding as
bookkeeping every code path has to get right; making them one object
means a stamp that verifies is a stamp that matched. Caveat to settle
before building: a shape hash is deterministic, so identical content
yields identical challenges, which is fine for binding and wrong for
replay. Probably sha256(shape_hash || server_nonce) with the nonce
recorded on the stamp — keeps the cryptographic binding, restores
single-use. Worth checking against the WebAuthn spec's challenge
requirements first.

**Q3 — What does a presence failure do to the queue?**

Nothing special — it waits, and the queue shows why. A presence-
required step cannot be cleared when you are not at a passkey; that is
the point, but it means a protocol can stall on human availability in
a way today's steps cannot. Surface presence-required steps as their
own group in My Day, beside the human/automation split already built.
The failure mode to avoid is a fallback path ('approve without
presence if urgent') — an assurance level with a bypass is a comment,
not a control.

**Q4 — Does the agent ever hold a credential?**

No — a broker acts and the agent holds only results. Narrowed from the
first draft now that the broker is the smaller piece rather than the
centrepiece. A short-lived token still lands in the agent's transcript
and logs, and a TTL bounds that exposure without removing it. The cost
is that each elevatable action needs a broker verb, so capability
grows by explicit reviewable addition rather than by handing over a
key.

**Q5 — Where should Kanidm run, given it becomes the gate?**

Honour the invariant and move it to the GCP box before anything
depends on it. idm-kanidm.md states 'the cluster is a client of
identity, never its host: rebuilding the cluster must not lose the
company's logins', and Kanidm is deployed on cp-1 inside that cluster
(correction 4c8259ea). Once elevation depends on it, a cluster rebuild
removes the ability to authorise repairing the cluster. The
alternative is to consciously retire the invariant and design a break-
glass path that does not need Kanidm — defensible, but it must be a
choice. The present state, where the invariant is written down and
false, is not.

**Q6 — Should a design review's resolutions be bound to the markdown they reviewed?**

Yes, and it is the same fix one level up. Discovered while revising
this doc: PUT /api/jobs/{id} accepts a full body and rewrites
metadata.markdown, so a design packet's prose can change after review
and the resolutions are not bound to what the reviewer read. This very
packet was rewritten in place after you saw the first version. Record
the hash of the markdown a resolution answered, so a doc edited
afterwards shows its resolutions as stale rather than silently
carrying them. Lower urgency than Q1-Q5 and cheap once the shape-hash
machinery exists — but worth recording now, because 'the reviewed
artefact can change after review' is obvious in hindsight and
invisible in practice.

