-- 20260928070119-a-dead-letter-is-redelivered-or-resolved-on-the-record.sql
-- — a dead-lettered outbox row has a way out, and the way out is a fact
-- (backlog e22b692e, the follow-up to e4019cbc).
--
-- 20260928041216 taught the relay to set aside a row the bus refuses
-- (`dead_lettered_at`, `dead_letter_reason`) instead of retrying it at
-- the head of the queue forever. Nothing could then take a row back
-- out: once the cause was fixed (the writer shrunk, the server's
-- max_payload raised with the staging bound beside it) the event stayed
-- off the bus for good, and a row that a person had judged audit-only
-- read as an open dead letter in every count, for good.
--
-- `boss events redeliver <outbox id>` is the way out — a client of
-- boss-events-api's `POST /api/events/outbox/{id}/redeliver|resolve`,
-- Operator tier, credited to the signed caller — and it has two arms,
-- each one transaction that changes the row AND stages the fact that
-- says so:
--
--   redeliver  clears `dead_lettered_at` / `dead_letter_reason`, so the
--              row is pending again and the relay publishes it (its
--              audit_log row was committed before the first attempt,
--              so the relay's audit insert is a no-op). Refused over
--              the staging bound: the bus would only refuse it again.
--              Records `events.outbox.redelivered`, which keeps the
--              refusal the row no longer carries and `overtaken_by`:
--              redelivery is OUT-OF-ORDER delivery, and that is how
--              many later rows subscribers had already seen.
--   --resolve  keeps the row dead-lettered and stamps it RESOLVED with
--              the operator's reason: the fact stays audit-only by
--              decision, and the counts stop naming it as open.
--              Records `events.outbox.resolved`. A RESOLUTION IS
--              FINAL: a resolved row is refused by both arms, since a
--              later redelivery would contradict the recorded decision
--              rather than amend it.
--
-- Conservation: neither arm deletes a row or touches its payload.
ALTER TABLE event_outbox
    ADD COLUMN IF NOT EXISTS dead_letter_resolved_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS dead_letter_resolution  TEXT;

-- Declared in the same car that first emits them: an emitted-but-
-- undeclared kind is the defect class the audit integrity check exists
-- to catch. Neither carries the event's payload — only its identity.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain, payload_fields) VALUES
  ('events.outbox.redelivered', 'events', 'An operator put a dead-lettered outbox row back on the relay''s queue once the cause of its refusal was fixed (boss events redeliver). The row is pending again; the refusal it carried rides here, since the row no longer does. Never the payload', NULL,
   '[
      {"name": "outbox_id",          "type": "int",    "note": "event_outbox.id of the row put back"},
      {"name": "event_id",           "type": "uuid",   "note": "the event''s id, as in audit_log"},
      {"name": "event_kind",         "type": "string", "note": "the event''s kind"},
      {"name": "event_source",       "type": "string", "note": "the service that staged it"},
      {"name": "payload_bytes",      "type": "int",    "note": "the event as the relay will encode it, within the staging bound"},
      {"name": "dead_lettered_at",   "type": "string", "note": "when the relay set it aside"},
      {"name": "dead_letter_reason", "type": "string", "note": "the bus''s refusal, verbatim, cleared from the row by this act"},
      {"name": "overtaken_by",       "type": "int",    "note": "later outbox rows already delivered when this one was put back: redelivery is out-of-order delivery, and subscribers see this event after that many later ones"}
    ]'::jsonb),
  ('events.outbox.resolved', 'events', 'An operator closed a dead-lettered outbox row without redelivering it (boss events redeliver --resolve): the fact stays in audit_log and off the bus, by decision, with the reason recorded. Never the payload', NULL,
   '[
      {"name": "outbox_id",          "type": "int",    "note": "event_outbox.id of the row resolved"},
      {"name": "event_id",           "type": "uuid",   "note": "the event''s id, as in audit_log"},
      {"name": "event_kind",         "type": "string", "note": "the event''s kind"},
      {"name": "event_source",       "type": "string", "note": "the service that staged it"},
      {"name": "payload_bytes",      "type": "int",    "note": "the event as the relay encoded it"},
      {"name": "dead_lettered_at",   "type": "string", "note": "when the relay set it aside"},
      {"name": "dead_letter_reason", "type": "string", "note": "the bus''s refusal, verbatim, still on the row"},
      {"name": "resolution",         "type": "string", "note": "the operator''s reason, also on event_outbox.dead_letter_resolution"}
    ]'::jsonb)
ON CONFLICT (kind_pattern) DO NOTHING;
