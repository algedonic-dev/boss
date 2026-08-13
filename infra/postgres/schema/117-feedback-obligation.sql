-- 117-feedback-obligation.sql — the feedback loop closes itself
-- (job 2c4ae549).
--
-- BOSS has two protocols that never touched. `user-feedback` runs
-- submitted → triage → {investigate|design-review|build|needs-info} →
-- closed. `ship-a-change` runs opened → scope → build → gate → review
-- → merged. A car has named its motivating packet through the
-- declared `('ship-a-change','backlog_item')` job edge since migration
-- 104 (dialed to `abort` by 105, so an unresolvable referent is
-- refused at the write) — and nothing ever read it. Cars therefore
-- carried the reference as `metadata.backlog_text` prose instead,
-- which nothing reads either.
--
-- MEASURED 2026-08-11: sixteen feedback packets sat at `submitted`
-- while the work they authorized was shipped, tested and live. The
-- loop closed only because an agent remembered to close it by hand.
--
-- David ratified the rule these two rows implement, verbatim: "Once
-- the user feedback results in either a shipped change or some other
-- terminal state, it can be closed without the filer approving. But,
-- we should always notify the filer with the terminal state and it
-- should show in their watchlist." The watchlist surface is a separate
-- car; the closing and the telling are these.
--
-- Both are RULE ROWS over generic handlers, not feedback-specific
-- code. `jobs.complete_linked_step` knows "follow the edge named by
-- `link`, complete whichever of the steps named by `steps` is open on
-- the far end" — a shape. WHICH edge, which steps and which Workflows
-- it applies to are these rows' business, so pointing the same handler
-- at a different obligation is a row, not a deploy.
--
-- Idempotence, since JetStream is at-least-once and `jobs.job.closed`
-- has three emit sites: the completion no-ops unless a named branch is
-- actually open (a re-run finds it `completed`), and the notification
-- carries a deterministic message id that collapses on the messages
-- surface's existing ON CONFLICT (id) DO NOTHING insert.
--
-- Rollback is `UPDATE dispatcher_rules SET status = 'retired'` on
-- either name; the protocols go back to not touching.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('complete-feedback-branch-on-car-merged', 1, 'active', 'jobs.job.closed',
   'kind = "ship-a-change" AND outcome = "merged"',
   '[{"handler":"jobs.complete_linked_step","args":{"link":"\"backlog_item\"","steps":"\"investigate,design-review,build\""}}]'::jsonb,
   NULL, NULL, NULL, NULL),
  ('notify-filer-on-feedback-terminal', 1, 'active', 'jobs.job.closed',
   'kind = "user-feedback"',
   '[{"handler":"messages.notify_job_terminal","args":{"recipient_key":"\"submitted_by\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
