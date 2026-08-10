-- 109-dispatcher-rule-flush-queue.sql — a recorded decision queues
-- its flush (cea82de0, link 1 of the decision-flush loop; consumes
-- S1's docs.design.decision_recorded).
--
-- `when` binds the FLAT `doc_path` identifier from the S1 payload.
-- One event per answered question; the handler treats the
-- no-pending-decisions 400 as a no-op, so bursts settle clean. The
-- flush WORKER stays operator-run until its tree/remote question is
-- decided (docs-as-data).
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('design-decision-flush-queue', 1, 'active', 'docs.design.decision_recorded',
   NULL,
   '[{"handler":"docs.flush_queue","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
