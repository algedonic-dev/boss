-- 150-feedback-obligation-done-metadata.sql — the car-merge obligation
-- learns the step kind's own completion vocabulary, as rule data.
--
-- WHY (0ab5fa3a, David accepted (a) 2026-08-19): user-feedback v11
-- makes the design-review step kind `answer-question`, whose `verdict`
-- and `answer` are REQUIRED at completion (StepType-level, and the
-- jobs API validates spec fields and StepType fields in union). The
-- `complete-feedback-branch-on-car-merged` obligation completed
-- design-review steps with only its evidence metadata, so on a v11
-- packet that completion would 400 — breaking the shipped-so-close
-- loop precisely on the protocol version that fixes the review UX.
--
-- THE TRANSLATION IS DATA. v2 of the rule carries a `done_metadata`
-- arg: a JSON object merged into the completion write, string values
-- substituting {branch}/{car}/{title} from the closing car. The
-- handler stays generic ("stamp these fields when completing");
-- WHAT a merged car means in a step kind's vocabulary — approved,
-- answered by shipping — is the rule row's statement, editable by
-- publishing v3, never by a deploy.
--
-- Applied uniformly to all three branch slugs on purpose: `verdict:
-- approved` / `answer: shipped: <branch>` is a true record on an
-- investigate or build branch too, and a per-slug carve-out would be
-- policy the handler has to know. Absent keys only — metadata a
-- person already wrote wins.
--
-- RETIRE v1 BEFORE INSERTING v2 (the 148 lesson):
-- `dispatcher_rules_one_active_per_name` rejects two active versions,
-- and this file runs in one transaction so there is no window where
-- the obligation is missing.
UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'complete-feedback-branch-on-car-merged'
   AND version = 1;

INSERT INTO dispatcher_rules
    (name, version, status, on_event, when_expr, do_steps,
     delay, schedule_cadence, schedule_anchor, schedule_calendar)
VALUES
  ('complete-feedback-branch-on-car-merged', 2, 'active', 'jobs.job.closed',
   'kind = "ship-a-change" AND outcome = "merged"',
   '[{"handler":"jobs.complete_linked_step","args":{"link":"\"backlog_item\"","steps":"\"investigate,design-review,build\"","done_metadata":"\"{\\\"verdict\\\": \\\"approved\\\", \\\"answer\\\": \\\"shipped: {branch} — {title}\\\"}\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
