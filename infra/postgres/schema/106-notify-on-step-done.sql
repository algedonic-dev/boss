-- 106-notify-on-step-done.sql — BOSS alerts us when a wait ends
-- (David, 2026-08-09: "we should be able to use BOSS itself to
-- alert ourselves when we can keep going").
--
-- Generic and OPT-IN, not train-specific: any workflow step whose
-- metadata carries `notify_on_done: true` notifies its assignee (or
-- the deterministic on-call for its authority_role) when it
-- completes — the ready-notify machinery, pointed at the other end
-- of the wait. The pr-train Workflow (v2) marks its ci / merged /
-- deployed steps; nothing else opts in yet, which is what keeps
-- this from feeding the inbox-noise fire (6bf43b6f): one rule,
-- explicitly-marked steps only.
--
-- The `when` matches a TOP-LEVEL payload field: the dispatcher's
-- expr binder resolves flat identifiers only (an absent identifier
-- is PredicateFailed → retry → dead-letter, not false), so the
-- step.done payload builder carries `notify_on_done` always,
-- defaulting false — pinned by tests/notify_on_done_rule.rs.
--
-- `id_prefix = "done"` keeps the dedup id distinct from the READY
-- notification the same step may have sent (same handler, same
-- ON CONFLICT collapse, different life moment).
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('notify-on-step-done-marked', 1, 'active', 'step.done.*',
   'notify_on_done = true',
   '[{"handler":"messages.notify","args":{"id_prefix":"\"done\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
