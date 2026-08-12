# Design: the gateway joins the log — auth events for the edge

**Status:** draft — open questions tracked at `/system/design`
**Origin:** `3b10f749`, surfaced by the OIDC build: Q2 of
[idm-kanidm.md](./idm-kanidm.md) demands an audit event on a
fail-closed login denial, and the gateway cannot produce one — it has
no outbox (no Postgres pool) and events-api is read-only. Denials are
structured `tracing::warn!` lines today: honest, greppable, and not
in the system of record.

## Why this matters more once the front door is real

Local auth failures were operator typos on a box the operator owns.
With Kanidm live, denials become *security telemetry*: an
authenticated-at-the-IdP stranger probing for access, a revoked
employee's stale passkey, an agent token used off-schedule. The audit
log is where the company's facts live; "who tried the door" is a
fact.

## The two shapes

- **(a) The gateway gains a minimal Postgres pool, outbox-only.** The
  S1 recipe verbatim (`record_event_in_tx`): auth events land
  transactionally, the relay moves them, replay reproduces them.
  Cost: the edge service grows a database dependency it never had —
  deployment surface, credentials, one more thing between a login
  and a session.
- **(b) events-api grows an authenticated ingest endpoint** for edge
  services that legitimately cannot hold a pool. Cost: a second
  write path into the log beside the outbox — the single-writer
  relay design (audit-log Q2) took the opposite bet, and an ingest
  API reopens it for a class of caller rather than a class of event.

The docs subsystem faced this exact fork and took (a) — but boss-docs
already owned a pool. The gateway does not, which is the honest
difference worth a decision rather than a reflex.

## Open questions

All 2 open questions were resolved 2026-08-12 via the in-app
decision tracker and flushed to git. See the Decisions
section below. This section is kept empty as the landing
place for any new questions that surface during
implementation.

---


## Decisions

### Q1: Pool-in-the-gateway or ingest-endpoint? (resolved)

Resolved 2026-08-12 — accept.

(a): the gateway gains one pg pool (max 2 connections) used only by an audit-outbox module on the existing recipe-3 machinery (EventRecorder/PgOutboxRecorder) — no new event plumbing. The pool's credentials are a dedicated Postgres role with INSERT-only on event_outbox. The tracing::warn stays as the backstop when the bounded queue is full or the pool is down: degrade to today's behavior, never to silence, and never block a login. (b) rejected — it either reopens the measured single-writer decision or reintroduces the retired post-commit-publish shape over an HTTP hop, and spends a new service-credential class to get strictly worse durability.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.


### Q2: Which auth events, day one? (resolved)

Resolved 2026-08-12 — accept.

auth.login.denied, auth.login.succeeded, auth.session.guest — all three registered in event_kinds with source='gateway' in the same change. denied carries a closed reason enum (bad_credentials | no_employee_record | idp_denied) and declares no subject reference (none exists; the ref-check trigger would abort one). IdP transport failures — discovery, token exchange, userinfo — stay warn-lines: plumbing facts, not who-tried-the-door facts. succeeded carries method (password | oidc | passkey | guest), sized for the passkey work in flight. No per-request events — ratified as a standing constraint, not a deferral.

**Rationale:** David approved the worked recommendations 2026-08-11 (evidence-grounded decision sheet); recorded by claude:fable.
