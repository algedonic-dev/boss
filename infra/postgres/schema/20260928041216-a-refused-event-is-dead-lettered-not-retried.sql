-- 20260928041216-a-refused-event-is-dead-lettered-not-retried.sql — the
-- outbox relay sets aside a row the bus will never carry, instead of
-- retrying it forever at the head of the queue (backlog e4019cbc, the
-- adversarial review of the gateway-auth car, finding H1).
--
-- `drain_outbox_once` stopped its batch on ANY publish error and left
-- the row pending, so the next drain re-selected the same row. For a
-- transport failure that is right. For a DETERMINISTIC refusal — an
-- event over NATS max_payload (1 MiB: the cluster's nats container sets
-- no -max_payload) or with an invalid subject — it is a poison pill: no
-- later event, for any service, reaches the bus again, and the batch
-- window stops advancing into audit_log. Silence, not an error.
--
-- A refused row is now DEAD-LETTERED: stamped `dead_lettered_at` with
-- the bus's own words in `dead_letter_reason`, and the relay moves on.
-- Conservation: the row is never deleted and its payload is kept whole;
-- its audit_log row was committed before the publish was tried, so the
-- fact is in the system of record either way. `delivered_at` stays NULL
-- — a refused row was never delivered, and nothing may say it was.
ALTER TABLE event_outbox
    ADD COLUMN IF NOT EXISTS dead_lettered_at   TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS dead_letter_reason TEXT;

-- The relay's pending predicate is now "neither delivered nor dead-
-- lettered"; the partial index follows it so a dead letter never sits
-- in the index the drain scans.
DROP INDEX IF EXISTS event_outbox_pending;
CREATE INDEX IF NOT EXISTS event_outbox_pending
    ON event_outbox(id) WHERE delivered_at IS NULL AND dead_lettered_at IS NULL;

-- The alarm. Staged in the same transaction that sets the row aside, so
-- a dead letter is never silent, and read by the dispatcher rule
-- `open-a-packet-when-an-event-is-dead-lettered`, which files the
-- backlog-item a person triages. Declared in the same car that first
-- emits it: an emitted-but-undeclared kind is the defect class the audit
-- integrity check exists to catch. No ref-check rules: it names an
-- outbox row and an event by id, and the refused payload never rides on
-- it.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain, payload_fields) VALUES
  ('events.outbox.dead_lettered', 'events', 'The outbox relay set aside an event the bus refused deterministically (over NATS max_payload, or an invalid subject) and went on to the rows behind it. Names the outbox row, the event, its kind and size, and the refusal; the payload stays on the outbox row and the fact is already in audit_log', NULL,
   '[
      {"name": "outbox_id",     "type": "int",    "note": "event_outbox.id of the dead-lettered row"},
      {"name": "event_id",      "type": "uuid",   "note": "the refused event''s id, as in audit_log"},
      {"name": "event_kind",    "type": "string", "note": "the refused event''s kind"},
      {"name": "event_source",  "type": "string", "note": "the service that staged it"},
      {"name": "payload_bytes", "type": "int",    "note": "the event as the relay encoded it for the bus"},
      {"name": "reason",        "type": "string", "note": "the bus''s refusal, verbatim; also on event_outbox.dead_letter_reason"}
    ]'::jsonb)
ON CONFLICT (kind_pattern) DO NOTHING;
