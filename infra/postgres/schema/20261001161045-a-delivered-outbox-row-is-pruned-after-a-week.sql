-- 20261001161045-a-delivered-outbox-row-is-pruned-after-a-week.sql —
-- the outbox's retention leaves a fact (backlog eec0c1f3, incident
-- d3c0a67c).
--
-- Until this car nothing deleted an event_outbox row: the relay stamped
-- `delivered_at` and the row stayed, so every event was stored twice for
-- good, once here and once in audit_log. Measured 2026-10-01, the day
-- the system of record's Postgres volume filled: 1,030,520 delivered
-- outbox rows over 720 h, exactly audit_log's row count.
--
-- boss-event-relay now runs an hourly retention pass
-- (`boss_events::outbox::prune_delivered_outbox`) that deletes a row
-- only when it is DELIVERED, delivered more than the window ago (default
-- seven days), and its event_id is in audit_log. Pending and
-- dead-lettered rows are never touched. Each batch is one short
-- transaction that deletes its rows and stages this fact beside them, so
-- the deletion and its record commit together.
--
-- No schema change: the table keeps its shape and its indexes. Declared
-- in the same car that first emits the kind — an emitted-but-undeclared
-- kind is the defect class the audit integrity check exists to catch.
-- No ref-check rules: the fact names outbox ids, a count and a cutoff,
-- and every event the batch removed is still in audit_log.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain, payload_fields) VALUES
  ('events.outbox.pruned', 'events', 'The outbox relay''s retention pass deleted one batch of DELIVERED event_outbox rows older than its window, each already in audit_log. Names how many, the outbox id range and the delivery cutoff; never an event. Pending and dead-lettered rows are never deleted', NULL,
   '[
      {"name": "deleted",          "type": "int",    "note": "event_outbox rows this batch deleted"},
      {"name": "first_outbox_id",  "type": "int",    "note": "lowest event_outbox.id deleted"},
      {"name": "last_outbox_id",   "type": "int",    "note": "highest event_outbox.id deleted; rows between that were not deleted were pending, dead-lettered, inside the window, locked, or not held in audit_log as the same fact (event_id, timestamp, source, kind and payload)"},
      {"name": "delivered_before", "type": "string", "note": "the cutoff, RFC 3339: the fact''s own wall timestamp minus the window; every deleted row was delivered before it"},
      {"name": "retention_hours",  "type": "int",    "note": "the window the pass ran with"}
    ]'::jsonb)
ON CONFLICT (kind_pattern) DO NOTHING;

-- `events.outbox.redelivered`'s `overtaken_by` was declared as an exact
-- count (20260928070119, applied, so not edited there). It is counted
-- from the rows the outbox still holds, and retention now deletes
-- delivered rows past the window, so for a dead letter older than the
-- window it is a LOWER bound (review afdc2d5d, N3). The note says so;
-- the field's name and type are unchanged.
UPDATE event_kinds
   SET payload_fields = (
       SELECT jsonb_agg(
                  CASE WHEN f->>'name' = 'overtaken_by'
                       THEN jsonb_set(f, '{note}', to_jsonb(
                            'later outbox rows already delivered when this one was put back, counted from the rows the outbox still holds: redelivery is out-of-order delivery, and subscribers see this event after at least that many later ones. A LOWER bound for a dead letter older than the outbox retention window (a week by default), since delivered rows past it are deleted, each batch recorded by events.outbox.pruned'::text))
                       ELSE f END
                  ORDER BY ord)
         FROM jsonb_array_elements(payload_fields) WITH ORDINALITY AS t(f, ord))
 WHERE kind_pattern = 'events.outbox.redelivered';
