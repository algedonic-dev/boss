-- 101-dispatcher-rule-step-assigned.sql — route the new
-- `step.assigned.<kind>` fact to the assignee's inbox.
--
-- The only notify trigger used to be step.ready.*, fired at the READY
-- transition. The common flow is the other order: a step materializes
-- ready and unassigned, and someone picks it up afterwards — that
-- assignment emitted nothing and the assignee was never told (backlog
-- 534a8dc8). Same handler as the ready path; the payload carries the
-- assignee, and the handler's deterministic message id collapses the
-- pair when both target the same person.
--
-- A NEW migration rather than a regenerated 41-dispatcher.sql: 41 is
-- an applied migration, and applied migrations are history
-- (docs/design/schema-migrations.md — editing one trips the checksum
-- guard on every live database). rules.toml remains the human-authored
-- source; the dispatcher_rules seed for a rule added after the runner
-- lands as its own manifest entry. ON CONFLICT DO NOTHING keeps the
-- file re-runnable.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('notify-assignee-on-step-assigned', 1, 'active', 'step.assigned.*', NULL, '[{"handler":"messages.notify","args":{}}]'::jsonb, NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
