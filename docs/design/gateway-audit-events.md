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

### Q1: Pool-in-the-gateway or ingest-endpoint?

Proposed: **(a)**, scoped hard: one pool, max 2 connections, used
exclusively by an `audit_outbox` module; the login path never blocks
on it (fire-and-forget task with a bounded queue — a slow database
must not slow a login, and a lost telemetry event is a warn-line
regression to today's behavior, not a correctness loss).

### Q2: Which auth events, day one?

Proposed: `auth.login.denied` (local + OIDC, with reason class),
`auth.login.succeeded` (email + method — the session mint moment),
`auth.session.guest` (the demo path, counted). Not proposed:
per-request events of any kind — the gateway sees everything, and
the log must not become a request log.
